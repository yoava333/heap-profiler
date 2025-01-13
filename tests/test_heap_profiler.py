import gc

import heap_profiler


def test_basic_allocation():
    # Enable the heap profiler
    heap_profiler.set_allocator()

    # Force a GC to clean up any existing allocations
    gc.collect()

    # Create some allocations
    data = []
    for i in range(10):
        # Create a large allocation that will definitely be sampled
        data.append(bytearray(1024 * 1024))  # 1MB each

    # Force objects out of scope
    data = None
    gc.collect()


if __name__ == "__main__":
    test_basic_allocation()
