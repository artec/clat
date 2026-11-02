"""macOS experiment confinement; fail closed on unsupported platforms."""
import json
from pathlib import Path
import sys

HOSTS = {"glm":"open.bigmodel.cn", "deepseek":"api.deepseek.com"}


def command(binary, output, role, family, port=None):
    if sys.platform != "darwin":
        raise RuntimeError("this launcher requires macOS Seatbelt; Linux requires an externally confined runner")
    profile = ["(version 1)", "(allow default)", "(deny network*)", "(deny file-write*)",
        "(deny file-read* (subpath " + json.dumps(str(Path.home())) + "))",
        "(allow file-read* (literal " + json.dumps(str(binary)) + "))",
        "(allow file-read* file-write* (subpath " + json.dumps(str(output)) + "))",
        '(allow file-read* file-write* (literal "/dev/null"))']
    for parent in sorted(set(output.parents) | set(binary.parents)):
        profile.append("(allow file-read* (literal " + json.dumps(str(parent)) + "))")
    if port is not None:
        profile.append('(allow network-outbound (remote tcp "localhost:' + str(port) + '"))')
    return ["/usr/bin/sandbox-exec","-p","\n".join(profile),str(binary)]
