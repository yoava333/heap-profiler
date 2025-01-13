use hashbrown::HashMap;
use once_cell::sync::Lazy;
use std::sync::Mutex;

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

pub static METADATA_STORE: Lazy<Mutex<HashMap<ThreadSafePtr, AllocationMetadata>>> =
    Lazy::new(|| Mutex::new(HashMap::with_capacity(179999)));

pub fn store_metadata(ptr: *mut std::ffi::c_void, metadata: AllocationMetadata) {
    if let Ok(mut store) = METADATA_STORE.lock() {
        store.insert(ThreadSafePtr(ptr), metadata);
    }
}

pub fn remove_metadata(ptr: *mut std::ffi::c_void) -> Option<AllocationMetadata> {
    METADATA_STORE.lock().ok()?.remove(&ThreadSafePtr(ptr))
}
