# 两仓源码地图（会话栈对齐用）

> CLAT 侧按当前布局（2026-09 重构后）；DSH 侧钉靶以调研文档
> 标注为准。升级时先核对本表文件是否仍在原位（两仓都在演进）。

## CLAT（本仓库）

| 职责 | 位置 | 要点 |
|---|---|---|
| 词汇唯一家 | `src/session/catalog.rs` + `catalog/`（validation.rs、run_events.rs） | 宏表一行=验证器+surface+ReplayKind+退役位；recorder/wire 投影同表生成 |
| 格式版本/世代 | `src/session/compat.rs` | `SESSION_FORMAT_VERSION`；世代文件名解析（v0 无后缀；正数 `session.vN`） |
| 读路径 | `src/session/persistence.rs` | `visit_from(key, from_seq, visitor)` 顺序流式；`read_events` 全量物化（带预算帽） |
| 武装/回放 | `src/session/use_cases.rs`（+ `use_cases/` 家族目录） | `arm_session`（单遍全量喂投影+replay+usage）；`history_active_with_usage`（尾窗分页）；`upgrade_active`（迁移重武装） |
| 跨进程锁 | `src/session/write_lease.rs` | flock+inode 复验；测试支持真 DSH 互锁（`DSH_CHECKOUT` opt-in） |
| 迁移 | `src/session/upgrade.rs` + persistence `upgrade_legacy` | 纯转换器；no-overwrite 硬链接发布；剧本见 case-study |
| 兼容金样 | `src/session/dsh_golden.rs` + `tests/fixtures/dsh-session/` | 字节级装载/重放断言；`.gitattributes` 已有通配钉 |
| 网关客户端 | `src/dsh/`（backend/mux/client/connect） | Typert 斜杠方法族；连接池已禁用（勿再启用） |
| 投影 | `src/session/projection.rs` | checkpoint_bounded 跳过无界单元；outline 有界可 checkpoint |

## DSH（平行目录 deepseek-harness）

| 职责 | 位置 | 要点 |
|---|---|---|
| 格式迁移链 | `packages/session/session-format*/` | `currentVersion`（generated.ts）；每代一个 v(N-1)-to-vN 包，有状态流式 Stage |
| 冷读 | `session-query/session-query/src/`（observation/cold-read） | 全量读入+LRU(5)+memo(2)；"never wrong, only possibly stale" 教义 |
| 世代发布 | `session-persistence-jsonl/src/generation.ts` | whole-generation：世代文件=完整日志，非增量段 |
| 锁 | 同包 `lease.ts` | CLAT write_lease 与之互锁（协议跨代未变，0.1.5 实证） |
| 网关 | `packages/api/gateway/` | 信封 client-request/server-response；`session/follow`（尾页快照+活流）/`session/page`（beforeSeq 游标，maxMessages 默认 50） |
| 投影缓存 | `packages/session/session-projection-cache/` | spec.ts:22 教义行；hydrate 需完整日志先行（省算不省读） |
| 性能门 | `benchmarks/` + 2026-09-04 纪事 | 会话打开预算 CI 门；575K-chunk 26s 事故史 |
| 发布策略 | `.agents/notes/implemented/architecture/` | 迁移/格式决策纪事（含 whole-artifact 内存爆炸教训） |

## 版本事实查询

- DSH 版本：平行目录 `package.json` + `git tag`；
- **npm dist-tags**：`npm view @deepseek-ai/dsh dist-tags`——
  `latest` ≠ GitHub HEAD（alpha 走 `alpha` tag）；
- CLAT 侧影响面速查：`codegraph impact <symbol>` 或
  `grep -rn "<event-type>" src/session/catalog.rs`。
