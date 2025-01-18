use crate::metadata_store::{self, AllocationMetadata};
use crate::platform::{Patcher, PlatformPatcher};
use crate::profile::{MemProfileRecord, DEFAULT_SAMPLE_RATE, PROFILE_DATA};
use crate::stack::{calculate_stack_hash, where_in_python, StackFrame};
use lazy_static::lazy_static;
use pyo3::ffi;
use pyo3::prelude::*;
use rand::Rng;
use std::cell::Cell;
use std::sync::Mutex;
use tracing::{info, warn};

use crate::recursion_guard::MallocRecursionGuard;

static mut ORIGINAL_MALLOC: *mut libc::c_void = std::ptr::null_mut();
static mut ORIGINAL_FREE: *mut libc::c_void = std::ptr::null_mut();
static mut ORIGINAL_REALLOC: *mut libc::c_void = std::ptr::null_mut();
static mut ORIGINAL_CALLOC: *mut libc::c_void = std::ptr::null_mut();
static mut ORIGINAL_DLOPEN: *mut libc::c_void = std::ptr::null_mut();

// Constants for sampling and allocation
const MIN_SAMPLE_SIZE: usize = 256 * 1024; // 256KB - always sample allocations larger than this

// aka "SMALL_REQUEST_THRESHOLD" in cpython/Objects/obmalloc.c
const PYMALLOC_MAX_SIZE: usize = 512;

// Add these constants at the top of the file with other constants
const RTLD_LAZY: libc::c_int = 1;
const RTLD_NOLOAD: libc::c_int = 4;
const RTLD_DI_SERINFOSIZE: libc::c_int = 5;
const RTLD_DI_SERINFO: libc::c_int = 6;

lazy_static! {
    static ref GLOBAL_PATCHER: Mutex<Option<PlatformPatcher>> = Mutex::new(None);
}

#[repr(C)]
struct DlSerPath {
    dls_name: *const libc::c_char,
    dls_flags: libc::c_ulong,
}

#[repr(C)]
struct DlSerInfo {
    dls_size: libc::size_t,
    dls_cnt: libc::c_uint,
    dls_serpath: [DlSerPath; 0],
}

impl DlSerInfo {
    unsafe fn get_paths(&self) -> &[DlSerPath] {
        let paths_ptr = (self as *const DlSerInfo).add(1) as *const DlSerPath;
        std::slice::from_raw_parts(paths_ptr, self.dls_cnt as usize)
    }

    fn base_size() -> usize {
        std::mem::size_of::<DlSerInfo>()
    }

    fn total_size(path_count: usize) -> usize {
        Self::base_size() + path_count * std::mem::size_of::<DlSerPath>()
    }
}

extern "C" {
    fn dlinfo(
        handle: *mut libc::c_void,
        request: libc::c_int,
        info: *mut libc::c_void,
    ) -> libc::c_int;
    fn dladdr(addr: *mut libc::c_void, info: *mut libc::Dl_info) -> libc::c_int;
}

thread_local! {
    static NEXT_SAMPLE: Cell<i32> = Cell::new(0);
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
fn fast_exp_rand(mean: i32) -> i32 {
    if mean <= 0 {
        return 0;
    }

    let capped_mean = mean.min(0x7000000);
    let random_bits = 26;
    let q = (rand::thread_rng().gen::<u32>() % (1 << random_bits)) + 1;
    let q_log = (q as f64).log2() - random_bits as f64;
    let q_log = q_log.min(0.0);

    const MINUS_LOG2: f64 = -0.6931471805599453;
    (q_log * MINUS_LOG2 * capped_mean as f64) as i32 + 1
}

fn should_sample(size: usize) -> bool {
    // Always sample large allocations
    if size >= MIN_SAMPLE_SIZE {
        return true;
    }

    NEXT_SAMPLE.with(|next| {
        let current = next.get();
        if current <= 0 {
            // Reset the counter with exponential distribution
            next.set(fast_exp_rand(DEFAULT_SAMPLE_RATE));
            true
        } else {
            next.set(current - size as i32);
            false
        }
    })
}

fn handle_malloc(ptr: *mut std::ffi::c_void, size: usize) {
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
        if let Ok(mut profile_data) = PROFILE_DATA.lock() {
            let record = profile_data
                .entry(stack_hash)
                .or_insert_with(|| MemProfileRecord::new(stack));
            record.alloc_bytes += size as i64;
            record.alloc_objects += 1;
        }
    }
}

fn handle_free(ptr: *mut std::ffi::c_void) {
    if let Some(metadata) = metadata_store::remove_metadata(ptr) {
        // Update profile data
        if let Ok(mut profile_data) = PROFILE_DATA.lock() {
            if let Some(record) = profile_data.get_mut(&metadata.stack_hash) {
                record.free_bytes += metadata.requested_size as i64;
                record.free_objects += 1;
            }
        }
    }
}

#[no_mangle]
unsafe extern "C" fn handle_native_malloc(size: libc::size_t) -> *mut libc::c_void {
    let _guard = MallocRecursionGuard::new();
    let original: unsafe extern "C" fn(libc::size_t) -> *mut libc::c_void =
        std::mem::transmute(ORIGINAL_MALLOC);
    let result = original(size);

    if !_guard.was_in_malloc() && !result.is_null() {
        info!(
            operation = "malloc",
            ptr = ?format!("{:#x}", result as usize),
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

    info!(
        operation = "free",
        ptr = ?format!("{:#x}", ptr as usize),
        "Freeing memory"
    );

    // Call the original free implementation
    let original: unsafe extern "C" fn(*mut libc::c_void) = std::mem::transmute(ORIGINAL_FREE);
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
        std::mem::transmute(ORIGINAL_CALLOC);
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
    if !guard.was_in_malloc() {
        if !ptr.is_null() {
            handle_free(ptr);
        }
    }
    let original: unsafe extern "C" fn(*mut libc::c_void, libc::size_t) -> *mut libc::c_void =
        std::mem::transmute(ORIGINAL_REALLOC);
    let result = original(ptr, size);

    if !guard.was_in_malloc() {
        if !result.is_null() {
            handle_malloc(result, size);
        }
    }

    drop(guard);

    result
}

#[no_mangle]
unsafe extern "C" fn handle_native_dlopen(
    filename: *const libc::c_char,
    flag: libc::c_int,
) -> *mut libc::c_void {
    let _guard = MallocRecursionGuard::new();
    let original: unsafe extern "C" fn(*const libc::c_char, libc::c_int) -> *mut libc::c_void =
        std::mem::transmute(ORIGINAL_DLOPEN);
    let mut ret = std::ptr::null_mut();

    #[cfg(target_os = "linux")]
    {
        if !filename.is_null() {
            let filename_str = std::ffi::CStr::from_ptr(filename).to_bytes();
            if !filename_str.is_empty() && !filename_str.contains(&b'/') {
                // Get caller's return address using inline assembly
                let return_addr: *mut libc::c_void;
                #[cfg(target_arch = "aarch64")]
                {
                    std::arch::asm!(
                        "mov {}, x30",  // x30 is the link register on aarch64
                        out(reg) return_addr,
                        options(nostack, preserves_flags)
                    );
                }
                #[cfg(target_arch = "x86_64")]
                {
                    std::arch::asm!(
                        "mov {}, [rbp + 8]",  // Return address is stored at rbp + 8
                        out(reg) return_addr,
                        options(nostack, preserves_flags)
                    );
                }
                #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
                {
                    return_addr = std::ptr::null_mut();
                }

                if !return_addr.is_null() {
                    let mut info: libc::Dl_info = std::mem::zeroed();
                    if dladdr(return_addr, &mut info) != 0 {
                        let dlname = info.dli_fname;
                        let mut is_main = false;

                        // Check if we're being called from main executable
                        let self_handle = original(std::ptr::null(), RTLD_LAZY | RTLD_NOLOAD);
                        if !self_handle.is_null() {
                            let main_sym = libc::dlsym(self_handle, b"main\0".as_ptr() as *const _);
                            let mut main_info: libc::Dl_info = std::mem::zeroed();

                            if !main_sym.is_null()
                                && dladdr(main_sym as *mut _, &mut main_info) != 0
                            {
                                if !main_info.dli_fname.is_null() && !info.dli_fname.is_null() {
                                    let main_fname = std::ffi::CStr::from_ptr(main_info.dli_fname);
                                    let info_fname = std::ffi::CStr::from_ptr(info.dli_fname);
                                    if main_fname.to_bytes() == info_fname.to_bytes() {
                                        is_main = true;
                                    }
                                }
                            }
                            let _ = original(self_handle as *const libc::c_char, RTLD_LAZY);
                        }

                        let caller_name = if is_main { std::ptr::null() } else { dlname };
                        let caller = original(caller_name, RTLD_LAZY | RTLD_NOLOAD);

                        if !caller.is_null() {
                            let mut size = DlSerInfo {
                                dls_size: DlSerInfo::base_size(),
                                dls_cnt: 0,
                                dls_serpath: [],
                            };

                            if dlinfo(
                                caller,
                                RTLD_DI_SERINFOSIZE,
                                &mut size as *mut DlSerInfo as *mut libc::c_void,
                            ) == 0
                            {
                                let layout = std::alloc::Layout::from_size_align(
                                    DlSerInfo::total_size(size.dls_cnt as usize),
                                    std::mem::align_of::<DlSerInfo>(),
                                )
                                .unwrap();
                                let paths = std::alloc::alloc(layout) as *mut DlSerInfo;
                                *paths = size;

                                if dlinfo(caller, RTLD_DI_SERINFO, paths as *mut libc::c_void) == 0
                                {
                                    let paths_slice = (*paths).get_paths();

                                    for path in paths_slice {
                                        if !path.dls_name.is_null() {
                                            let name = std::ffi::CStr::from_ptr(path.dls_name);
                                            if !name.to_bytes().is_empty() {
                                                let mut dir =
                                                    String::from_utf8_lossy(name.to_bytes())
                                                        .into_owned();
                                                if !dir.ends_with('/') {
                                                    dir.push('/');
                                                }
                                                dir.push_str(
                                                    std::ffi::CStr::from_ptr(filename)
                                                        .to_str()
                                                        .unwrap(),
                                                );

                                                let dir_cstring =
                                                    std::ffi::CString::new(dir).unwrap();
                                                ret = original(dir_cstring.as_ptr(), flag);
                                                if !ret.is_null() {
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                                std::alloc::dealloc(paths as *mut u8, layout);
                            }
                            let _ = original(caller as *const libc::c_char, RTLD_LAZY);
                        }
                    }
                }
            }
        }
    }

    // Fallback if we found nothing
    if ret.is_null() {
        ret = original(filename, flag);
    }

    if !ret.is_null() && !filename.is_null() {
        // Reapply hooks when a new module is loaded
        if let Ok(mut patcher_guard) = GLOBAL_PATCHER.lock() {
            if let Some(patcher) = patcher_guard.as_mut() {
                patcher.apply_hooks();
            }
        }
    }

    ret
}

fn init_native_hooks() {
    unsafe {
        let handle = libc::dlopen(std::ptr::null(), 1); // RTLD_LAZY
        ORIGINAL_MALLOC = libc::dlsym(handle, b"malloc\0".as_ptr() as *const _);
        ORIGINAL_FREE = libc::dlsym(handle, b"free\0".as_ptr() as *const _);
        ORIGINAL_REALLOC = libc::dlsym(handle, b"realloc\0".as_ptr() as *const _);
        ORIGINAL_CALLOC = libc::dlsym(handle, b"calloc\0".as_ptr() as *const _);
        ORIGINAL_DLOPEN = libc::dlsym(handle, b"dlopen\0".as_ptr() as *const _);
        libc::dlclose(handle);
    }

    // Initialize global patcher if not already initialized
    let mut patcher_guard = GLOBAL_PATCHER.lock().unwrap();
    if patcher_guard.is_none() {
        let mut patcher = PlatformPatcher::new("heap_profiler".to_string());
        use crate::platform::Hook;
        unsafe {
            patcher.register_hook(Hook {
                symbol_name: "malloc",
                original_fn: ORIGINAL_MALLOC as *mut libc::c_void,
                intercept_fn: handle_native_malloc as *mut libc::c_void,
            });
            patcher.register_hook(Hook {
                symbol_name: "free",
                original_fn: ORIGINAL_FREE as *mut libc::c_void,
                intercept_fn: handle_native_free as *mut libc::c_void,
            });
            patcher.register_hook(Hook {
                symbol_name: "realloc",
                original_fn: ORIGINAL_REALLOC as *mut libc::c_void,
                intercept_fn: handle_native_realloc as *mut libc::c_void,
            });
            patcher.register_hook(Hook {
                symbol_name: "calloc",
                original_fn: ORIGINAL_CALLOC as *mut libc::c_void,
                intercept_fn: handle_native_calloc as *mut libc::c_void,
            });
            patcher.register_hook(Hook {
                symbol_name: "dlopen",
                original_fn: ORIGINAL_DLOPEN as *mut libc::c_void,
                intercept_fn: handle_native_dlopen as *mut libc::c_void,
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
pub fn set_allocator() -> PyResult<()> {
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

#[inline]
fn is_python_initialized() -> bool {
    unsafe { ffi::Py_IsInitialized() != 0 }
}

// Memory allocation functions
extern "C" fn my_malloc(ctx: *mut std::ffi::c_void, size: usize) -> *mut std::ffi::c_void {
    // We can get to a situation where we're allocating memory before Python is initialized
    // But this is the Python allocator, you say, how can that happen?
    // Well, when python is shutting down, is disables PyIsInitialized, which in turn causes
    // pyo3 to panic.
    if !is_python_initialized() {
        return std::ptr::null_mut();
    }

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

    // SAFETY: ctx is guaranteed to be a valid pointer to MyAllocator
    let my_allocator = unsafe { &mut *(ctx as *mut MyAllocator) };

    let _guard = MallocRecursionGuard::new();
    let original_allocator = my_allocator.original_allocator;

    // SAFETY: original_allocator.malloc is guaranteed to be Some
    let ptr = (original_allocator.malloc.unwrap())(original_allocator.ctx, new_size);

    if !ptr.is_null() {
        handle_malloc(ptr, size);
    }

    ptr
}

extern "C" fn my_calloc(
    ctx: *mut std::ffi::c_void,
    nelem: usize,
    elsize: usize,
) -> *mut std::ffi::c_void {
    let nbytes = nelem * elsize;
    let ptr = my_malloc(ctx, nbytes);
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        std::ptr::write_bytes(ptr, 0, nbytes);
    }
    ptr
}

extern "C" fn my_realloc(
    ctx: *mut std::ffi::c_void,
    ptr: *mut std::ffi::c_void,
    new_size: usize,
) -> *mut std::ffi::c_void {
    if !is_python_initialized() {
        return std::ptr::null_mut();
    }

    info!(
        operation = "realloc",
        ptr = ?format!("{:#x}", ptr as usize),
        new_size = new_size,
        "Reallocating memory"
    );

    let new_size = if new_size < 8 { 8 } else { new_size };

    if ptr.is_null() {
        return my_malloc(ctx, new_size);
    }

    let guard = MallocRecursionGuard::new();

    // SAFETY: ctx is guaranteed to be a valid pointer to MyAllocator
    let my_allocator = unsafe { &mut *(ctx as *mut MyAllocator) };
    let original_allocator = my_allocator.original_allocator;

    // Use the original pointer for realloc
    let new_ptr = (original_allocator.realloc.unwrap())(original_allocator.ctx, ptr, new_size);

    // Check if realloc failed
    if new_ptr.is_null() {
        warn!(
            operation = "realloc_failed",
            requested_size = new_size,
            "Memory reallocation failed"
        );
        return std::ptr::null_mut();
    }

    if !guard.was_in_malloc() {
        handle_malloc(new_ptr, new_size);
    }

    new_ptr
}

extern "C" fn my_free(ctx: *mut std::ffi::c_void, ptr: *mut std::ffi::c_void) {
    if !is_python_initialized() || ptr.is_null() {
        return;
    }

    let _guard = MallocRecursionGuard::new();

    // SAFETY: ctx is guaranteed to be a valid pointer to MyAllocator
    let my_allocator = unsafe { &mut *(ctx as *mut MyAllocator) };
    let original_allocator = my_allocator.original_allocator;

    handle_free(ptr);

    // SAFETY: original_allocator.free is guaranteed to be Some
    (original_allocator.free.unwrap())(original_allocator.ctx, ptr);
}
