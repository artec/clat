//! Core-owned single-poller host for a confirmed WeChat binding.
//!
//! The storage-root lease already guarantees one CLAT process. This host adds
//! one process-local worker, owns notify start/stop, advances the durable
//! cursor only after every delivery in a batch was handled, and uses stable
//! delivery-derived client ids so a crash-window replay is idempotent.

use super::PairingAttempt;
use super::ilink::{self, Client, Credentials, Error, InboundMessage};
use crate::TrustedProjectApplication;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Frontend-neutral delivery port. The host invokes it only after the
/// machine binding is current and the remote user is explicitly authorized.
/// Implementations must return only after they have either committed the
/// delivery or deliberately handled it; an error retains the durable cursor
/// so the same batch can be retried.
pub(crate) trait AuthorizedMessageHandler: Send + Sync {
    fn handle(&self, message: &InboundMessage) -> Result<(), String>;
}

pub(crate) fn spawn_wechat_host(
    app: Arc<Mutex<TrustedProjectApplication>>,
    credentials: Credentials,
    shutdown: Arc<AtomicBool>,
    handler: Arc<dyn AuthorizedMessageHandler>,
) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name("clat-wechat-poller".into())
        .spawn(move || {
            if let Err(error) = run_host(app, credentials, shutdown, handler) {
                eprintln!("clat: WeChat IM stopped: {error}");
            }
        })
        .map_err(|error| format!("could not start the WeChat IM poller: {error}"))
}

fn run_host(
    app: Arc<Mutex<TrustedProjectApplication>>,
    credentials: Credentials,
    shutdown: Arc<AtomicBool>,
    handler: Arc<dyn AuthorizedMessageHandler>,
) -> Result<(), String> {
    run_host_with_client(app, credentials, shutdown, handler, Client::new())
}

fn run_host_with_client(
    app: Arc<Mutex<TrustedProjectApplication>>,
    credentials: Credentials,
    shutdown: Arc<AtomicBool>,
    handler: Arc<dyn AuthorizedMessageHandler>,
    client: Client,
) -> Result<(), String> {
    match client.notify(&credentials, true) {
        Ok(()) => {}
        Err(Error::InvalidCredential) => {
            invalidate_binding_if_current(&app, &credentials)?;
            return Err("binding credential is invalid; bind again".into());
        }
        Err(error) => return Err(error.to_string()),
    }

    let mut backoff = ilink::PollBackoff::new();
    'poll: while !shutdown.load(Ordering::Acquire) {
        let Some(checkpoint) = app
            .lock()
            .expect("application lock")
            .begin_wechat_poll(&credentials)
            .map_err(|error| error.to_string())?
        else {
            break;
        };
        match client.get_updates(&credentials, checkpoint.cursor()) {
            Ok(updates) => {
                for message in &updates.messages {
                    if shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    // Unbind/rebind is an authorization revocation boundary.
                    // A long poll may have started with the old credential;
                    // re-check before every delivery so its result cannot be
                    // admitted after local revocation.
                    if app
                        .lock()
                        .expect("application lock")
                        .begin_wechat_poll(&credentials)
                        .map_err(|error| error.to_string())?
                        .is_none()
                    {
                        break 'poll;
                    }
                    if let Err(error) = handle_pairing_delivery(
                        &app,
                        &client,
                        &credentials,
                        handler.as_ref(),
                        message,
                    ) {
                        eprintln!(
                            "clat: WeChat delivery was not committed; retaining the cursor for retry: {}",
                            crate::redact::redact_secrets(&error)
                        );
                        sleep_cancelable(&shutdown, backoff.next_delay());
                        continue 'poll;
                    }
                }
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                if !app
                    .lock()
                    .expect("application lock")
                    .commit_wechat_poll(&checkpoint, &updates.cursor)
                    .map_err(|error| error.to_string())?
                {
                    break;
                }
                backoff.reset();
            }
            Err(Error::PollBackoff) => {
                sleep_cancelable(&shutdown, backoff.next_delay());
            }
            Err(Error::InvalidCredential) => {
                invalidate_binding_if_current(&app, &credentials)?;
                return Err("binding credential is invalid; bind again".into());
            }
            Err(error) => {
                eprintln!("clat: WeChat poll retrying after a bounded delay: {error}");
                sleep_cancelable(&shutdown, backoff.next_delay());
            }
        }
    }
    if let Err(error) = client.notify(&credentials, false)
        && !matches!(error, Error::InvalidCredential)
    {
        return Err(format!("notify-stop failed: {error}"));
    }
    Ok(())
}

/// Clear only the binding that produced the invalid-credential response.
///
/// A blocking poll may return after a concurrent QR replacement committed a
/// new credential. Comparing and clearing under the same Application lock
/// prevents the stale poller from revoking that replacement.
fn invalidate_binding_if_current(
    app: &Arc<Mutex<TrustedProjectApplication>>,
    expected: &Credentials,
) -> Result<(), String> {
    app.lock()
        .expect("application lock")
        .revoke_wechat_binding_if_current(expected)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn handle_pairing_delivery(
    app: &Arc<Mutex<TrustedProjectApplication>>,
    client: &Client,
    credentials: &Credentials,
    handler: &dyn AuthorizedMessageHandler,
    message: &InboundMessage,
) -> Result<(), String> {
    let user_id = message.from_user_id.trim();
    if user_id.is_empty() {
        return Ok(());
    }
    let text = standalone_text(message).unwrap_or_default();
    let Some(code) = pairing_code(text) else {
        // MR-I3 default deny. Ordinary unpaired input receives no reflection,
        // avoiding an attacker-controlled reply oracle.
        let delivery_id = delivery_id(message)?;
        if matches!(
            app.lock()
                .expect("application lock")
                .classify_wechat_delivery(user_id, &delivery_id),
            crate::application::WechatDeliveryDisposition::Accept
        ) {
            handler.handle(message)?;
        }
        return Ok(());
    };
    let delivery_id = delivery_id(message)?;
    let outcome = app
        .lock()
        .expect("application lock")
        .submit_wechat_pairing_code(&delivery_id, user_id, code)
        .map_err(|error| error.to_string())?;
    let reply = match outcome {
        PairingAttempt::Paired => "配对成功。绑定只授权当前微信用户，不扩大 CLAT 工具权限。".into(),
        PairingAttempt::AlreadyPaired => "当前微信用户已经配对。".into(),
        PairingAttempt::Invalid { remaining_attempts } => {
            format!("配对码无效；当前 5 分钟窗口还可尝试 {remaining_attempts} 次。")
        }
        PairingAttempt::Expired => "配对码已过期，请在电脑端重新生成。".into(),
        PairingAttempt::Unavailable => "当前没有可用配对码，请先在电脑端生成。".into(),
        PairingAttempt::RateLimited { retry_after_ms } => format!(
            "配对尝试过多，请约 {} 秒后重试。",
            retry_after_ms.div_ceil(1000)
        ),
    };
    if !message.context_token.is_empty() {
        send_reply(client, credentials, message, &reply)?;
    }
    Ok(())
}

fn pairing_code(text: &str) -> Option<&str> {
    let mut words = text.split_whitespace();
    if words.next()? != "/pair" {
        return None;
    }
    let code = words.next()?;
    if words.next().is_some() || code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(code)
}

fn standalone_text(message: &InboundMessage) -> Option<&str> {
    if message.item_list.len() != 1 {
        return None;
    }
    let item = message.item_list.first()?;
    if item.kind != 1 || item.image_item.is_some() {
        return None;
    }
    Some(item.text_item.as_ref()?.text.trim())
}

pub(crate) fn delivery_id(message: &InboundMessage) -> Result<String, String> {
    let bytes =
        if message.message_id.is_some() || message.client_id.is_some() || message.seq.is_some() {
            serde_json::to_vec(&(
                &message.message_id,
                &message.client_id,
                &message.seq,
                &message.from_user_id,
            ))
        } else {
            // Some iLink-compatible relays omit every documented wire id. In
            // that degraded shape, include the complete inbound payload so two
            // different messages from one user cannot collapse onto one durable
            // replay key. Exact redelivery remains deterministic.
            serde_json::to_vec(&("ilink-payload-v1", message))
        }
        .map_err(|error| format!("could not encode delivery identity: {error}"))?;
    let digest = Sha256::digest(bytes);
    Ok(format!("ilink:{digest:x}"))
}

fn send_reply(
    client: &Client,
    credentials: &Credentials,
    message: &InboundMessage,
    text: &str,
) -> Result<(), String> {
    let delivery_id = delivery_id(message)?;
    client
        .send_text(
            credentials,
            &message.from_user_id,
            &message.context_token,
            &format!("clat-{delivery_id}"),
            text,
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn sleep_cancelable(shutdown: &AtomicBool, duration: Duration) {
    let deadline = std::time::Instant::now() + duration;
    while !shutdown.load(Ordering::Acquire) {
        let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
            break;
        };
        std::thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::im::ilink::{InboundItem, Request, Response, TextItem, Transport, WireId};
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;

    fn message(text: &str) -> InboundMessage {
        InboundMessage {
            message_id: Some(WireId::String("message-secret".into())),
            client_id: Some(WireId::String("client-secret".into())),
            seq: Some(WireId::Number(7.into())),
            from_user_id: "user-secret".into(),
            context_token: "context-secret".into(),
            item_list: vec![InboundItem {
                kind: 1,
                text_item: Some(TextItem { text: text.into() }),
                image_item: None,
            }],
        }
    }

    #[test]
    fn pairing_command_is_exact_and_not_embedded_natural_language() {
        assert_eq!(pairing_code("/pair 012345"), Some("012345"));
        assert_eq!(pairing_code("  /pair 012345  "), Some("012345"));
        assert_eq!(pairing_code("please /pair 012345"), None);
        assert_eq!(pairing_code("/pair 012345 extra"), None);
        assert_eq!(pairing_code("/pair 12345"), None);
        assert_eq!(pairing_code("/PAIR 012345"), None);
        let mut mixed = message("/pair 012345");
        mixed.item_list.push(InboundItem::default());
        assert_eq!(standalone_text(&mixed), None);
    }

    #[test]
    fn delivery_key_is_stable_and_hides_wire_identity() {
        let first = delivery_id(&message("/pair 012345")).unwrap();
        let second = delivery_id(&message("different text")).unwrap();
        assert_eq!(
            first, second,
            "wire identity, not message text, keys replay"
        );
        assert!(!first.contains("secret"));
        assert_eq!(first.len(), "ilink:".len() + 64);
    }

    #[test]
    fn delivery_key_falls_back_to_payload_when_wire_identity_is_absent() {
        let mut first = message("first body");
        first.message_id = None;
        first.client_id = None;
        first.seq = None;
        let mut second = first.clone();
        second.item_list[0].text_item.as_mut().unwrap().text = "second body".into();

        assert_ne!(delivery_id(&first).unwrap(), delivery_id(&second).unwrap());
        assert_eq!(delivery_id(&first).unwrap(), delivery_id(&first).unwrap());
    }

    #[test]
    fn cancelable_sleep_observes_shutdown_without_waiting_full_backoff() {
        let shutdown = AtomicBool::new(true);
        let started = std::time::Instant::now();
        sleep_cancelable(&shutdown, Duration::from_secs(32));
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    struct CountingHandler(AtomicUsize);

    impl AuthorizedMessageHandler for CountingHandler {
        fn handle(&self, _message: &InboundMessage) -> Result<(), String> {
            self.0.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    struct ScriptedTransport {
        responses: Mutex<VecDeque<(Response, bool)>>,
        shutdown: Arc<AtomicBool>,
    }

    impl Transport for ScriptedTransport {
        fn execute(&self, _request: Request) -> Result<Response, Error> {
            let (response, stop) = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| Error::Transport("script exhausted".into()))?;
            if stop {
                self.shutdown.store(true, Ordering::Release);
            }
            Ok(response)
        }
    }

    struct DurableOnceHandler {
        app: Arc<Mutex<TrustedProjectApplication>>,
        shutdown: Arc<AtomicBool>,
        commits: AtomicUsize,
        interrupt_once: AtomicBool,
    }

    impl AuthorizedMessageHandler for DurableOnceHandler {
        fn handle(&self, message: &InboundMessage) -> Result<(), String> {
            let delivery = delivery_id(message)?;
            let application = self.app.lock().expect("application lock");
            if matches!(
                application.classify_wechat_delivery(&message.from_user_id, &delivery),
                crate::application::WechatDeliveryDisposition::Accept
            ) {
                application
                    .commit_wechat_delivery(&delivery)
                    .map_err(|error| error.to_string())?;
                self.commits.fetch_add(1, Ordering::AcqRel);
            }
            if self.interrupt_once.swap(false, Ordering::AcqRel) {
                self.shutdown.store(true, Ordering::Release);
            }
            Ok(())
        }
    }

    fn json_response(value: serde_json::Value) -> Response {
        Response {
            status: 200,
            body: serde_json::to_vec(&value).unwrap(),
        }
    }

    fn transport(
        shutdown: Arc<AtomicBool>,
        responses: impl IntoIterator<Item = (serde_json::Value, bool)>,
    ) -> Client {
        Client::with_transport(Arc::new(ScriptedTransport {
            responses: Mutex::new(
                responses
                    .into_iter()
                    .map(|(value, stop)| (json_response(value), stop))
                    .collect(),
            ),
            shutdown,
        }))
    }

    #[test]
    fn ordinary_delivery_reaches_the_frontend_only_after_explicit_authorization() {
        let (storage_root, project_root) = crate::test_support::roots("wechat-host-auth");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = crate::Project::new(&project_root);
        let application = crate::BootstrapApplication::open(project, storage_root.clone())
            .unwrap()
            .authorize_and_mount(crate::ProjectAuthorization::grant())
            .unwrap();
        let credentials = Credentials::new(
            "test-token".into(),
            "test-bot".into(),
            None,
            "https://ilinkai.weixin.qq.com".into(),
        )
        .unwrap();
        application.replace_wechat_binding(&credentials).unwrap();
        let app = Arc::new(Mutex::new(application));
        let handler = CountingHandler(AtomicUsize::new(0));
        let ordinary = message("hello");

        handle_pairing_delivery(&app, &Client::new(), &credentials, &handler, &ordinary).unwrap();
        assert_eq!(handler.0.load(Ordering::Acquire), 0);

        app.lock()
            .unwrap()
            .set_wechat_allowlist("user-secret", true)
            .unwrap();
        handle_pairing_delivery(&app, &Client::new(), &credentials, &handler, &ordinary).unwrap();
        assert_eq!(handler.0.load(Ordering::Acquire), 1);

        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn stale_invalid_credential_response_cannot_clear_a_replacement_binding() {
        let (storage_root, project_root) =
            crate::test_support::roots("wechat-host-stale-invalid-credential");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = crate::Project::new(&project_root);
        let application = crate::BootstrapApplication::open(project, storage_root.clone())
            .unwrap()
            .authorize_and_mount(crate::ProjectAuthorization::grant())
            .unwrap();
        let old = Credentials::new(
            "old-token".into(),
            "old-bot".into(),
            None,
            "https://ilinkai.weixin.qq.com".into(),
        )
        .unwrap();
        let replacement = Credentials::new(
            "replacement-token".into(),
            "replacement-bot".into(),
            None,
            "https://ilinkai.weixin.qq.com".into(),
        )
        .unwrap();
        application.replace_wechat_binding(&old).unwrap();
        application.replace_wechat_binding(&replacement).unwrap();
        let app = Arc::new(Mutex::new(application));

        invalidate_binding_if_current(&app, &old).unwrap();
        assert_eq!(
            app.lock().unwrap().wechat_binding().unwrap().credentials,
            Some(replacement.clone()),
            "a stale poller must not revoke the newly confirmed binding"
        );

        invalidate_binding_if_current(&app, &replacement).unwrap();
        assert!(
            app.lock()
                .unwrap()
                .wechat_binding()
                .unwrap()
                .credentials
                .is_none()
        );

        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn shutdown_after_commit_retains_cursor_and_restart_replay_does_not_resubmit() {
        let (storage_root, project_root) = crate::test_support::roots("wechat-host-restart");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = crate::Project::new(&project_root);
        let application = crate::BootstrapApplication::open(project, storage_root.clone())
            .unwrap()
            .authorize_and_mount(crate::ProjectAuthorization::grant())
            .unwrap();
        let credentials = Credentials::new(
            "test-token".into(),
            "test-bot".into(),
            None,
            "https://ilinkai.weixin.qq.com".into(),
        )
        .unwrap();
        application.replace_wechat_binding(&credentials).unwrap();
        application
            .set_wechat_allowlist("user-secret", true)
            .unwrap();
        let app = Arc::new(Mutex::new(application));
        let shutdown = Arc::new(AtomicBool::new(false));
        let handler = Arc::new(DurableOnceHandler {
            app: Arc::clone(&app),
            shutdown: Arc::clone(&shutdown),
            commits: AtomicUsize::new(0),
            interrupt_once: AtomicBool::new(true),
        });
        let wire_message = serde_json::to_value(message("compile the project")).unwrap();
        let updates = serde_json::json!({
            "get_updates_buf": "cursor-1",
            "msgs": [wire_message],
        });

        run_host_with_client(
            Arc::clone(&app),
            credentials.clone(),
            Arc::clone(&shutdown),
            handler.clone(),
            transport(
                Arc::clone(&shutdown),
                [
                    (serde_json::json!({}), false),
                    (updates.clone(), false),
                    (serde_json::json!({}), false),
                ],
            ),
        )
        .unwrap();
        assert_eq!(handler.commits.load(Ordering::Acquire), 1);
        assert_eq!(
            app.lock()
                .unwrap()
                .begin_wechat_poll(&credentials)
                .unwrap()
                .unwrap()
                .cursor(),
            ""
        );

        shutdown.store(false, Ordering::Release);
        run_host_with_client(
            Arc::clone(&app),
            credentials,
            Arc::clone(&shutdown),
            handler.clone(),
            transport(
                Arc::clone(&shutdown),
                [
                    (serde_json::json!({}), false),
                    (updates, false),
                    (
                        serde_json::json!({ "get_updates_buf": "cursor-1", "msgs": [] }),
                        true,
                    ),
                    (serde_json::json!({}), false),
                ],
            ),
        )
        .unwrap();
        assert_eq!(handler.commits.load(Ordering::Acquire), 1);
        assert_eq!(
            app.lock()
                .unwrap()
                .begin_wechat_poll(
                    &Credentials::new(
                        "test-token".into(),
                        "test-bot".into(),
                        None,
                        "https://ilinkai.weixin.qq.com".into(),
                    )
                    .unwrap()
                )
                .unwrap()
                .unwrap()
                .cursor(),
            "cursor-1"
        );

        drop(handler);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }
}
