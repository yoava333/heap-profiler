use crate::profile::{MemProfileRecord, DEFAULT_SAMPLE_RATE, PROFILE_DATA};
use crate::stack::{calculate_stack_hash, where_in_python, StackFrame};
use pyo3::ffi;
use pyo3::prelude::*;
use rand::Rng;
use std::cell::Cell;

// Constants for sampling and allocation
const MIN_SAMPLE_SIZE: usize = 256 * 1024; // 256KB - always sample allocations larger than this
const SAMPLE_MASK: usize = 0xFF; // For alignment
const METADATA_SIZE: usize = std::mem::size_of::<AllocMetadata>();

thread_local! {
    static NEXT_SAMPLE: Cell<i32> = Cell::new(0);
    static IN_MALLOC: Cell<bool> = Cell::new(false);
}

#[derive(Clone)]
pub struct MyAllocator {
    pub original_allocator: ffi::PyMemAllocatorEx,
}

#[repr(C)]
pub struct AllocMetadata {
    magic: u64,      // Magic number to identify our allocations
    stack_hash: u64, // Hash of the stack trace
    requested_size: usize,
    total_size: usize,
}

impl MyAllocator {
    pub fn new(original_allocator: ffi::PyMemAllocatorEx) -> Self {
        Self { original_allocator }
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

#[pyfunction]
pub fn set_allocator() -> PyResult<()> {
    let mut original_raw_allocator = get_default_allocator();
    let mut original_mem_allocator = get_default_allocator();
    let mut original_obj_allocator = get_default_allocator();

    // Get the allocators
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

    let my_raw_allocator = Box::new(MyAllocator::new(original_raw_allocator));
    let mut new_raw_allocator = my_raw_allocator.get_allocator_ex();
    new_raw_allocator.ctx =
        Box::leak(my_raw_allocator) as *mut MyAllocator as *mut std::ffi::c_void;

    let my_mem_allocator = Box::new(MyAllocator::new(original_mem_allocator));
    let mut new_mem_allocator = my_mem_allocator.get_allocator_ex();
    new_mem_allocator.ctx =
        Box::leak(my_mem_allocator) as *mut MyAllocator as *mut std::ffi::c_void;

    let my_obj_allocator = Box::new(MyAllocator::new(original_obj_allocator));
    let mut new_obj_allocator = my_obj_allocator.get_allocator_ex();
    new_obj_allocator.ctx =
        Box::leak(my_obj_allocator) as *mut MyAllocator as *mut std::ffi::c_void;

    // Set our new allocator for all domains
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
    let my_allocator = unsafe { &mut *(ctx as *mut MyAllocator) };
    if IN_MALLOC.get() {
        let original_allocator = my_allocator.original_allocator;
        return (original_allocator.malloc.unwrap())(original_allocator.ctx, size);
    }

    IN_MALLOC.set(true);

    let should_profile = should_sample(size);
    let alloc_size = if should_profile {
        // Align the metadata size
        size + METADATA_SIZE + (SAMPLE_MASK - ((size + METADATA_SIZE) & SAMPLE_MASK))
    } else {
        size
    };

    let original_allocator = my_allocator.original_allocator;
    let ptr = (original_allocator.malloc.unwrap())(original_allocator.ctx, alloc_size);

    if should_profile && !ptr.is_null() {
        // Record the stack trace
        let mut stack = std::array::from_fn(|_| StackFrame::default());
        let depth = where_in_python(&mut stack);

        // Calculate stack hash
        let stack_hash = calculate_stack_hash(&stack[..depth]);

        // Store metadata at the start of allocation
        let metadata = ptr as *mut AllocMetadata;
        unsafe {
            (*metadata) = AllocMetadata {
                magic: 0xDEADBEEF, // Magic number to identify our allocations
                stack_hash,
                requested_size: size,
                total_size: alloc_size,
            };
        }

        // Update profile data
        if let Ok(mut profile_data) = PROFILE_DATA.lock() {
            let record = profile_data
                .entry(stack_hash)
                .or_insert_with(|| MemProfileRecord::new(stack));
            record.alloc_bytes += size as i64;
            record.alloc_objects += 1;
        }

        // Return pointer after metadata
        let data_ptr = unsafe { ptr.add(METADATA_SIZE) };
        IN_MALLOC.set(false);
        return data_ptr;
    }

    IN_MALLOC.set(false);
    ptr
}

extern "C" fn my_calloc(
    ctx: *mut std::ffi::c_void,
    nelem: usize,
    elsize: usize,
) -> *mut std::ffi::c_void {
    let my_allocator = unsafe { &mut *(ctx as *mut MyAllocator) };
    if IN_MALLOC.get() {
        let original_allocator = my_allocator.original_allocator;
        return (original_allocator.calloc.unwrap())(original_allocator.ctx, nelem, elsize);
    }

    IN_MALLOC.set(true);
    let size = nelem.saturating_mul(elsize);

    let should_profile = should_sample(size);
    let alloc_size = if should_profile {
        size + METADATA_SIZE + (SAMPLE_MASK - ((size + METADATA_SIZE) & SAMPLE_MASK))
    } else {
        size
    };

    let original_allocator = my_allocator.original_allocator;
    let ptr = (original_allocator.malloc.unwrap())(original_allocator.ctx, alloc_size);

    if !ptr.is_null() {
        // Zero the memory
        unsafe {
            std::ptr::write_bytes(ptr, 0, alloc_size);
        }

        if should_profile {
            // Record the stack trace
            let mut stack = std::array::from_fn(|_| StackFrame::default());
            let depth = where_in_python(&mut stack);

            let stack_hash = calculate_stack_hash(&stack[..depth]);

            // Store metadata at the start of allocation
            let metadata = ptr as *mut AllocMetadata;
            unsafe {
                (*metadata) = AllocMetadata {
                    magic: 0xDEADBEEF,
                    stack_hash,
                    requested_size: size,
                    total_size: alloc_size,
                };
            }

            // Update profile data
            if let Ok(mut profile_data) = PROFILE_DATA.lock() {
                let record = profile_data
                    .entry(stack_hash)
                    .or_insert_with(|| MemProfileRecord::new(stack));
                record.alloc_bytes += size as i64;
                record.alloc_objects += 1;
            }

            // Return pointer after metadata
            let data_ptr = unsafe { ptr.add(METADATA_SIZE) };
            IN_MALLOC.set(false);
            return data_ptr;
        }
    }

    IN_MALLOC.set(false);
    ptr
}

extern "C" fn my_realloc(
    ctx: *mut std::ffi::c_void,
    ptr: *mut std::ffi::c_void,
    new_size: usize,
) -> *mut std::ffi::c_void {
    let my_allocator = unsafe { &mut *(ctx as *mut MyAllocator) };

    // Early return for recursive calls or null context
    if IN_MALLOC.get() {
        let original_allocator = my_allocator.original_allocator;
        return (original_allocator.realloc.unwrap())(original_allocator.ctx, ptr, new_size);
    }

    IN_MALLOC.set(true);

    // Handle null pointer case
    if ptr.is_null() {
        let result = my_malloc(ctx, new_size);
        IN_MALLOC.set(false);
        return result;
    }

    // Check if this was a sampled allocation with safety checks
    let was_sampled = unsafe {
        let potential_metadata = (ptr as *mut AllocMetadata).offset(-1);
        // Verify the pointer is properly aligned and within reasonable bounds
        if (potential_metadata as usize) % std::mem::align_of::<AllocMetadata>() == 0 {
            // Read the magic number with safety check
            let magic = std::ptr::read_volatile(&(*potential_metadata).magic);
            if magic == 0xDEADBEEF {
                // Verify the size is reasonable
                let size = (*potential_metadata).requested_size;
                size > 0 && size <= new_size * 2 // Allow up to double size for realloc
            } else {
                false
            }
        } else {
            false
        }
    };

    let should_profile = if was_sampled {
        true
    } else {
        should_sample(new_size)
    };

    let new_alloc_size = if should_profile {
        new_size + METADATA_SIZE + (SAMPLE_MASK - ((new_size + METADATA_SIZE) & SAMPLE_MASK))
    } else {
        new_size
    };

    let original_allocator = my_allocator.original_allocator;
    let new_ptr = if was_sampled {
        // Get original metadata safely
        let old_ptr = unsafe { (ptr as *mut AllocMetadata).offset(-1) as *mut std::ffi::c_void };
        (original_allocator.realloc.unwrap())(original_allocator.ctx, old_ptr, new_alloc_size)
    } else {
        (original_allocator.realloc.unwrap())(original_allocator.ctx, ptr, new_alloc_size)
    };

    // Handle allocation failure
    if new_ptr.is_null() {
        IN_MALLOC.set(false);
        return std::ptr::null_mut();
    }

    if should_profile {
        // Record the stack trace
        let mut stack = std::array::from_fn(|_| StackFrame::default());
        let depth = where_in_python(&mut stack);

        let stack_hash = calculate_stack_hash(&stack[..depth]);

        // Store metadata at the start of allocation with safety checks
        let metadata = new_ptr as *mut AllocMetadata;
        unsafe {
            std::ptr::write_volatile(
                metadata,
                AllocMetadata {
                    magic: 0xDEADBEEF,
                    stack_hash,
                    requested_size: new_size,
                    total_size: new_alloc_size,
                },
            );
        }

        // Update profile data
        if let Ok(mut profile_data) = PROFILE_DATA.lock() {
            let record = profile_data
                .entry(stack_hash)
                .or_insert_with(|| MemProfileRecord::new(stack));
            record.alloc_bytes += new_size as i64;
            record.alloc_objects += 1;
        }

        // Return pointer after metadata
        let data_ptr = unsafe { new_ptr.add(METADATA_SIZE) };
        IN_MALLOC.set(false);
        return data_ptr;
    }

    IN_MALLOC.set(false);
    new_ptr
}

extern "C" fn my_free(ctx: *mut std::ffi::c_void, ptr: *mut std::ffi::c_void) {
    let my_allocator = unsafe { &mut *(ctx as *mut MyAllocator) };
    if IN_MALLOC.get() || ptr.is_null() {
        let original_allocator = my_allocator.original_allocator;
        (original_allocator.free.unwrap())(original_allocator.ctx, ptr);
        return;
    }

    IN_MALLOC.set(true);

    // Check if this was a sampled allocation
    let potential_metadata = unsafe { (ptr as *mut AllocMetadata).offset(-1) };
    let is_sampled = unsafe { (*potential_metadata).magic == 0xDEADBEEF };

    if is_sampled {
        let metadata = unsafe { &*potential_metadata };

        // Update profile data
        if let Ok(mut profile_data) = PROFILE_DATA.lock() {
            if let Some(record) = profile_data.get_mut(&metadata.stack_hash) {
                record.free_bytes += metadata.requested_size as i64;
                record.free_objects += 1;
            }
        }

        // Free the original allocation (including metadata)
        let original_ptr = potential_metadata as *mut std::ffi::c_void;
        let original_allocator = my_allocator.original_allocator;
        (original_allocator.free.unwrap())(original_allocator.ctx, original_ptr);
    } else {
        // Normal free
        let original_allocator = my_allocator.original_allocator;
        (original_allocator.free.unwrap())(original_allocator.ctx, ptr);
    }

    IN_MALLOC.set(false);
}
