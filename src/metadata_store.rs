use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;

// A wrapper around raw pointers that implements Send and Sync
#[derive(Clone, Copy, Hash, Eq, PartialEq)]
pub struct ThreadSafePtr(*mut std::ffi::c_void);

// Safety: We ensure thread-safety through the Mutex
unsafe impl Send for ThreadSafePtr {}
unsafe impl Sync for ThreadSafePtr {}

#[derive(Clone, Copy)]
pub struct AllocationMetadata {
    pub magic: u64,
    pub stack_hash: u64,
    pub requested_size: usize,
    pub total_size: usize,
}

pub static METADATA_STORE: Lazy<Mutex<HashMap<ThreadSafePtr, AllocationMetadata>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub const HEAP_PROFILER_MAGIC_NUMBER: u64 = 0xDEADBEEF;

pub fn store_metadata(ptr: *mut std::ffi::c_void, metadata: AllocationMetadata) {
    if let Ok(mut store) = METADATA_STORE.lock() {
        store.insert(ThreadSafePtr(ptr), metadata);
    }
}

pub fn get_metadata(ptr: *mut std::ffi::c_void) -> Option<AllocationMetadata> {
    METADATA_STORE
        .lock()
        .ok()?
        .get(&ThreadSafePtr(ptr))
        .copied()
}

pub fn remove_metadata(ptr: *mut std::ffi::c_void) -> Option<AllocationMetadata> {
    METADATA_STORE.lock().ok()?.remove(&ThreadSafePtr(ptr))
}
