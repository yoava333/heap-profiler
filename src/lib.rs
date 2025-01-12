use lazy_static::lazy_static;
use pyo3::ffi::{
    PyBytes_AsString, PyFrame_GetBack, PyFrame_GetCode, PyFrame_GetLineNumber,
    PyMemAllocatorDomain, PyMemAllocatorEx, PyMem_GetAllocator, PyMem_SetAllocator,
    PyUnicode_AsASCIIString,
};
use pyo3::prelude::*;
use std::cell::Cell;
use std::ffi::CStr;
use std::io::Write;
use std::sync::Once;

#[derive(Clone)]
struct MyAllocator {
    name: String,
    original_allocator: PyMemAllocatorEx,
}

#[derive(Debug, Default, Clone)]
struct stackFrame {
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
    stack0: [stackFrame; 128],
}

thread_local! {
    static in_malloc: Cell<bool> = Cell::new(false);
}

impl MyAllocator {
    fn new(name: String, original_allocator: PyMemAllocatorEx) -> Self {
        Self {
            name,
            original_allocator,
        }
    }

    fn get_allocator_ex(&self) -> PyMemAllocatorEx {
        PyMemAllocatorEx {
            ctx: std::ptr::null_mut(),
            malloc: Some(my_malloc),
            calloc: Some(my_calloc),
            realloc: Some(my_realloc),
            free: Some(my_free),
        }
    }

    fn get_name(&self) -> &str {
        &self.name
    }
}

fn get_default_allocator() -> PyMemAllocatorEx {
    PyMemAllocatorEx {
        ctx: std::ptr::null_mut(),
        malloc: None,
        calloc: None,
        realloc: None,
        free: None,
    }
}

static START: Once = Once::new();

extern "C" fn my_malloc(ctx: *mut std::ffi::c_void, size: usize) -> *mut std::ffi::c_void {
    let my_allocator = unsafe { &mut *(ctx as *mut MyAllocator) };
    if in_malloc.get() {
        return unsafe {
            (my_allocator.original_allocator.malloc.unwrap())(
                my_allocator.original_allocator.ctx,
                size,
            )
        };
    }

    in_malloc.set(true);

    println!(
        "malloc called with size: {} for allocator: {}",
        size,
        my_allocator.get_name()
    );

    START.call_once(|| {
        let mut stack = std::array::from_fn(|_| stackFrame::default());
        let depth = where_in_python(&mut stack);
        println!("depth: {}", depth);
        println!("stack: {:?}", stack);
        std::io::stdout().flush().unwrap();
        std::process::exit(0);
    });

    let original_allocator = my_allocator.original_allocator;
    let ptr = unsafe { (original_allocator.malloc.unwrap())(original_allocator.ctx, size) };
    in_malloc.set(false);
    ptr
}

extern "C" fn my_calloc(
    ctx: *mut std::ffi::c_void,
    nelem: usize,
    elsize: usize,
) -> *mut std::ffi::c_void {
    let my_allocator = unsafe { &mut *(ctx as *mut MyAllocator) };
    if in_malloc.get() {
        return unsafe {
            (my_allocator.original_allocator.calloc.unwrap())(
                my_allocator.original_allocator.ctx,
                nelem,
                elsize,
            )
        };
    }

    in_malloc.set(true);

    println!(
        "calloc called with nelem: {}, elsize: {} for allocator: {}",
        nelem,
        elsize,
        my_allocator.get_name()
    );
    let original_allocator = my_allocator.original_allocator;
    let ptr =
        unsafe { (original_allocator.calloc.unwrap())(original_allocator.ctx, nelem, elsize) };
    in_malloc.set(false);
    ptr
}

extern "C" fn my_realloc(
    ctx: *mut std::ffi::c_void,
    ptr: *mut std::ffi::c_void,
    new_size: usize,
) -> *mut std::ffi::c_void {
    let my_allocator = unsafe { &mut *(ctx as *mut MyAllocator) };
    if in_malloc.get() {
        return unsafe {
            (my_allocator.original_allocator.realloc.unwrap())(
                my_allocator.original_allocator.ctx,
                ptr,
                new_size,
            )
        };
    }

    in_malloc.set(true);

    println!(
        "realloc called with ptr: {:?}, new_size: {} for allocator: {}",
        ptr,
        new_size,
        my_allocator.get_name()
    );
    let original_allocator = my_allocator.original_allocator;
    let ptr =
        unsafe { (original_allocator.realloc.unwrap())(original_allocator.ctx, ptr, new_size) };
    in_malloc.set(false);
    ptr
}

extern "C" fn my_free(ctx: *mut std::ffi::c_void, ptr: *mut std::ffi::c_void) {
    let my_allocator = unsafe { &mut *(ctx as *mut MyAllocator) };
    if in_malloc.get() {
        return unsafe {
            (my_allocator.original_allocator.free.unwrap())(
                my_allocator.original_allocator.ctx,
                ptr,
            )
        };
    }

    in_malloc.set(true);

    println!(
        "free called with ptr: {:?} for allocator: {}",
        ptr,
        my_allocator.get_name()
    );
    let original_allocator = my_allocator.original_allocator;
    let _ = unsafe { (original_allocator.free.unwrap())(original_allocator.ctx, ptr) };

    in_malloc.set(false);
}

fn where_in_python(stack: &mut [stackFrame; 128]) -> usize {
    let mut depth = 0;

    Python::with_gil(|_py| {
        // Get current thread state and frame
        let thread_state = unsafe { pyo3::ffi::PyGILState_GetThisThreadState() };
        let mut frame = if !thread_state.is_null() {
            unsafe { pyo3::ffi::PyThreadState_GetFrame(thread_state) }
        } else {
            std::ptr::null_mut()
        };

        // If frame is null, return 0 depth
        if frame.is_null() {
            return 0;
        }

        unsafe {
            while !frame.is_null() && depth < 128 {
                let code = PyFrame_GetCode(frame);
                if code.is_null() {
                    break;
                }

                // Get filename as PyObject
                let co_filename = (*code).co_filename;
                let ascii_str = PyUnicode_AsASCIIString(co_filename);
                if ascii_str.is_null() {
                    pyo3::ffi::Py_DECREF(code as *mut pyo3::ffi::PyObject);
                    break;
                }

                let fname = PyBytes_AsString(ascii_str);
                if !fname.is_null() {
                    if let Ok(fname_str) = CStr::from_ptr(fname).to_str() {
                        if should_trace(fname_str) {
                            stack[depth] = stackFrame {
                                filename: fname_str.to_string(),
                                lineno: PyFrame_GetLineNumber(frame),
                                bytei: 0, // You might want to get the actual bytecode index if needed
                            };
                            depth += 1;
                        }
                    }
                }

                // Clean up references
                pyo3::ffi::Py_XDECREF(ascii_str);
                pyo3::ffi::Py_DECREF(code as *mut pyo3::ffi::PyObject);

                // Get next frame
                let next_frame = PyFrame_GetBack(frame);
                pyo3::ffi::Py_XDECREF(frame as *mut pyo3::ffi::PyObject);
                frame = next_frame;
            }
        }
        depth
    });

    depth
}

// Helper function that would need to be implemented
fn should_trace(filename: &str) -> bool {
    // Implement your tracing logic here
    // This would replace the TraceConfig::should_trace functionality
    true // placeholder
}

#[pyfunction]
fn set_allocator() -> PyResult<()> {
    let mut original_raw_allocator = get_default_allocator();
    let mut original_mem_allocator = get_default_allocator();
    let mut original_obj_allocator = get_default_allocator();

    // Get the allocators
    unsafe {
        PyMem_GetAllocator(
            PyMemAllocatorDomain::PYMEM_DOMAIN_RAW,
            &mut original_raw_allocator,
        );
        PyMem_GetAllocator(
            PyMemAllocatorDomain::PYMEM_DOMAIN_MEM,
            &mut original_mem_allocator,
        );
        PyMem_GetAllocator(
            PyMemAllocatorDomain::PYMEM_DOMAIN_OBJ,
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
        "original_raw_allocator".to_string(),
        original_raw_allocator,
    ));

    let mut new_raw_allocator = my_raw_allocator.get_allocator_ex();
    new_raw_allocator.ctx =
        Box::leak(my_raw_allocator) as *mut MyAllocator as *mut std::ffi::c_void;

    let my_mem_allocator = Box::new(MyAllocator::new(
        "original_mem_allocator".to_string(),
        original_mem_allocator,
    ));
    let mut new_mem_allocator = my_mem_allocator.get_allocator_ex();
    new_mem_allocator.ctx =
        Box::leak(my_mem_allocator) as *mut MyAllocator as *mut std::ffi::c_void;

    let my_obj_allocator = Box::new(MyAllocator::new(
        "original_obj_allocator".to_string(),
        original_obj_allocator,
    ));
    let mut new_obj_allocator = my_obj_allocator.get_allocator_ex();
    new_obj_allocator.ctx =
        Box::leak(my_obj_allocator) as *mut MyAllocator as *mut std::ffi::c_void;

    // Set our new allocator for all domains
    unsafe {
        PyMem_SetAllocator(
            PyMemAllocatorDomain::PYMEM_DOMAIN_RAW,
            &mut new_raw_allocator,
        );
        PyMem_SetAllocator(
            PyMemAllocatorDomain::PYMEM_DOMAIN_MEM,
            &mut new_mem_allocator,
        );
        PyMem_SetAllocator(
            PyMemAllocatorDomain::PYMEM_DOMAIN_OBJ,
            &mut new_obj_allocator,
        );
    }

    Ok(())
}

/// A Python module implemented in Rust.
#[pymodule]
fn heap_profiler(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(set_allocator, m)?)?;
    Ok(())
}
