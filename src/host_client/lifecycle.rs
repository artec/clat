//! Explicit host management. Never opens a writer or retries a stop request.
use super::HostClient;
use crate::i18n::UiMessage;
use serde_json::json;

#[derive(Debug, PartialEq, Eq)]
pub struct HostManagementArgs {
    pub stop: bool,
    pub port: Option<u16>,
}

impl HostManagementArgs {
    pub fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        Self::parse_presented(args).map_err(|m| m.english())
    }
    pub fn parse_presented(mut args: impl Iterator<Item = String>) -> Result<Self, UiMessage> {
        let stop = match args.next().as_deref() {
            Some("status") => false,
            Some("stop") => true,
            _ => return Err(UiMessage::new("cli.host_usage")),
        };
        let mut port = None;
        if let Some(arg) = args.next() {
            if arg != "--port" {
                return Err(UiMessage::new("cli.host_only_port"));
            }
            port = Some(
                args.next()
                    .and_then(|s| s.parse().ok())
                    .filter(|p| *p != 0)
                    .ok_or(UiMessage::new("cli.host_nonzero_port"))?,
            );
        }
        if args.next().is_some() {
            return Err(UiMessage::new("cli.host_extra_argument"));
        }
        Ok(Self { stop, port })
    }

    pub fn execute(&self) -> Result<String, String> {
        self.execute_presented().map(|m| m.english())
    }
    pub fn execute_presented(&self) -> Result<crate::i18n::UiMessage, String> {
        let client = match self.port {
            Some(port) => {
                let root = crate::control_storage::sentinel::default_storage_root()?;
                HostClient::connect_for_management(
                    port,
                    super::credentials::read_token(&root)?,
                    &root,
                )?
            }
            None => HostClient::discover_local_for_management()?,
        };
        if self.stop {
            let result = client.call("host.stop", &json!({}))?;
            if result["stopping"] != true {
                return Err("host did not acknowledge shutdown; do not retry automatically".into());
            }
            Ok(crate::i18n::UiMessage::new("cli.host_stopping")
                .arg("instance", client.instance_id()))
        } else {
            Ok(crate::i18n::UiMessage::new("cli.host_status")
                .arg("instance", client.instance_id())
                .arg("port", client.port)
                .arg("root", client.storage_root().display().to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_management_requires_explicit_action_and_valid_port() {
        let parse = |args: &[&str]| HostManagementArgs::parse(args.iter().map(|s| s.to_string()));
        assert_eq!(
            parse(&["status"]).unwrap(),
            HostManagementArgs {
                stop: false,
                port: None
            }
        );
        assert_eq!(
            parse(&["stop", "--port", "4321"]).unwrap(),
            HostManagementArgs {
                stop: true,
                port: Some(4321)
            }
        );
        for args in [
            vec![],
            vec!["restart"],
            vec!["stop", "--force"],
            vec!["stop", "--port", "0"],
            vec!["stop", "--port"],
            vec!["stop", "--port", "65536"],
            vec!["stop", "status"],
            vec!["stop", "--port", "4321", "extra"],
        ] {
            assert!(
                parse(&args).is_err(),
                "must reject {args:?} before connecting"
            );
        }
    }
}
