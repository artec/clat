"""Private, credential-redacted subprocess contract; no public CLAT interface."""
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
from contextlib import nullcontext

ENTRY = "application::reliability_tests::harness2::harness2_bridge"
KEYS = ("CLAT_GLM_CODING_PLAN_KEY", "CLAT_HARNESS2_DEEPSEEK_KEY")


def driver_hash():
    driver=b"".join(p.name.encode()+p.read_bytes() for p in sorted(Path(__file__).parent.glob("*.py")))
    return hashlib.sha256(driver).hexdigest()


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False).encode()).hexdigest()


def write_new(path, value):
    data = json.dumps(value, ensure_ascii=False, indent=2)
    for name in KEYS:
        if os.environ.get(name):
            data = data.replace(os.environ[name], "[REDACTED]")
    with Path(path).open("x", encoding="utf-8") as file:
        os.chmod(path, 0o600)
        file.write(data + "\n")


class Worker:
    def __init__(self, binary, identity=None, *, isolated=True):
        self.binary = Path(binary).resolve(strict=True)
        self.binary_hash = hashlib.sha256(self.binary.read_bytes()).hexdigest()
        self.driver_hash=driver_hash()
        self.identity = dict(driver=self.driver_hash, **(identity or {}))
        self.isolated = isolated
        self.safety_blocked = False

    def run(self, output, role, **fields):
        if self.safety_blocked:
            raise RuntimeError('execution safety failed or unknown; all model calls stopped')
        output=Path(output).resolve()
        output.parent.mkdir(parents=True,exist_ok=True)
        lock=output.parent / ("."+output.name+".launch-lock")
        try:
            lock.mkdir()
        except FileExistsError:
            raise RuntimeError("another launcher owns this experiment identity") from None
        try:
            result=self._run(output,role,**fields)
            if role=='execute' and result.get('safety_failure') is not False:
                self.safety_blocked=True
                raise RuntimeError('execution safety failed or unknown; all model calls stopped')
            return result
        except BaseException:
            if role=='execute':
                self.safety_blocked=True
            raise
        finally:
            lock.rmdir()

    def _run(self, output, role, **fields):
        if hashlib.sha256(self.binary.read_bytes()).hexdigest()!=self.binary_hash:
            raise RuntimeError("frozen executable changed")
        if driver_hash()!=self.driver_hash:
            raise RuntimeError("frozen driver changed")
        output = Path(output).resolve()
        manifest = dict(role=role, output=str(output), **fields)
        identity = digest(dict(binary=self.binary_hash, settings=self.identity, manifest=manifest))
        if output.exists():
            if output.is_symlink() or not (output / "complete.json").is_file():
                raise RuntimeError("incomplete earlier attempt retained; use a new experiment identity")
            marker = json.loads((output / "complete.json").read_text())
            result = json.loads((output / "result.json").read_text())
            if marker != dict(identity=identity, result=digest(result)):
                raise RuntimeError("result or execution identity changed")
            return result
        if (output.parent / (output.name+".launch.json")).exists():
            raise RuntimeError("earlier startup failure retained; use a new experiment identity")
        output.parent.mkdir(parents=True, exist_ok=True)
        env = {k: os.environ[k] for k in ("PATH", "SYSTEMROOT", "TMPDIR", "SSL_CERT_FILE", "SSL_CERT_DIR") if k in os.environ}
        secret = KEYS[0] if role == "execute" or (role == "probe" and fields.get("family", "").startswith("glm")) else KEYS[1]
        if role not in ("baseline", "offline", "corpus") and secret in os.environ:
            env[secret] = os.environ[secret]
        env.update(CLAT_HARNESS2_LIVE="1", CLAT_LANG="en")
        from isolation import command, HOSTS
        from egress import relay
        env["HOME"] = str(output / "home")
        env["TMPDIR"] = str(output)
        host = HOSTS["glm" if secret == KEYS[0] else "deepseek"]
        with relay(host) if self.isolated and role not in ("baseline","offline","corpus") else nullcontext(None) as port:
            prefix=command(self.binary,output,role,fields.get("family",""),port) if self.isolated else [str(self.binary)]
            if port:
                env["https_proxy"]="http://127.0.0.1:"+str(port)
            stdout,stderr,code=self.invoke(prefix,manifest,env)
        write_new(output.parent / (output.name+".launch.json"),dict(exit_code=code,stdout=stdout,stderr=stderr))
        if output.exists():
            write_new(output / "worker.json", dict(exit_code=code, stdout=stdout, stderr=stderr))
        if code or not (output / "result.json").is_file():
            raise RuntimeError("worker failed; inspect private launch record")
        result = json.loads((output / "result.json").read_text())
        write_new(output / "complete.json", dict(identity=identity, result=digest(result)))
        return result

    def invoke(self,prefix,manifest,env):
        process = subprocess.Popen([*prefix, ENTRY, "--exact", "--ignored", "--nocapture"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env, start_new_session=True)
        try:
            stdout, stderr = process.communicate(json.dumps(manifest, ensure_ascii=False))
        except BaseException:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate()
            raise
        return stdout,stderr,process.returncode
