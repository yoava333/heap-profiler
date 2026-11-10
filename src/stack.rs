use once_cell::sync::Lazy;
use pyo3::ffi;
use std::ffi::CStr;
use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::interner::intern;
use crate::recursion_guard::PythonGCGuard;

static EMPTY_STRING: Lazy<Arc<str>> = Lazy::new(|| Arc::from(""));

const NATIVE_THREAD_MARKER: &str = "<native thread>";
const NATIVE_FUNCTION_MARKER: &str = "<native function>";
static NATIVE_FILENAME: Lazy<Arc<str>> = Lazy::new(|| Arc::from(NATIVE_THREAD_MARKER));
static DETACHED_FILENAME: Lazy<Arc<str>> = Lazy::new(|| Arc::from(DETACHED_THREAD_MARKER));
static NATIVE_FUNCTION: Lazy<Arc<str>> = Lazy::new(|| Arc::from(NATIVE_FUNCTION_MARKER));
pub type StackBuffer = smallvec::SmallVec<[StackFrame; 16]>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackFrame {
    pub filename: Arc<str>,
    pub lineno: i32,
    pub first_lineno: i32,
    pub function_name: Arc<str>,
}

impl Default for StackFrame {
    fn default() -> Self {
        Self {
            filename: Arc::clone(&EMPTY_STRING),
            lineno: 0,
            first_lineno: 0,
            function_name: Arc::clone(&EMPTY_STRING),
        }
    }
}

impl Hash for StackFrame {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.filename.hash(state);
        self.lineno.hash(state);
        self.first_lineno.hash(state);
        self.function_name.hash(state);
    }
}

pub fn calculate_stack_hash(stack: &[StackFrame]) -> u64 {
    // foldhash rather than the standard library's SipHash, which is designed to
    // resist hash flooding that profiler-internal keys never face. A fixed
    // seed needs no initialization inside the allocator hook; collisions are
    // resolved by comparing stacks, so they can never merge distinct stacks.
    let mut hasher = foldhash::fast::FixedState::default().build_hasher();
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

const DETACHED_THREAD_MARKER: &str = "<detached thread>";

/// Reads a unicode attribute of `obj` as an interned UTF-8 string.
/// Returns None (clearing the conversion error) on failure, so a single bad
/// frame doesn't abort the rest of the stack walk.
unsafe fn read_str_attr(obj: *mut ffi::PyObject, attr: &CStr) -> Option<Arc<str>> {
    let value = ffi::PyObject_GetAttrString(obj, attr.as_ptr());
    if value.is_null() {
        ffi::PyErr_Clear();
        warn!("attribute {:?} is null", attr);
        return None;
    }
    let _value_guard = RefCountGuard(value);

    let mut length = 0;
    let ptr = ffi::PyUnicode_AsUTF8AndSize(value, &mut length);
    if ptr.is_null() {
        ffi::PyErr_Clear();
        return None;
    }
    std::str::from_utf8(std::slice::from_raw_parts(ptr.cast(), length as usize))
        .ok()
        .map(intern)
}

unsafe fn walk_frames(thread_state: *mut ffi::PyThreadState, stack: &mut StackBuffer) -> usize {
    let mut depth: usize = 0;

    // SAFETY: PyThreadState_GetFrame is safe to call on a valid thread state
    let mut frame = ffi::PyThreadState_GetFrame(thread_state);

    while !frame.is_null() && depth < 128 {
        let _frame_guard = RefCountGuard(frame as *mut ffi::PyObject);

        // Get code object and ensure cleanup
        let code = ffi::PyFrame_GetCode(frame);
        if code.is_null() {
            frame = std::ptr::null_mut();
            ffi::PyErr_Clear();
            break;
        }
        let _code_guard = RefCountGuard(code as *mut ffi::PyObject);

        if let Some(filename) = read_str_attr(code as *mut ffi::PyObject, c"co_filename") {
            debug!("Got filename: {}", filename);
            // Skip frames that are from the importer machinery
            if !filename.starts_with("<frozen importlib.") {
                let function_name = read_str_attr(code as *mut ffi::PyObject, c"co_name")
                    .unwrap_or(Arc::clone(&EMPTY_STRING));
                stack.push(StackFrame {
                    filename,
                    // SAFETY: frame is still valid here as we hold the reference
                    lineno: ffi::PyFrame_GetLineNumber(frame),
                    first_lineno: {
                        let line =
                            ffi::PyObject_GetAttrString(code.cast(), c"co_firstlineno".as_ptr());
                        let _line_guard = RefCountGuard(line);
                        if line.is_null() {
                            ffi::PyErr_Clear();
                            0
                        } else {
                            let value = ffi::PyLong_AsLong(line);
                            if value == -1 && !ffi::PyErr_Occurred().is_null() {
                                ffi::PyErr_Clear();
                                0
                            } else {
                                value as i32
                            }
                        }
                    },
                    function_name,
                });
                depth += 1;
            }
        }

        // Get next frame (previous guard will clean up current frame)
        frame = ffi::PyFrame_GetBack(frame);
    }
    // At the depth limit GetBack has already returned a new reference.
    unsafe {
        ffi::Py_XDECREF(frame.cast());
    }
    depth
}

pub fn where_in_python(stack: &mut StackBuffer) -> usize {
    stack.clear();
    #[cfg(Py_3_13)]
    if unsafe { ffi::Py_IsFinalizing() } != 0 {
        return 0;
    }
    if unsafe { ffi::Py_IsInitialized() } == 0 {
        return 0;
    }
    // PyGILState_Check can return 1 unconditionally after ANY subinterpreter
    // has existed. Compare the attached state with this thread's TLS state:
    // in 3.11 the unchecked getter can return ANOTHER thread's global state.
    // Never acquire a GIL inside malloc.
    let thread_state = unsafe { ffi::compat::PyThreadState_GetUnchecked() };
    let own_state = unsafe { ffi::PyGILState_GetThisThreadState() };
    if own_state.is_null() || thread_state != own_state {
        let detached = !own_state.is_null();
        stack.push(StackFrame {
            filename: Arc::clone(if detached {
                &DETACHED_FILENAME
            } else {
                &NATIVE_FILENAME
            }),
            lineno: 0,
            first_lineno: 0,
            function_name: Arc::clone(&NATIVE_FUNCTION),
        });
        return 1;
    }

    {
        // disable gc
        let _gc_guard = PythonGCGuard::new();

        // Preserve any in-flight exception: the C-API calls below can clobber
        // it, and we run inside arbitrary interpreter state.
        #[cfg(Py_3_12)]
        let exception = unsafe { ffi::PyErr_GetRaisedException() };
        #[cfg(not(Py_3_12))]
        let mut exc_type = std::ptr::null_mut();
        #[cfg(not(Py_3_12))]
        let mut exc_value = std::ptr::null_mut();
        #[cfg(not(Py_3_12))]
        let mut exc_tb = std::ptr::null_mut();
        #[cfg(not(Py_3_12))]
        unsafe {
            ffi::PyErr_Fetch(&mut exc_type, &mut exc_value, &mut exc_tb)
        };

        // SAFETY: thread state is valid and we hold the GIL
        let depth = unsafe { walk_frames(thread_state, stack) };

        #[cfg(Py_3_12)]
        unsafe {
            ffi::PyErr_SetRaisedException(exception);
        }
        #[cfg(not(Py_3_12))]
        unsafe {
            ffi::PyErr_Restore(exc_type, exc_value, exc_tb)
        };
        depth
    }
}

pub(crate) fn initialize_markers() {
    Lazy::force(&EMPTY_STRING);
    Lazy::force(&NATIVE_FILENAME);
    Lazy::force(&DETACHED_FILENAME);
    Lazy::force(&NATIVE_FUNCTION);
}
