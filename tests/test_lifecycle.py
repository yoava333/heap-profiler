import gc
import lzma
import os
import signal
import threading
import time
import zlib

import pytest

from heap_profiler import (
    get_profile_data,
    reset_profile_data,
    start_tracing,
    stop_tracing,
)

pytestmark = pytest.mark.forked

KiB = 1024
MiB = 1024 * KiB

DETACHED_MARKER = "<detached thread>"


def _all_frames():
    return [f for entry in get_profile_data() for f in entry.stack_frames]


def test_reset_profile_data():
    start_tracing(sample_rate=1)
    keep_a = bytes(1 * MiB)
    assert len(get_profile_data()) > 0

    reset_profile_data()
    immediately_after = sum(e.alloc_bytes for e in get_profile_data())
    assert immediately_after < 1 * MiB, "reset must clear accumulated data"

    keep_b = bytes(2 * MiB)
    resumed = sum(e.alloc_bytes for e in get_profile_data())
    assert resumed >= 2 * MiB, "recording must continue after reset"


def test_gil_released_allocations_get_detached_marker():
    """Allocations made while a thread has released the GIL must be recorded
    with a marker frame instead of blocking on the GIL inside malloc."""
    start_tracing(sample_rate=1)

    # lzma/zlib release the GIL during (de)compression and their C internals
    # allocate work buffers through libc malloc in that region.
    payload = os.urandom(4 * MiB)
    for preset in (1, 6):
        lzma.compress(payload, preset=preset)
    for level in (1, 6):
        zlib.compress(payload, level=level)

    assert any(f[0] == DETACHED_MARKER for f in _all_frames()), (
        f"expected sampled allocations from GIL-released regions to carry the {DETACHED_MARKER} marker"
    )


def test_stop_tracing_stops_recording():
    start_tracing(sample_rate=1)
    keep_a = bytes(1 * MiB)
    before = sum(e.alloc_bytes for e in get_profile_data())

    stop_tracing()
    keep_b = [bytes(1 * MiB) for _ in range(8)]
    after = sum(e.alloc_bytes for e in get_profile_data())
    assert after - before < 4 * MiB, "allocations after stop_tracing must not be recorded"

    # Restartable
    start_tracing(sample_rate=1)
    keep_c = bytes(4 * MiB)
    resumed = sum(e.alloc_bytes for e in get_profile_data())
    assert resumed - after >= 4 * MiB, "tracing must resume after restart"


@pytest.mark.no_tracing
def test_stop_tracing_without_start_is_noop():
    stop_tracing()
    stop_tracing()


def test_clean_interpreter_shutdown_with_tracing():
    """The wrapped allocators must delegate (not fail) during interpreter
    startup remnants and finalization."""
    import subprocess
    import sys

    code = (
        "import heap_profiler, atexit\n"
        "heap_profiler.start_tracing(sample_rate=1)\n"
        "blocks = [bytes(4096) for _ in range(1000)]\n"
        "atexit.register(lambda: [bytes(4096) for _ in range(100)])\n"
    )
    result = subprocess.run(  # noqa: S603
        [sys.executable, "-c", code],
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert result.returncode == 0, f"stderr: {result.stderr}"


def test_fork_with_allocating_threads_does_not_deadlock():
    """A child forked while other threads hold profiler locks must not
    deadlock on its first allocation/free."""
    start_tracing(sample_rate=512)
    stop = threading.Event()

    def churn():
        while not stop.is_set():
            data = [bytes(128) for _ in range(50)]
            del data

    threads = [threading.Thread(target=churn, daemon=True) for _ in range(4)]
    for t in threads:
        t.start()

    try:
        for _ in range(10):
            pid = os.fork()
            if pid == 0:
                # Child: allocate, free, read profile data, exit cleanly.
                try:
                    data = [bytes(1024) for _ in range(1000)]
                    del data
                    gc.collect()
                    get_profile_data()
                    os._exit(0)
                except BaseException:
                    os._exit(1)
            deadline = time.monotonic() + 30
            status = None
            while time.monotonic() < deadline:
                done_pid, status = os.waitpid(pid, os.WNOHANG)
                if done_pid:
                    break
                time.sleep(0.05)
            else:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
                pytest.fail("forked child deadlocked (killed after 30s)")  # ty: ignore[invalid-argument-type]
            assert os.waitstatus_to_exitcode(status) == 0
    finally:
        stop.set()


def test_concurrent_gil_released_allocations_no_deadlock():
    """Regression smoke test: concurrent compression in many threads while
    other threads hold/contend the GIL must not deadlock."""
    start_tracing(sample_rate=64)

    def worker():
        for _ in range(20):
            zlib.compress(os.urandom(256 * KiB), level=6)

    threads = [threading.Thread(target=worker) for _ in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=120)
        assert not t.is_alive(), "worker thread deadlocked"
