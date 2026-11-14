use super::state::{MAX_TEXT, Start};
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn checked_start(start: Start, root: &Path) -> Result<Start, String> {
    if start
        .remote
        .as_ref()
        .is_some_and(|id| id.is_empty() || id.len() > 1024 || start.origin.is_some())
    {
        return Err("invalid ACP remote session identifier".into());
    }
    if start.additional_directories.len() > 8
        || start.mcp_servers.len() > 16
        || start
            .system_prompt
            .as_ref()
            .is_some_and(|text| text.trim().is_empty() || text.len() > MAX_TEXT)
    {
        return Err("invalid ACP session options".into());
    }
    for path in &start.additional_directories {
        if !path.is_absolute() {
            return Err("ACP workspace roots must be absolute".into());
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| "ACP workspace root unavailable")?;
        if canonical != *path || !path.is_dir() || !path.starts_with(root) {
            return Err("ACP workspace root must be inside the trusted project".into());
        }
    }
    for server in &start.mcp_servers {
        check_mcp(server)?;
    }
    Ok(start)
}

fn check_mcp(server: &Value) -> Result<(), String> {
    let name = server["name"]
        .as_str()
        .filter(|name| !name.is_empty() && name.len() <= 128);
    let endpoint = server["url"]
        .as_str()
        .and_then(|url| url::Url::parse(url).ok());
    if name.is_none()
        || !matches!(server["type"].as_str(), Some("http" | "sse"))
        || endpoint.is_none_or(|url| {
            url.scheme() != "https"
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || url.host_str().is_none()
        })
        || server.as_object().is_none_or(|obj| {
            obj.keys()
                .any(|key| !matches!(key.as_str(), "name" | "type" | "url" | "headers"))
        })
        || server
            .get("headers")
            .is_some_and(|head| head.as_array().is_none_or(|list| !list.is_empty()))
    {
        return Err(
            "ACP MCP sharing requires an explicitly supplied HTTPS endpoint without credentials"
                .into(),
        );
    }
    Ok(())
}

pub(super) fn session_parameters(
    capabilities: &Value,
    cwd: &str,
    start: &Start,
) -> Result<Value, String> {
    let mut params = json!({"cwd":cwd,"mcpServers":[]});
    let session = &capabilities["sessionCapabilities"];
    if !start.additional_directories.is_empty() {
        if !session["additionalDirectories"].is_object() {
            return Err("ACP Agent does not support additional directories".into());
        }
        params["additionalDirectories"] = json!(start.additional_directories);
    }
    if let Some(text) = &start.system_prompt {
        if !session["systemPrompt"].is_object() {
            return Err("ACP Agent does not support a session instruction".into());
        }
        params["systemPrompt"] = json!({"mode":"append","content":[{"type":"text","text":text}]});
    }
    let mut mcp = Vec::new();
    for server in &start.mcp_servers {
        let mode = server["type"].as_str().unwrap_or_default();
        if capabilities["mcpCapabilities"][mode] != true {
            return Err("ACP Agent does not support the selected MCP transport".into());
        }
        let mut server = server.clone();
        server["headers"] = json!([]);
        mcp.push(server);
    }
    params["mcpServers"] = json!(mcp);
    Ok(params)
}

pub(super) fn checked_content(
    capabilities: &Value,
    text: &str,
    content: &[Value],
) -> Result<Vec<Value>, String> {
    let mut blocks = Vec::new();
    if !text.trim().is_empty() {
        blocks.push(json!({"type":"text","text":text}));
    }
    for item in content {
        let field = match item["type"].as_str() {
            Some("text") if item["text"].is_string() => None,
            Some("resource_link") if item["uri"].is_string() && item["name"].is_string() => None,
            Some("image") if item["data"].is_string() && item["mimeType"].is_string() => {
                Some("image")
            }
            Some("audio") if item["data"].is_string() && item["mimeType"].is_string() => {
                Some("audio")
            }
            Some("resource")
                if item["resource"]["uri"].is_string()
                    && (item["resource"]["text"].is_string()
                        || item["resource"]["blob"].is_string()) =>
            {
                Some("embeddedContext")
            }
            _ => return Err("invalid ACP prompt content block".into()),
        };
        if field.is_some_and(|name| capabilities["promptCapabilities"][name] != true) {
            return Err("ACP Agent does not accept this content type".into());
        }
        blocks.push(item.clone());
    }
    if blocks.is_empty()
        || serde_json::to_vec(&blocks).map_or(true, |bytes| bytes.len() > super::wire::FRAME - 2048)
    {
        return Err("ACP prompt is empty or exceeds the protocol frame".into());
    }
    Ok(blocks)
}
