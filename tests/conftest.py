import gc

import pytest
from heap_profiler import start_tracing


@pytest.fixture(autouse=True)
def setup_profiler():
    """Setup the profiler before each test and cleanup after"""
    # Setup
    start_tracing()

    # Run test
    yield

    # Cleanup
    gc.collect()  # Force garbage collection to clean up any remaining objects
