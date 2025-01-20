import pyperformance.cli
from heap_profiler import get_pprof_data, start_tracing

if __name__ == "__main__":
    start_tracing()
    pyperformance.cli.main()
    data = get_pprof_data()
    with open("heap.prof", "wb") as f:
        f.write(data)
