#!/usr/bin/env bash
# Sync the uncommitted source into a separate native Windows test checkout.
# Never changes Defender, guest security policy, or the owner's C:\clat tree.
set -euo pipefail
cd "$(dirname "$0")/.."
vm="${CLAT_WINDOWS_VM:-Windows 11}"
guest="${CLAT_WINDOWS_WORKSPACE:-C:\clat-sandbox-validation}"
shared="${CLAT_WINDOWS_SHARED_SOURCE:-\\\\Mac\\clat}"
output_dir="${CLAT_WINDOWS_OUTPUT:-output/windows-sandbox}"
mkdir -p "$output_dir"
case "$guest" in
  'C:\clat'|'C:\'|'') echo 'Use a separate Windows validation directory' >&2; exit 2 ;;
esac
if [[ "$guest$shared" == *'"'* ]]; then
  echo 'Windows paths must not contain quotes' >&2; exit 2
fi
mode="${1:-sandbox}"
case "$mode" in
  sandbox) command='cargo.exe test --locked --offline --test windows_sandbox -- --nocapture' ;;
  full) command='cargo.exe clippy --locked --offline --all-targets --all-features -- -D warnings && cargo.exe test --locked --offline --all-targets --all-features' ;;
  sync) command='' ;;
  *) echo 'Usage: scripts/windows-vm.sh [sync|sandbox|full]' >&2; exit 2 ;;
esac
prlctl exec "$vm" cmd /c \
  "robocopy \"$shared\" \"$guest\" /E /XD target .git node_modules .codegraph output .release-secrets /XF *.log /R:1 /W:1 > NUL & if errorlevel 8 exit /b 1" \
  > "$output_dir/sync.log" 2>&1
if [[ -z "$command" ]]; then exit 0; fi
# This is the installed guest toolchain's profile, not the prlctl service user.
prefix='set RUSTUP_HOME=C:\Users\deng\.rustup&& set CARGO_HOME=C:\Users\deng\.cargo&& set PATH=C:\Users\deng\.cargo\bin;C:\Program Files\Git\usr\bin;C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\Llvm\bin;%PATH%&& '
prlctl exec "$vm" --current-user cmd /c "$prefix cd /d \"$guest\" && $command" \
  > "$output_dir/$mode.log" 2>&1
cat "$output_dir/$mode.log"
