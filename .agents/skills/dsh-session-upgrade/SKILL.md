---
name: dsh-session-upgrade
description: CLAT 跟随 DSH（deepseek-harness）版本升级的操作手册——会话格式代际（v2→v3→…）、词汇、网关 wire、锁协议的对齐全流程。Use whenever DSH releases a new version or changes its session journal/format/gateway/wire surface, when CLAT fails to read DSH-produced sessions, when planning an alignment task wave, or when implementing a session-format migration (/update-style upgrade).
---

# DSH 版本对齐与会话格式升级手册

本手册源自 CLAT 第十一轮 DSH 0.1.3 对齐实战（2026-09，DV-1..11，
12 份审计）与 0.1.5 预研。完整案例见 `references/case-study-round-11.md`
（v0→v2 剧本）与 `references/case-study-round-12-v3.md`（V3 对齐 +
ensure-current）；两仓源码地图见 `references/repo-map.md`。按阶段执行，勿跳步。

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
  所有后续验证。词汇竞争已发生 5 次（model/selection、team/*、
  feedback/*、system/message、ptc 改名双座位），是每次版本升级
  的固定税。
- **词表是有世代窗口的**（V3 批新增形状）：类型有出生代与退役代
  （如 `system/message` 从 v3 起、旧 PTC 名被 v3 退役）。座位表旁
  落两张窗口表：越代类型走 unknown 通道（ignorable 信封级放行、
  required 拒），退役代 required 拒 / ignorable 不透明——与上游
  per-generation known-event-types 对表，不做成单一布尔。
- 独立易项并行一波；主战役（格式读写）单列；迁移/升级（如有）
  依赖格式读写完成后收尾。**版本常量 bump 与迁移边必须同批落地**
  （V3 教训：写侧已是 v3、转换器还产 v2 → 文件名与头版本自相矛盾）。
- CLAT 侧改动全部走 catalog 座位表（见 AGENTS.md 词汇唯一家），
  不得新开字符串 match 臂。

## 第 3 阶段：实施纪律

- **不变量先行**：动持久状态前写下不变量，审计每个读者和每个
  写者（宪法 State discipline 全文适用）。
- **字节级金样**是安全网：对齐过程中金样零变更是行为保持的
  证据；新格式的金样按 DSH 真实产物铸造（原语级生成脚本）。
- **fixture 铸造脚本随上游版本重验**（V3 教训）：上一代能跑的
  生成脚本会在新代失效——rc.2 把 SessionStore 与
  JsonlSessionPersistence 解耦（B8 剧本的 persistence.load 消失）、
  持久化插件改 default export、store 逻辑头缺
  `delegationDepth`（released 头校验要求在位）。先小探针验证
  API 面，再写生成器；`node:zlib` 一次只解一个 zstd 帧（两帧
  布局要自己拆）。
- **写侧必须满足自家准入（canonical 对称）**：准入层先行的
  严格规则（如空可选字段必须缺省），写侧要在同一处规则家落地
  （V3 的 `persisted_request_header`）——"写出的日志自己的
  admission 要认" 是判别测试，ignored 测试面抓到过真实违反。
- **value 搬家时，所有比较必须迁到新家同形**：V3 把 system 从
  header 搬进消息流，header 去重比较两侧若一侧已剥一侧未剥，
  每 run 假 "change"。定义"持久形"唯一规则函数，写入与比较
  都走它。
- **`--ignored` 测试面是门禁的一部分**：跨读产物测试、私有源
  演练等 ignored 测试在全量门禁才跑，且专抓写侧/准入不对称
  的真 bug（V3 批的空 tools 违规就是它抓到的）。交付前必须
  让它们也绿。
- **真实互锁测试**：`DSH_CHECKOUT=<钉靶路径>` 跑 write_lease
  互锁与 cohort 钉靶（DW-3 后显式 opt-in）。
- **预言机重钉**决策：compat oracle 是否推进到新靶由负责人定。
- **zstd 物理事实**：会话是单一顺序压缩流——尾部读取也要从头
  解压，**不存在也不需要反向读**（0.1.3 调研定案，勿再发明）。
- **形状预算适用于迁移代码**：转换器容易写成百行大函数——
  用状态机/按家族拆分（V3 的 `Migration::push_source`），
  `code-health.py` 长函数数不得超基点。

## 第 4 阶段：会话格式代际迁移（/update = ensure-current）

当需要把低于当前代的会话升到 `SESSION_FORMAT_VERSION` 时，执行
`references/case-study-round-11.md` 的完整剧本，其七条不变量不可裁剪。
要点速记：

- **ensure-current 语义（2026-09-13，V3 批确立）**：`/update` 不是
  "vN→vN+1" 单边命令，而是"升到 `SESSION_FORMAT_VERSION` 常量"——
  一切低于当前代的会话（v0、存量 v2，将来 v3）经**内存迁移链**逐边
  接力，但**只在全链成功后发布唯一最终世代**；任何一边拒绝 = 零发布、
  源字节不动、目录零中间世代残留（DSH "only the exact source and final
  current generation" 规则照抄）。命令对已当前代幂等（no-op）。
- 退役代（如 v1）显式拒绝并指路 /new，绝不静默收下或跳过。
- 旧代**只读打开**（错误信息指路 /update 或 /new），普通路径
  零副作用：不补 end-seed、不修复、不写 checkpoint、不建 writer；
- **bump 会制造"新的旧代"（2026-09-15 缺陷入册）**：常量每进一代，
  上一代就从"可写"翻成"可解码但不可写"。按版本号路由读写状态的
  地方（协调器只读分支、写路门槛、命令可见性）一律用**"凡低于
  `SESSION_FORMAT_VERSION`"**表达，禁止写死具体版本号——只读分支
  写死 `== 0` 的债在 V3 bump 后爆雷：v2 打不开、/update 又只在
  legacy 会话内可达，"打不开就没法 update"死锁（合并后实机发现）。
- **结构性改名会从宿主的"事件服务面"漏进客户端 wire 缝（2026-09-15
  缺陷入册）**：新代的信封/字段改名不只存在于 journal 文件——宿主的
  `session/page`、live `session/event` 把各代事件**原样**发给客户端，
  且不带世代标签。任何把 journal 事件当 wire 载荷消费的客户端
  （`clat dsh`、将来桌面端）都要在解析缝做**形状驱动**的信封规范化
  （V3 `startSeq`/`endSeq` → 逻辑 `start`/`end`）。一条解析失败 =
  整页历史被拒 → 转录只剩暂存 live 帧（"打开会话只见最后一条"）。
- 迁移 = 租约内读 → **纯转换器** → 发布前**字节往返验证** →
  源 revision 复验 → **no-overwrite 原子发布** → 目录 fsync →
  **重武装**（世代重排由整体重建吸收，不做水位算术；投影永远由
  当前代全量扫描重建，缓存行不得绕过改变基数的迁移）；
- **原件永久保留**，永不删；
- 无损映射不了的输入明确拒绝，不静默造"兼容"数据；上游冻结边
  保真的字段（turn/step、ids、时间、交付坐标）逐字保留——保留规则
  例外必须写成文档化 deviation（见 V3 案例研究的 step 基裁决）。

### 迁移边施工要点（V2→V3 实战沉淀）

- **上游 oracle 对拍是最强判别**：用与上游迁移器测试同款方法铸造
  一份源（同 header id、同时间锚点），分别喂上游迁移器与本地转换
  器，**逐事件等价**（含合成 id 的 SHA-256 材料）。金样即对拍基准。
- **旧代真实日志可能不被新边接受**：0.1.3 真 V2 日志的头在
  step/start 之前，rc.2 迁移器按规格拒绝（提示词变化在开放 step
  外）。拒绝类要**复刻并钉判别测试**；上游自己的测试铸造法
  （migration.spec.ts 的合成源）是金样源的合法替代。
- **bump 批次的必备判别腿（2026-09-15 补）**：经协调器**真实 resume
  路径**（不是 upgrade 自读）只读打开一个**上一发布代**会话——成功、
  零副作用（源字节不动、无锁/检查点/seed/writer 残留）、写面拒绝
  指路 /update、/update 后同一会话当场可写。ensure-current/金样/
  准入测试全绿不覆盖这条腿（V3 批漏网实录）。
- **重映射只做同名工件引用表**（surface 信封/sourceEventSeqs/
  command.done/compaction Range+Seqs/title messageSeqs），其余一律
  保真；单遍走日志即可——引用只指向前方，走到时映射必已建立，
  无需两遍。
- **serde_json 的 `Value::take()` 是"取值留 Null"**——剥键必须
  `as_object_mut().remove()`，否则 admission 看到 Null 键照旧拒绝。

### 跨 run 的持久状态要 reseed（V3 教训：受保护头）

写侧新引入的跨 run 概念（如受保护 system 头）必须有投影派生的
reseed 路径，否则第二个 run 会重建一份（第二个头 = 唯一性破坏）。
落法：surface 投影派生 `last_system_head()`（首 append 定头、恰罩头
替换移头、非头 append 只供有效文本——DSH findLast(non-empty) 语义），
经请求数据结构带进 recorder。

### 压缩/裁剪与新 surface 节点的相互影响

新 surface 类型改变 shadow span 的几何：事件序里若新节点不在
surface 首位（CLAT 首条 user 先于受保护头），前缀切割的遮蔽区间会
跨过它——上游 restore 明文拒绝跨头 shadow。修法是**写事件处的
clamp**（头之前的前缀节点从遮蔽前缀剔除），绝不为过 fold 而放宽
校验；代价（至多少压一条首消息）如实记档。

### /update 命令面的经验（round-11 + round-12 合并）

- **发布机器与转换器解耦**：字节往返验证、源 revision 复验、
  no-overwrite 硬链接、目录 fsync、失败清理——round-11 建的发布
  序列逐代复用，每代只换转换器。不要为新一代重写发布面。
- **源世代守卫要通用**：不要硬编码"源必须是 v0"（V3 批曾因此
  拒绝存量 v2）——用"当前代幂等 no-op、其余走链"表达。
- **门控借常量之力**：`SESSION_FORMAT_VERSION` bump 后，既有的
  只读判定（可写代 `<` 常量比较）自动把存量当前代降为 legacy，
  `/update` 的可见性门控无需改命令层。
- **幂等 = 与自己比**：目标已存在且重读重编码一致时 no-op（重试
  与二次执行都靠它）。
- **目录级断言零中间残留**：不止断言目标文件存在——列出目录，
  断言没有 `session.vN.*` 之类的中间代残留（链式原子性的直接
  证据）。
- **错误信息可行动**：退役代指路 /new；活跃 writer 指路先关闭；
  撕裂尾拒绝而非丢弃。

## 第 5 阶段：审计验收

- 前置红测试 + 变异抽查（删实现臂必红，恢复必绿）；
- **迁移边的上游 oracle 对拍**（存在上游迁移器时：同输入逐事件
  等价，V3 批建立的最强判别）；
- 测试数对账（重构/拆包不得丢测试）；
- **形状预算复查**（`code-health.py` 长函数数与根占比不劣于基点）；
- 全脸 `gates.sh --full`（含 `--ignored` 面）+ 互锁腿；
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
7. "已知闪测"标签挂账不查——红两次必开病历（typert 案）；
8. 版本 bump 只改常量不动转换器 → 写侧文件名与头版本自相矛盾
   （V3 第一轮全量门禁红）；bump 与迁移链必须同批；
9. `serde_json::Value::take()` 当 `remove` 用 → 键留 Null，
   自家 admission 照拒（V3 转换器首跑红）；
10. 上一代的 fixture 生成脚本直接复用 → rc.2 解耦了 store 与
    persistence、插件改 default export、B8 剧本失灵——API 面先
    探针后使用；
11. 把 0.1.3 真实日志当 rc.2 迁移器的输入 → 头在 step 外被规格
    拒绝；金样源按上游测试铸造法造，真实日志的拒绝类另钉测试；
12. 忽略 `--ignored` 测试面 → 写侧/准入不对称的真 bug（空 tools
    违规）漏到全量门禁才炸；ignored 面交付前必绿。
13. 只读路由写死旧代版本号（`version == 0`）→ bump 后新旧代打不开、
    /update 不可达，死锁（2026-09-15 实机发现）；条件用
    `< SESSION_FORMAT_VERSION`，测试带"旧代只读 resume → /update
    可写"腿。
14. 新代信封改名只对齐 journal 读侧、漏掉 dsh 客户端 wire 缝 →
    宿主 `session/page`/live 帧里一条 V3 形替换事件让整页
    "invalid history event"，TUI 只剩最后一条消息（2026-09-15
    实机发现）；wire 无世代标签，按形状在解析缝规范化
    （`dsh::frames::canonicalize_wire_event`）。

## 关键落点速查（详见 references/repo-map.md）

| 面 | CLAT | DSH |
|---|---|---|
| 词汇唯一家 | `src/session/catalog/`（座位表） | `packages/core/session` 各单元 |
| 格式常量/世代 | `src/session/compat.rs` | `session-format/src/`（迁移链） |
| v2→v3 迁移边 | `src/session/upgrade/v2_to_v3.rs` + `upgrade::ensure_current` | `session-format-v2-to-v3/src/` |
| 读路径 | `persistence.rs`（visit_from 顺序流） | `cold-read.ts`/`storage.ts` |
| 锁 | `write_lease.rs`（互锁测试） | `lease.ts` |
| /update（ensure-current） | `persistence.rs::upgrade_legacy` + `use_cases::upgrade_active` | 各 `session-format-vN-to-vM/` + persistence 发布 |
| 网关客户端 | `src/dsh/`（backend/mux/client） | `packages/api/gateway/` |
| 兼容金样 | `dsh_golden.rs` + `tests/fixtures/dsh-session/` | 各包 spec 测试 |
