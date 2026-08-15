# CLAT

**cl + at = command-line agent**

CLAT is a fast, local-first, open-source command-line agent runtime
written in Rust. The project starts with a simple rule: build the agent
we actually want to use every day, dogfood it on real repositories, and
generalize those needs into reusable open-source capabilities.

## Status

Early development. The first milestone is a useful single-agent coding
workflow that can work on real projects such as ECAR and CLAT itself.

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
Rust toolchain if it is missing).

## Quick start

```bash
clat          # open the TUI, then /model to configure a provider
clat --help   # usage
clat demo     # deterministic model → tool → model loop, no remote model needed
```

## Documentation

- [Architecture](docs/architecture.md) — core abstractions, model
  protocol, conversation model, trust gate, native read tools
- [Using the TUI](docs/usage.md) — panels, keys, commands, markdown,
  scrolling, status line
- [Model editor](docs/model-editor.md) — presets (DeepSeek V4.0
  Flash/Pro), advanced fields
- [Permissions](docs/permissions.md) — safe-by-default policy,
  interactive approvals with mandatory argument review
- [MCP integration](docs/mcp.md) — `~/.clat/mcp.json`, protocol
  support, tool mapping, resource limits
- [Providers](docs/providers.md) — OpenAI Responses and Compatible
  adapters, DeepSeek reasoning replay
- [Persistent state](docs/storage.md) — `~/.clat` layout, contents, and
  integrity guarantees
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
