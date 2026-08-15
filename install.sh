#!/usr/bin/env bash
# CLAT installer for macOS and Linux.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/artec/clat/main/install.sh | sh
#
# Detects the OS and architecture, prefers a prebuilt binary from GitHub
# Releases, and falls back to building from source when no release exists
# yet (offering to install the Rust toolchain if cargo is missing).
set -euo pipefail

REPO="artec/clat"
BIN="clat"

info()  { printf '\033[1;36m>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33m!\033[0m %s\n' "$*"; }
fail()  { printf '\033[1;31mx\033[0m %s\n' "$*" >&2; exit 1; }

detect_target() {
  local os arch
  os=$(uname -s)
  arch=$(uname -m)
  case "$os" in
    Darwin)
      case "$arch" in
        arm64|aarch64) echo "aarch64-apple-darwin" ;;
        x86_64)
          # A shell running under Rosetta reports x86_64 on Apple Silicon;
          # prefer the native ARM build in that case.
          if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null)" = "1" ]; then
            echo "aarch64-apple-darwin"
          else
            echo "x86_64-apple-darwin"
          fi ;;
        *) fail "unsupported macOS architecture: $arch" ;;
      esac ;;
    Linux)
      case "$arch" in
        x86_64)  echo "x86_64-unknown-linux-gnu" ;;
        aarch64) echo "aarch64-unknown-linux-gnu" ;;
        *)       fail "unsupported Linux architecture: $arch" ;;
      esac ;;
    *)
      fail "unsupported OS: $os — use install.ps1 on Windows" ;;
  esac
}

install_binary() {
  local file="$1" dest="${HOME}/.local/bin"
  mkdir -p "$dest"
  install -m 755 "$file" "$dest/$BIN"
  info "installed $BIN to $dest"
  case ":$PATH:" in
    *":$dest:"*) ;;
    *) warn "add $dest to your PATH, for example:  export PATH=\"$dest:\$PATH\"" ;;
  esac
}

# Returns 0 and installs the prebuilt binary, or 1 when no release asset
# exists for this target.
install_from_release() {
  local target="$1" tag asset url tmp
  tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1) || return 1
  [ -n "$tag" ] || return 1
  asset="clat-${tag}-${target}.tar.gz"
  url="https://github.com/$REPO/releases/download/${tag}/${asset}"
  tmp=$(mktemp -d)
  info "downloading $url"
  if ! curl -fsSL -o "$tmp/$asset" "$url"; then
    rm -rf "$tmp"
    return 1
  fi
  if ! tar -xzf "$tmp/$asset" -C "$tmp"; then
    rm -rf "$tmp"
    return 1
  fi
  install_binary "$tmp/$BIN"
  rm -rf "$tmp"
}

install_from_source() {
  if ! command -v cargo >/dev/null 2>&1; then
    info "Rust is required to build from source; installing via rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
  fi
  info "building CLAT from source (takes a few minutes)"
  cargo install --git "https://github.com/$REPO.git" --locked
  info "installed $BIN to $HOME/.cargo/bin"
  case ":$PATH:" in
    *":$HOME/.cargo/bin:"*) ;;
    *) warn "add $HOME/.cargo/bin to your PATH" ;;
  esac
}

main() {
  local target
  info "installing CLAT (github.com/$REPO)"
  target=$(detect_target)
  info "detected target: $target"
  if install_from_release "$target"; then
    info "done"
  else
    warn "no prebuilt release for $target — building from source"
    install_from_source
    info "done"
  fi
}

main "$@"
