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

# BEGIN GENERATED LOCALES
# Generated from locales/*.json by bun scripts/i18n-installers.ts. Do not edit.
text() {
  local key="$1" value name parameter prefix tail index found
  shift
  local -a parameters=("$@")
  case "$clat_install_locale:$key" in
    en:installer.arch) value='unsupported {platform} architecture: {arch}' ;;
    en:installer.archive_binary) value='release archive contains links or did not contain a regular {bin} file' ;;
    en:installer.archive_inspect) value='cannot inspect release archive {asset}' ;;
    en:installer.archive_link_entry) value='release archive contains a link entry' ;;
    en:installer.archive_links) value='cannot inspect release archive links for {asset}' ;;
    en:installer.archive_path) value='unsafe path in release archive: {entry}' ;;
    en:installer.build) value='building CLAT from source (takes a few minutes)' ;;
    en:installer.checksum_invalid) value='invalid checksum file for {asset} — aborting' ;;
    en:installer.checksum_mismatch) value='checksum mismatch for {asset} — aborting' ;;
    en:installer.checksum_missing) value='required checksum unavailable for {asset} — aborting' ;;
    en:installer.checksum_verify) value='verifying checksum' ;;
    en:installer.done) value='done' ;;
    en:installer.download) value='downloading {url}' ;;
    en:installer.installed) value='installed {bin} to {dest}' ;;
    en:installer.installing) value='installing CLAT (github.com/{repo})' ;;
    en:installer.language_invalid) value='unsupported installer language: {value}; use system|en|zh-Hans|zh-Hant' ;;
    en:installer.os) value='unsupported OS: {os} — use install.ps1 on Windows' ;;
    en:installer.path) value='add {dest} to your PATH, for example:  {command}' ;;
    en:installer.path_simple) value='add {dest} to your PATH' ;;
    en:installer.rust) value='Rust is required to build from source; installing via {installer}' ;;
    en:installer.source_fallback) value='no prebuilt release for {target} — building from source' ;;
    en:installer.target) value='detected target: {target}' ;;
    en:installer.usage) value='usage: install.sh [--lang system|en|zh-Hans|zh-Hant]' ;;
    zh-Hans:installer.arch) value='不支持的 {platform} 架构：{arch}' ;;
    zh-Hans:installer.archive_binary) value='发布归档包含链接，或没有常规 {bin} 文件' ;;
    zh-Hans:installer.archive_inspect) value='无法检查发布归档 {asset}' ;;
    zh-Hans:installer.archive_link_entry) value='发布归档中含有链接项' ;;
    zh-Hans:installer.archive_links) value='无法检查 {asset} 归档中的链接' ;;
    zh-Hans:installer.archive_path) value='发布归档中包含不安全路径：{entry}' ;;
    zh-Hans:installer.build) value='正在从源码构建 CLAT（需要几分钟）' ;;
    zh-Hans:installer.checksum_invalid) value='{asset} 校验文件无效，已中止' ;;
    zh-Hans:installer.checksum_mismatch) value='{asset} 校验和不匹配，已中止' ;;
    zh-Hans:installer.checksum_missing) value='找不到 {asset} 所需校验和，已中止' ;;
    zh-Hans:installer.checksum_verify) value='正在验证校验和' ;;
    zh-Hans:installer.done) value='完成' ;;
    zh-Hans:installer.download) value='正在下载 {url}' ;;
    zh-Hans:installer.installed) value='已将 {bin} 安装到 {dest}' ;;
    zh-Hans:installer.installing) value='正在安装 CLAT（github.com/{repo}）' ;;
    zh-Hans:installer.language_invalid) value='不支持的安装语言：{value}；可选 system|en|zh-Hans|zh-Hant' ;;
    zh-Hans:installer.os) value='不支持的系统：{os}，Windows 请使用 install.ps1' ;;
    zh-Hans:installer.path) value='请将 {dest} 添加到 PATH，例如：{command}' ;;
    zh-Hans:installer.path_simple) value='请将 {dest} 添加到 PATH' ;;
    zh-Hans:installer.rust) value='源码构建需要 Rust，正在通过 {installer} 安装' ;;
    zh-Hans:installer.source_fallback) value='没有 {target} 预编译版本，将从源码构建' ;;
    zh-Hans:installer.target) value='检测到目标平台：{target}' ;;
    zh-Hans:installer.usage) value='用法：install.sh [--lang system|en|zh-Hans|zh-Hant]' ;;
    zh-Hant:installer.arch) value='不支援的 {platform} 架構：{arch}' ;;
    zh-Hant:installer.archive_binary) value='發佈封存檔包含連結，或沒有一般 {bin} 檔案' ;;
    zh-Hant:installer.archive_inspect) value='無法檢查發佈封存檔 {asset}' ;;
    zh-Hant:installer.archive_link_entry) value='發佈封存檔中含有連結項' ;;
    zh-Hant:installer.archive_links) value='無法檢查 {asset} 封存檔中的連結' ;;
    zh-Hant:installer.archive_path) value='發佈封存檔中包含不安全路徑：{entry}' ;;
    zh-Hant:installer.build) value='正在從原始碼建置 CLAT（需要幾分鐘）' ;;
    zh-Hant:installer.checksum_invalid) value='{asset} 校驗檔案無效，已中止' ;;
    zh-Hant:installer.checksum_mismatch) value='{asset} 校驗碼不符，已中止' ;;
    zh-Hant:installer.checksum_missing) value='找不到 {asset} 所需校驗碼，已中止' ;;
    zh-Hant:installer.checksum_verify) value='正在驗證校驗碼' ;;
    zh-Hant:installer.done) value='完成' ;;
    zh-Hant:installer.download) value='正在下載 {url}' ;;
    zh-Hant:installer.installed) value='已將 {bin} 安裝至 {dest}' ;;
    zh-Hant:installer.installing) value='正在安裝 CLAT（github.com/{repo}）' ;;
    zh-Hant:installer.language_invalid) value='不支援的安裝語言：{value}；可選 system|en|zh-Hans|zh-Hant' ;;
    zh-Hant:installer.os) value='不支援的系統：{os}，Windows 請使用 install.ps1' ;;
    zh-Hant:installer.path) value='請將 {dest} 加入 PATH，例如：{command}' ;;
    zh-Hant:installer.path_simple) value='請將 {dest} 加入 PATH' ;;
    zh-Hant:installer.rust) value='原始碼建置需要 Rust，正在透過 {installer} 安裝' ;;
    zh-Hant:installer.source_fallback) value='沒有 {target} 預先編譯版本，將從原始碼建置' ;;
    zh-Hant:installer.target) value='偵測到目標平台：{target}' ;;
    zh-Hant:installer.usage) value='用法：install.sh [--lang system|en|zh-Hans|zh-Hant]' ;;
    *) value="$key" ;;
  esac
  while [[ "$value" == *'{'*'}'* ]]; do
    prefix=${value%%\{*}; tail=${value#*\{}; name=${tail%%\}*}
    printf '%s' "$prefix"
    found=0; index=0
    while [ "$index" -lt "${#parameters[@]}" ]; do
      if [ "${parameters[$index]}" = "$name" ]; then
        index=$((index + 1)); printf '%s' "${parameters[$index]}"; found=1; break
      fi
      index=$((index + 2))
    done
    [ "$found" = 1 ] || printf '{%s}' "$name"
    value=${tail#*\}}
  done
  printf '%s' "$value"
}
# END GENERATED LOCALES

resolve_language() {
  local value="$1"
  case "$value" in
    system) value="${LC_ALL:-${LC_MESSAGES:-${LANG:-en}}}" ;;
  esac
  value=$(printf '%s' "$value" | tr '[:upper:]_' '[:lower:]-')
  case "$value" in
    zh*hant*) clat_install_locale=zh-Hant ;;
    zh*hans*) clat_install_locale=zh-Hans ;;
    zh-tw*|zh-hk*|zh-mo*) clat_install_locale=zh-Hant ;;
    zh*) clat_install_locale=zh-Hans ;;
    *) clat_install_locale=en ;;
  esac
}

installer_language() {
  local value="${CLAT_LANG:-system}" explicit=0
  if [ "${1:-}" = --lang ]; then value="${2:-}"; explicit=1; shift; [ $# -eq 0 ] || shift; fi
  resolve_language "$value"
  case "$value" in
    system|en|en-*|zh|zh-Hans|zh-Hant|zh-CN|zh-SG|zh-TW|zh-HK|zh-MO) ;;
    *) if [ "$explicit" = 1 ]; then fail "$(text installer.language_invalid value "$value")"; fi
       warn "$(text installer.language_invalid value "$value")" ;;
  esac
  [ $# -eq 0 ] || fail "$(text installer.usage)"
}

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
        *) fail "$(text installer.arch platform macOS arch "$arch")" ;;
      esac ;;
    Linux)
      case "$arch" in
        x86_64)  echo "x86_64-unknown-linux-gnu" ;;
        aarch64) echo "aarch64-unknown-linux-gnu" ;;
        *)       fail "$(text installer.arch platform Linux arch "$arch")" ;;
      esac ;;
    *)
      fail "$(text installer.os os "$os")" ;;
  esac
}

install_binary() {
  local file="$1" dest="${HOME}/.local/bin"
  mkdir -p "$dest"
  install -m 755 "$file" "$dest/$BIN"
  info "$(text installer.installed bin "$BIN" dest "$dest")"
  case ":$PATH:" in
    *":$dest:"*) ;;
    *) warn "$(text installer.path dest "$dest" command "export PATH=\"$dest:\$PATH\"")" ;;
  esac
}

# Returns 0 and installs the prebuilt binary, or 1 when no release asset
# exists for this target.
install_from_release() {
  local target="$1" tag asset url tmp checksum_url
  tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1) || return 1
  [ -n "$tag" ] || return 1
  asset="clat-${tag}-${target}.tar.gz"
  url="https://github.com/$REPO/releases/download/${tag}/${asset}"
  checksum_url="${url}.sha256"
  tmp=$(mktemp -d)
  info "$(text installer.download url "$url")"
  if ! curl -fsSL -o "$tmp/$asset" "$url"; then
    rm -rf "$tmp"
    return 1
  fi
  # The checksum is mandatory. Missing, malformed, misnamed, and mismatched
  # checksum files all abort instead of silently installing unverified bytes.
  if ! curl -fsSL -o "$tmp/$asset.sha256" "$checksum_url"; then
    rm -rf "$tmp"
    fail "$(text installer.checksum_missing asset "$asset")"
  fi
  info "$(text installer.checksum_verify)"
  local expected published_name extra actual entries entry first link checksum_lines verbose
  checksum_lines=$(awk 'NF { count++ } END { print count + 0 }' "$tmp/$asset.sha256")
  if [ "$checksum_lines" -ne 1 ]; then
    rm -rf "$tmp"
    fail "$(text installer.checksum_invalid asset "$asset")"
  fi
  read -r expected published_name extra < "$tmp/$asset.sha256" || true
  published_name=${published_name#\*}
  case "$expected" in
    ""|*[!0-9a-fA-F]*)
      rm -rf "$tmp"
      fail "$(text installer.checksum_invalid asset "$asset")" ;;
  esac
  if [ "${#expected}" -ne 64 ] || [ "$published_name" != "$asset" ] || [ -n "${extra:-}" ]; then
    rm -rf "$tmp"
    fail "$(text installer.checksum_invalid asset "$asset")"
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/$asset")
  else
    actual=$(shasum -a 256 "$tmp/$asset")
  fi
  actual=${actual%% *}
  expected=$(printf '%s' "$expected" | tr '[:upper:]' '[:lower:]')
  if [ "$expected" != "$actual" ]; then
    rm -rf "$tmp"
    fail "$(text installer.checksum_mismatch asset "$asset")"
  fi

  # Validate every archive path before extraction. Reject parent traversal,
  # absolute paths, Windows-style separators, and drive prefixes.
  entries="$tmp/archive.entries"
  if ! tar -tzf "$tmp/$asset" > "$entries"; then
    rm -rf "$tmp"
    fail "$(text installer.archive_inspect asset "$asset")"
  fi
  while IFS= read -r entry; do
    first=${entry%%/*}
    case "$entry" in
      ""|/*|*\\*)
        rm -rf "$tmp"
        fail "$(text installer.archive_path entry "$entry")" ;;
    esac
    case "$first" in
      *:*)
        rm -rf "$tmp"
        fail "$(text installer.archive_path entry "$entry")" ;;
    esac
    case "/$entry/" in
      *"/../"*)
        rm -rf "$tmp"
        fail "$(text installer.archive_path entry "$entry")" ;;
    esac
  done < "$entries"
  verbose="$tmp/archive.verbose"
  if ! tar -tvzf "$tmp/$asset" > "$verbose"; then
    rm -rf "$tmp"
    fail "$(text installer.archive_links asset "$asset")"
  fi
  while IFS= read -r entry; do
    case "$entry" in
      l*|h*|*" link to "*)
        rm -rf "$tmp"
        fail "$(text installer.archive_link_entry)" ;;
    esac
  done < "$verbose"
  if ! tar -xzf "$tmp/$asset" -C "$tmp"; then
    rm -rf "$tmp"
    return 1
  fi
  # A tar archive can carry links away from the extraction root. Reject any
  # link in the extracted tree, not just the final binary path.
  link=$(find "$tmp" -type l -print -quit)
  if [ -n "$link" ] || [ ! -f "$tmp/$BIN" ]; then
    rm -rf "$tmp"
    fail "$(text installer.archive_binary bin "$BIN")"
  fi
  install_binary "$tmp/$BIN"
  rm -rf "$tmp"
}

install_from_source() {
  if ! command -v cargo >/dev/null 2>&1; then
    info "$(text installer.rust installer rustup)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
  fi
  info "$(text installer.build)"
  cargo install --git "https://github.com/$REPO.git" --locked
  info "$(text installer.installed bin "$BIN" dest "$HOME/.cargo/bin")"
  case ":$PATH:" in
    *":$HOME/.cargo/bin:"*) ;;
    *) warn "$(text installer.path_simple dest "$HOME/.cargo/bin")" ;;
  esac
}

main() {
  local target
  installer_language "$@"
  info "$(text installer.installing repo "$REPO")"
  target=$(detect_target)
  info "$(text installer.target target "$target")"
  if install_from_release "$target"; then
    info "$(text installer.done)"
  else
    warn "$(text installer.source_fallback target "$target")"
    install_from_source
    info "$(text installer.done)"
  fi
}

main "$@"
