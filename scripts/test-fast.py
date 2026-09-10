#!/usr/bin/env python3
"""Development feedback only. Full gates remain the delivery/CI safety net."""
import argparse
import pathlib
import re
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
# Shared contracts affect essentially every consumer: fall back to all Rust tests.
SHARED = {"lib", "model", "event", "tool", "permission", "message", "project",
          "plugin", "plugins", "test_support"}
# Include cross-domain consumers where a narrow source change changes behavior.
CONSUMERS = {
    "session": ("session", "application", "dsh", "wire", "serve"),
    "application": ("application", "exec", "serve", "tui"),
    "providers": ("providers", "application", "presets"),
    "presets": ("presets", "application", "tui"),
    "control_storage": ("control_storage", "application", "session"),
    "dsh": ("dsh", "tui"),
    "tui": ("tui",),
    "serve": ("serve",),
    "wire": ("wire", "serve", "exec"),
    "exec": ("exec",),
}


def run(command, capture=False):
    result = subprocess.run(command, cwd=ROOT, text=True,
                            stdout=subprocess.PIPE if capture else None)
    if result.returncode:
        if capture and result.stdout:
            print(result.stdout, end="")
        raise SystemExit(result.returncode)
    return result.stdout if capture else ""


def changed_paths():
    # HEAD diff includes staged and unstaged changes; untracked source is included.
    tracked = run(["git", "diff", "--name-only", "-z", "HEAD"], True)
    new = run(["git", "ls-files", "--others", "--exclude-standard", "-z"], True)
    return sorted(set(filter(None, (tracked + new).split("\0"))))


def selection(paths):
    filters = set()
    for name in paths:
        path = pathlib.PurePosixPath(name)
        if name.startswith("src/") and path.suffix == ".rs":
            domain = path.parts[1].removesuffix(".rs")
            if domain in SHARED or domain not in CONSUMERS:
                return None
            filters.update(f"{item}::" for item in CONSUMERS[domain])
        elif (name.startswith(("docs/", ".github/ISSUE_TEMPLATE/"))
              or name in {"README.md", "README.zh.md", "AGENTS.md", "LICENSE"}):
            continue
        else:
            # Includes fixtures, Cargo metadata, workflow/scripts, new domains,
            # adapter and web changes: no optimistic zero-test classification.
            return None
    return sorted(filters)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("filters", nargs="*", help="explicit Rust test name filters (OR)")
    parser.add_argument("--plan", action="store_true", help="show selection without building")
    args = parser.parse_args()
    paths = changed_paths()
    chosen = args.filters or selection(paths)
    print("快速反馈（非完整交付）：", "all Rust targets" if chosen is None else chosen,
          flush=True)
    if args.plan:
        return
    started = time.monotonic()
    run(["git", "diff", "--check"])
    if chosen == []:
        print("仅文档或无改动：diff 检查完成；未运行 Rust 测试。")
        return
    target = "--all-targets" if chosen is None else "--lib"
    command = ["cargo", "test", target, "--all-features", "--"] + (chosen or [])
    output = run(command + ["--quiet"], True)
    print(output, end="")
    if not any(int(value) for value in re.findall(r"(\d+) passed;", output)):
        raise SystemExit("没有实际通过的测试（零匹配或只匹配 ignored）；本次验证失败。")
    # Architecture/integration targets belong to the full gate. Building one
    # here also builds the CLI binary, doubling the library-only edit cycle.
    print(f"快速反馈完成：{time.monotonic() - started:.1f}s；交付运行 scripts/gates.sh --full")


if __name__ == "__main__":
    main()
