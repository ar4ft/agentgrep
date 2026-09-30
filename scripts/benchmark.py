#!/usr/bin/env python3
"""Synthetic evidence benchmark; no task-success or cost claims."""
import argparse
import json
from pathlib import Path
import shutil
import statistics
import subprocess
import tempfile
import time


def measure(command, runs=5):
    durations = []
    for _ in range(runs):
        started = time.perf_counter()
        result = subprocess.run(command, check=True, capture_output=True)
        durations.append((time.perf_counter() - started) * 1000)
    return {"median_ms": round(statistics.median(durations), 2), "stdout_bytes": len(result.stdout), "runs": runs}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/release/agx")
    parser.add_argument("--files", type=int, default=1000)
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve(strict=True))
    with tempfile.TemporaryDirectory(prefix="agx-bench-") as directory:
        root = Path(directory)
        for n in range(args.files):
            folder = root / "src" / f"module_{n // 50}"
            folder.mkdir(parents=True, exist_ok=True)
            behavior = "session_token" if n % 20 == 0 else "render_canvas"
            (folder / f"file_{n}.py").write_text(f"def {behavior}_{n}(value):\n    # {behavior} implementation\n    return value\n" * 20)
        prefix = [binary, "search"]
        report = {"corpus": {"files": args.files, "lines_per_file": 60}, "measurements": {}}
        report["measurements"]["text"] = measure(prefix + ["session_token", directory, "--limit", "8"])
        report["measurements"]["symbol"] = measure(prefix + ["session_token", directory, "--mode", "symbol", "--limit", "8"])
        ranked = prefix + ["session token", directory, "--mode", "ranked", "--limit", "8"]
        report["measurements"]["ranked_cold"] = measure(ranked, runs=1)
        report["measurements"]["ranked_warm"] = measure(ranked)
        if shutil.which("rg"):
            report["measurements"]["ripgrep_unbounded_lines"] = measure(["rg", "-n", "session_token", directory])
        subprocess.run([binary, "clean", directory], check=True, capture_output=True)
        report["limitations"] = "Synthetic corpus, no semantic models, outputs have different scope; timings include process startup. Source byte limits do not bound JSON metadata. Not an agent solve-rate evaluation."
        print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
