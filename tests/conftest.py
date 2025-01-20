import pytest


def pytest_configure(config):
    config.addinivalue_line("markers", "no_tracing: skip heap tracing for this test")


@pytest.fixture(scope="function", autouse=True)
def setup_test_environment(request):
    """Setup and teardown for each test."""
    if request.node.get_closest_marker("no_tracing"):
        yield
        return

    import heap_profiler

    heap_profiler.start_tracing()
    # This fixture will run automatically for each test
    yield  # This is where the test runs
    # Any cleanup after the test would go here
