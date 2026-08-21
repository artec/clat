//! MCP (Model Context Protocol) 协议栈：`transport` 拥有 stdio/HTTP
//! 传输（行分帧 JSON-RPC 2.0），`client` 拥有客户端会话、握手协商与
//! 远端工具到 [`crate::tool::Tool`] 的适配。挂载侧插件见
//! `plugins/mcp.rs`。

mod transport;

pub(crate) mod client;
