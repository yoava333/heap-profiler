"""Fail when profiler overhead, measured in executed instructions, exceeds its budget.

Each workload from overhead.py runs under Valgrind's cachegrind with and without
tracing, at two iteration counts. Subtracting the counts cancels interpreter
startup, imports, and start_tracing(), leaving the cost of the workload loop.
Instruction counts are deterministic, unlike wall time on shared CI runners.

Ratios depend on the CPU architecture and interpreter build, so the budget
records both and the gate refuses to compare against a different setup.

    python benchmarks/instruction_gate.py --python .venv/bin/python
    python benchmarks/instruction_gate.py --python .venv/bin/python --update
"""

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
WORKLOAD_SCRIPT = HERE / "overhead.py"
DEFAULT_BUDGET = HERE / "instruction_budget.json"
INSTRUCTIONS = re.compile(r"I\s+refs:\s+([\d,]+)")
# A deadlocked profiler would otherwise hang the gate instead of failing it.
TIMEOUT_SECONDS = 600


def count_instructions(python: str, workload: str, iterations: int, tracing: bool) -> int:
    command = [
        "valgrind",
        "--tool=cachegrind",
        "--cache-sim=no",
        "--cachegrind-out-file=/dev/null",
        python,
        str(WORKLOAD_SCRIPT),
        "--workload",
        workload,
        "--iterations",
        str(iterations),
        "--repeat",
        "1",
    ]
    if tracing:
        command.append("--tracing")
    env = dict(os.environ, PYTHONHASHSEED="0")
    try:
        result = subprocess.run(command, capture_output=True, text=True, env=env, check=False, timeout=TIMEOUT_SECONDS)  # noqa: S603
    except subprocess.TimeoutExpired:
        raise SystemExit(f"{workload} did not finish within {TIMEOUT_SECONDS}s under valgrind") from None
    match = INSTRUCTIONS.search(result.stderr)
    if result.returncode != 0 or not match:
        raise SystemExit(f"valgrind failed for {workload} (exit {result.returncode}):\n{result.stderr}")
    return int(match.group(1).replace(",", ""))


def workload_instructions(python: str, workload: str, iterations: int, tracing: bool) -> int:
    # Populate bytecode caches first; otherwise only the short run pays for
    # compiling imports and the subtraction underestimates the workload.
    warm_up = [python, str(WORKLOAD_SCRIPT), "--workload", workload, "--iterations", "1", "--repeat", "1"]
    if tracing:
        warm_up.append("--tracing")
    subprocess.run(warm_up, check=True, capture_output=True, timeout=TIMEOUT_SECONDS)  # noqa: S603
    short = count_instructions(python, workload, iterations, tracing)
    long = count_instructions(python, workload, 2 * iterations, tracing)
    return long - short


def interpreter_identity(python: str) -> dict[str, str]:
    script = (
        "import json, platform; print(json.dumps({'machine': platform.machine(), 'python': platform.python_version()}))"
    )
    output = subprocess.check_output([python, "-c", script], text=True)  # noqa: S603
    identity: dict[str, str] = json.loads(output)
    return identity


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--python", required=True, help="interpreter with heap_profiler installed")
    parser.add_argument("--budget", type=Path, default=DEFAULT_BUDGET)
    parser.add_argument("--output", type=Path, help="write measured counts and ratios as JSON")
    parser.add_argument("--update", action="store_true", help="record the measured ratios as the new budget")
    parser.add_argument(
        "--margin", type=float, default=0.10, help="allowed growth over the recorded ratio with --update"
    )
    args = parser.parse_args()

    budget = json.loads(args.budget.read_text())
    identity = interpreter_identity(args.python)
    if not args.update and identity != {"machine": budget["machine"], "python": budget["python"]}:
        print(
            f"Budget was recorded on {budget['machine']} with Python {budget['python']}, "
            f"but the interpreter is {identity['machine']} with Python {identity['python']}. "
            "Use the recorded setup, or re-record with --update.",
            file=sys.stderr,
        )
        return 2

    results = {}
    failures = []
    for name, entry in budget["workloads"].items():
        iterations = entry["iterations"]
        baseline = workload_instructions(args.python, name, iterations, tracing=False)
        profiled = workload_instructions(args.python, name, iterations, tracing=True)
        ratio = profiled / baseline
        results[name] = {"baseline_instructions": baseline, "profiled_instructions": profiled, "ratio": ratio}
        if args.update:
            entry["recorded_ratio"] = round(ratio, 3)
            entry["max_ratio"] = round(ratio * (1 + args.margin), 3)
            print(f"{name}: {ratio:.3f}x instructions, budget {entry['max_ratio']:.3f}x")
        else:
            status = "ok" if ratio <= entry["max_ratio"] else "OVER BUDGET"
            print(
                f"{name}: {ratio:.3f}x instructions (recorded {entry['recorded_ratio']:.3f}x, budget {entry['max_ratio']:.3f}x) {status}"
            )
            if ratio > entry["max_ratio"]:
                failures.append(name)

    if args.output:
        args.output.write_text(json.dumps({**identity, "workloads": results}, indent=2) + "\n")
    if args.update:
        budget.update(identity)
        args.budget.write_text(json.dumps(budget, indent=2) + "\n")
        print(f"Updated {args.budget}")
    elif failures:
        print(f"Instruction overhead over budget: {', '.join(failures)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
