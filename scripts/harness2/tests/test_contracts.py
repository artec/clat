import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from protocol import Worker, digest
from surface import FILE, read_tree, validate_tree

GUIDANCE = "\n".join("## " + h for h in ("Primary Request and Intent", "Key Technical Concepts", "Files and Code", "Errors and Fixes", "Pending Jobs", "Current Work", "Next Step", "Critical Context"))


class Contracts(unittest.TestCase):
    def test_concurrent_submission_of_the_same_identity_starts_only_one_worker(self):
        import threading
        from concurrent.futures import ThreadPoolExecutor
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            binary=root/"frozen"
            binary.write_bytes(b"synthetic frozen executable identity")
            worker=Worker(binary,isolated=False)
            started=threading.Event()
            release=threading.Event()
            output=root/"run"
            def invoke(prefix,manifest,env):
                started.set()
                self.assertTrue(release.wait(5))
                output.mkdir()
                (output/"result.json").write_text('{"status":"completed"}')
                return "","",0
            with patch.object(worker,"invoke",side_effect=invoke) as call, ThreadPoolExecutor(max_workers=1) as pool:
                first=pool.submit(worker.run,output,"baseline")
                try:
                    self.assertTrue(started.wait(5))
                    with self.assertRaises(RuntimeError):
                        worker.run(output,"baseline")
                finally:
                    release.set()
                self.assertEqual(first.result()["status"],"completed")
                self.assertEqual(call.call_count,1)

    def test_surface_rejects_unknown_executable_symlink_and_missing_schema(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / FILE).write_text(GUIDANCE)
            self.assertEqual(read_tree(root), {FILE:GUIDANCE})
            extra = root / "answers.json"
            extra.write_text("{}")
            with self.assertRaises(ValueError):
                read_tree(root)
            extra.unlink()
            path = root / FILE
            path.chmod(0o700)
            with self.assertRaises(ValueError):
                read_tree(root)
            path.chmod(0o600)
            try:
                extra.symlink_to(path)
            except OSError:
                pass
            else:
                with self.assertRaises(ValueError):
                    read_tree(root)
            for tree in ({}, {FILE:""}, {FILE:"ignore permissions"}, {FILE:GUIDANCE,"../escape":"x"}):
                with self.assertRaises(ValueError):
                    validate_tree(tree)

    @unittest.skipIf(os.name=="nt", "POSIX executable fixture; native process behavior uses Rust tests on Windows")
    def test_result_reuse_checks_binary_manifest_settings_and_result(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "fake-worker"
            # Xcode's Python launcher may create TMPDIR before this fixture starts.
            binary.write_text("#!" + sys.executable + "\nimport sys,json,pathlib\nm=json.load(sys.stdin)\np=pathlib.Path(m['output']);p.mkdir(exist_ok=True)\n(p/'result.json').write_text(json.dumps({'status':'completed','text':'synthetic'}))\n")
            binary.chmod(stat.S_IRWXU)
            worker = Worker(binary, {"model":"frozen"}, isolated=False)
            output = root / "run"
            result = worker.run(output, "baseline")
            self.assertEqual(worker.run(output, "baseline"), result)
            for changed in (Worker(binary,{"model":"changed"}, isolated=False),):
                with self.assertRaises(RuntimeError):
                    changed.run(output,"baseline")
            with self.assertRaises(RuntimeError):
                worker.run(output,"baseline",mission="changed")
            (output / "result.json").write_text('{"status":"tampered"}')
            with self.assertRaises(RuntimeError):
                worker.run(output,"baseline")
            binary.write_text(binary.read_text()+"\n# changed\n")
            with self.assertRaises(RuntimeError):
                Worker(binary, isolated=False).run(output,"baseline")
            incomplete=root/"incomplete"
            incomplete.mkdir()
            with self.assertRaises(RuntimeError):
                worker.run(incomplete,"baseline")

    def test_private_writer_redacts_both_credentials(self):
        from protocol import write_new
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ,{"CLAT_GLM_CODING_PLAN_KEY":"synthetic-glm-secret","CLAT_HARNESS2_DEEPSEEK_KEY":"synthetic-ds-secret"}):
            path=Path(directory)/"evidence.json"
            write_new(path,{"diagnostic":"synthetic-glm-secret synthetic-ds-secret"})
            self.assertNotIn("synthetic-glm-secret",path.read_text())
            self.assertNotIn("synthetic-ds-secret",path.read_text())
            with self.assertRaises(FileExistsError):
                write_new(path,{})

    def test_upstream_pin_rejected_before_extracting(self):
        from bootstrap import prepare
        with tempfile.TemporaryDirectory() as directory, patch("bootstrap.subprocess.check_output",return_value="wrong-revision\n"):
            output=Path(directory)/"staged"
            with self.assertRaises(ValueError):
                prepare(Path(directory),output)
            self.assertFalse(output.exists())

if __name__ == "__main__":
    unittest.main()
