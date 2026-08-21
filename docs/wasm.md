# WASM plugins (Phase 2a)

CLAT can run WebAssembly components as in-process plugins — the third
transport of the plugin host bridge (stdio / HTTP / WIT). One contract,
three transports: a WASM plugin speaks the same semantics as an MCP
server, minus the process and the protocol plumbing.

- Distribution is a **single `.wasm` file** (the pilot `digest` plugin is
  ~160 KB) — cross-platform, no Node, no per-platform binaries.
- Plugins run **sandboxed with capability-based grants**: no environment
  variables, closed stdio, no usable network (sockets exist but every
  address is denied — networked plugins should be MCP servers). File
  access is granted through preopens that mirror the active permission
  mode (Phase 2b): the project root is always readable; plugins that
  declare `write`/`execute`/`destructive` tools additionally get it
  read-write under **Project Write**, plus their configured `dirs`
  read-write under **Full Access**. Grants are re-evaluated on every
  tool call — switching `/perm` rebuilds the instance, so a mode change
  takes effect on the next call (in-memory plugin state does not
  survive the rebuild; persist to a granted directory instead).
  What a plugin *can* do beyond files is declared by what it imports:
  the host bridge's `sampling` (run the configured model —
  permission-gated, usage-accounted) and `elicitation` (ask the user —
  sequential single-question dialogs). A plugin that imports neither
  and only declares read tools is effectively a pure function.
- Every tool call is bounded by a fuel budget (fuel is consumed only
  while the component itself executes — waiting on a user dialog or a
  model call costs nothing) and memory-capped (256 MB); a spinning or
  leaking plugin fails as a tool error, never kills the run or the
  host. The run's **cancel token is an execution-time capability**: an
  epoch interruption traps the component within milliseconds of `Esc`,
  so a spinning plugin cannot outlast your cancel until its fuel runs
  out (host waits still cost neither fuel nor interruption).

## Configuration

`~/.clat/plugins.json` (absent file = no plugins, zero cost):

```json
{
  "digest": { "path": "~/.clat/plugins/digest.wasm" },
  "read": {
    "path": "~/.clat/plugins/read.wasm",
    "dirs": ["/Volumes/Data"]
  }
}
```

`path` accepts `~/…` (expanded against your home directory), relative
paths (against `~/.clat`), or absolute paths. `dirs` lists extra
directories granted read-write **only** under Full Access (and only to
plugins that declare write-capable tools); in the guest they appear
under their (sanitized) directory name — address them as
`/<dirname>/…` while the project root is the default (`project/…`). Plugins appear in `/mcp`
alongside MCP servers (transport `wasm`), and their tools are named
`wasm_{plugin}_{tool}` (e.g. `wasm_digest_digest`).

## The contract (`wit/plugin.wit`)

World `clat:plugin@0.1.0` mirrors the MCP leaf semantics:

- **export `tools`** — `list-tools()` returns tool definitions
  (`name`, `description`, JSON-Schema `input-schema`, `effect`); 
  `call(name, arguments-json)` returns a JSON string or an error string.
  `effect` uses CLAT's classification (`pure`…`session-write`) and
  drives permission gating exactly like tool effects elsewhere.
- **import `sampling`** (optional) — `create-message(request)` runs the
  host's configured model on the plugin's behalf. Permission-gated (a
  dialog unless Full Access), usage-accounted.
- **import `elicitation`** (optional) — `elicit(form)` asks the user a
  primitive form (text / number / boolean / choice fields); the answer
  comes back typed, or `declined` / `cancelled`.

## Writing a plugin (Rust, with the SDK)

The author-facing path is the `clat-plugin` SDK crate
(`sdk/clat-plugin` in this repository). `plugins/greeter` is the
minimal template — copy it and go:

```rust
wit_bindgen::generate!({ path: "../../wit", world: "plugin" });

#[derive(Deserialize)]
struct GreetArgs { name: String }

fn greet_impl(args: GreetArgs) -> Result<GreetOut, String> { /* … */ }

clat_plugin::define_plugin! {
    tool "greet" desc("Greets using the configured greeting.")
        effect(Pure) schema(GREET_SCHEMA) args(GreetArgs) call(greet_impl);
}
```

The DSL generates the `Guest` implementation, the tool listing, and the
JSON argument/result plumbing; handlers take typed args and return any
`Serialize` value. Host calls are plain functions
(`clat::plugin::sampling::create_message`,
`clat::plugin::elicitation::elicit`), and `plugin_config::<T>()` reads
the plugin's own `config` object from `plugins.json`:

```json
{ "greeter": { "path": "…/greeter.wasm",
               "config": { "greeting": "Hola", "upper": true } } }
```

A plugin sees only its own config (never the host environment or other
plugins'); an unconfigured plugin gets an error, not an empty value.
The SDK also declares the dependency versions validated with the host
(wit-bindgen 0.43 / serde 1 / serde_json 1) — proc-macro path rules
mean `wit-bindgen` and `serde` (derive) must still be direct
dependencies; copy the versions from `plugins/greeter/Cargo.toml`.

The raw wit-bindgen path (implement the generated `Guest` trait by
hand) remains fully supported — see `plugins/probe` and `plugins/read`.

Build (once per machine): `rustup target add wasm32-wasip2`, then
`cargo build --release --target wasm32-wasip2` and point `plugins.json`
at the `.wasm` artifact under `target/wasm32-wasip2/release/`.

## Status

Phase 2a (runtime + contract + pilot plugins), Phase 2b (capability
grants + the `read` tool port), and Phase 2c (the `clat-plugin` SDK +
per-plugin config) are delivered; see `docs/todo/wasm-plugin-runtime.md`
for invariants and the batching plan.
