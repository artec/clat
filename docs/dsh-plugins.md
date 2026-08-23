# 把 DSH 插件发布给 CLAT 用户

本文面向 DeepSeek Harness（DSH）插件作者，介绍如何用
`@artec/clat-dsh-adapter` 把现有**叶子插件**包装成 MCP stdio server。
插件主体不需要为 CLAT 分叉，CLAT 也不会内嵌 Node 或 JS 引擎。

适配器的方向是刻意反转的：它进入插件作者自己的发行物，加载原插件，
再向 CLAT 或其他 MCP 宿主暴露标准 MCP。终端用户看到的只是一个 server。

包级 API、安装和最小示例另见
[适配器中文 README](../sdk/dsh-adapter/README.zh.md)。

## 先判断是否适合适配

| 插件类别 | 判断方式 | 结论 |
|---|---|---|
| 纯算法 | 输入 → 输出，不依赖宿主脊柱 | 直接适配；若追求终端用户零 JS，也可提供 Rust/WASM 发行 |
| 外部服务适配器 | 封装搜索、SaaS、数据库或其他 API | 最适合 adapter |
| 宿主脊柱 | 深度依赖会话、agent 循环、fs/shell seam、subagent | 不适配；这应由宿主本身实现 |
| UI 插件 | 面板、设置页、浏览器交互 | 不提供 UI；可选接线会优雅降级 |
| 内容资产 | 只读取/生产 DSH 会话日志 | 不需要 adapter；CLAT 与 DSH 会话格式兼容 |

核心判断不是“代码是不是 TypeScript”，而是“能力能否收敛成叶子工具，
并通过 MCP sampling / elicitation 使用宿主能力”。

## 最小移植：增加一个 bin

假设原插件已经导出 `apply`、`Config`、`inject` 与 `name`：

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

在 `package.json` 添加一条 `bin`。同一个 npm 包可以保留原 Cordis/DSH
入口，同时为 MCP 宿主提供这个入口。

CLAT 用户在 `~/.clat/mcp.json` 配置：

```json
{
  "my-plugin": {
    "command": "node",
    "args": ["/path/to/package/bin/clat.mjs"],
    "env": { "MY_API_KEY": "..." }
  }
}
```

stdout 是 JSON-RPC 专线。诊断只能写 `ctx.logger`、`console.error` 或
其他 stderr 通道。

## `serveClat` 选项

| 选项 | 类型 | 用途 |
|---|---|---|
| `name` | `string` | MCP `serverInfo.name`；缺省取 `plugin.name`，再退到 `dsh-plugin` |
| `version` | `string` | MCP server 版本 |
| `config` | `unknown` | 传给 `apply(ctx, config)`；有 `Config` 时先校验 |
| `toolHints` | `Record<string, ToolHint>` | 声明每个工具的副作用档位 |
| `input` / `output` | streams | 测试缝；缺省为进程 stdio |

MCP `initialize` 应答会等待 `apply()` 结算。`apply` 抛错时，适配器先
执行关停，再拒绝启动。

## 支持面

| DSH 侧 | MCP 侧 | 语义 |
|---|---|---|
| `ctx.tools.register(defineTool(...))` | `tools/list` + `tools/call` | 接受已编译 JSON Schema；`output.render` 产出模型可见内容 |
| `ctx.llm.stream(options)` | `sampling/createMessage` | 使用宿主当前模型，经过宿主权限门与共享花费预算，usage 回到当前 run |
| `ctx.userQuestions.ask(...)` | `elicitation/create` | 一批问题变为一个表单，由宿主逐字段询问 |
| `ctx.web.registerSearchProvider(...)` | 内置 `web_search` 工具 | 多 query 轮转、URL 去重、`maxResults` 截断与 source 输出 |
| `ctx.web.registerFetchProvider(...)` | 只登记 | v0 不暴露 `web_fetch` 工具，stderr 明确提示 |
| `ctx.get(key)` | 恒 `undefined` | `launchEnvironmentOf(ctx)` 可回退 `process.env` |
| `ctx.effect(gen)` | 进程内清理器 | LIFO；单个清理失败不会截断其余清理 |
| `ctx.logger` | stderr | 不污染协议帧 |
| 插件 `Config` | 启动校验 | 失败即拒绝启动，不运行 `apply` |

### LLM sampling

`ctx.llm.stream` 的 system prompt 与每条文本消息会完整进入 MCP sampling
请求。CLAT 宿主审批时看到的也是完整出站正文，而不是截断预览。

宿主决定最终 provider/model；插件传入的 provider/model 只会产生 stderr
说明。缺省 `maxTokens` 为 4096，宿主仍会应用自己的 8192 单请求上限、
每 run 64 次硬上限和跨传输共享的近似 token 花费门。

结果会适配回 dsh-llm chunk 协议，`BlockAssembler` 等聚合器可以继续
使用。sampling 不支持工具调用，消息只支持文本；图片等内容块返回
`NON_TEXT_CONTENT`。

`stopSequences` 会进入 MCP 参数，但 CLAT 当前 host bridge 忽略它，并
在每个 run 最多写一次 stderr 说明。不要把 stop 当成跨宿主一致语义。

### 用户问题

单选 options 无损映射成枚举字段。`multiSelect` 降级为逗号分隔文本，
按大小写不敏感标签匹配；未匹配文本并入 custom。MCP 单选字段不能同时
提供自由输入，因此“options + custom”问题应改成纯文本问题。

一次 ask 最多 16 问，每问最多 16 个选项。超过时返回
`TOO_MANY_QUESTIONS` 或 `TOO_MANY_OPTIONS`。

### 取消

宿主的 `notifications/cancelled` 与 adapter shutdown 会触发当前
`tools/call` 的 `exec.signal`。在途 sampling / elicitation promise 也会
以 `ABORTED` 类错误收束。取消若先于 call 注册到达，会在 call 建立时
补做对账，避免竞态丢失。

插件自己的长任务必须实际监听 `exec.signal`；只接收 signal 而不检查，
仍然无法被协作式取消。

## 拒绝与优雅降级

适配器把宿主脊柱与可选 UI 接线分开处理。

### 启动即拒绝

- 静态 `inject` 声明 `fs`、`shell`、`sessions`、`agents`、`subagents`、
  `settings`、`commands`、`systemPrompt` 或 UI 服务；
- 运行期直接访问 `ctx.fs`、`ctx.shell`、`ctx.sessions` 等脊柱服务；
- `extends Service` 的类插件；
- 未编译的手工 DSL，而不是 `defineTool()` 产出的 schema。

错误会列出支持面和改写方向。正确的改写通常是把能力收敛为
`ctx.tools.register` 叶子工具，或直接依赖 CLAT 的原生能力。

### 优雅降级

运行期 `ctx.inject(deps, callback)` 属于可选接线。目标服务未挂载时，
回调跳过并写一条 stderr 注记，插件继续工作。这与无 UI 的 DSH 宿主
行为一致。

因此，纯 UI 插件可能“连接成功、0 工具”。这不是假装支持 UI，而是
明确保留插件非 UI 部分并报告降级。

`exec.deferContext()` 与 `exec.concludeTurn()` 当前也是 stderr 警告加
no-op；MCP 叶子工具没有 agent-loop 上下文渡口。

## 安全边界

Adapter 产物最终是一个 MCP stdio **任意代码子进程**，不是 WASM
capability sandbox。CLAT 启动它时会把工作目录设为 `~/.clat`，但子进程
仍继承 CLAT 的完整环境变量，并拥有当前操作系统账户授予的文件、网络与
进程权限。用 Bun 编译成单一可执行文件不会改变这一边界。

`toolHints` 只影响 CLAT 在工具调用前如何分类和审批，既不限制进程权限，
也不能阻止插件在工具函数之外执行代码。对不完全信任的插件，应从干净
环境启动 CLAT、使用 `env -i` 一类 wrapper、收窄凭据，并把它当成普通
第三方可执行程序审查。完整宿主侧威胁模型见
[MCP Security posture](mcp.md#security-posture)。

## `toolHints` 与权限

DSH 工具没有静态 effect 字段。缺省时 adapter 按最保守的 destructive
处理。作者应按真实行为声明：

| hint | MCP annotations | CLAT effect |
|---|---|---|
| `'read-only'` | readOnly=true, openWorld=false | `ExternalRead` |
| `'network'` | readOnly=true, openWorld=true | `Network` |
| `'write'` | readOnly=false, destructive=false | `Write` |
| `'destructive'` 或缺省 | 保守缺省 | `Destructive` |

这是作者对自己工具行为的声明，不是逃过权限门的手段。MCP 注解在 CLAT
中永远不会升级成原生 `Read`。

## 用户侧免 Node

作者侧需要 Node.js 22.19 或更高版本。若不希望终端用户安装 Node，可用
Bun 把 bin 编译成独立可执行：

```bash
bun build bin/clat.mjs --compile --outfile clat-my-plugin
```

用户的 `mcp.json` 直接把 `command` 指向该产物。运行时随可执行文件一起
分发，仍符合 CLAT 终端用户的一二进制体验。

## 验收

至少覆盖三层：

1. **适配器单元测试**——schema、调用、sampling、elicitation、取消、
   清理与协议帧；
2. **真实插件免网络验收**——本仓库
   `sdk/dsh-adapter/examples/exa/test.mjs` 原样加载 npm 插件并断言工具面；
3. **CLAT 全链**——bin → `mcp.json` → `/mcp` → 模型调用 → 权限/
   usage/取消闭环。

开发命令：

```bash
cd sdk/dsh-adapter
npm install
npm test
```

当前包 API 面钉在 DSH `dsh-v0.1.0-rc.7`，已对 rc.8 与
`0.1.1-rc.1` 的插件面复核。上游变更后应重新跑真实插件验收，不能只
依赖 TypeScript 编译通过。

## 上游包排查提示

DSH 社区包的 peer dependency 可能不完整。遇到“adapter 启动前就模块
找不到”时，先检查插件发行物，而不是修改 adapter 语义：

- 旧包可能仍引用未发布的 `@deepseek-ai/dsh-environment`，后来上游改名
  为 `dsh-launch-environment`；
- `dsh-tools`、`dsh-llm`、`dsh-session`、`dsh-scope`、`dsh-timeout`、
  `dsh-settings`、`dsh-credentials` 或 `cordis` 可能在运行期 import，
  却没有完整出现在依赖表；
- 某些插件依赖尚未进入 npm 发行版的导出，只能等待或选择匹配的上游版本。

`examples/exa/` 展示了测试环境下的 stub 处理方式。正式发行应由插件
作者修正 package metadata，不应要求每个终端用户手工猜依赖。
