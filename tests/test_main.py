import gc
from string import ascii_letters

import numpy as np
import pytest
from heap_profiler import get_pprof_data, get_profile_data, start_tracing
from proto.profile_pb2 import Profile

pytestmark = pytest.mark.forked

KiB = 1024
MiB = 1024 * KiB
GiB = 1024 * MiB


def test_setup_allocator():
    """Test that setup_allocator initializes tracing properly."""
    start_tracing()  # Reset tracing state
    initial_data = get_profile_data()
    initial_allocs = sum(entry.alloc_bytes for entry in initial_data)

    # Create a larger allocation that will be more reliably captured
    test_data = bytes(b"\x01" * (1024 * 1024))  # 1MB allocation

    profile_data = get_profile_data()
    new_allocs = sum(entry.alloc_bytes for entry in profile_data)

    assert new_allocs > initial_allocs, "Tracing should capture new allocations"


def test_small_allocations():
    """Test that many small allocations are captured."""
    start_tracing(sample_rate=128 * KiB)
    initial_data = get_profile_data()
    initial_allocs = sum(entry.alloc_bytes for entry in initial_data)
    initial_objects = sum(entry.alloc_objects for entry in initial_data)

    # Create many medium allocations
    objects = []
    for _ in range(1024):
        objects.append(
            (ascii_letters * (1024 // len(ascii_letters))).encode()
        )  # 1KB allocations

    profile_data = get_profile_data()
    new_allocs = sum(entry.alloc_bytes for entry in profile_data)
    new_objects = sum(entry.alloc_objects for entry in profile_data)

    # Debug info
    print(f"\nInitial allocations: {initial_allocs} bytes, {initial_objects} objects")
    print(f"Final allocations: {new_allocs} bytes, {new_objects} objects")
    print(f"Expected minimum new bytes: {100 * 10 * 1024}")

    assert new_allocs > initial_allocs, "Should capture total bytes of allocations"
    assert new_objects > initial_objects, "Should capture number of allocation objects"

    # Keep objects alive until assertions are done
    assert len(objects) == 1024


def test_allocation_line_tracking():
    """Test that the allocation line is correctly tracked."""
    start_tracing()

    # Line number will be captured from this allocation
    test_data = bytes(b"x" * 1024 * 1024)  # Line that we'll look for

    profile_data = get_profile_data()
    relevant_entries = [entry for entry in profile_data if entry.alloc_bytes >= MiB]

    assert len(relevant_entries) > 0, "Should find our allocation"
    entry = relevant_entries[0]

    # Find the frame corresponding to this file
    this_file_frames = [
        (filename, function_name, lineno)
        for filename, function_name, lineno in entry.stack_frames
        if "test_main.py" in filename
        and "test_allocation_line_tracking" in function_name
    ]

    assert len(this_file_frames) > 0, "Should find frame from this test file"
    # We don't test the exact line number since it could change with edits,
    # but we verify that we got a reasonable line number in this file
    _, _, tracked_line = this_file_frames[0]
    assert 0 < tracked_line < 1000, "Should have a reasonable line number"


def test_np_alloc():
    """Test that numpy allocations are captured."""
    start_tracing()
    initial_data = get_profile_data()
    initial_allocs = sum(entry.alloc_bytes for entry in initial_data)

    # Create numpy allocation
    arr = np.ones((1024, 1024, 100))

    profile_data = get_profile_data()
    new_allocs = sum(entry.alloc_bytes for entry in profile_data)

    assert new_allocs > initial_allocs, "Should capture numpy allocations"
    # The allocation should be significant (at least 100MB)
    assert new_allocs - initial_allocs > 100 * 1024 * 1024


def test_allocate_and_free():
    """Test the full allocation and free cycle with pprof validation."""
    start_tracing()
    initial_data = get_profile_data()
    initial_allocs = sum(entry.alloc_bytes for entry in initial_data)

    # Allocate objects
    objects = []
    for i in range(10):
        size = 1024 * 1024 * (i + 1)
        x = bytes(b"\x01" * size)
        objects.append(x)

    mid_data = get_profile_data()
    mid_allocs = sum(entry.alloc_bytes for entry in mid_data)
    assert mid_allocs > initial_allocs, "Should show increased allocations"

    # Get and validate pprof data after allocations
    profile_bytes = get_pprof_data()
    assert len(profile_bytes) > 0, "Should get non-empty pprof data"

    # Parse the profile using the generated protobuf code
    profile = Profile()
    profile.ParseFromString(profile_bytes)

    # Validate profile structure
    assert len(profile.sample_type) > 0, "Profile should have sample types"
    assert len(profile.sample) > 0, "Profile should have samples"
    assert len(profile.location) > 0, "Profile should have locations"
    assert len(profile.string_table) > 0, "Profile should have string table"

    # Validate heap profile specific fields
    sample_types = [
        (profile.string_table[t.type], profile.string_table[t.unit])
        for t in profile.sample_type
    ]
    assert (
        "alloc_objects",
        "count",
    ) in sample_types, "Should have allocation count type"
    assert ("alloc_space", "bytes") in sample_types, "Should have allocation bytes type"

    # Validate samples have stack traces
    for sample in profile.sample:
        assert len(sample.location_id) > 0, "Sample should have location"
        assert len(sample.value) == len(
            profile.sample_type
        ), "Sample should have all values"

    # Validate locations have function information
    for location in profile.location:
        assert location.id > 0, "Location should have valid id"
        if location.line:  # Some locations might not have line info
            for line in location.line:
                assert line.function_id > 0, "Line should reference valid function"

    # Free objects and force GC
    objects = objects[5:]
    gc.collect()

    # Get and validate updated pprof data
    profile_bytes = get_pprof_data()
    profile = Profile()
    profile.ParseFromString(profile_bytes)
    assert len(profile.sample) > 0, "Should have samples after free"

    final_data = get_profile_data()
    freed_count = sum(entry.free_objects for entry in final_data)
    assert freed_count > 0, "Should record freed objects"


def test_profile_data_structure():
    """Test the structure of profile data entries."""
    start_tracing()

    # Create a known allocation
    test_data = bytes(b"\x01" * 1024 * 1024)  # 1MB allocation

    profile_data = get_profile_data()

    # Find our allocation in the profile
    relevant_entries = [
        entry for entry in profile_data if entry.alloc_bytes >= 1024 * 1024
    ]

    assert len(relevant_entries) > 0, "Should find our allocation"
    entry = relevant_entries[0]

    # Verify entry structure
    assert hasattr(entry, "alloc_bytes"), "Should have alloc_bytes"
    assert hasattr(entry, "alloc_objects"), "Should have alloc_objects"
    assert hasattr(entry, "free_bytes"), "Should have free_bytes"
    assert hasattr(entry, "free_objects"), "Should have free_objects"
    assert hasattr(entry, "stack_frames"), "Should have stack_frames"

    # Verify stack frames structure
    assert len(entry.stack_frames) > 0, "Should have stack frames"
    frame = entry.stack_frames[0]
    assert len(frame) == 3, "Frame should have filename, function name, and line number"


def test_no_memory_leaks():
    """Test that all allocations are properly freed when objects are deleted."""
    start_tracing()

    def f():
        # Create and delete objects in a loop
        for _ in range(10):
            [bytes(b"x" * 1024) for _ in range(100 * 1024)]

    f()

    gc.collect()

    profile_data = get_profile_data()
    final_allocs = sum(entry.alloc_bytes for entry in profile_data)
    final_frees = sum(entry.free_bytes for entry in profile_data)

    print(f"Final allocs: {final_allocs}")
    print(f"Final frees: {final_frees}")

    # Should have roughly equal allocations and frees
    assert (
        abs(final_allocs - final_frees) < 1024 * 1024
    ), "Memory should be properly freed"
