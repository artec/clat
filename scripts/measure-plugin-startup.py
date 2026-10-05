#!/usr/bin/env python3
"""Explicitly authorized host stop/start measurements using the real local store.

Never changes plugin configuration. Run after builds/tests finish, on an idle host.
The report describes process restarts with OS disk caches retained, not cold disks.
"""
import argparse
import json
import pathlib
import statistics
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", action="append", required=True, help="label=/absolute/binary")
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--allow-stop-host", action="store_true", required=True)
    args = parser.parse_args()
    if args.samples < 3:
        parser.error("at least three repeated samples required")
    rows = []
    for item in args.binary:
        label, raw = item.split("=", 1)
        binary = str(pathlib.Path(raw).resolve(strict=True))
        for sample in range(args.samples):
            stopped = subprocess.run([binary, "host", "stop"], capture_output=True, text=True)
            if stopped.returncode and "no host" not in stopped.stderr.lower() and "not running" not in stopped.stderr.lower() and "host is offline" not in stopped.stderr.lower():
                raise RuntimeError(stopped.stderr or stopped.stdout)
            deadline = time.monotonic() + 10
            while subprocess.run([binary, "host", "status"], capture_output=True).returncode == 0:
                if time.monotonic() >= deadline:
                    raise RuntimeError("host did not finish stopping")
                time.sleep(0.05)
            time.sleep(0.1)
            start = time.perf_counter()
            result = subprocess.run([binary, "host", "start", "--trust"], capture_output=True, text=True)
            elapsed = time.perf_counter() - start
            if result.returncode:
                raise RuntimeError(result.stderr or result.stdout)
            rows.append({"binary": label, "sample": sample + 1, "seconds": round(elapsed, 6)})
            print(json.dumps(rows[-1]), flush=True)
    stopped = subprocess.run([binary, "host", "stop"], capture_output=True, text=True)
    if stopped.returncode:
        raise RuntimeError(stopped.stderr or stopped.stdout)
    summary = {label: statistics.median(row["seconds"] for row in rows if row["binary"] == label)
               for label in dict.fromkeys(row["binary"] for row in rows)}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps({"measurement": "real host process restart; existing local plugins; warm OS disk cache", "samples": rows, "median_seconds": summary}, indent=2) + "\n")


if __name__ == "__main__":
    main()
