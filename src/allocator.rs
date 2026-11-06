use crate::metadata_store::{self, AllocationMetadata};
use crate::platform::{Patcher, PlatformPatcher};
use crate::profile::{
    MemProfileRecord, DEFAULT_SAMPLE_RATE, PROFILE_DATA, SAMPLE_RATE, TOTAL_ALLOC_BYTES,
};
use crate::stack::{calculate_stack_hash, where_in_python, StackBuffer, StackFrame};
use crate::sync::Mutex;
use once_cell::sync::Lazy;
use pyo3::ffi;
use pyo3::prelude::*;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use crate::recursion_guard::MallocRecursionGuard;

pub(crate) static TRACING: AtomicBool = AtomicBool::new(false);
static NATIVE_ENABLED: AtomicBool = AtomicBool::new(false);

static ORIGINAL_MALLOC: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_FREE: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_REALLOC: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_CALLOC: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_POSIX_MEMALIGN: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_ALIGNED_ALLOC: AtomicPtr<libc::c_void> = AtomicPtr::new(std::ptr::null_mut());

pub(crate) static GLOBAL_PATCHER: Lazy<Mutex<Option<PlatformPatcher>>> =
    Lazy::new(|| Mutex::new(None));

pub(crate) struct SavedAllocators {
    raw: ffi::PyMemAllocatorEx,
    mem: ffi::PyMemAllocatorEx,
    obj: ffi::PyMemAllocatorEx,
    installed: [ffi::PyMemAllocatorEx; 3],
}
// SAFETY: these are captured CPython allocator tables. Their callback/context
// lifetimes follow the installed wrapping chain; table access is mutex-protected.
unsafe impl Send for SavedAllocators {}

pub(crate) static SAVED_ALLOCATORS: Lazy<Mutex<Option<SavedAllocators>>> =
    Lazy::new(|| Mutex::new(None));

#[derive(Clone, Copy)]
struct SamplerState {
    remaining: i64,
    rate: i32,
}
thread_local! {
    // No destructor: allocation callbacks can run from C++ TLS destructors.
    static RANDOM_STATE: Cell<u64> = const { Cell::new(0) };
    static SAMPLER: Cell<SamplerState> = const { Cell::new(SamplerState { remaining: 0, rate: 0 }) };
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

    pub fn get_allocator_ex(&self, slot: usize) -> ffi::PyMemAllocatorEx {
        ffi::PyMemAllocatorEx {
            // Preserve the underlying context across installation/restoration.
            // RAW callers can read function and ctx from different table versions.
            ctx: self.original_allocator.ctx,
            malloc: Some(PYTHON_MALLOC[slot]),
            calloc: Some(PYTHON_CALLOC[slot]),
            realloc: Some(PYTHON_REALLOC[slot]),
            free: Some(PYTHON_FREE[slot]),
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

// SplitMix64 is used only for statistical sampling, not for cryptography.
// State lives in a const-initialized, non-dropping TLS Cell. rand::thread_rng
// can panic after its destructor has run while C++ TLS destructors still malloc.
static SEED_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn mix_random(mut value: u64) -> u64 {
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

fn fast_exp_rand(mean: i32) -> i64 {
    if mean <= 0 {
        return 0;
    }
    let random = RANDOM_STATE.with(|cell| {
        let mut state = cell.get();
        if state == 0 {
            let sequence = SEED_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let thread = unsafe { libc::pthread_self() } as usize as u64;
            let process = unsafe { libc::getpid() } as u64;
            state = mix_random(sequence ^ thread ^ process.rotate_left(32));
        }
        state = state.wrapping_add(0x9e3779b97f4a7c15);
        cell.set(state);
        mix_random(state)
    });
    // 53 random bits avoid the 26-bit generator's inflated minimum-interval
    // mass at large rates (32x too frequent for one-byte requests at i32::MAX).
    let uniform = (random >> 11) as f64 * (1.0 / 9007199254740992.0);
    exp_interval(mean, uniform)
}

fn exp_interval(mean: i32, uniform: f64) -> i64 {
    (-(-uniform).ln_1p() * mean as f64) as i64 + 1
}

pub(crate) fn reset_sampler_after_fork() {
    RANDOM_STATE.with(|state| state.set(0));
    SAMPLER.with(|state| {
        state.set(SamplerState {
            remaining: 0,
            rate: 0,
        })
    });
}

#[inline]
fn should_sample(size: usize, rate: i32) -> bool {
    if rate == 1 {
        return true;
    }
    // Keep the budget and configuration in one TLS slot: on macOS each TLS
    // access otherwise crosses a thread-local variable resolver.
    SAMPLER.with(|sampler| {
        let mut state = sampler.get();
        if state.rate != rate {
            state = SamplerState {
                remaining: fast_exp_rand(rate),
                rate,
            };
        }
        let sampled = state.remaining <= 0 || size >= state.remaining as usize;
        if sampled {
            state.remaining = fast_exp_rand(rate);
        } else {
            state.remaining -= size as i64;
        }
        sampler.set(state);
        sampled
    })
}

type ProfileTransaction = crate::sync::MutexGuard<'static, ()>;

fn handle_malloc(ptr: *mut std::ffi::c_void, size: usize) {
    handle_malloc_in_transaction(ptr, size, None);
}

fn handle_malloc_in_transaction(
    ptr: *mut std::ffi::c_void,
    size: usize,
    transaction: Option<&ProfileTransaction>,
) {
    if !TRACING.load(Ordering::Acquire) {
        return;
    }
    TOTAL_ALLOC_BYTES.fetch_add(size as u64, Ordering::Relaxed);

    let rate = SAMPLE_RATE.load(Ordering::Relaxed);
    let should_profile = should_sample(size, rate);
    if should_profile {
        let _transaction = transaction
            .is_none()
            .then(|| crate::profile::PROFILE_TRANSACTION.lock());
        if !TRACING.load(Ordering::Acquire) {
            return;
        }
        // Record the stack trace
        let mut stack = StackBuffer::new();
        let depth = where_in_python(&mut stack);

        // Calculate stack hash
        let mut profile_data = PROFILE_DATA.lock();
        let stack_hash = resolve_stack_key(
            &profile_data,
            &stack[..depth],
            calculate_stack_hash(&stack[..depth]),
        );

        let metadata = AllocationMetadata {
            stack_hash,
            requested_size: size,
            generation: crate::profile::GENERATION.load(Ordering::Relaxed),
            sample_rate: rate,
        };

        metadata_store::store_metadata(ptr, metadata);

        // Update profile data under the same lock used to resolve collisions.
        let record = profile_data
            .entry(stack_hash)
            .or_insert_with(|| MemProfileRecord::new(&stack[..depth]));
        record.alloc_bytes = record
            .alloc_bytes
            .saturating_add(size.min(i64::MAX as usize) as i64);
        record.alloc_objects = record.alloc_objects.saturating_add(1);
        let (objects, bytes) = crate::profile::sample_weight(size, metadata.sample_rate);
        record.scaled_alloc_objects += objects;
        record.scaled_alloc_bytes += bytes;
    }
}

fn record_free(metadata: AllocationMetadata) {
    if metadata.generation != crate::profile::GENERATION.load(Ordering::Relaxed) {
        return;
    }
    let mut profile_data = PROFILE_DATA.lock();
    if let Some(record) = profile_data.get_mut(&metadata.stack_hash) {
        record.free_bytes = record
            .free_bytes
            .saturating_add(metadata.requested_size.min(i64::MAX as usize) as i64);
        record.free_objects = record.free_objects.saturating_add(1);
        let (objects, bytes) =
            crate::profile::sample_weight(metadata.requested_size, metadata.sample_rate);
        record.scaled_free_objects += objects;
        record.scaled_free_bytes += bytes;
    }
}

/// Probe without removing metadata, then drain the full allocator operation
/// under the fork/reset transaction. Until acquired, a fork sees the original
/// live allocation AND its metadata, never just one of them.
fn begin_change(ptr: *mut std::ffi::c_void) -> Option<ProfileTransaction> {
    metadata_store::contains_metadata(ptr).then(|| crate::profile::PROFILE_TRANSACTION.lock())
}

fn handle_free(ptr: *mut std::ffi::c_void) -> Option<ProfileTransaction> {
    let transaction = begin_change(ptr)?;
    if let Some(metadata) = metadata_store::remove_metadata(ptr) {
        record_free(metadata);
    }
    Some(transaction)
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
    transaction: Option<&ProfileTransaction>,
) {
    let owned = (tracked.is_some() && transaction.is_none())
        .then(|| crate::profile::PROFILE_TRANSACTION.lock());
    let transaction = transaction.or(owned.as_ref());
    if new_ptr.is_null() {
        // realloc failed: the old allocation is still live, restore its metadata
        if let Some(metadata) = tracked {
            if metadata.generation == crate::profile::GENERATION.load(Ordering::Relaxed) {
                metadata_store::store_metadata(old_ptr, metadata);
            }
        }
        return;
    }
    if let Some(metadata) = tracked {
        record_free(metadata);
    }
    handle_malloc_in_transaction(new_ptr, size, transaction);
}

#[no_mangle]
unsafe extern "C" fn handle_native_malloc(size: libc::size_t) -> *mut libc::c_void {
    let _guard = MallocRecursionGuard::new();
    let original: unsafe extern "C" fn(libc::size_t) -> *mut libc::c_void =
        std::mem::transmute(ORIGINAL_MALLOC.load(Ordering::Relaxed));
    let result = original(size);
    let _errno = (!_guard.was_in_malloc()).then(|| ErrnoGuard::new());

    if !_guard.was_in_malloc() && NATIVE_ENABLED.load(Ordering::Acquire) && !result.is_null() {
        handle_malloc(result, size);
    }

    result
}

#[no_mangle]
unsafe extern "C" fn handle_native_free(ptr: *mut libc::c_void) {
    let guard = MallocRecursionGuard::new();
    let _errno = (!guard.was_in_malloc()).then(|| ErrnoGuard::new());

    // We need to call our API before we call the real free implementation
    // to make sure that the pointer is not reused in-between.
    let _transaction = if !guard.was_in_malloc() && NATIVE_ENABLED.load(Ordering::Acquire) {
        handle_free(ptr)
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
    let _errno = (!guard.was_in_malloc()).then(|| ErrnoGuard::new());

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
    let transaction = if tracking { begin_change(ptr) } else { None };
    let tracked = if transaction.is_some() {
        handle_realloc_start(ptr)
    } else {
        None
    };
    let original: unsafe extern "C" fn(*mut libc::c_void, libc::size_t) -> *mut libc::c_void =
        std::mem::transmute(ORIGINAL_REALLOC.load(Ordering::Relaxed));
    let result = original(ptr, size);
    let _errno = (!guard.was_in_malloc()).then(|| ErrnoGuard::new());

    if tracking {
        // On glibc/Darwin realloc(p, 0) frees p even if it returns NULL.
        if !ptr.is_null() && size == 0 && result.is_null() {
            if let Some(metadata) = tracked {
                record_free(metadata);
            }
        } else {
            handle_realloc_end(tracked, ptr, result, size, transaction.as_ref());
        }
    }
    // Unlock while the recursion and errno guards are still active.
    drop(transaction);

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
    let _errno = (!guard.was_in_malloc()).then(|| ErrnoGuard::new());

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
    let _errno = (!guard.was_in_malloc()).then(|| ErrnoGuard::new());

    if !guard.was_in_malloc() && NATIVE_ENABLED.load(Ordering::Acquire) && !result.is_null() {
        handle_malloc(result, size);
    }

    result
}

fn init_native_hooks() -> PyResult<()> {
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

#[pyfunction]
pub fn get_sample_rate() -> i32 {
    SAMPLE_RATE.load(Ordering::Relaxed)
}

#[pyfunction]
#[pyo3(signature = (sample_rate=DEFAULT_SAMPLE_RATE, *, native=true))]
pub fn start_tracing(sample_rate: i32, native: bool) -> PyResult<()> {
    let _guard = MallocRecursionGuard::new();
    // Configure sample rate if provided
    if sample_rate <= 0 {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "Sample rate must be positive",
        ));
    }
    if SAVED_ALLOCATORS.lock().is_some() && native != NATIVE_ENABLED.load(Ordering::Acquire) {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "Stop tracing before changing native interception",
        ));
    }
    SAMPLE_RATE.store(sample_rate, Ordering::Relaxed);
    SAMPLER.with(|sampler| {
        sampler.set(SamplerState {
            remaining: fast_exp_rand(sample_rate),
            rate: sample_rate,
        })
    });

    crate::fork::install_atfork_handlers()?;

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

    if SAVED_ALLOCATORS.lock().is_some() {
        refresh_native_hooks();
        return Ok(());
    }

    let mut new_raw_allocator = cached_allocator(
        original_raw_allocator,
        ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_RAW,
    )?;
    let mut new_mem_allocator = cached_allocator(
        original_mem_allocator,
        ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_MEM,
    )?;
    let mut new_obj_allocator = cached_allocator(
        original_obj_allocator,
        ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_OBJ,
    )?;
    if native {
        init_native_hooks()?;
    }
    NATIVE_ENABLED.store(native, Ordering::Release);
    *SAVED_ALLOCATORS.lock() = Some(SavedAllocators {
        raw: original_raw_allocator,
        mem: original_mem_allocator,
        obj: original_obj_allocator,
        installed: [new_raw_allocator, new_mem_allocator, new_obj_allocator],
    });

    // Publish immutable slot state before exposing callbacks to RAW threads.
    std::sync::atomic::fence(Ordering::SeqCst);
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

    // Publish only after all original function pointers are resolved.
    TRACING.store(true, Ordering::Release);

    Ok(())
}

/// stop_tracing()
/// --
///
/// Restores the original Python allocators and native hooks. Profile data is
/// kept and can still be read. Frees of allocations sampled before stopping
/// are no longer observed, so inuse numbers drift for memory freed while
/// stopped. Allocator wrappers are retained and reused so in-flight calls and
/// extensions wrapping our allocators remain valid across start/stop cycles.
#[pyfunction]
pub fn stop_tracing() -> PyResult<()> {
    let _guard = MallocRecursionGuard::new();
    TRACING.store(false, Ordering::Release);
    let saved = SAVED_ALLOCATORS.lock().take();
    let Some(saved) = saved else {
        return Ok(()); // not started, or already stopped
    };
    // An extension may have wrapped us since start. Preserve its allocator
    // chain; our retained wrappers delegate while TRACING is false.
    for ((domain, mut original), installed) in [
        (ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_RAW, saved.raw),
        (ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_MEM, saved.mem),
        (ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_OBJ, saved.obj),
    ]
    .into_iter()
    .zip(saved.installed)
    {
        let mut current = get_default_allocator();
        unsafe {
            ffi::PyMem_GetAllocator(domain, &mut current);
        }
        if same_allocator(&current, &installed) {
            unsafe {
                ffi::PyMem_SetAllocator(domain, &mut original);
            }
        }
    }

    let mut patcher_guard = GLOBAL_PATCHER.lock();
    if NATIVE_ENABLED.swap(false, Ordering::AcqRel) {
        if let Some(patcher) = patcher_guard.as_mut() {
            patcher.restore_hooks();
        }
    }

    drop(patcher_guard);

    // Addresses may be reused while stopped. Old metadata must never be
    // matched against allocations from a later tracing session.
    let _transaction = crate::profile::PROFILE_TRANSACTION.lock();
    crate::metadata_store::clear();
    crate::profile::GENERATION.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Re-scan loaded libraries without interposing caller-sensitive dlopen.
#[pyfunction]
pub fn refresh_native_hooks() {
    let _guard = MallocRecursionGuard::new();
    if TRACING.load(Ordering::Acquire) && NATIVE_ENABLED.load(Ordering::Acquire) {
        if let Some(patcher) = GLOBAL_PATCHER.lock().as_mut() {
            patcher.apply_hooks();
        }
    }
}

#[inline]
fn is_python_initialized() -> bool {
    unsafe { ffi::Py_IsInitialized() != 0 }
}

// Memory allocation functions
extern "C" fn my_malloc(ctx: *mut std::ffi::c_void, size: usize) -> *mut std::ffi::c_void {
    // SAFETY: ctx is guaranteed to be a valid pointer to MyAllocator
    let my_allocator = unsafe { &*(ctx as *const MyAllocator) };
    let original_allocator = my_allocator.original_allocator;

    // We can get to a situation where we're allocating memory before Python is initialized
    // But this is the Python allocator, you say, how can that happen?
    // Well, when python is shutting down, is disables PyIsInitialized, which in turn causes
    // pyo3 to panic. The RAW domain is documented to keep working before init and during
    // finalization, so delegate to the original allocator without profiling.
    if !is_python_initialized() {
        return (original_allocator.malloc.unwrap())(original_allocator.ctx, size);
    }

    let guard = MallocRecursionGuard::new();

    // SAFETY: original_allocator.malloc is guaranteed to be Some
    let ptr = (original_allocator.malloc.unwrap())(original_allocator.ctx, size);
    let _errno = (!guard.was_in_malloc()).then(|| unsafe { ErrnoGuard::new() });

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
    let my_allocator = unsafe { &*(ctx as *const MyAllocator) };
    let original_allocator = my_allocator.original_allocator;

    if !is_python_initialized() {
        return (original_allocator.calloc.unwrap())(original_allocator.ctx, nelem, elsize);
    }

    let guard = MallocRecursionGuard::new();

    // Delegate to the original calloc: it checks nelem * elsize for overflow
    // and keeps the zero-page optimization for large allocations.
    // SAFETY: original_allocator.calloc is guaranteed to be Some
    let ptr = (original_allocator.calloc.unwrap())(original_allocator.ctx, nelem, elsize);
    let _errno = (!guard.was_in_malloc()).then(|| unsafe { ErrnoGuard::new() });

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
    let my_allocator = unsafe { &*(ctx as *const MyAllocator) };
    let original_allocator = my_allocator.original_allocator;

    if !is_python_initialized() {
        // Delegate without profiling: the RAW domain must keep working before
        // interpreter init and during finalization (see my_malloc).
        return (original_allocator.realloc.unwrap())(original_allocator.ctx, ptr, new_size);
    }

    let guard = MallocRecursionGuard::new();

    let transaction = if !guard.was_in_malloc() {
        begin_change(ptr)
    } else {
        None
    };
    let tracked = if transaction.is_some() {
        handle_realloc_start(ptr)
    } else {
        None
    };

    // Use the original pointer for realloc
    let new_ptr = (original_allocator.realloc.unwrap())(original_allocator.ctx, ptr, new_size);
    let _errno = (!guard.was_in_malloc()).then(|| unsafe { ErrnoGuard::new() });

    if !guard.was_in_malloc() {
        handle_realloc_end(tracked, ptr, new_ptr, new_size, transaction.as_ref());
    }

    drop(transaction);
    new_ptr
}

extern "C" fn my_free(ctx: *mut std::ffi::c_void, ptr: *mut std::ffi::c_void) {
    if ptr.is_null() {
        return;
    }

    let _guard = MallocRecursionGuard::new();
    let _errno = (!_guard.was_in_malloc()).then(|| unsafe { ErrnoGuard::new() });

    // SAFETY: ctx is guaranteed to be a valid pointer to MyAllocator
    let my_allocator = unsafe { &*(ctx as *const MyAllocator) };
    let original_allocator = my_allocator.original_allocator;

    // Accounting is pure Rust (locks only, no Python C-API), so it is safe in
    // any interpreter state; the underlying free must ALWAYS run.
    let _transaction = if !_guard.was_in_malloc() {
        handle_free(ptr)
    } else {
        None
    };

    // SAFETY: original_allocator.free is guaranteed to be Some
    (original_allocator.free.unwrap())(original_allocator.ctx, ptr);
}

/// Profiler bookkeeping must not overwrite the allocator's errno result.
struct ErrnoGuard {
    value: libc::c_int,
}
impl ErrnoGuard {
    unsafe fn location() -> *mut libc::c_int {
        #[cfg(target_os = "linux")]
        {
            libc::__errno_location()
        }
        #[cfg(target_os = "macos")]
        {
            libc::__error()
        }
    }
    unsafe fn new() -> Self {
        Self {
            value: *Self::location(),
        }
    }
}
impl Drop for ErrnoGuard {
    fn drop(&mut self) {
        unsafe {
            *Self::location() = self.value;
        }
    }
}

// Boxes intentionally keep callback contexts stable across cache growth.
#[allow(clippy::vec_box)]
pub(crate) static ALLOCATOR_CACHE: Lazy<Mutex<Vec<Box<MyAllocator>>>> =
    Lazy::new(|| Mutex::new(Vec::new()));
// CPython requires allocator callbacks and contexts to be thread-safe. We
// only read the captured function table after construction.
unsafe impl Send for MyAllocator {}

fn same_allocator(a: &ffi::PyMemAllocatorEx, b: &ffi::PyMemAllocatorEx) -> bool {
    a.ctx == b.ctx
        && a.malloc.map(|f| f as usize) == b.malloc.map(|f| f as usize)
        && a.calloc.map(|f| f as usize) == b.calloc.map(|f| f as usize)
        && a.realloc.map(|f| f as usize) == b.realloc.map(|f| f as usize)
        && a.free.map(|f| f as usize) == b.free.map(|f| f as usize)
}

// Each immutable slot has distinct C callbacks. They ignore CPython's ctx
// argument and forward through that slot's captured original table. Keeping the
// public ctx unchanged makes mixed old/new table reads safe for both callbacks.
// Slots are never reused: in-flight calls and later allocator wrappers can keep
// an earlier callback generation alive indefinitely.
const ALLOCATOR_SLOT_COUNT: usize = 96;
static ALLOCATOR_SLOTS: [AtomicPtr<MyAllocator>; ALLOCATOR_SLOT_COUNT] =
    [const { AtomicPtr::new(std::ptr::null_mut()) }; ALLOCATOR_SLOT_COUNT];

fn slot_context<const SLOT: usize>() -> *mut libc::c_void {
    let context = ALLOCATOR_SLOTS[SLOT].load(Ordering::Acquire);
    debug_assert!(!context.is_null());
    context.cast()
}
extern "C" fn slot_malloc<const SLOT: usize>(
    _ctx: *mut libc::c_void,
    size: usize,
) -> *mut libc::c_void {
    my_malloc(slot_context::<SLOT>(), size)
}
extern "C" fn slot_calloc<const SLOT: usize>(
    _ctx: *mut libc::c_void,
    count: usize,
    size: usize,
) -> *mut libc::c_void {
    my_calloc(slot_context::<SLOT>(), count, size)
}
extern "C" fn slot_realloc<const SLOT: usize>(
    _ctx: *mut libc::c_void,
    ptr: *mut libc::c_void,
    size: usize,
) -> *mut libc::c_void {
    my_realloc(slot_context::<SLOT>(), ptr, size)
}
extern "C" fn slot_free<const SLOT: usize>(_ctx: *mut libc::c_void, ptr: *mut libc::c_void) {
    my_free(slot_context::<SLOT>(), ptr)
}
macro_rules! allocator_callbacks {
    ($($slot:literal),* $(,)?) => {
        const PYTHON_MALLOC: [extern "C" fn(*mut libc::c_void, usize) -> *mut libc::c_void; ALLOCATOR_SLOT_COUNT] = [$(slot_malloc::<$slot>),*];
        const PYTHON_CALLOC: [extern "C" fn(*mut libc::c_void, usize, usize) -> *mut libc::c_void; ALLOCATOR_SLOT_COUNT] = [$(slot_calloc::<$slot>),*];
        const PYTHON_REALLOC: [extern "C" fn(*mut libc::c_void, *mut libc::c_void, usize) -> *mut libc::c_void; ALLOCATOR_SLOT_COUNT] = [$(slot_realloc::<$slot>),*];
        const PYTHON_FREE: [extern "C" fn(*mut libc::c_void, *mut libc::c_void); ALLOCATOR_SLOT_COUNT] = [$(slot_free::<$slot>),*];
    };
}
allocator_callbacks!(
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
    50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73,
    74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95,
);

fn cached_allocator(
    original: ffi::PyMemAllocatorEx,
    domain: ffi::PyMemAllocatorDomain,
) -> PyResult<ffi::PyMemAllocatorEx> {
    let mut cache = ALLOCATOR_CACHE.lock();
    if let Some(index) = cache.iter().position(|wrapper| {
        wrapper.domain as i32 == domain as i32
            && same_allocator(&wrapper.original_allocator, &original)
    }) {
        return Ok(cache[index].get_allocator_ex(index));
    }
    let index = cache.len();
    if index == ALLOCATOR_SLOT_COUNT {
        return Err(pyo3::exceptions::PyRuntimeError::new_err(
            "Profiler callback slots exhausted by distinct Python allocator chains",
        ));
    }
    cache.push(Box::new(MyAllocator::new(original, domain)));
    let wrapper = &*cache[index];
    ALLOCATOR_SLOTS[index].store(
        wrapper as *const MyAllocator as *mut MyAllocator,
        Ordering::Release,
    );
    Ok(wrapper.get_allocator_ex(index))
}

fn resolve_stack_key(
    records: &hashbrown::HashMap<u64, MemProfileRecord>,
    stack: &[StackFrame],
    mut key: u64,
) -> u64 {
    while let Some(record) = records.get(&key) {
        if record.stack.as_ref() == stack {
            break;
        }
        key = key.wrapping_add(1);
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_collisions_do_not_merge_stacks() {
        let mut records = hashbrown::HashMap::new();
        let stack = vec![StackFrame::default()];
        records.insert(u64::MAX, MemProfileRecord::new(&[]));
        let key = resolve_stack_key(&records, &stack, u64::MAX);
        assert_eq!(key, 0);
        records.insert(key, MemProfileRecord::new(&stack));
        assert_eq!(resolve_stack_key(&records, &stack, u64::MAX), key);
    }

    #[test]
    fn realloc_failure_restores_metadata() {
        let _serial = crate::GLOBAL_TEST_LOCK.lock();
        let ptr = 0xDEAD_1000 as *mut std::ffi::c_void;
        let meta = AllocationMetadata {
            stack_hash: 0xABCD_0001,
            requested_size: 64,
            generation: crate::profile::GENERATION.load(Ordering::Relaxed),
            sample_rate: DEFAULT_SAMPLE_RATE,
        };
        metadata_store::store_metadata(ptr, meta);

        let tracked = handle_realloc_start(ptr);
        assert!(tracked.is_some());

        // Simulate the underlying realloc failing: old pointer is still live
        handle_realloc_end(tracked, ptr, std::ptr::null_mut(), 128, None);

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
                generation: crate::profile::GENERATION.load(Ordering::Relaxed),
                sample_rate: DEFAULT_SAMPLE_RATE,
            },
        );

        TRACING.store(true, Ordering::Release);
        let tracked = handle_realloc_start(ptr);
        handle_realloc_end(tracked, ptr, new_ptr, 128, None);
        TRACING.store(false, Ordering::Release);

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
    fn tiny_sampling_probabilities_retain_precision() {
        let rate = i32::MAX;
        let probability = -(-1.0 / rate as f64).exp_m1();
        assert_eq!(exp_interval(rate, probability * 0.5), 1);
        assert_eq!(exp_interval(rate, probability * 1.5), 2);
        assert_eq!(exp_interval(rate, probability * 2.5), 3);
    }

    #[test]
    fn huge_allocations_are_always_sampled() {
        let _serial = crate::GLOBAL_TEST_LOCK.lock();
        SAMPLE_RATE.store(DEFAULT_SAMPLE_RATE, Ordering::Relaxed);
        SAMPLER.with(|sampler| {
            sampler.set(SamplerState {
                remaining: fast_exp_rand(DEFAULT_SAMPLE_RATE),
                rate: DEFAULT_SAMPLE_RATE,
            })
        });
        // 3 GiB allocation: larger than any sane counter value, must sample
        assert!(should_sample(3 * 1024 * 1024 * 1024, DEFAULT_SAMPLE_RATE));
    }
}
