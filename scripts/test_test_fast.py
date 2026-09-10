"""Selection is advisory, but must never turn an unknown change into a pass."""
import importlib.util
import contextlib
import io
import pathlib
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "test_fast", pathlib.Path(__file__).with_name("test-fast.py"))
fast = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fast)


class SelectionTests(unittest.TestCase):
    def setUp(self):
        self.output = contextlib.redirect_stdout(io.StringIO())
        self.output.__enter__()
        self.addCleanup(self.output.__exit__, None, None, None)

    def test_documentation_needs_no_rust_build(self):
        self.assertEqual(fast.selection(["docs/usage.md", "README.zh.md"]), [])

    def test_nested_domain_keeps_consumers(self):
        self.assertEqual(fast.selection(["src/session/catalog/validation.rs"]),
                         ["application::", "dsh::", "serve::", "session::", "wire::"])

    def test_shared_unknown_and_non_rust_inputs_fall_back(self):
        for path in ("src/model/context.rs", "src/new_domain.rs", "Cargo.lock",
                     "tests/fixtures/log.zstd", "web/app.js", "sdk/dsh-adapter/src/index.ts"):
            with self.subTest(path=path):
                self.assertIsNone(fast.selection(["README.md", path]))

    def test_staged_unstaged_and_new_paths_are_combined(self):
        with patch.object(fast, "run", side_effect=["src/tui.rs\0", "src/tui/new.rs\0"]):
            self.assertEqual(fast.changed_paths(), ["src/tui.rs", "src/tui/new.rs"])

    def test_zero_matching_and_ignored_only_fail(self):
        for output in ("test result: ok. 0 passed; 0 failed; 0 ignored;",
                       "test result: ok. 0 passed; 0 failed; 1 ignored;"):
            with self.subTest(output=output), \
                 patch.object(fast, "changed_paths", return_value=[]), \
                 patch.object(fast.sys, "argv", ["test-fast.py", "typo"]), \
                 patch.object(fast, "run", side_effect=["", output]):
                with self.assertRaises(SystemExit):
                    fast.main()

    def test_failed_command_preserves_failure(self):
        with patch.object(fast.subprocess, "run") as run:
            run.return_value.returncode = 7
            run.return_value.stdout = "compiler failed\n"
            with self.assertRaises(SystemExit) as result:
                fast.run(["cargo", "test"], True)
            self.assertEqual(result.exception.code, 7)


if __name__ == "__main__":
    unittest.main()
