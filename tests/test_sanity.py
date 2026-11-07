import gc
import os

import psutil
import pytest
from proto.profile_pb2 import Profile

from heap_profiler import (
    get_pprof_data,
    get_profile_data,
    get_sample_rate,
    start_tracing,
)

pytestmark = pytest.mark.forked

KiB = 1024
MiB = 1024 * KiB
GiB = 1024 * MiB


def test_validate_forked():
    parent = psutil.Process(os.getppid())
    assert any("pytest" in line for line in parent.cmdline()), (
        "Test should run in a new process, make sure to use pytest-forked"
    )


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
    assert total_alloc_bytes - before_total_alloc_bytes > alloc_size, "Should have captured some allocations"

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
    assert isinstance(profile_data, list), "Should return an empty list before starting tracing"
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
    assert profile_data is not None, "Profiler should handle zero-byte allocations gracefully"


def test_sample_rate():
    from heap_profiler import get_profile_data, get_sample_rate, start_tracing

    start_tracing(sample_rate=1)
    assert get_sample_rate() == 1

    def test_function():
        _ = list(range(10))

    test_function()

    profile_data = get_profile_data()
    assert any("test_function" in frame[1] for entry in profile_data for frame in entry.stack_frames), (
        "Should have captured the test function"
    )

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

    assert total_allocs >= MiB, "Should capture allocations from C extensions"


def test_heap_sampling_scaling():
    # Enable profiling with a known sample rate for predictable scaling
    sample_rate = 1024  # 1KB
    start_tracing(sample_rate=sample_rate)

    # Create some allocations to generate profile data
    data = []
    allocation_size = 1024  # 1KB allocations
    num_allocations = 1000
    for _ in range(num_allocations):
        data.append(bytearray(allocation_size))

    # Get unscaled data
    profile_data = get_profile_data()
    assert len(profile_data) > 0

    # Get pprof data and decode it
    pprof_bytes = get_pprof_data()
    assert pprof_bytes is not None

    # Decode the protobuf
    profile = Profile()
    profile.ParseFromString(pprof_bytes)

    # Calculate total allocations from profile data
    total_unscaled_bytes = sum(record.alloc_bytes for record in profile_data)
    total_unscaled_objects = sum(record.alloc_objects for record in profile_data)

    # Calculate total scaled allocations from pprof data
    total_scaled_bytes = sum(sample.value[1] for sample in profile.sample)  # alloc_space
    total_scaled_objects = sum(sample.value[0] for sample in profile.sample)  # alloc_objects

    print(f"\nTotal unscaled: {total_unscaled_objects} objects, {total_unscaled_bytes} bytes")
    print(f"Total scaled: {total_scaled_objects} objects, {total_scaled_bytes} bytes")

    # Our test allocations should be a significant portion of the total
    # Calculate the expected size of our test allocations
    expected_test_bytes = allocation_size * num_allocations

    # The total scaled bytes should be at least as large as our test allocations
    assert total_scaled_bytes >= expected_test_bytes, (
        f"Total scaled bytes {total_scaled_bytes} should be >= test allocations {expected_test_bytes}"
    )

    # The scaling should increase the totals
    assert total_scaled_bytes > total_unscaled_bytes, "Scaled bytes should be larger than unscaled bytes"
    assert total_scaled_objects > total_unscaled_objects, "Scaled objects should be larger than unscaled objects"

    # Calculate average scaling factors
    bytes_scale = total_scaled_bytes / total_unscaled_bytes
    objects_scale = total_scaled_objects / total_unscaled_objects

    print(f"\nScaling factors: bytes={bytes_scale:.2f}x, objects={objects_scale:.2f}x")

    # The scaling factors should be reasonable (not too small or large)
    assert 1.0 <= bytes_scale <= 100.0, f"Bytes scaling factor {bytes_scale} is out of reasonable range"
    assert 1.0 <= objects_scale <= 100.0, f"Objects scaling factor {objects_scale} is out of reasonable range"

    # Clean up
    data = None
    gc.collect()
    start_tracing()
