"""The guard rejects new sites, rather than granting file/function exemptions."""
import importlib.util
import pathlib
import shutil
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "check_test_waits", pathlib.Path(__file__).with_name("check-test-waits.py"))
waits = importlib.util.module_from_spec(spec)
spec.loader.exec_module(waits)


class WaitGuardTests(unittest.TestCase):
    def scan(self, source, path="src/domain.rs"):
        return waits.scan_source(path, source)

    def test_inline_helpers_and_test_only_functions_are_covered(self):
        source = """
        fn production() { rx.recv(); }
        #[cfg(all(test, feature = "runtime-tests"))]
        mod tests { fn helper() { cmd.output(); }
          #[test] fn run() { rx.recv(); child.wait_with_output(); barrier.wait(); }
        }
        #[cfg(test)] fn fixture() { child.wait(); }
        fn production_after() { cmd.output(); }
        """
        self.assertEqual([key[2] for key, _ in self.scan(source)],
                         ["output", "recv", "wait_with_output", "wait", "wait"])

    def test_external_test_files_and_new_files_are_covered(self):
        for path in ("tests/new.rs", "tests/support/child.rs", "src/test_support/agent.rs",
                     "src/domain/protocol_tests.rs", "src/domain/tests.rs"):
            with self.subTest(path=path):
                self.assertEqual(len(self.scan("fn helper() { rx.recv(); }", path)), 1)
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "tests").mkdir()
            (root / "tests/new.rs").write_text("fn helper() { cmd.output(); }")
            self.assertEqual(len(waits.inventory(root)), 1)

    def test_comments_strings_raw_strings_chars_and_lifetimes(self):
        source = r'''
        // #[cfg(test)] mod ghost { cmd.output(); }
        /* nested /* #[test] fn ghost() { rx.recv(); } */ comment */
        #[test] fn real<'a>() {
          let a = "rx.recv(); }";
          let b = br###"child.wait_with_output(); {"###;
          let c = '{'; let d = '\\';
          rx
            .recv_timeout(deadline);
          child
            .wait();
        }
        '''
        self.assertEqual([key[2] for key, _ in self.scan(source)], ["wait"])

    def test_test_only_external_modules_are_covered_even_with_generic_names(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "src").mkdir()
            (root / "src/lib.rs").write_text(
                '#[cfg(test)] #[path="helper.rs"] mod helpers;')
            (root / "src/helper.rs").write_text('mod fixture; fn helper() { rx.recv(); }')
            (root / "src/helper").mkdir()
            (root / "src/helper/fixture.rs").write_text('fn fixture() { cmd.output(); }')
            self.assertEqual(len(waits.inventory(root)), 2)

    def test_crlf_and_whitespace_do_not_change_registration(self):
        source = "#[test]\nfn run() {\n child\n .wait();\n}\n"
        self.assertEqual(self.scan(source), self.scan(source.replace("\n", "\r\n")))
        key = self.scan(source)[0][0]
        self.assertEqual(key, self.scan("#[test] fn run(){child.wait();}")[0][0])

    def test_each_call_is_registered_and_stale_entries_fail(self):
        old = self.scan("#[test] fn run() { rx.recv(); }")
        entries = {old[0][0]: "legacy receipt wait"}
        self.assertEqual(waits.violations(old, entries), [])
        added = self.scan("#[test] fn run() { rx.recv(); rx.recv(); other.recv(); }")
        self.assertEqual(len(waits.violations(added, entries)), 2)
        self.assertTrue(waits.violations([], entries))

    def test_multiline_ufcs_and_custom_waits_require_registration(self):
        calls = self.scan("#[test] fn run() { Child::wait(&mut child); outbox.recv(&shutdown); }")
        self.assertEqual([key[2] for key, _ in calls], ["wait", "recv"])

    def test_namespaced_async_test_attributes_are_covered(self):
        calls = self.scan("#[tokio::test] async fn run() { child.output().await; }")
        self.assertEqual([key[2] for key, _ in calls], ["output"])

    def test_gates_reject_unknown_waits_before_fast_full_and_ci_work(self):
        scripts = pathlib.Path(__file__).parent
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "scripts").mkdir()
            (root / "tests").mkdir()
            for name in ("gates.sh", "check-test-waits.py"):
                shutil.copyfile(scripts / name, root / "scripts" / name)
            (root / "scripts/test-waits.allowlist.tsv").write_text("# empty inventory\n")
            (root / "scripts/test-fast.py").write_text('print("fast tier ran")\n')
            (root / "tests/new.rs").write_text('fn test_helper() { child.wait_with_output(); }')
            for mode in ("--fast", "--full", "--ci"):
                with self.subTest(mode=mode):
                    result = subprocess.run(
                        ["bash", "scripts/gates.sh", mode], cwd=root,
                        capture_output=True, text=True, timeout=10)
                    self.assertEqual(result.returncode, 1, result.stderr)
                    self.assertIn("tests/new.rs:1: unregistered", result.stderr)
                    self.assertIn("wait_with_output", result.stderr)
                    self.assertNotIn("fast tier ran", result.stdout)

    def test_old_converge_calls_have_no_exemption(self):
        root = pathlib.Path(__file__).resolve().parents[1]
        entries = waits.read_allowlist(root / "scripts/test-waits.allowlist.tsv")
        source = """fn host_spawn_or_attach_requires_trust_and_converges_two_launchers() {
        first.wait_with_output(); second.wait_with_output();
        command().args(["host", "status"]).output();
        command().args(["host", "stop"]).output(); }"""
        calls = self.scan(source, "tests/serve_lifecycle.rs")
        self.assertTrue(all(key not in entries for key, _ in calls))


if __name__ == "__main__":
    unittest.main()
