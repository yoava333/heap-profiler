"""Regression tests exercise the public API in bounded subprocesses."""

import json
import os
import subprocess
import sys
import textwrap

import pytest

pytestmark = pytest.mark.no_tracing


def run_code(code, *, env=None):
    result = subprocess.run(  # noqa: S603
        [sys.executable, "-c", textwrap.dedent(code)],
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )
    assert result.returncode == 0, result.stdout + result.stderr


def test_snapshots_and_reset_at_full_sampling():
    run_code("""
        import gzip
        import heap_profiler as h
        h.start_tracing(1)
        live = [bytearray(i) for i in range(32, 10000, 13)]
        for _ in range(30):
            assert h.get_profile_data()
            assert isinstance(h.get_pprof_data(), bytes)
            assert gzip.decompress(h.get_pprof_data(compress=True))
            h.reset_profile_data()
            live.append(bytearray(4096))
        h.stop_tracing()
    """)


def test_depth_limit_does_not_retain_caller_locals():
    run_code("""
        import gc, weakref
        import heap_profiler as h
        class Payload: pass
        refs = []
        def descend(n):
            payload = Payload()
            refs.append(weakref.ref(payload))
            if n:
                return descend(n - 1)
            return bytearray(100000)
        h.start_tracing(1)
        block = descend(150)
        h.stop_tracing()
        gc.collect()
        assert not any(ref() is not None for ref in refs)
    """)


def test_dynamic_code_frames_are_included():
    run_code("""
        import heap_profiler as h
        h.start_tracing(1)
        namespace = {}
        exec(compile("def generated(): return bytearray(1000000)", "<generated>", "exec"), namespace)
        block = namespace["generated"]()
        frames = [f for e in h.get_profile_data() for f in e.stack_frames]
        h.stop_tracing()
        assert any(f[0] == "<generated>" and f[1] == "generated" for f in frames)
    """)


def test_sampling_rate_changes_do_not_rescale_existing_samples():
    run_code("""
        import heap_profiler as h
        h.start_tracing(1)
        live = bytearray(1000000)
        # Period describes the current interval; per-allocation weights must
        # survive a subsequent configuration change and the eventual free.
        h.start_tracing(2147483647)
        del live
        entries = h.get_profile_data()
        h.stop_tracing()
        assert all(e.free_bytes <= e.alloc_bytes for e in entries)
        assert all(e.free_objects <= e.alloc_objects for e in entries)
    """)


def test_trace_logging_does_not_recurse_in_allocator():
    run_code(
        """
        import heap_profiler as h
        h.start_tracing(1)
        for _ in range(10):
            block = bytearray(10000)
            h.get_pprof_data()
        h.stop_tracing()
    """,
        env={**os.environ, "HEAP_PROFILER_LOG": "heap_profiler=trace"},
    )


def test_reset_during_native_churn():
    run_code("""
        import heap_profiler as h
        import threading, zlib
        h.start_tracing(512)
        def work():
            for _ in range(100): zlib.compress(bytes(100000))
        threads = [threading.Thread(target=work) for _ in range(4)]
        for t in threads: t.start()
        for _ in range(100):
            h.reset_profile_data()
            entries = h.get_profile_data()
            assert all(e.free_bytes <= e.alloc_bytes for e in entries)
        for t in threads: t.join()
        h.stop_tracing()
    """)


def test_sampling_preserves_exception_and_gc_state():
    run_code("""
        import gc, sys
        import heap_profiler as h
        h.start_tracing(1)
        for enabled in (True, False):
            gc.enable() if enabled else gc.disable()
            try:
                raise ValueError("sentinel")
            except ValueError as error:
                for _ in range(100): block = bytearray(4096)
                assert sys.exception() is error
                assert gc.isenabled() == enabled
        h.stop_tracing()
        gc.enable()
    """)


def test_python_only_mode_and_restart():
    run_code("""
        import heap_profiler as h
        h.start_tracing(1, native=False)
        block = bytearray(1000000)
        assert sum(e.alloc_bytes for e in h.get_profile_data()) >= 1000000
        h.refresh_native_hooks()
        h.start_tracing(512, native=False)
        try:
            h.start_tracing(1, native=True)
        except ValueError:
            pass
        else:
            raise AssertionError("native mode changed while running")
        assert h.get_sample_rate() == 512
        h.stop_tracing()
        h.start_tracing(1, native=True)
        h.stop_tracing()
    """)


def test_snapshot_outputs_are_not_profiled():
    run_code("""
        import heap_profiler as h
        a = b = blob = None
        h.start_tracing(1, native=False)
        h.reset_profile_data()
        a = h.get_profile_data()
        b = h.get_profile_data()
        assert len(a) == len(b) == 0
        h.reset_profile_data()
        blob = h.get_pprof_data()
        a = h.get_profile_data()
        assert len(a) == 0
        h.stop_tracing()
    """)


@pytest.mark.skipif(sys.version_info < (3, 14), reason="direct frame reads exist on CPython 3.14+")
def test_direct_frame_reads_are_used_on_this_interpreter():
    # A layout mismatch silently falls back to the slower C-API walk. Fail
    # instead, so a new CPython minor version gets a layout entry.
    result = subprocess.run(
        [sys.executable, "-c", "import heap_profiler as h; h.start_tracing(); h.stop_tracing()"],
        capture_output=True,
        text=True,
        timeout=30,
        env={**os.environ, "HEAP_PROFILER_LOG": "heap_profiler=debug"},
    )
    assert result.returncode == 0, result.stderr
    layouts = [
        record["fields"]["valid"]
        for line in result.stderr.splitlines()
        if line.startswith("{")
        for record in [json.loads(line)]
        if record.get("fields", {}).get("message") == "Frame layout table"
    ]
    assert layouts == [True]
