# heap-profiler

A high-performance Python memory profiler that helps you understand memory usage patterns in your Python applications. This profiler uses Rust for low-level memory tracking and provides Python bindings through PyO3.

## Features

- **Low Overhead**: Uses statistical sampling for minimal performance impact
- **Detailed Memory Tracking**: Captures allocation and deallocation events with stack traces
- **Python Integration**: Seamlessly integrates with Python applications
- **Rich Profile Data**: Provides detailed memory usage statistics per stack trace
- **pprof Compatible**: Outputs data in pprof format for visualization

## TL;DR

```bash
pip install heap-profiler
```

```python
import heap_profiler
heap_profiler.start_tracing()

# your code here
# ...

with open("heap.pb.gz", "wb") as f:
    f.write(heap_profiler.get_pprof_data(compress=True))
```

See [examples/pprof_server.py](examples/pprof_server.py) for a simple pprof server.

## Why another memory profiler?

Our inspriation was the pprof module from golang, which provides low overhead production memory tracing.
There are a few projects that provide memory tracing for python, but they didn't fit our needs.

Not suiteable for production (performance overhead):

- memory_profiler
- tracemalloc
- memray
- guppy3
- filprofiler
- pympler

Not maintained:

- mprofile

Hard to use:

- scalene

## Installation

```bash
pip install heap-profiler
```

## Usage

```python
import heap_profiler

# Start memory profiling
heap_profiler.start_tracing()

# Your code here
# ...

# Get profile data as a list of ProfileEntry objects
profile_data = heap_profiler.get_profile_data()

# Or get data in pprof format for visualization
pprof_data = heap_profiler.get_pprof_data()
```

Each `ProfileEntry` contains:

- `alloc_bytes`: Total bytes allocated
- `free_bytes`: Total bytes freed
- `alloc_objects`: Number of allocations
- `free_objects`: Number of deallocations
- `stack_frames`: List of (filename, line_number) tuples representing the stack trace

## How It Works

The profiler works by intercepting memory allocation functions at multiple levels:

1. Python's memory allocator interface
2. Native malloc/free calls
3. Dynamic library loading (dlopen)

It uses statistical sampling for small allocations (< 256KB) and tracks all large allocations to maintain accuracy while minimizing overhead. Stack traces are captured for each sampled allocation to provide context about where memory is being used.

## Development

### Prerequisites

- Rust toolchain (latest stable)
- Python 3.10+
- Development packages for Python
- Cargo and maturin for building

### Building from Source

1. Clone the repository:

```bash
# install maturin
uv tool install maturin

# install rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# clone the repository
git clone https://github.com/yoava333/heap-profiler.git
cd heap-profiler
```

2. For development installation:

```bash
maturin develop --uv
```

3. For testing:

```bash
cargo test
uv run pytest tests/
```

### Project Structure

- `src/lib.rs`: Main library entry point and Python module definition
- `src/allocator.rs`: Core memory tracking implementation
- `src/profile.rs`: Profile data collection and management
- `src/stack.rs`: Stack trace capture and processing
- `src/platform/`: Platform-specific memory interception code

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

[Insert your chosen license here]

## Acknowledgments

This project uses:

- [PyO3](https://github.com/PyO3/pyo3) for Rust-Python bindings
- [pprof](https://github.com/google/pprof) format for profile data visualization
