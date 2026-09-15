//! Explicit host management. Never opens a writer or retries a stop request.
use super::HostClient;
use serde_json::json;

#[derive(Debug, PartialEq, Eq)]
pub struct HostManagementArgs {
    pub stop: bool,
    pub port: Option<u16>,
}

impl HostManagementArgs {
    pub fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let stop = match args.next().as_deref() {
            Some("status") => false,
            Some("stop") => true,
            _ => return Err("use clat host status|stop [--port <n>]".into()),
        };
        let mut port = None;
        if let Some(arg) = args.next() {
            if arg != "--port" {
                return Err("host accepts only --port <n>".into());
            }
            port = Some(
                args.next()
                    .and_then(|s| s.parse().ok())
                    .filter(|p| *p != 0)
                    .ok_or("host --port needs a nonzero port")?,
            );
        }
        if args.next().is_some() {
            return Err("unexpected host argument".into());
        }
        Ok(Self { stop, port })
    }

    pub fn execute(&self) -> Result<String, String> {
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
            Ok(format!(
                "Host {} accepted shutdown; all connected frontends will disconnect.",
                client.instance_id()
            ))
        } else {
            Ok(format!(
                "Host {} online at 127.0.0.1:{}\nStorage: {}",
                client.instance_id(),
                client.port,
                client.storage_root().display()
            ))
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
