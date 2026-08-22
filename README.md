# CLAT

English | [中文](README.zh.md)

**cl + at = command-line agent**

[Homepage](https://cl.at.cn)

CLAT is a rock-solid agent foundation: fast, local-first, open-source,
shipped as a single Rust binary. The project starts with a simple rule:
build the agent we actually want to use every day, dogfood it on real
repositories, and generalize those needs into reusable open-source
capabilities.

## Features

**Agent workflow**

- Inspects the repository, edits files, and runs commands to verify its
  own work — no turn limits, and long conversations compact
  automatically without losing the original history
- Asks you questions when a decision is yours to make, accepts steering
  messages while a run is active, and keeps a per-session todo list
- Attach local images by dragging them into the terminal; vision presets
  read them natively

**Models**

- Official presets for DeepSeek, GLM, Qwen, and Kimi — pick one in
  `/model`, paste an API key, and start; any OpenAI-compatible endpoint
  works too
- Thinking levels, live usage/cache/context telemetry, and provider
  balance or quota in the status bar

**Tools & extensions**

- Built-in file, search, and command tools
- MCP servers from `~/.clat/mcp.json` over stdio or HTTP; `/mcp` shows
  every server's state and tools
- Sandboxed in-process WebAssembly plugins — a single `.wasm` file, no
  Node required
- DSH (DeepSeek Harness) plugin authors can serve existing TS plugins
  over MCP with a ~10-line bin — no CLAT-specific fork of their code
- GLM Coding Plan users get the four official GLM MCP servers configured
  automatically

**Safety**

- Three switchable permission modes — **Read Only**, **Project Write**
  (default), **Full Access** — via `/perm`, or escalated right from a
  permission prompt
- Every side-effecting action passes interactive review with full
  argument inspection — in the TUI and in headless runs alike

**Sessions**

- Conversations persist locally and survive crashes; `/resume` reopens
  any previous conversation, the model titles each one, `/rename` edits
  it
- Session journals use the DSH-compatible format — readable by DeepSeek
  Harness tooling and vice versa
- All state lives under `~/.clat`

**Interfaces**

- Terminal UI with markdown rendering, scrolling, text selection, and a
  notification sound when a run finishes or needs your approval
- `clat exec` for headless one-shot runs in scripts and CI, with the
  same permission model
- `clat demo` for a deterministic offline walkthrough of the agent loop

## Principles

- Local first
- One binary
- Model agnostic
- MCP native
- Project aware
- Permission first
- Dogfood driven
- Generalize, never special-case

## Install

macOS / Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/artec/clat/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/artec/clat/main/install.ps1 | iex
```

The scripts detect the operating system and architecture, prefer a
prebuilt binary from GitHub Releases, and fall back to building from
source when no release is available yet (offering to install the Rust
toolchain if it is missing). Prebuilt binaries cover macOS (arm64,
x86_64), Windows (x86_64, arm64), and Linux (x86_64, aarch64; glibc
2.39+ — older distributions build from source; a Rust toolchain is the
only requirement). First installs verify a SHA-256 manifest; once
installed, `clat upgrade` additionally authenticates release manifests
with the Minisign public key embedded in the binary. Details:
[release signing](docs/releasing.md).

## Quick start

```bash
clat          # open the TUI, then /model to configure a provider
clat exec "explain this repository in one sentence"   # headless one-shot run
git diff | clat exec "review this diff"               # piped input becomes context
clat --help   # usage
clat demo     # deterministic model → tool → model loop, no remote model needed
```

## Documentation

**Using CLAT**

- [Using the TUI](docs/usage.md) — panels, keys, slash commands, image
  attachments, notifications, thinking levels, and headless `clat exec`
- [Model editor](docs/model-editor.md) — `/model` presets (DeepSeek,
  GLM, Qwen, Kimi) and advanced endpoint fields
- [Permissions](docs/permissions.md) — the three permission modes,
  interactive approval with argument review, sandbox path fences

**Extending CLAT**

- [MCP integration](docs/mcp.md) — `~/.clat/mcp.json`, protocol
  support, server-initiated sampling & elicitation, resource limits
- [WASM plugins](docs/wasm.md) — `~/.clat/plugins.json`, the
  `clat:plugin` WIT contract, and the Rust authoring SDK
- [DSH plugin porting](docs/dsh-plugins.md) — serve existing DeepSeek
  Harness TS plugins to CLAT over MCP with `@artec/clat-dsh-adapter`

**Internals**

- [Architecture](docs/architecture.md) — core/frontend layering, the
  agent loop, the plugin host bridge, trust gate, native tools
- [Providers](docs/providers.md) — protocol adapters, built-in presets,
  vendor-specific behavior, retry and deadlines
- [Persistent state](docs/storage.md) — `~/.clat` layout, the
  DSH-compatible session journal, crash recovery
- [Release signing](docs/releasing.md) — Minisign trust root, offline
  signing, platform baseline
- [Live-model validation](docs/live-validation.md) — the two gates
  before the first dogfood run

## Development

CLAT is a normal Rust project: clone, verify, build, run.

### Prerequisites

- Git
- the current stable Rust toolchain (`rustup`, `rustc`, `cargo`)

Check the toolchain:

```bash
rustc --version
cargo --version
```

### Build and test

```bash
git clone https://github.com/artec/clat.git
cd clat
cargo test
cargo build
./target/debug/clat
```

On Windows:

```powershell
.\target\debug\clat.exe
```

Install the current checkout into Cargo's binary directory:

```bash
cargo install --path . --debug --force
```

Repository layout beyond the cargo workspace members: `sdk/clat-plugin`
is the WASM authoring SDK, `sdk/dsh-adapter` is the npm adapter package
(not a cargo member), `plugins/` holds the WASM pilot plugins, and
`wit/` defines the plugin contract.

Live model validation is intentionally not part of the normal test suite
because it requires a user-supplied provider credential and may incur
provider charges — see [live-model validation](docs/live-validation.md).

Contributors and coding agents should also read
[AGENTS.md](AGENTS.md), the project constitution.

## License

[MIT](LICENSE)
