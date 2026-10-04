use super::*;
#[test]
fn stream_headers_precede_body_and_reads_are_bounded_with_real_eof() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "http://stream.invalid:{}",
            listener.local_addr().unwrap().port()
        );
        let (_run, scope, request, res) = setup(&origin, Duration::from_secs(2));
        let (gate, _) = gate(PermissionMode::FullAccess, true, false);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 7\r\nX-Test: yes\r\n\r\n")
                .await
                .unwrap();
            rx.await.unwrap();
            socket.write_all(b"abcdefg").await.unwrap();
        };
        let client = async {
            let mut response = tokio::time::timeout(
                Duration::from_millis(150),
                open(
                    request,
                    &res,
                    &scope,
                    &gate,
                    &CancelToken::new(),
                    Duration::from_secs(1),
                    7,
                ),
            )
            .await
            .expect("headers must return before server sends body")
            .unwrap();
            assert_eq!(response.status().unwrap(), 201);
            assert_eq!(response.headers().unwrap()["x-test"], "yes");
            assert_eq!(
                response.read(0).await,
                Err(Failure::Permission(permission::Failure::InvalidRequest))
            );
            assert_eq!(
                response.read(65537).await,
                Err(Failure::Permission(permission::Failure::InvalidRequest))
            );
            tx.send(()).unwrap();
            let mut full = vec![];
            loop {
                let chunk = response.read(2).await.unwrap();
                assert!(chunk.len() <= 2);
                if chunk.is_empty() {
                    break;
                }
                full.extend(chunk);
            }
            assert_eq!(full, b"abcdefg");
            assert!(response.read(2).await.unwrap().is_empty());
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(client, server);
        })
        .await
        .unwrap();
    });
}
#[test]
fn stream_cancel_and_drop_close_socket_and_release_process_reservations() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        for cancel in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!(
                "http://stream.invalid:{}",
                listener.local_addr().unwrap().port()
            );
            let (_run, scope, request, res) = setup(&origin, Duration::from_secs(2));
            let (gate, _) = gate(PermissionMode::FullAccess, true, false);
            let (tx, rx) = tokio::sync::oneshot::channel();
            let server = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                read_request(&mut socket).await;
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx")
                    .await
                    .unwrap();
                let mut rest = vec![];
                tokio::time::timeout(Duration::from_millis(250), socket.read_to_end(&mut rest))
                    .await
                    .expect("cancel/drop must physically close socket")
                    .unwrap();
                tx.send(()).unwrap();
            };
            let client = async {
                let mut response = open(
                    request,
                    &res,
                    &scope,
                    &gate,
                    &CancelToken::new(),
                    Duration::from_secs(1),
                    2,
                )
                .await
                .unwrap();
                assert_eq!(response.read(1).await.unwrap(), b"x");
                if cancel {
                    response.cancel();
                    response.cancel();
                    assert!(response.status().is_err());
                } else {
                    drop(response);
                }
                let permits: Vec<_> = (0..8)
                    .map(|_| reserve().expect("all process reservations released"))
                    .collect();
                assert!(reserve().is_err());
                drop(permits);
                rx.await.unwrap();
            };
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(client, server);
            })
            .await
            .unwrap();
        }
    });
}
#[test]
fn stream_metadata_and_read_reject_revocation_and_original_expiry() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        for trigger in 0..3 {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!(
                "http://stream.invalid:{}",
                listener.local_addr().unwrap().port()
            );
            let (run, scope, request, res) = setup(
                &origin,
                if trigger == 2 {
                    Duration::from_millis(80)
                } else {
                    Duration::from_secs(2)
                },
            );
            let (gate, _) = gate(PermissionMode::FullAccess, true, false);
            let server = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                read_request(&mut socket).await;
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx")
                    .await
                    .unwrap();
                let mut rest = vec![];
                tokio::time::timeout(Duration::from_millis(250), socket.read_to_end(&mut rest))
                    .await
                    .unwrap()
                    .unwrap();
            };
            let client = async {
                let mut response = open(
                    request,
                    &res,
                    &scope,
                    &gate,
                    &CancelToken::new(),
                    Duration::from_secs(1),
                    2,
                )
                .await
                .unwrap();
                match trigger {
                    0 => gate.fixture.as_ref().unwrap().set_plan(true),
                    1 => run.invalidate(),
                    _ => tokio::time::sleep(Duration::from_millis(100)).await,
                }
                assert!(response.status().is_err());
                assert!(response.headers().is_err());
                assert!(response.read(1).await.is_err());
                assert!(response.read(1).await.is_err());
            };
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(client, server);
            })
            .await
            .unwrap();
        }
    });
}
#[test]
fn stream_guest_limit_rejects_chunked_excess_without_false_eof() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "http://stream.invalid:{}",
            listener.local_addr().unwrap().port()
        );
        let (_run, scope, request, res) = setup(&origin, Duration::from_secs(2));
        let (gate, _) = gate(PermissionMode::FullAccess, true, false);
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nabcd\r\n0\r\n\r\n",
                )
                .await
                .unwrap();
        };
        let client = async {
            let mut response = open(
                request,
                &res,
                &scope,
                &gate,
                &CancelToken::new(),
                Duration::from_secs(1),
                3,
            )
            .await
            .unwrap();
            assert_eq!(response.read(1).await, Err(Failure::Limit));
            assert_eq!(response.read(1).await, Err(Failure::Limit));
        };
        tokio::join!(client, server);
    });
}

#[test]
fn stream_request_ceiling_and_pending_read_cancel_close_socket() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        for cancel_now in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!(
                "http://stream.invalid:{}",
                listener.local_addr().unwrap().port()
            );
            let (_run, scope, request, res) = setup(&origin, Duration::from_secs(2));
            let (gate, _) = gate(PermissionMode::FullAccess, true, false);
            let cancel = CancelToken::new();
            let server = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                read_request(&mut socket).await;
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx")
                    .await
                    .unwrap();
                if cancel_now {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    cancel.cancel();
                }
                let mut rest = vec![];
                tokio::time::timeout(Duration::from_millis(250), socket.read_to_end(&mut rest))
                    .await
                    .expect("request expiry or read cancellation must close socket")
                    .unwrap();
            };
            let client = async {
                let mut response = open(
                    request,
                    &res,
                    &scope,
                    &gate,
                    &cancel,
                    if cancel_now {
                        Duration::from_secs(1)
                    } else {
                        Duration::from_millis(80)
                    },
                    2,
                )
                .await
                .unwrap();
                assert_eq!(response.read(1).await.unwrap(), b"x");
                let error = response.read(1).await.unwrap_err();
                assert!(matches!(
                    error,
                    Failure::Permission(
                        permission::Failure::Deadline | permission::Failure::Cancelled
                    ) | Failure::Authority(AuthorityFailure::DeadlineExceeded)
                ));
            };
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(client, server);
            })
            .await
            .unwrap();
        }
    });
}

#[test]
fn stream_dropped_read_future_aborts_response_before_resource_drop() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "http://stream.invalid:{}",
            listener.local_addr().unwrap().port()
        );
        let (_run, scope, request, res) = setup(&origin, Duration::from_secs(2));
        let (gate, _) = gate(PermissionMode::FullAccess, true, false);
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n")
                .await
                .unwrap();
            let mut rest = vec![];
            tokio::time::timeout(Duration::from_millis(250), socket.read_to_end(&mut rest))
                .await
                .expect("dropping pending read must close live response socket")
                .unwrap();
        };
        let client = async {
            let mut response = open(
                request,
                &res,
                &scope,
                &gate,
                &CancelToken::new(),
                Duration::from_secs(1),
                1,
            )
            .await
            .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(20), response.read(1))
                    .await
                    .is_err()
            );
            assert!(
                response.status().is_err(),
                "response must already be cancelled before drop"
            );
            let held: Vec<_> = (0..8).map(|_| reserve().unwrap()).collect();
            drop(held);
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(client, server);
        })
        .await
        .unwrap();
    });
}

#[test]
fn resource_owner_closes_idle_http_on_close_drop_run_or_plan() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        for trigger in 0..5 {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!(
                "http://stream.invalid:{}",
                listener.local_addr().unwrap().port()
            );
            let (run, scope, request, res) = setup(&origin, Duration::from_secs(2));
            let (gate, _) = gate(PermissionMode::FullAccess, true, false);
            let server = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                read_request(&mut socket).await;
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx")
                    .await
                    .unwrap();
                let mut rest = vec![];
                tokio::time::timeout(Duration::from_millis(250), socket.read_to_end(&mut rest))
                    .await
                    .expect("owner boundary must close idle socket without body read")
                    .unwrap();
            };
            let client = async {
                let response = open(
                    request,
                    &res,
                    &scope,
                    &gate,
                    &CancelToken::new(),
                    Duration::from_secs(1),
                    2,
                )
                .await
                .unwrap();
                let network = crate::http_authority::network::NetworkScope::begin_tool(
                    &run,
                    Fence::new(Some(&[origin.as_str()]), None).unwrap(),
                    Arc::new(gate.clone()),
                    CancelToken::new(),
                    Instant::now() + Duration::from_secs(1),
                )
                .unwrap();
                let mut owner = crate::network_resources::Owner::new(network.scope());
                owner.insert(response).unwrap();
                match trigger {
                    0 => {
                        owner.close().unwrap();
                        assert_eq!(owner.len(), 0);
                    }
                    1 => drop(owner),
                    2 => {
                        run.invalidate();
                        assert!(owner.check().is_err());
                        assert_eq!(owner.len(), 0);
                    }
                    3 => {
                        gate.fixture.as_ref().unwrap().set_plan(true);
                        assert!(owner.check().is_err());
                        assert_eq!(owner.len(), 0);
                    }
                    _ => {
                        drop(network);
                        assert!(owner.check().is_err());
                        assert_eq!(owner.len(), 0);
                    }
                }
                let held: Vec<_> = (0..8).map(|_| reserve().unwrap()).collect();
                drop(held);
            };
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(client, server);
            })
            .await
            .unwrap();
        }
    });
}

#[test]
fn resource_task_read_ready_drop_cancels_real_socket_and_discards_result() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        for ready_then_cancel in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!(
                "http://stream.invalid:{}",
                listener.local_addr().unwrap().port()
            );
            let (_run, scope, request, res) = setup(&origin, Duration::from_secs(2));
            let (gate, _) = gate(PermissionMode::FullAccess, true, false);
            let server = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                read_request(&mut socket).await;
                socket
                    .write_all(if ready_then_cancel {
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx"
                    } else {
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n"
                    })
                    .await
                    .unwrap();
                let mut rest = vec![];
                tokio::time::timeout(Duration::from_millis(250), socket.read_to_end(&mut rest))
                    .await
                    .unwrap()
                    .unwrap();
            };
            let client = async {
                let response = open(
                    request,
                    &res,
                    &scope,
                    &gate,
                    &CancelToken::new(),
                    Duration::from_secs(1),
                    2,
                )
                .await
                .unwrap();
                let mut task = crate::network_resources::Task::read(
                    &scope,
                    response,
                    1,
                    Instant::now() + Duration::from_secs(1),
                );
                if ready_then_cancel {
                    task.ready().await;
                    task.cancel();
                } else {
                    assert!(
                        tokio::time::timeout(Duration::from_millis(20), task.ready())
                            .await
                            .is_err()
                    );
                }
                assert!(matches!(task.get(), Some(Err(_))));
                let held: Vec<_> = (0..8).map(|_| reserve().unwrap()).collect();
                drop(held);
            };
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(client, server);
            })
            .await
            .unwrap();
        }
    });
}

#[test]
fn resource_task_http_and_read_share_typed_private_result_chain() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "http://stream.invalid:{}",
            listener.local_addr().unwrap().port()
        );
        let (run, _old_scope, request, _old_res) = setup(&origin, Duration::from_secs(2));
        let (gate, _) = gate(PermissionMode::FullAccess, true, false);
        let network = Arc::new(
            crate::http_authority::network::NetworkScope::begin_tool(
                &run,
                Fence::new(Some(&[origin.as_str()]), None).unwrap(),
                Arc::new(gate),
                CancelToken::new(),
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap(),
        );
        let resolution = test_resolution(
            network.scope(),
            Origin::parse(&origin).unwrap(),
            vec!["127.0.0.1".parse().unwrap()],
            Instant::now() + Duration::from_secs(1),
        );
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx")
                .await
                .unwrap();
            let mut rest = vec![];
            tokio::time::timeout(Duration::from_millis(250), socket.read_to_end(&mut rest))
                .await
                .unwrap()
                .unwrap();
        };
        let client = async {
            let mut task = crate::network_resources::Task::http(
                network.clone(),
                resolution,
                request,
                Duration::from_secs(1),
                2,
            )
            .unwrap();
            assert!(task.get().is_none());
            task.ready().await;
            let crate::network_resources::Outcome::Response(mut response) =
                task.get().unwrap().unwrap()
            else {
                panic!("typed response expected")
            };
            assert_eq!(response.status().unwrap(), 200);
            assert!(task.get().unwrap().is_err());
            let mut read = crate::network_resources::Task::read(
                network.scope(),
                response,
                1,
                Instant::now() + Duration::from_secs(1),
            );
            read.ready().await;
            let crate::network_resources::Outcome::Read(response, bytes) =
                read.get().unwrap().unwrap()
            else {
                panic!("typed read expected")
            };
            assert_eq!(bytes, b"x");
            assert!(read.get().unwrap().is_err());
            drop(response);
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(client, server);
        })
        .await
        .unwrap();
    });
}
