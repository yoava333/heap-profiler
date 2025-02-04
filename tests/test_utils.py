import ctypes
import gc

import psutil


def force_malloc_trim():
    """Attempt to force libc to return unused memory to the OS."""
    try:
        libc = ctypes.CDLL("libc.so.6")
        libc.malloc_trim(0)
    except OSError:
        pass


def a():
    b()


def b():
    c()


def c():
    d()


def d():
    e()


def e():
    f()


def f():
    g()


def g():
    h()


def h():
    objs = list(range(1000))
    del objs


def memory_stress():
    """Stress test to track the profiler memory usage."""
    process = psutil.Process()
    initial_memory = None
    warm_up_iterations = 2000

    for i in range(10_000):
        if i > warm_up_iterations and i % warm_up_iterations == 0:
            if initial_memory is None:
                initial_memory = process.memory_info().rss
            print(f"Memory usage: {process.memory_info().rss / 1024 / 1024}MiB")
        for _ in range(1):
            a()

    gc.collect()  # Ensure a clean slate for memory tracking
    force_malloc_trim()
    final_memory = process.memory_info().rss
    return initial_memory, final_memory
