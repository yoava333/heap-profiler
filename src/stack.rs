use pyo3::ffi;
use pyo3::prelude::*;
use std::borrow::Cow;
use std::ffi::CStr;
use std::hash::{Hash, Hasher};
use tracing::{debug, warn};

use crate::interner::intern;
use crate::recursion_guard::PythonGCGuard;

#[derive(Debug, Clone)]
pub struct StackFrame {
    pub filename: Cow<'static, str>,
    pub lineno: i32,
    pub bytei: i32,
    pub function_name: Cow<'static, str>,
}

impl Default for StackFrame {
    fn default() -> Self {
        Self {
            filename: Cow::Borrowed(""),
            lineno: 0,
            bytei: 0,
            function_name: Cow::Borrowed(""),
        }
    }
}

impl Hash for StackFrame {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.filename.hash(state);
        self.lineno.hash(state);
        self.bytei.hash(state);
        self.function_name.hash(state);
    }
}

pub fn calculate_stack_hash(stack: &[StackFrame]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for frame in stack {
        frame.hash(&mut hasher);
    }
    hasher.finish()
}

struct RefCountGuard(*mut ffi::PyObject);
impl Drop for RefCountGuard {
    fn drop(&mut self) {
        unsafe { ffi::Py_XDECREF(self.0) };
    }
}

pub fn where_in_python(stack: &mut [StackFrame; 128]) -> usize {
    let mut depth: usize = 0;

    Python::with_gil(|_py| {
        // disable gc
        let _gc_guard = PythonGCGuard::new();

        // SAFETY: PyGILState_GetThisThreadState is safe to call while holding the GIL
        let thread_state = unsafe { ffi::PyGILState_GetThisThreadState() };
        if thread_state.is_null() {
            return 0;
        }

        // SAFETY: PyThreadState_GetFrame is safe to call on a valid thread state
        let mut frame = unsafe { ffi::PyThreadState_GetFrame(thread_state) };
        if frame.is_null() {
            return 0;
        }

        // SAFETY: The following unsafe block handles Python frame traversal
        unsafe {
            while !frame.is_null() && depth < 128 {
                let _frame_guard = RefCountGuard(frame as *mut ffi::PyObject);

                // Get code object and ensure cleanup
                let code = ffi::PyFrame_GetCode(frame);
                if code.is_null() {
                    break;
                }
                let _code_guard = RefCountGuard(code as *mut ffi::PyObject);

                // Get filename from code object
                let co_filename = (*code).co_filename;
                if co_filename.is_null() {
                    warn!("co_filename is null");
                    break;
                }

                let ascii_str = ffi::PyUnicode_AsASCIIString(co_filename);
                if ascii_str.is_null() {
                    warn!("Failed to convert filename to ASCII string");
                    break;
                }
                let _ascii_guard = RefCountGuard(ascii_str as *mut ffi::PyObject);

                // Convert filename to string
                // SAFETY: PyBytes_AsString returns a pointer to the internal buffer
                // which is valid as long as we hold the reference to ascii_str
                let fname = ffi::PyBytes_AsString(ascii_str);
                if fname.is_null() {
                    warn!("Failed to get filename string pointer");
                    break;
                }

                // SAFETY: fname points to a null-terminated ASCII string
                // CStr::from_ptr is safe as long as the string is valid
                match CStr::from_ptr(fname).to_str() {
                    Ok(fname_str) => {
                        debug!("Got filename: {}", fname_str);

                        // Get function name from code object
                        let co_name = (*code).co_name;
                        let mut function_name = Cow::Borrowed("");

                        if !co_name.is_null() {
                            let name_ascii = ffi::PyUnicode_AsASCIIString(co_name);
                            if !name_ascii.is_null() {
                                let name_ptr = ffi::PyBytes_AsString(name_ascii);
                                if !name_ptr.is_null() {
                                    match CStr::from_ptr(name_ptr).to_str() {
                                        Ok(name_str) => {
                                            debug!("Got function name: {}", name_str);
                                            function_name = intern(name_str);
                                        }
                                        Err(e) => warn!(
                                            "Failed to convert function name to string: {}",
                                            e
                                        ),
                                    }
                                } else {
                                    warn!("Failed to get function name string pointer");
                                }
                                ffi::Py_XDECREF(name_ascii);
                            } else {
                                warn!("Failed to convert function name to ASCII string");
                            }
                        } else {
                            warn!("co_name is null");
                        }

                        // Skip frames that are from the importer machinery
                        if !fname_str.starts_with('<') {
                            stack[depth] = StackFrame {
                                filename: intern(fname_str),
                                // SAFETY: frame is still valid here as we hold the reference
                                lineno: ffi::PyFrame_GetLineNumber(frame),
                                bytei: 0,
                                function_name,
                            };
                            depth += 1;
                        }
                    }
                    Err(e) => warn!("Failed to convert filename to string: {}", e),
                }

                // Get next frame (previous guard will clean up current frame)
                frame = ffi::PyFrame_GetBack(frame);
            }
        }
        depth
    })
}
