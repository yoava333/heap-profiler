#!/usr/bin/env python3
import gc
import subprocess
import sys
import time


def run_pyperformance(with_profiler=False):
    """Run selected pyperformance benchmarks."""
    if with_profiler:
        import heap_profiler

        heap_profiler.start_tracing()

    # Select memory-intensive benchmarks
    benchmarks = [
        "json_dumps",  # String processing and memory allocation
        "pickle_pure_python",  # Object serialization
        "pickle_list",  # List serialization
        "pickle_dict",  # Dict serialization
        "pathlib",  # File system operations
    ]

    results = {}
    for benchmark in benchmarks:
        print(f"\nRunning pyperformance benchmark: {benchmark}")
        times = []
        for i in range(3):  # Run each benchmark 3 times
            cmd = [
                sys.executable,
                "-m",
                "pyperformance",
                "run",
                "--benchmarks",
                benchmark,
                "-r",
                "1",
                "--quiet",
            ]
            try:
                output = subprocess.check_output(
                    cmd, stderr=subprocess.STDOUT, text=True
                )
                # Extract timing information
                for line in output.split("\n"):
                    if "Total time:" in line:
                        time_value = float(line.split(":")[1].strip().split()[0])
                        times.append(time_value)
                        print(f"  Run {i+1}: {time_value:.3f}s")
            except subprocess.CalledProcessError as e:
                print(f"Error running benchmark {benchmark}: {e}")
                continue

        if times:
            avg_time = sum(times) / len(times)
            results[benchmark] = avg_time
            print(f"  Average: {avg_time:.3f}s")

    return results


def memory_intensive_task():
    """A memory-intensive task that creates and destroys objects."""
    data = []
    total_start = time.time()
    phase_times = []

    try:
        # Phase 1: String operations
        phase_start = time.time()
        for i in range(100):
            strings = ["x" * 100 for _ in range(100)]
            data.append(strings[0])
            if i % 10 == 0:
                strings = None
                gc.collect()
        phase_times.append(("Strings", time.time() - phase_start))

        # Phase 2: Dictionary operations
        phase_start = time.time()
        for i in range(100):
            dicts = [{str(j): j} for j in range(100)]
            data.append(dicts[0])
            if i % 10 == 0:
                dicts = None
                gc.collect()
        phase_times.append(("Dictionaries", time.time() - phase_start))

        # Phase 3: List operations
        phase_start = time.time()
        for i in range(100):
            lists = [[k for k in range(10)] for _ in range(100)]
            data.append(lists[0])
            if i % 10 == 0:
                lists = None
                gc.collect()
        phase_times.append(("Lists", time.time() - phase_start))

    except Exception as e:
        print(f"Error during task: {e}")
        import traceback

        traceback.print_exc()
        return None, None

    total_time = time.time() - total_start
    return total_time, phase_times


def run_custom_benchmark(with_profiler=False):
    """Run the custom memory benchmark."""
    if with_profiler:
        try:
            import heap_profiler

            heap_profiler.start_tracing()
            print("Successfully enabled heap profiler")
        except Exception as e:
            print(f"Error enabling heap profiler: {e}")
            return None
        print("Running with heap profiler enabled...")
    else:
        print("Running without heap profiler...")

    # Clear any garbage before run
    gc.collect()

    print("Starting memory intensive task...")
    result = memory_intensive_task()

    if result is None:
        return None

    total_time, phase_times = result
    return {"total_time": total_time, "phase_times": phase_times}


def print_custom_benchmark_results(baseline_results, profiler_results):
    """Print results from custom memory benchmark."""
    print("\nCustom Memory Benchmark Results:")
    print("=" * 80)
    baseline_time = baseline_results["total_time"]
    profiler_time = profiler_results["total_time"]
    overhead_pct = ((profiler_time - baseline_time) / baseline_time) * 100

    print(
        f"{'Operation':<15} {'Baseline (s)':<15} {'With Profiler (s)':<15} {'Overhead'}"
    )
    print("-" * 80)
    print(
        f"{'Total':<15} {baseline_time:>14.3f} {profiler_time:>14.3f} {overhead_pct:>7.1f}%"
    )

    for (baseline_phase, baseline_time), (profiler_phase, profiler_time) in zip(
        baseline_results["phase_times"], profiler_results["phase_times"]
    ):
        assert baseline_phase == profiler_phase
        phase_overhead = ((profiler_time - baseline_time) / baseline_time) * 100
        print(
            f"{baseline_phase:<15} {baseline_time:>14.3f} {profiler_time:>14.3f} {phase_overhead:>7.1f}%"
        )
    print("=" * 80)


def print_pyperformance_results(baseline_results, profiler_results):
    """Print results from pyperformance benchmarks."""
    print("\nPyperformance Benchmark Results:")
    print("=" * 80)
    print(
        f"{'Benchmark':<20} {'Baseline (s)':<15} {'With Profiler (s)':<15} {'Overhead'}"
    )
    print("-" * 80)

    for benchmark in baseline_results:
        if benchmark in profiler_results:
            baseline_time = baseline_results[benchmark]
            profiler_time = profiler_results[benchmark]
            overhead_pct = ((profiler_time - baseline_time) / baseline_time) * 100
            print(
                f"{benchmark:<20} {baseline_time:>14.3f} {profiler_time:>14.3f} {overhead_pct:>7.1f}%"
            )
    print("=" * 80)


def main():
    print("Starting benchmarks...")
    print(f"Python version: {sys.version}")

    # Run custom memory benchmarks
    print("\nRunning custom memory benchmarks...")
    print("\nRunning baseline measurement...")
    baseline_custom = run_custom_benchmark(with_profiler=False)
    if baseline_custom is None:
        print("Baseline measurement failed!")
        return

    print("\nRunning measurement with profiler...")
    profiler_custom = run_custom_benchmark(with_profiler=True)
    if profiler_custom is None:
        print("Profiler measurement failed!")
        return

    # Run pyperformance benchmarks
    print("\nRunning pyperformance benchmarks...")
    print("\nRunning baseline measurements...")
    baseline_pyperf = run_pyperformance(with_profiler=False)

    print("\nRunning measurements with profiler...")
    profiler_pyperf = run_pyperformance(with_profiler=True)

    # Print all results
    print_custom_benchmark_results(baseline_custom, profiler_custom)
    print_pyperformance_results(baseline_pyperf, profiler_pyperf)


if __name__ == "__main__":
    main()
