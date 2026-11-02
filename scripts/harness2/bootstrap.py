"""Stage exactly the pinned upstream plus the reviewed CLAT-only extension."""
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tarfile

PIN = "1cfe254e8d25b15f37048c3730134686abe60335"
HERE = Path(__file__).resolve().parent


def tree_identity(root):
    return {str(p.relative_to(root)):hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted((root / "harness2").rglob("*.py"))}


def prepare(source: Path, destination: Path) -> Path:
    actual = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=source, text=True).strip()
    if actual != PIN:
        raise ValueError("Harness2 source revision differs from the pinned revision")
    patch = (HERE / "upstream-clat.patch").read_bytes()
    identity = hashlib.sha256(PIN.encode() + patch).hexdigest()
    if destination.exists():
        marker=json.loads((destination / ".clat-extension").read_text())
        if destination.is_symlink() or (destination / ".env").exists() or marker != dict(extension=identity,tree=tree_identity(destination)):
            raise ValueError("staged upstream identity mismatch")
        return destination
    archive = subprocess.check_output(["git", "archive", PIN], cwd=source)
    destination.mkdir(parents=True)
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        tar.extractall(destination, filter="data")
    subprocess.run(["git", "apply", "--check", str(HERE / "upstream-clat.patch")], cwd=destination, check=True)
    subprocess.run(["git", "apply", str(HERE / "upstream-clat.patch")], cwd=destination, check=True)
    (destination / ".clat-extension").write_text(json.dumps(dict(extension=identity,tree=tree_identity(destination))))
    return destination
