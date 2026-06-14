use pyo3::ffi;
use pyo3::prelude::*;
use std::borrow::Cow;
use std::ffi::CStr;
use std::hash::{Hash, Hasher};
use tracing::{debug, warn};

use crate::interner::intern;
use crate::recursion_guard::PythonGCGuard;

const NATIVE_THREAD_MARKER: &str = "<native thread>";
const NATIVE_FUNCTION_MARKER: &str = "<native function>";

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

const DETACHED_THREAD_MARKER: &str = "<detached thread>";

/// Reads a unicode attribute of `obj` as an interned UTF-8 string.
/// Returns None (clearing the conversion error) on failure, so a single bad
/// frame doesn't abort the rest of the stack walk.
unsafe fn read_str_attr(obj: *mut ffi::PyObject, attr: &CStr) -> Option<Cow<'static, str>> {
    let value = ffi::PyObject_GetAttrString(obj, attr.as_ptr());
    if value.is_null() {
        ffi::PyErr_Clear();
        warn!("attribute {:?} is null", attr);
        return None;
    }
    let _value_guard = RefCountGuard(value);

    let utf8 = ffi::PyUnicode_AsUTF8String(value);
    if utf8.is_null() {
        ffi::PyErr_Clear();
        warn!("Failed to encode attribute {:?} as UTF-8", attr);
        return None;
    }
    let _utf8_guard = RefCountGuard(utf8);

    // SAFETY: PyBytes_AsString returns a pointer to the internal buffer
    // which is valid as long as we hold the reference to utf8
    let ptr = ffi::PyBytes_AsString(utf8);
    if ptr.is_null() {
        ffi::PyErr_Clear();
        return None;
    }
    CStr::from_ptr(ptr).to_str().ok().map(intern)
}

unsafe fn walk_frames(
    thread_state: *mut ffi::PyThreadState,
    stack: &mut [StackFrame; 128],
) -> usize {
    let mut depth: usize = 0;

    // SAFETY: PyThreadState_GetFrame is safe to call on a valid thread state
    let mut frame = ffi::PyThreadState_GetFrame(thread_state);

    while !frame.is_null() && depth < stack.len() {
        let _frame_guard = RefCountGuard(frame as *mut ffi::PyObject);

        // Get code object and ensure cleanup
        let code = ffi::PyFrame_GetCode(frame);
        if code.is_null() {
            break;
        }
        let _code_guard = RefCountGuard(code as *mut ffi::PyObject);

        if let Some(filename) = read_str_attr(code as *mut ffi::PyObject, c"co_filename") {
            debug!("Got filename: {}", filename);
            // Skip frames that are from the importer machinery
            if !filename.starts_with('<') {
                let function_name = read_str_attr(code as *mut ffi::PyObject, c"co_name")
                    .unwrap_or(Cow::Borrowed(""));
                stack[depth] = StackFrame {
                    filename,
                    // SAFETY: frame is still valid here as we hold the reference
                    lineno: ffi::PyFrame_GetLineNumber(frame),
                    bytei: 0,
                    function_name,
                };
                depth += 1;
            }
        }

        // Get next frame (previous guard will clean up current frame)
        frame = ffi::PyFrame_GetBack(frame);
    }
    depth
}

pub fn where_in_python(stack: &mut [StackFrame; 128]) -> usize {
    // SAFETY: it's safe to call PyGILState_GetThisThreadState without holding the GIL
    let thread_state = unsafe { ffi::PyGILState_GetThisThreadState() };

    // We have to check this outside the GIL!
    // The reason is that we have encountered situations where python native extensions spawn
    // native threads (rdkafka is one such example).
    //
    // If we try to acquire the GIL in native thread, we will be blocked forever.
    // And more importantly, if a python thread is waiting for the native thread to finish,
    // it will be blocked forever.
    // So we have to check this outside the GIL.
    if thread_state.is_null() {
        stack[0] = StackFrame {
            // TODO: add native thread unwinding
            filename: Cow::Borrowed(NATIVE_THREAD_MARKER),
            lineno: 0,
            bytei: 0,
            function_name: Cow::Borrowed(NATIVE_FUNCTION_MARKER),
        };
        return 1;
    }

    // During interpreter finalization (or before init) Python::attach panics,
    // and the native allocation hooks can still fire then: the finalizing
    // thread holds the GIL while tearing down, so the GIL check below would
    // pass. Bail out before touching any frame APIs.
    //
    // A thread that HAS a thread state but released the GIL (ctypes calls,
    // Py_BEGIN_ALLOW_THREADS regions in C extensions) must not block on the
    // GIL inside malloc either: if the GIL holder is waiting on this thread,
    // that is a deadlock. Record a marker frame instead of walking the stack.
    if unsafe { ffi::Py_IsInitialized() } == 0 || unsafe { ffi::PyGILState_Check() } != 1 {
        stack[0] = StackFrame {
            filename: Cow::Borrowed(DETACHED_THREAD_MARKER),
            lineno: 0,
            bytei: 0,
            function_name: Cow::Borrowed(NATIVE_FUNCTION_MARKER),
        };
        return 1;
    }

    Python::attach(|_py| {
        // disable gc
        let _gc_guard = PythonGCGuard::new();

        // Preserve any in-flight exception: the C-API calls below can clobber
        // it, and we run inside arbitrary interpreter state.
        let mut exc_type = std::ptr::null_mut();
        let mut exc_value = std::ptr::null_mut();
        let mut exc_tb = std::ptr::null_mut();
        unsafe { ffi::PyErr_Fetch(&mut exc_type, &mut exc_value, &mut exc_tb) };

        // SAFETY: thread state is valid and we hold the GIL
        let depth = unsafe { walk_frames(thread_state, stack) };

        unsafe { ffi::PyErr_Restore(exc_type, exc_value, exc_tb) };
        depth
    })
}
