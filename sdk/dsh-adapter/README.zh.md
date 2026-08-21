# @artec/clat-dsh-adapter

[English](README.md) | 中文

把现成的 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)（DSH）叶子插件以 **MCP stdio server** 形态对外服务——[CLAT](https://github.com/artec/clat) 用户（以及任何 MCP 宿主的用户）即可使用你的插件，**插件本体零修改**。

兼容方向是刻意反转的：CLAT 不内嵌 JS 运行时。适配器活在**你的**发行物里，包住你自己的插件；对 MCP 客户端而言，产物就是一个普通 MCP server。

## 快速开始

全部改造量是一个 bin 文件。假设你的插件现有导出：

```ts
// src/index.ts —— 你原有的 DSH 插件，不改
import { defineTool } from '@deepseek-ai/dsh-tools'

export const name = 'my-plugin'
export const inject = [] as const

export function Config(config: { apiKey: string }) {
  if (!config.apiKey) throw new Error('MY_API_KEY is required')
  return config
}

export function apply(ctx, config: { apiKey: string }) {
  ctx.tools.register(defineTool({ /* … */ }))
}
```

新增这个 bin：

```ts
// bin/clat.mjs —— 唯一新增的文件
import { serveClat } from '@artec/clat-dsh-adapter'
import { apply, Config, inject, name } from '../src/index.js'

serveClat({ apply, Config, inject, name }, {
  name: 'my-plugin',                          // MCP serverInfo 名
  version: '1.0.0',
  config: { apiKey: process.env.MY_API_KEY ?? '' },
  toolHints: { my_tool: 'network' },          // 可选，见下
}).catch(error => {
  console.error(error)
  process.exit(1)
})
```

package.json 再加一条 `"bin"`，一个包同时服务两个运行时：DSH 用户照旧走
Cordis 入口，CLAT 用户把 MCP 配置指向这个 bin。

CLAT 用户在 `~/.clat/mcp.json` 里配置（任何 MCP stdio 客户端同理）：

```json
{
  "my-plugin": {
    "command": "node",
    "args": ["/path/to/your/package/bin/clat.mjs"],
    "env": { "MY_API_KEY": "…" }
  }
}
```

## 选项

`serveClat(plugin, options)` 接受插件的导出对象（或裸 `apply` 函数）。
`apply()` 结算后 resolve——MCP `initialize` 应答以此为闸——`apply` 抛错
则先关停再 reject。

| 选项 | 类型 | 用途 |
|---|---|---|
| `name` | `string` | MCP serverInfo 名；缺省取 `plugin.name`，再退 `dsh-plugin` |
| `version` | `string` | MCP serverInfo 版本 |
| `config` | `unknown` | 传给 `apply(ctx, config)`；有 `Config` 导出时先经其校验 |
| `toolHints` | `Record<string, ToolHint>` | 声明自家工具的副作用档位（见下） |
| `input` / `output` | 流 | 测试缝；缺省进程 stdio |

## toolHints

DSH 工具没有静态 effect 字段；不声明时宿主按最保守档处理
（`destructive`——每次调用都可能过权限门）。请如实声明：

| hint | 含义 |
|---|---|
| `'read-only'` | 只读，无副作用 |
| `'network'` | 只读，但访问网络 |
| `'write'` | 改写文件或外部状态 |
| `'destructive'` / 缺省 | 保守兜底 |

## 支持面

| DSH 侧 | 对外服务为 |
|---|---|
| `ctx.tools.register(defineTool(…))` | MCP `tools/list` + `tools/call`（接受编译后的 schema；`output.render` 产出模型可见内容） |
| `ctx.llm.stream(…)` | MCP `sampling/createMessage`——宿主会话模型 + 宿主权限门 + usage 记账 |
| `ctx.userQuestions.ask(…)` | MCP `elicitation/create`——整批问题一个表单，逐字段问 |
| `ctx.web.registerSearchProvider(…)` | 内置 `web_search` 工具（dsh-tool-web 语义：多问合并、URL 去重、上限 8 条） |
| `ctx.get` / `ctx.effect` / `ctx.logger` | 进程内实现（`launchEnvironmentOf` 回退 `process.env`；清理器 LIFO；日志走 stderr） |

两级策略（语义对齐 DSH 宿主）：

- **启动即明确报错**：静态 `inject` 导出声明脊柱服务（`fs`、`shell`、
  `sessions`、`agents`、`subagents`、`settings`、`commands`、
  `systemPrompt`、各 UI 服务），或运行期直接访问 `ctx.<脊柱>`。那些
  seam 属于宿主自身的工程——请把能力收敛为叶子工具。类插件
  （`extends Service`）同样被拒。
- **优雅降级**：运行期 `ctx.inject(deps, callback)` 的可选服务接线
  （如 dsh-settings 设置面板）按宿主"未挂载"契约处理——回调跳过、
  stderr 记一条注记，插件照常工作，与无 UI 的 DSH 宿主行为一致。

## 已知收窄（v0）

- stdout 是协议专线——严禁 `console.log`；诊断走 `ctx.logger` 或 `console.error`
- `apply()` 必须在宿主握手超时内结算（CLAT 为 10 秒）
- `multiSelect` 问题降级为逗号分隔文本
- `ctx.llm.stream({ tools })` 直接报错（MCP sampling 不带工具调用）；消息仅文本（图片块报 `NON_TEXT_CONTENT`）
- 取消不转发到 `exec.signal`；宿主以调用截止兜底
- 一次 ask：≤16 问、每问 ≤16 选项

完整移植指南——兼容矩阵（纯算法 / 外部适配器 / 脊柱 / UI / 内容资产）、
全部收窄、冒烟验收：
[docs/dsh-plugins.md](https://github.com/artec/clat/blob/main/docs/dsh-plugins.md)

## 用户侧免 Node：编译你的 bin

```sh
bun build bin/clat.mjs --compile --outfile clat-my-plugin
```

`command` 直接指向产物二进制——JS 运行时打进可执行文件，终端用户
什么都不用装。

## 环境要求与状态

- Node.js ≥ 22.19，**作者侧**（用 Bun 编译分发时终端用户无需 Node）
- 零 runtime 依赖
- 对 DSH 插件 API 面钉靶 `dsh-v0.1.0-rc.7`（rc.8 实测等价；0.1.1-rc.1 复核无变化）

npm 真实发布物 `@deepseek-ai/dsh-web-search-exa` 被本仓库验收测试
**原样挂载**：[examples/exa](https://github.com/artec/clat/tree/main/sdk/dsh-adapter/examples/exa)。

## 开发

```sh
npm install
npm test        # tsc 构建 + node:test
```

仓库：[artec/clat](https://github.com/artec/clat) ·
[sdk/dsh-adapter](https://github.com/artec/clat/tree/main/sdk/dsh-adapter)

MIT
