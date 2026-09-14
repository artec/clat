#!/usr/bin/env python3
"""Reject unregistered blocking calls in Rust test code (standard library only)."""
import collections
import hashlib
import os
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
ALLOWLIST = ROOT / "scripts/test-waits.allowlist.tsv"
EVENT = re.compile(r"[{};]|(?:\.|::)\s*(output|wait_with_output|recv|wait)\s*\(")
ITEM = re.compile(r"\b(fn|mod)\s+(\w+)")
RAW = re.compile(r'(?:br|r)(#*)"')
CHAR = re.compile(r"(?:b)?'(?:\\u\{[0-9a-fA-F]+\}|\\.|[^'\\\n])'")


def mask_non_code(source):
    """Keep offsets/newlines, including nested comments and Rust raw strings."""
    result = list(source)
    index = 0
    while index < len(source):
        end = index
        if source.startswith("//", index):
            end = source.find("\n", index)
            end = len(source) if end < 0 else end
        elif source.startswith("/*", index):
            end, depth = index + 2, 1
            while end < len(source) and depth:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
        else:
            raw = RAW.match(source, index)
            char = CHAR.match(source, index)
            if raw:
                terminator = '"' + raw.group(1)
                found = source.find(terminator, raw.end())
                end = len(source) if found < 0 else found + len(terminator)
            elif char:
                end = char.end()
            elif source[index] == '"' or source.startswith('b"', index):
                end = index + (2 if source[index] == "b" else 1)
                while end < len(source):
                    if source[end] == "\\":
                        end += 2
                    elif source[end] == '"':
                        end += 1
                        break
                    else:
                        end += 1
        if end > index:
            for offset in range(index, end):
                if result[offset] != "\n":
                    result[offset] = " "
            index = end
        else:
            index += 1
    return "".join(result)


def test_file(path):
    path = pathlib.PurePosixPath(path)
    return (bool({"tests", "test_support"} & set(path.parts))
            or path.stem in {"tests", "test_support"}
            or path.stem.endswith("_tests"))


def test_attribute(header):
    return any(re.search(r"\btest\b", attribute)
               for attribute in re.findall(r"#\s*\[([^]]*)\]", header)
               if re.match(r"\s*(?:(?:\w+::)*test\b|cfg(?:_attr)?\b)", attribute))


def call_end(code, opening):
    depth = 1
    for index in range(opening + 1, len(code)):
        if code[index] == "(":
            depth += 1
        elif code[index] == ")":
            depth -= 1
            if not depth:
                return index + 1
    raise ValueError("unclosed blocking call")


def external_module(path, header, original):
    module = re.search(r"\bmod\s+(\w+)\s*$", header)
    if not module:
        return []
    parent = pathlib.PurePosixPath(path).parent
    explicit = re.search(r'#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]', original)
    if explicit:
        return [(parent / explicit.group(1)).as_posix()]
    file = pathlib.PurePosixPath(path)
    base = parent if file.stem in {"lib", "main", "mod", "core"} else parent / file.stem
    name = module.group(1)
    return [(base / f"{name}.rs").as_posix(), (base / name / "mod.rs").as_posix()]


def scan_source(path, source, inherited=False, external=None):
    source = source.replace("\r\n", "\n")
    code = mask_non_code(source)
    stack = [(test_file(path) or inherited, "")]
    boundary = 0
    occurrences = collections.Counter()
    calls = []
    for event in EVENT.finditer(code):
        token = event.group(0)
        if token == "{":
            header = code[boundary:event.start()]
            items = list(ITEM.finditer(header))
            name = items[-1].group(2) if items else ""
            testing = stack[-1][0] or test_attribute(header)
            if items and items[-1].group(1) == "mod":
                testing = testing or name == "tests" or name.endswith("_tests")
            stack.append((testing, name))
        elif token == "}":
            if len(stack) > 1:
                stack.pop()
        elif token == ";" and external is not None:
            header = code[boundary:event.start()]
            if stack[-1][0] or test_attribute(header):
                external.extend(external_module(path, header, source[boundary:event.start()]))
        elif event.group(1) and stack[-1][0]:
            operation = event.group(1)
            end = call_end(code, event.end() - 1)
            expression = re.sub(r"\s+", "", code[boundary:end])
            fingerprint = hashlib.sha256(expression.encode()).hexdigest()[:16]
            scope = "::".join(name for _, name in stack if name) or "<file>"
            base = (path, scope, operation, fingerprint)
            occurrences[base] += 1
            key = (*base, str(occurrences[base]))
            line = source.count("\n", 0, event.start()) + 1
            calls.append((key, line))
        if token in {"{", "}", ";"}:
            boundary = event.end()
    return calls


def inventory(root):
    sources = {}
    # Walk new directories too, without depending on git's tracked-file index.
    for directory, children, files in os.walk(root):
        children[:] = sorted(set(children) - {"target", "node_modules", ".git", ".codegraph"})
        for name in sorted(files):
            if name.endswith(".rs"):
                file = pathlib.Path(directory) / name
                path = file.relative_to(root).as_posix()
                sources[path] = file.read_text(encoding="utf-8")
    calls, inherited = {}, set()
    pending = list(sorted(sources))
    while pending:
        path = pending.pop()
        external = []
        calls[path] = scan_source(path, sources[path], path in inherited, external)
        for reference in external:
            # Normalize ../ path attributes without relying on the git index.
            reference = os.path.normpath(reference).replace(os.sep, "/")
            if reference in sources and reference not in inherited:
                inherited.add(reference)
                pending.append(reference)
    return [call for path in sorted(calls) for call in calls[path]]


def read_allowlist(path):
    entries = {}
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line or line.startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) != 6 or not fields[-1].strip():
            raise ValueError(f"{path}:{number}: expected site fields and review reason")
        key = tuple(fields[:5])
        if key in entries:
            raise ValueError(f"{path}:{number}: duplicate registration {key}")
        entries[key] = fields[-1]
    return entries


def violations(calls, entries):
    actual = {key for key, _ in calls}
    errors = [f"{key[0]}:{line}: unregistered {key[1]} .{key[2]}() [{key[3]} #{key[4]}]"
              for key, line in calls if key not in entries]
    errors.extend(f"stale registration: {' / '.join(key)}"
                  for key in sorted(set(entries) - actual))
    return errors


def main():
    calls = inventory(ROOT)
    errors = violations(calls, read_allowlist(ALLOWLIST))
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"Rust test wait inventory: {len(calls)} individually registered calls; no new unbounded waits")
    return 0


if __name__ == "__main__":
    sys.exit(main())
