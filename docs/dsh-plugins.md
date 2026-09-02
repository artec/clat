# DSH 插件兼容与移植

CLAT 把 DeepSeek Harness（DSH）作为插件协议参考实现，但不在 Rust 核心中
内嵌 Node.js 或 JavaScript 引擎。兼容层分成两部分：CLAT 核心提供静态、
带权限与生命周期的插件内核；`@artec/clat-dsh-adapter` 在独立进程中加载
原 DSH 插件，把可移植能力映射为 MCP。

本文当前钉在 DSH `dsh-v0.1.1-rc.2`，源代码提交
`b150a551b8d465e31e418e1b2eaf5e79bbb7d28e`。DSH Web 设置中看到的
147 项是 preset、base bundle 与 Web patch 组装后的插件配置项，不等同于
147 个彼此独立的 npm 包。兼容目标是尽量让这些配置背后的插件原样加载，
不是宣称它们已经全部逐个转换和验收。

## 两层插件内核

| DSH/Cordis 概念 | CLAT 对应物 |
|---|---|
| plugin、`inject`、service | `PluginDescriptor`、依赖规划、typed service registry |
| Fiber 生命周期、effect | scope lease、逆序清理、失败隔离 |
| scoped Context | Global / TrustedProject / Run scope |
| tool registry | `ToolRegistry`、middleware、observer、permission gate |
| system-prompt registry | `PromptRegistry` 与可撤销 contribution |
| 动态 JS 插件 | adapter 子进程，经 MCP `tools` / `prompts` 接入 |
| 动态 Cordis 包子系统 | **不映射**（显式 deliberate deviation，见下） |

Rust 内核有意去掉 Cordis 的运行时猴子补丁、原型链注入、热替换与任意服务
动态重启；其余可静态表达的依赖、所有权、挂载事务、作用域和反向清理语义
由 CLAT 自己持有。这样桌面端、TUI、headless 与未来客户端仍共享同一内核。

**显式偏差（2026-09-02 记档）：动态 Cordis 包子系统不映射。** DSH 的
`packages/extensions/` 四包（`tool-cordis` + host/client runner +
`ui-cordis`）让运行中的模型可以自主检查当前进程的插件/服务、定义动态
Cordis 包（host 半 + 浏览器半）、运行/停止/移除，浏览器面板全程操作；
定义只存活于进程内存。CLAT 不提供任何对位物，理由与上一段同一裁定：
内核不持有运行时动态 JS——进程内 eval 无签名、无能力门，与 CLAT 的
权限/签名市场模型不相容。若未来出现"模型自主定义沙盒插件"的真实病历，
CLAT 的对位路径是 WASM 宿主（fuel/epoch 沙盒、哈希绑定写授予、事务
存储），届时再立项；`ui-cordis` 浏览器半同样无对应物（PWA 是前端，
不是插件宿主）。

## 当前兼容面

| DSH 侧接口 | 兼容行为 | 状态 |
|---|---|---|
| function / `{ apply }` 插件 | 原样调用，`Config` 先校验，返回/yield 的 cleanup 被接管 | 支持 |
| `class Foo extends Service` | 构造、`initHooks`、`Service.init`、generator cleanup | 支持静态生命周期 |
| 静态 `inject` | adapter service 存在则启动；缺少必需 service 时明确拒绝 | 支持 |
| `ctx.get/set/provide`、`ctx.reflect.provide` | 进程内 service 注册、查询、撤销 | 支持单 adapter 作用域 |
| `ctx.inject(deps, callback)` | service 已存在立即接线；返回可 await/dispose 的静态 Fiber；缺失时跳过 | 支持，无动态重启 |
| `ctx.effect` | 立即 setup，支持函数、Promise、同步/异步 generator，多 disposer 逆序清理 | 支持 |
| `ctx.on/once` | disposer 所有权、prepend 与一次性监听 | 支持 |
| `emit/parallel/serial/bail/waterfall` | 对齐 Cordis 调度与 bail 值；parallel 聚合异常 | 支持单作用域 |
| `ctx.tools.register(defineTool(...))` | MCP `tools/list` / `tools/call`，保留 JSON Schema、render 与 structured result | 支持 |
| `ctx.systemPrompt` | section/context/order/complete/variable/tools/change/waterfall | 支持静态组装 |
| `ctx.llm.stream` | MCP `sampling/createMessage`，回适配 dsh-llm chunks | 支持文本采样 |
| `ctx.userQuestions.ask` | MCP `elicitation/create` | 支持，有收窄 |
| `ctx.web` search/fetch provider | `web_search` / `web_fetch` 内置工具，保留 provider 选择错误 | 支持 |
| `ctx.clat` | 读取当前 run 的有界上下文；调用宿主 allowlist 工具 | 支持（CLAT 扩展） |
| `ctx.fs` | 经 `read_file` / `list_files` / `write_file` / `edit_file` 投影 DSH FileSystem | 支持，有明确收窄 |
| `ctx.shell` | 经 `run_command` 前台执行 | 支持前台 `resolve` / `run` |
| `ctx.sessions` | 当前 CLAT session 的只读、run 级镜像 | 支持只读镜像 |
| `ctx.agents` | 当前 root agent 的只读镜像 | 支持只读镜像 |
| callable `ctx.logger(name)` | 所有级别写 stderr，stdout 只走协议 | 支持 |

DSH system prompt 通过带 CLAT 标记的 MCP prompt 发布。CLAT 只导入明确
带 `io.artec.clat/dshSystemPrompt` 元数据的 prompt，传入真实项目目录作为
`cwd` 变量，并在首个 run 开始前与 MCP 工具一起冻结 registry。普通 MCP
prompt 不会未经用户选择自动进入系统指令。

当前 CLAT 的 `PromptRegistry` 只接受 system instruction，因此 adapter
会在 MCP 元数据中保留 DSH runtime-context snapshot，但 CLAT 暂不把它作为
独立 user-role snapshot 注入。这是已知的宿主能力缺口，不应把
`ctx.systemPrompt.context()` 写成已端到端等价。

## 仍需原生宿主实现的部分

第二阶段没有把 DSH agent host 的脊柱复制进 JavaScript。CLAT 内核新增了
传输无关的 host contract；DSH adapter 与 Rust/WASM 插件都调用同一个
`PluginHostBridge`。上下文只在活动 run 内存在；stdio adapter 在卸载时会尽力
推送 `null`，权威状态仍是 `context/get`，且所有新宿主调用都会在桥层拒绝旧 run。宿主
工具只开放 `list_files`、`read_file`、`search`、`write_file`、`edit_file`
与 `run_command`，仍依次经过当前 run 的权限策略、项目路径围栏、取消令牌
和 `ToolExecutionPipeline`。`run_command` 现在还经过同一个 run-owned
ProcessService：macOS 使用当前 Seatbelt 策略，其他平台如实报告无强制隔离的
fallback；这不等于 adapter 子进程本身被沙箱化。其中 fs 投影的读写路径都被收紧到当前项目根；
不继承 CLAT agent 原生读工具“显式绝对路径可读”的宽松能力。

以下能力仍不能由 MCP 子进程安全地伪造：

- sessions/agents 的创建、恢复、写入、实时事件流，subagents、agent loop 与 compaction；
- `ctx.shell.start` 后台进程、fs 原子版本 guard、`replaceAll`；
- permission/approval 策略本身与 project fence 的修改权；
- settings、commands、credentials、UI/Web 面板；
- Cordis scope chain、isolate/intercept、动态依赖重启、插件热更新；
- tool pre/post waterfall、并发策略、`finalizeContent` 与完整 presentation；
- agent-scoped prompt shadowing、`toolOrder` 配置和 runtime-context 的角色语义。

插件若把这些服务列为静态必需 `inject`，adapter 会在启动时失败并列出缺失
项；若只通过 `ctx.inject()` 进行可选 UI 接线，缺失时跳过回调并写 stderr。
不能把“成功启动但没有贡献工具或 prompt”理解成该 UI 功能已兼容。

## 最小移植

原插件继续保留自己的 DSH/Cordis 入口，只增加一个 MCP bin：

```ts
// bin/clat.mjs
import { serveClat } from '@artec/clat-dsh-adapter'
import plugin from '../src/index.js'

await serveClat(plugin, {
  name: 'my-plugin',
  version: '1.0.0',
  config: { apiKey: process.env.MY_API_KEY ?? '' },
  toolHints: { my_tool: 'network' },
})
```

CLAT 用户在 `~/.clat/mcp.json` 配置这个入口：

```json
{
  "my-plugin": {
    "command": "node",
    "args": ["/path/to/package/bin/clat.mjs"],
    "env": { "MY_API_KEY": "..." }
  }
}
```

stdout 是 JSON-RPC 专线；诊断只能写 `ctx.logger`、`console.error` 或其他
stderr 通道。包级 API 和双语示例见
[adapter 中文 README](../sdk/dsh-adapter/README.zh.md)。

## 行为收窄

- `apply()` 必须在 MCP 握手超时内结算；CLAT 当前为 10 秒。
- `ctx.llm.stream({ tools })` 被拒绝；MCP sampling 没有工具调用面。
- sampling 只支持文本，provider/model 最终由宿主会话决定。
- `stopSequences` 会发送，但当前 CLAT sampling bridge 忽略。
- `ask({ agent })` 不支持；multi-select 降为逗号分隔文本。
- 一次 ask 最多 16 问，每问最多 16 个选项。
- `exec.deferContext()` 与 `exec.concludeTurn()` 是带警告的 no-op。
- `web_fetch` 返回 provider 的规范化文本并限制为 100,000 字符；不会复刻
  DSH 完整的 HTML 到 Markdown 清洗管线。
- event 的 context filter、`global` 过滤与 scoped shadowing 在单插件进程内
  没有可观察的多 scope 对象，因此只保留注册、顺序和调度语义。
- `ctx.fs.readText/readBytes` 受宿主 `read_file` 的 64 KiB 完整读取上限；
  超限明确报 `FS_TOO_LARGE`，不会返回假装完整的截断内容。
- `ctx.fs` 的 DSH `expected` 版本 guard 无法由当前原生工具原子表达，因此
  明确报 `FS_GUARD_UNSUPPORTED`；无 guard 的写入仍过 CLAT 权限与路径围栏。
- `ctx.shell` 固定在项目根运行，不接受 `env`、`dshEnv`、`stdin` 或任意
  workdir；`start()` 明确不可用。前台 `resolve/run` 复用宿主 `run_command`
  的 Execute 审批、TTL、受管进程组清理和平台 sandbox facts。
- `ctx.sessions` / `ctx.agents` 仅镜像当前活动 run，最多携带最近 64 个、
  合计 256 KiB 的模型项；所有 mutation API 报 `READ_ONLY_HOST_SERVICE`。

宿主 `notifications/cancelled` 与 adapter shutdown 会触发当前 tool call 的
`exec.signal`，并让该调用中的 sampling/elicitation 等待以取消错误收束。
插件自己的异步工作必须监听 signal，才能真正停止外部副作用。

## 权限与安全

DSH 工具没有 CLAT 的静态 effect，作者通过 `toolHints` 声明：

| hint | CLAT 分类 |
|---|---|
| `read-only` | `ExternalRead` |
| `network` | `Network` |
| `write` | `Write` |
| `destructive` 或缺省 | `Destructive` |

hint 只是调用前审批分类，不是 sandbox。Adapter 是运行任意第三方代码的
stdio 子进程，继承宿主环境与操作系统账户权限。CLAT 把其 cwd 固定在
`~/.clat`；真实项目根只作为受控的 `{{cwd}}` prompt 参数传入，这不构成
文件系统隔离。完整边界见 [MCP security posture](mcp.md#security-posture)。

若终端用户不应安装 Node，可由插件作者用 Bun 生成独立可执行文件：

```bash
bun build bin/clat.mjs --compile --outfile clat-my-plugin
```

## 验收要求

一次兼容声明至少需要：

1. adapter 单元测试覆盖插件形态、events/effects、service、prompt、工具、
   sampling、elicitation、web 与取消；
2. 原 npm 插件免网络 fixture 验收；
3. CLAT 端到端验证 bin → MCP 握手 → tools/prompts 导入 → 权限/调用/清理；
4. 对目标 DSH tag 重新做源码契约比对，不能只以 TypeScript 编译通过替代。

开发命令：

```bash
cd sdk/dsh-adapter
npm install
npm test
```

新增 host-spine 兼容面时，应先在 CLAT Rust 内核中建立对应 typed service，
再让 adapter 做协议投影；不要把关键权限或持久化语义塞回 JavaScript shim。

## 兼容性扫描器

Adapter 的 v2 扫描器使用 TypeScript AST、`apply`/可信 `Context` 参数、
Cordis `Service` 来源与静态 `inject` 证明真正的 DSH context 绑定，不再把
任意局部变量 `ctx` 当作插件证据。它还区分 `sessions.get` 与
`sessions.create` 等成员级语义。扫描器按 package 报告使用的 seam，并把
结果分成 `portable`、`host-bridged`、`partial`、`unsupported` 与
`not-plugin`；结果按包名排序、携带 DSH Git revision，可作为后续逐包移植
清单，但不能替代行为验收：

```bash
cd sdk/dsh-adapter
npm run scan -- /path/to/deepseek-harness --output /tmp/dsh-compat.json
```

对本页钉定的 `b150a551…` checkout，v2 扫描到 249 个 package，其中
234 个含插件候选证据：2 `portable`、171 `partial`、61 `unsupported`、
15 `not-plugin`。完整矩阵的稳定 SHA-256 为
`0328b3b3eea092d261df1f93b7bd9185dcf42a1ebbed76e1639cd37e21219d71`。
成员级判断比 v1 更严格，所以 `unsupported` 增多不代表兼容性倒退。

主插件移植只分析 package 默认入口及其相对 import graph，不会因为独立的
`./invariant` companion export 把主入口误判为 partial。钉定的 12 包代表
cohort 覆盖 web、LLM、用户提问、todo、fs、agent loop、shell、subagent、
skill 与 storage，证据位于 `sdk/dsh-adapter/compat/official-cohort.json`。

## 转换、测试与打包

作者侧工具需要 TypeScript 5.7+；它不进入 CLAT 核心，也不是 adapter
server 的运行时依赖：

```bash
clat-dsh inspect /path/to/dsh-plugin
clat-dsh port /path/to/dsh-plugin --out ./clat-port
clat-dsh test ./clat-port
clat-dsh package ./clat-port --out ./clat-package
clat plugin install ./clat-package --accept-capabilities
clat plugin pack ./clat-package --output my-dsh-plugin.clatpkg
```

`port` 保留原 DSH 入口并生成独立 MCP wrapper、逐 seam 报告和 TODO。
`package` 默认拒绝存在 unsupported seam 的报告；人工审查后只能显式使用
`--allow-partial`。它通过 Bun 生成单一可执行文件，因此最终用户不需要
Node.js。`test` 和 `package` 都会进行真实 MCP initialize/tools-list smoke。

DSH 上游已把 Cordis 框架在 npm 公开（`@deepseek-ai` scope 从 restricted
转 public，2026-08-13 `a213befd0f`）。adapter 本身不依赖 `cordis`（接口
是结构化鸭子类型，插件自带 type-only import），收益落在被移植的 DSH
插件一侧：port 后的插件及其依赖树可以直接从公开 npm 解析
`@deepseek-ai/cordis`，打包工具链不需要 vendor 源码或私有 registry
凭据——DSH 插件打包路径的安装面因此收窄为纯公开依赖。

可选的 `--publisher`、`--publisher-key`、`--minisign-key` 会生成
Minisign companion 文件。CLAT 安装器重新计算完整包树并验签，但
`publisher/verified` 只证明同一自声明 publisher key 签过该包；从远程市场
安装时还要求 `pi.at.cn` 的签名索引把该 publisher 与精确 key 标为可信、
未撤销且处于发布有效期。DSH 兼容包与 Rust/WASM 原生包使用同一套市场索引、
依赖求解、漏洞阻断与原子安装协议。

这是刻意保守的证据：未知/可变 service 不会被算作完全兼容，也没有把 Web
preset 的 147 个配置行冒充 147 个已行为验收包。

CLAT 插件包、Rust/WASM 原生插件与远程市场的统一身份模型见
[CLAT 插件与包格式](plugins.md)。
