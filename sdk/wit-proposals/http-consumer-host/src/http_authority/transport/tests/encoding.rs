use super::super::encoding as codec;
use super::*;

#[test]
fn shared_encoding_vectors_return_original_entity_bytes() {
    let vectors: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../dsh-wasm-flavor/encoding-cases.json"
    ))
    .unwrap();
    for case in vectors.as_array().unwrap() {
        let encoding = case["encoding"].as_str().unwrap();
        let wire = unhex(case["wireHex"].as_str().unwrap());
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: {encoding}\r\nContent-Length: {}\r\n\r\n",
            wire.len()
        )
        .into_bytes();
        response.extend(wire);
        let result = response_case(response);
        assert_eq!(
            result.unwrap().body,
            unhex(case["plainHex"].as_str().unwrap()),
            "{encoding}"
        );
    }
}
fn unhex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|part| u8::from_str_radix(std::str::from_utf8(part).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn compressed_bomb_and_truncation_fail_without_usable_entity() {
    use std::io::Write;
    let mut compressor = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    compressor.write_all(&vec![b'x'; LIMIT + 1]).unwrap();
    let compressed = compressor.finish().unwrap();
    assert!(matches!(
        response_case(wire("gzip", &compressed)),
        Err(Failure::Limit)
    ));
    let mut small = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    small.write_all(b"small valid entity").unwrap();
    let small = small.finish().unwrap();
    assert!(matches!(
        response_case(wire("gzip", &small[..small.len() - 8])),
        Err(Failure::Encoding)
    ));
    assert!(matches!(
        response_case(wire("br", b"invalid")),
        Err(Failure::Encoding)
    ));
    assert!(matches!(
        response_case(wire("unknown", b"body")),
        Err(Failure::Encoding)
    ));
}
fn wire(encoding: &str, data: &[u8]) -> Vec<u8> {
    let mut result = format!(
        "HTTP/1.1 200 OK\r\nContent-Encoding: {encoding}\r\nContent-Length: {}\r\n\r\n",
        data.len()
    )
    .into_bytes();
    result.extend_from_slice(data);
    result
}

#[test]
fn codec_memory_and_cancel_have_finite_failure_and_release_reservations() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        let headers =
            http::HeaderMap::from_iter([(http::header::CONTENT_ENCODING, "br".parse().unwrap())]);
        let codec = codec::Encoding::parse(&headers).unwrap();
        let held: Vec<_> = (0..8).map(|_| reserve().unwrap()).collect();
        assert_eq!(
            codec::decode(&codec, b"invalid", 100, || Ok(())).await,
            Err(Failure::Limit)
        );
        drop(held);
        let headers =
            http::HeaderMap::from_iter([(http::header::CONTENT_ENCODING, "gzip".parse().unwrap())]);
        let codec = codec::Encoding::parse(&headers).unwrap();
        assert_eq!(
            codec::decode(&codec, b"invalid", 100, || Err(Failure::Authority(
                AuthorityFailure::Cancelled
            )))
            .await,
            Err(Failure::Authority(AuthorityFailure::Cancelled))
        );
        let _all: Vec<_> = (0..8).map(|_| reserve().unwrap()).collect();
    });
}

#[test]
fn compressed_read_cancel_and_expiry_close_actual_socket_without_partial_entity() {
    let _lock = LOCK.lock().unwrap();
    runtime().block_on(async {
        for cancel_first in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://encoding.invalid:{}", listener.local_addr().unwrap().port());
            let (_run, scope, request, resolution) = setup(&origin, Duration::from_secs(2));
            let (gate, _) = gate(PermissionMode::FullAccess, true, false);
            let cancel = CancelToken::new();
            let trigger = cancel.clone();
            let server = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                read_request(&mut socket).await;
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 100\r\n\r\nx").await.unwrap();
                let mut rest = Vec::new();
                tokio::time::timeout(Duration::from_millis(500), socket.read_to_end(&mut rest)).await
                    .expect("compressed read must physically close on cancel or expiry").unwrap();
            };
            let client = async {
                let mut response = open(request, &resolution, &scope, &gate, &cancel, Duration::from_millis(100), 100).await.unwrap();
                let abort = async {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    if cancel_first { trigger.cancel(); }
                };
                let (result, _) = tokio::join!(response.read(64), abort);
                assert!(result.is_err(), "encoded prefix must never become a usable entity");
                assert!(response.read(64).await.is_err());
                assert!(response.status().is_err());
                let _held: Vec<_> = (0..8).map(|_| reserve().unwrap()).collect();
            };
            tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(client, server); }).await.unwrap();
        }
    });
}

#[test]
fn empty_gzip_members_check_authority_while_consuming_compressed_input() {
    let _lock = LOCK.lock().unwrap();
    let member = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast())
        .finish()
        .unwrap();
    let wire = member.repeat(20000);
    let checks = std::sync::atomic::AtomicUsize::new(0);
    runtime().block_on(async {
        let result = codec::decode(&codec::Encoding::Gzip, &wire, 100, || {
            if checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 6 {
                Err(Failure::Authority(AuthorityFailure::Cancelled))
            } else {
                Ok(())
            }
        })
        .await;
        assert_eq!(
            result,
            Err(Failure::Authority(AuthorityFailure::Cancelled)),
            "empty members must observe revocation before consuming the entire entity"
        );
    });
}
