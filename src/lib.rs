#![feature(no_sanitize)]

// Main library file that re-exports and coordinates modules
mod allocator;
mod metadata_store;
mod profile;
mod proto;
mod recursion_guard;
mod stack;

use pyo3::prelude::*;

pub use allocator::{set_allocator, MyAllocator};
pub use profile::{get_profile_data, write_heap_proto, PROFILE_DATA};
pub use stack::{where_in_python, StackFrame};

/// A Python module implemented in Rust.
#[pymodule]
fn heap_profiler(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(set_allocator, m)?)?;
    m.add_function(wrap_pyfunction!(get_profile_data, m)?)?;
    m.add_function(wrap_pyfunction!(write_heap_proto, m)?)?;
    Ok(())
}
