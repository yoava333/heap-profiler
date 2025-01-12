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
        // SAFETY: PyGILState_GetThisThreadState is safe to call while holding the GIL
        // It returns the current thread state which is guaranteed to be valid
        let thread_state = unsafe { ffi::PyGILState_GetThisThreadState() };

        // SAFETY: PyThreadState_GetFrame is safe to call on a valid thread state
        // The returned frame pointer may be null, which we check
        let mut frame = if !thread_state.is_null() {
            unsafe { ffi::PyThreadState_GetFrame(thread_state) }
        } else {
            std::ptr::null_mut()
        };

        if frame.is_null() {
            return 0;
        }

        // SAFETY: The following unsafe block handles Python frame traversal
        // All operations are safe because:
        // 1. We hold the GIL throughout
        // 2. We check all pointers for null before dereferencing
        // 3. We properly manage reference counts with Py_DECREF and Py_XDECREF
        // 4. We stay within the bounds of the stack array (depth < 128)
        unsafe {
            while !frame.is_null() && depth < 128 {
                // Get code object from frame
                let code = ffi::PyFrame_GetCode(frame);
                if code.is_null() {
                    break;
                }

                // Get filename from code object
                // SAFETY: code is guaranteed non-null here, and co_filename is a required field
                let co_filename = (*code).co_filename;
                let ascii_str = ffi::PyUnicode_AsASCIIString(co_filename);
                if ascii_str.is_null() {
                    // Clean up code object if filename conversion fails
                    ffi::Py_DECREF(code as *mut ffi::PyObject);
                    break;
                }

                // Convert filename to string
                // SAFETY: PyBytes_AsString returns a pointer to the internal buffer
                // which is valid as long as we hold the reference to ascii_str
                let fname = ffi::PyBytes_AsString(ascii_str);
                if !fname.is_null() {
                    // SAFETY: fname points to a null-terminated ASCII string
                    // CStr::from_ptr is safe as long as the string is valid
                    if let Ok(fname_str) = CStr::from_ptr(fname).to_str() {
                        stack[depth] = StackFrame {
                            filename: fname_str.to_string(),
                            // SAFETY: frame is still valid here as we hold the reference
                            lineno: ffi::PyFrame_GetLineNumber(frame),
                            bytei: 0,
                        };
                        depth += 1;
                    }
                }

                // Clean up references
                // SAFETY: Py_XDECREF handles null pointers safely
                ffi::Py_XDECREF(ascii_str);
                // SAFETY: code is guaranteed non-null here
                ffi::Py_DECREF(code as *mut ffi::PyObject);

                // Get next frame
                // SAFETY: PyFrame_GetBack returns a new reference or null
                let next_frame = ffi::PyFrame_GetBack(frame);
                // SAFETY: Py_XDECREF handles null pointers safely
                ffi::Py_XDECREF(frame as *mut ffi::PyObject);
                frame = next_frame;
            }
        }
        depth
    })
}
