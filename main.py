import numpy as np
from heap_profiler import get_profile_data, set_allocator, write_heap_proto


def setup_allocator():
    set_allocator()


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
    for (
        alloc_bytes,
        free_bytes,
        alloc_objects,
        free_objects,
        stack,
    ) in get_profile_data():
        print("\nAllocation point:")
        print(f"Allocated: {alloc_bytes} bytes ({alloc_objects} objects)")
        print(f"Freed: {free_bytes} bytes ({free_objects} objects)")
        print("Stack trace:")
        for filename, lineno in stack:
            print(f"  {filename}:{lineno}")

    # Write heap profile to file
    profile_data = write_heap_proto()
    with open("heap.prof", "wb") as f:
        f.write(profile_data)

    # Free half the objects
    objects = objects[5:]

    # Force garbage collection to ensure objects are freed
    import gc

    gc.collect()

    # Print profile data after frees
    print("\nProfile data after frees:")
    for (
        alloc_bytes,
        free_bytes,
        alloc_objects,
        free_objects,
        stack,
    ) in get_profile_data():
        print("\nAllocation point:")
        print(f"Allocated: {alloc_bytes} bytes ({alloc_objects} objects)")
        print(f"Freed: {free_bytes} bytes ({free_objects} objects)")
        print("Stack trace:")
        for filename, lineno in stack:
            print(f"  {filename}:{lineno}")

    # Write updated heap profile
    profile_data = write_heap_proto()
    with open("heap_after_free.prof", "wb") as f:
        f.write(profile_data)

    np_alloc()

    gc.collect()

    profile_data = write_heap_proto()
    with open("heap_after_np_alloc.prof", "wb") as f:
        f.write(profile_data)


if __name__ == "__main__":
    setup_allocator()
    allocate_and_free()
    allocate_and_free()  # Run twice to test multiple allocations
