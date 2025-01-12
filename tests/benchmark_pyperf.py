import pyperformance.cli
from heap_profiler import set_allocator

if __name__ == "__main__":
    set_allocator()
    pyperformance.cli.main()
