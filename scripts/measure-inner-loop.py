#!/usr/bin/env python3
"""Measure one library edit loop, separating build from executable startup/tests.

Use after a real edit. --rebuild only changes a source mtime and is explicitly
reported as a synthetic rebuild, never evidence of a substantive logic edit.
No signing, security configuration, artifact stripping or dependency changes.
"""
import argparse
import importlib.util
import json
import pathlib
import re
import subprocess
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("test_fast", ROOT / "scripts/test-fast.py")
FAST = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FAST)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", choices=["clat", "clat-core"], required=True)
    parser.add_argument("--rebuild", type=pathlib.Path)
    parser.add_argument("--budget", type=float, help="fail when end-to-end seconds exceed this")
    parser.add_argument("filters", nargs="+")
    args = parser.parse_args()
    if args.rebuild:
        source = (ROOT / args.rebuild).resolve(strict=True)
        if not source.is_relative_to(ROOT / "src") or source.suffix != ".rs":
            parser.error("--rebuild must name an existing Rust source under src")
        source.touch()
    started = time.monotonic()
    build = subprocess.run(
        ["cargo", "test", "-p", args.package, "--lib",
         "--no-default-features" if args.package == "clat" and FAST.pure_ui_filters(args.filters)
         else "--all-features", "--no-run",
         "--message-format=json"], cwd=ROOT, text=True, stdout=subprocess.PIPE)
    built = time.monotonic()
    if build.returncode:
        print(build.stdout)
        raise SystemExit(build.returncode)
    artifacts = [json.loads(line) for line in build.stdout.splitlines() if line.startswith("{")]
    binaries = [item for item in artifacts if item.get("reason") == "compiler-artifact"
                and item.get("executable") and item.get("profile", {}).get("test")]
    if len(binaries) != 1:
        raise SystemExit(f"expected one harness, got {len(binaries)}")
    binary = pathlib.Path(binaries[0]["executable"])
    run = subprocess.run([str(binary), *args.filters, "--quiet"], cwd=ROOT,
                         text=True, stdout=subprocess.PIPE)
    finished = time.monotonic()
    print(run.stdout, end="")
    report = {
        "package": args.package, "filters": args.filters,
        "pure_ui": args.package == "clat" and FAST.pure_ui_filters(args.filters),
        "synthetic_rebuild": str(args.rebuild) if args.rebuild else None,
        "harness_fresh": binaries[0]["fresh"], "binary_bytes": binary.stat().st_size,
        "build_seconds": round(built - started, 3),
        "startup_and_tests_seconds": round(finished - built, 3),
        "total_seconds": round(finished - started, 3),
    }
    print(json.dumps(report, ensure_ascii=False))
    if run.returncode:
        raise SystemExit(run.returncode)
    if not any(int(n) for n in re.findall(r"(\d+) passed;", run.stdout)):
        raise SystemExit("no passing tests: zero/ignored-only match is not verification")
    if args.budget is not None and finished - started > args.budget:
        raise SystemExit("inner-loop latency budget exceeded")


if __name__ == "__main__":
    main()
