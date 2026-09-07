# CI 盒子镜像——官方钉版 rust 之上补齐 runner 语义（Dockerfile 即决策
# 档案，详见 docs/research/ci-parity.md）：
#   1. python3：gated 假 server 需要（bookworm 基础面不保证带）；
#   2. node 22：gated 的 dsh-adapter e2e 腿 spawn node（CI 的
#      "Build dsh-adapter for the gated e2e" 步骤即为此；bookworm 自带
#      nodejs 18 太旧，用 nodesource 22）；
#   3. 非 root 用户 ci：GitHub runner 是非特权用户——root 会绕过
#      chmod 只读目录，application 的「只读存储目录」类测试假红
#      （2026-09-07 首跑实证：new_session_write_failure_keeps_the_old_
#      session 等 2 腿容器内红、CI 绿）。
# clippy/rustfmt 显式补装（官方镜像按 minimal profile 装，不保证带）。
ARG CHANNEL=1.98.0
FROM rust:${CHANNEL}-bookworm
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 curl ca-certificates gnupg \
    && curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/* \
    && rustup component add clippy rustfmt \
    && useradd --create-home --uid 1000 ci \
    && mkdir /work /target /cargo-cache /npm-cache \
    && chown ci:ci /work /target /cargo-cache /npm-cache
USER ci
ENV HOME=/home/ci
