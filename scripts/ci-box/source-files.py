#!/usr/bin/env python3
"""Export the current Git-visible worktree, including new files but no ignored archives."""
import os
import pathlib
import stat
import subprocess
import sys


def source_files(root):
    listed = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        cwd=root, capture_output=True, check=True, timeout=30,
    ).stdout
    files = []
    for name in sorted(set(listed.split(b"\0")) - {b""}):
        path = root / os.fsdecode(name)
        try:
            mode = path.lstat().st_mode
        except FileNotFoundError:
            continue  # A tracked deletion must stay deleted in the exported tree.
        if not (stat.S_ISREG(mode) or stat.S_ISLNK(mode)):
            raise ValueError(f"CI source must be a file or symlink: {os.fsdecode(name)}")
        files.append(name)
    return files


def main():
    root = pathlib.Path(__file__).resolve().parents[2]
    files = source_files(root)
    sys.stdout.buffer.write(b"".join(name + b"\0" for name in files))


if __name__ == "__main__":
    main()
