//! One process-owned lock inventory coordinates fork without destructor-managed
//! TLS. Native thread-local destructors may fork after Rust TLS has been torn down.
use once_cell::sync::Lazy;
use std::cell::Cell;
use std::sync::OnceLock;

use crate::allocator::{ALLOCATOR_REGISTRY, GLOBAL_PATCHER, SAVED_ALLOCATORS};
use crate::interner::STRING_INTERNER;
use crate::metadata_store::METADATA_SHARDS;
use crate::profile::{PROFILE_DATA, PROFILE_TRANSACTION};
use crate::sync::ForkMutex;

thread_local! {
    static PREVIOUS_RECURSION: Cell<bool> = const { Cell::new(false) };
}
static ATFORK_INIT: OnceLock<libc::c_int> = OnceLock::new();
static FORK_MUTEXES: Lazy<Vec<&'static dyn ForkMutex>> = Lazy::new(|| {
    let mut locks: Vec<&'static dyn ForkMutex> = vec![
        &PROFILE_TRANSACTION,
        &*GLOBAL_PATCHER,
        &*SAVED_ALLOCATORS,
        &ALLOCATOR_REGISTRY,
        &*PROFILE_DATA,
    ];
    locks.extend(METADATA_SHARDS.iter().map(|shard| shard.fork_mutex()));
    locks.push(&*STRING_INTERNER);
    locks
});

pub fn install_atfork_handlers() -> pyo3::PyResult<()> {
    let status = *ATFORK_INIT.get_or_init(|| {
        crate::stack::initialize_markers();
        Lazy::force(&FORK_MUTEXES);
        // SAFETY: all process-owned mutexes and marker strings are initialized;
        // handlers use no drop-managed TLS or per-fork heap bookkeeping.
        unsafe { libc::pthread_atfork(Some(prepare), Some(parent), Some(child)) }
    });
    if status != 0 {
        return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
            "Could not install fork handlers: {}",
            std::io::Error::from_raw_os_error(status)
        )));
    }
    Ok(())
}

extern "C" fn prepare() {
    PREVIOUS_RECURSION.with(|state| state.set(crate::recursion_guard::replace_in_malloc(true)));
    for lock in FORK_MUTEXES.iter() {
        // SAFETY: acquire the fixed inventory in its documented order before
        // fork; parent/child handlers finish this ownership protocol.
        unsafe {
            lock.acquire_for_fork();
        }
    }
}

extern "C" fn parent() {
    for lock in FORK_MUTEXES.iter().rev() {
        // SAFETY: prepare acquired this mutex on the same surviving parent thread.
        unsafe {
            lock.release_after_fork();
        }
    }
    restore_recursion();
}

extern "C" fn child() {
    for lock in FORK_MUTEXES.iter() {
        // SAFETY: only the child thread survived, prepare owned every mutex,
        // and no guard exists that could unlock replacement mutex state.
        unsafe {
            lock.reset_in_child();
        }
    }
    crate::allocator::reset_sampler_after_fork();
    restore_recursion();
}

fn restore_recursion() {
    PREVIOUS_RECURSION.with(|state| {
        crate::recursion_guard::replace_in_malloc(state.get());
    });
}
