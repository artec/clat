//! clat probe 插件（插件桥 Phase 2a 的测试件，非 dogfood）。
//!
//! `probe` 工具把宿主 sampling / elicitation 的往返结果回显为 JSON；
//! `spin` 工具死循环——宿主的 epoch deadline 必须把它打断成工具错误
//! （INV-W3），否则门控测试红。

wit_bindgen::generate!({
    path: "../../wit",
    world: "plugin",
});

use crate::clat::plugin::elicitation::{Field, FieldKind, Form, Outcome, Value};
use crate::clat::plugin::sampling::{Message, Request, Role};
use crate::exports::clat::plugin::tools::{Definition, Effect, Guest};
use serde::Deserialize;

const PROBE_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "sample": { "type": "boolean" },
    "elicit": { "type": "boolean" },
    "context": { "type": "boolean" },
    "read_path": { "type": "string" },
    "text": { "type": "string" }
  }
}"#;

const SPIN_SCHEMA: &str = r#"{"type": "object"}"#;

const NET_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "host": { "type": "string" },
    "port": { "type": "integer" }
  },
  "required": ["host", "port"]
}"#;

const ENV_SCHEMA: &str = r#"{"type": "object"}"#;

const ALLOC_SCHEMA: &str = r#"{"type": "object"}"#;

#[derive(Deserialize, Default)]
struct ProbeArgs {
    #[serde(default)]
    sample: bool,
    #[serde(default)]
    elicit: bool,
    #[serde(default)]
    context: bool,
    #[serde(default)]
    read_path: Option<String>,
    #[serde(default = "default_text")]
    text: String,
}

fn default_text() -> String {
    "probe".to_owned()
}

struct ProbePlugin;

impl Guest for ProbePlugin {
    fn list_tools() -> Vec<Definition> {
        vec![
            Definition {
                name: "probe".to_owned(),
                description: "Test fixture: round-trips host sampling and elicitation \
                              through the plugin host bridge."
                    .to_owned(),
                input_schema: PROBE_SCHEMA.to_owned(),
                effect: Effect::Execute,
            },
            Definition {
                name: "spin".to_owned(),
                description: "Test fixture: spins forever; the host must interrupt it.".to_owned(),
                input_schema: SPIN_SCHEMA.to_owned(),
                effect: Effect::Execute,
            },
            Definition {
                name: "net".to_owned(),
                description:
                    "Test fixture: attempts a TCP connect (the sandbox must deny every address)."
                        .to_owned(),
                input_schema: NET_SCHEMA.to_owned(),
                effect: Effect::Network,
            },
            Definition {
                name: "env".to_owned(),
                description: "Test fixture: lists visible environment variables (must be none)."
                    .to_owned(),
                input_schema: ENV_SCHEMA.to_owned(),
                effect: Effect::Pure,
            },
            Definition {
                name: "alloc".to_owned(),
                description: "Test fixture: grows memory until the 256 MiB cap traps.".to_owned(),
                input_schema: ALLOC_SCHEMA.to_owned(),
                effect: Effect::Pure,
            },
        ]
    }

    fn call(name: String, arguments: String) -> Result<String, String> {
        match name.as_str() {
            "probe" => probe(arguments),
            "spin" => {
                let mut counter = 0u64;
                loop {
                    counter = counter.wrapping_add(1);
                    std::hint::black_box(counter);
                }
            }
            "net" => net(arguments),
            "env" => env_probe(),
            "alloc" => {
                let mut sink: Vec<u8> = Vec::new();
                loop {
                    let chunk = vec![0u8; 1024 * 1024];
                    sink.extend(chunk);
                    std::hint::black_box(&sink);
                }
            }
            other => Err(format!("unknown tool `{other}`")),
        }
    }
}

#[derive(Deserialize)]
struct NetArgs {
    host: String,
    port: u16,
}

fn net(arguments: String) -> Result<String, String> {
    let args: NetArgs =
        serde_json::from_str(&arguments).map_err(|error| format!("invalid arguments: {error}"))?;
    let payload = match std::net::TcpStream::connect((args.host.as_str(), args.port)) {
        Ok(_) => serde_json::json!({ "connected": true }),
        Err(error) => serde_json::json!({ "connected": false, "error": error.to_string() }),
    };
    serde_json::to_string(&payload).map_err(|error| format!("serialize: {error}"))
}

fn env_probe() -> Result<String, String> {
    let vars: Vec<(String, String)> = std::env::vars().collect();
    serde_json::to_string(&serde_json::json!({ "count": vars.len(), "vars": vars }))
        .map_err(|error| format!("serialize: {error}"))
}

fn probe(arguments: String) -> Result<String, String> {
    let args: ProbeArgs =
        serde_json::from_str(&arguments).map_err(|error| format!("invalid arguments: {error}"))?;
    let mut parts: Vec<(String, serde_json::Value)> = Vec::new();

    if args.context {
        let raw = clat::plugin::host::context()
            .map_err(|error| format!("host context error: {error}"))?;
        let context =
            serde_json::from_str(&raw).map_err(|error| format!("invalid host context: {error}"))?;
        parts.push(("context".to_owned(), context));
    }

    if let Some(path) = args.read_path {
        let arguments = serde_json::to_string(&serde_json::json!({ "path": path }))
            .map_err(|error| format!("serialize host tool arguments: {error}"))?;
        let raw = clat::plugin::host::call_tool("read_file", &arguments)
            .map_err(|error| format!("host read error: {error}"))?;
        let output = serde_json::from_str(&raw)
            .map_err(|error| format!("invalid host read output: {error}"))?;
        parts.push(("host_read".to_owned(), output));
    }

    if args.sample {
        match clat::plugin::sampling::create_message(&Request {
            system_prompt: Some("You are a one-word echo.".to_owned()),
            messages: vec![Message {
                role: Role::User,
                text: args.text.clone(),
            }],
            max_tokens: 32,
            temperature: None,
        }) {
            Ok(outcome) => {
                parts.push((
                    "sampling".to_owned(),
                    serde_json::json!({
                        "text": outcome.text,
                        "model": outcome.model,
                        "stop_reason": outcome.stop_reason,
                    }),
                ));
            }
            Err(error) => {
                parts.push(("sampling".to_owned(), serde_json::json!({ "error": error })));
            }
        }
    }

    if args.elicit {
        let outcome = clat::plugin::elicitation::elicit(&Form {
            message: "probe form".to_owned(),
            fields: vec![
                Field {
                    name: "flavor".to_owned(),
                    title: Some("Flavor".to_owned()),
                    description: None,
                    kind: FieldKind::Choice,
                    options: vec!["vanilla".to_owned(), "pistachio".to_owned()],
                    required: true,
                },
                Field {
                    name: "servings".to_owned(),
                    title: None,
                    description: Some("how many".to_owned()),
                    kind: FieldKind::Number,
                    options: Vec::new(),
                    required: true,
                },
            ],
        })
        .map_err(|error| format!("elicitation error: {error}"))?;
        match outcome {
            Outcome::Accepted(values) => {
                let map: serde_json::Map<String, serde_json::Value> = values
                    .into_iter()
                    .map(|(name, value)| {
                        let value = match value {
                            Value::Text(text) => serde_json::json!(text),
                            Value::Number(number) => serde_json::json!(number),
                            Value::Boolean(flag) => serde_json::json!(flag),
                        };
                        (name, value)
                    })
                    .collect();
                parts.push(("elicit".to_owned(), serde_json::Value::Object(map)));
            }
            Outcome::Declined => {
                parts.push(("elicit".to_owned(), serde_json::json!("declined")));
            }
            Outcome::Cancelled => {
                parts.push(("elicit".to_owned(), serde_json::json!("cancelled")));
            }
        }
    }

    let object = serde_json::Map::from_iter(parts);
    serde_json::to_string(&serde_json::Value::Object(object))
        .map_err(|error| format!("serialize: {error}"))
}

export!(ProbePlugin);
