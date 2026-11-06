# heap-profiler

A sampling memory profiler for CPython, implemented in Rust with Python bindings through PyO3. It tracks allocator events and exports pprof-compatible heap profiles.

## Features

- **Configurable Sampling**: Tune stack-capture frequency with `sample_rate`
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

## Scope and overhead

The goal is pprof-compatible heap sampling from Python allocator domains and
selected native allocation calls. Overhead depends on allocation rate, stack
depth, sampling interval, and native coverage. The included allocation-heavy
benchmarks can approach twice the unprofiled runtime; C-bound work is much less
affected. Measure a representative workload before choosing the interval, and
increase `sample_rate` when throughput matters more than sampling resolution.

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

`get_profile_data()` returns raw sampled counts; `get_pprof_data()` returns values scaled to estimate the full heap (pprof-style).

To stop profiling or take interval snapshots:

```python
heap_profiler.stop_tracing()        # restore original allocators/hooks
heap_profiler.reset_profile_data()  # clear accumulated data (interval profiling)
```

Set `HEAP_PROFILER_LOG=<filter>` (e.g. `heap_profiler=debug`) to enable the profiler's internal JSON logging to stderr.

Each `ProfileEntry` contains:

- `alloc_bytes`: Total bytes allocated
- `free_bytes`: Total bytes freed
- `alloc_objects`: Number of allocations
- `free_objects`: Number of deallocations
- `stack_frames`: List of (filename, function_name, line_number) tuples representing the stack trace

## How It Works

The profiler works by intercepting memory allocation functions at multiple levels:

1. Python's memory allocator interface
2. Native malloc/free calls
3. Explicit rescans of loaded native libraries

It uses statistical sampling with a default mean interval of 128 KiB. An allocation
of size `s` is sampled with probability approximately `1 - exp(-s / sample_rate)`;
large allocations are more likely to be sampled. `sample_rate=1` records every
allocation. Stack traces contain up to 128 Python frames, with markers for
native or detached threads. Requested sizes are reported, rather than allocator
arena sizes or process RSS. Object counts mean allocator requests, not a census
of live Python objects (which may be served from interpreter free lists).

The pprof export applies each allocation's own sampling weight. Changing the
sampling rate does not retroactively rescale earlier allocations or their frees.
Raw `ProfileEntry` counters remain unscaled.

Import native extensions before starting tracing when practical. After loading
more native code, explicitly refresh the hooks:

```python
import heap_profiler
heap_profiler.start_tracing()
import my_native_extension
heap_profiler.refresh_native_hooks()
```

The profiler leaves `dlopen` unchanged, preserving caller-relative library search
paths. Repeating `start_tracing()` also refreshes hooks. Unresolved lazy symbols,
custom allocator bindings, direct `dlsym` function pointers, and native allocation
APIs other than malloc/calloc/realloc/free/posix_memalign/aligned_alloc are outside
this interception coverage. Python allocator hooks work independently of native
symbol interception.

Supported platforms are 64-bit Linux with glibc and macOS, on x86_64 and ARM64.
On macOS, native interception skips the dyld shared cache and authenticated
pointer sections; chained fixups are not rewritten. Avoid concurrent native
library unloading while starting, stopping, or refreshing hooks on macOS.
Use `start_tracing(native=False)` to track Python allocator domains without
patching native libraries. Stop before changing the `native` option.
Free-threaded CPython builds and importing this module in subinterpreters are
explicitly unsupported. Sampling never attaches a thread or acquires a GIL.

`reset_profile_data()` begins a new interval: allocations from earlier intervals
are forgotten, including live allocations whose frees occur later. Stopping
keeps historical counters but discards pointer metadata; frees while stopped or
after restarting cannot update the previous session's in-use estimates. Reset
before restarting when you need a fresh interval. Forked children inherit the
profile; reset in the child when you want child-only measurements.

Profile history grows with the number of distinct stacks and sampled live
pointers. Reset periodically to release accumulated history and interned strings.
Allocator wrapper contexts are retained for safety and reused across restarts.

## Development

### Prerequisites

- Rust toolchain (latest stable)
- GIL-enabled CPython 3.11–3.15
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
uv sync --locked
```

3. For testing:

```bash
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
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

MIT — see [LICENSE.txt](LICENSE.txt).

## Acknowledgments

This project uses:

- [PyO3](https://github.com/PyO3/pyo3) for Rust-Python bindings
- [pprof](https://github.com/google/pprof) format for profile data visualization

See [the audit report](docs/deep-review.md) for findings, CPython source references, and validation results.
