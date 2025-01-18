// #![feature(no_sanitize)]

// Main library file that re-exports and coordinates modules
mod allocator;
mod metadata_store;
mod platform;
mod profile;
mod proto;
mod recursion_guard;
mod stack;

use pyo3::prelude::*;
use tracing::{debug, info};
use tracing_subscriber::EnvFilter;

pub use allocator::{set_allocator, MyAllocator};
pub use profile::{get_profile_data, write_heap_proto, PROFILE_DATA};
pub use stack::{where_in_python, StackFrame};

/// A Python module implemented in Rust.
#[pymodule]
fn heap_profiler(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize tracing subscriber with a more verbose filter
    std::env::set_var("RUST_LOG", "warn");
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_target(true)
        .init();

    info!("Initializing heap profiler");
    debug!("Setting up Python module");

    m.add_function(wrap_pyfunction!(allocator::set_allocator, m)?)?;
    m.add_function(wrap_pyfunction!(profile::get_profile_data, m)?)?;
    m.add_function(wrap_pyfunction!(profile::write_heap_proto, m)?)?;

    Ok(())
}
