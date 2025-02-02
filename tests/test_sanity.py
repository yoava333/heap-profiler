import os

import psutil
import pytest
from heap_profiler import get_profile_data, get_sample_rate, start_tracing

pytestmark = pytest.mark.forked

KiB = 1024
MiB = 1024 * KiB
GiB = 1024 * MiB


def test_validate_forked():
    parent = psutil.Process(os.getppid())
    assert any(
        "pytest" in line for line in parent.cmdline()
    ), "Test should run in a new process, make sure to use pytest-forked"


def test_heap_profiler_sanity():
    """Basic sanity test for heap profiler."""

    start_tracing()
    profile_data = get_profile_data()
    before_total_alloc_bytes = sum(entry.alloc_bytes for entry in profile_data)

    alloc_size = 1000000

    # Create some allocations
    test_list = [1] * alloc_size  # Allocate a large list

    # Get profile data
    profile_data = get_profile_data()

    # Basic assertions
    assert len(profile_data) > 0, "Profile data should not be empty"

    # Check that we captured some allocations
    total_alloc_bytes = sum(entry.alloc_bytes for entry in profile_data)
    assert (
        total_alloc_bytes - before_total_alloc_bytes > alloc_size
    ), "Should have captured some allocations"

    # Verify stack frames are captured
    has_stack_frames = any(len(entry.stack_frames) > 0 for entry in profile_data)
    assert has_stack_frames, "Should have captured stack frames"


def test_invalid_sample_rate():
    with pytest.raises(ValueError):
        start_tracing(sample_rate=0)  # Zero sample rate
    with pytest.raises(ValueError):
        start_tracing(sample_rate=-100)  # Negative sample rate


def test_repeated_start_tracing():
    start_tracing()
    start_tracing()  # Should not raise an exception
    start_tracing(sample_rate=128 * KiB)  # Updating sample rate
    assert get_sample_rate() == 128 * KiB


@pytest.mark.no_tracing
def test_get_profile_data_before_start_tracing():
    profile_data = get_profile_data()
    assert isinstance(
        profile_data, list
    ), "Should return an empty list before starting tracing"
    assert len(profile_data) == 0, "Profile data should be empty before tracing"


def test_large_allocation():
    start_tracing()
    large_data = bytearray(1 * GiB)  # 1GB allocation
    profile_data = get_profile_data()
    total_allocs = sum(entry.alloc_bytes for entry in profile_data)
    assert total_allocs >= 1 * GiB, "Should capture large allocations"


def test_zero_byte_allocation():
    start_tracing()
    zero_data = bytes(0)
    profile_data = get_profile_data()
    # Zero-byte allocations may or may not be captured, but the profiler should not crash
    assert (
        profile_data is not None
    ), "Profiler should handle zero-byte allocations gracefully"


def test_sample_rate():
    from heap_profiler import get_profile_data, get_sample_rate, start_tracing

    start_tracing(sample_rate=1)
    assert get_sample_rate() == 1

    def test_function():
        _ = list(range(10))

    test_function()

    profile_data = get_profile_data()
    assert any(
        "test_function" in frame[1]
        for entry in profile_data
        for frame in entry.stack_frames
    ), "Should have captured the test function"

    assert len(profile_data) > 1, "Should have captured at least one allocation"


def test_c_extension_allocations():
    import numpy as np

    start_tracing()

    # Allocate and free memory using C standard library
    arr = np.ones(MiB, dtype=np.int8)  # 1MB allocation

    profile_data = get_profile_data()

    total_allocs = 0
    for entry in profile_data:
        for frame in entry.stack_frames:
            if "ones" in frame[1]:
                total_allocs += entry.alloc_bytes

    assert total_allocs == MiB, "Should capture allocations from C extensions"


def test_native_stress():
    from malloc_stresser import native_stress

    # This test checks for deadlocks in native threads and their GIL interaction.
    # stack.rs where_in_python() for full details.

    native_stress(threads=100, iterations=1000)
