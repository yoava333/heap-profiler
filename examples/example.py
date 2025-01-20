from heap_profiler import get_pprof_data, get_profile_data, start_tracing

start_tracing()
import ssl
import sys

import numpy as np


def setup_allocator():
    start_tracing()


def np_alloc():
    np.ones((1024, 1024, 100))


def allocate_and_free():
    # Allocate multiple objects of different sizes
    objects = []
    for i in range(10):
        size = 1024 * 1024 * (i + 1)  # 1MB to 10MB
        x = bytes(b"\x01" * size)
        objects.append(x)

    # Print profile data after allocations
    print("\nProfile data after allocations:")
    for profile_entry in get_profile_data():
        print("\nAllocation point:")
        print(
            f"Allocated: {profile_entry.alloc_bytes} bytes ({profile_entry.alloc_objects} objects)"
        )
        print(
            f"Freed: {profile_entry.free_bytes} bytes ({profile_entry.free_objects} objects)"
        )
        print("Stack trace:")
        for filename, function, lineno in profile_entry.stack_frames:
            print(f"  {filename}:{lineno}:{function}")

    # Write heap profile to file
    profile_data = get_pprof_data()
    with open("heap.prof", "wb") as f:
        f.write(profile_data)

    # Free half the objects
    objects = objects[5:]

    # Force garbage collection to ensure objects are freed
    import gc

    gc.collect()

    # Print profile data after frees
    print("\nProfile data after frees:")
    for profile_entry in get_profile_data():
        print("\nAllocation point:")
        print(
            f"Allocated: {profile_entry.alloc_bytes} bytes ({profile_entry.alloc_objects} objects)"
        )
        print(
            f"Freed: {profile_entry.free_bytes} bytes ({profile_entry.free_objects} objects)"
        )
        print("Stack trace:")
        for filename, function, lineno in profile_entry.stack_frames:
            print(f"  {filename}:{lineno}:{function}")

    # Write updated heap profile
    profile_data = get_pprof_data(compress=True)
    with open("heap_after_free.prof", "wb") as f:
        f.write(profile_data)

    np_alloc()

    gc.collect()

    profile_data = get_pprof_data(compress=True)
    with open("heap_after_np_alloc.prof", "wb") as f:
        f.write(profile_data)


if __name__ == "__main__":
    len(sys.argv)
    ssl.create_default_context()
    setup_allocator()
    allocate_and_free()
    allocate_and_free()  # Run twice to test multiple allocations
