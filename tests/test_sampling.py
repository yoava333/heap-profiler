"""Validate zero-size request counts against controlled native and RAW calls."""

import ctypes
import math

import pytest
from test_native import detached_profile
from test_review2 import compile_library

import heap_profiler as h

pytestmark = [pytest.mark.forked, pytest.mark.no_tracing]


@pytest.mark.parametrize("raw", [False, True], ids=["native", "raw"])
@pytest.mark.parametrize("rate", [1, 2, 128])
def test_zero_size_requests_are_sampled_and_freed(tmp_path, raw, rate):
    library = compile_library(tmp_path, r"""
        #include <stddef.h>
        #include <stdlib.h>
        extern void *PyMem_RawMalloc(size_t);
        extern void PyMem_RawFree(void *);
        static void *blocks[100000];
        static size_t used;
        int allocate(int raw) {
            for (; used < 100000; ++used) {
                blocks[used] = raw ? PyMem_RawMalloc(0) : malloc(0);
                if (!blocks[used]) return -1;
            }
            return 0;
        }
        void release(int raw) {
            while (used) {
                void *p = blocks[--used];
                if (raw) PyMem_RawFree(p); else free(p);
            }
        }
    """)
    lib = ctypes.CDLL(str(library))
    lib.allocate.argtypes = [ctypes.c_int]
    lib.allocate.restype = ctypes.c_int
    lib.release.argtypes = [ctypes.c_int]
    lib.release.restype = None
    # Resolve all lazy native bindings before installing interception.
    assert lib.allocate(raw) == 0
    lib.release(raw)
    h.start_tracing(rate)
    h.reset_profile_data()
    try:
        assert lib.allocate(raw) == 0
        sampled = sum(
            entry.alloc_objects for entry in h.get_profile_data()
            if any(frame[0] == "<detached thread>" for frame in entry.stack_frames)
        )
        probability = 1.0 if rate == 1 else -math.expm1(-1.0 / rate)
        expected = 100000 * probability
        # Eight binomial standard deviations avoid a flaky random threshold.
        tolerance = 8 * math.sqrt(expected * (1 - probability))
        assert abs(sampled - expected) <= tolerance
        before = detached_profile()
        assert abs(before[0] - sampled / probability) <= 0.5
        assert before == [before[0], 0, before[0], 0]
        # Frees must retain the allocation's weight despite configuration changes.
        h.start_tracing(128 if rate == 1 else 1)
        lib.release(raw)
        assert detached_profile() == [before[0], 0, 0, 0]
    finally:
        lib.release(raw)
        h.stop_tracing()
