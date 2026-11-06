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
        let status = unsafe { libc::pthread_mutex_lock(self.raw.get()) };
        assert_eq!(status, 0, "profiler mutex lock failed");
        MutexGuard {
            mutex: self,
            unlock: true,
            not_send: PhantomData,
        }
    }

    /// All profiler mutexes must be held by the forking thread, no other
    /// thread may exist in the child, and inherited guards must be disarmed.
    pub unsafe fn reset_after_fork(&self) {
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
    unlock: bool,
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
        if self.unlock {
            let status = unsafe { libc::pthread_mutex_unlock(self.mutex.raw.get()) };
            assert_eq!(status, 0, "profiler mutex unlock failed");
        }
    }
}

pub(crate) trait ForkGuard {
    fn disarm_after_fork(&mut self);
}
impl<T> ForkGuard for MutexGuard<'_, T> {
    fn disarm_after_fork(&mut self) {
        self.unlock = false;
    }
}
