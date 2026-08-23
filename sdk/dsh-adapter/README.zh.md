# @artec/clat-dsh-adapter

[English](README.md) | 中文

把现成的
[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) 叶子插件
作为 MCP stdio server 提供给
[CLAT](https://github.com/artec/clat) 或任意 MCP 宿主，插件主体无需修改。

适配器运行在插件作者自己的发行物中。CLAT 不内嵌 JavaScript 运行时；
对终端用户而言，产物只是一个普通 MCP server。

## 适合使用吗？

纯算法插件，以及搜索、SaaS、数据库或其他外部 API 包装器最适合。
若插件直接依赖宿主会话、agent、subagent、fs/shell seam 或 UI 服务，
它属于宿主脊柱，必须先重构成叶子工具。

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

`serveClat(plugin, options)` 接受插件导出对象或裸 `apply` 函数。MCP
initialize 会等待 `apply()` 结算；它抛错时，适配器先关停再拒绝启动。

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
| `ctx.web.registerFetchProvider(...)` | 接受注册；v0 不提供 `web_fetch` 工具 |
| `ctx.get(key)` | 恒为 `undefined` |
| `launchEnvironmentOf(ctx)` | 插件查询环境时回退 `process.env` |
| `ctx.effect`、`ctx.logger` | 进程内 LIFO 清理与 stderr 日志 |
| 导出的 `Config` | `apply` 前的启动校验 |

静态脊柱服务 `inject`、运行期直接访问脊柱、类插件（`extends Service`）
会在启动时明确失败并给出迁移提示。运行期可选
`ctx.inject(deps, callback)` 按 DSH “未挂载”契约处理：跳过回调，stderr
说明，插件继续运行。

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

宿主 `notifications/cancelled` 与 adapter shutdown 会中止活动
`tools/call` signal 及在途 sampling/elicitation promise。插件自己的工作
必须监听 `exec.signal` 才能协作式停止。

## 安全边界

Adapter 是运行任意插件代码的 MCP stdio 子进程，不是 WASM capability
sandbox。在 CLAT 下，它继承宿主进程环境变量，以及当前操作系统账户的
文件、进程与网络权限。CLAT 会把 MCP 子进程 cwd 设为 `~/.clat`，但这
不构成隔离。

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
- API 钉靶：`dsh-v0.1.0-rc.7`，rc.8 实测等价，并复核
  `0.1.1-rc.1` 插件面。
- 验收 fixture：npm 发布物 `@deepseek-ai/dsh-web-search-exa` 在
  [`examples/exa`](https://github.com/artec/clat/tree/main/sdk/dsh-adapter/examples/exa)
  中原样挂载。

```bash
npm install
npm test
```

仓库：[artec/clat](https://github.com/artec/clat) ·
[sdk/dsh-adapter](https://github.com/artec/clat/tree/main/sdk/dsh-adapter)

MIT
