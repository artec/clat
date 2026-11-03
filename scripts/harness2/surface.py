"""The only editable experiment component is the summary directive."""
from pathlib import Path
import re

FILE = "summary_guidance.md"
def surface_spec():
    from harness2.core.tree import SurfaceSpec
    return SurfaceSpec(name="clat", always_on=(FILE,), always_on_max=65536,
    always_on_cap_reason="private experiment input safety limit; real capacity is measured by CLAT",
    skill_dir="skills", skill_name_rule="must_match", trigger_hint="Skills are unavailable",
    autotrigger_file=FILE, trigger_line="", subagent_requires_mode=True,
    plugin_dir=None, hooks_file=None, reject_empty_always_on=True)


def validate_tree(tree):
    if set(tree) != {FILE} or not tree[FILE].strip() or "\0" in tree[FILE] or len(tree[FILE].encode()) > 65536:
        raise ValueError("only bounded, nonempty summary_guidance.md may be edited")
    for heading in ("Primary Request and Intent", "Key Technical Concepts", "Files and Code", "Errors and Fixes", "Pending Jobs", "Current Work", "Next Step", "Critical Context"):
        if "## " + heading not in tree[FILE]:
            raise ValueError("the eight-section checkpoint contract must remain")
    if re.search(r"\b(?:ORION|HEAD|TAIL|STEER)[-_]\d+|artifacts/(?:合成\d+|星河)/",tree[FILE]):
        raise ValueError("general guidance cannot hardcode synthetic task answers")


def read_tree(root):
    root = Path(root)
    result = {}
    if root.is_symlink():
        raise ValueError("harness root must not be a symlink")
    for path in root.rglob("*"):
        if path.is_symlink():
            raise ValueError("symlinks are outside the editable surface")
        if path.is_file():
            if path.relative_to(root).as_posix() != FILE or path.stat().st_mode & 0o111:
                raise ValueError("unknown or executable harness file")
            result[FILE] = path.read_text(encoding="utf-8")
    validate_tree(result)
    return result


class ClatSubstrate:
    name = "clat"

    def surface(self):
        return surface_spec()

    def materialise(self, tree_dir, spec, ctx):
        from harness2.substrate.base import Install
        from protocol import digest
        tree = read_tree(tree_dir)
        return Install(Path(tree_dir), digest(tree))
