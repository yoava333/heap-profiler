"""Run in separate interpreters, with and without --tracing, to compare overhead."""

import argparse
import json
import statistics
import time
import zlib

parser = argparse.ArgumentParser()
parser.add_argument("--tracing", action="store_true")
parser.add_argument("--repeat", type=int, default=7)
parser.add_argument("--iterations", type=int, default=10000)
parser.add_argument(
    "--workload", choices=["all", "small_allocations", "medium_allocations", "compression"], default="all"
)
args = parser.parse_args()


def small_allocations():
    for _ in range(args.iterations):
        blocks = [bytearray(64) for _ in range(16)]
        del blocks


def medium_allocations():
    for _ in range(args.iterations):
        blocks = [bytearray(4096) for _ in range(8)]
        del blocks


payload = bytes(range(256)) * 4096


def compression():
    for _ in range(max(1, args.iterations // 200)):
        zlib.compress(payload)


if args.tracing:
    import heap_profiler

    heap_profiler.start_tracing()

results = {}
for workload in (small_allocations, medium_allocations, compression):
    if args.workload != "all" and workload.__name__ != args.workload:
        continue
    workload()
    timings = []
    for _ in range(args.repeat):
        start = time.perf_counter()
        workload()
        timings.append(time.perf_counter() - start)
    results[workload.__name__] = statistics.median(timings)

if args.tracing:
    heap_profiler.stop_tracing()
print(json.dumps(results, sort_keys=True))
