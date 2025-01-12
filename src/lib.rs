use lazy_static::lazy_static;
use pyo3::ffi;
use pyo3::prelude::*;
use rand::Rng;
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::CStr;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

// Constants for sampling
const DEFAULT_SAMPLE_RATE: i32 = 512 * 1024; // 512KB
const MIN_SAMPLE_SIZE: usize = 256 * 1024; // 256KB - always sample allocations larger than this
const SAMPLE_MASK: usize = 0xFF; // For alignment
const METADATA_SIZE: usize = std::mem::size_of::<AllocMetadata>();

lazy_static! {
    static ref PROFILE_DATA: Mutex<HashMap<u64, MemProfileRecord>> = Mutex::new(HashMap::new());
}

thread_local! {
    static NEXT_SAMPLE: Cell<i32> = Cell::new(0);
    static IN_MALLOC: Cell<bool> = Cell::new(false);
}

#[derive(Clone)]
struct MyAllocator {
    original_allocator: ffi::PyMemAllocatorEx,
}

#[derive(Debug, Default, Clone)]
struct StackFrame {
    filename: String,
    lineno: i32,
    bytei: i32,
}

#[derive(Debug)]
struct MemProfileRecord {
    alloc_bytes: i64,
    free_bytes: i64,
    alloc_objects: i64,
    free_objects: i64,
    stack0: [StackFrame; 128],
}

#[repr(C)]
struct AllocMetadata {
    magic: u64,      // Magic number to identify our allocations
    stack_hash: u64, // Hash of the stack trace
    requested_size: usize,
    total_size: usize,
}

impl MemProfileRecord {
    fn new(stack: [StackFrame; 128]) -> Self {
        Self {
            alloc_bytes: 0,
            free_bytes: 0,
            alloc_objects: 0,
            free_objects: 0,
            stack0: stack,
        }
    }
}

fn calculate_stack_hash(stack: &[StackFrame]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for frame in stack {
        frame.filename.hash(&mut hasher);
        frame.lineno.hash(&mut hasher);
        frame.bytei.hash(&mut hasher);
    }
    hasher.finish()
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

impl MyAllocator {
    fn new(original_allocator: ffi::PyMemAllocatorEx) -> Self {
        Self { original_allocator }
    }

    fn get_allocator_ex(&self) -> ffi::PyMemAllocatorEx {
        ffi::PyMemAllocatorEx {
            ctx: std::ptr::null_mut(),
            malloc: Some(my_malloc),
            calloc: Some(my_calloc),
            realloc: Some(my_realloc),
            free: Some(my_free),
        }
    }
}

fn get_default_allocator() -> ffi::PyMemAllocatorEx {
    ffi::PyMemAllocatorEx {
        ctx: std::ptr::null_mut(),
        malloc: None,
        calloc: None,
        realloc: None,
        free: None,
    }
}

fn where_in_python(stack: &mut [StackFrame; 128]) -> usize {
    let mut depth = 0;

    Python::with_gil(|_py| {
        // Get current thread state and frame
        let thread_state = unsafe { ffi::PyGILState_GetThisThreadState() };
        let mut frame = if !thread_state.is_null() {
            unsafe { ffi::PyThreadState_GetFrame(thread_state) }
        } else {
            std::ptr::null_mut()
        };

        // If frame is null, return 0 depth
        if frame.is_null() {
            return 0;
        }

        unsafe {
            while !frame.is_null() && depth < 128 {
                let code = ffi::PyFrame_GetCode(frame);
                if code.is_null() {
                    break;
                }

                // Get filename as PyObject
                let co_filename = (*code).co_filename;
                let ascii_str = ffi::PyUnicode_AsASCIIString(co_filename);
                if ascii_str.is_null() {
                    ffi::Py_DECREF(code as *mut ffi::PyObject);
                    break;
                }

                let fname = ffi::PyBytes_AsString(ascii_str);
                if !fname.is_null() {
                    if let Ok(fname_str) = CStr::from_ptr(fname).to_str() {
                        stack[depth] = StackFrame {
                            filename: fname_str.to_string(),
                            lineno: ffi::PyFrame_GetLineNumber(frame),
                            bytei: 0, // You might want to get the actual bytecode index if needed
                        };
                        depth += 1;
                    }
                }

                // Clean up references
                ffi::Py_XDECREF(ascii_str);
                ffi::Py_DECREF(code as *mut ffi::PyObject);

                // Get next frame
                let next_frame = ffi::PyFrame_GetBack(frame);
                ffi::Py_XDECREF(frame as *mut ffi::PyObject);
                frame = next_frame;
            }
        }
        depth
    })
}

#[pyfunction]
fn set_allocator() -> PyResult<()> {
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

#[pyfunction]
fn get_profile_data() -> PyResult<Vec<(i64, i64, i64, i64, Vec<(String, i32)>)>> {
    let profile_data = PROFILE_DATA.lock().map_err(|_| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("Failed to lock profile data")
    })?;

    let mut result = Vec::new();
    for record in profile_data.values() {
        let stack_frames: Vec<(String, i32)> = record
            .stack0
            .iter()
            .take_while(|frame| !frame.filename.is_empty())
            .map(|frame| (frame.filename.clone(), frame.lineno))
            .collect();

        result.push((
            record.alloc_bytes,
            record.free_bytes,
            record.alloc_objects,
            record.free_objects,
            stack_frames,
        ));
    }

    Ok(result)
}

/// A Python module implemented in Rust.
#[pymodule]
fn heap_profiler(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(set_allocator, m)?)?;
    m.add_function(wrap_pyfunction!(get_profile_data, m)?)?;
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
    if IN_MALLOC.get() {
        let original_allocator = my_allocator.original_allocator;
        return (original_allocator.realloc.unwrap())(original_allocator.ctx, ptr, new_size);
    }

    IN_MALLOC.set(true);

    // Check if this was a sampled allocation
    let was_sampled = if !ptr.is_null() {
        let potential_metadata = unsafe { (ptr as *mut AllocMetadata).offset(-1) };
        unsafe { (*potential_metadata).magic == 0xDEADBEEF }
    } else {
        false
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
        // Get original metadata
        let old_ptr = unsafe { (ptr as *mut AllocMetadata).offset(-1) as *mut std::ffi::c_void };
        (original_allocator.realloc.unwrap())(original_allocator.ctx, old_ptr, new_alloc_size)
    } else {
        (original_allocator.realloc.unwrap())(original_allocator.ctx, ptr, new_alloc_size)
    };

    if should_profile && !new_ptr.is_null() {
        // Record the stack trace
        let mut stack = std::array::from_fn(|_| StackFrame::default());
        let depth = where_in_python(&mut stack);

        let stack_hash = calculate_stack_hash(&stack[..depth]);

        // Store metadata at the start of allocation
        let metadata = new_ptr as *mut AllocMetadata;
        unsafe {
            (*metadata) = AllocMetadata {
                magic: 0xDEADBEEF,
                stack_hash,
                requested_size: new_size,
                total_size: new_alloc_size,
            };
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
