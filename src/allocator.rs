use crate::metadata_store::{self, AllocationMetadata};
use crate::platform::{Patcher, PlatformPatcher};
use crate::profile::{
    MemProfileRecord, DEFAULT_SAMPLE_RATE, PROFILE_DATA, SAMPLE_RATE, TOTAL_ALLOC_BYTES,
};
use crate::stack::{calculate_stack_hash, where_in_python, StackFrame};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use pyo3::ffi;
use pyo3::prelude::*;
use rand::Rng;
use std::cell::Cell;
use std::sync::atomic::{AtomicPtr, Ordering};
use tracing::{trace, warn};

use crate::recursion_guard::MallocRecursionGuard;

static ORIGINAL_MALLOC: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_FREE: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_REALLOC: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_CALLOC: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_DLOPEN: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_POSIX_MEMALIGN: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_ALIGNED_ALLOC: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());

// aka "SMALL_REQUEST_THRESHOLD" in cpython/Objects/obmalloc.c
const PYMALLOC_MAX_SIZE: usize = 512;

const ARENA_ALLOCATOR_ENABLED: bool = false;

pub(crate) static GLOBAL_PATCHER: Lazy<Mutex<Option<PlatformPatcher>>> =
    Lazy::new(|| Mutex::new(None));

struct SavedAllocators {
    raw: ffi::PyMemAllocatorEx,
    mem: ffi::PyMemAllocatorEx,
    obj: ffi::PyMemAllocatorEx,
}
// SAFETY: the raw pointers inside are CPython allocator state, valid for the
// process lifetime; access is serialized by the Mutex.
unsafe impl Send for SavedAllocators {}

static SAVED_ALLOCATORS: Lazy<Mutex<Option<SavedAllocators>>> = Lazy::new(|| Mutex::new(None));

thread_local! {
    static NEXT_SAMPLE: Cell<i64> = Cell::new(0);
}

#[derive(Clone)]
pub struct MyAllocator {
    pub original_allocator: ffi::PyMemAllocatorEx,
    pub domain: ffi::PyMemAllocatorDomain,
}

impl MyAllocator {
    pub fn new(
        original_allocator: ffi::PyMemAllocatorEx,
        domain: ffi::PyMemAllocatorDomain,
    ) -> Self {
        Self {
            original_allocator,
            domain,
        }
    }

    pub fn get_allocator_ex(&self) -> ffi::PyMemAllocatorEx {
        ffi::PyMemAllocatorEx {
            ctx: std::ptr::null_mut(),
            malloc: Some(my_malloc),
            calloc: Some(my_calloc),
            realloc: Some(my_realloc),
            free: Some(my_free),
        }
    }
}

pub fn get_default_allocator() -> ffi::PyMemAllocatorEx {
    ffi::PyMemAllocatorEx {
        ctx: std::ptr::null_mut(),
        malloc: None,
        calloc: None,
        realloc: None,
        free: None,
    }
}

// Helper functions for sampling
fn fast_exp_rand(mean: i32) -> i64 {
    if mean <= 0 {
        return 0;
    }

    let capped_mean = mean.min(0x7000000);
    let random_bits = 26;
    let q = (rand::thread_rng().gen::<u32>() % (1 << random_bits)) + 1;
    let q_log = (q as f64).log2() - random_bits as f64;
    let q_log = q_log.min(0.0);

    const MINUS_LOG2: f64 = -0.6931471805599453;
    (q_log * MINUS_LOG2 * capped_mean as f64) as i64 + 1
}

#[inline]
fn should_sample(size: usize) -> bool {
    // Always sample large allocations
    NEXT_SAMPLE.with(|next| {
        let current = next.get();
        if current <= size as i64 {
            // Reset the counter with exponential distribution
            next.set(fast_exp_rand(SAMPLE_RATE.load(Ordering::Relaxed)));
            true
        } else {
            next.set(current - size as i64);
            false
        }
    })
}

fn handle_malloc(ptr: *mut std::ffi::c_void, size: usize) {
    TOTAL_ALLOC_BYTES.fetch_add(size as u64, Ordering::Relaxed);

    let should_profile = should_sample(size);
    if should_profile {
        // Record the stack trace
        let mut stack = std::array::from_fn(|_| StackFrame::default());
        let depth = where_in_python(&mut stack);

        // Calculate stack hash
        let stack_hash = calculate_stack_hash(&stack[..depth]);

        let metadata = AllocationMetadata {
            stack_hash,
            requested_size: size,
        };

        metadata_store::store_metadata(ptr, metadata);

        // Update profile data
        let mut profile_data = PROFILE_DATA.lock();
        let record = profile_data
            .entry(stack_hash)
            .or_insert_with(|| MemProfileRecord::new(&stack[..depth]));
        record.alloc_bytes += size as i64;
        record.alloc_objects += 1;
    }
}

fn record_free(metadata: AllocationMetadata) {
    let mut profile_data = PROFILE_DATA.lock();
    if let Some(record) = profile_data.get_mut(&metadata.stack_hash) {
        record.free_bytes += metadata.requested_size as i64;
        record.free_objects += 1;
    }
}

fn handle_free(ptr: *mut std::ffi::c_void) {
    if let Some(metadata) = metadata_store::remove_metadata(ptr) {
        record_free(metadata);
    }
}

/// Metadata for `ptr` must be removed BEFORE the underlying realloc/free runs:
/// once the allocator owns the pointer again, another thread can immediately
/// reuse the address and a late removal would delete the new allocation's
/// metadata.
fn handle_realloc_start(ptr: *mut std::ffi::c_void) -> Option<AllocationMetadata> {
    metadata_store::remove_metadata(ptr)
}

fn handle_realloc_end(
    tracked: Option<AllocationMetadata>,
    old_ptr: *mut std::ffi::c_void,
    new_ptr: *mut std::ffi::c_void,
    size: usize,
) {
    if new_ptr.is_null() {
        // realloc failed: the old allocation is still live, restore its metadata
        if let Some(metadata) = tracked {
            metadata_store::store_metadata(old_ptr, metadata);
        }
        return;
    }
    if let Some(metadata) = tracked {
        record_free(metadata);
    }
    handle_malloc(new_ptr, size);
}

#[no_mangle]
unsafe extern "C" fn handle_native_malloc(size: libc::size_t) -> *mut libc::c_void {
    let _guard = MallocRecursionGuard::new();
    let original: unsafe extern "C" fn(libc::size_t) -> *mut libc::c_void =
        std::mem::transmute(ORIGINAL_MALLOC.load(Ordering::Relaxed));
    let result = original(size);

    if !_guard.was_in_malloc() && !result.is_null() {
        trace!(
            operation = "malloc",
            ptr = result as usize,
            "Allocating memory"
        );
        handle_malloc(result, size);
    }

    drop(_guard);

    result
}

#[no_mangle]
unsafe extern "C" fn handle_native_free(ptr: *mut libc::c_void) {
    let guard = MallocRecursionGuard::new();

    // We need to call our API before we call the real free implementation
    // to make sure that the pointer is not reused in-between.
    if !guard.was_in_malloc() {
        handle_free(ptr);
    }

    trace!(
        operation = "free",
        ptr = ptr as usize,
        "Freeing native memory"
    );

    // Call the original free implementation
    let original: unsafe extern "C" fn(*mut libc::c_void) =
        std::mem::transmute(ORIGINAL_FREE.load(Ordering::Relaxed));
    original(ptr);

    drop(guard);
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

    if !guard.was_in_malloc() {
        if !result.is_null() {
            handle_malloc(result, nelem * elsize);
        }
    }

    drop(guard);

    result
}

#[no_mangle]
unsafe extern "C" fn handle_native_realloc(
    ptr: *mut libc::c_void,
    size: libc::size_t,
) -> *mut libc::c_void {
    let guard = MallocRecursionGuard::new();
    let tracked = if !guard.was_in_malloc() && !ptr.is_null() {
        handle_realloc_start(ptr)
    } else {
        None
    };
    let original: unsafe extern "C" fn(*mut libc::c_void, libc::size_t) -> *mut libc::c_void =
        std::mem::transmute(ORIGINAL_REALLOC.load(Ordering::Relaxed));
    let result = original(ptr, size);

    if !guard.was_in_malloc() {
        handle_realloc_end(tracked, ptr, result, size);
    }

    drop(guard);

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

    if !guard.was_in_malloc() && result == 0 && !memptr.is_null() && !(*memptr).is_null() {
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

    if !guard.was_in_malloc() && !result.is_null() {
        handle_malloc(result, size);
    }

    result
}

#[no_mangle]
unsafe extern "C" fn handle_native_dlopen(
    filename: *const libc::c_char,
    flag: libc::c_int,
) -> *mut libc::c_void {
    let _guard = MallocRecursionGuard::new();
    let original: unsafe extern "C" fn(*const libc::c_char, libc::c_int) -> *mut libc::c_void =
        std::mem::transmute(ORIGINAL_DLOPEN.load(Ordering::Relaxed));
    let mut ret: *mut libc::c_void = std::ptr::null_mut();

    // TODO: support glibc dlopen
    // In GLIBC, dlopen() will respect the RPATH/RUNPATH of the caller when searching for the
    // library, which won't work if we intercept dlopen() as we will be the caller. This means that
    // callers that rely on RUNPATH to find their dependencies will fail to load. To work around
    // this, we need to manually find our caller and walk the linker search path to know what we need
    // to dlopen().

    // Fallback if we found nothing
    if ret.is_null() {
        ret = original(filename, flag);
    }

    if !ret.is_null() && !filename.is_null() {
        // Reapply hooks when a new module is loaded
        let mut patcher_guard = GLOBAL_PATCHER.lock();
        if let Some(patcher) = patcher_guard.as_mut() {
            patcher.apply_hooks();
        }
    }

    ret
}

fn init_native_hooks() {
    unsafe {
        let handle = libc::dlopen(std::ptr::null(), 1); // RTLD_LAZY
        ORIGINAL_MALLOC.store(
            libc::dlsym(handle, b"malloc\0".as_ptr() as *const _),
            Ordering::Relaxed,
        );
        ORIGINAL_FREE.store(
            libc::dlsym(handle, b"free\0".as_ptr() as *const _),
            Ordering::Relaxed,
        );
        ORIGINAL_REALLOC.store(
            libc::dlsym(handle, b"realloc\0".as_ptr() as *const _),
            Ordering::Relaxed,
        );
        ORIGINAL_CALLOC.store(
            libc::dlsym(handle, b"calloc\0".as_ptr() as *const _),
            Ordering::Relaxed,
        );
        ORIGINAL_DLOPEN.store(
            libc::dlsym(handle, b"dlopen\0".as_ptr() as *const _),
            Ordering::Relaxed,
        );
        ORIGINAL_POSIX_MEMALIGN.store(
            libc::dlsym(handle, b"posix_memalign\0".as_ptr() as *const _),
            Ordering::Relaxed,
        );
        ORIGINAL_ALIGNED_ALLOC.store(
            libc::dlsym(handle, b"aligned_alloc\0".as_ptr() as *const _),
            Ordering::Relaxed,
        );
        libc::dlclose(handle);
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
        patcher.register_hook(Hook {
            symbol_name: "dlopen",
            original_fn: ORIGINAL_DLOPEN.load(Ordering::Relaxed),
            intercept_fn: handle_native_dlopen as *mut libc::c_void,
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
}

#[pyfunction]
pub fn get_sample_rate() -> i32 {
    SAMPLE_RATE.load(Ordering::Relaxed)
}

#[pyfunction]
#[pyo3(signature = (sample_rate=DEFAULT_SAMPLE_RATE))]
pub fn start_tracing(sample_rate: i32) -> PyResult<()> {
    // Configure sample rate if provided
    if sample_rate <= 0 {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "Sample rate must be positive",
        ));
    }
    SAMPLE_RATE.store(sample_rate, Ordering::Relaxed);
    NEXT_SAMPLE.with(|next| {
        next.set(fast_exp_rand(SAMPLE_RATE.load(Ordering::Relaxed)));
    });

    crate::fork::install_atfork_handlers();

    if ARENA_ALLOCATOR_ENABLED {
        setup_arena_allocator();
    }

    let mut original_raw_allocator = get_default_allocator();
    let mut original_mem_allocator = get_default_allocator();
    let mut original_obj_allocator = get_default_allocator();

    // SAFETY: PyMem_GetAllocator is safe to call and only writes to the provided allocator struct
    // The allocator domains are valid constants from the Python C-API
    unsafe {
        ffi::PyMem_GetAllocator(
            ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_RAW,
            &mut original_raw_allocator,
        );
        ffi::PyMem_GetAllocator(
            ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_MEM,
            &mut original_mem_allocator,
        );
        ffi::PyMem_GetAllocator(
            ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_OBJ,
            &mut original_obj_allocator,
        );
    }

    // Check if we already have a custom allocator
    if let Some(malloc) = original_raw_allocator.malloc {
        if malloc == my_malloc {
            return Ok(());
        }
    }

    *SAVED_ALLOCATORS.lock() = Some(SavedAllocators {
        raw: original_raw_allocator,
        mem: original_mem_allocator,
        obj: original_obj_allocator,
    });

    let my_raw_allocator = Box::new(MyAllocator::new(
        original_raw_allocator,
        ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_RAW,
    ));
    let mut new_raw_allocator = my_raw_allocator.get_allocator_ex();
    // SAFETY: Box::leak is safe here as we need the allocator to live for the entire program
    // The pointer will never be deallocated, preventing use-after-free
    new_raw_allocator.ctx =
        Box::leak(my_raw_allocator) as *mut MyAllocator as *mut std::ffi::c_void;

    let my_mem_allocator = Box::new(MyAllocator::new(
        original_mem_allocator,
        ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_MEM,
    ));
    let mut new_mem_allocator = my_mem_allocator.get_allocator_ex();
    new_mem_allocator.ctx =
        Box::leak(my_mem_allocator) as *mut MyAllocator as *mut std::ffi::c_void;

    let my_obj_allocator = Box::new(MyAllocator::new(
        original_obj_allocator,
        ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_OBJ,
    ));
    let mut new_obj_allocator = my_obj_allocator.get_allocator_ex();
    new_obj_allocator.ctx =
        Box::leak(my_obj_allocator) as *mut MyAllocator as *mut std::ffi::c_void;

    // SAFETY: PyMem_SetAllocator is safe to call with valid allocator structs
    // The allocators are properly initialized and will live for the program's duration
    unsafe {
        ffi::PyMem_SetAllocator(
            ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_RAW,
            &mut new_raw_allocator,
        );
        ffi::PyMem_SetAllocator(
            ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_MEM,
            &mut new_mem_allocator,
        );
        ffi::PyMem_SetAllocator(
            ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_OBJ,
            &mut new_obj_allocator,
        );
    }

    init_native_hooks();

    Ok(())
}

/// stop_tracing()
/// --
///
/// Restores the original Python allocators and native hooks. Profile data is
/// kept and can still be read. Frees of allocations sampled before stopping
/// are no longer observed, so inuse numbers drift for memory freed while
/// stopped. Each start/stop cycle leaks three small allocator wrappers by
/// design (they may still be referenced by in-flight calls).
#[pyfunction]
pub fn stop_tracing() -> PyResult<()> {
    let saved = SAVED_ALLOCATORS.lock().take();
    let Some(saved) = saved else {
        return Ok(()); // not started, or already stopped
    };
    let mut raw = saved.raw;
    let mut mem = saved.mem;
    let mut obj = saved.obj;

    // SAFETY: restoring previously captured allocator structs
    unsafe {
        ffi::PyMem_SetAllocator(ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_RAW, &mut raw);
        ffi::PyMem_SetAllocator(ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_MEM, &mut mem);
        ffi::PyMem_SetAllocator(ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_OBJ, &mut obj);
    }

    let mut patcher_guard = GLOBAL_PATCHER.lock();
    if let Some(patcher) = patcher_guard.as_mut() {
        patcher.restore_hooks();
    }

    Ok(())
}

#[inline]
fn is_python_initialized() -> bool {
    unsafe { ffi::Py_IsInitialized() != 0 }
}

// Memory allocation functions
extern "C" fn my_malloc(ctx: *mut std::ffi::c_void, size: usize) -> *mut std::ffi::c_void {
    // SAFETY: ctx is guaranteed to be a valid pointer to MyAllocator
    let my_allocator = unsafe { &mut *(ctx as *mut MyAllocator) };
    let original_allocator = my_allocator.original_allocator;

    // Ensure all allocation requests are multiples of eight,
    // mirroring the actual allocation sizes employed by pymalloc
    let new_size = if size <= PYMALLOC_MAX_SIZE {
        if size == 0 {
            8
        } else {
            (size + 7) & !7
        }
    } else {
        size
    };

    // We can get to a situation where we're allocating memory before Python is initialized
    // But this is the Python allocator, you say, how can that happen?
    // Well, when python is shutting down, is disables PyIsInitialized, which in turn causes
    // pyo3 to panic. The RAW domain is documented to keep working before init and during
    // finalization, so delegate to the original allocator without profiling.
    if !is_python_initialized() {
        return (original_allocator.malloc.unwrap())(original_allocator.ctx, new_size);
    }

    let guard = MallocRecursionGuard::new();

    // SAFETY: original_allocator.malloc is guaranteed to be Some
    let ptr = (original_allocator.malloc.unwrap())(original_allocator.ctx, new_size);
    trace!(
        operation = "malloc",
        size = new_size,
        ptr = ptr as usize,
        "Allocating memory"
    );

    if !ptr.is_null() && !guard.was_in_malloc() {
        handle_malloc(ptr, size);
    }

    ptr
}

extern "C" fn my_calloc(
    ctx: *mut std::ffi::c_void,
    nelem: usize,
    elsize: usize,
) -> *mut std::ffi::c_void {
    // SAFETY: ctx is guaranteed to be a valid pointer to MyAllocator
    let my_allocator = unsafe { &mut *(ctx as *mut MyAllocator) };
    let original_allocator = my_allocator.original_allocator;

    if !is_python_initialized() {
        return (original_allocator.calloc.unwrap())(original_allocator.ctx, nelem, elsize);
    }

    let guard = MallocRecursionGuard::new();

    // Delegate to the original calloc: it checks nelem * elsize for overflow
    // and keeps the zero-page optimization for large allocations.
    // SAFETY: original_allocator.calloc is guaranteed to be Some
    let ptr = (original_allocator.calloc.unwrap())(original_allocator.ctx, nelem, elsize);

    if !ptr.is_null() && !guard.was_in_malloc() {
        // The underlying calloc succeeded, so nelem * elsize cannot overflow
        handle_malloc(ptr, nelem * elsize);
    }

    ptr
}

extern "C" fn my_realloc(
    ctx: *mut std::ffi::c_void,
    ptr: *mut std::ffi::c_void,
    new_size: usize,
) -> *mut std::ffi::c_void {
    // SAFETY: ctx is guaranteed to be a valid pointer to MyAllocator
    let my_allocator = unsafe { &mut *(ctx as *mut MyAllocator) };
    let original_allocator = my_allocator.original_allocator;

    let new_size = if new_size < 8 { 8 } else { new_size };

    if !is_python_initialized() {
        // Delegate without profiling: the RAW domain must keep working before
        // interpreter init and during finalization (see my_malloc).
        return (original_allocator.realloc.unwrap())(original_allocator.ctx, ptr, new_size);
    }

    if ptr.is_null() {
        return my_malloc(ctx, new_size);
    }

    trace!(
        operation = "realloc",
        ptr = ptr as usize,
        new_size = new_size,
        "Reallocating memory"
    );
    let guard = MallocRecursionGuard::new();

    let tracked = if !guard.was_in_malloc() {
        handle_realloc_start(ptr)
    } else {
        None
    };

    // Use the original pointer for realloc
    let new_ptr = (original_allocator.realloc.unwrap())(original_allocator.ctx, ptr, new_size);

    if new_ptr.is_null() {
        warn!(
            operation = "realloc_failed",
            requested_size = new_size,
            "Memory reallocation failed"
        );
    }

    if !guard.was_in_malloc() {
        handle_realloc_end(tracked, ptr, new_ptr, new_size);
    }

    new_ptr
}

extern "C" fn my_free(ctx: *mut std::ffi::c_void, ptr: *mut std::ffi::c_void) {
    if ptr.is_null() {
        return;
    }

    let _guard = MallocRecursionGuard::new();

    // SAFETY: ctx is guaranteed to be a valid pointer to MyAllocator
    let my_allocator = unsafe { &mut *(ctx as *mut MyAllocator) };
    let original_allocator = my_allocator.original_allocator;

    trace!(operation = "free", ptr = ptr as usize, "Freeing memory");

    // Accounting is pure Rust (locks only, no Python C-API), so it is safe in
    // any interpreter state; the underlying free must ALWAYS run.
    handle_free(ptr);

    // SAFETY: original_allocator.free is guaranteed to be Some
    (original_allocator.free.unwrap())(original_allocator.ctx, ptr);
}

fn setup_arena_allocator() {
    // Set up arena allocator
    unsafe {
        let mut arena_allocator = ffi::PyObjectArenaAllocator::default();
        ffi::PyObject_GetArenaAllocator(&mut arena_allocator);

        let my_arena_allocator = Box::new(MyArenaAllocator::new(arena_allocator));
        let mut new_arena_allocator = my_arena_allocator.get_allocator();
        new_arena_allocator.ctx =
            Box::leak(my_arena_allocator) as *mut MyArenaAllocator as *mut std::ffi::c_void;

        ffi::PyObject_SetArenaAllocator(&mut new_arena_allocator);
    }
}

#[derive(Clone)]
pub struct MyArenaAllocator {
    pub original_allocator: ffi::PyObjectArenaAllocator,
}

impl MyArenaAllocator {
    pub fn new(original_allocator: ffi::PyObjectArenaAllocator) -> Self {
        Self { original_allocator }
    }

    pub fn get_allocator(&self) -> ffi::PyObjectArenaAllocator {
        ffi::PyObjectArenaAllocator {
            ctx: std::ptr::null_mut(),
            alloc: Some(my_arena_alloc),
            free: Some(my_arena_free),
        }
    }
}

extern "C" fn my_arena_alloc(ctx: *mut std::ffi::c_void, size: usize) -> *mut std::ffi::c_void {
    if !is_python_initialized() {
        return std::ptr::null_mut();
    }

    let _guard = MallocRecursionGuard::new();

    // SAFETY: ctx is guaranteed to be a valid pointer to MyArenaAllocator
    let my_allocator = unsafe { &mut *(ctx as *mut MyArenaAllocator) };
    let original_allocator = my_allocator.original_allocator;

    // SAFETY: original_allocator.alloc is guaranteed to be Some
    let ptr = (original_allocator.alloc.unwrap())(original_allocator.ctx, size);

    warn!(
        operation = "arena_alloc",
        size = size,
        ptr = ?format!("{:#x}", ptr as usize),
        "Allocating arena memory"
    );

    ptr
}

extern "C" fn my_arena_free(ctx: *mut std::ffi::c_void, ptr: *mut std::ffi::c_void, size: usize) {
    if !is_python_initialized() || ptr.is_null() {
        return;
    }

    let _guard = MallocRecursionGuard::new();

    // SAFETY: ctx is guaranteed to be a valid pointer to MyArenaAllocator
    let my_allocator = unsafe { &mut *(ctx as *mut MyArenaAllocator) };
    let original_allocator = my_allocator.original_allocator;

    warn!(
        operation = "arena_free",
        ptr = ?format!("{:#x}", ptr as usize),
        size = size,
        "Freeing arena memory"
    );

    // SAFETY: original_allocator.free is guaranteed to be Some
    (original_allocator.free.unwrap())(original_allocator.ctx, ptr, size);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realloc_failure_restores_metadata() {
        let _serial = crate::GLOBAL_TEST_LOCK.lock();
        let ptr = 0xDEAD_1000 as *mut std::ffi::c_void;
        let meta = AllocationMetadata {
            stack_hash: 0xABCD_0001,
            requested_size: 64,
        };
        metadata_store::store_metadata(ptr, meta);

        let tracked = handle_realloc_start(ptr);
        assert!(tracked.is_some());

        // Simulate the underlying realloc failing: old pointer is still live
        handle_realloc_end(tracked, ptr, std::ptr::null_mut(), 128);

        assert!(
            metadata_store::remove_metadata(ptr).is_some(),
            "metadata must be restored when realloc fails"
        );
    }

    #[test]
    fn realloc_success_credits_free_to_old_record() {
        let _serial = crate::GLOBAL_TEST_LOCK.lock();
        let stack_hash = 0xABCD_0002u64;
        let ptr = 0xDEAD_2000 as *mut std::ffi::c_void;
        let new_ptr = 0xDEAD_3000 as *mut std::ffi::c_void;

        PROFILE_DATA
            .lock()
            .insert(stack_hash, MemProfileRecord::new(&[]));
        metadata_store::store_metadata(
            ptr,
            AllocationMetadata {
                stack_hash,
                requested_size: 64,
            },
        );

        let tracked = handle_realloc_start(ptr);
        handle_realloc_end(tracked, ptr, new_ptr, 128);

        let profile_data = PROFILE_DATA.lock();
        let record = profile_data.get(&stack_hash).unwrap();
        assert_eq!(record.free_bytes, 64);
        assert_eq!(record.free_objects, 1);
        drop(profile_data);

        assert!(
            metadata_store::remove_metadata(ptr).is_none(),
            "old pointer metadata must be gone after successful realloc"
        );
    }

    #[test]
    fn fast_exp_rand_is_positive() {
        for _ in 0..10_000 {
            assert!(fast_exp_rand(DEFAULT_SAMPLE_RATE) > 0);
        }
        assert_eq!(fast_exp_rand(0), 0);
        assert_eq!(fast_exp_rand(-5), 0);
    }

    #[test]
    fn huge_allocations_are_always_sampled() {
        SAMPLE_RATE.store(DEFAULT_SAMPLE_RATE, Ordering::Relaxed);
        NEXT_SAMPLE.with(|next| next.set(fast_exp_rand(DEFAULT_SAMPLE_RATE)));
        // 3 GiB allocation: larger than any sane counter value, must sample
        assert!(should_sample(3 * 1024 * 1024 * 1024));
    }
}
