# CLAT

**cl + at = command-line agent**

[Homepage](https://cl.at.cn)

CLAT is a fast, local-first, open-source command-line agent runtime
written in Rust. The project starts with a simple rule: build the agent
we actually want to use every day, dogfood it on real repositories, and
generalize those needs into reusable open-source capabilities.

## Features

**Agent workflow**

- Inspects the repository, edits files, and runs commands to verify its
  own work
- No turn limits — a run continues as long as the task needs it, and
  long conversations compact automatically without losing the original
  history
- Asks you questions when a decision is yours to make, and accepts
  steering messages while a run is active
- Attach local images by dragging them into the terminal — they are
  stored with the session and sent to vision models
- Keeps a per-session todo list

**Models**

- Presets for DeepSeek, GLM, Qwen, and Kimi — pick one in `/model`,
  paste an API key, and start; any OpenAI-compatible endpoint works too
- Thinking levels, live usage/cache/context telemetry, and provider
  balance or quota display in the status bar

**Tools & MCP**

- Built-in file, search, and command tools; MCP servers from
  `~/.clat/mcp.json` over stdio or HTTP
- GLM Coding Plan users get the four official GLM MCP servers configured
  automatically
- `/mcp` shows every server's connection state, registered tools, and
  failures

**Safety**

- Three switchable permission modes — **Read Only**, **Project Write**
  (default: file edits, reads, and network tools run, commands and
  destructive tools still ask), **Full Access** (also unlocks
  absolute-path writes) — switched via `/perm` or escalated right from
  a permission prompt; the mode travels with the conversation
  (journaled as DSH-compatible `sandbox/mode` events)
- Every side-effecting action still passes interactive review with full
  argument inspection in the lower modes — in the TUI and in headless
  runs alike
- Read-only tools run freely, including absolute paths outside the
  project

**Sessions**

- Conversations persist locally and survive crashes; `/resume` reopens
  any previous conversation of the project
- The model titles each conversation automatically; `/rename` edits the
  title, shown at the top of the conversation
- All state lives under `~/.clat`

**Interfaces**

- Terminal UI with markdown rendering, scrolling, and text
  selection/copy
- A bell (or a custom command via `CLAT_BELL_COMMAND`) notifies you when
  a run finishes or needs your approval
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
source when no release is available yet (they will offer to install the
Rust toolchain if it is missing). First installation needs no extra
verification tool: the scripts download over HTTPS and require a matching
SHA-256 manifest. Once installed, `clat upgrade` authenticates future release
manifests with the public key embedded in the binary.

Prebuilt binaries cover macOS (arm64, x86_64), Windows (x86_64, arm64),
and Linux (x86_64, aarch64; glibc 2.39+ — Ubuntu 24.04 generation and
newer). Older Linux distributions are outside the prebuilt baseline; build
from source instead — the tree bundles SQLite and uses rustls, so a Rust
toolchain is the only requirement.

## Quick start

```bash
clat          # open the TUI, then /model to configure a provider
clat exec "explain this repository in one sentence"   # headless one-shot run
git diff | clat exec "review this diff"               # piped input becomes context
clat --help   # usage
clat demo     # deterministic model → tool → model loop, no remote model needed
```

## Documentation

- [Architecture](docs/architecture.md) — core abstractions, agent loop,
  trust gate, native read tools
- [Using the TUI](docs/usage.md) — panels, keys, commands, markdown,
  scrolling, status line
- [Model editor](docs/model-editor.md) — provider presets (DeepSeek,
  GLM, Qwen, Kimi) and advanced fields
- [Permissions](docs/permissions.md) — safe-by-default policy,
  interactive approvals with mandatory argument review
- [MCP integration](docs/mcp.md) — `~/.clat/mcp.json`, protocol
  support, tool mapping, resource limits
- [Providers](docs/providers.md) — provider adapters and
  vendor-specific behavior (reasoning, caching, quotas)
- [Persistent state](docs/storage.md) — `~/.clat` layout, session
  journal, crash recovery, integrity guarantees
- [Release signing](docs/releasing.md) — Minisign trust root, offline
  signing, draft publishing, and key rotation
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

Live model validation is intentionally not part of the normal test suite
because it requires a user-supplied provider credential and may incur
provider charges.

## License

[MIT](LICENSE)
