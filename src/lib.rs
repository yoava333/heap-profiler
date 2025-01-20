// Main library file that re-exports and coordinates modules
mod allocator;
mod interner;
mod metadata_store;
mod platform;
mod profile;
mod proto;
mod recursion_guard;
mod stack;

use pyo3::prelude::*;
use tracing::debug;
use tracing_subscriber::EnvFilter;

pub use allocator::{start_tracing, MyAllocator};
pub use profile::{get_pprof_data, get_profile_data, PROFILE_DATA};
pub use stack::{where_in_python, StackFrame};

#[pyclass(get_all)]
pub struct ProfileEntry {
    alloc_bytes: i64,
    free_bytes: i64,
    alloc_objects: i64,
    free_objects: i64,
    stack_frames: Vec<(String, i32)>,
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

/// Initialize the heap profiler
#[pymodule]
fn heap_profiler(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize tracing subscriber with a more verbose filter
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_file(true)
        .with_ansi(false)
        .with_line_number(true)
        .with_target(true)
        .json()
        .with_writer(std::io::stderr)
        .try_init();

    debug!("Initializing heap profiler");

    m.add_class::<ProfileEntry>()?;
    m.add_function(wrap_pyfunction!(allocator::start_tracing, m)?)?;
    m.add_function(wrap_pyfunction!(profile::get_profile_data, m)?)?;
    m.add_function(wrap_pyfunction!(profile::get_pprof_data, m)?)?;

    Ok(())
}
