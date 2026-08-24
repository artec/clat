# @artec/clat-dsh-adapter

[English](README.md) | 中文

把现成
[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) 插件中的
可移植能力作为 MCP stdio server 提供给
[CLAT](https://github.com/artec/clat) 或任意 MCP 宿主，插件主体无需修改。

适配器运行在插件作者自己的发行物中。CLAT 不内嵌 JavaScript 运行时；
对终端用户而言，产物只是一个普通 MCP server。

## 适合使用吗？

工具、system prompt、模型采样、用户问题、web provider、fs/shell、只读
session/agent 检视、本地 service 与 Cordis events/effects 都可适配。
可变 session/agent、subagent、权限、settings、commands 或 UI 服务仍需
CLAT 先提供对应原生宿主能力，或在该边界拆分插件。

完整兼容矩阵与迁移方法见
[移植指南](https://github.com/artec/clat/blob/main/docs/dsh-plugins.md)。

## 快速开始

假设原插件为：

```ts
// src/index.ts —— 原 DSH 插件，不改
import { defineTool } from '@deepseek-ai/dsh-tools'

export const name = 'my-plugin'
export const inject = [] as const

export function Config(config: { apiKey: string }) {
  if (!config.apiKey) throw new Error('MY_API_KEY is required')
  return config
}

export function apply(ctx, config: { apiKey: string }) {
  ctx.tools.register(defineTool({ /* ... */ }))
}
```

新增一个 bin：

```ts
// bin/clat.mjs
import { serveClat } from '@artec/clat-dsh-adapter'
import { apply, Config, inject, name } from '../src/index.js'

serveClat({ apply, Config, inject, name }, {
  name: 'my-plugin',
  version: '1.0.0',
  config: { apiKey: process.env.MY_API_KEY ?? '' },
  toolHints: { my_tool: 'network' },
}).catch(error => {
  console.error(error)
  process.exit(1)
})
```

在 `package.json` 增加 `bin`。DSH 用户继续走原 Cordis 入口，MCP 用户
运行这个 bin。

CLAT 用户配置 `~/.clat/mcp.json`：

```json
{
  "my-plugin": {
    "command": "node",
    "args": ["/path/to/package/bin/clat.mjs"],
    "env": { "MY_API_KEY": "..." }
  }
}
```

stdout 是协议专线。诊断请使用 `ctx.logger` 或 `console.error`。

## API

`serveClat(plugin, options)` 接受插件导出对象、裸 `apply` 函数或静态
`class Foo extends Service` 插件。MCP initialize 会等待启动结算；抛错时，
适配器先关停再拒绝启动。

| 选项 | 类型 | 用途 |
|---|---|---|
| `name` | `string` | MCP server 名；缺省取 `plugin.name`，再退到 `dsh-plugin` |
| `version` | `string` | MCP server 版本 |
| `config` | `unknown` | 传给 `apply`；有 `Config` 时先校验 |
| `toolHints` | `Record<string, ToolHint>` | 已注册工具的副作用声明 |
| `input`、`output` | streams | 测试缝；缺省进程 stdio |

## 支持面

| DSH API | MCP 行为 |
|---|---|
| `ctx.tools.register(defineTool(...))` | `tools/list` + `tools/call`；保留编译后 schema 与 `output.render` |
| `ctx.llm.stream(...)` | `sampling/createMessage`，使用宿主模型、权限门、花费预算和 usage 账本 |
| `ctx.userQuestions.ask(...)` | `elicitation/create`；逐字段询问 |
| `ctx.web.registerSearchProvider(...)` | 内置 `web_search`，支持多 query 合并、URL 去重和有界结果 |
| `ctx.web.registerFetchProvider(...)` | 内置、有输出上限的 `web_fetch` |
| `ctx.systemPrompt` | section/context/order/complete/variable/tools/change/assemble waterfall |
| `ctx.clat` | 有界的当前 run 上下文与过权限门的原生宿主工具调用 |
| `ctx.fs` | 通过 CLAT read/list/write/edit 工具投影的 DSH FileSystem |
| `ctx.shell` | 通过 `run_command` 提供前台 `resolve` / `run` |
| `ctx.sessions`、`ctx.agents` | 当前 run 的分离、只读镜像 |
| `ctx.get/set/provide`、`ctx.reflect.provide` | 进程内 service 注册、查询与撤销 |
| `ctx.on/once`、`emit/parallel/serial/bail/waterfall` | 进程内 Cordis 调度语义 |
| `launchEnvironmentOf(ctx)` | 插件查询环境时回退 `process.env` |
| `ctx.effect`、可调用的 `ctx.logger(name)` | 函数/Promise/generator cleanup 逆序执行；stderr 日志 |
| `class Foo extends Service` | constructor、`initHooks`、`Service.init` 与 yielded cleanup |
| 导出的 `Config` | `apply` 前的启动校验 |

静态 `inject` 对 adapter/local service 生效，缺少宿主脊柱 service 时给出
精确错误。运行期 `ctx.inject(deps, callback)` 在依赖齐全时立即接线并返回
可 await/dispose 的静态 Fiber 形状，缺失时按 DSH “未挂载”契约跳过。
这里是静态、单作用域 Cordis 子集，不模拟热更新、scope chain、
isolate/intercept 过滤或依赖驱动重启。function/object `apply()` 与 Service
生命周期返回的直接、Promise、同步/异步 generator cleanup 均由 adapter 接管。

system-prompt 贡献通过带标记的 MCP prompt 暴露。CLAT 只自动导入该标记
prompt，把真实项目目录作为 `cwd` 参数，并在首个 run 前和工具一起冻结。
runtime-context 文本仍保留在 MCP 元数据中，因为 CLAT 尚无 DSH 的 user-role
context-snapshot registry。

## Tool hints

DSH 工具没有静态 effect。不声明时按最保守的 `destructive` 处理。

| Hint | 含义 |
|---|---|
| `'read-only'` | 读取闭世界数据，无副作用 |
| `'network'` | 读取型开放世界/网络访问 |
| `'write'` | 修改文件或外部状态 |
| `'destructive'` 或缺省 | 破坏性或未知行为 |

Hint 会转为 MCP annotations；最终 effect 映射与权限策略仍由宿主决定。

## 已知收窄

- `apply()` 必须在宿主握手超时内结算（CLAT 为 10 秒）。
- `ctx.llm.stream({ tools })` 被拒；MCP sampling 不携带工具调用。
- sampling 消息只支持文本；图片块返回 `NON_TEXT_CONTENT`。
- 会发送 `stopSequences`，但当前 CLAT host bridge 会忽略。
- `multiSelect` 问题降级为逗号分隔文本。
- 一次 ask 最多 16 问、每问最多 16 个选项。
- `exec.deferContext()` 与 `exec.concludeTurn()` 是警告 + no-op seam。
- `web_fetch` 输出最多 100,000 字符，不复刻 DSH 完整 HTML→Markdown 管线。
- session/agent 修改与实时事件、subagent、permission/settings/commands/UI、
  后台 shell、fs 原子版本 guard 与 scoped prompt shadowing 仍属原生宿主职责。
- `ctx.fs.readText/readBytes` 对超过宿主 64 KiB 完整读取上限的文件明确拒绝；
  guarded write 与 `replaceAll` 不伪造 DSH 原子性，投影的 fs 路径只能位于
  当前 CLAT 项目内。Shell cwd 固定为项目根，env/stdin override 会被拒绝。

宿主 `notifications/cancelled` 与 adapter shutdown 会中止活动
`tools/call` signal 及在途 sampling/elicitation promise。插件自己的工作
必须监听 `exec.signal` 才能协作式停止。

## 安全边界

Adapter 是运行任意插件代码的 MCP stdio 子进程，不是 WASM capability
sandbox。在 CLAT 下，它继承宿主进程环境变量，以及当前操作系统账户的
文件、进程与网络权限。CLAT 会把 MCP 子进程 cwd 设为 `~/.clat`；真实项目
根只作为受控 prompt 参数 `cwd` 传入。两者都不构成隔离。

`toolHints` 只影响调用前的审批分类，不能限制进程在工具 handler 之外
做什么。请把插件当作可执行依赖审查，使用窄权限凭据，必要时经干净环境
wrapper 启动。完整边界见 CLAT 的
[MCP 安全说明](https://github.com/artec/clat/blob/main/docs/mcp.md#security-posture)。

## 让终端用户免装 Node

用 Bun 编译 bin：

```bash
bun build bin/clat.mjs --compile --outfile clat-my-plugin
```

MCP `command` 指向这个可执行文件。运行时已经打入产物，终端用户无需
安装 JavaScript 环境。

## 兼容性与验证

- 作者侧运行时：Node.js 22.19 或更高。
- adapter runtime 依赖：零。
- API 钉靶：`dsh-v0.1.1-rc.2`，源提交
  `b150a551b8d465e31e418e1b2eaf5e79bbb7d28e`。
- 验收 fixture：npm 发布物 `@deepseek-ai/dsh-web-search-exa` 在
  [`examples/exa`](https://github.com/artec/clat/tree/main/sdk/dsh-adapter/examples/exa)
  中原样挂载。

```bash
npm install
npm test
npm run scan -- /path/to/deepseek-harness --output /tmp/dsh-compat.json
```

扫描器输出钉定 revision、逐包可机读的 seam 矩阵。它是保守的静态证据，
不能替代 fixture 与端到端验收。

仓库：[artec/clat](https://github.com/artec/clat) ·
[sdk/dsh-adapter](https://github.com/artec/clat/tree/main/sdk/dsh-adapter)

MIT
