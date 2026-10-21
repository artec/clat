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
        self.assertNotIn("run: |", windows)


if __name__ == "__main__":
    unittest.main()
