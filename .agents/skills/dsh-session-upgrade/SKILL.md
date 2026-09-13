---
name: dsh-session-upgrade
description: CLAT 跟随 DSH（deepseek-harness）版本升级的操作手册——会话格式代际（v2→v3→…）、词汇、网关 wire、锁协议的对齐全流程。Use whenever DSH releases a new version or changes its session journal/format/gateway/wire surface, when CLAT fails to read DSH-produced sessions, when planning an alignment task wave, or when implementing a session-format migration (/update-style upgrade).
---

# DSH 版本对齐与会话格式升级手册

本手册源自 CLAT 第十一轮 DSH 0.1.3 对齐实战（2026-09，DV-1..11，
12 份审计）与 0.1.5 预研。完整案例见 `references/case-study-round-11.md`；
两仓源码地图见 `references/repo-map.md`。按阶段执行，勿跳步。

## 第 0 阶段：版本事实核查（动手前必做）

1. **钉靶**：DSH 平行目录 checkout 到新 tag/commit，记录哈希。
   所有调研结论标注钉靶。
2. **查 npm dist-tags，不要只看 GitHub**——DSH 的 alpha 只发
   `alpha` tag，`latest` 可能落后多代；"用户实际拿到的版本"以
   dist-tags 为准（2026-09-10 教训：GitHub 已 0.1.5，latest 仍
   0.1.2-rc.1，对齐时机由此定）。
3. **四个面分别核对，不要混为一谈**：
   - **会话格式**：`packages/session/session-format*/` 的
     `currentVersion`（每代一格的迁移链）；世代文件是**全量整档**
     （whole-generation publication），无尾部段文件；
   - **词汇**：新 durable 事件类型（required-on-read 一律要收编）；
   - **网关 wire**：`packages/api/gateway/`（信封/方法/帧形状）；
   - **锁协议**：`session-persistence-jsonl/src/lease.ts`。
   每个面独立 diff、独立下结论——格式红了不代表 wire 变了。

## 第 1 阶段：三遍研究法（负责人定的方法论）

1. **第一遍 差异对表**：旧调研档案 vs 新版源码，逐文件 diff；
2. **第二遍 自我纠偏**：核对第一遍产出——凡"没亲眼核实的断言"
   必错率高（历史病例：端点分隔符、末 seq 等式、压缩模型三处
   全错，均为转述未实证）；
3. **第三遍 全新初读**：抛开旧档案，直接读新版文档+源码形成
   平台图，再回头看旧结论哪些过期。
每遍产出独立文档，错误以带日期勘误形式留档，不删原文。

## 第 2 阶段：任务分解（波次，按依赖序）

- **词汇先行**：新事件类型必须最先收编（catalog 座位表一行+
  金样）——准入层 fail-closed，不收编则新日志整个拒载，阻塞
  所有后续验证。词汇竞争已发生 4 次（model/selection、team/*、
  feedback/*、system/message），是每次版本升级的固定税。
- 独立易项并行一波；主战役（格式读写）单列；迁移/升级（如有）
  依赖格式读写完成后收尾。
- CLAT 侧改动全部走 catalog 座位表（见 AGENTS.md 词汇唯一家），
  不得新开字符串 match 臂。

## 第 3 阶段：实施纪律

- **不变量先行**：动持久状态前写下不变量，审计每个读者和每个
  写者（宪法 State discipline 全文适用）。
- **字节级金样**是安全网：对齐过程中金样零变更是行为保持的
  证据；新格式的金样按 DSH 真实产物铸造（原语级生成脚本）。
- **真实互锁测试**：`DSH_CHECKOUT=<钉靶路径>` 跑 write_lease
  互锁与 cohort 钉靶（DW-3 后显式 opt-in）。
- **预言机重钉**决策：compat oracle 是否推进到新靶由负责人定。
- **zstd 物理事实**：会话是单一顺序压缩流——尾部读取也要从头
  解压，**不存在也不需要反向读**（0.1.3 调研定案，勿再发明）。

## 第 4 阶段：会话格式代际迁移（/update 类升级）

当需要对旧代资产升级时（负责人显式授权才做，零用户期间默认
不做），执行 `references/case-study-round-11.md` 的完整剧本，其
七条不变量不可裁剪。要点速记：

- 旧代**只读打开**（错误信息指路 /update 或 /new），普通路径
  零副作用：不补 end-seed、不修复、不写 checkpoint、不建 writer；
- 迁移 = 租约内读 → **纯转换器** → 发布前**字节往返验证** →
  源 revision 复验 → **no-overwrite 硬链接原子发布** → 目录
  fsync → **重武装**（世代重排由整体重建吸收，不做水位算术）；
- **原件永久保留**，永不删；
- 无损映射不了的输入明确拒绝，不静默造"兼容"数据。

## 第 5 阶段：审计验收

- 前置红测试 + 变异抽查（删实现臂必红，恢复必绿）；
- 测试数对账（重构/拆包不得丢测试）；
- 全脸 `gates.sh --full` + 互锁腿；
- 真实旧资产副本演练（不改用户原数据）。

## 冤枉路清单（前人已走过，勿重复）

1. 只看 GitHub 版本不看 npm dist-tags → 对齐时机误判；
2. 把 DSH 的压缩模型（世代重写）安到 CLAT 头上（append-only）
   ——两库压缩语义不同，游标/缓存语义推导前先确认自己在哪边；
3. 为懒加载发明反向 zstd 读取——物理不存在；
4. 新词汇直接在 recorder/replay/wire 加 match 臂——违反词汇
   唯一家，走 catalog；
5. 迁移时信任中间状态、跳过字节往返验证——v0 时代唯一"质量
   最高件"的称号来自七条不变量一条不缺；
6. 单样本计时/单次运行下结论——多次采样净状态（宪法测量纪律）；
7. "已知闪测"标签挂账不查——红两次必开病历（typert 案）。

## 关键落点速查（详见 references/repo-map.md）

| 面 | CLAT | DSH |
|---|---|---|
| 词汇唯一家 | `src/session/catalog/`（座位表） | `packages/core/session` 各单元 |
| 格式常量/世代 | `src/session/compat.rs` | `session-format/src/`（迁移链） |
| 读路径 | `persistence.rs`（visit_from 顺序流） | `cold-read.ts`/`storage.ts` |
| 锁 | `write_lease.rs`（互锁测试） | `lease.ts` |
| 迁移 | `upgrade.rs` + `use_cases::upgrade_active` | `session-format-v2-to-v3/` 等 |
| 网关客户端 | `src/dsh/`（backend/mux/client） | `packages/api/gateway/` |
| 兼容金样 | `dsh_golden.rs` + `tests/fixtures/dsh-session/` | 各包 spec 测试 |
