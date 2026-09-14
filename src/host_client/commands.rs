//! Client-only command presentation. Host mutations remain authoritative RPCs.
use super::HostClient;

impl HostClient {
    pub fn is_detach_command(text: &str) -> bool {
        matches!(text.trim(), "/quit" | "/exit")
    }

    pub fn is_permission_dialog_command(text: &str) -> bool {
        matches!(text.trim(), "/perm" | "/permission")
    }

    /// Caller has completed the explicit full-access confirmation dialog.
    pub fn set_confirmed_permission(
        &self,
        mode: crate::PermissionMode,
        selection: u64,
    ) -> Result<serde_json::Value, String> {
        self.call(
            "permission.set",
            &serde_json::json!({
                "mode":mode.journal_value(),
                "confirm":mode.journal_value(),
                "expected_selection_generation":selection
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_commands_are_exact_and_local() {
        for command in ["/quit", " /exit "] {
            assert!(HostClient::is_detach_command(command));
        }
        for command in ["quit", "/quit now", "/exit-host", "/cancel"] {
            assert!(!HostClient::is_detach_command(command));
        }
    }

    #[test]
    fn compact_requests_use_host_api_and_selection_fence() {
        use serde_json::{Value, json};
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for action in ["start", "cancel", "danger-full-access", "clear"] {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let method = match action {
                    "danger-full-access" => "permission.set",
                    "clear" => "session.new",
                    _ => "session.compact",
                };
                assert_eq!(
                    request,
                    format!("POST /workspace/default/api/{method} HTTP/1.1\r\n")
                );
                let mut size = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        size = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; size];
                reader.read_exact(&mut body).unwrap();
                let params: Value = serde_json::from_slice(&body).unwrap();
                let expected = if action == "danger-full-access" {
                    json!({"mode":action,"confirm":action,"expected_selection_generation":17})
                } else if action == "clear" {
                    json!({"expected_selection_generation":17})
                } else {
                    json!({"action":action,"expected_selection_generation":17})
                };
                assert_eq!(params, expected);
                let reply = if action != "cancel" {
                    json!({"ok":true,"value":{"status":"started"}})
                } else {
                    json!({"ok":false,"error":{"code":"selection_changed","message":"stale selection"}})
                }.to_string();
                write!(
                    reader.get_mut(),
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            }
        });
        let root = std::env::temp_dir();
        let client = HostClient {
            port,
            token: "test-token".into(),
            prefix: "/workspace/default".into(),
            instance: String::new(),
            root: root.clone(),
            clipboard: std::sync::Arc::new(crate::draft::DraftImageStore::new(&root)),
        };
        assert_eq!(
            client.submit_text(" /compact ", false, 17).unwrap()["status"],
            "started"
        );
        assert!(
            client
                .submit_text("/compact cancel", false, 17)
                .unwrap_err()
                .to_string()
                .contains("stale selection")
        );
        client
            .set_confirmed_permission(crate::PermissionMode::FullAccess, 17)
            .unwrap();
        client.submit_text("/clear", false, 17).unwrap();
        server.join().unwrap();
    }
}
