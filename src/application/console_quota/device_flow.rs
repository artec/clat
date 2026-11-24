use super::login_flow::{DEVICE, LoginFailure, Start};
use super::*;
use serde_json::{Value, json};
use tokenplan_quota::{DeviceLogin, PollStatus};

impl TrustedProjectApplication {
    pub fn console_login_start(&self) -> Result<Value, ApplicationError> {
        let id = match DEVICE.start(&self.control)? {
            Start::New(id) => id,
            Start::Pending(view) => return Ok(view),
        };
        let attempt = self.begin_console_login(id);
        if attempt.is_err() {
            DEVICE.settle(&self.control, id, Err(LoginFailure::Failed));
        }
        attempt
    }

    fn begin_console_login(&self, id: Uuid) -> Result<Value, ApplicationError> {
        let client_id = self
            .control
            .read_console_credential("qianwen")
            .map_err(ApplicationError::new)?
            .and_then(|row| row.client_id.filter(|id| !id.trim().is_empty()))
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let transport = UreqTransport::new(FETCH_TIMEOUT);
        let login = tokenplan_quota::start_device_login(
            &transport,
            &client_id,
            tokenplan_quota::DEFAULT_AUTH_BASE,
        )
        .map_err(|_| ApplicationError::new("console device authorization could not start"))?;
        if !valid_verification_url(&login.verification_url) {
            return Err(ApplicationError::new("invalid console verification URL"));
        }
        let view = json!({"url": login.verification_url, "expires_in": login.expires_in_secs, "interval": login.interval_secs});
        DEVICE.publish(&self.control, id, view.clone(), None)?;
        let control = Arc::clone(&self.control);
        let subscribers = Arc::clone(&self.subscribers);
        std::thread::Builder::new()
            .name("clat-console-device-login".into())
            .spawn(move || {
                let result = poll_login(&control, id, &login);
                if DEVICE.settle(&control, id, result) {
                    notify_changed(&subscribers);
                }
            })
            .map_err(|_| ApplicationError::new("console device worker could not start"))?;
        Ok(view)
    }

    pub fn console_login_state(&self) -> Value {
        json!({"status": DEVICE.status(&self.control)})
    }

    pub fn console_login_cancel(&self) {
        DEVICE.cancel(&self.control);
    }
}

fn valid_verification_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url
                .host_str()
                .is_some_and(|host| host == "qianwenai.com" || host.ends_with(".qianwenai.com"))
    })
}

fn poll_login(
    control: &ControlStorage,
    id: Uuid,
    login: &DeviceLogin,
) -> Result<ConsoleCredentialRow, LoginFailure> {
    let transport = UreqTransport::new(FETCH_TIMEOUT);
    let deadline = Instant::now() + Duration::from_secs(login.expires_in_secs.clamp(1, 900));
    let mut interval = Duration::from_secs(login.interval_secs.clamp(1, 900));
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining <= interval {
            std::thread::sleep(remaining);
            return Err(LoginFailure::Expired);
        }
        std::thread::sleep(interval);
        if !DEVICE.is_pending(control, id) {
            return Err(LoginFailure::Cancelled);
        }
        match tokenplan_quota::poll_device_login(
            &transport,
            login,
            tokenplan_quota::DEFAULT_AUTH_BASE,
        ) {
            Ok(PollStatus::Complete(session)) => return seal_session(&transport, login, session),
            Ok(PollStatus::ExpiredToken) => return Err(LoginFailure::Expired),
            Ok(PollStatus::AccessDenied) => return Err(LoginFailure::Denied),
            Ok(PollStatus::AuthorizationPending) => {}
            Ok(PollStatus::SlowDown) => interval += Duration::from_secs(5),
            Err(_) => return Err(LoginFailure::Failed),
        }
    }
}

fn seal_session(
    transport: &UreqTransport,
    login: &DeviceLogin,
    session: Session,
) -> Result<ConsoleCredentialRow, LoginFailure> {
    let derived = tokenplan_quota::derive_cli_access_token(
        transport,
        &session,
        tokenplan_quota::DEFAULT_GATEWAY_JSON,
    )
    .map_err(|_| LoginFailure::Failed)?;
    let credential = QianwenCredential {
        access_token: session.access_token,
        refresh_token: session.refresh_token,
        expires_at: session.expires_at,
        derived,
    };
    let plain = serde_json::to_string(&credential).map_err(|_| LoginFailure::Failed)?;
    let blob = seal(&plain).map_err(|_| LoginFailure::Failed)?;
    Ok(ConsoleCredentialRow {
        client_id: Some(login.client_id.clone()),
        blob,
        updated_at: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settled_device_login_cannot_be_settled_again() {
        let (root, _) = crate::test_support::roots("console-device-settled");
        std::fs::create_dir_all(&root).unwrap();
        crate::control_storage::sentinel::initialize(&root).unwrap();
        let control = ControlStorage::open_ready(&root).unwrap();
        let id = new_login(&control);
        assert!(DEVICE.settle(&control, id, Err(LoginFailure::Expired)));
        assert!(!DEVICE.settle(&control, id, Err(LoginFailure::Failed)));
        assert_eq!(DEVICE.status(&control), "expired");
        DEVICE.remove(&control).unwrap();
        crate::test_support::cleanup_tree(root.parent().unwrap());
    }

    #[test]
    fn console_authorization_removal_fences_late_device_approval() {
        let (root, _) = crate::test_support::roots("console-device-removed");
        std::fs::create_dir_all(&root).unwrap();
        crate::control_storage::sentinel::initialize(&root).unwrap();
        let control = ControlStorage::open_ready(&root).unwrap();
        let id = new_login(&control);
        DEVICE.publish(&control, id, json!({}), None).unwrap();
        DEVICE.remove(&control).unwrap();
        let late = ConsoleCredentialRow {
            blob: "late-device-token".into(),
            ..Default::default()
        };
        assert!(!DEVICE.settle(&control, id, Ok(late)));
        assert!(
            control
                .read_console_credential("qianwen")
                .unwrap()
                .is_none()
        );
        crate::test_support::cleanup_tree(root.parent().unwrap());
    }

    fn new_login(control: &ControlStorage) -> Uuid {
        let Start::New(id) = DEVICE.start(control).unwrap() else {
            panic!("expected a new device login");
        };
        id
    }

    #[test]
    fn console_authorization_link_stays_on_qianwen_https() {
        assert!(valid_verification_url("https://account.qianwenai.com/sso"));
        assert!(!valid_verification_url(
            "https://qianwenai.com.example.test/sso"
        ));
        assert!(!valid_verification_url(
            "https://account.qianwenai.com@example.test/sso"
        ));
        assert!(!valid_verification_url("http://account.qianwenai.com/sso"));
    }
}
