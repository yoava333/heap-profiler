//! Lifecycle coordination for Python and native allocation recording.
mod native;
mod python;
mod recording;
mod sampling;

use crate::profile::{DEFAULT_SAMPLE_RATE, SAMPLE_RATE};
use crate::recursion_guard::MallocRecursionGuard;
use pyo3::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) use native::GLOBAL_PATCHER;
pub use python::MyAllocator;
pub(crate) use python::{ALLOCATOR_REGISTRY, SAVED_ALLOCATORS};
pub(crate) use sampling::reset_sampler_after_fork;

pub(crate) static TRACING: AtomicBool = AtomicBool::new(false);
static NATIVE_ENABLED: AtomicBool = AtomicBool::new(false);

#[pyfunction]
pub fn get_sample_rate() -> i32 {
    SAMPLE_RATE.load(Ordering::Relaxed)
}

#[pyfunction]
#[pyo3(signature = (sample_rate=DEFAULT_SAMPLE_RATE, *, native=true))]
pub fn start_tracing(sample_rate: i32, native: bool) -> PyResult<()> {
    let _guard = MallocRecursionGuard::new();
    // Configure sample rate if provided
    if sample_rate <= 0 {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "Sample rate must be positive",
        ));
    }
    if python::is_installed() && native != NATIVE_ENABLED.load(Ordering::Acquire) {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "Stop tracing before changing native interception",
        ));
    }
    sampling::configure(sample_rate);

    crate::fork::install_atfork_handlers()?;

    if python::is_installed() {
        refresh_native_hooks();
        return Ok(());
    }
    let session = python::prepare_session()?;
    if native {
        native::init_native_hooks()?;
    }
    NATIVE_ENABLED.store(native, Ordering::Release);
    python::install_session(session);

    // Publish only after all original function pointers are resolved.
    TRACING.store(true, Ordering::Release);

    Ok(())
}

/// stop_tracing()
/// --
///
/// Restores the original Python allocators and native hooks. Profile data is
/// kept and can still be read. Frees of allocations sampled before stopping
/// are no longer observed, so inuse numbers drift for memory freed while
/// stopped. Allocator wrappers are retained and reused so in-flight calls and
/// extensions wrapping our allocators remain valid across start/stop cycles.
#[pyfunction]
pub fn stop_tracing() -> PyResult<()> {
    let _guard = MallocRecursionGuard::new();
    TRACING.store(false, Ordering::Release);
    if !python::restore_session() {
        return Ok(());
    }
    native::restore_hooks();

    // Addresses may be reused while stopped. Old metadata must never be
    // matched against allocations from a later tracing session.
    let _transaction = crate::profile::PROFILE_TRANSACTION.lock();
    crate::metadata_store::clear();
    crate::profile::GENERATION.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Re-scan loaded libraries without interposing caller-sensitive dlopen.
#[pyfunction]
pub fn refresh_native_hooks() {
    let _guard = MallocRecursionGuard::new();
    native::refresh_hooks();
}
