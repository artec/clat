# 案例研究：会话格式 V3 对齐与 v0/v2→v3 ensure-current（SV 批，2026-09-13）

> 任务书：`docs/todo/session-v3-migration.md`；规格源：DSH 0.1.5-rc.2
> checkout（`c291e7961a`）`packages/session/session-format-v2-to-v3/README.md`。
> 执行账本：`docs/todo/session-v3-execution.md`（决策 D1–D5、禁地账本、
> 门禁日志的权威记录）。

## 钉靶与范围

- DSH `dsh-v0.1.5-rc.2-139-gc291e7961a`（npm latest 仍是 rc.1，按负责人
  裁定以本地 rc.2 为钉靶）。会话格式面：版本常量、头（字段集零变化）、
  词表（+4 类型、PTC 改名）、系统提示词升格为受保护 surface 头、
  canonical 信封（startSeq/endSeq）、request/header 剥 system。
- `clat dsh` 客户端连通（Gateway/鉴权）按 2026-09-12 负责人令跳过。

## V3 结构变化的 CLAT 落点（实施顺序 = 依赖序）

1. **词表先行**（历史税第 5 次如期而至）：catalog 座位 +5
   （system/message=surface 第 4 类型、ptc-dispatch×2、
   deliverables/presented、subagent/catalog）+ **世代窗口**
   （KNOWN_FROM_V3 / RETIRED_BY_V3）：越代 ignorable 走信封级不透明
   通道、required 拒——与上游 `assertV3EventAdmission` 同构。
2. **V3 读**：版本门 0|2|3；信封改名在 Value 层做（逻辑 SurfaceOp
   不动）；native V3 拒绝面（header.system 含空即拒、空 tools/[]
   adapterDefaults/{} 必须缺省、assistant 禁 sourceEventSeqs、
   tool/result data.error ⇒ 单块 isError）。
3. **V3 原生写**：`SESSION_FORMAT_VERSION = 3`；recorder 发受保护头
   （首 step/start 后立即、提示词变化 → 恰罩头的替换）；request/header
   落盘形剥 system（`persisted_request_header` 是唯一规则家，去重比较
   两侧必须同形——形状不一致会引发每 run 假 "change"）。
4. **v2→v3 迁移边**（`src/session/upgrade/v2_to_v3.rs`）：插入空头与
   替换头、同名工件引用重映射、PTC/preset 改名、canonical 化、交付
   守卫、有限 kind 源审计。**决定性测试**：与钉靶迁移器在同一铸造输入
   上逐事件对拍（金样 `v3-migrated-0.1.5.jsonl.zstd`）。
5. **ensure-current**：`upgrade::ensure_current` 链（v0→v2→v3 内存接力）
   + 发布侧原子性（唯一最终世代、零中间残留、幂等、v1 显式拒）。

## 冤枉路与教训（本案新增）

1. **DSH 0.1.3 真 V2 日志不可被 rc.2 迁移器迁移**——其 request/header
   在 step/start 之前，提示词变化发生在开放 step 外（"cannot retain
   source chronology"）。迁移金样的源改用上游 migration.spec.ts 同款
   in-step 铸造；CLAT 转换器复刻该拒绝面（判别测试钉住）。CLAT 自己
   的日志头在 step 内，不受影响。
2. **受保护头跨 run 恢复**：recorder 的头状态若不从投影重seed，第二
   个 run 会追加第二个头（受保护头唯一性破坏）。落地为
   `SessionService::last_system_head()`（SurfaceUnit 派生，不新增
   checkpoint 行）+ RequestHeaderData.system_head。
3. **压缩 span 会跨受保护头**：CLAT 事件序是首条 user 先于头（DSH 的
   头是 surface 节点 0），前缀切割的遮蔽区间必然罩住头。修复 =
   写事件处的头安全 clamp（头之前的前缀节点从遮蔽前缀剔除，至多损失
   一条首消息的压缩覆盖）。不要试图让 fold 容忍跨头——上游 restore
   明文拒绝 "compaction cannot shadow the protected system head"。
4. **serde_json 的 `Value::take()` 是"取值留 Null"**，不是删键——剥
   `header.system` 必须用 `as_object_mut().remove("system")`，否则
   admission 会看到 Null 键照旧拒绝。
5. **Fixture 铸造对拍法**：迁移/原生金样由 `gen-v3-fixtures.mts` 在
   钉靶 checkout 用真实 SessionStore（append 期校验）+ 真实 writer
   编码函数产出；rc.2 把 SessionStore 与 JsonlSessionPersistence 解耦
   （0.1.3 的 B8 剧本不再直接可用），store 快照 + codec 编码是等效
   替代。zstd 金样两帧布局（头帧+正文帧）node:zlib 一次只能解一帧。

## 更多工程教训（写入 SKILL.md 主线，此处存细节）

- **持久形不对称引发每 run 假变更**：header 去重比较一侧用持久投影
  （已剥 system）、一侧用内存新头（未剥）→ 每 run 一个 "change" 头。
  教训：值搬家后，"持久形"要定义成唯一规则函数（`persisted_request_header`），
  写入与所有比较同走；两侧形状不对称是纯逻辑 bug，测试若只盯单侧
  永远发现不了。
- **ignored 测试面抓真 bug**：跨读产物测试只在全量门禁跑——它先暴露
  夹具硬编码 version=2，修复后又暴露写侧持久化空 `tools:[]` 被自家
  准入拒绝。教训：写侧与准入的 canonical 对称要有专门的判别测试
  （`persisted_header_omits_empty_tools`），且 `--ignored` 面交付前必绿。
- **受保护头是跨 run 状态**：recorder 的头簿记若不从投影 reseed，
  第二个 run 追加第二个头。`last_system_head()` 从 SurfaceUnit 派生
  （不新增 checkpoint 行——生产装配从不消费 checkpoint，投影一律
  当前代全量重建，这也是 S5"缓存不绕迁移"的结构性答案）。
- **压缩 clamp**：CLAT 事件序首条 user 先于受保护头，压缩前缀切割
  的 span 必然跨头。写事件处剔除头前节点（auto + manual 两执行点）；
  预试验的"压缩视图过滤"方案被否决回退——它会改变触发时序行为，
  clamp 才是行为保持的修法。
- **协议桩随常量**：测试里的 `journal_version` 硬编码（tui native 桩）
  属于握手判据面，bump 后必须同步——这类一行属 S6 例外，逐行注释
  并在执行记录列账。
- **断言迁移 = 判别测试搬家**：断言 header.system 的测试全部迁到
  system 头面（goal/plan/skills/application 四处）；它们不是放松，
  是把"旧位置无此值"这一新事实钉进测试。
- **场景金样双列表**：agent_eval 的 expected 定义在 scenario.json
  （gate 输入）、观测金样在 expected-report.json（顶层 + expected 块
  两处 durable_event_kinds）——事件族变化要三处同步，且用整行匹配
  做插入（子串 replace 会撞缩进）。

## 文档化 deviation（审计重点，均有判别测试或兼容矩阵行）

- **step 编号基**：V3 原生写侧转 1 基（对齐 DSH 全代际）；**迁移边
  逐字保真**（上游规则），CLAT 产旧日志迁移后仍 0 基——DSH restore
  拒绝 CLAT 产日志（V2 起既存偏差，见 dsh-compat.md 行）。
- **seeded v2 源**拒绝迁移（inherited-cut 机制不复刻）——判别测试
  `seeded_v2_sources_refuse_migration`（转换器单元）+
  `seeded_v2_generation_refuses_update_without_publication`（/update
  端到端：源字节不动、零发布）；兼容矩阵行
  "Seeded-session migration"。
- **成员级 exact-key 审计**不复刻（CLAT 自有扩展本就是 unaudited
  members）；有限 content-kind 集审计照抄。
- **模型请求路径不变**：提示词的 journal 表示变了（system/message 面），
  但 CLAT 仍经 `ModelRequest.instructions` 供词——surface→ModelItem
  适配器跳过 system 节点（压缩 shadow 因此天然不碰头）。

## 交付后缺陷（2026-09-15，合并后负责人实机发现）

- **v2 会话打不开 → /update 不可达，死锁**：协调器只读路由写死
  `version == 0`（round-11 时代 v0 是唯一旧代的债）；bump 制造的
  "可解码但不可写" v2 状态无路可走，resume 被写路门槛拒绝，而
  /update 只在已打开的 legacy 会话内可用。修复 = 路由条件泛化为
  "凡低于 `SESSION_FORMAT_VERSION`" + 必备判别腿（旧代只读
  resume → /update 可写，pre-fix 红在生产报错原文）。全记录见任务书
  `session-v3-migration.md` §5；已入 SKILL.md 第 4 阶段与冤枉路 13。
  教训：bump 的自查面不止"常量传播三点"（S5），还有"按版本号路由的
  状态机是否笼统覆盖了全部旧代"。
