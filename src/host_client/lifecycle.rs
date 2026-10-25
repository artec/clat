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
        self.execute_presented()
            .map(|m| m.english())
            .map_err(|e| e.raw)
    }
    pub fn execute_presented(&self) -> Result<crate::i18n::UiMessage, crate::i18n::PresentedText> {
        self.execute_with(|| match self.port {
            Some(port) => {
                let root = crate::control_storage::sentinel::default_storage_root()?;
                HostClient::connect_for_management(
                    port,
                    super::credentials::read_token(&root)?,
                    &root,
                )
            }
            None => HostClient::discover_local_for_management(),
        })
    }

    fn execute_with(
        &self,
        connect: impl FnOnce() -> Result<HostClient, crate::i18n::PresentedText>,
    ) -> Result<UiMessage, crate::i18n::PresentedText> {
        let client = connect()?;
        if self.stop {
            let result = client
                .call_with_receipt("host.stop", &json!({}))
                .map_err(|e| e.presented())?;
            if result["stopping"] != true {
                return Err(UiMessage::new("host.stop_unacknowledged").into());
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
    fn offline_management_discovery_keeps_localized_reason_without_retry() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let error = HostClient::connect_checked(
            port,
            "fixture-token".into(),
            &std::env::temp_dir(),
            None,
            crate::host_client::BuildMatchPolicy::Ignore,
        )
        .err()
        .unwrap()
        .presented();
        assert_eq!(error.raw, "host is offline; no operation was retried");
        assert_eq!(error.message().key, "host.offline_no_retry");
        let connections = std::cell::Cell::new(0);
        let command = HostManagementArgs {
            stop: true,
            port: Some(port),
        };
        let managed = command
            .execute_with(|| {
                connections.set(connections.get() + 1);
                Err(error.clone())
            })
            .unwrap_err();
        assert_eq!(connections.get(), 1);
        assert_eq!(managed, error);
        for locale in [
            crate::i18n::Locale::Simplified,
            crate::i18n::Locale::Traditional,
        ] {
            assert_ne!(
                error.render(crate::i18n::LocaleContext::new(locale)),
                error.raw
            );
            let shown = crate::i18n::LocaleContext::new(locale)
                .render(&UiMessage::new("common.error").arg_message("detail", managed.message()));
            assert!(!shown.contains("host is offline"));
        }
    }

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
