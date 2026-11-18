use super::*;

fn exchange(addr: SocketAddr, host: &str, headers: &str, path: &str) -> String {
    let mut stream = connect(addr);
    let body = if path == "/auth" { "" } else { "{}" };
    write!(stream, "POST {path} HTTP/1.1\r\nHost: {host}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply
}

#[test]
fn computer_use_routes_require_local_owner_and_current_host() {
    use crate::host_client::HostClient;
    let (handle, storage, project) = spawn_serve("computer-use-owner", TestBehavior::Success);
    let host = HostClient::connect(handle.addr.port(), TEST_TOKEN.into(), &storage).unwrap();
    let mut stream = host.computer_use_events().unwrap();
    assert!(stream.next_computer_use().unwrap().activities.is_empty());
    assert!(
        !host
            .stop_computer_use(&crate::native_tools::ui_activity::ComputerUseTarget {
                run_id: "stale".into(),
                project_root: project.to_string_lossy().into_owned(),
                session_id: "old session".into(),
            })
            .unwrap()
    );
    let authority = handle.addr.to_string();
    let bearer = format!("Authorization: Bearer {TEST_TOKEN}\r\n");
    let generation = format!("X-Clat-Host-Instance: {}\r\n", host.instance_id());
    for headers in [
        generation.clone(),
        bearer.clone(),
        format!("{bearer}X-Clat-Host-Instance: replaced\r\n"),
        format!(
            "{bearer}{generation}Origin: http://{authority}\r\nSec-Fetch-Site: same-origin\r\n"
        ),
    ] {
        let response = exchange(handle.addr, &authority, &headers, "/api/computer-use/stop");
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    }
    handle.shutdown();
    assert!(stream.next_computer_use().is_err());
    cleanup(handle, &storage, &project);
}

#[test]
fn computer_use_feed_echoes_stop_for_one_turn_and_keeps_other_projects_live() {
    use crate::host_client::HostClient;
    use crate::native_tools::ui_activity::{ComputerUseKind, ComputerUseTarget};

    let mut activity = None;
    let (handle, storage, project) = spawn_serve_observed(
        "computer-use-stream",
        TestBehavior::Success,
        super::super::state::SUBSCRIBER_QUEUE_FRAMES,
        |app| activity = Some(app.computer_use_activity_for_test()),
    );
    let registry = activity.unwrap();
    let host = HostClient::connect(handle.addr.port(), TEST_TOKEN.into(), &storage).unwrap();
    let mut events = host.computer_use_events().unwrap();
    assert!(events.next_computer_use().unwrap().activities.is_empty());
    let target = |run: &str, root: &Path| ComputerUseTarget {
        run_id: run.into(),
        project_root: root.to_string_lossy().into_owned(),
        session_id: "same-id-in-both-projects".into(),
    };
    let first = target("one", &project);
    let second = target("two", &project.join("other-project"));
    let turn = crate::CancelToken::new();
    let first_ui = turn.child();
    let second_ui = crate::CancelToken::new();
    let _read = registry
        .start(
            &first,
            &first_ui,
            &first_ui,
            ComputerUseKind::Read,
            Some("Editor"),
        )
        .unwrap();
    let _wait = registry
        .start(
            &second,
            &second_ui,
            &second_ui,
            ComputerUseKind::Wait,
            Some("Viewer"),
        )
        .unwrap();
    let mut rows = events.next_computer_use().unwrap().activities;
    if rows.len() == 1 {
        rows = events.next_computer_use().unwrap().activities;
    }
    assert_eq!(rows.len(), 2);
    assert_eq!((&rows[0].target, &rows[1].target), (&second, &first));

    assert!(host.stop_computer_use(&first).unwrap());
    assert!(!host.stop_computer_use(&first).unwrap());
    let remaining = events.next_computer_use().unwrap().activities;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].target, second);
    assert!(first_ui.is_cancelled() && !turn.is_cancelled() && !second_ui.is_cancelled());
    drop(events);
    cleanup(handle, &storage, &project);
}

#[test]
fn local_auth_silently_mints_and_remote_fences_issue_nothing() {
    let (handle, storage, project) = spawn_serve("ux1-local-auth", TestBehavior::Success);
    let host = handle.addr.to_string();
    let local = format!("Origin: http://{host}\r\nSec-Fetch-Site: same-origin\r\n");
    let reply = exchange(handle.addr, &host, &local, "/auth");
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(
        reply.contains("Set-Cookie:"),
        "local auth must issue a session"
    );
    assert!(reply.contains("HttpOnly") && reply.contains("SameSite=Strict"));
    assert!(!reply.contains(TEST_TOKEN));
    assert!(
        exchange(handle.addr, &host, &local, "/api/workbench.info").starts_with("HTTP/1.1 200")
    );
    for (authority, headers) in [
        (
            host.as_str(),
            "Origin: http://evil.example\r\nSec-Fetch-Site: same-origin\r\n".to_owned(),
        ),
        (host.as_str(), "Sec-Fetch-Site: cross-site\r\n".to_owned()),
        ("evil.example", String::new()),
        (host.as_str(), format!("Origin: http://{host}\r\n")),
    ] {
        let rejected = exchange(handle.addr, authority, &headers, "/auth");
        assert!(rejected.starts_with("HTTP/1.1 403"), "{rejected}");
        assert!(!rejected.contains("Set-Cookie:"));
    }
    cleanup(handle, &storage, &project);
}

#[test]
fn local_auth_remote_same_origin_requires_valid_credentials() {
    let (handle, storage, project) = spawn_serve("ux1-remote-auth", TestBehavior::Success);
    let headers = "Origin: http://remote.example\r\nSec-Fetch-Site: same-origin\r\n";
    let denied = exchange(handle.addr, "remote.example", headers, "/auth");
    assert!(denied.starts_with("HTTP/1.1 403"));
    let allowed = exchange(
        handle.addr,
        "remote.example",
        &format!("{headers}Authorization: Bearer {TEST_TOKEN}\r\n"),
        "/auth",
    );
    assert!(allowed.starts_with("HTTP/1.1 200"), "{allowed}");
    let cookie = allowed
        .lines()
        .find_map(|line| line.strip_prefix("Set-Cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let resumed = exchange(
        handle.addr,
        "remote.example",
        &format!("{headers}Cookie: {cookie}\r\n"),
        "/auth",
    );
    assert!(resumed.starts_with("HTTP/1.1 200"), "{resumed}");
    let api = exchange(
        handle.addr,
        "remote.example",
        &format!("{headers}Cookie: {cookie}\r\n"),
        "/api/workbench.info",
    );
    assert!(
        api.starts_with("HTTP/1.1 200") && api.contains("\"value\":"),
        "{api}"
    );
    let wrong_authority = exchange(
        handle.addr,
        "other.example",
        &format!(
            "Origin: http://other.example\r\nSec-Fetch-Site: same-origin\r\nCookie: {cookie}\r\n"
        ),
        "/auth",
    );
    assert!(
        wrong_authority.starts_with("HTTP/1.1 401")
            && wrong_authority.contains("invalid_credentials")
    );
    let cross_site = exchange(
        handle.addr,
        "remote.example",
        &format!(
            "Origin: http://evil.example\r\nSec-Fetch-Site: cross-site\r\nCookie: {cookie}\r\n"
        ),
        "/auth",
    );
    assert!(cross_site.starts_with("HTTP/1.1 403"));
    assert!(!cross_site.contains("Set-Cookie:"));
    cleanup(handle, &storage, &project);
}

#[test]
fn local_auth_alias_redirect_preserves_only_workspace() {
    let (handle, storage, project) = spawn_serve("ux1-alias", TestBehavior::Success);
    let mut stream = connect(handle.addr);
    write!(stream, "GET /?workspace=default&t={TEST_TOKEN} HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n\r\n", handle.port()).unwrap();
    let reply = read_raw_response(&mut stream);
    assert!(reply.starts_with("HTTP/1.1 307"));
    assert!(reply.contains(&format!(
        "Location: http://{}/?workspace=default",
        handle.addr
    )));
    assert!(!reply.contains(TEST_TOKEN) && !reply.contains("Set-Cookie:"));
    cleanup(handle, &storage, &project);
}

#[test]
fn local_auth_nonempty_exchange_body_is_bad_request_without_session() {
    let (handle, storage, project) = spawn_serve("ux1-auth-body", TestBehavior::Success);
    let mut stream = connect(handle.addr);
    write!(
        stream,
        "POST /auth HTTP/1.1\r\nHost: {}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}",
        handle.addr
    )
    .unwrap();
    let reply = read_raw_response(&mut stream);
    assert!(reply.starts_with("HTTP/1.1 400"));
    assert!(reply.contains("\"code\":\"bad-request\""));
    assert!(!reply.contains("Set-Cookie:"));
    cleanup(handle, &storage, &project);
}
