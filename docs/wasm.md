# WASM plugins

CLAT can run WebAssembly components as in-process tools. A component implements
the `clat:plugin@0.1.0` WIT contract and is distributed as one `.wasm` file.
End users do not need Node.js, Python, or a platform-specific plugin binary.

Choose WASM for portable local computation with a narrow capability set. Use
[MCP](mcp.md) when the extension needs unrestricted networking, a long-lived
service, an existing language runtime, or an out-of-process isolation boundary.

## Runtime guarantees

Configured components run inside Wasmtime with no ambient authority:

- no inherited environment variables;
- closed stdin/stdout/stderr;
- no usable sockets or network addresses;
- only explicitly preopened filesystem directories;
- 256 MiB memory limit;
- a fresh fuel budget for every tool call;
- epoch interruption connected to the run cancel token.

Fuel is consumed only while guest instructions execute. Waiting for a host
model call or user question consumes neither fuel nor a wall-clock execution
budget. Cancelling a run interrupts a spinning guest promptly and also aborts
host waits.

## Configuration

Create `~/.clat/plugins.json`. Absence means no components and no WASM startup
cost.

```json
{
  "digest": {
    "path": "~/.clat/plugins/digest.wasm"
  },
  "greeter": {
    "path": "./plugins/greeter.wasm",
    "config": { "greeting": "Hola", "upper": true }
  },
  "workspace-tools": {
    "path": "/opt/clat/workspace-tools.wasm",
    "dirs": ["/Volumes/Data"]
  }
}
```

`path` accepts:

- `~/...`, expanded against the user's home directory;
- a relative path, resolved from `~/.clat`;
- an absolute path.

`config` is private to that plugin. The SDK's `plugin_config::<T>()` helper
deserializes it; one plugin cannot see another plugin's object or host
environment. A plugin that expects missing configuration receives an explicit
error.

`dirs` lists extra host directories that may become read-write only under Full
Access and only for a component declaring write-capable tools. It does not
grant anything by itself.

Restart CLAT after changing the file. Components appear in `/mcp` with
transport `wasm`; tools are named `wasm_{plugin}_{tool}`.

## Filesystem capability model

Every component receives the project root as guest path `project`, initially
read-only. Extra directories are preopened under a sanitized basename, falling
back to `dirN` on an empty or colliding name.

The effective preopens are the intersection of the component's declared tool
effects, current permission mode, configured directories, and stored write
grant:

| Mode/client | Project preopen | Extra `dirs` |
|---|---|---|
| Read Only | read-only | absent |
| Project Write | read-write only after grant for a write-capable component | absent |
| Full Access | read-write only after grant for a write-capable component | read-write after the same exact-set grant |
| `clat exec` classic mode | read-only | absent |

Components declaring only read/pure tools never receive write authority,
regardless of mode. A mode change rebuilds the instance before the next call so
capabilities change immediately. In-memory guest state does not survive that
rebuild; persist necessary state to a granted directory.

## Filesystem write grants

Permission mode describes how much the user trusts the agent. It does not imply
trust in globally installed third-party component code. Write preopens
therefore require a separate approval bound to:

- plugin name;
- component SHA-256 digest;
- exact host directory set that would become writable.

The first write-capable use asks with the digest and directories. Approval is
recorded in `~/.clat/plugin-grants.json`. Rebuilding the component, renaming it,
or adding a directory changes the identity and asks again. A previously granted
superset also covers a later subset.

A deny leaves that plugin's preopens read-only for the rest of the run. The next
run asks again. Headless operation without a matching record cannot prompt and
therefore remains read-only. Failure to persist an allow affects only reuse:
the current run proceeds, and a later run asks again.

Delete a matching record—or the file—to revoke. A missing or malformed grants
file is treated as no grants and does not prevent CLAT startup.

## WIT contract

The world in `wit/plugin.wit` exposes one required export and two optional host
imports.

### Export: tools

`list-tools()` returns definitions containing:

- name and description;
- JSON Schema input;
- one CLAT effect (`pure`, `read`, `write`, `execute`, `network`,
  `external-read`, `destructive`, or `session-write`).

`call(name, arguments-json)` returns a JSON string or an error string. The
effect enters the same permission table as native and MCP tools; declaring it
accurately is part of the component's security contract.

### Import: sampling

`sampling.create-message(request)` borrows the active session model through the
shared plugin-host bridge. It is permission-gated, usage-accounted, cancellable,
and subject to the shared per-run sampling limits described in
[MCP sampling](mcp.md#server-initiated-sampling).

### Import: elicitation

`elicitation.elicit(form)` asks primitive text, number, boolean, or single-choice
fields through the frontend's user-question port. The result is typed, declined,
or cancelled. Headless clients without a question frontend return an error.

A component importing neither host service and declaring only pure/read tools
is effectively a local bounded computation with read-only project access.

## Authoring with the Rust SDK

The author SDK lives at `sdk/clat-plugin`. `plugins/greeter` is the smallest
template.

```rust
wit_bindgen::generate!({ path: "../../wit", world: "plugin" });

#[derive(serde::Deserialize)]
struct GreetArgs {
    name: String,
}

fn greet_impl(args: GreetArgs) -> Result<String, String> {
    Ok(format!("Hello, {}", args.name))
}

clat_plugin::define_plugin! {
    tool "greet" desc("Greets one person.")
        effect(Pure) schema(GREET_SCHEMA) args(GreetArgs) call(greet_impl);
}
```

The macro generates the WIT `Guest` implementation, tool listing, JSON
argument decoding, and result serialization. Handlers accept typed arguments
and can return any serializable value.

Host imports are ordinary generated functions:

```rust
clat::plugin::sampling::create_message(request);
clat::plugin::elicitation::elicit(form);
```

The SDK pins versions validated with the host. Because proc-macro resolution
still requires direct dependencies, plugin crates should copy the
`wit-bindgen`, `serde`, and `serde_json` versions from
`plugins/greeter/Cargo.toml`.

Implementing the generated `Guest` trait by hand remains supported; see
`plugins/probe` and `plugins/read` for lower-level examples.

## Build and install

Install the WASI Preview 2 target once:

```bash
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2
```

Point the plugin's `path` at
`target/wasm32-wasip2/release/<name>.wasm`, restart CLAT, and inspect `/mcp`.

For distribution, publish the component file plus its checksum and document:

- tool names and effects;
- configuration schema;
- imported host services;
- files/directories it expects;
- whether writes require Project Write or Full Access.

## Failure behavior

- A missing, malformed, incompatible, or uninstantiable component fails only
  that plugin and appears in `/mcp` diagnostics.
- A fuel, memory, or epoch trap becomes a tool error; it does not kill CLAT.
- Invalid tool JSON or an unknown tool name becomes a bounded tool error.
- A denied write grant gives the guest physical read-only preopens, so a buggy
  tool cannot bypass the policy by ignoring its own return path.
- Mode changes and grant changes rebuild the guest instance, intentionally
  invalidating in-memory state.

These failure boundaries make WASM suitable for local extensions, but not a
substitute for reviewing third-party code and declared effects.
