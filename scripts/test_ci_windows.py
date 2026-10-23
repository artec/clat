"""A failing Windows product suite must not be masked by a later Cargo command."""
from pathlib import Path
import re
import unittest


class WindowsGateTests(unittest.TestCase):
    def test_product_and_wasm_tests_are_separate_single_command_steps(self):
        workflow = (Path(__file__).resolve().parent.parent / ".github/workflows/ci.yml").read_text().replace("\r\n", "\n")
        windows = workflow.split("\n  windows:\n", 1)[1].split("\n  plugin-performance:", 1)[0]
        commands = re.findall(r"^        run: (cargo test[^\n]*)$", windows, re.M)
        self.assertEqual(commands, ["cargo test --all-targets --all-features", "cargo test -p clat-wasm-net --lib"])
        # Installer parsing needs a multiline PowerShell step. Cargo suites
        # must still be single-command steps so a later success cannot mask red.
        blocks = re.findall(r"^        run: \|\n((?:          [^\n]*\n)*)", windows, re.M)
        for block in blocks:
            self.assertNotIn("cargo test", block)


class LinuxGateBudgetTests(unittest.TestCase):
    def test_sequential_linux_gate_has_bounded_cold_build_headroom(self):
        workflow = (Path(__file__).resolve().parent.parent / ".github/workflows/ci.yml").read_text().replace("\r\n", "\n")
        step = workflow.split("      - name: Complete Linux gates\n", 1)[1].split("      - name:", 1)[0]
        minutes = int(re.search(r"timeout-minutes: (\d+)", step).group(1))
        self.assertGreaterEqual(minutes, 30, "the full cold-build sequence exceeded 15 minutes")
        self.assertLessEqual(minutes, 45, "the complete gate must remain time bounded")
        self.assertIn("run: scripts/gates.sh --ci", step)


if __name__ == "__main__":
    unittest.main()
