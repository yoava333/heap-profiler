use super::recording::{finish_reallocation, handle_malloc, PendingChange, Reallocation};
use crate::recursion_guard::MallocRecursionGuard;
use crate::sync::Mutex;
use once_cell::sync::Lazy;
use pyo3::{ffi, prelude::*};
use std::sync::{atomic::Ordering, OnceLock};

const DOMAINS: [ffi::PyMemAllocatorDomain; 3] = [
    ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_RAW,
    ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_MEM,
    ffi::PyMemAllocatorDomain::PYMEM_DOMAIN_OBJ,
];

#[derive(Clone, Copy)]
pub(crate) struct SavedAllocators {
    original: [ffi::PyMemAllocatorEx; 3],
    installed: [ffi::PyMemAllocatorEx; 3],
}
// SAFETY: captured tables are immutable; CPython owns their callback/context
// lifetimes through the installed wrapping chain. Access is mutex-protected.
unsafe impl Send for SavedAllocators {}

pub(crate) static SAVED_ALLOCATORS: Lazy<Mutex<Option<SavedAllocators>>> =
    Lazy::new(|| Mutex::new(None));

pub(super) fn is_installed() -> bool {
    SAVED_ALLOCATORS.lock().is_some()
}

fn read_allocator(domain: ffi::PyMemAllocatorDomain) -> ffi::PyMemAllocatorEx {
    let mut allocator = ffi::PyMemAllocatorEx {
        ctx: std::ptr::null_mut(),
        malloc: None,
        calloc: None,
        realloc: None,
        free: None,
    };
    // SAFETY: these are the three CPython domains and the output is valid.
    unsafe {
        ffi::PyMem_GetAllocator(domain, &mut allocator);
    }
    allocator
}

pub(super) fn prepare_session() -> PyResult<SavedAllocators> {
    let original = DOMAINS.map(read_allocator);
    let mut installed = original;
    for (index, domain) in DOMAINS.into_iter().enumerate() {
        installed[index] = cached_allocator(original[index], domain)?;
    }
    Ok(SavedAllocators {
        original,
        installed,
    })
}

pub(super) fn install_session(session: SavedAllocators) {
    let mut installed = session.installed;
    *SAVED_ALLOCATORS.lock() = Some(session);
    std::sync::atomic::fence(Ordering::SeqCst);
    for (domain, allocator) in DOMAINS.into_iter().zip(&mut installed) {
        // SAFETY: slot initialization is complete before callback publication;
        // contexts are unchanged, and callback generations live for the process.
        unsafe {
            ffi::PyMem_SetAllocator(domain, allocator);
        }
    }
}

pub(super) fn restore_session() -> bool {
    let Some(session) = SAVED_ALLOCATORS.lock().take() else {
        return false;
    };
    for (index, domain) in DOMAINS.into_iter().enumerate() {
        if same_allocator(&read_allocator(domain), &session.installed[index]) {
            let mut original = session.original[index];
            // SAFETY: restore only tables still owned by this session.
            unsafe {
                ffi::PyMem_SetAllocator(domain, &mut original);
            }
        }
    }
    true
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

    fn get_allocator_ex(&self, slot: usize) -> ffi::PyMemAllocatorEx {
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

#[inline]
fn is_python_initialized() -> bool {
    unsafe { ffi::Py_IsInitialized() != 0 }
}

// Memory allocation functions
fn my_malloc(my_allocator: &MyAllocator, size: usize) -> *mut std::ffi::c_void {
    let original_allocator = my_allocator.original_allocator;

    // We can get to a situation where we're allocating memory before Python is initialized
    // But this is the Python allocator, you say, how can that happen?
    // Well, when python is shutting down, is disables PyIsInitialized, which in turn causes
    // pyo3 to panic. The RAW domain is documented to keep working before init and during
    // finalization, so delegate to the original allocator without profiling.
    let guard = MallocRecursionGuard::new();
    if guard.was_in_malloc() || !is_python_initialized() {
        return (original_allocator.malloc.unwrap())(original_allocator.ctx, size);
    }

    // SAFETY: original_allocator.malloc is guaranteed to be Some
    let ptr = (original_allocator.malloc.unwrap())(original_allocator.ctx, size);
    let _errno = guard.preserve_errno();

    if !ptr.is_null() {
        handle_malloc(ptr, size);
    }

    ptr
}

fn my_calloc(my_allocator: &MyAllocator, nelem: usize, elsize: usize) -> *mut std::ffi::c_void {
    let original_allocator = my_allocator.original_allocator;

    let guard = MallocRecursionGuard::new();
    if guard.was_in_malloc() || !is_python_initialized() {
        return (original_allocator.calloc.unwrap())(original_allocator.ctx, nelem, elsize);
    }

    // Delegate to the original calloc: it checks nelem * elsize for overflow
    // and keeps the zero-page optimization for large allocations.
    // SAFETY: original_allocator.calloc is guaranteed to be Some
    let ptr = (original_allocator.calloc.unwrap())(original_allocator.ctx, nelem, elsize);
    let _errno = guard.preserve_errno();

    if !ptr.is_null() {
        // The underlying calloc succeeded, so nelem * elsize cannot overflow
        handle_malloc(ptr, nelem * elsize);
    }

    ptr
}

fn my_realloc(
    my_allocator: &MyAllocator,
    ptr: *mut std::ffi::c_void,
    new_size: usize,
) -> *mut std::ffi::c_void {
    let original_allocator = my_allocator.original_allocator;

    let guard = MallocRecursionGuard::new();
    if guard.was_in_malloc() || !is_python_initialized() {
        // Delegate without profiling: the RAW domain must keep working before
        // interpreter init and during finalization (see my_malloc).
        return (original_allocator.realloc.unwrap())(original_allocator.ctx, ptr, new_size);
    }

    let pending = PendingChange::begin(ptr);

    // Use the original pointer for realloc
    let new_ptr = (original_allocator.realloc.unwrap())(original_allocator.ctx, ptr, new_size);
    let _errno = guard.preserve_errno();

    finish_reallocation(pending, Reallocation::python(new_ptr, new_size));

    new_ptr
}

fn my_free(my_allocator: &MyAllocator, ptr: *mut std::ffi::c_void) {
    if ptr.is_null() {
        return;
    }

    let _guard = MallocRecursionGuard::new();
    let _errno = (!_guard.was_in_malloc()).then(|| _guard.preserve_errno());

    let original_allocator = my_allocator.original_allocator;

    // Accounting is pure Rust (locks only, no Python C-API), so it is safe in
    // any interpreter state; the underlying free must ALWAYS run.
    let _transaction = if !_guard.was_in_malloc() {
        PendingChange::free(ptr)
    } else {
        None
    };

    // SAFETY: original_allocator.free is guaranteed to be Some
    (original_allocator.free.unwrap())(original_allocator.ctx, ptr);
}

pub(crate) static ALLOCATOR_REGISTRY: Mutex<()> = Mutex::new(());
// SAFETY: the table is immutable after capture. CPython requires callbacks to
// obey the domain's threading contract; callers preserve those C-API conditions.
unsafe impl Send for MyAllocator {}
unsafe impl Sync for MyAllocator {}

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
static ALLOCATOR_SLOTS: [OnceLock<MyAllocator>; ALLOCATOR_SLOT_COUNT] =
    [const { OnceLock::new() }; ALLOCATOR_SLOT_COUNT];

fn slot_allocator<const SLOT: usize>() -> &'static MyAllocator {
    ALLOCATOR_SLOTS[SLOT]
        .get()
        .expect("callback slot published only after initialization")
}
extern "C" fn slot_malloc<const SLOT: usize>(
    _ctx: *mut libc::c_void,
    size: usize,
) -> *mut libc::c_void {
    my_malloc(slot_allocator::<SLOT>(), size)
}
extern "C" fn slot_calloc<const SLOT: usize>(
    _ctx: *mut libc::c_void,
    count: usize,
    size: usize,
) -> *mut libc::c_void {
    my_calloc(slot_allocator::<SLOT>(), count, size)
}
extern "C" fn slot_realloc<const SLOT: usize>(
    _ctx: *mut libc::c_void,
    ptr: *mut libc::c_void,
    size: usize,
) -> *mut libc::c_void {
    my_realloc(slot_allocator::<SLOT>(), ptr, size)
}
extern "C" fn slot_free<const SLOT: usize>(_ctx: *mut libc::c_void, ptr: *mut libc::c_void) {
    my_free(slot_allocator::<SLOT>(), ptr)
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
    let _registry = ALLOCATOR_REGISTRY.lock();
    for (index, slot) in ALLOCATOR_SLOTS.iter().enumerate() {
        if let Some(wrapper) = slot.get() {
            if wrapper.domain as i32 == domain as i32
                && same_allocator(&wrapper.original_allocator, &original)
            {
                return Ok(wrapper.get_allocator_ex(index));
            }
        }
    }
    let index = ALLOCATOR_SLOTS
        .iter()
        .position(|slot| slot.get().is_none())
        .ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "Profiler callback slots exhausted by distinct Python allocator chains",
            )
        })?;
    let wrapper = ALLOCATOR_SLOTS[index].get_or_init(|| MyAllocator::new(original, domain));
    Ok(wrapper.get_allocator_ex(index))
}
