//! Process-lived profiler mutexes with explicit post-fork reinitialization.
//! Unlike a userspace parking queue, pthread mutex state is entirely local to
//! the object and can be replaced in the single surviving child thread.
use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

pub struct Mutex<T> {
    raw: UnsafeCell<libc::pthread_mutex_t>,
    data: UnsafeCell<T>,
}

// Access to T is serialized; pthread mutexes support use by any thread.
unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            raw: UnsafeCell::new(libc::PTHREAD_MUTEX_INITIALIZER),
            data: UnsafeCell::new(data),
        }
    }

    // Requiring a static borrow prevents moving a pthread mutex after use.
    pub fn lock(&'static self) -> MutexGuard<'static, T> {
        self.acquire();
        MutexGuard {
            mutex: self,
            not_send: PhantomData,
        }
    }

    fn acquire(&self) {
        // SAFETY: raw storage is initialized and remains at a stable address.
        let status = unsafe { libc::pthread_mutex_lock(self.raw.get()) };
        assert_eq!(status, 0, "profiler mutex lock failed");
    }

    fn release(&self) {
        // SAFETY: only an owning guard or the matching fork parent releases it.
        let status = unsafe { libc::pthread_mutex_unlock(self.raw.get()) };
        assert_eq!(status, 0, "profiler mutex unlock failed");
    }

    /// # Safety
    /// All profiler mutexes must be held by the forking thread, no other
    /// thread may exist in the child, and no inherited guards may exist.
    unsafe fn reset_after_fork(&self) {
        self.raw.get().write(libc::PTHREAD_MUTEX_INITIALIZER);
    }
}

impl<T> Drop for Mutex<T> {
    fn drop(&mut self) {
        unsafe {
            libc::pthread_mutex_destroy(self.raw.get());
        }
    }
}

pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
    not_send: PhantomData<Rc<()>>,
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.data.get() }
    }
}
impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.data.get() }
    }
}
impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.release();
    }
}

/// Erase only the protected data type for the one process-owned fork inventory.
pub(crate) trait ForkMutex: Sync {
    /// # Safety
    /// Pair with parent release or child reset; do not access protected data.
    unsafe fn acquire_for_fork(&self);
    /// # Safety
    /// The forking parent must own this mutex from acquire_for_fork.
    unsafe fn release_after_fork(&self);
    /// # Safety
    /// Only the surviving child may reset a mutex acquired before the fork.
    unsafe fn reset_in_child(&self);
}
impl<T: Send> ForkMutex for Mutex<T> {
    unsafe fn acquire_for_fork(&self) {
        self.acquire();
    }
    unsafe fn release_after_fork(&self) {
        self.release();
    }
    unsafe fn reset_in_child(&self) {
        self.reset_after_fork();
    }
}
