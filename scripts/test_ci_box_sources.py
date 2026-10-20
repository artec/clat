"""CI exports current product sources, never ignored historical worktree copies."""
import importlib.util
import os
import pathlib
import subprocess
import tempfile
import unittest

SCRIPT_ROOT = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("ci_sources", SCRIPT_ROOT / "ci-box/source-files.py")
sources = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sources)


class SourceExportTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.base = pathlib.Path(self.directory.name)
        self.root = self.base / "repo"
        self.root.mkdir()
        subprocess.run(["git", "init", "-q", str(self.root)], check=True, timeout=10)

    def write(self, name, content=b"content"):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        return path

    def add(self, name):
        subprocess.run(["git", "add", "--", name], cwd=self.root, check=True, timeout=10)

    def test_visible_new_and_tracked_ignored_files_survive_but_archives_do_not(self):
        self.write("src/tracked.rs")
        self.add("src/tracked.rs")
        self.write(".gitignore", b".tmp/\noutput/\nsecret.env\nsrc/tracked.rs\n")
        self.write("src/new.rs")
        self.write(".tmp/old/src/waits.rs", b"fn old() { rx.recv(); }")
        self.write("output/session.txt")
        self.write("secret.env")
        self.assertEqual(set(sources.source_files(self.root)),
                         {b".gitignore", b"src/tracked.rs", b"src/new.rs"})

    def test_tracked_deletions_stay_absent_and_modified_files_appear_once(self):
        removed = self.write("src/removed.rs")
        self.add("src/removed.rs")
        self.write("src/changed.rs")
        self.add("src/changed.rs")
        self.write("src/changed.rs", b"new content")
        removed.unlink()
        self.assertEqual(sources.source_files(self.root), [b"src/changed.rs"])

    def test_nul_manifest_roundtrips_unusual_names_and_symlinks_without_following_them(self):
        names = ["src/a space.rs", "src/line\nbreak.rs", "-leading-option", "source"]
        for name in names:
            self.write(name, os.fsencode(name))
        (self.root / "alias").symlink_to("source")
        manifest = b"".join(name + b"\0" for name in sources.source_files(self.root))
        archive = self.base / "source.tar"
        subprocess.run(["tar", "--null", "--no-recursion", "-T", "-", "-cf", str(archive)],
                       cwd=self.root, input=manifest, check=True, timeout=10)
        destination = self.base / "export"
        destination.mkdir()
        subprocess.run(["tar", "-xf", str(archive)], cwd=destination, check=True, timeout=10)
        for name in names:
            self.assertEqual((destination / name).read_bytes(), os.fsencode(name))
        self.assertTrue((destination / "alias").is_symlink())
        self.assertEqual(os.readlink(destination / "alias"), "source")

    def test_missing_git_is_an_error_and_entrypoint_preserves_shared_ci_gates(self):
        plain = self.base / "no-git"
        plain.mkdir()
        with self.assertRaises(subprocess.CalledProcessError):
            sources.source_files(plain)
        entrypoint = (SCRIPT_ROOT / "ci-box.sh").read_text().replace("\r\n", "\n")
        self.assertIn("source-files.py | docker run", entrypoint)
        self.assertIn("--null --no-recursion -T -", entrypoint)
        self.assertIn("scripts/gates.sh --ci", entrypoint)
        gates = (SCRIPT_ROOT / "gates.sh").read_text().replace("\r\n", "\n")
        self.assertIn("test_ci_box_sources.py", gates)


if __name__ == "__main__":
    unittest.main()
