use super::*;
use crate::dns_authority::{Fence, Origin, Run, test_resolution};
use crate::http_authority::{Declaration, HttpFence};
use std::time::{Duration, Instant};
use tokio::{io::AsyncReadExt, net::TcpListener};
fn setup(origin: &str) -> (Run, Scope, PreparedRequest) {
    let run = Run::new(Duration::from_secs(5)).unwrap();
    let scope = Scope::new(&run, Fence::new(Some(&[origin]), None).unwrap());
    let f = HttpFence::new(
        Some(&[Declaration {
            origin,
            methods: &["GET"],
        }]),
        None,
    )
    .unwrap();
    let request = f
        .prepare(&format!("{origin}/path?secret=x"), "GET", &[], &[])
        .unwrap();
    (run, scope, request)
}
fn resolution(scope: &Scope, origin: &str, remaining: Duration) -> Resolution {
    test_resolution(
        scope,
        Origin::parse(origin).unwrap(),
        vec!["127.0.0.1".parse().unwrap()],
        Instant::now() + remaining,
    )
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
#[test]
fn binding_rejects_origin_change_foreign_store_and_second_consumption() {
    let (run, scope, request) = setup("http://original.invalid");
    let res = resolution(&scope, "http://different.invalid", Duration::from_secs(1));
    assert!(matches!(
        ConnectionRequest::bind(request, &res, &scope),
        Err(Failure::InvalidOrigin)
    ));
    let (_, _, request) = setup("http://different.invalid");
    let foreign = Scope::new(
        &run,
        Fence::new(Some(&["http://different.invalid"]), None).unwrap(),
    );
    assert!(matches!(
        ConnectionRequest::bind(request, &res, &foreign),
        Err(Failure::InvalidResolution)
    ));
    let (_, _, request) = setup("http://different.invalid");
    let bound = ConnectionRequest::bind(request, &res, &scope).unwrap();
    assert_eq!(bound.request.url().query(), Some("secret=x"));
    let (_, _, request) = setup("http://different.invalid");
    assert!(matches!(
        ConnectionRequest::bind(request, &res, &scope),
        Err(Failure::InvalidResolution)
    ));
}
#[test]
fn numeric_dial_connects_unresolvable_hostname_without_dns() {
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "http://does-not-exist.invalid:{}",
            listener.local_addr().unwrap().port()
        );
        let (_run, scope, request) = setup(&origin);
        let res = resolution(&scope, &origin, Duration::from_secs(1));
        let bound = ConnectionRequest::bind(request, &res, &scope).unwrap();
        let connection = tokio::time::timeout(Duration::from_secs(2), bound.connect())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(connection, Connection::Plain(_)));
        let (mut peer, _) = listener.accept().await.unwrap();
        drop(connection);
        let mut b = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), peer.read(&mut b))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    });
}
#[test]
fn tls_wait_cancellation_closes_physical_socket() {
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "https://does-not-exist.invalid:{}",
            listener.local_addr().unwrap().port()
        );
        let (_run, scope, request) = setup(&origin);
        let res = resolution(&scope, &origin, Duration::from_secs(2));
        let bound = ConnectionRequest::bind(request, &res, &scope).unwrap();
        let connect = bound.connect();
        tokio::pin!(connect);
        let (mut peer, _) = tokio::select! {
            result = listener.accept() => result.unwrap(),
            _ = &mut connect => panic!("TLS should wait for peer handshake"),
            _ = tokio::time::sleep(Duration::from_secs(1)) => panic!("bounded accept"),
        };
        scope.invalidate();
        let result = tokio::time::timeout(Duration::from_millis(250), &mut connect)
            .await
            .unwrap();
        assert!(matches!(
            result,
            Err(ConnectFailure::Authority(Failure::Cancelled))
        ));
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), peer.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
    });
}
#[test]
fn original_deadline_bounds_tls_and_failed_attempt_does_not_reset_credential() {
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "https://does-not-exist.invalid:{}",
            listener.local_addr().unwrap().port()
        );
        let (_run, scope, request) = setup(&origin);
        let res = resolution(&scope, &origin, Duration::from_millis(80));
        let bound = ConnectionRequest::bind(request, &res, &scope).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), bound.connect())
                .await
                .unwrap(),
            Err(ConnectFailure::Authority(Failure::DeadlineExceeded))
        ));
        let (_, _, request) = setup(&origin);
        assert!(ConnectionRequest::bind(request, &res, &scope).is_err());
    });
}
#[test]
fn invalidation_before_wait_registration_is_not_lost() {
    runtime().block_on(async {
        let (run, scope, request) = setup("http://example.invalid");
        let res = resolution(&scope, "http://example.invalid", Duration::from_secs(2));
        let bound = ConnectionRequest::bind(request, &res, &scope).unwrap();
        run.invalidate();
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(250), bound.pins.closed())
                .await
                .unwrap(),
            Failure::Cancelled
        );
    });
}

fn trusted_tls() -> TlsConnector {
    let ca = rustls::pki_types::CertificateDer::from(
        include_bytes!("../../../test-data/tls/ca.der").to_vec(),
    );
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca).unwrap();
    TlsConnector::from(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}
fn server_tls() -> tokio_rustls::TlsAcceptor {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let cert = CertificateDer::from(include_bytes!("../../../test-data/tls/leaf.der").to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        include_bytes!("../../../test-data/tls/leaf-key.der").to_vec(),
    ));
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    tokio_rustls::TlsAcceptor::from(Arc::new(config))
}
fn tls_case(host: &str, trusted: bool, success: bool) {
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("https://{host}:{}", listener.local_addr().unwrap().port());
        let (_run, scope, request) = setup(&origin);
        let res = resolution(&scope, &origin, Duration::from_secs(2));
        let bound = ConnectionRequest::bind(request, &res, &scope).unwrap();
        let acceptor = server_tls();
        let server = async {
            let (stream, _) = listener.accept().await.unwrap();
            acceptor.accept(stream).await
        };
        let tls = if trusted {
            trusted_tls()
        } else {
            TlsConnector::from(Arc::new(default_tls()))
        };
        let (client, server) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(bound.connect_with(tls), server)
        })
        .await
        .unwrap();
        if success {
            assert!(matches!(client, Ok(Connection::Tls(_))));
            assert_eq!(server.unwrap().get_ref().1.server_name(), Some(host));
        } else {
            assert!(matches!(client, Err(ConnectFailure::Tls)));
            assert!(server.is_err());
            let (_, _, retry) = setup(&origin);
            assert!(
                matches!(
                    ConnectionRequest::bind(retry, &res, &scope),
                    Err(Failure::InvalidResolution)
                ),
                "a real failed TLS attempt must not restore the credential"
            );
        }
    });
}
#[test]
fn tls_valid_chain_uses_original_hostname_for_sni() {
    tls_case("tls-fixture.invalid", true, true);
}
#[test]
fn tls_rejects_trusted_chain_with_wrong_hostname() {
    tls_case("other.invalid", true, false);
}
#[test]
fn tls_rejects_untrusted_chain_even_when_hostname_matches() {
    tls_case("tls-fixture.invalid", false, false);
}

#[test]
fn run_invalidation_wakes_an_already_registered_waiter() {
    runtime().block_on(async {
        let (run, scope, request) = setup("http://example.invalid");
        let res = resolution(&scope, "http://example.invalid", Duration::from_secs(2));
        let bound = ConnectionRequest::bind(request, &res, &scope).unwrap();
        let invalidate = async {
            tokio::task::yield_now().await;
            run.invalidate();
        };
        let (failure, _) = tokio::time::timeout(Duration::from_millis(250), async {
            tokio::join!(bound.pins.closed(), invalidate)
        })
        .await
        .unwrap();
        assert_eq!(failure, Failure::Cancelled);
    });
}
#[test]
fn completion_cannot_publish_after_synchronous_invalidation() {
    runtime().block_on(async {
        let (_run, scope, request) = setup("http://example.invalid");
        let res = resolution(&scope, "http://example.invalid", Duration::from_secs(2));
        let bound = ConnectionRequest::bind(request, &res, &scope).unwrap();
        let result = guarded(&bound.pins, async {
            scope.invalidate();
            Ok(())
        })
        .await;
        assert_eq!(result, Err(ConnectFailure::Authority(Failure::Cancelled)));
    });
}
