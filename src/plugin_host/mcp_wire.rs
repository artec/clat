//! MCP decoding and per-connection negotiation/pending accounting.
use super::*;
use crate::mcp::client::McpServerRequestHandler;

// ---------------------------------------------------------------------------
// MCP wire 翻译（域类型 ↔ JSON-RPC）。只在本文件：宿主桥的语义面与
// 协议面分离，将来 WIT 传输复用语义面。
// ---------------------------------------------------------------------------

/// 把宿主桥适配为 MCP 服务端请求处理器（每 server 一个，插件桥
/// Phase 1 的唯一使用方；stdio/HTTP 传输经 mcp_client 注入）。在途
/// 计数 per-handler（W1-05）：只有**本连接**正在处理的 sampling/
/// elicitation 才延长**本连接**的 tools/call 截止——共享桥的其他
/// server 与 WASM 直调不串扰（per-server failure isolation）。
pub struct McpHostHandler {
    bridge: Arc<PluginHostBridge>,
    server: String,
    pending: AtomicUsize,
    host_services_enabled: std::sync::atomic::AtomicBool,
}

impl McpHostHandler {
    pub fn new(bridge: Arc<PluginHostBridge>, server: &str) -> Self {
        Self {
            bridge,
            server: server.to_owned(),
            pending: AtomicUsize::new(0),
            host_services_enabled: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Enable the CLAT extension only after the server advertised the matching
    /// experimental capability during initialize/discover. Merely knowing a
    /// private method name is not authority to call native host tools.
    pub(crate) fn enable_host_services(&self) {
        self.host_services_enabled.store(true, Ordering::Release);
    }

    fn require_host_services(&self) -> Result<(), (i64, String)> {
        if self.host_services_enabled.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err((
                -32601,
                "CLAT host-services were not negotiated for this MCP server".into(),
            ))
        }
    }
}

impl McpServerRequestHandler for McpHostHandler {
    fn handle(&self, method: &str, params: Value) -> Result<Value, (i64, String)> {
        let _pending = PendingGuard::new(&self.pending);
        match method {
            "io.artec.clat/context/get" => {
                self.require_host_services()?;
                self.bridge.host_context().map_err(|error| error.json_rpc())
            }
            "io.artec.clat/tools/call" => {
                self.require_host_services()?;
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| (-32602, "host tool call requires string `name`".into()))?;
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if !arguments.is_object() {
                    return Err((-32602, "host tool `arguments` must be an object".into()));
                }
                self.bridge
                    .call_host_tool(PluginSource::Mcp(self.server.clone()), name, arguments)
                    .map(|output| json!({ "output": output }))
                    .map_err(|error| error.json_rpc())
            }
            "sampling/createMessage" => {
                let request = parse_sampling_params(&params)?;
                self.bridge
                    .sample(PluginSource::Mcp(self.server.clone()), request)
                    .map(|outcome| {
                        json!({
                            "role": "assistant",
                            "content": { "type": "text", "text": outcome.text },
                            "model": outcome.model,
                            "stopReason": outcome.stop_reason,
                        })
                    })
                    .map_err(|error| error.json_rpc())
            }
            "elicitation/create" => {
                let form = parse_elicitation_params(&params)?;
                self.bridge
                    .elicit(form)
                    .map(|outcome| match outcome {
                        ElicitOutcome::Accepted(content) => {
                            json!({ "action": "accept", "content": Value::Object(content) })
                        }
                        ElicitOutcome::Declined => json!({ "action": "declined" }),
                        ElicitOutcome::Cancelled => json!({ "action": "cancel" }),
                    })
                    .map_err(|error| error.json_rpc())
            }
            other => Err((
                -32601,
                format!("CLAT does not implement server request `{other}`"),
            )),
        }
    }

    fn pending_requests(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }
}

fn message_text(content: &Value) -> Result<String, (i64, String)> {
    let text_only = |block: &Value| -> Option<String> {
        (block.get("type").and_then(Value::as_str) == Some("text"))
            .then(|| block.get("text").and_then(Value::as_str).map(str::to_owned))
            .flatten()
    };
    match content {
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                match text_only(block) {
                    Some(text) => parts.push(text),
                    None => {
                        return Err((
                            -32602,
                            "sampling/createMessage: only text content blocks are supported".into(),
                        ));
                    }
                }
            }
            Ok(parts.join("\n"))
        }
        Value::Object(_) => text_only(content).ok_or_else(|| {
            (
                -32602,
                "sampling/createMessage: only text content blocks are supported".into(),
            )
        }),
        _ => Err((
            -32602,
            "sampling/createMessage: content must be a block or block array".into(),
        )),
    }
}

pub(super) fn parse_sampling_params(params: &Value) -> Result<SamplingRequest, (i64, String)> {
    let invalid = |what: &str| {
        (
            -32602_i64,
            format!("sampling/createMessage: invalid {what}"),
        )
    };
    let messages = params
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("messages (non-empty array required)"))?;
    if messages.is_empty() || messages.len() > MAX_SAMPLING_MESSAGES {
        return Err(invalid(&format!(
            "messages (1..={MAX_SAMPLING_MESSAGES} required)"
        )));
    }
    let mut parsed = Vec::with_capacity(messages.len());
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("message role"))?;
        let role = match role {
            "user" => SamplingRole::User,
            "assistant" => SamplingRole::Assistant,
            other => return Err(invalid(&format!("message role {other:?}"))),
        };
        let content = message
            .get("content")
            .ok_or_else(|| invalid("message content"))?;
        parsed.push(SamplingMessage {
            role,
            text: message_text(content)?,
        });
    }
    let max_tokens = params
        .get("maxTokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("maxTokens"))?;
    if max_tokens == 0 {
        return Err(invalid("maxTokens (must be >= 1)"));
    }
    Ok(SamplingRequest {
        system_prompt: params
            .get("systemPrompt")
            .and_then(Value::as_str)
            .map(str::to_owned),
        messages: parsed,
        max_tokens: max_tokens.min(SAMPLING_MAX_OUTPUT),
        // B7（C2）：解析进请求（宿主接受但忽略——sample() 发一次/run
        // 的 stderr 诊断，作者可见）。非字符串成员宽松过滤，不拒整次
        // 请求。WASM WIT 路径无此字段，永不触发。
        stop_sequences: params
            .get("stopSequences")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        temperature: params.get("temperature").and_then(Value::as_f64),
    })
}

pub(super) fn parse_elicitation_params(params: &Value) -> Result<ElicitForm, (i64, String)> {
    let invalid = |what: &str| (-32602_i64, format!("elicitation/create: invalid {what}"));
    if let Some(mode) = params.get("mode").and_then(Value::as_str)
        && mode != "form"
    {
        return Err(invalid(&format!("mode {mode:?} (v1 supports form only)")));
    }
    let message = params
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("message"))?
        .to_owned();
    let schema = params
        .get("requestedSchema")
        .ok_or_else(|| invalid("requestedSchema"))?;
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("requestedSchema.properties (object required)"))?;
    if properties.is_empty() {
        return Err(invalid("requestedSchema.properties (must not be empty)"));
    }
    if properties.len() > MAX_ELICIT_FIELDS {
        return Err(invalid(&format!(
            "requestedSchema.properties (at most {MAX_ELICIT_FIELDS} fields)"
        )));
    }
    let required: std::collections::HashSet<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_default();
    let mut fields = Vec::with_capacity(properties.len());
    for (name, property) in properties {
        let kind = if let Some(values) = property.get("enumValues").and_then(Value::as_array) {
            let mut options = Vec::with_capacity(values.len());
            for value in values {
                match value.as_str() {
                    Some(option) => options.push(option.to_owned()),
                    None => return Err(invalid(&format!("field `{name}` enumValues"))),
                }
            }
            if options.is_empty() || options.len() > MAX_ELICIT_OPTIONS {
                return Err(invalid(&format!(
                    "field `{name}` enumValues (1..={MAX_ELICIT_OPTIONS} required)"
                )));
            }
            ElicitFieldKind::Choice(options)
        } else {
            match property.get("type").and_then(Value::as_str) {
                Some("string") => ElicitFieldKind::Text,
                Some("number") | Some("integer") => ElicitFieldKind::Number,
                Some("boolean") => ElicitFieldKind::Boolean,
                Some(other) => {
                    return Err(invalid(&format!(
                        "field `{name}` type {other:?} (v1: string/number/boolean/enum)"
                    )));
                }
                None => return Err(invalid(&format!("field `{name}` type (missing)"))),
            }
        };
        fields.push(ElicitField {
            name: name.clone(),
            title: property
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned),
            description: property
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
            kind,
            required: required.contains(name.as_str()),
        });
    }
    Ok(ElicitForm { message, fields })
}
