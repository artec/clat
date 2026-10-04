use super::*;
use crate::{
    dns_authority::{Fence, Origin, Run, test_resolution},
    http_authority::{Declaration, HttpFence},
};
use clat_core::{
    PermissionApprover, PermissionDecision, PermissionMode, PermissionRequest, Project, ToolEffect,
};
use std::sync::Mutex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
pub(crate) static LOCK: Mutex<()> = Mutex::new(());
#[test]
fn core_gate_dns_http_share_actual_factory_plan_guard_and_safe_fields() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        let (_, _, request, _) = setup("http://example.invalid", Duration::from_secs(2));
        let parent = CancelToken::new();
        let (gate, seen) = gate(PermissionMode::ReadOnly, true, false);
        for method in [None, Some("GET")] {
            gate.authorize_origin(
                request.origin(),
                method,
                &parent,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .unwrap();
        }
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 2);
            assert_eq!(seen[0].arguments["action"], "resolve");
            assert_eq!(seen[1].arguments["action"], "http");
            assert!(
                seen.iter().all(|r| r.effect == ToolEffect::Network
                    && !r.arguments.to_string().contains("secret"))
            );
        }
        let plan = Gate::for_test(
            PermissionMode::FullAccess,
            true,
            Arc::new(|_: PermissionRequest, _: &CancelToken| {
                panic!("Plan must reject before approver")
            }),
            Project::new("."),
        );
        for method in [None, Some("GET")] {
            assert_eq!(
                plan.authorize_origin(
                    request.origin(),
                    method,
                    &parent,
                    Instant::now() + Duration::from_secs(1)
                )
                .await,
                Err(permission::Failure::Denied)
            );
        }
    });
}
#[test]
fn core_gate_revocation_closes_http_header_and_body_waits() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        for body in [false, true] {
            for trigger in 0..7 {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let origin = format!(
                    "http://http-fixture.invalid:{}",
                    listener.local_addr().unwrap().port()
                );
                let (_run, scope, request, res) = setup(&origin, Duration::from_secs(2));
                let fixture = clat_core::test_support::network::Fixture::new(
                    PermissionMode::FullAccess,
                    false,
                    Arc::new(|_: PermissionRequest, _: &CancelToken| PermissionDecision::Allow),
                    Project::new("."),
                );
                let gate = Gate::new(Arc::new(
                    fixture
                        .lease(
                            Instant::now()
                                + if trigger == 5 {
                                    Duration::from_millis(150)
                                } else {
                                    Duration::from_secs(2)
                                },
                        )
                        .unwrap(),
                ));
                let cancel = CancelToken::new();
                let server = async {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    read_request(&mut stream).await;
                    if body {
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx")
                            .await
                            .unwrap();
                    }
                    match trigger {
                        0 => {
                            fixture.set_mode(PermissionMode::ReadOnly);
                            fixture.set_mode(PermissionMode::FullAccess);
                        }
                        1 => fixture.refresh(),
                        6 => fixture.set_plan(true),
                        2 => fixture.clear(),
                        3 => fixture.new_run(),
                        4 => fixture.cancel(),
                        _ => {}
                    }
                    let mut rest = vec![];
                    tokio::time::timeout(Duration::from_millis(250), stream.read_to_end(&mut rest))
                        .await
                        .expect("core revocation must physically close socket")
                        .unwrap();
                };
                let (result, _) = tokio::time::timeout(Duration::from_secs(1), async {
                    tokio::join!(execute(request, &res, &scope, &gate, &cancel), server)
                })
                .await
                .unwrap();
                assert!(
                    matches!(
                        result,
                        Err(Failure::Permission(
                            permission::Failure::Cancelled | permission::Failure::Deadline
                        ))
                    ),
                    "expected core revocation error"
                );
            }
        }
    });
}
struct Approver {
    seen: Arc<Mutex<Vec<PermissionRequest>>>,
    allow: bool,
    hold: bool,
}
impl PermissionApprover for Approver {
    fn decide(&self, r: PermissionRequest, c: &CancelToken) -> PermissionDecision {
        self.seen.lock().unwrap().push(r);
        if self.hold {
            let until = Instant::now() + Duration::from_secs(2);
            while !c.is_cancelled() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if self.allow {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Deny {
                reason: "denied".into(),
            }
        }
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn gate(
    mode: PermissionMode,
    allow: bool,
    hold: bool,
) -> (Gate, Arc<Mutex<Vec<PermissionRequest>>>) {
    let seen = Arc::new(Mutex::new(vec![]));
    (
        Gate::for_test(
            mode,
            false,
            Arc::new(Approver {
                seen: seen.clone(),
                allow,
                hold,
            }),
            Project::new("."),
        ),
        seen,
    )
}
fn setup(origin: &str, remaining: Duration) -> (Run, Scope, PreparedRequest, Resolution) {
    let run = Run::new(Duration::from_secs(5)).unwrap();
    let scope = Scope::new(&run, Fence::new(Some(&[origin]), None).unwrap());
    let request = HttpFence::new(
        Some(&[Declaration {
            origin,
            methods: &["GET"],
        }]),
        None,
    )
    .unwrap()
    .prepare(
        &format!("{origin}/path?secret=query"),
        "GET",
        &[("authorization", "Bearer secret-header")],
        &[],
    )
    .unwrap();
    let res = test_resolution(
        &scope,
        Origin::parse(origin).unwrap(),
        vec!["127.0.0.1".parse().unwrap()],
        Instant::now() + remaining,
    );
    (run, scope, request, res)
}
async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut data = vec![];
    loop {
        let mut b = [0; 1024];
        let n = stream.read(&mut b).await.unwrap();
        assert!(n > 0);
        data.extend_from_slice(&b[..n]);
        assert!(data.len() < 65536);
        if data.windows(4).any(|b| b == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(data).unwrap()
}
fn response_case(raw: Vec<u8>) -> Result<Response, Failure> {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "http://http-fixture.invalid:{}",
            listener.local_addr().unwrap().port()
        );
        let (_run, scope, request, res) = setup(&origin, Duration::from_secs(2));
        let (gate, seen) = gate(PermissionMode::ReadOnly, true, false);
        let cancel = CancelToken::new();
        let server = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let wire = read_request(&mut stream).await;
            assert!(wire.starts_with("GET /path?secret=query HTTP/1.1\r\n"));
            assert!(wire.contains(&format!(
                "host: http-fixture.invalid:{}",
                listener.local_addr().unwrap().port()
            )));
            assert!(wire.contains("authorization: Bearer secret-header"));
            let _ = stream.write_all(&raw).await;
        };
        let (result, _) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(execute(request, &res, &scope, &gate, &cancel), server)
        })
        .await
        .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].effect, ToolEffect::Network);
        assert_eq!(seen[0].arguments["origin"], origin);
        assert_eq!(seen[0].arguments["method"], "GET");
        let shown = serde_json::to_string(&seen[0].arguments).unwrap();
        assert!(!shown.contains("secret"));
        result
    })
}
#[test]
fn http_wire_preserves_original_host_target_and_auth_without_approval_secret() {
    let r = response_case(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec()).unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.body, b"ok");
}
#[test]
fn redirect_is_returned_without_following_or_cookie_jar() {
    let r=response_case(b"HTTP/1.1 302 Found\r\nLocation: http://evil.invalid/\r\nSet-Cookie: x=y\r\nContent-Length: 0\r\n\r\n".to_vec()).unwrap();
    assert_eq!(r.status, 302);
    assert_eq!(r.headers["location"], "http://evil.invalid/");
}
#[test]
fn encoded_body_is_explicitly_rejected() {
    assert!(matches!(
        response_case(
            b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 0\r\n\r\n".to_vec()
        ),
        Err(Failure::Encoding)
    ));
}
#[test]
fn advertised_oversize_response_is_rejected_before_body_wait() {
    assert!(matches!(
        response_case(b"HTTP/1.1 200 OK\r\nContent-Length: 8388609\r\n\r\n".to_vec()),
        Err(Failure::Limit)
    ));
}
#[test]
fn chunked_cumulative_body_cannot_escape_limit() {
    let mut raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for _ in 0..129 {
        raw.extend_from_slice(b"10000\r\n");
        raw.extend_from_slice(&vec![b'x'; 65536]);
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(b"0\r\n\r\n");
    assert!(matches!(response_case(raw), Err(Failure::Limit)));
}
#[test]
fn truncated_body_and_upgrade_are_finite_failures() {
    assert!(matches!(
        response_case(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nx".to_vec()),
        Err(Failure::Transport)
    ));
    assert!(matches!(response_case(b"HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\nUpgrade: websocket\r\n\r\n".to_vec()),Err(Failure::Transport)));
}
#[test]
fn modes_use_actual_core_network_policy_and_each_call_asks_again() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        let (_, _, request, _) = setup("http://example.invalid", Duration::from_secs(2));
        let cancel = CancelToken::new();
        for mode in [
            PermissionMode::ReadOnly,
            PermissionMode::ProjectWrite,
            PermissionMode::FullAccess,
        ] {
            let (gate, seen) = gate(mode, true, false);
            for _ in 0..2 {
                gate.authorize(&request, &cancel, Instant::now() + Duration::from_secs(1))
                    .await
                    .unwrap();
            }
            assert_eq!(
                seen.lock().unwrap().len(),
                if mode == PermissionMode::ReadOnly {
                    2
                } else {
                    0
                }
            );
        }
        let (denied, _) = gate(PermissionMode::ReadOnly, false, false);
        assert_eq!(
            denied
                .authorize(&request, &cancel, Instant::now() + Duration::from_secs(1))
                .await,
            Err(permission::Failure::Denied)
        );
    });
}
#[test]
fn denial_and_cancelled_late_approval_never_connect() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "http://http-fixture.invalid:{}",
            listener.local_addr().unwrap().port()
        );
        for hold in [false, true] {
            let (_run, scope, request, res) = setup(&origin, Duration::from_secs(2));
            let (gate, seen) = gate(PermissionMode::ReadOnly, hold, hold);
            let cancel = CancelToken::new();
            let cancel_future = async {
                if hold {
                    while seen.lock().unwrap().is_empty() {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    cancel.cancel();
                }
            };
            let (result, _) = tokio::time::timeout(Duration::from_millis(250), async {
                tokio::join!(
                    execute(request, &res, &scope, &gate, &cancel),
                    cancel_future
                )
            })
            .await
            .unwrap();
            assert!(matches!(result, Err(Failure::Permission(_))));
            assert!(
                tokio::time::timeout(Duration::from_millis(20), listener.accept())
                    .await
                    .is_err()
            );
        }
    });
}
#[test]
fn response_holding_keeps_process_budget_reserved_until_drop() {
    let _lock = LOCK.lock().unwrap();
    let mut held = vec![];
    for _ in 0..8 {
        held.push(reserve().unwrap());
    }
    assert!(matches!(reserve(), Err(Failure::Limit)));
    held.pop();
    assert!(reserve().is_ok());
}
#[test]
fn mode_change_while_approval_waits_invalidates_answer() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        let (_, _, request, _) = setup("http://example.invalid", Duration::from_secs(2));
        let cancel = CancelToken::new();
        let (gate, seen) = gate(PermissionMode::ReadOnly, true, true);
        let change = async {
            while seen.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            gate.fixture
                .as_ref()
                .unwrap()
                .set_mode(PermissionMode::FullAccess);
        };
        let (result, _) = tokio::time::timeout(Duration::from_millis(250), async {
            tokio::join!(
                gate.authorize(&request, &cancel, Instant::now() + Duration::from_secs(2)),
                change
            )
        })
        .await
        .unwrap();
        assert_eq!(result, Err(permission::Failure::Cancelled));
    });
}
#[test]
fn waiting_body_cancel_and_original_deadline_close_socket() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        for explicit in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!(
                "http://http-fixture.invalid:{}",
                listener.local_addr().unwrap().port()
            );
            let (_run, scope, request, res) = setup(
                &origin,
                if explicit {
                    Duration::from_secs(2)
                } else {
                    Duration::from_millis(80)
                },
            );
            let (gate, _) = gate(PermissionMode::FullAccess, true, false);
            let cancel = CancelToken::new();
            let server = async {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_request(&mut stream).await;
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx")
                    .await
                    .unwrap();
                if explicit {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    cancel.cancel();
                }
                let mut rest = vec![];
                let closed =
                    tokio::time::timeout(Duration::from_millis(250), stream.read_to_end(&mut rest))
                        .await;
                assert!(
                    closed.is_ok(),
                    "cancelled body must physically close peer socket"
                );
            };
            let (result, _) = tokio::time::timeout(Duration::from_millis(300), async {
                tokio::join!(execute(request, &res, &scope, &gate, &cancel), server)
            })
            .await
            .unwrap();
            assert!(matches!(
                result,
                Err(Failure::Permission(_)) | Err(Failure::Authority(_))
            ));
        }
    });
}
#[test]
fn response_header_count_is_bounded() {
    let mut raw = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n".to_vec();
    for n in 0..64 {
        raw.extend_from_slice(format!("x-{n}: v\r\n").as_bytes());
    }
    raw.extend_from_slice(b"\r\n");
    assert!(response_case(raw).is_err());
}

mod streaming;
