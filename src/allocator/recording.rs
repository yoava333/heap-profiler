use super::TRACING;
use crate::metadata_store::{self, AllocationMetadata};
use crate::profile::{MemProfileRecord, PROFILE_DATA, SAMPLE_RATE, TOTAL_ALLOC_BYTES};
use crate::stack::{calculate_stack_hash, where_in_python, StackBuffer, StackFrame};
use std::sync::atomic::Ordering;
type ProfileTransaction = crate::sync::MutexGuard<'static, ()>;

#[inline]
pub(super) fn handle_malloc(ptr: *mut std::ffi::c_void, size: usize) {
    handle_malloc_in_transaction(ptr, size, None);
}

#[inline]
fn handle_malloc_in_transaction(
    ptr: *mut std::ffi::c_void,
    size: usize,
    transaction: Option<&ProfileTransaction>,
) {
    if !TRACING.load(Ordering::Acquire) {
        return;
    }
    TOTAL_ALLOC_BYTES.fetch_add(size as u64, Ordering::Relaxed);

    let rate = SAMPLE_RATE.load(Ordering::Relaxed);
    if super::sampling::should_sample(size, rate) {
        record_allocation(ptr, size, rate, transaction);
    }
}

// Stack capture and metadata updates are rare at the default sample rate.
// Keep their stack buffer and lock orchestration off the unsampled path.
#[cold]
#[inline(never)]
fn record_allocation(
    ptr: *mut std::ffi::c_void,
    size: usize,
    rate: i32,
    transaction: Option<&ProfileTransaction>,
) {
    let _transaction = transaction
        .is_none()
        .then(|| crate::profile::PROFILE_TRANSACTION.lock());
    if !TRACING.load(Ordering::Acquire) {
        return;
    }
    // Record the stack trace
    let mut stack = StackBuffer::new();
    let depth = where_in_python(&mut stack);

    // Calculate stack hash
    let mut profile_data = PROFILE_DATA.lock();
    let stack_hash = resolve_stack_key(
        &profile_data,
        &stack[..depth],
        calculate_stack_hash(&stack[..depth]),
    );

    let metadata = AllocationMetadata {
        stack_hash,
        requested_size: size,
        generation: crate::profile::GENERATION.load(Ordering::Relaxed),
        sample_rate: rate,
    };

    metadata_store::store_metadata(ptr, metadata);

    // Update profile data under the same lock used to resolve collisions.
    let record = profile_data
        .entry(stack_hash)
        .or_insert_with(|| MemProfileRecord::new(&stack[..depth]));
    record.alloc_bytes = record
        .alloc_bytes
        .saturating_add(size.min(i64::MAX as usize) as i64);
    record.alloc_objects = record.alloc_objects.saturating_add(1);
    let (objects, bytes) = crate::profile::sample_weight(size, metadata.sample_rate);
    record.scaled_alloc_objects += objects;
    record.scaled_alloc_bytes += bytes;
}

fn record_free(metadata: AllocationMetadata) {
    if metadata.generation != crate::profile::GENERATION.load(Ordering::Relaxed) {
        return;
    }
    let mut profile_data = PROFILE_DATA.lock();
    if let Some(record) = profile_data.get_mut(&metadata.stack_hash) {
        record.free_bytes = record
            .free_bytes
            .saturating_add(metadata.requested_size.min(i64::MAX as usize) as i64);
        record.free_objects = record.free_objects.saturating_add(1);
        let (objects, bytes) =
            crate::profile::sample_weight(metadata.requested_size, metadata.sample_rate);
        record.scaled_free_objects += objects;
        record.scaled_free_bytes += bytes;
    }
}

/// A tracked operation owns the lock from metadata removal through the real
/// allocator call and completion. Removed metadata stays paired with its lock.
pub(super) struct PendingChange {
    transaction: ProfileTransaction,
    original_ptr: *mut std::ffi::c_void,
    metadata: Option<AllocationMetadata>,
}

impl PendingChange {
    pub(super) fn begin(ptr: *mut std::ffi::c_void) -> Option<Self> {
        if !metadata_store::contains_metadata(ptr) {
            return None;
        }
        let transaction = crate::profile::PROFILE_TRANSACTION.lock();
        let metadata = metadata_store::remove_metadata(ptr);
        Some(Self {
            transaction,
            original_ptr: ptr,
            metadata,
        })
    }

    pub(super) fn free(ptr: *mut std::ffi::c_void) -> Option<Self> {
        let mut pending = Self::begin(ptr)?;
        pending.record_free();
        Some(pending)
    }

    fn record_free(&mut self) {
        if let Some(metadata) = self.metadata.take() {
            record_free(metadata);
        }
    }

    fn finish(mut self, result: Reallocation) {
        match result {
            Reallocation::Failed => {
                if let Some(metadata) = self.metadata.take() {
                    if metadata.generation == crate::profile::GENERATION.load(Ordering::Relaxed) {
                        metadata_store::store_metadata(self.original_ptr, metadata);
                    }
                }
            }
            Reallocation::Freed => self.record_free(),
            Reallocation::Allocated { ptr, size } => {
                self.record_free();
                handle_malloc_in_transaction(ptr, size, Some(&self.transaction));
            }
        }
    }
}

pub(super) enum Reallocation {
    Failed,
    Freed,
    Allocated {
        ptr: *mut std::ffi::c_void,
        size: usize,
    },
}

impl Reallocation {
    pub(super) fn python(ptr: *mut std::ffi::c_void, size: usize) -> Self {
        if ptr.is_null() {
            Self::Failed
        } else {
            Self::Allocated { ptr, size }
        }
    }
    pub(super) fn native(
        old: *mut std::ffi::c_void,
        ptr: *mut std::ffi::c_void,
        size: usize,
    ) -> Self {
        if !old.is_null() && ptr.is_null() && size == 0 {
            Self::Freed
        } else {
            Self::python(ptr, size)
        }
    }
}

pub(super) fn finish_reallocation(pending: Option<PendingChange>, result: Reallocation) {
    if let Some(pending) = pending {
        pending.finish(result);
    } else if let Reallocation::Allocated { ptr, size } = result {
        handle_malloc(ptr, size);
    }
}

fn resolve_stack_key(
    records: &hashbrown::HashMap<u64, MemProfileRecord>,
    stack: &[StackFrame],
    mut key: u64,
) -> u64 {
    while let Some(record) = records.get(&key) {
        if record.stack.as_ref() == stack {
            break;
        }
        key = key.wrapping_add(1);
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::DEFAULT_SAMPLE_RATE;

    #[test]
    fn zero_size_realloc_distinguishes_native_free_from_python_failure() {
        let old = 0xDEAD_4000 as *mut std::ffi::c_void;
        let null = std::ptr::null_mut();
        assert!(matches!(
            Reallocation::native(old, null, 0),
            Reallocation::Freed
        ));
        assert!(matches!(
            Reallocation::python(null, 0),
            Reallocation::Failed
        ));
        assert!(matches!(
            Reallocation::native(null, null, 0),
            Reallocation::Failed
        ));
        assert!(matches!(
            Reallocation::native(old, null, 128),
            Reallocation::Failed
        ));
    }

    #[test]
    fn hash_collisions_do_not_merge_stacks() {
        let mut records = hashbrown::HashMap::new();
        let stack = vec![StackFrame::default()];
        records.insert(u64::MAX, MemProfileRecord::new(&[]));
        let key = resolve_stack_key(&records, &stack, u64::MAX);
        assert_eq!(key, 0);
        records.insert(key, MemProfileRecord::new(&stack));
        assert_eq!(resolve_stack_key(&records, &stack, u64::MAX), key);
    }

    #[test]
    fn realloc_failure_restores_metadata() {
        let _serial = crate::GLOBAL_TEST_LOCK.lock();
        let ptr = 0xDEAD_1000 as *mut std::ffi::c_void;
        let meta = AllocationMetadata {
            stack_hash: 0xABCD_0001,
            requested_size: 64,
            generation: crate::profile::GENERATION.load(Ordering::Relaxed),
            sample_rate: DEFAULT_SAMPLE_RATE,
        };
        metadata_store::store_metadata(ptr, meta);

        let pending = PendingChange::begin(ptr).expect("tracked allocation");
        assert!(metadata_store::remove_metadata(ptr).is_none());

        // Simulate the underlying realloc failing: old pointer is still live
        pending.finish(Reallocation::Failed);

        assert!(
            metadata_store::remove_metadata(ptr).is_some(),
            "metadata must be restored when realloc fails"
        );
    }

    #[test]
    fn realloc_success_credits_free_to_old_record() {
        let _serial = crate::GLOBAL_TEST_LOCK.lock();
        let stack_hash = 0xABCD_0002u64;
        let ptr = 0xDEAD_2000 as *mut std::ffi::c_void;
        let new_ptr = 0xDEAD_3000 as *mut std::ffi::c_void;

        PROFILE_DATA
            .lock()
            .insert(stack_hash, MemProfileRecord::new(&[]));
        metadata_store::store_metadata(
            ptr,
            AllocationMetadata {
                stack_hash,
                requested_size: 64,
                generation: crate::profile::GENERATION.load(Ordering::Relaxed),
                sample_rate: DEFAULT_SAMPLE_RATE,
            },
        );

        TRACING.store(true, Ordering::Release);
        let pending = PendingChange::begin(ptr).expect("tracked allocation");
        pending.finish(Reallocation::python(new_ptr, 128));
        TRACING.store(false, Ordering::Release);

        let profile_data = PROFILE_DATA.lock();
        let record = profile_data.get(&stack_hash).unwrap();
        assert_eq!(record.free_bytes, 64);
        assert_eq!(record.free_objects, 1);
        drop(profile_data);

        assert!(
            metadata_store::remove_metadata(ptr).is_none(),
            "old pointer metadata must be gone after successful realloc"
        );
    }
}
