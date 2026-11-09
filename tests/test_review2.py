"""Independent second-review reproductions for thread teardown and fork."""

import shutil
import subprocess
import sys

import pytest
from test_regressions import run_code

pytestmark = pytest.mark.no_tracing


def compile_library(tmp_path, source, *, cpp=False):
    compiler = shutil.which("c++" if cpp else "cc")
    if not compiler:
        pytest.skip("C/C++ compiler required")  # ty: ignore[too-many-positional-arguments]
    path = tmp_path / ("probe.cpp" if cpp else "probe.c")
    library = tmp_path / "probe.so"
    path.write_text(source)
    flags = ["-shared", "-fPIC", "-fno-builtin", "-pthread"]
    if cpp:
        flags += ["-std=c++11"]
    elif sys.platform == "darwin":
        flags += ["-undefined", "dynamic_lookup"]
    subprocess.run([compiler, *flags, str(path), "-o", str(library)], check=True, capture_output=True)  # noqa: S603
    return library


def test_allocations_from_cpp_thread_local_destructors(tmp_path):
    library = compile_library(
        tmp_path,
        r"""
        #include <cstdlib>
        #include <pthread.h>
        struct Probe {
            ~Probe() {
                for (int i = 0; i < 8; ++i) {
                    void *p = malloc(1 << 20);
                    if (p) { ((volatile char *)p)[0] = 1; free(p); }
                }
            }
        };
        static thread_local Probe probe;
        static void *worker(void *) {
            (void)&probe;  // Register C++ destruction before the sampler's TLS.
            void *p = malloc(1 << 20);
            if (p) { ((volatile char *)p)[0] = 1; free(p); }
            return nullptr;
        }
        extern "C" int run() {
            pthread_t thread;
            if (pthread_create(&thread, nullptr, worker, nullptr)) return 1;
            return pthread_join(thread, nullptr);
        }
        """,
        cpp=True,
    )
    run_code(f"""
        import ctypes
        import heap_profiler as h
        lib = ctypes.CDLL({str(library)!r})
        assert lib.run() == 0
        h.start_tracing(1024)
        for _ in range(10): assert lib.run() == 0
        h.stop_tracing()
    """)


@pytest.mark.parametrize("operation", [1, 2], ids=["realloc", "free"])
def test_fork_drains_tracked_raw_allocator_operations(tmp_path, operation):
    library = compile_library(
        tmp_path,
        r"""
        #include <stddef.h>
        #include <stdatomic.h>
        #include <pthread.h>
        #include <sched.h>
        #include <time.h>
        typedef struct {
            void *ctx;
            void *(*malloc)(void *, size_t);
            void *(*calloc)(void *, size_t, size_t);
            void *(*realloc)(void *, void *, size_t);
            void (*free)(void *, void *);
        } Alloc;
        extern void PyMem_GetAllocator(int, Alloc *);
        extern void PyMem_SetAllocator(int, Alloc *);
        extern void *PyMem_RawMalloc(size_t);
        extern void *PyMem_RawRealloc(void *, size_t);
        extern void PyMem_RawFree(void *);
        static Alloc saved;
        static _Atomic(void *) target;
        static _Atomic int operation, entered, proceed, finished;
        static pthread_t unblocker;
        static void pause_operation(int kind, void *pointer) {
            if (atomic_load(&operation) == kind && pointer == atomic_load(&target)) {
                atomic_store(&entered, 1);
                while (!atomic_load(&proceed)) sched_yield();
            }
        }
        static void *allocate(void *ctx, size_t n) { return saved.malloc(saved.ctx, n); }
        static void *zero(void *ctx, size_t n, size_t s) { return saved.calloc(saved.ctx, n, s); }
        static void *resize(void *ctx, void *p, size_t n) {
            if (p == atomic_load(&target) && atomic_load(&operation) == 1) {
                pause_operation(1, p);
                // An equal-size realloc may legally retain the existing block.
                atomic_store(&finished, 1);
                return p;
            }
            return saved.realloc(saved.ctx, p, n);
        }
        static void release(void *ctx, void *p) {
            int selected = p == atomic_load(&target) && atomic_load(&operation) == 2;
            if (selected) pause_operation(2, p);
            saved.free(saved.ctx, p);
            if (selected) atomic_store(&finished, 1);
        }
        void install(void) {
            PyMem_GetAllocator(0, &saved);
            Alloc wrapped = {NULL, allocate, zero, resize, release};
            PyMem_SetAllocator(0, &wrapped);
        }
        void restore(void) { PyMem_SetAllocator(0, &saved); }
        void *make_pointer(size_t n) { return PyMem_RawMalloc(n); }
        void free_pointer(void *p) { PyMem_RawFree(p); }
        void *resize_pointer(void *p, size_t n) { return PyMem_RawRealloc(p, n); }
        void select_operation(void *p, int kind) { atomic_store(&target, p); atomic_store(&operation, kind); }
        int is_entered(void) { return atomic_load(&entered); }
        int is_finished(void) { return atomic_load(&finished); }
        static void *unblock(void *unused) {
            while (!atomic_load(&entered)) sched_yield();
            struct timespec delay = {0, 100000000};
            nanosleep(&delay, NULL);
            atomic_store(&proceed, 1);
            return NULL;
        }
        int schedule_unblock(void) { return pthread_create(&unblocker, NULL, unblock, NULL); }
        int join_unblocker(void) { return pthread_join(unblocker, NULL); }
        """,
    )
    run_code(f"""
        import ctypes, json, os, threading, time
        import heap_profiler as h
        lib = ctypes.CDLL({str(library)!r})
        gil = ctypes.PyDLL({str(library)!r})
        lib.make_pointer.argtypes = [ctypes.c_size_t]
        lib.make_pointer.restype = ctypes.c_void_p
        lib.free_pointer.argtypes = [ctypes.c_void_p]
        lib.resize_pointer.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
        lib.resize_pointer.restype = ctypes.c_void_p
        lib.select_operation.argtypes = [ctypes.c_void_p, ctypes.c_int]
        gil.install()
        h.start_tracing(1, native=False)
        h.reset_profile_data()
        pointer = lib.make_pointer(123456)
        assert pointer
        lib.select_operation(pointer, {operation})
        def work():
            if {operation} == 1:
                assert lib.resize_pointer(pointer, 123456) == pointer
            else:
                lib.free_pointer(pointer)
        worker = threading.Thread(target=work)
        assert lib.schedule_unblock() == 0
        worker.start()
        deadline = time.monotonic() + 5
        while not lib.is_entered():
            assert time.monotonic() < deadline
            time.sleep(0.001)
        pid = os.fork()
        if pid == 0:
            try:
                assert lib.is_finished(), "fork crossed an incomplete tracked allocator operation"
                if {operation} == 1: lib.free_pointer(pointer)
                records = [e for e in h.get_profile_data()
                           if any(f[0] == "<detached thread>" for f in e.stack_frames)]
                assert records
                assert all(e.alloc_bytes == e.free_bytes for e in records)
                assert all(e.alloc_objects == e.free_objects for e in records)
                os._exit(0)
            except BaseException:
                import traceback
                traceback.print_exc()
                os._exit(1)
        _, status = os.waitpid(pid, 0)
        worker.join(timeout=5)
        assert not worker.is_alive()
        assert lib.join_unblocker() == 0
        if {operation} == 1: lib.free_pointer(pointer)
        h.stop_tracing()
        gil.restore()
        assert os.waitstatus_to_exitcode(status) == 0
    """)


@pytest.mark.parametrize("outer_wrapper", [False, True], ids=["underlying", "outer"])
def test_start_stop_while_native_threads_use_raw_allocator(tmp_path, outer_wrapper):
    library = compile_library(
        tmp_path,
        r"""
#include <stddef.h>
#include <stdatomic.h>
#include <pthread.h>
#include <unistd.h>
#include <stdlib.h>
typedef struct { void *ctx; void *(*malloc)(void *,size_t); void *(*calloc)(void *,size_t,size_t); void *(*realloc)(void *,void *,size_t); void (*free)(void *,void *); } Alloc;
extern void PyMem_GetAllocator(int, Alloc *);
extern void PyMem_SetAllocator(int, Alloc *);
extern void *PyMem_RawMalloc(size_t);
extern void PyMem_RawFree(void *);
static Alloc saved;
static size_t sentinel[8]={0x12345678};
static _Atomic int stop;
static pthread_t workers[4];
static void check(void *ctx) { if(ctx != sentinel) {write(2,"RAW CONTEXT MISMATCH\n",21); _exit(90);} }
static void *m(void *ctx,size_t n) {check(ctx);return saved.malloc(saved.ctx,n);}
static void *c(void *ctx,size_t n,size_t s) {check(ctx);return saved.calloc(saved.ctx,n,s);}
static void *r(void *ctx,void *p,size_t n) {check(ctx);return saved.realloc(saved.ctx,p,n);}
static void f(void *ctx,void *p) {check(ctx);saved.free(saved.ctx,p);}
void install(void) {PyMem_GetAllocator(0,&saved);Alloc wrapped={sentinel,m,c,r,f};PyMem_SetAllocator(0,&wrapped);}
void restore(void) {PyMem_SetAllocator(0,&saved);}
static void *work(void *ignored) {while(!atomic_load(&stop)) {void *p=PyMem_RawMalloc(65);PyMem_RawFree(p);}return NULL;}
void start(void) {for(int i=0;i<4;i++) if(pthread_create(workers+i,NULL,work,NULL)) abort();}
void finish(void) {atomic_store(&stop,1);for(int i=0;i<4;i++) pthread_join(workers[i],NULL);}
    """,
    )
    run_code(f"""
        import ctypes
        import heap_profiler as h
        gil = ctypes.PyDLL({str(library)!r})
        lib = ctypes.CDLL({str(library)!r})
        if {outer_wrapper!r}: h.start_tracing(2147483647, native=False)
        gil.install()
        if {outer_wrapper!r}: h.stop_tracing()
        lib.start()
        for _ in range(2000):
            h.start_tracing(2147483647, native=False)
            h.stop_tracing()
        lib.finish()
        gil.restore()
    """)


def test_fork_from_cpp_thread_local_destructor(tmp_path):
    library = compile_library(
        tmp_path,
        r"""
        #include <pthread.h>
        #include <sys/wait.h>
        #include <unistd.h>
        #include <cstdlib>
        static void fork_once() {
            pid_t child = fork();
            if (child == 0) _exit(0);
            if (child < 0) std::abort();
            int status;
            if (waitpid(child, &status, 0) != child || status != 0) std::abort();
        }
        struct Probe { ~Probe() { fork_once(); } };
        static thread_local Probe probe;
        static void *worker(void *) {
            (void)&probe;
            fork_once();  // Initializes the old drop-managed fork TLS after Probe.
            return nullptr;
        }
        extern "C" int run() {
            pthread_t thread;
            if (pthread_create(&thread, nullptr, worker, nullptr)) return 1;
            return pthread_join(thread, nullptr);
        }
    """,
        cpp=True,
    )
    run_code(f"""
        import ctypes
        import heap_profiler as h
        lib = ctypes.CDLL({str(library)!r})
        h.start_tracing(1024, native=False)
        assert lib.run() == 0
        h.stop_tracing()
        # Atfork handlers remain registered after stopping.
        assert lib.run() == 0
    """)
