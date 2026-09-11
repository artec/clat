import importlib.util
import pathlib
import subprocess
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "signing", pathlib.Path(__file__).with_name("measure-signing.py"))
signing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(signing)


class TimingTests(unittest.TestCase):
    def test_preserves_signal_exit_and_output(self):
        result = subprocess.CompletedProcess(["probe"], -9, "", "")
        with patch.object(signing.subprocess, "run", return_value=result):
            measured = signing.timed(["probe"])
        self.assertEqual(measured["returncode"], -9)
        self.assertEqual(measured["stdout"], "")

    def test_timeout_cannot_look_like_success(self):
        with patch.object(signing.subprocess, "run",
                          side_effect=subprocess.TimeoutExpired(["probe"], 60)):
            measured = signing.timed(["probe"])
        self.assertTrue(measured["timeout"])
        self.assertNotIn("returncode", measured)


if __name__ == "__main__":
    unittest.main()
