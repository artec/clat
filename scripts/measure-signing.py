#!/usr/bin/env python3
"""macOS signing experiment on disposable copies, never on Cargo artifacts.

Build a harness first, then pass its path and a test filter. Each round uses
fresh copies with alternating order; repeat launches expose warm-cache effects.
Unsigned arm64 executables may be killed by the OS: that is not a timing win.
No security settings, quarantine attributes or production binaries are changed.
"""
import argparse
import json
import pathlib
import platform
import shutil
import subprocess
import tempfile
import time


def timed(command):
    start = time.monotonic()
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=60)
        return dict(seconds=round(time.monotonic() - start, 3),
                    returncode=result.returncode, stdout=result.stdout, stderr=result.stderr)
    except subprocess.TimeoutExpired:
        return dict(seconds=round(time.monotonic() - start, 3), timeout=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("filter")
    args = parser.parse_args()
    if platform.system() != "Darwin":
        parser.error("codesign experiment requires macOS")
    binary = args.binary.resolve(strict=True)
    directory = pathlib.Path(tempfile.mkdtemp(prefix="clat-signing-"))
    print(json.dumps(dict(directory=str(directory), binary=str(binary),
                          machine=platform.machine(), bytes=binary.stat().st_size,
                          original_signature=timed(["codesign", "-d", "-vv", str(binary)]))), flush=True)
    for round_number, order in enumerate([
            ["linker", "unsigned", "adhoc"], ["adhoc", "unsigned", "linker"]], 1):
        for mode in order:
            copy = directory / f"round-{round_number}-{mode}"
            shutil.copy2(binary, copy)
            preparation = None
            if mode != "linker":
                command = (["codesign", "--remove-signature"] if mode == "unsigned"
                           else ["codesign", "--force", "--sign", "-", "--identifier", "clat-signing-probe"])
                preparation = timed(command + [str(copy)])
            signature = timed(["codesign", "-d", "-vv", str(copy)])
            print(json.dumps(dict(round=round_number, mode=mode,
                                  preparation=preparation, signature=signature)), flush=True)
            if preparation and preparation.get("returncode") != 0:
                continue
            for launch in range(1, 4):
                print(json.dumps(dict(round=round_number, mode=mode, launch=launch,
                                      result=timed([str(copy), args.filter, "--quiet"]))), flush=True)


if __name__ == "__main__":
    main()
