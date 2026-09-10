#!/usr/bin/env bash
# Linux CI 盒子——在容器里**完整复刻** ci.yml 的 Linux job，收敛两层
# 本地复刻不了的环境差异（决议档案 docs/research/ci-parity.md）：
#   1. macOS ↔ Linux 平台语义（flock 文件锁、进程树、TCP 栈行为）
#   2. 低核满载竞争——本地多核低载下 connect 分段竞态那类测试几乎
#      必绿，runner 少核满载下必现概率大增（2026-09-07 两轮 CI 红）
#
# 形态：
#   - scripts/ci-box/Dockerfile：官方 rust:<rust-toolchain.toml 钉版>-
#     bookworm + python3 + node 22 + 非 root 用户 ci（root 会绕过 chmod
#     只读目录制造假红；GitHub runner 是非特权用户——2026-09-07 首跑
#     实证后定形）
#   - 默认 --cpus=2 模拟 runner 竞争；--cpus N 可调
#   - 仓库只读挂载，源码拷进容器可写层跑（不污染工作区）；cargo
#     registry / npm cache / target 缓存在 named volume（二次起跑快）
#   - Windows 腿仍无法本地复刻（历史用 cargo xwin check 另行）
set -euo pipefail
cd "$(dirname "$0")/.."

cpus=2
while [ $# -gt 0 ]; do
    case "$1" in
        --cpus) cpus="${2:?--cpus needs a count}"; shift 2 ;;
        *) echo "unknown flag: $1" >&2; exit 2 ;;
    esac
done

toolchain=$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)
[ -n "$toolchain" ] || { echo "cannot read channel from rust-toolchain.toml" >&2; exit 1; }

image=clat-ci-box:${toolchain}
echo "== Linux CI 盒子：${image}（rust:${toolchain}-bookworm 基座），--cpus=${cpus}"
docker build -q -f scripts/ci-box/Dockerfile --build-arg CHANNEL="$toolchain" -t "$image" scripts/ci-box >/dev/null

docker run --rm --cpus "$cpus" \
    -v "$PWD":/src:ro \
    -v clat-ci-box-cargo:/cargo-cache \
    -v clat-ci-box-npm:/npm-cache \
    -v clat-ci-box-target:/target \
    -e CARGO_HOME=/cargo-cache \
    -e npm_config_cache=/npm-cache \
    -w /work \
    "$image" \
    bash -c '
        set -euo pipefail
        # 源码拷进可写层：剔除宿主 target/.git/node_modules（大且平台不符）
        tar -C /src --exclude=./target --exclude=./.git --exclude=node_modules -cf - . | tar -xf -
        export CARGO_TARGET_DIR=/target
        scripts/gates.sh --ci
    '
