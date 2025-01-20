import pytest


@pytest.fixture(scope="function", autouse=True)
def setup_test_environment():
    """Setup and teardown for each test."""
    import heap_profiler

    heap_profiler.start_tracing()
    # This fixture will run automatically for each test
    yield  # This is where the test runs
    # Any cleanup after the test would go here
