"""Test real native call sites, not ctypes' cached dlsym function pointers."""

import ctypes
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import threading

import pytest
from proto.profile_pb2 import Profile

import heap_profiler as h

pytestmark = [pytest.mark.forked, pytest.mark.no_tracing]


@pytest.fixture
def native(tmp_path):
    compiler = shutil.which("cc")
    if not compiler:
        pytest.skip("native regression tests require a C compiler")
    # The patcher once skipped every library whose path contained this
    # package's name, so every native test now runs from such a directory.
    directory = tmp_path / "heap_profiler_probe"
    directory.mkdir()
    source = directory / "probe.c"
    library = directory / "probe.so"
    source.write_text("""
        #include <stdlib.h>
        #include <errno.h>
        #include <stdint.h>
        #include <pthread.h>
        void *allocate(size_t n) { return malloc(n); }
        void release(void *p) { free(p); }
        void *resize(void *p, size_t n) { return realloc(p, n); }
        int free_errno(void *p) { errno = EDOM; free(p); return errno; }
        void churn(size_t n) {
            for (size_t i = 0; i < n; ++i) {
                void *p = malloc(4096 + (i % 64));
                if (p) { ((volatile char *)p)[0] = 1; free(p); }
            }
        }
        void *worker(void *iterations) { churn((size_t)iterations); return NULL; }
        int native_stress(size_t iterations) {
            pthread_t threads[16];
            int started = 0;
            for (; started < 16; ++started)
                if (pthread_create(&threads[started], NULL, worker, (void *)iterations)) break;
            for (int i = 0; i < started; ++i) pthread_join(threads[i], NULL);
            return started == 16;
        }
    """)
    flags = ["-shared", "-fPIC", "-fno-builtin", "-pthread"]
    if sys.platform == "linux":
        flags += ["-Wl,-z,relro,-z,now"]
    subprocess.run([compiler, *flags, str(source), "-o", str(library)], check=True, capture_output=True)  # noqa: S603
    lib = ctypes.CDLL(str(library), mode=os.RTLD_NOW)
    lib.allocate.argtypes = [ctypes.c_size_t]
    lib.allocate.restype = ctypes.c_void_p
    lib.release.argtypes = [ctypes.c_void_p]
    lib.resize.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
    lib.resize.restype = ctypes.c_void_p
    lib.free_errno.argtypes = [ctypes.c_void_p]
    lib.free_errno.restype = ctypes.c_int
    lib.churn.argtypes = [ctypes.c_size_t]
    # Warm resolved slots (Darwin lazy binding) before scanning.
    lib.release(lib.allocate(8))
    lib.release(lib.resize(None, 8))
    h.start_tracing(1)
    yield lib, library
    h.stop_tracing()


def detached_profile():
    profile = Profile.FromString(h.get_pprof_data())
    locations = {loc.id: loc for loc in profile.location}
    functions = {func.id: func for func in profile.function}
    values = [0, 0, 0, 0]
    for sample in profile.sample:
        names = [
            profile.string_table[functions[line.function_id].filename]
            for loc_id in sample.location_id
            for line in locations[loc_id].line
        ]
        if "<detached thread>" in names:
            values = [a + b for a, b in zip(values, sample.value, strict=True)]
    return values


def test_native_allocation_free_and_errno(native):
    import errno

    lib, _ = native
    h.reset_profile_data()
    pointer = lib.allocate(123456)
    assert pointer
    before = detached_profile()
    assert before[1] == 123456, [
        (e.alloc_bytes, e.stack_frames) for e in h.get_profile_data() if e.alloc_bytes >= 123456
    ]
    assert lib.free_errno(pointer) == errno.EDOM
    after = detached_profile()
    assert after[2:] == [0, 0]


@pytest.mark.skipif(sys.platform != "linux", reason="glibc realloc(p, 0) frees p and returns NULL")
def test_native_realloc_zero_is_a_free(native):
    lib, _ = native
    h.reset_profile_data()
    pointer = lib.allocate(123456)
    assert pointer
    assert lib.resize(pointer, 0) is None
    assert detached_profile()[2:] == [0, 0]


def test_mixed_sizes_and_sample_rates_keep_matched_weights(native):
    lib, _ = native
    h.start_tracing(128)
    h.reset_profile_data()
    pointers = [lib.allocate(n) for n in [16, 64, 256, 4096, 1000000] * 100]
    before = detached_profile()
    h.start_tracing(2147483647)
    for ptr in pointers:
        lib.release(ptr)
    after = detached_profile()
    assert after[:2] == before[:2], "changing the rate must not rescale existing allocations"
    assert after[2:] == [0, 0], "each free must subtract its allocation's original weight"


@pytest.mark.skipif(sys.platform != "linux", reason="Linux procfs permissions")
def test_native_patching_preserves_relro(native):
    _, path = native

    def mappings():
        with open("/proc/self/maps") as stream:
            return [line.split()[1] for line in stream if str(path) in line]

    initial = mappings()
    assert "r--p" in initial
    h.refresh_native_hooks()
    assert mappings() == initial
    h.stop_tracing()
    assert mappings() == initial


def test_native_threads_during_reset_and_snapshots(native):
    lib, _ = native
    h.start_tracing(64)
    threads = [threading.Thread(target=lib.churn, args=(10000,)) for _ in range(4)]
    for thread in threads:
        thread.start()
    for _ in range(100):
        h.reset_profile_data()
        assert all(e.free_bytes <= e.alloc_bytes for e in h.get_profile_data())
        h.get_pprof_data()
    for thread in threads:
        thread.join(timeout=15)
        assert not thread.is_alive()


def test_native_threads_never_wait_for_the_gil(native):
    _, path = native
    # PyDLL keeps the calling thread's GIL while C waits for native pthreads.
    # Acquiring the GIL from a malloc callback would deadlock this test.
    lib = ctypes.PyDLL(str(path))
    lib.native_stress.argtypes = [ctypes.c_size_t]
    lib.native_stress.restype = ctypes.c_int
    h.refresh_native_hooks()
    assert lib.native_stress(1000) == 1
    assert any(f[0] == "<native thread>" for e in h.get_profile_data() for f in e.stack_frames)


def test_refresh_covers_libraries_loaded_after_start(native, tmp_path):
    _, path = native
    late_path = tmp_path / "late.so"
    shutil.copyfile(path, late_path)
    late = ctypes.CDLL(str(late_path), mode=os.RTLD_NOW)
    late.allocate.argtypes = [ctypes.c_size_t]
    late.allocate.restype = ctypes.c_void_p
    late.release.argtypes = [ctypes.c_void_p]
    late.release(late.allocate(8))
    h.refresh_native_hooks()
    h.reset_profile_data()
    pointer = late.allocate(234567)
    assert pointer
    assert detached_profile()[1] == 234567
    late.release(pointer)
    assert detached_profile()[2:] == [0, 0]


def test_subinterpreter_history_does_not_fake_gil_ownership(native):
    try:
        import _interpreters as interpreters  # ty: ignore[import-not-found]
    except ImportError:
        import _xxsubinterpreters as interpreters  # ty: ignore[import-not-found]
    interpreter = interpreters.create()
    interpreters.destroy(interpreter)
    lib, _ = native
    h.reset_profile_data()
    pointer = lib.allocate(123456)
    assert pointer
    assert detached_profile()[1] == 123456
    lib.release(pointer)


@pytest.mark.skipif(sys.platform != "linux", reason="glibc caller-relative RUNPATH")
def test_dlopen_preserves_callers_runpath(native, tmp_path):
    compiler = shutil.which("cc")
    if not compiler:
        pytest.skip("C compiler required")
    child = tmp_path / "child.c"
    child.write_text("int answer(void) { return 42; }")
    caller = tmp_path / "caller.c"
    caller.write_text("""
        #include <dlfcn.h>
        int load_child(void) {
            void *handle = dlopen("child.so", RTLD_NOW);
            if (!handle) return -1;
            int (*answer)(void) = dlsym(handle, "answer");
            int result = answer ? answer() : -2;
            dlclose(handle);
            return result;
        }
    """)
    subprocess.run([compiler, "-shared", "-fPIC", str(child), "-o", str(tmp_path / "child.so")], check=True)  # noqa: S603
    library = tmp_path / "caller.so"
    subprocess.run(  # noqa: S603
        [compiler, "-shared", "-fPIC", str(caller), "-o", str(library), "-Wl,-rpath,$ORIGIN", "-Wl,-z,now", "-ldl"],
        check=True,
    )
    lib = ctypes.CDLL(str(library))
    h.refresh_native_hooks()
    assert lib.load_child() == 42


def test_native_realloc_failure_preserves_live_allocation(native):
    lib, _ = native
    h.reset_profile_data()
    pointer = lib.allocate(123456)
    assert pointer
    assert lib.resize(pointer, ctypes.c_size_t(-1).value) is None
    assert detached_profile()[2:] == [1, 123456]
    lib.release(pointer)
    assert detached_profile()[2:] == [0, 0]


def test_pprof_uses_function_definition_line(native):
    def allocate_on_two_lines():
        first = bytearray(123456)
        second = bytearray(234567)
        return first, second

    h.reset_profile_data()
    blocks = allocate_on_two_lines()
    profile = Profile.FromString(h.get_pprof_data())
    functions = [f for f in profile.function if profile.string_table[f.name] == "allocate_on_two_lines"]
    assert len(functions) == 1
    assert functions[0].start_line == allocate_on_two_lines.__code__.co_firstlineno
    assert blocks


def test_fork_while_native_threads_hold_profiler_locks(native):
    import signal
    import time

    lib, _ = native
    h.start_tracing(64)
    stop = threading.Event()

    def churn():
        while not stop.is_set():
            lib.churn(10000)

    threads = [threading.Thread(target=churn, daemon=True) for _ in range(4)]
    for thread in threads:
        thread.start()
    try:
        for _ in range(10):
            pid = os.fork()
            if pid == 0:
                try:
                    h.reset_profile_data()
                    lib.churn(1000)
                    h.get_pprof_data()
                    os._exit(0)
                except BaseException:
                    os._exit(1)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                finished, status = os.waitpid(pid, os.WNOHANG)
                if finished:
                    assert os.waitstatus_to_exitcode(status) == 0
                    break
                time.sleep(0.01)
            else:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
                pytest.fail("forked child deadlocked during native allocation")
    finally:
        stop.set()
        for thread in threads:
            thread.join(timeout=5)
            assert not thread.is_alive()


def test_native_patching_skips_only_this_extension():
    result = subprocess.run(
        [sys.executable, "-c", "import heap_profiler as h; h.start_tracing(); h.stop_tracing()"],
        capture_output=True,
        text=True,
        timeout=30,
        env={**os.environ, "HEAP_PROFILER_LOG": "heap_profiler=debug"},
    )
    assert result.returncode == 0, result.stderr
    prefix = "Skipping this extension: "  # SKIPPING_THIS_EXTENSION in src/platform/mod.rs
    skipped = {
        message.removeprefix(prefix)
        for line in result.stderr.splitlines()
        if line.startswith("{")
        for message in [json.loads(line).get("fields", {}).get("message", "")]
        if message.startswith(prefix)
    }
    # Applying and restoring hooks both skip the extension, and nothing else.
    extension = importlib.util.find_spec("heap_profiler.heap_profiler")
    assert extension and extension.origin
    assert {os.path.realpath(path) for path in skipped} == {os.path.realpath(extension.origin)}
