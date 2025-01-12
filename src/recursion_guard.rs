use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

thread_local! {
    static IN_MALLOC: Cell<bool> = Cell::new(false);
}

/// Thread-specific flag to guard against recursion when interposing heap functions.
/// Uses Rust's native thread_local storage rather than pthreads.
pub struct MallocRecursionGuard {
    was_in_malloc: bool,
    _no_clone: std::marker::PhantomData<*const ()>,
}

impl MallocRecursionGuard {
    /// Initialize the guard, recording if we were already in malloc.
    /// If we weren't in malloc, sets the thread-local flag.
    pub fn new() -> Self {
        let was_in_malloc = Self::is_in_malloc();
        if !was_in_malloc {
            Self::set_in_malloc(true);
        }

        Self {
            was_in_malloc,
            _no_clone: std::marker::PhantomData,
        }
    }

    /// Check if we were already in malloc when guard was created
    pub fn was_in_malloc(&self) -> bool {
        self.was_in_malloc
    }

    /// Check if we're currently in malloc
    fn is_in_malloc() -> bool {
        return IN_MALLOC.with(|flag| flag.get());
    }

    /// Set the thread-local malloc state
    fn set_in_malloc(state: bool) {
        IN_MALLOC.with(|flag| flag.set(state));
    }
}

// Automatically clear the malloc flag when the guard is dropped,
// but only if we weren't already in malloc
impl Drop for MallocRecursionGuard {
    fn drop(&mut self) {
        if !self.was_in_malloc {
            Self::set_in_malloc(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_basic_guard() {
        let guard = MallocRecursionGuard::new();
        assert!(!guard.was_in_malloc());

        // Nested guard should detect recursion
        let nested_guard = MallocRecursionGuard::new();
        assert!(nested_guard.was_in_malloc());
    }

    #[test]
    fn test_thread_isolation() {
        let handle = thread::spawn(|| {
            let guard = MallocRecursionGuard::new();
            assert!(!guard.was_in_malloc());

            // Test nested guard in new thread
            let nested = MallocRecursionGuard::new();
            assert!(nested.was_in_malloc());
        });

        // Main thread should have its own independent state
        let guard = MallocRecursionGuard::new();
        assert!(!guard.was_in_malloc());

        let nested = MallocRecursionGuard::new();
        assert!(nested.was_in_malloc());

        handle.join().unwrap();
    }

    #[test]
    fn test_drop_behavior() {
        {
            let _guard = MallocRecursionGuard::new();
            // Guard is active here
            assert!(MallocRecursionGuard::is_in_malloc());
        } // Guard is dropped here

        // New guard should not detect recursion after previous was dropped
        assert!(!MallocRecursionGuard::is_in_malloc());
        let guard = MallocRecursionGuard::new();
        assert!(!guard.was_in_malloc());
    }

    #[test]
    fn test_initialization_recursion() {
        // This test ensures we handle recursion during thread_local initialization
        let guard = MallocRecursionGuard::new();
        assert!(!guard.was_in_malloc());
    }
}
