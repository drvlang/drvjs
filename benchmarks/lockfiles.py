#!/usr/bin/env python3
"""Compare release binaries on existing pnpm workspaces, without fetching or building."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import subprocess
import tempfile
import time


def measure(binary, project, operation, selector, scratch):
    with tempfile.TemporaryDirectory(dir=scratch) as tmp:
        export = Path(tmp) / "export"
        commands = {
            "list": ["workspace-info", project, "--list"],
            "select": ["workspace-info", project, "--", selector],
            "filtered_export": ["export", "-C", project, "-o", export, selector],
            "full_export": ["export", "-C", project, "-o", export],
        }
        start = time.perf_counter()
        output = subprocess.check_output([binary, *commands[operation]])
        elapsed = time.perf_counter() - start
        if export.exists():
            # The bundled worker source intentionally differs between binaries.
            signature = [hashlib.sha256((export / path).read_bytes()).hexdigest()
                         for path in ("project/pnpm-lock.yaml", "plan.json")]
        else:
            signature = json.loads(output)
        return elapsed * 1000, signature


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True)
    parser.add_argument("--optimized", required=True)
    parser.add_argument("--project", type=Path, action="append", required=True)
    parser.add_argument("--filter", required=True, help="workspace name present in every project")
    parser.add_argument("--runs", type=int, default=9)
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    results = []
    with tempfile.TemporaryDirectory(prefix="drvjs-lockfile-bench-") as scratch:
        for project in args.project:
            for operation in ("list", "select", "filtered_export", "full_export"):
                samples = {"baseline": [], "optimized": []}
                expected = None
                for trial in range(args.runs + 1):
                    for name in (samples if trial % 2 == 0 else reversed(samples)):
                        ms, signature = measure(getattr(args, name), project, operation, args.filter, scratch)
                        if expected is None:
                            expected = signature
                        elif signature != expected:
                            raise ValueError(f"different outputs for {operation} in {project}")
                        if trial:
                            samples[name].append(round(ms, 3))
                results.append({"project": project.name, "operation": operation, "samples_ms": samples,
                                "median_ms": {name: statistics.median(times) for name, times in samples.items()}})
    print(json.dumps({
        "baseline": args.baseline, "optimized": args.optimized, "filter": args.filter,
        "runs": args.runs,
        "inputs": {p.name: hashlib.sha256((p / "pnpm-lock.yaml").read_bytes()).hexdigest()
                   for p in args.project},
        "results": results,
    }, indent=2))


if __name__ == "__main__":
    main()
