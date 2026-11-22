use super::*;
use serde_json::{Value, json};
use tokenplan_quota::{DeviceLogin, PollStatus};

struct Flow {
    id: Uuid,
    status: &'static str,
    view: Option<Value>,
}

static FLOWS: Mutex<Option<HashMap<PathBuf, Flow>>> = Mutex::new(None);

impl TrustedProjectApplication {
    pub fn console_login_start(&self) -> Result<Value, ApplicationError> {
        let root = self.control.root_path();
        let id = Uuid::new_v4();
        {
            let mut flows = FLOWS.lock().expect("console device flows");
            let flows = flows.get_or_insert_with(HashMap::new);
            if let Some(flow) = flows.get(root) {
                match flow.status {
                    "pending" => return Ok(flow.view.clone().expect("pending device view")),
                    "starting" => return Err(ApplicationError::new("console login is starting")),
                    _ => {}
                }
            }
            flows.insert(
                root.to_owned(),
                Flow {
                    id,
                    status: "starting",
                    view: None,
                },
            );
        }
        let attempt = self.begin_console_login(id);
        if attempt.is_err() {
            settle_flow(&self.control, id, Err("failed"));
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
        let mut flows = FLOWS.lock().expect("console device flows");
        let flow = current_flow(&mut flows, &self.control, id)
            .ok_or_else(|| ApplicationError::new("console login was cancelled"))?;
        flow.status = "pending";
        flow.view = Some(view.clone());
        let control = Arc::clone(&self.control);
        let subscribers = Arc::clone(&self.subscribers);
        std::thread::Builder::new()
            .name("clat-console-device-login".into())
            .spawn(move || {
                let result = poll_login(&control, id, &login);
                if settle_flow(&control, id, result) {
                    notify_changed(&subscribers);
                }
            })
            .map_err(|_| ApplicationError::new("console device worker could not start"))?;
        Ok(view)
    }

    pub fn console_login_state(&self) -> Value {
        let flows = FLOWS.lock().expect("console device flows");
        let status = flows
            .as_ref()
            .and_then(|map| map.get(self.control.root_path()))
            .map_or("none", |flow| flow.status);
        json!({"status": status})
    }

    pub fn console_login_cancel(&self) {
        let mut flows = FLOWS.lock().expect("console device flows");
        if let Some(flows) = flows.as_mut()
            && flows
                .get(self.control.root_path())
                .is_some_and(|flow| matches!(flow.status, "starting" | "pending"))
        {
            flows.remove(self.control.root_path());
        }
    }
}

pub(super) fn remove(control: &ControlStorage) -> Result<(), ApplicationError> {
    let mut flows = FLOWS.lock().expect("console device flows");
    remove_credential(control, "qianwen")?;
    if let Some(flows) = flows.as_mut() {
        flows.remove(control.root_path());
    }
    Ok(())
}

fn current_flow<'a>(
    flows: &'a mut Option<HashMap<PathBuf, Flow>>,
    control: &ControlStorage,
    id: Uuid,
) -> Option<&'a mut Flow> {
    flows
        .as_mut()?
        .get_mut(control.root_path())
        .filter(|flow| flow.id == id)
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
) -> Result<ConsoleCredentialRow, &'static str> {
    let transport = UreqTransport::new(FETCH_TIMEOUT);
    let deadline = Instant::now() + Duration::from_secs(login.expires_in_secs.clamp(1, 900));
    let mut interval = Duration::from_secs(login.interval_secs.clamp(1, 900));
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining <= interval {
            std::thread::sleep(remaining);
            return Err("expired");
        }
        std::thread::sleep(interval);
        if current_flow(
            &mut FLOWS.lock().expect("console device flows"),
            control,
            id,
        )
        .is_none()
        {
            return Err("cancelled");
        }
        match tokenplan_quota::poll_device_login(
            &transport,
            login,
            tokenplan_quota::DEFAULT_AUTH_BASE,
        ) {
            Ok(PollStatus::Complete(session)) => return seal_session(&transport, login, session),
            Ok(PollStatus::ExpiredToken) => return Err("expired"),
            Ok(PollStatus::AccessDenied) => return Err("denied"),
            Ok(PollStatus::AuthorizationPending) => {}
            Ok(PollStatus::SlowDown) => interval += Duration::from_secs(5),
            Err(_) => return Err("failed"),
        }
    }
}

fn seal_session(
    transport: &UreqTransport,
    login: &DeviceLogin,
    session: Session,
) -> Result<ConsoleCredentialRow, &'static str> {
    let derived = tokenplan_quota::derive_cli_access_token(
        transport,
        &session,
        tokenplan_quota::DEFAULT_GATEWAY_JSON,
    )
    .map_err(|_| "failed")?;
    let credential = QianwenCredential {
        access_token: session.access_token,
        refresh_token: session.refresh_token,
        expires_at: session.expires_at,
        derived,
    };
    let plain = serde_json::to_string(&credential).map_err(|_| "failed")?;
    let blob = seal(&plain).map_err(|_| "failed")?;
    Ok(ConsoleCredentialRow {
        client_id: Some(login.client_id.clone()),
        blob,
        updated_at: None,
    })
}

fn settle_flow(
    control: &ControlStorage,
    id: Uuid,
    result: Result<ConsoleCredentialRow, &'static str>,
) -> bool {
    let mut flows = FLOWS.lock().expect("console device flows");
    let Some(flow) = current_flow(&mut flows, control, id) else {
        return false;
    };
    flow.view = None;
    flow.status = match result {
        Ok(row) if control.write_console_credential("qianwen", &row).is_ok() => {
            invalidate_cache(control, "qianwen");
            "complete"
        }
        Ok(_) => "failed",
        Err(status) => status,
    };
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_authorization_removal_fences_late_device_approval() {
        let (root, _) = crate::test_support::roots("console-device-removed");
        std::fs::create_dir_all(&root).unwrap();
        crate::control_storage::sentinel::initialize(&root).unwrap();
        let control = ControlStorage::open_ready(&root).unwrap();
        let id = Uuid::new_v4();
        FLOWS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(
                root.clone(),
                Flow {
                    id,
                    status: "pending",
                    view: None,
                },
            );
        remove(&control).unwrap();
        let late = ConsoleCredentialRow {
            blob: "late-device-token".into(),
            ..Default::default()
        };
        assert!(!settle_flow(&control, id, Ok(late)));
        assert!(
            control
                .read_console_credential("qianwen")
                .unwrap()
                .is_none()
        );
        crate::test_support::cleanup_tree(root.parent().unwrap());
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
