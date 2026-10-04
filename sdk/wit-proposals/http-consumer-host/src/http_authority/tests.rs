use super::*;
fn fence() -> HttpFence {
    HttpFence::new(
        Some(&[Declaration {
            origin: "https://example.com",
            methods: &["GET", "POST"],
        }]),
        None,
    )
    .unwrap()
}
fn rejects(
    f: &HttpFence,
    url: &str,
    method: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    error: Failure,
) {
    assert!(matches!(f.prepare(url, method, headers, body), Err(e) if e == error));
}
#[test]
fn origin_and_method_are_joint_authority() {
    for (u, m) in [
        ("https://example.com.evil/a", "GET"),
        ("http://example.com/a", "GET"),
        ("https://example.com:444/a", "GET"),
        ("https://example.com/a", "DELETE"),
    ] {
        rejects(&fence(), u, m, &[], &[], Failure::CapabilityDenied);
    }
}
#[test]
fn configuration_only_narrows_methods() {
    let manifest = [Declaration {
        origin: "https://example.com",
        methods: &["GET", "POST"],
    }];
    let config = [Declaration {
        origin: "https://example.com",
        methods: &["GET", "DELETE"],
    }];
    let f = HttpFence::new(Some(&manifest), Some(&config)).unwrap();
    assert!(f.prepare("https://example.com/a", "GET", &[], &[]).is_ok());
    for m in ["POST", "DELETE"] {
        rejects(
            &f,
            "https://example.com/a",
            m,
            &[],
            &[],
            Failure::CapabilityDenied,
        );
    }
    let empty = HttpFence::new(Some(&manifest), Some(&[])).unwrap();
    rejects(
        &empty,
        "https://example.com/a",
        "GET",
        &[],
        &[],
        Failure::CapabilityDenied,
    );
    assert!(matches!(
        HttpFence::new(None, None),
        Err(Failure::CapabilityDenied)
    ));
}
#[test]
fn raw_url_cannot_hide_authority_or_controls() {
    for u in [
        "https://user:pass@example.com/a",
        "https://example.com/a#x",
        "https://example.com/a\n",
        "https://example.com\\a",
        "https://example.com./a",
        "https://127.0.0.1/a",
    ] {
        rejects(&fence(), u, "GET", &[], &[], Failure::InvalidRequest);
    }
}
#[test]
fn transport_control_headers_are_closed() {
    for n in [
        "Host",
        "content-length",
        "Transfer-Encoding",
        "connection",
        "Upgrade",
        "proxy-authorization",
        "proxy-connection",
        "proxy-authenticate",
        "Trailer",
        "TE",
        "Keep-Alive",
        "Expect",
    ] {
        rejects(
            &fence(),
            "https://example.com/a",
            "GET",
            &[(n, "x")],
            &[],
            Failure::InvalidRequest,
        );
    }
}
#[test]
fn controls_in_all_header_values_are_rejected() {
    for v in ["x\r\ny", "x\0y", "x\ty", "x\u{7f}y"] {
        rejects(
            &fence(),
            "https://example.com/a",
            "GET",
            &[("x-test", v)],
            &[],
            Failure::InvalidRequest,
        );
    }
}
#[test]
fn budgets_include_raw_url_headers_and_body() {
    rejects(
        &fence(),
        &format!("https://example.com/{}", "a".repeat(2048)),
        "GET",
        &[],
        &[],
        Failure::LimitExceeded,
    );
    rejects(
        &fence(),
        "https://example.com/a",
        "GET",
        &vec![("x", "y"); 65],
        &[],
        Failure::LimitExceeded,
    );
    let large = "a".repeat(32768);
    rejects(
        &fence(),
        "https://example.com/a",
        "GET",
        &[("x", &large)],
        &[],
        Failure::LimitExceeded,
    );
    rejects(
        &fence(),
        "https://example.com/a",
        "POST",
        &[],
        &vec![0; 1048577],
        Failure::LimitExceeded,
    );
}
#[test]
fn declarations_are_bounded_and_unambiguous() {
    let duplicate = [
        Declaration {
            origin: "https://example.com",
            methods: &["GET"],
        },
        Declaration {
            origin: "https://example.com:443",
            methods: &["POST"],
        },
    ];
    assert!(matches!(
        HttpFence::new(Some(&duplicate), None),
        Err(Failure::InvalidRequest)
    ));
    for m in ["CONNECT", "TRACE", "get", "", "X-CUSTOM"] {
        assert!(matches!(
            HttpFence::new(
                Some(&[Declaration {
                    origin: "https://example.com",
                    methods: &[m]
                }]),
                None
            ),
            Err(Failure::InvalidRequest)
        ));
    }
    let many: Vec<_> = (0..65)
        .map(|_| Declaration {
            origin: "https://example.com",
            methods: &["GET"],
        })
        .collect();
    assert!(matches!(
        HttpFence::new(Some(&many), None),
        Err(Failure::LimitExceeded)
    ));
}
#[test]
fn summary_never_carries_guest_secrets() {
    let p = fence()
        .prepare(
            "https://example.com/secret-path?key=secret-query",
            "POST",
            &[("authorization", "Bearer secret-header")],
            b"secret-body",
        )
        .unwrap();
    assert_eq!(p.approval_summary(), "POST https://example.com:443");
    assert_eq!(p.body(), b"secret-body");
    assert_eq!(p.headers()[0].1, "Bearer secret-header");
    assert!(p.url().as_str().contains("secret-query"));
}
#[test]
fn canonical_origin_and_bounded_positive_request() {
    let p = fence()
        .prepare(
            "https://EXAMPLE.com:443/a?q=x",
            "GET",
            &[("accept", "application/json")],
            &vec![0; 1048576],
        )
        .unwrap();
    assert_eq!(p.origin(), &Origin::parse("https://example.com").unwrap());
    assert_eq!(p.method(), Method::GET);
}
