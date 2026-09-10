#!/usr/bin/env bash
# 本地门禁——逐条镜像 .github/workflows/ci.yml 的 Linux job（check）。
# 纪律（AGENTS.md 工程约束）：ci.yml 步骤变更时同批改本脚本，两处
# diverge 即 bug。目标：本地全绿 ⇒ CI Linux job 绿。
#
# Windows 腿无法本地复刻；涉平台语义（文件锁 / 进程树 / TCP 时序 /
# 低核满载竞争）的改动加跑 scripts/ci-box.sh（Linux 容器复刻）。
# 残余不可收敛项归档于 docs/research/ci-parity.md。
#
# 用法：
#   scripts/gates.sh [FILTER ...]  # 默认快速反馈：相关 Rust 测试（非交付门禁）
#   scripts/gates.sh --plan        # 查看本次自动选择，零编译
#   scripts/gates.sh --full        # 全量镜像：fmt → clippy → rustdoc →
#                                  #   test → adapter build/test → gated
#   scripts/gates.sh --ci          # CI/容器完整 Linux 面，Windows 由独立 job 覆盖
#   scripts/gates.sh --stress N    # 全量后把 Test + Gated 复跑 N 遍
#                                  #   （网络 / 并发 / 时序敏感改动用）
#   scripts/gates.sh --rust-only   # 跳过 node 两步（无 node 环境时；
#                                  #   注意这不是完整的 CI 镜像）
#
# DSH 互锁与 official-cohort 两条外部 checkout 测试默认跳过，避免兄弟
# 目录 pull 后把未钉靶的本机状态混入 CLAT 门禁。显式 opt-in：
#   DSH_CHECKOUT=/absolute/path/to/deepseek-harness scripts/gates.sh --full
# 当前 DSH 0.1.5 checkout 首次运行前需在其根目录执行 `pnpm install`，
# 再执行 `pnpm build:native-system`；具体钉靶 revision 仍由测试断言负责。
set -euo pipefail
cd "$(dirname "$0")/.."

stress=0
rust_only=0
mode=fast
filters=()
while [ $# -gt 0 ]; do
    case "$1" in
        --full) mode=full; shift ;;
        --ci) mode=ci; shift ;;
        --fast) mode=fast; shift ;;
        --stress) stress="${2:?--stress needs a repeat count}"; mode=full; shift 2 ;;
        --rust-only) rust_only=1; mode=full; shift ;;
        --plan) filters+=("--plan"); shift ;;
        --*) echo "unknown flag: $1" >&2; exit 2 ;;
        *) filters+=("$1"); shift ;;
    esac
done

if [ "$mode" = fast ]; then
    exec python3 scripts/test-fast.py "${filters[@]}"
fi
if [ "${#filters[@]}" -ne 0 ]; then
    echo "test filters/--plan require --fast" >&2; exit 2
fi
[[ "$stress" =~ ^[0-9]+$ ]] || { echo "--stress must be a non-negative integer" >&2; exit 2; }

step() { printf '\n\033[1m== %s\033[0m\n' "$1"; }

step "Test selection contract"
python3 -m unittest discover -s scripts -p test_test_fast.py -q

step "Format (cargo fmt --all -- --check)"
cargo fmt --all -- --check

step "Clippy (cargo clippy --all-targets --all-features -- -D warnings)"
cargo clippy --all-targets --all-features -- -D warnings

# Windows 静态面（2026-09-08 负责人裁定「本地绿 ⇒ CI 绿」）：CI 的
# Windows clippy 腿本地原本看不见——cfg 孤儿/dead-code 类两次漏网
# （d922723 / a45c4c5 病历）。xwin 交叉面把该腿搬进本地门禁；判别
# 已验证：重加 #[cfg(unix)] 闸 → 本步骤红出与 CI 同款 dead_code。
# 缺件**硬失败**（软跳过 = 重新打开盲区）。首跑需下 Windows SDK。
if [ "$mode" != ci ]; then
step "Windows static face (cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets)"
if ! cargo xwin --version >/dev/null 2>&1; then
    echo "cargo-xwin 缺席：cargo install cargo-xwin && rustup target add x86_64-pc-windows-msvc" >&2
    exit 1
fi
cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings
fi

step "Rustdoc (RUSTDOCFLAGS=-D warnings cargo doc --no-deps --all-features)"
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features

step "Test (cargo test --all-targets --all-features)"
cargo test --all-targets --all-features

if [ "$rust_only" -eq 0 ]; then
    command -v npm >/dev/null || {
        echo "npm 不在 PATH：装 node 22，或用 --rust-only（非完整镜像）" >&2
        exit 1
    }
    step "Adapter build and tests (npm test includes the build)"
    (cd sdk/dsh-adapter && npm ci && npm test)
else
    echo "--rust-only：跳过 npm 两步——这不是完整的 CI 镜像"
fi

step "Gated tests (cargo test --lib -- --ignored)"
cargo test --lib -- --ignored

i=1
while [ "$i" -le "$stress" ]; do
    step "Stress $i/$stress — Test (时序敏感复跑)"
    cargo test --all-targets --all-features
    step "Stress $i/$stress — Gated"
    cargo test --lib -- --ignored
    i=$((i + 1))
done

if [ "$rust_only" -eq 0 ]; then
    printf '\n\033[1m门禁全绿（CI Linux job 镜像）\033[0m\n'
else
    printf '\n\033[1mRust 门禁全绿（未验证 adapter，不是完整交付）\033[0m\n'
fi
