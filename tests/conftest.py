import gc

import pytest
from heap_profiler import set_allocator


@pytest.fixture(autouse=True)
def setup_profiler():
    """Setup the profiler before each test and cleanup after"""
    # Setup
    set_allocator()

    # Run test
    yield

    # Cleanup
    gc.collect()  # Force garbage collection to clean up any remaining objects
