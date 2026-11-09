import gc
import random
from string import ascii_letters

import numpy as np
import pytest
from proto.profile_pb2 import Profile
from test_utils import memory_stress

from heap_profiler import get_pprof_data, get_profile_data, reset_profile_data, start_tracing

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
        new_chars_list = list(ascii_letters)
        random.shuffle(new_chars_list)
        new_chars = "".join(new_chars_list)
        objects.append((new_chars * (1024 // len(new_chars))).encode())  # 1KB allocations

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
        if "test_main.py" in filename and "test_allocation_line_tracking" in function_name
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
    sample_types = [(profile.string_table[t.type], profile.string_table[t.unit]) for t in profile.sample_type]
    assert (
        "alloc_objects",
        "count",
    ) in sample_types, "Should have allocation count type"
    assert ("alloc_space", "bytes") in sample_types, "Should have allocation bytes type"

    # Validate samples have stack traces
    for sample in profile.sample:
        assert len(sample.location_id) > 0, "Sample should have location"
        assert len(sample.value) == len(profile.sample_type), "Sample should have all values"

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
    relevant_entries = [entry for entry in profile_data if entry.alloc_bytes >= 1024 * 1024]

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
    assert abs(final_allocs - final_frees) < 1024 * 1024, "Memory should be properly freed"


def test_pprof_default_sample_type_is_inuse_space():
    start_tracing()
    data = bytes(b"\x01" * 2 * MiB)

    profile = Profile()
    profile.ParseFromString(get_pprof_data())

    assert profile.string_table[profile.default_sample_type] == "inuse_space"


def test_pprof_total_alloc_comment_emitted_without_tags():
    start_tracing()
    data = bytes(b"\x01" * 2 * MiB)

    profile = Profile()
    profile.ParseFromString(get_pprof_data())

    comments = [profile.string_table[i] for i in profile.comment]
    assert any(c.startswith("total_alloc_bytes:") for c in comments)


def test_non_ascii_filename_in_stack(tmp_path):
    import importlib.util

    mod_path = tmp_path / "módulo_ñ.py"
    mod_path.write_text(
        "def alloc_buffer():\n    return bytes(2 * 1024 * 1024)\n",
        encoding="utf-8",
    )
    spec = importlib.util.spec_from_file_location("modulo_n", mod_path)
    assert spec is not None and spec.loader is not None
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)

    start_tracing()
    data = mod.alloc_buffer()

    frames = [f for entry in get_profile_data() for f in entry.stack_frames]
    assert any("módulo_ñ" in filename for filename, _, _ in frames), "stack should contain the non-ASCII filename"
    # The walk must not stop at the non-ASCII frame: this test file must appear too
    assert any("test_main.py" in filename for filename, _, _ in frames)


@pytest.mark.parametrize(
    ("filename", "function"),
    [
        ("latin1_ñ.py", "función_ñ"),  # one byte per character, not ASCII
        ("bmp_函数.py", "函数"),  # two bytes per character
        ("astral_😀.py", "astral_𐌰"),  # four bytes per character
    ],
)
def test_non_ascii_names_are_exact(filename, function):
    # CPython stores str in 1, 2, or 4 bytes per character; names must decode
    # exactly from each representation.
    namespace = {}
    exec(compile(f"def {function}():\n    return bytes(2 * 1024 * 1024)\n", filename, "exec"), namespace)  # noqa: S102
    reset_profile_data()
    start_tracing(1)
    data = namespace[function]()
    frames = [frame[:2] for entry in get_profile_data() for frame in entry.stack_frames]
    assert (filename, function) in frames
    assert data


def test_profiler_memory_consumption():
    """Test that the profiler itself doesn't consume too much memory or have leaks."""
    start_tracing()
    gc.collect()  # Ensure a clean slate for memory tracking

    initial_memory, final_memory = memory_stress()

    print(f"Initial memory: {initial_memory / 1024 / 1024} MiB")
    print(f"Final memory: {final_memory / 1024 / 1024} MiB")

    # Allow a small increase in memory usage, but not more than 5 MiB
    assert (final_memory - initial_memory) / (1024 * 1024) < 5, "Memory usage increased significantly"
