from heap_profiler import get_profile_data, set_allocator


def setup_allocator():
    set_allocator()


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


if __name__ == "__main__":
    setup_allocator()
    allocate_and_free()
    allocate_and_free()
