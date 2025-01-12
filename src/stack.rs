use pyo3::ffi;
use pyo3::prelude::*;
use std::ffi::CStr;
use std::hash::{Hash, Hasher};

#[derive(Debug, Default, Clone)]
pub struct StackFrame {
    pub filename: String,
    pub lineno: i32,
    pub bytei: i32,
}

impl Hash for StackFrame {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.filename.hash(state);
        self.lineno.hash(state);
        self.bytei.hash(state);
    }
}

pub fn calculate_stack_hash(stack: &[StackFrame]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for frame in stack {
        frame.hash(&mut hasher);
    }
    hasher.finish()
}

pub fn where_in_python(stack: &mut [StackFrame; 128]) -> usize {
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
