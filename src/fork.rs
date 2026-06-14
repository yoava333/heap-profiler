//! Fork safety: a child process inherits mutexes in whatever state they were
//! in at fork() time. If another thread held a profiler lock, the child would
//! deadlock on its first allocation or free. The classic fix: acquire every
//! profiler lock before fork and release it in both parent and child.
//!
//! parking_lot mutexes are a single word with no owner thread, so force_unlock
//! from the post-fork handlers is sound.

use parking_lot::lock_api::RawMutex as _;
use std::sync::Once;

use crate::allocator::GLOBAL_PATCHER;
use crate::interner::STRING_INTERNER;
use crate::metadata_store::METADATA_SHARDS;
use crate::profile::PROFILE_DATA;

static ATFORK_INIT: Once = Once::new();

pub fn install_atfork_handlers() {
    ATFORK_INIT.call_once(|| unsafe {
        libc::pthread_atfork(Some(prepare), Some(release_all), Some(release_all));
    });
}

// Lock order must be identical here and nowhere else may two of these locks
// be held at once (they aren't: each call site takes exactly one).
extern "C" fn prepare() {
    unsafe {
        GLOBAL_PATCHER.raw().lock();
        PROFILE_DATA.raw().lock();
        for shard in METADATA_SHARDS.iter() {
            shard.raw().lock();
        }
        STRING_INTERNER.raw().lock();
    }
}

extern "C" fn release_all() {
    unsafe {
        STRING_INTERNER.force_unlock();
        for shard in METADATA_SHARDS.iter() {
            shard.force_unlock();
        }
        PROFILE_DATA.force_unlock();
        GLOBAL_PATCHER.force_unlock();
    }
}
