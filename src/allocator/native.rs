use super::recording::{finish_reallocation, handle_malloc, PendingChange, Reallocation};
use super::NATIVE_ENABLED;
use crate::platform::{Patcher, PlatformPatcher};
use crate::recursion_guard::MallocRecursionGuard;
use crate::sync::Mutex;
use once_cell::sync::Lazy;
use pyo3::prelude::*;
use std::sync::atomic::{AtomicPtr, Ordering};
static ORIGINAL_MALLOC: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_FREE: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_REALLOC: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_CALLOC: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_POSIX_MEMALIGN: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_ALIGNED_ALLOC: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());

pub(crate) static GLOBAL_PATCHER: Lazy<Mutex<Option<PlatformPatcher>>> =
    Lazy::new(|| Mutex::new(None));

#[no_mangle]
unsafe extern "C" fn handle_native_malloc(size: libc::size_t) -> *mut libc::c_void {
    let _guard = MallocRecursionGuard::new();
    let original: unsafe extern "C" fn(libc::size_t) -> *mut libc::c_void =
        std::mem::transmute(ORIGINAL_MALLOC.load(Ordering::Relaxed));
    let result = original(size);
    let _errno = (!_guard.was_in_malloc()).then(|| _guard.preserve_errno());

    if !_guard.was_in_malloc() && NATIVE_ENABLED.load(Ordering::Acquire) && !result.is_null() {
        handle_malloc(result, size);
    }

    result
}

#[no_mangle]
unsafe extern "C" fn handle_native_free(ptr: *mut libc::c_void) {
    let guard = MallocRecursionGuard::new();
    let _errno = (!guard.was_in_malloc()).then(|| guard.preserve_errno());

    // We need to call our API before we call the real free implementation
    // to make sure that the pointer is not reused in-between.
    let _transaction = if !guard.was_in_malloc() && NATIVE_ENABLED.load(Ordering::Acquire) {
        PendingChange::free(ptr)
    } else {
        None
    };

    // Call the original free implementation
    let original: unsafe extern "C" fn(*mut libc::c_void) =
        std::mem::transmute(ORIGINAL_FREE.load(Ordering::Relaxed));
    original(ptr);
}

#[no_mangle]
unsafe extern "C" fn handle_native_calloc(
    nelem: libc::size_t,
    elsize: libc::size_t,
) -> *mut libc::c_void {
    let guard = MallocRecursionGuard::new();
    let original: unsafe extern "C" fn(libc::size_t, libc::size_t) -> *mut libc::c_void =
        std::mem::transmute(ORIGINAL_CALLOC.load(Ordering::Relaxed));
    let result = original(nelem, elsize);
    let _errno = (!guard.was_in_malloc()).then(|| guard.preserve_errno());

    if !guard.was_in_malloc() && NATIVE_ENABLED.load(Ordering::Acquire) && !result.is_null() {
        handle_malloc(result, nelem * elsize);
    }

    result
}

#[no_mangle]
unsafe extern "C" fn handle_native_realloc(
    ptr: *mut libc::c_void,
    size: libc::size_t,
) -> *mut libc::c_void {
    let guard = MallocRecursionGuard::new();
    let tracking = !guard.was_in_malloc() && NATIVE_ENABLED.load(Ordering::Acquire);
    let pending = if tracking {
        PendingChange::begin(ptr)
    } else {
        None
    };
    let original: unsafe extern "C" fn(*mut libc::c_void, libc::size_t) -> *mut libc::c_void =
        std::mem::transmute(ORIGINAL_REALLOC.load(Ordering::Relaxed));
    let result = original(ptr, size);
    let _errno = (!guard.was_in_malloc()).then(|| guard.preserve_errno());

    if tracking {
        finish_reallocation(pending, Reallocation::native(ptr, result, size));
    }

    result
}

#[no_mangle]
unsafe extern "C" fn handle_native_posix_memalign(
    memptr: *mut *mut libc::c_void,
    alignment: libc::size_t,
    size: libc::size_t,
) -> libc::c_int {
    let guard = MallocRecursionGuard::new();
    let original: unsafe extern "C" fn(
        *mut *mut libc::c_void,
        libc::size_t,
        libc::size_t,
    ) -> libc::c_int = std::mem::transmute(ORIGINAL_POSIX_MEMALIGN.load(Ordering::Relaxed));
    let result = original(memptr, alignment, size);
    let _errno = (!guard.was_in_malloc()).then(|| guard.preserve_errno());

    if !guard.was_in_malloc()
        && NATIVE_ENABLED.load(Ordering::Acquire)
        && result == 0
        && !memptr.is_null()
        && !(*memptr).is_null()
    {
        handle_malloc(*memptr, size);
    }

    result
}

#[no_mangle]
unsafe extern "C" fn handle_native_aligned_alloc(
    alignment: libc::size_t,
    size: libc::size_t,
) -> *mut libc::c_void {
    let guard = MallocRecursionGuard::new();
    let original: unsafe extern "C" fn(libc::size_t, libc::size_t) -> *mut libc::c_void =
        std::mem::transmute(ORIGINAL_ALIGNED_ALLOC.load(Ordering::Relaxed));
    let result = original(alignment, size);
    let _errno = (!guard.was_in_malloc()).then(|| guard.preserve_errno());

    if !guard.was_in_malloc() && NATIVE_ENABLED.load(Ordering::Acquire) && !result.is_null() {
        handle_malloc(result, size);
    }

    result
}

pub(super) fn init_native_hooks() -> PyResult<()> {
    unsafe {
        // glibc RTLD_DEFAULT may return the executable's canonical PLT stub
        // rather than the libc implementation. Patching that stub's GOT and
        // calling it as our original would recurse into this hook.
        #[cfg(target_os = "linux")]
        let handle = libc::RTLD_NEXT;
        #[cfg(target_os = "macos")]
        let handle = libc::RTLD_DEFAULT;
        ORIGINAL_MALLOC.store(libc::dlsym(handle, c"malloc".as_ptr()), Ordering::Relaxed);
        ORIGINAL_FREE.store(libc::dlsym(handle, c"free".as_ptr()), Ordering::Relaxed);
        ORIGINAL_REALLOC.store(libc::dlsym(handle, c"realloc".as_ptr()), Ordering::Relaxed);
        ORIGINAL_CALLOC.store(libc::dlsym(handle, c"calloc".as_ptr()), Ordering::Relaxed);
        ORIGINAL_POSIX_MEMALIGN.store(
            libc::dlsym(handle, c"posix_memalign".as_ptr()),
            Ordering::Relaxed,
        );
        ORIGINAL_ALIGNED_ALLOC.store(
            libc::dlsym(handle, c"aligned_alloc".as_ptr()),
            Ordering::Relaxed,
        );
    }

    if [
        &ORIGINAL_MALLOC,
        &ORIGINAL_FREE,
        &ORIGINAL_REALLOC,
        &ORIGINAL_CALLOC,
    ]
    .iter()
    .any(|symbol| symbol.load(Ordering::Relaxed).is_null())
    {
        return Err(pyo3::exceptions::PyRuntimeError::new_err(
            "Could not resolve the native allocator functions",
        ));
    }

    // Initialize global patcher if not already initialized
    let mut patcher_guard = GLOBAL_PATCHER.lock();
    if patcher_guard.is_none() {
        let mut patcher = PlatformPatcher::new("heap_profiler".to_string());
        use crate::platform::Hook;
        patcher.register_hook(Hook {
            symbol_name: "malloc",
            original_fn: ORIGINAL_MALLOC.load(Ordering::Relaxed),
            intercept_fn: handle_native_malloc as *mut libc::c_void,
        });
        patcher.register_hook(Hook {
            symbol_name: "free",
            original_fn: ORIGINAL_FREE.load(Ordering::Relaxed),
            intercept_fn: handle_native_free as *mut libc::c_void,
        });
        patcher.register_hook(Hook {
            symbol_name: "realloc",
            original_fn: ORIGINAL_REALLOC.load(Ordering::Relaxed),
            intercept_fn: handle_native_realloc as *mut libc::c_void,
        });
        patcher.register_hook(Hook {
            symbol_name: "calloc",
            original_fn: ORIGINAL_CALLOC.load(Ordering::Relaxed),
            intercept_fn: handle_native_calloc as *mut libc::c_void,
        });

        // Not present on every platform/libc: register only when found
        if !ORIGINAL_POSIX_MEMALIGN.load(Ordering::Relaxed).is_null() {
            patcher.register_hook(Hook {
                symbol_name: "posix_memalign",
                original_fn: ORIGINAL_POSIX_MEMALIGN.load(Ordering::Relaxed),
                intercept_fn: handle_native_posix_memalign as *mut libc::c_void,
            });
        }
        if !ORIGINAL_ALIGNED_ALLOC.load(Ordering::Relaxed).is_null() {
            patcher.register_hook(Hook {
                symbol_name: "aligned_alloc",
                original_fn: ORIGINAL_ALIGNED_ALLOC.load(Ordering::Relaxed),
                intercept_fn: handle_native_aligned_alloc as *mut libc::c_void,
            });
        }
        *patcher_guard = Some(patcher);
    }

    // Apply hooks
    if let Some(patcher) = patcher_guard.as_mut() {
        patcher.apply_hooks();
    }
    Ok(())
}

pub(super) fn refresh_hooks() {
    if super::TRACING.load(Ordering::Acquire) && NATIVE_ENABLED.load(Ordering::Acquire) {
        if let Some(patcher) = GLOBAL_PATCHER.lock().as_mut() {
            patcher.apply_hooks();
        }
    }
}
pub(super) fn restore_hooks() {
    let mut patcher = GLOBAL_PATCHER.lock();
    if NATIVE_ENABLED.swap(false, Ordering::AcqRel) {
        if let Some(patcher) = patcher.as_mut() {
            patcher.restore_hooks();
        }
    }
}
