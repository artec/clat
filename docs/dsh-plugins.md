# 把 DSH 插件发布给 CLAT 用户：@artec/clat-dsh-adapter 移植指南

> 面向 DSH（DeepSeek Harness）插件**作者**。CLAT 宿主不运行 Node、
> 不内嵌 JS 引擎——兼容的方向是反转的：你在自己的发行物里挂一个
> 适配器，把现成插件以 **MCP stdio server** 形态对外服务，CLAT 用户
> 把它当普通 MCP server 配置即可（任何 MCP 宿主都能用）。
> 适配器对 DSH 插件 API 面钉 revision `99f6f02f`（0.1.0-rc.7），
> 以 rc.8（`141eb6f`，插件面源码抽查等价）验证。

## 一、全部改造量：一个 bin 入口

```ts
// bin/clat.mjs —— 你的仓库新增的唯一文件
import { serveClat } from '@artec/clat-dsh-adapter'
import { apply, Config, inject, name } from '../src/index.js' // 你原有的插件导出

serveClat({ apply, Config, inject, name }, {
  name: 'my-plugin',            // MCP serverInfo 名（缺省用 plugin.name）
  version: '1.0.0',
  config: { apiKey: process.env.MY_API_KEY ?? '' }, // 经你的 Config 校验后传给 apply
  toolHints: { my_tool: 'network' }, // 可选：声明自家工具的副作用档位
})
```

package.json 加一条 `"bin"`，一仓库双出口：同一个包既含 Cordis 插件
入口（DSH 用户），又含 adapter bin（CLAT 用户）。

CLAT 用户侧的配置（`~/.clat/mcp.json`）：

```json
{
  "my-plugin": {
    "command": "node",
    "args": ["/path/to/your/package/bin/clat.mjs"],
    "env": { "MY_API_KEY": "..." }
  }
}
```

## 二、支持面（ctx 服务 → MCP 映射）

| DSH 侧 | 适配器翻译成 | 说明 |
|---|---|---|
| `ctx.tools.register(defineTool(...))` | MCP `tools/list` + `tools/call` | 接受 `defineTool()` 的产物（其 `parameters` 已是编译后的 JSON Schema，`output.render` 产出模型可见内容）；手工构造的未编译 DSL 会被拒绝并引导 |
| `ctx.llm.stream(options)` | MCP `sampling/createMessage` | 宿主会话模型 + 宿主权限门 + usage 记账；`provider`/`model` 被忽略（stderr 记录）；结果适配回 dsh-llm 的 chunk 协议（BlockAssembler 等聚合器可直接消费） |
| `ctx.userQuestions.ask({...})` | MCP `elicitation/create` | 整批问题翻成一个表单，宿主逐字段问；单选 options → 选择字段（无损）；`multiSelect` 降级为逗号分隔文本（见"已知收窄"） |
| `ctx.web.registerSearchProvider(...)` | 内置 `web_search` 工具 | seam 语义 1:1（执行期选源、maxResults 截断、round-robin 多问合并去重）；工具镜像 dsh-tool-web 的 queries 参数与 sources 输出，标注 readOnly+openWorld |
| `ctx.web.registerFetchProvider(...)` | （登记，v0 无工具面） | 注册被接受，但适配器 v0 不暴露 `web_fetch` 工具（stderr 提示） |
| `ctx.get(key)` | 恒 undefined | `launchEnvironmentOf(ctx)` 库内自动回退 `process.env`——API key 走环境变量的插件无需改动 |
| `ctx.effect(gen)` / `ctx.logger` | 进程内实现 | 清理器 LIFO；日志全部走 stderr |
| 插件导出 `Config` | serveClat 启动时校验 | 可调用则先验 `config`，抛错即拒绝启动 |

**拒绝与降级面**（两类，语义对齐 DSH 宿主，2026-08-21 社区插件实测定稿）：

- **静态 `inject` 声明**（`export const inject = ['sessions', …]`）与运行期
  **直接访问** `ctx.fs` / `ctx.shell` / `ctx.sessions` 等脊柱服务 → 启动即
  报错（带支持清单与改写指引），不假装支持。改写方向：把能力收敛为
  `ctx.tools.register` 的叶子工具，或直接使用 CLAT 的内建工具。
- **运行期 `ctx.inject(deps, callback)`**（可选服务接线，如 dsh-settings
  的设置面板、`systemPrompt` 贡献）→ 按 DSH 宿主的"未挂载"契约处理：
  回调跳过、stderr 记一条注记，插件照常工作——这正是无 UI 宿主的 DSH
  环境会发生的降级。纯 UI 插件（如 dsh-smooth-stream）因此以"连接成功
  但 0 工具"收场，stderr 注记说明原因。**类插件**（`extends Service`）
  仍被拒。

## 三、兼容矩阵（先给自己定位，再动手）

| 类别 | 判据 | 建议 |
|---|---|---|
| **A 纯算法** | 不碰网络/宿主服务，只做输入→输出变换 | adapter 直接可用；更推荐顺手出 **Rust/WASM 双发行**（`docs/wasm.md`，用户侧零 Node） |
| **B 外部适配器** | 包一层外部 API（web 检索、SaaS、数据库…） | adapter 的主战场；试点见 `sdk/dsh-adapter/examples/exa/`（npm 真实发布物 web-search-exa 原样挂载） |
| **C 脊柱** | 深依赖宿主服务（会话、agent 循环、fs/shell seam） | 拒绝——这是 CLAT 自己的工程；按"拒绝面"改写 |
| **D UI** | 前端/交互面板 | 永远不做 |
| **E 内容资产** | 只消费/生产会话日志（分析、导出、replay、索引） | **零成本直搬**：CLAT 会话日志与 DSH（rc.7 起）字节级格式兼容，不需要 adapter |

## 四、toolHints（可选，但建议）

DSH 工具没有静态 effect 字段；不声明时 CLAT 按最保守档（Destructive）
处理——每次调用都可能过权限门。用 `toolHints` 声明真实档位：

| hint | MCP annotations | CLAT 档位 |
|---|---|---|
| `'read-only'` | `{readOnlyHint: true, openWorldHint: false}` | ExternalRead |
| `'network'` | `{readOnlyHint: true, openWorldHint: true}` | Network |
| `'write'` | `{readOnlyHint: false, destructiveHint: false, openWorldHint: false}` | Write |
| `'destructive'` 或缺省 | 不发标注 | Destructive（保守兜底） |

声明是作者对自家工具的背书（MCP 惯例），请如实填写。

## 五、已知收窄与硬约束

1. **stdout 是协议专线**：任何 `console.log` 都会污染 JSON-RPC 帧。
   诊断一律 `ctx.logger`（stderr）或 `console.error`。
2. **`apply()` 必须在握手超时内结算**（CLAT 10s）：initialize 应答
   等待 apply 完成；重初始化放工具 execute 或后台。
3. **multiSelect 降级**：宿主逐字段单问，多选翻成"逗号分隔标签"
   的文本字段（大小写不敏感匹配，未匹配文本并入 custom）。
4. **`options+custom` 双模式**：选择字段不提供自由输入；需要 custom
   的问题请改用无 options 的文本问题。
5. **sampling 无工具面**：`ctx.llm.stream({tools: [...]})` 直接报错
   （MCP sampling 不带 tool-calling）；`provider`/`model` 恒用宿主
   会话模型；`maxTokens` 缺省 4096；`stopSequences` 忠实发送但部分
   宿主（含 CLAT v1）忽略。
6. **sampling 仅文本**：`ctx.llm.stream` 的消息里出现图片等非文本
   内容块会直接报错（`NON_TEXT_CONTENT`）——多模态采样暂不在桥上；
   工具调用参数里的图片附件是 JSON 透传，不受影响。
7. **取消不转发**：宿主发 `notifications/cancelled` 仅记录；工具的
   `exec.signal` v0 不会触发（宿主侧以调用截止兜底）。
8. **`exec.deferContext()` / `exec.concludeTurn()`**：stderr 警告 +
   no-op（MCP 没有 agent-loop 上下文渡口）。
9. **数量上限**：一次 ask ≤16 问、每问 ≤16 选项（宿主 elicitation
   上限）；错误码 `TOO_MANY_QUESTIONS`/`TOO_MANY_OPTIONS`。

## 六、用户侧免 Node：编译型分发

CLAT 用户不想装 Node？用 Bun 把你的 bin 编译成独立可执行：

```sh
bun build bin/clat.mjs --compile --outfile clat-my-plugin
```

mcp.json 的 `command` 直接指向该二进制。分发物自带运行时，宿主
不需要任何 JS 环境。

## 七、验收与冒烟

- 适配器仓库内建三层测试可参照：单元（`sdk/dsh-adapter/test/`）、
  真实插件免网络验收（`examples/exa/test.mjs`——断言插件原样挂载、
  工具面板出现、无 key 时的 `WEB_PROVIDER_UNAVAILABLE`）、CLAT 侧
  全链 e2e（`src/plugin_host.rs` 门控测试）。
- 联网冒烟：`serveClat` 的 `config` 传入真实 key 后，在 CLAT 里
  直接调用你的工具即可（tools 面板 `/mcp` 可见 `transport: stdio`）。
- **npm 现状提示（2026-08-21 社区插件实测）**：DSH 侧发布物的 peer
  声明普遍不齐，独立安装大概率要手工补。两类问题：
  1. **死引用**：`@deepseek-ai/dsh-environment` 从未发布（后改名
     `dsh-launch-environment`）——官方与社区插件的 peerDependencies
     都还挂着它，需按 `examples/exa/` 的方式本地 stub；
  2. **漏声明**：`dsh-tools` / `dsh-llm` / `dsh-session` / `dsh-scope` /
     `dsh-timeout` / `dsh-settings` / `dsh-credentials` / `cordis`
     常被运行期 import 却不在依赖表里，`--legacy-peer-deps` 安装后
     逐个补装即可（实测 dsh-free-search 补了 7 个）。
  个别插件（如 dsh-memento）依赖**尚未发布**的新版导出（如
  `KNOWN_SESSION_EVENT_TYPES`），npm 上目前装不起来——只能等上游。
  这是 DSH 生态的发布物问题，与适配器无关。
- **实测背书**：官方 `dsh-web-search-deepseek`（带 key）与社区
  `dsh-free-search`（免 key）均已通过"bin → mcp.json → CLAT 真实模型
  调用"全链验证；`dsh-smooth-stream` 验证了 UI 面的优雅降级路径。
