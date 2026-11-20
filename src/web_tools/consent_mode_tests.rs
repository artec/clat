//! Behavioral regression for session-scoped native web address consent.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct FakeIpTransport {
    calls: AtomicUsize,
}

impl WebTransport for FakeIpTransport {
    fn execute(
        &self,
        request: &HttpRequest,
        cancel: &CancelToken,
    ) -> Result<HttpResponse, WebError> {
        check(cancel)?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        if request.consented.is_empty() {
            return Err(WebError::new("blocked"));
        }
        assert_eq!(
            request.consented,
            vec![std::net::IpAddr::from([198, 18, 15, 96])]
        );
        Ok(HttpResponse {
            status: 200,
            content_type: "text/plain".into(),
            location: None,
            body: b"news".to_vec(),
        })
    }
}

#[test]
fn full_access_auto_consents_fake_ip_without_persistent_host_grants() {
    let root = std::env::temp_dir().join(format!("clat-web-consent-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    crate::control_storage::sentinel::initialize(&root).unwrap();
    let control = Arc::new(crate::control_storage::ControlStorage::open_ready(&root).unwrap());
    let mode = Arc::new(std::sync::RwLock::new(crate::PermissionMode::FullAccess));
    let transport = Arc::new(FakeIpTransport::default());
    let service = WebService::with_transport(control, transport.clone())
        .with_permission_mode(Some(mode.clone()));
    *service.resolve_hook.lock().unwrap() = Some(Arc::new(|_| {
        Some(vec![std::net::IpAddr::from([198, 18, 15, 96])])
    }));
    // No user asker installed: Full Access itself is the grant.
    let request = HttpRequest {
        url: "https://en.aletihad.ae/".into(),
        method: "GET",
        headers: vec![],
        body: vec![],
        timeout: TIMEOUT,
        consented: vec![],
    };
    let cancel = CancelToken::new();
    assert_eq!(
        service
            .consent_execute(request.clone(), &cancel, &cancel)
            .unwrap()
            .status,
        200
    );
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    assert!(!root.join("web-consents.json").exists());

    // Session downgrade is effective on the next request. Full Access must
    // not silently save per-host approvals for another session or mode.
    *mode.write().unwrap() = crate::PermissionMode::ProjectWrite;
    let denied = service
        .consent_execute(request.clone(), &cancel, &cancel)
        .unwrap_err();
    assert_eq!(denied.code, "private_denied");
    assert_eq!(transport.calls.load(Ordering::SeqCst), 3);
    *mode.write().unwrap() = crate::PermissionMode::ReadOnly;
    assert_eq!(
        service
            .consent_execute(request.clone(), &cancel, &cancel)
            .unwrap_err()
            .code,
        "private_denied"
    );
    *mode.write().unwrap() = crate::PermissionMode::FullAccess;
    assert_eq!(
        service
            .consent_execute(request, &cancel, &cancel)
            .unwrap()
            .status,
        200
    );
    assert!(!root.join("web-consents.json").exists());
    drop(service);
    std::fs::remove_dir_all(root).unwrap();
}
