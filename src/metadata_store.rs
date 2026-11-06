use crate::sync::Mutex;
use hashbrown::HashMap;
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicBool, Ordering};

// A wrapper around raw pointers that implements Send and Sync
#[derive(Clone, Copy, Hash, Eq, PartialEq)]
pub struct ThreadSafePtr(*mut std::ffi::c_void);

// Safety: We ensure thread-safety through the Mutex
unsafe impl Send for ThreadSafePtr {}
unsafe impl Sync for ThreadSafePtr {}

#[derive(Clone, Copy)]
pub struct AllocationMetadata {
    pub stack_hash: u64,
    pub requested_size: usize,
    pub generation: u64,
    pub sample_rate: i32,
}

/// Number of independent shards. Every free in the process takes one of these
/// locks, so a single global mutex would serialize the entire free path.
pub const SHARD_COUNT: usize = 64;

pub struct MetadataShard {
    entries: Mutex<HashMap<ThreadSafePtr, AllocationMetadata>>,
    occupied: AtomicBool,
}

impl MetadataShard {
    pub(crate) fn lock(
        &'static self,
    ) -> crate::sync::MutexGuard<'static, HashMap<ThreadSafePtr, AllocationMetadata>> {
        self.entries.lock()
    }
    pub(crate) unsafe fn reset_after_fork(&self) {
        self.entries.reset_after_fork();
    }
}

pub static METADATA_SHARDS: Lazy<Vec<MetadataShard>> = Lazy::new(|| {
    (0..SHARD_COUNT)
        .map(|_| MetadataShard {
            entries: Mutex::new(HashMap::new()),
            occupied: AtomicBool::new(false),
        })
        .collect()
});

#[inline]
fn shard_for(ptr: *mut std::ffi::c_void) -> &'static MetadataShard {
    &METADATA_SHARDS[(ptr as usize >> 4) % SHARD_COUNT]
}

pub fn store_metadata(ptr: *mut std::ffi::c_void, metadata: AllocationMetadata) {
    let shard = shard_for(ptr);
    let mut entries = shard.entries.lock();
    entries.insert(ThreadSafePtr(ptr), metadata);
    // Publish before returning the allocated pointer to its owner. No free
    // of a tracked live pointer may observe an empty shard after publication.
    shard.occupied.store(true, Ordering::Release);
}

pub fn remove_metadata(ptr: *mut std::ffi::c_void) -> Option<AllocationMetadata> {
    let shard = shard_for(ptr);
    if !shard.occupied.load(Ordering::Acquire) {
        return None;
    }
    let mut entries = shard.entries.lock();
    let metadata = entries.remove(&ThreadSafePtr(ptr));
    if entries.is_empty() {
        shard.occupied.store(false, Ordering::Release);
    }
    metadata
}

pub fn clear() {
    for shard in METADATA_SHARDS.iter() {
        let mut entries = shard.entries.lock();
        entries.clear();
        shard.occupied.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_remove_across_shards() {
        let _serial = crate::GLOBAL_TEST_LOCK.lock();
        // Pointers chosen to land in different shards
        let ptrs: Vec<*mut std::ffi::c_void> = (0..256)
            .map(|i| (i * 16 + 0x1000_0000) as *mut std::ffi::c_void)
            .collect();
        for (i, &ptr) in ptrs.iter().enumerate() {
            store_metadata(
                ptr,
                AllocationMetadata {
                    stack_hash: i as u64,
                    requested_size: i,
                    generation: 0,
                    sample_rate: 1,
                },
            );
        }
        for (i, &ptr) in ptrs.iter().enumerate() {
            let meta = remove_metadata(ptr).expect("metadata must be found");
            assert_eq!(meta.stack_hash, i as u64);
            assert_eq!(meta.requested_size, i);
        }
        assert!(
            remove_metadata(ptrs[0]).is_none(),
            "second remove must miss"
        );
    }

    #[test]
    fn clear_empties_all_shards() {
        let _serial = crate::GLOBAL_TEST_LOCK.lock();
        let ptr = 0x2000_0000 as *mut std::ffi::c_void;
        store_metadata(
            ptr,
            AllocationMetadata {
                stack_hash: 7,
                requested_size: 7,
                generation: 0,
                sample_rate: 1,
            },
        );
        clear();
        assert!(remove_metadata(ptr).is_none());
    }
}
