import json
import os
import signal
import traceback

import pytest
from _pytest.runner import runtestprotocol


def pytest_configure(config):
    config.addinivalue_line("markers", "no_tracing: skip heap tracing for this test")
    config.addinivalue_line("markers", "forked: run the test in a forked child process")


@pytest.hookimpl(tryfirst=True)
def pytest_runtest_protocol(item, nextitem):
    """Run tests marked forked in a child, so global hooks cannot leak between tests.

    This replaces pytest-forked, which serializes reports with marshal and fails
    every test on pytest 9, whose reports carry an unmarshalable NodeId. Reports
    cross the process boundary through pytest's own serialization hooks.
    """
    if item.get_closest_marker("forked") is None:
        return None
    item.ihook.pytest_runtest_logstart(nodeid=item.nodeid, location=item.location)
    for report in _run_forked(item):
        item.ihook.pytest_runtest_logreport(report=report)
    item.ihook.pytest_runtest_logfinish(nodeid=item.nodeid, location=item.location)
    return True


def _run_forked(item):
    hook = item.config.hook
    read_fd, write_fd = os.pipe()
    pid = os.fork()
    if pid == 0:
        os.close(read_fd)
        try:
            reports = runtestprotocol(item, log=False)
            data = [hook.pytest_report_to_serializable(config=item.config, report=report) for report in reports]
            with os.fdopen(write_fd, "w") as stream:
                json.dump(data, stream)
        except BaseException:
            traceback.print_exc()
            os._exit(1)
        os._exit(0)

    os.close(write_fd)
    # Drain the pipe before waiting, so a large report cannot block the child.
    with os.fdopen(read_fd) as stream:
        payload = stream.read()
    _, status = os.waitpid(pid, 0)
    if os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0:
        return [hook.pytest_report_from_serializable(config=item.config, data=data) for data in json.loads(payload)]

    if os.WIFSIGNALED(status):
        reason = f"CRASHED with signal {os.WTERMSIG(status)} ({signal.Signals(os.WTERMSIG(status)).name})"
    else:
        reason = f"EXITED with status {os.WEXITSTATUS(status)}"
    message = f"{item.location[0]}:{item.location[1]}: running the test {reason}"
    call = pytest.CallInfo.from_call(lambda: pytest.fail(message, pytrace=False), "call")
    return [hook.pytest_runtest_makereport(item=item, call=call)]


@pytest.fixture(scope="function", autouse=True)
def setup_test_environment(request):
    """Setup and teardown for each test."""
    if request.node.get_closest_marker("no_tracing"):
        yield
        return

    import heap_profiler

    heap_profiler.start_tracing()
    try:
        yield
    finally:
        heap_profiler.stop_tracing()
        heap_profiler.reset_profile_data()
