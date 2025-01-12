from heap_profiler import set_allocator


def setup_allocator():
    set_allocator()


def allocate():
    x = bytes(b"\x01" * 1024 * 1024)
    print(len(x))


setup_allocator()
allocate()
