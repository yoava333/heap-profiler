//! Allocation-free stack capture for CPython 3.14+.
//!
//! The C-API walk calls PyObject_GetAttrString inside the allocator hook, which
//! creates and frees Python objects, so it must disable the GC and save the
//! pending exception around every sample. This walker instead mirrors
//! Python/tracemalloc.c: it reads this thread's interpreter frames and code
//! object fields directly, using the layout table CPython publishes for
//! out-of-process profilers (`_PyRuntime.debug_offsets`), and borrows ASCII
//! text in place, decoding other strings into a caller-owned buffer. No Python object is
//! created, no reference count is touched, no exception can be raised and no
//! CPython lock is taken. That is also what free-threaded builds will require.
//!
//! The layout is only stable within a minor version. When validation fails,
//! for example on a newer CPython, callers fall back to the C-API walk.
use pyo3::ffi;
use std::ffi::c_int;
use std::sync::OnceLock;

/// Prefix of `struct _Py_DebugOffsets` (Include/internal/pycore_debug_offsets.h).
/// Its layout is only stable within one minor version; validated at runtime.
#[repr(C)]
struct DebugOffsets {
    cookie: [u8; 8],
    version: u64,
    free_threaded: u64,
    runtime_state: [u64; 3],
    // 3.14/3.15: size id next threads_head threads_main gc imports_modules
    // sysdict builtins ceval_gil gil_runtime_state gil_enabled gil_locked
    // gil_holder code_object_generation tlbc_generation
    interpreter_state: [u64; 16],
    thread_state: ThreadStateOffsets,
    #[cfg(Py_3_15)]
    err_stackitem: [u64; 1],
    interpreter_frame: FrameOffsets,
    code_object: CodeOffsets,
}

#[cfg(not(Py_3_15))]
#[repr(C)]
struct ThreadStateOffsets {
    size: u64,
    prev: u64,
    next: u64,
    interp: u64,
    current_frame: u64,
    thread_id: u64,
    native_thread_id: u64,
    datastack_chunk: u64,
    status: u64,
}

#[cfg(Py_3_15)]
#[repr(C)]
struct ThreadStateOffsets {
    size: u64,
    prev: u64,
    next: u64,
    interp: u64,
    current_frame: u64,
    base_frame: u64,
    last_profiled_frame: u64,
    last_profiled_frame_seq: u64,
    thread_id: u64,
    native_thread_id: u64,
    datastack_chunk: u64,
    status: u64,
    holds_gil: u64,
    gil_requested: u64,
    current_exception: u64,
    exc_state: u64,
}

#[repr(C)]
struct FrameOffsets {
    size: u64,
    previous: u64,
    executable: u64,
    instr_ptr: u64,
    localsplus: u64,
    owner: u64,
    stackpointer: u64,
    tlbc_index: u64,
}

#[repr(C)]
struct CodeOffsets {
    size: u64,
    filename: u64,
    name: u64,
    qualname: u64,
    linetable: u64,
    firstlineno: u64,
}

extern "C" {
    // debug_offsets is the first member of _PyRuntimeState by contract.
    static _PyRuntime: DebugOffsets;
}

#[derive(Clone, Copy)]
pub(crate) struct Layout {
    current_frame: usize,
    previous: usize,
    executable: usize,
    owner: usize,
    filename: usize,
    name: usize,
    firstlineno: usize,
}

/// Frames owned by the interpreter or the C stack are never complete.
/// FRAME_OWNED_BY_INTERPRETER == 3 (3.14, 3.15), FRAME_OWNED_BY_CSTACK == 4 (3.14).
const FIRST_INCOMPLETE_OWNER: u8 = 3;
/// _PyStackRef tag bits (Py_TAG_BITS is 1 in free-threaded and 3 in default builds).
const STACKREF_TAG_MASK: usize = 3;

static LAYOUT: OnceLock<Option<Layout>> = OnceLock::new();

fn read_layout() -> Option<Layout> {
    // SAFETY: _PyRuntime is an exported, process-lived static; we only read
    // its immutable debug-offset header.
    let offsets = unsafe { &*std::ptr::addr_of!(_PyRuntime) };
    let expected_minor: u64 = if cfg!(Py_3_15) { 15 } else { 14 };
    let valid = &offsets.cookie == b"xdebugpy"
        && offsets.version >> 24 == 3
        && (offsets.version >> 16) & 0xff == expected_minor
        && (offsets.free_threaded != 0) == cfg!(Py_GIL_DISABLED)
        && offsets.thread_state.current_frame < offsets.thread_state.size
        && offsets.interpreter_frame.previous < offsets.interpreter_frame.size
        && offsets.interpreter_frame.executable < offsets.interpreter_frame.size
        && offsets.interpreter_frame.owner < offsets.interpreter_frame.size
        && offsets.code_object.filename < offsets.code_object.size
        && offsets.code_object.name < offsets.code_object.size
        && offsets.code_object.firstlineno < offsets.code_object.size;
    valid.then_some(Layout {
        current_frame: offsets.thread_state.current_frame as usize,
        previous: offsets.interpreter_frame.previous as usize,
        executable: offsets.interpreter_frame.executable as usize,
        owner: offsets.interpreter_frame.owner as usize,
        filename: offsets.code_object.filename as usize,
        name: offsets.code_object.name as usize,
        firstlineno: offsets.code_object.firstlineno as usize,
    })
}

/// Resolve (and validate) the layout. Call once outside any allocator hook.
pub(crate) fn layout() -> Option<Layout> {
    *LAYOUT.get_or_init(read_layout)
}

#[inline]
unsafe fn read_at<T: Copy>(base: *const u8, offset: usize) -> T {
    std::ptr::read_unaligned(base.add(offset) as *const T)
}

/// One Python frame, with borrowed (frame-owned) string objects.
pub(crate) struct RawFrame {
    pub filename: *mut ffi::PyObject,
    pub name: *mut ffi::PyObject,
    pub lineno: c_int,
    pub first_lineno: c_int,
}

/// Visit this thread's complete Python frames, innermost first, until
/// `max_depth` visits have returned true.
///
/// # Safety
/// `tstate` must be the calling thread's own, attached thread state. The
/// frames and code objects are owned by this thread's stack and cannot be
/// freed while it is inside this allocator callback.
pub(crate) unsafe fn for_each_frame(
    layout: &Layout,
    tstate: *mut ffi::PyThreadState,
    max_depth: usize,
    mut visit: impl FnMut(RawFrame) -> bool,
) {
    let mut frame: *const u8 = read_at(tstate as *const u8, layout.current_frame);
    let code_type = std::ptr::addr_of_mut!(ffi::PyCode_Type);
    let mut depth = 0;
    while !frame.is_null() && depth < max_depth {
        let owner: u8 = read_at(frame, layout.owner);
        if owner < FIRST_INCOMPLETE_OWNER {
            let executable: usize = read_at(frame, layout.executable);
            let code = (executable & !STACKREF_TAG_MASK) as *mut ffi::PyObject;
            if !code.is_null() && ffi::Py_TYPE(code) == code_type {
                let code_bytes = code as *const u8;
                let counted = visit(RawFrame {
                    filename: read_at(code_bytes, layout.filename),
                    name: read_at(code_bytes, layout.name),
                    lineno: ffi::PyUnstable_InterpreterFrame_GetLine(frame as *mut _).max(0),
                    first_lineno: read_at(code_bytes, layout.firstlineno),
                });
                depth += usize::from(counted);
            }
        }
        frame = read_at(frame, layout.previous);
    }
}

/// The text of a str object, without allocating Python memory, taking locks,
/// or raising. ASCII strings, which include nearly all file and function
/// names, are borrowed in place; other strings are decoded into `scratch`.
/// Returns None for non-str values.
///
/// # Safety
/// `value` must be a live object pointer (or null), and the returned text is
/// only valid while that object stays alive.
pub(crate) unsafe fn read_str(value: *mut ffi::PyObject, scratch: &mut String) -> Option<&str> {
    if value.is_null() || ffi::PyUnicode_Check(value) == 0 {
        return None;
    }
    let length = usize::try_from(ffi::PyUnicode_GET_LENGTH(value)).ok()?;
    let data = ffi::PyUnicode_DATA(value);
    scratch.clear();
    match ffi::PyUnicode_KIND(value) {
        ffi::PyUnicode_1BYTE_KIND => {
            let bytes = std::slice::from_raw_parts(data as *const u8, length);
            if bytes.is_ascii() {
                // SAFETY: ASCII is valid UTF-8.
                return Some(std::str::from_utf8_unchecked(bytes));
            }
            scratch.extend(bytes.iter().map(|&byte| char::from(byte))); // Latin-1
        }
        ffi::PyUnicode_2BYTE_KIND => {
            let units = std::slice::from_raw_parts(data as *const u16, length);
            scratch.extend(units.iter().map(|&unit| to_char(unit.into())));
        }
        ffi::PyUnicode_4BYTE_KIND => {
            let units = std::slice::from_raw_parts(data as *const u32, length);
            scratch.extend(units.iter().map(|&unit| to_char(unit)));
        }
        _ => return None,
    }
    Some(scratch.as_str())
}

/// Lone surrogates are legal in Python strings but not in Rust ones.
fn to_char(code_point: u32) -> char {
    char::from_u32(code_point).unwrap_or(char::REPLACEMENT_CHARACTER)
}
