use std::cell::Cell;

struct CallbackState {
    in_malloc: Cell<bool>,
    errno_location: Cell<*mut libc::c_int>,
}

thread_local! {
    // No destructor: native TLS destructors can still invoke our callbacks.
    static CALLBACK_STATE: CallbackState = const { CallbackState {
        in_malloc: Cell::new(false),
        errno_location: Cell::new(std::ptr::null_mut()),
    } };
}

pub struct MallocRecursionGuard {
    was_in_malloc: bool,
    state: std::ptr::NonNull<CallbackState>,
}

impl MallocRecursionGuard {
    pub fn new() -> Self {
        CALLBACK_STATE.with(|state| {
            let was_in_malloc = state.in_malloc.get();
            if !was_in_malloc {
                state.in_malloc.set(true);
            }
            Self {
                was_in_malloc,
                state: std::ptr::NonNull::from(state),
            }
        })
    }

    pub fn was_in_malloc(&self) -> bool {
        self.was_in_malloc
    }

    pub(crate) fn preserve_errno(&self) -> ErrnoGuard {
        let state = self.state();
        let mut location = state.errno_location.get();
        if location.is_null() {
            // libc's errno storage has a stable address for this thread's
            // lifetime, including native TLS teardown and the fork child.
            location = unsafe {
                #[cfg(target_os = "linux")]
                {
                    libc::__errno_location()
                }
                #[cfg(target_os = "macos")]
                {
                    libc::__error()
                }
            };
            state.errno_location.set(location);
        }
        ErrnoGuard {
            location,
            // SAFETY: location is this thread's libc errno storage.
            value: unsafe { *location },
        }
    }

    fn state(&self) -> &CallbackState {
        // SAFETY: const TLS with no destructor remains at a stable address on
        // this thread. NonNull makes this guard !Send; the borrow cannot escape
        // it. Reusing the address also avoids resolving TLS again during drop.
        unsafe { self.state.as_ref() }
    }
}

impl Drop for MallocRecursionGuard {
    fn drop(&mut self) {
        if !self.was_in_malloc {
            self.state().in_malloc.set(false);
        }
    }
}

/// Restore the allocator's current errno, using its thread's cached address.
pub(crate) struct ErrnoGuard {
    location: *mut libc::c_int,
    value: libc::c_int,
}

impl Drop for ErrnoGuard {
    fn drop(&mut self) {
        // SAFETY: the address belongs to this thread, and this guard is !Send.
        unsafe {
            *self.location = self.value;
        }
    }
}

// GC Disable
pub struct PythonGCGuard {
    was_gc_enabled: bool,
}

impl PythonGCGuard {
    pub fn new() -> Self {
        #[cfg(Py_3_10)]
        unsafe {
            use pyo3_ffi::{PyGC_Disable, PyGC_IsEnabled};
            let was_gc_enabled = PyGC_IsEnabled() == 1;
            if was_gc_enabled {
                PyGC_Disable();
            }
            Self { was_gc_enabled }
        }
        #[cfg(not(Py_3_10))]
        Self {
            was_gc_enabled: false,
        }
    }
}

impl Drop for PythonGCGuard {
    fn drop(&mut self) {
        #[cfg(Py_3_10)]
        unsafe {
            use pyo3_ffi::PyGC_Enable;
            if self.was_gc_enabled {
                PyGC_Enable();
            }
        }
    }
}

/// Used by pthread_atfork: suppression spans prepare through parent/child.
pub(crate) fn replace_in_malloc(value: bool) -> bool {
    CALLBACK_STATE.with(|state| state.in_malloc.replace(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errno_snapshots_follow_each_threads_current_value() {
        let threads: Vec<_> = [libc::EDOM, libc::EINVAL]
            .into_iter()
            .map(|sentinel| {
                std::thread::spawn(move || {
                    let guard = MallocRecursionGuard::new();
                    let original = guard.preserve_errno();
                    let location = original.location;
                    unsafe {
                        *location = sentinel;
                    }
                    {
                        let nested = MallocRecursionGuard::new();
                        assert!(nested.was_in_malloc());
                        let _saved = nested.preserve_errno();
                        unsafe {
                            *location = libc::ERANGE;
                        }
                    }
                    assert_eq!(unsafe { *location }, sentinel);
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    }
}
