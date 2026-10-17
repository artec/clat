"""Occupied local host means a visible coverage gap, never false passes."""
import contextlib
import io
import os
import pathlib
import socket
import subprocess
import unittest
from unittest.mock import patch

import rust_test_ports as ports


class FixedPortTests(unittest.TestCase):
    def setUp(self):
        output = contextlib.redirect_stdout(io.StringIO())
        output.__enter__()
        self.addCleanup(output.__exit__, None, None, None)
        self.command = ["cargo", "test", "--all-targets", "--all-features", "--"]
        self.listing = subprocess.CompletedProcess([], 0, stdout="\n".join(
            name + ": test" for name in (*ports.FIXED_TESTS, "ordinary_test")))

    def test_occupied_local_host_skips_only_listed_tests_and_reports_count(self):
        with socket.socket() as host, patch.dict(os.environ, {}, clear=True):
            host.bind(("127.0.0.1", 0))
            host.listen()
            port = host.getsockname()[1]
            output = io.StringIO()
            with patch.object(ports.subprocess, "run", return_value=self.listing), \
                    contextlib.redirect_stdout(output):
                command = ports.prepare(self.command, port=port)
            self.assertEqual(command[len(self.command):],
                             [arg for name in ports.FIXED_TESTS for arg in ("--skip", name)])
            self.assertIn("4 fixed-port tests skipped (not passed)", output.getvalue())
            self.assertIn("occupied", output.getvalue())
            self.assertNotIn("SKIP ordinary_test", output.getvalue())
            self.assertTrue(ports.occupied(port))

    def test_free_port_does_not_skip_or_change_selection(self):
        with patch.object(ports, "occupied", return_value=False):
            self.assertEqual(ports.prepare(self.command, ci=True), self.command)

    def test_ci_occupied_is_hard_failure(self):
        with patch.object(ports, "occupied", return_value=True):
            with self.assertRaisesRegex(SystemExit, "refusing to skip"):
                ports.prepare(self.command, ci=True)
            with patch.dict(os.environ, {"CI": "true"}):
                with self.assertRaisesRegex(SystemExit, "refusing to skip"):
                    ports.prepare(self.command)

    def test_library_faces_never_probe_host(self):
        with patch.object(ports, "occupied", side_effect=AssertionError("unexpected probe")):
            command = ["cargo", "test", "--lib", "--", "serve::"]
            self.assertEqual(ports.prepare(command), command)

    def test_platform_or_filter_selection_controls_actual_skip_count(self):
        listed = subprocess.CompletedProcess([], 0, stdout=ports.FIXED_TESTS[0] + ": test\n")
        with patch.dict(os.environ, {}, clear=True), \
                patch.object(ports, "occupied", return_value=True), \
                patch.object(ports.subprocess, "run", return_value=listed):
            command = ports.prepare(self.command)
        self.assertEqual(command[len(self.command):], ["--skip", ports.FIXED_TESTS[0]])

    def test_skip_substring_collision_fails_closed(self):
        self.listing.stdout += "\n" + ports.FIXED_TESTS[0] + "_other: test"
        with patch.dict(os.environ, {}, clear=True), \
                patch.object(ports, "occupied", return_value=True), \
                patch.object(ports.subprocess, "run", return_value=self.listing):
            with self.assertRaisesRegex(SystemExit, "Ambiguous"):
                ports.prepare(self.command)

    def test_equivalents_are_reported_and_never_skipped(self):
        self.listing.stdout += "\n" + "\n".join(
            name + ": test" for name in ports.EQUIVALENT_TESTS)
        with patch.dict(os.environ, {}, clear=True), \
                patch.object(ports, "occupied", return_value=True), \
                patch.object(ports.subprocess, "run", return_value=self.listing):
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                command = ports.prepare(self.command)
        skipped_args = command[len(self.command):]
        for name in ports.EQUIVALENT_TESTS:
            self.assertNotIn(name, skipped_args,
                             "equivalent tests must never enter the skip list")
        text = output.getvalue()
        self.assertIn(
            f"Port-independent equivalents still executed: "
            f"{len(ports.EQUIVALENT_TESTS)}/{len(ports.EQUIVALENT_TESTS)}", text)
        self.assertIn(f"EQUIV {ports.EQUIVALENT_TESTS[-1]}", text)

    def test_fixed_skip_substring_hitting_an_equivalent_fails_closed(self):
        # 若有人把固定口用例名改成某个等效用例的前缀，--skip 会连带丢弃
        # 等效用例——必须拒绝执行而不是静默降覆盖。
        equivalent = ports.EQUIVALENT_TESTS[-1]
        prefix = equivalent[:len("random_port_serve")]
        self.assertIn(prefix, equivalent, "fixture requires a real prefix relation")
        listing = subprocess.CompletedProcess(
            [], 0, stdout=f"{prefix}: test\n{equivalent}: test\n")
        with patch.object(ports, "FIXED_TESTS", (prefix,)), \
                patch.dict(os.environ, {}, clear=True), \
                patch.object(ports, "occupied", return_value=True), \
                patch.object(ports.subprocess, "run", return_value=listing):
            with self.assertRaisesRegex(SystemExit, "substring of"):
                ports.prepare(self.command)

    def test_equivalent_inventory_matches_the_random_port_test_file(self):
        root = pathlib.Path(__file__).resolve().parents[1]
        import re
        source = (root / "tests/host_random_port_lifecycle.rs").read_text()
        source = source.replace("\r\n", "\n")
        found = re.findall(r"#\[test\]\s*\nfn (\w+)\(", source)
        listed = {name for name in ports.EQUIVALENT_TESTS
                  if name.startswith("random_port_")}
        self.assertEqual(set(found), listed,
                         "EQUIVALENT_TESTS must track every random-port test")

    def test_inventory_matches_all_mutex_users(self):
        root = pathlib.Path(__file__).resolve().parents[1]
        import re
        found = []
        for file in ("tests/serve_lifecycle.rs", "tests/support/browser_launch.rs"):
            source = (root / file).read_text().replace("\r\n", "\n")
            for name in re.findall(r"fn (\w+)\(\) \{\s*let \w+ = FIXED_BACKGROUND_PORT", source):
                found.append(("browser_launch::" if "browser_launch" in file else "") + name)
        self.assertEqual(set(found), set(ports.FIXED_TESTS))


if __name__ == "__main__":
    unittest.main()
