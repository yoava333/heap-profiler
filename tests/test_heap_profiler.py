import gc
import threading
import time

import pytest
from heap_profiler import get_profile_data, set_allocator, write_heap_proto


def setup_module():
    """Initialize the profiler before running tests"""
    set_allocator()


def test_basic_allocation():
    """Test basic allocation and deallocation"""
    # Create a large allocation
    data = bytes(b"\x01" * 1024 * 1024)  # 1MB

    # Get profile data
    profiles = get_profile_data()

    # Find our allocation
    found = False
    for alloc_bytes, free_bytes, alloc_objects, free_objects, stack in profiles:
        if alloc_bytes >= 1024 * 1024:  # Look for our 1MB allocation
            found = True
            assert alloc_objects > 0
            assert free_bytes == 0  # Not freed yet
            assert free_objects == 0
            # Check stack trace points to this file
            assert any("test_heap_profiler.py" in frame[0] for frame in stack)

    assert found, "Large allocation not found in profile data"

    # Delete the data and force GC
    del data
    gc.collect()

    # Check that it was freed
    profiles = get_profile_data()
    found = False
    for alloc_bytes, free_bytes, alloc_objects, free_objects, stack in profiles:
        if alloc_bytes >= 1024 * 1024:
            found = True
            assert free_bytes >= alloc_bytes  # Should be fully freed
            assert free_objects >= alloc_objects

    assert found, "Freed allocation not found in profile data"


def test_multiple_allocations():
    """Test multiple allocations of different sizes"""
    # Create allocations of increasing sizes
    data = []
    for i in range(5):
        size = (i + 1) * 1024 * 1024  # 1MB to 5MB
        data.append(bytes(b"\x01" * size))

    # Get profile data
    profiles = get_profile_data()

    # Find our allocations
    total_found = 0
    for alloc_bytes, free_bytes, alloc_objects, free_objects, stack in profiles:
        if alloc_bytes >= 1024 * 1024:  # Look for MB-sized allocations
            total_found += 1
            assert alloc_objects > 0
            assert any("test_heap_profiler.py" in frame[0] for frame in stack)

    assert total_found > 0, "No large allocations found in profile data"

    # Clean up
    del data
    gc.collect()


def test_reallocation():
    """Test reallocation behavior"""
    # Create a list with controlled growth
    data = []
    initial_size = 1024 * 1024  # 1MB initial size

    # First allocation
    data = bytearray(initial_size)

    # Force a reallocation by extending
    data.extend(bytearray(initial_size))  # Double to 2MB

    # Get profile data
    profiles = get_profile_data()

    # We should see some allocations from this file
    found = False
    total_bytes = 0
    for alloc_bytes, free_bytes, alloc_objects, free_objects, stack in profiles:
        if any("test_heap_profiler.py" in frame[0] for frame in stack):
            found = True
            total_bytes += alloc_bytes

    assert found, "No allocations from test file found"
    assert total_bytes >= initial_size, "Expected at least initial size in allocations"

    # Clean up
    del data
    gc.collect()


def worker_function(results, thread_id):
    """Worker function for threading test"""
    # Allocate some memory
    data = []
    for i in range(3):
        size = (thread_id + 1) * 1024 * 1024  # Different size per thread
        data.append(bytes(b"\x01" * size))

    # Get profile data
    profiles = get_profile_data()

    # Find our allocations
    found = False
    for alloc_bytes, free_bytes, alloc_objects, free_objects, stack in profiles:
        if alloc_bytes >= 1024 * 1024 and any(
            "test_heap_profiler.py" in frame[0] for frame in stack
        ):
            found = True
            results[thread_id] = True
            break

    # Hold the memory for a bit
    time.sleep(0.1)

    # Clean up
    del data
    gc.collect()


def test_threaded_allocations():
    """Test allocations from multiple threads"""
    num_threads = 4
    threads = []
    results = [False] * num_threads

    # Create threads
    for i in range(num_threads):
        thread = threading.Thread(target=worker_function, args=(results, i))
        threads.append(thread)
        thread.start()

    # Wait for all threads to complete
    for thread in threads:
        thread.join()

    # Verify all threads recorded allocations
    assert all(results), "Not all threads recorded allocations"

    # Get final profile data
    profiles = get_profile_data()

    # Verify we see allocations and frees
    found_allocs = 0
    for alloc_bytes, free_bytes, alloc_objects, free_objects, stack in profiles:
        if alloc_bytes >= 1024 * 1024 and any(
            "test_heap_profiler.py" in frame[0] for frame in stack
        ):
            found_allocs += 1
            assert free_bytes > 0, "Memory not freed after thread completion"
            assert free_objects > 0

    assert found_allocs > 0, "No thread allocations found in profile data"


def test_small_allocations():
    """Test that small allocations are sampled at a lower rate"""
    # Make many small allocations
    data = []
    for _ in range(1000):
        data.append(bytes(b"\x01" * 100))  # 100 bytes each

    # Get profile data
    profiles = get_profile_data()

    # Count small allocations from this file
    small_allocs = 0
    for alloc_bytes, _, _, _, stack in profiles:
        if alloc_bytes <= 100 and any(
            "test_heap_profiler.py" in frame[0] for frame in stack
        ):
            small_allocs += 1

    # We should see some small allocations, but not all 1000
    assert small_allocs < 1000, "Small allocations not being sampled properly"

    # Clean up
    del data
    gc.collect()


def test_large_allocation_sampling():
    """Test that large allocations are always sampled"""
    # Make several large allocations
    data = []
    num_allocs = 5
    for _ in range(num_allocs):
        data.append(bytes(b"\x01" * (1024 * 1024)))  # 1MB each

    # Get profile data
    profiles = get_profile_data()

    # Count large allocations from this file
    large_allocs = 0
    for alloc_bytes, _, _, _, stack in profiles:
        if alloc_bytes >= 1024 * 1024 and any(
            "test_heap_profiler.py" in frame[0] for frame in stack
        ):
            large_allocs += 1

    # We should see all large allocations
    assert large_allocs >= num_allocs, "Not all large allocations were sampled"

    # Clean up
    del data
    gc.collect()


def test_heap_proto_export():
    """Test heap profile protobuf export"""
    # Create some allocations
    data = []
    for i in range(3):
        size = (i + 1) * 1024 * 1024  # 1MB to 3MB
        data.append(bytes(b"\x01" * size))

    # Get the protobuf data
    proto_data = write_heap_proto()

    # Basic validation of protobuf output
    assert isinstance(proto_data, bytes)
    assert len(proto_data) > 0

    # The protobuf should contain our stack trace
    stack_trace = b"test_heap_profiler.py"
    assert stack_trace in proto_data

    # Clean up
    del data
    gc.collect()


if __name__ == "__main__":
    pytest.main([__file__])
