use hashbrown::HashMap;
use once_cell::sync::Lazy;
use parking_lot::Mutex;

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
}

/// Number of independent shards. Every free in the process takes one of these
/// locks, so a single global mutex would serialize the entire free path.
pub const SHARD_COUNT: usize = 64;

pub static METADATA_SHARDS: Lazy<Vec<Mutex<HashMap<ThreadSafePtr, AllocationMetadata>>>> =
    Lazy::new(|| {
        (0..SHARD_COUNT)
            .map(|_| Mutex::new(HashMap::new()))
            .collect()
    });

#[inline]
fn shard_for(
    ptr: *mut std::ffi::c_void,
) -> &'static Mutex<HashMap<ThreadSafePtr, AllocationMetadata>> {
    // Low 4 bits are alignment zeros; use the next bits to spread pointers
    &METADATA_SHARDS[(ptr as usize >> 4) % SHARD_COUNT]
}

pub fn store_metadata(ptr: *mut std::ffi::c_void, metadata: AllocationMetadata) {
    shard_for(ptr).lock().insert(ThreadSafePtr(ptr), metadata);
}

pub fn remove_metadata(ptr: *mut std::ffi::c_void) -> Option<AllocationMetadata> {
    shard_for(ptr).lock().remove(&ThreadSafePtr(ptr))
}

pub fn clear() {
    for shard in METADATA_SHARDS.iter() {
        shard.lock().clear();
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
            .map(|i| ((i * 16 + 0x1000_0000) as *mut std::ffi::c_void))
            .collect();
        for (i, &ptr) in ptrs.iter().enumerate() {
            store_metadata(
                ptr,
                AllocationMetadata {
                    stack_hash: i as u64,
                    requested_size: i,
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
            },
        );
        clear();
        assert!(remove_metadata(ptr).is_none());
    }
}
