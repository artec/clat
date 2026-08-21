# CLAT

[English](README.md) | 中文

**cl + at = command-line agent**

[主页](https://cl.at.cn)

CLAT 是一个快速、本地优先、开源的命令行智能体运行时，用 Rust
编写。项目从一条简单的规矩出发：先造我们自己每天真想用的智能体，
在真实仓库上 dogfood，再把这些需求泛化为可复用的开源能力。

## 特性

**智能体工作流**

- 自主检查仓库、编辑文件、运行命令来验证自己的工作——无轮次
  上限，长对话自动压缩且不丢失原始历史
- 该你拍板时问你，运行中可随时插话转向，并维护每会话待办清单
- 把图片拖进终端即可附加本地图片；视觉预设原生读取

**模型**

- 内置 DeepSeek、GLM、Qwen、Kimi 官方预设——`/model` 里选一个、
  粘上 API key 即可开始；任何 OpenAI 兼容端点也能用
- 思考档位、实时用量/缓存/上下文遥测、状态栏里的余额或配额

**工具与扩展**

- 内置文件、搜索与命令工具
- `~/.clat/mcp.json` 配置 MCP server（stdio 或 HTTP）；`/mcp` 查看
  每个 server 的状态与工具
- 沙箱化的进程内 WebAssembly 插件——单个 `.wasm` 文件，无需 Node
- DSH（DeepSeek Harness）插件作者可用约 10 行的 bin 把现成 TS
  插件以 MCP 形态对外服务——无需为 CLAT 分叉自己的代码
- GLM Coding Plan 用户自动获得四个官方 GLM MCP server

**安全**

- 三档可切换权限模式——**Read Only**、**Project Write**（默认）、
  **Full Access**——经 `/perm` 切换，或直接从权限弹窗升级
- 每个有副作用的动作都要过交互式审查、完整查看参数——TUI 与
  无头运行一视同仁

**会话**

- 会话本地持久化、可从崩溃中恢复；`/resume` 重开任一历史会话，
  模型自动起标题，`/rename` 可改名
- 会话日志采用 DSH 兼容格式——DeepSeek Harness 工具链可直接
  读取，反之亦然
- 所有状态都在 `~/.clat` 之下

**界面**

- 终端 UI：Markdown 渲染、滚动、文本选择，运行结束或等待批准时
  播放通知音
- `clat exec` 面向脚本与 CI 的无头单次运行——同一套权限模型
- `clat demo` 无需远程模型，确定性走一遍智能体循环

## 原则

- 本地优先
- 单一二进制
- 模型无关
- MCP 原生
- 项目感知
- 权限优先
- Dogfood 驱动
- 泛化而非特化

## 安装

macOS / Linux：

```bash
curl -fsSL https://raw.githubusercontent.com/artec/clat/main/install.sh | sh
```

Windows（PowerShell）：

```powershell
irm https://raw.githubusercontent.com/artec/clat/main/install.ps1 | iex
```

脚本自动识别操作系统与架构，优先使用 GitHub Releases 的预编译
二进制，无可用发行版时回退源码构建（缺 Rust 工具链时会提议
安装）。预编译覆盖 macOS（arm64、x86_64）、Windows（x86_64、
arm64）、Linux（x86_64、aarch64；glibc 2.39+——更老的发行版请
源码构建，唯一前置是 Rust 工具链）。首次安装校验 SHA-256 清单；
安装后 `clat upgrade` 额外用内嵌在二进制里的 Minisign 公钥认证
发行清单。细节见[发布签名](docs/releasing.md)。

## 快速上手

```bash
clat          # 打开 TUI，然后 /model 配置模型
clat exec "explain this repository in one sentence"   # 无头单次运行
git diff | clat exec "review this diff"               # 管道输入作为上下文
clat --help   # 用法
clat demo     # 确定性的 模型 → 工具 → 模型 循环，无需远程模型
```

## 文档

详细文档目前以英文为主（DSH 移植指南除外，它本身就是中文）。

**使用 CLAT**

- [Using the TUI](docs/usage.md) —— 面板、按键、斜杠命令、图片
  附件、通知、思考档位、无头 `clat exec`
- [Model editor](docs/model-editor.md) —— `/model` 预设（DeepSeek、
  GLM、Qwen、Kimi）与高级端点字段
- [Permissions](docs/permissions.md) —— 三档权限模式、带参数审查
  的交互批准、沙箱路径围栏

**扩展 CLAT**

- [MCP integration](docs/mcp.md) —— `~/.clat/mcp.json`、协议支持、
  server 发起的 sampling 与 elicitation、资源上限
- [WASM plugins](docs/wasm.md) —— `~/.clat/plugins.json`、
  `clat:plugin` WIT 契约、Rust 编写 SDK
- [DSH 移植指南](docs/dsh-plugins.md) —— 用 `@artec/clat-dsh-adapter`
  把现成 DeepSeek Harness TS 插件经 MCP 服务给 CLAT（中文）

**内部机制**

- [Architecture](docs/architecture.md) —— 核心/前端分层、智能体
  循环、插件宿主桥、信任门、内建工具
- [Providers](docs/providers.md) —— 协议适配器、内置预设、厂商
  特性、重试与截止
- [Persistent state](docs/storage.md) —— `~/.clat` 布局、DSH 兼容
  会话日志、崩溃恢复
- [Release signing](docs/releasing.md) —— Minisign 信任根、离线
  签名、平台基线
- [Live-model validation](docs/live-validation.md) —— 首次 dogfood
  前的两道验证门

## 开发

CLAT 是一个普通的 Rust 项目：clone、验证、构建、运行。

### 前置条件

- Git
- 当前稳定的 Rust 工具链（`rustup`、`rustc`、`cargo`）

检查工具链：

```bash
rustc --version
cargo --version
```

### 构建与测试

```bash
git clone https://github.com/artec/clat.git
cd clat
cargo test
cargo build
./target/debug/clat
```

Windows：

```powershell
.\target\debug\clat.exe
```

把当前检出版本装进 Cargo 的 bin 目录：

```bash
cargo install --path . --debug --force
```

cargo workspace 之外的仓库布局：`sdk/clat-plugin` 是 WASM 编写
SDK，`sdk/dsh-adapter` 是 npm 适配器包（非 cargo 成员），`plugins/`
存放 WASM 试点插件，`wit/` 定义插件契约。

真机模型验证有意不进常规测试套件——它需要用户自备的凭据且可能
产生费用，见 [live-model validation](docs/live-validation.md)。

贡献者与编码智能体还应阅读 [AGENTS.md](AGENTS.md)，项目宪法。

## 许可证

[MIT](LICENSE)
