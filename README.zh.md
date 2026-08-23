# CLAT

[English](README.md) | 中文

**cl + at = command-line agent** · [主页](https://cl.at.cn)

CLAT 是一个本地优先的编码智能体底座，以 Rust 单二进制交付。它可以
检查仓库、编辑文件、执行命令、调用外部工具并持久化会话，不要求用户
另外安装 JavaScript 或 Python 运行时。
这条承诺针对 CLAT 自带核心；用户自行配置的 MCP server 与 DSH adapter
可以声明自己的运行时要求。

项目以真实仓库 dogfood 为起点，把反复出现的需求沉淀为可复用、模型
无关的能力。

## 快速开始

```bash
# 在当前仓库打开终端界面。
clat

# 首次进入后运行 /model，选择预设并填写 API key。

# 无头调用：位置参数是指令，管道输入是上下文。
clat exec "用一句话解释这个仓库"
git diff | clat exec "审查这个 diff"

# 不需要凭据，离线验证 模型 -> 工具 -> 模型 循环。
clat demo

# 检查是否有通过签名验证的升级。
clat upgrade --check
```

完整命令行参数见 `clat --help`。

## 使用界面

| 界面 | 适用场景 | 入口 |
|---|---|---|
| 终端 UI | 日常交互式仓库开发 | `clat` |
| 无头运行器 | 脚本、CI、git hook、编辑器集成 | `clat exec` |
| Web 工作台 | 可安装的本地 PWA 与 HTTP+SSE 客户端 | `clat serve` |
| DSH 客户端 | 用 CLAT TUI 连接本地 DeepSeek Harness 宿主 | `clat dsh` |
| 离线演示 | 无凭据验证核心循环 | `clat demo` |

`clat serve` 默认只绑定 `127.0.0.1:2691`。API 使用持久化的
`~/.clat/web-token` Bearer 凭据，token 不进入 URL；同一个二进制还会
直接服务响应式三栏 PWA。

## 已包含的能力

- **智能体工作流**——无固定轮次上限的 模型 → 工具 → 模型 循环、运行中
  插话、向用户提问、每会话 todo、自动标题，以及保留原始日志的上下文压缩。
- **模型**——DeepSeek、GLM、Qwen、Kimi 内置预设，命名自定义档案，
  OpenAI Responses 与 OpenAI 兼容协议，思考/用量/缓存/上下文/配额遥测。
- **原生工具**——有界的文件列表、读取、搜索、原子写入、精确编辑，以及
  能管理整个进程树的命令执行。
- **权限**——Read Only、Project Write、Full Access 三档，完整参数审查，
  项目信任、路径围栏，以及无头场景的失败关闭。
- **会话**——`~/.clat` 下可从崩溃恢复的追加式 DSH 兼容日志、本地 replay
  与按项目恢复当前会话。
- **扩展**——stdio / Streamable HTTP MCP、沙箱化 WebAssembly 组件，
  以及 DSH 叶子插件适配器。
- **前端中立核心**——TUI、无头运行器、本地服务和未来客户端共享同一个
  Application 门面与事件词汇。

## 安装

macOS / Linux：

```bash
curl -fsSL https://raw.githubusercontent.com/artec/clat/main/install.sh | sh
```

Windows（PowerShell）：

```powershell
irm https://raw.githubusercontent.com/artec/clat/main/install.ps1 | iex
```

安装器优先使用预编译发行产物，没有对应产物时回退源码构建。预编译
覆盖 macOS arm64 / x86_64、Windows x86_64 / arm64，以及 glibc 2.39+
的 Linux x86_64 / aarch64。更老的 Linux 可用稳定版 Rust 工具链源码
构建。信任模型与平台基线见[发布签名](docs/releasing.md)。

预编译版本在 macOS/Linux 安装到 `~/.local/bin/clat`，在 Windows 安装到
`%LOCALAPPDATA%\clat\bin\clat.exe`；PATH 未包含该目录时安装器会给出
提示。源码回退使用 Cargo 的 bin 目录，通常是 `~/.cargo/bin`。卸载时
删除对应可执行文件即可；`~/.clat` 用户状态会保留，除非你另行删除。

## 文档

从与你当前任务最接近的文档开始：

| 目标 | 文档 |
|---|---|
| 使用 TUI、`exec`、`serve` 或 `dsh` | [CLAT 使用指南](docs/usage.md) |
| 配置预设或自定义模型 | [模型编辑器](docs/model-editor.md) |
| 理解审批、权限档位与路径边界 | [权限](docs/permissions.md) |
| 配置 MCP server | [MCP 集成](docs/mcp.md) |
| 安装或编写 WASM 组件 | [WASM 插件](docs/wasm.md) |
| 移植 DSH 叶子插件 | [DSH 插件移植指南](docs/dsh-plugins.md) |
| 理解核心边界与生命周期 | [架构](docs/architecture.md) |
| 理解 Provider 适配与重试 | [Providers](docs/providers.md) |
| 理解文件、会话日志与恢复 | [持久化状态](docs/storage.md) |
| 构建和发布版本 | [发布签名](docs/releasing.md) |
| 运行带真实凭据的冒烟验证 | [真实模型验证](docs/live-validation.md) |

DSH 适配器包另有独立的[英文](sdk/dsh-adapter/README.md)和
[中文](sdk/dsh-adapter/README.zh.md)包文档。

## 开发

前置条件只有 Git 与当前稳定版 Rust 工具链：

```bash
git clone https://github.com/artec/clat.git
cd clat
cargo test --all-targets --all-features
cargo build
./target/debug/clat demo
```

常用仓库路径：

| 路径 | 用途 |
|---|---|
| `src/` | Rust 核心与各前端 |
| `web/` | `clat serve` 内嵌的零构建 Web 资源 |
| `wit/` | WASM 插件契约 |
| `sdk/clat-plugin/` | WASM 插件作者使用的 Rust SDK |
| `sdk/dsh-adapter/` | DSH 插件作者使用的 npm 适配器 |
| `plugins/` | WASM 示例与试点插件 |

真实 Provider 检查不会进入常规测试套件，因为它需要用户凭据且可能产生
费用。Provider 行为在改动范围内时，请按[真实模型验证](docs/live-validation.md)
执行。贡献者和编码智能体还应阅读项目宪法 [AGENTS.md](AGENTS.md)。

## 原则

本地优先 · 单一二进制 · 模型无关 · MCP 原生 · 项目感知 · 权限优先 ·
Dogfood 驱动 · 泛化而非特化。

## 许可证

[MIT](LICENSE)
