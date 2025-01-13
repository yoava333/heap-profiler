use crate::metadata_store::{self, AllocationMetadata};
use crate::profile::{MemProfileRecord, DEFAULT_SAMPLE_RATE, PROFILE_DATA};
use crate::stack::{calculate_stack_hash, where_in_python, StackFrame};
use pyo3::ffi;
use pyo3::prelude::*;
use rand::Rng;
use std::cell::Cell;
use std::sync::Once;
use tracing::{info, warn};
use tracing_subscriber::{prelude::*, EnvFilter};

use crate::recursion_guard::MallocRecursionGuard;

// Constants for sampling and allocation
const MIN_SAMPLE_SIZE: usize = 256 * 1024; // 256KB - always sample allocations larger than this

// aka "SMALL_REQUEST_THRESHOLD" in cpython/Objects/obmalloc.c
const PYMALLOC_MAX_SIZE: usize = 512;

static INIT_TRACING: Once = Once::new();

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

#[pyfunction]
pub fn set_allocator() -> PyResult<()> {
    // Initialize tracing only once
    INIT_TRACING.call_once(|| {
        tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer())
            .with(EnvFilter::from_default_env())
            .try_init()
            .ok();
    });

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

    Ok(())
}

// Memory allocation functions
extern "C" fn my_malloc(ctx: *mut std::ffi::c_void, size: usize) -> *mut std::ffi::c_void {
    info!(operation = "malloc", size = size, "Allocating memory");

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
    if ptr.is_null() {
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
