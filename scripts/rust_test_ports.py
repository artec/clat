"""Explicit local coverage gaps for tests that require the stable host port."""
import errno
import os
import socket
import subprocess
import sys

FIXED_TESTS = (
    "host_spawn_or_attach_requires_trust_and_converges_two_launchers",
    "background_host_refuses_a_non_clat_occupant_on_the_fixed_port",
    "background_host_restart_reuses_the_stable_pwa_port",
    "browser_launch::host_first_start_hands_off_clean_url_once_and_attach_does_not_open",
)


def occupied(port):
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        try:
            probe.bind(("127.0.0.1", port))
        except OSError as error:
            if error.errno == errno.EADDRINUSE:
                return True
            raise
    return False


def prepare(command, ci=False, port=2691):
    # Library-only faces never include serve_lifecycle. Preserve their selection.
    lifecycle = "--all-targets" in command or "serve_lifecycle" in command
    if not lifecycle:
        return command
    if not occupied(port):
        print(f"Fixed-port coverage: 127.0.0.1:{port} free; 0 skipped.", flush=True)
        return command
    if ci or os.environ.get("CI"):
        raise SystemExit(f"CI requires full fixed-port coverage: 127.0.0.1:{port} occupied; "
                         "refusing to skip tests.")
    listed = subprocess.run(command + ["--list"], text=True, capture_output=True,
                            check=True, timeout=120)
    names = [line.removesuffix(": test") for line in listed.stdout.splitlines()
             if line.endswith(": test")]
    skipped = [name for name in FIXED_TESTS if name in names]
    # libtest --skip is a substring filter; never accidentally exclude another test.
    if any(other != name and name in other for name in skipped for other in names):
        raise SystemExit("Ambiguous fixed-port skip filter; refusing partial coverage.")
    print(f"LOCAL COVERAGE GAP: 127.0.0.1:{port} occupied; "
          f"{len(skipped)} fixed-port tests skipped (not passed).", flush=True)
    for name in skipped:
        print(f"  SKIP {name}", flush=True)
    if skipped:
        print("补验：由宿主负责人执行 clat host stop 后重新跑 gates --full，"
              "或运行 scripts/ci-box.sh 补 Linux 面（macOS 专属测试仍需本机空口）；本脚本不会停止在用宿主。", flush=True)
    return command + [arg for name in skipped for arg in ("--skip", name)]


def main():
    args = sys.argv[1:]
    ci = args[:1] == ["--ci"]
    if ci:
        args.pop(0)
    if args[:2] != ["cargo", "test"] or "--" not in args:
        raise SystemExit("usage: rust_test_ports.py [--ci] cargo test OPTIONS -- FILTERS")
    command = prepare(args, ci)
    raise SystemExit(subprocess.run(command).returncode)


if __name__ == "__main__":
    main()
