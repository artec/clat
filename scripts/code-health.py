#!/usr/bin/env python3
"""Reproducible source-shape indicators, not semantic architecture proof.

Compare --revision HEAD with the working tree. Test-only files are excluded;
inline test modules are cut at cfg(test) mod, not at test-only imports. Function
length uses rustfmt closing indentation (generated macro bodies are excluded).
"""
import argparse
import json
import pathlib
import re
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision")
    args = parser.parse_args()
    if args.revision:
        paths = subprocess.check_output(
            ["git", "ls-tree", "-r", "--name-only", args.revision, "src"],
            cwd=ROOT, text=True).splitlines()
        sources = {p: subprocess.check_output(["git", "show", f"{args.revision}:{p}"],
                                             cwd=ROOT, text=True)
                   for p in paths if p.endswith(".rs")}
    else:
        sources = {str(p.relative_to(ROOT)): p.read_text() for p in (ROOT / "src").rglob("*.rs")}
    production = {}
    for path, source in sources.items():
        if ("test_support" in path or "tests" in pathlib.Path(path).stem
                or pathlib.Path(path).stem in {"agent_eval", "dsh_golden", "live_openai"}):
            continue
        source = re.split(r"#\[cfg\((?:test|all\(test,[^\n]*\))\)\]\s*(?:#\[[^\n]*\]\s*)?mod \w+", source)[0]
        production[path] = source
    total = sum(len(s.splitlines()) for s in sources.values())
    prod = sum(len(s.splitlines()) for s in production.values())
    root_prod = sum(len(s.splitlines()) for p, s in production.items() if len(pathlib.Path(p).parts) == 2)
    long_functions = []
    for path, source in production.items():
        lines = source.splitlines()
        for index, line in enumerate(lines):
            match = re.match(r"( *)(?:pub(?:\([^)]*\))? )?(?:async )?fn (\w+)", line)
            if not match:
                continue
            close = match[1] + "}"
            end = next((j for j in range(index + 1, len(lines)) if lines[j] == close), None)
            if end is not None and end - index + 1 >= 80:
                long_functions.append([path, index + 1, match[2], end - index + 1])
    plugin_root = production["src/plugins/mod.rs"]
    plugin_fanout = sorted(set(re.findall(r"\bcrate::([a-z][a-z_0-9]*)::", "\n".join(
        s for p, s in production.items() if p.startswith("src/plugins/")))) - {"plugins"})
    print(json.dumps({
        "revision": args.revision or "working-tree", "source_files": len(sources),
        "total_lines": total, "production_prefix_lines": prod,
        "root_production_lines": root_prod, "root_production_percent": round(root_prod * 100 / prod, 2),
        "functions_ge_80": len(long_functions),
        "plugin_direct_modules": len(re.findall(r"^(?:pub\(crate\) )?mod ", plugin_root, re.M)),
        "plugin_aggregate_core_dependencies": plugin_fanout,
        "long_functions": sorted(long_functions, key=lambda x: -x[3]),
    }, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
