"""Sampling heap profiler for CPython with pprof export.

Call start_tracing() to begin sampling allocations, get_pprof_data() to
capture a profile for `go tool pprof`, and stop_tracing() to restore the
original allocators.
"""

__version__: str

class ProfileEntry:
    """Raw sampled counters for one allocation stack.

    Returned by get_profile_data(). Counters are not scaled by the sampling
    probability; live sampled bytes are alloc_bytes - free_bytes.
    """

    @property
    def alloc_bytes(self) -> int:
        """Requested bytes of the sampled allocations made from this stack."""
    @property
    def alloc_objects(self) -> int:
        """Number of sampled allocation requests made from this stack."""
    @property
    def free_bytes(self) -> int:
        """Requested bytes of those sampled allocations that were later freed."""
    @property
    def free_objects(self) -> int:
        """Number of those sampled allocations that were later freed."""
    @property
    def stack_frames(self) -> list[tuple[str, str, int]]:
        """Python frames as (filename, function_name, line_number), innermost first."""

def start_tracing(sample_rate: int = 131072, *, native: bool = True) -> None:
    """Start sampling allocations, or update the sampling interval while tracing.

    Args:
        sample_rate: Mean sampling interval in bytes. Defaults to 128 KiB.
            Use 1 to record every allocation request.
        native: Also intercept malloc, calloc, realloc, free, posix_memalign,
            and aligned_alloc calls made by loaded native libraries.

    Calling this again while tracing changes the interval and refreshes native
    hooks. Stop tracing before changing ``native``.

    Raises:
        ValueError: If sample_rate is not positive, or native differs from
            the running session.
        OverflowError: If sample_rate does not fit in a signed 32-bit integer.
        RuntimeError: If the allocator hooks or fork handlers cannot be
            installed.
    """

def refresh_native_hooks() -> None:
    """Hook allocator calls in native libraries loaded since tracing started.

    Library loading is not intercepted, so call this after importing native
    extensions or loading shared libraries. Does nothing unless tracing with
    native interception enabled.
    """

def stop_tracing() -> None:
    """Stop sampling and restore the original Python allocators and native hooks.

    Recorded profile data is kept and can still be read. Frees that happen
    while tracing is stopped are not observed, so capture a snapshot before
    stopping. Calling this when tracing is not active does nothing.
    """

def reset_profile_data() -> None:
    """Discard all recorded samples and start a fresh accounting interval.

    Tracing state and the sampling interval are unchanged. Allocations
    sampled before the reset are forgotten, including live ones, and their
    later frees are not counted.
    """

def get_profile_data() -> list[ProfileEntry]:
    """Return raw sampled counters for each distinct allocation stack.

    Returns a list of ProfileEntry. Counters are not scaled by the sampling
    probability; use get_pprof_data() for estimated totals.
    """

def get_pprof_data(tags: dict[str, str] | None = None, compress: bool = False) -> bytes:
    """Return the current profile in pprof protobuf format.

    Args:
        tags: Optional key/value pairs stored as profile comments.
        compress: Gzip the result, as expected for ``.pb.gz`` files.

    Sample values are estimated totals: alloc_objects, alloc_space,
    inuse_objects, and inuse_space (the default view). Open the result with
    ``go tool pprof``.
    """

def get_sample_rate() -> int:
    """Return the current mean sampling interval in bytes."""
