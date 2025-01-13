import pyperformance.cli
from heap_profiler import set_allocator, write_heap_proto

if __name__ == "__main__":
    set_allocator()
    pyperformance.cli.main()
    data = write_heap_proto()
    with open("heap.prof", "wb") as f:
        f.write(data)
