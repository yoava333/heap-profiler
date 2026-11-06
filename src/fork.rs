//! Drain transactions before fork. Parent releases normally; child replaces
//! inherited mutex state, including waiter state from threads that disappeared.
use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use crate::allocator::{ALLOCATOR_CACHE, GLOBAL_PATCHER, SAVED_ALLOCATORS};
use crate::interner::STRING_INTERNER;
use crate::metadata_store::METADATA_SHARDS;
use crate::profile::{PROFILE_DATA, PROFILE_TRANSACTION};
use crate::sync::ForkGuard;

thread_local! {
    static PREVIOUS_RECURSION: Cell<bool> = const { Cell::new(false) };
    static HELD_LOCKS: RefCell<Vec<Box<dyn ForkGuard>>> = const { RefCell::new(Vec::new()) };
}
static ATFORK_INIT: OnceLock<libc::c_int> = OnceLock::new();

pub fn install_atfork_handlers() -> pyo3::PyResult<()> {
    let status = *ATFORK_INIT.get_or_init(|| unsafe {
        crate::stack::initialize_markers();
        once_cell::sync::Lazy::force(&GLOBAL_PATCHER);
        once_cell::sync::Lazy::force(&SAVED_ALLOCATORS);
        once_cell::sync::Lazy::force(&ALLOCATOR_CACHE);
        once_cell::sync::Lazy::force(&PROFILE_DATA);
        once_cell::sync::Lazy::force(&METADATA_SHARDS);
        once_cell::sync::Lazy::force(&STRING_INTERNER);
        libc::pthread_atfork(Some(prepare), Some(parent), Some(child))
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
    HELD_LOCKS.with(|slot| {
        let mut guards = slot.borrow_mut();
        assert!(guards.is_empty());
        guards.push(Box::new(PROFILE_TRANSACTION.lock()));
        guards.push(Box::new(GLOBAL_PATCHER.lock()));
        guards.push(Box::new(SAVED_ALLOCATORS.lock()));
        guards.push(Box::new(ALLOCATOR_CACHE.lock()));
        guards.push(Box::new(PROFILE_DATA.lock()));
        for shard in METADATA_SHARDS.iter() {
            guards.push(Box::new(shard.lock()));
        }
        guards.push(Box::new(STRING_INTERNER.lock()));
    });
}

extern "C" fn parent() {
    release(false);
}
extern "C" fn child() {
    release(true);
}

fn release(in_child: bool) {
    if in_child {
        // Only the forking thread survived. No old waiters may be woken or
        // retained, and no inherited guard may unlock the replacement mutexes.
        unsafe {
            PROFILE_TRANSACTION.reset_after_fork();
            GLOBAL_PATCHER.reset_after_fork();
            SAVED_ALLOCATORS.reset_after_fork();
            ALLOCATOR_CACHE.reset_after_fork();
            PROFILE_DATA.reset_after_fork();
            for shard in METADATA_SHARDS.iter() {
                shard.reset_after_fork();
            }
            STRING_INTERNER.reset_after_fork();
        }
    }
    HELD_LOCKS.with(|slot| {
        let mut guards = slot.borrow_mut();
        if in_child {
            for guard in guards.iter_mut() {
                guard.disarm_after_fork();
            }
        }
        while let Some(guard) = guards.pop() {
            drop(guard);
        }
    });
    PREVIOUS_RECURSION.with(|state| {
        crate::recursion_guard::replace_in_malloc(state.get());
    });
}
