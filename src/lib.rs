#[cfg(Py_GIL_DISABLED)]
compile_error!("heap-profiler requires a GIL-enabled CPython build");
#[cfg(not(all(
    target_pointer_width = "64",
    any(target_arch = "aarch64", target_arch = "x86_64"),
    any(target_os = "linux", target_os = "macos")
)))]
compile_error!("heap-profiler supports 64-bit Linux/macOS on x86_64 and aarch64");
#[cfg(all(target_os = "linux", not(target_env = "gnu")))]
compile_error!("the ELF native patcher currently requires glibc");

// Main library file that re-exports and coordinates modules
mod allocator;
mod fork;
mod interner;
mod metadata_store;
mod platform;
mod profile;
mod proto;
mod recursion_guard;
mod stack;
mod sync;

use pyo3::prelude::*;
use tracing::debug;
use tracing_subscriber::EnvFilter;

pub use allocator::{start_tracing, MyAllocator};
pub use profile::{get_pprof_data, get_profile_data, PROFILE_DATA};
pub use stack::{where_in_python, StackFrame};

/// Serializes unit tests that mutate the global profile/metadata stores.
#[cfg(test)]
pub(crate) static GLOBAL_TEST_LOCK: crate::sync::Mutex<()> = crate::sync::Mutex::new(());

/// Raw sampled counters for one allocation stack.
///
/// Returned by get_profile_data(). Counters are not scaled by the sampling
/// probability; live sampled bytes are alloc_bytes - free_bytes.
#[pyclass(get_all, frozen, module = "heap_profiler")]
pub struct ProfileEntry {
    /// Requested bytes of the sampled allocations made from this stack.
    alloc_bytes: i64,
    /// Requested bytes of those sampled allocations that were later freed.
    free_bytes: i64,
    /// Number of sampled allocation requests made from this stack.
    alloc_objects: i64,
    /// Number of those sampled allocations that were later freed.
    free_objects: i64,
    /// Python frames as (filename, function_name, line_number), innermost first.
    stack_frames: Vec<(String, String, i32)>,
}

#[pymethods]
impl ProfileEntry {
    fn __repr__(&self) -> String {
        format!(
            "ProfileEntry(alloc_bytes={}, free_bytes={}, alloc_objects={}, free_objects={}, stack_frames={:?})",
            self.alloc_bytes, self.free_bytes, self.alloc_objects, self.free_objects, self.stack_frames
        )
    }
}

/// Sampling heap profiler for CPython with pprof export.
///
/// Call start_tracing() to begin sampling allocations, get_pprof_data() to
/// capture a profile for `go tool pprof`, and stop_tracing() to restore the
/// original allocators.
#[pymodule]
fn heap_profiler(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    if unsafe { pyo3::ffi::PyInterpreterState_Get() != pyo3::ffi::PyInterpreterState_Main() } {
        return Err(pyo3::exceptions::PyImportError::new_err(
            "heap-profiler does not support subinterpreters",
        ));
    }
    // Only install a tracing subscriber when explicitly requested: a profiler
    // embedded in a host application must not hijack its tracing registry.
    if let Ok(filter) = std::env::var("HEAP_PROFILER_LOG") {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new(filter))
            .with_file(true)
            .with_ansi(false)
            .with_line_number(true)
            .with_target(true)
            .json()
            .with_writer(std::io::stderr)
            .try_init();
    }

    debug!("Initializing heap profiler");

    m.add_class::<ProfileEntry>()?;
    m.add_function(wrap_pyfunction!(allocator::start_tracing, m)?)?;
    m.add_function(wrap_pyfunction!(allocator::stop_tracing, m)?)?;
    m.add_function(wrap_pyfunction!(allocator::get_sample_rate, m)?)?;
    m.add_function(wrap_pyfunction!(allocator::refresh_native_hooks, m)?)?;
    m.add_function(wrap_pyfunction!(profile::get_profile_data, m)?)?;
    m.add_function(wrap_pyfunction!(profile::get_pprof_data, m)?)?;
    m.add_function(wrap_pyfunction!(profile::reset_profile_data, m)?)?;

    // add the version of the package
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;

    Ok(())
}
