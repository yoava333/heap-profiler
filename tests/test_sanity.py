import os

import psutil
import pytest
from heap_profiler import get_profile_data, start_tracing

pytestmark = pytest.mark.forked


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
