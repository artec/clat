use super::*;
use serde_json::Value;
fn manifest() -> Value {
    let caps =
        crate::capabilities::tests::descriptor("https://example.com", true)["capabilities"].clone();
    serde_json::json!({"manifestVersion":2,"id":"io.artec.network-wasm","version":"0.1.0","runtime":{"kind":"wasm-component","entry":"plugin.wasm","sha256":format!("{:x}",Sha256::digest(b"component"))},"capabilities":caps})
}
#[test]
fn signed_v2_manifest_never_uses_self_asserted_keys_or_unsigned_bytes() {
    let bytes = serde_json::to_vec(&manifest()).unwrap();
    assert!(VerifiedManifest::verify(&bytes, "", "package-provided key").is_err());
    let key = include_str!("../../../../../release/minisign.pub")
        .lines()
        .find(|l| !l.starts_with("untrusted comment:") && !l.trim().is_empty())
        .unwrap();
    assert!(VerifiedManifest::verify(&bytes, "", key).is_err());
}
#[test]
fn signed_v2_manifest_closed_shape_identity_and_component_digest() {
    let original = manifest();
    let verified =
        VerifiedManifest::parse_verified(&serde_json::to_vec(&original).unwrap()).unwrap();
    verified.verify_component(b"component").unwrap();
    assert!(verified.verify_component(b"changed").is_err());
    assert_eq!(verified.identity(), ("io.artec.network-wasm", "0.1.0"));
    for (key, value) in [
        ("manifestVersion", serde_json::json!(1)),
        ("id", serde_json::json!("io.artec.network")),
        ("selfAssertedPublisherKey", serde_json::json!("key")),
    ] {
        let mut value2 = original.clone();
        value2[key] = value;
        assert!(VerifiedManifest::parse_verified(&serde_json::to_vec(&value2).unwrap()).is_err());
    }
    for path in [
        "../plugin.wasm",
        "/plugin.wasm",
        "a//b",
        "a\\b",
        "a:b",
        "./plugin.wasm",
    ] {
        let mut value = original.clone();
        value["runtime"]["entry"] = serde_json::json!(path);
        assert!(VerifiedManifest::parse_verified(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}
#[test]
fn signed_v2_policy_reuses_authority_and_configuration_only_narrows() {
    let original = manifest();
    let verified =
        VerifiedManifest::parse_verified(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert!(
        verified
            .policy(Some(
                br#"{"clock":false,"network":{"protocol":"clat:net-task@0.1.0","origins":[]}}"#
            ))
            .is_ok()
    );
    assert!(
        verified
            .policy(Some(br#"{"hostTools":["run_command"]}"#))
            .is_err()
    );
    let mut dangerous = original;
    dangerous["capabilities"]["preopens"] = serde_json::json!(["/"]);
    assert!(VerifiedManifest::parse_verified(&serde_json::to_vec(&dangerous).unwrap()).is_err());
}
#[test]
#[ignore = "requires separately signed v2 test manifest and explicit trusted test publisher key"]
fn signed_v2_actual_signature_rejects_byte_tampering_and_wrong_trust() {
    let root = std::path::PathBuf::from(
        std::env::var("PLG4_SIGNED_V2_FIXTURE").expect("signed fixture directory"),
    );
    let bytes = std::fs::read(root.join("manifest.json")).unwrap();
    let signature = std::fs::read_to_string(root.join("manifest.minisig")).unwrap();
    let key = std::fs::read_to_string(root.join("test.pub")).unwrap();
    let key = key
        .lines()
        .find(|l| !l.starts_with("untrusted comment:") && !l.trim().is_empty())
        .unwrap();
    let verified = VerifiedManifest::verify(&bytes, &signature, key).unwrap();
    assert_eq!(verified.signed_bytes(), bytes);
    verified.verify_component(b"component").unwrap();
    let mut tampered = bytes.clone();
    tampered.push(b' ');
    assert!(VerifiedManifest::verify(&tampered, &signature, key).is_err());
    let production = include_str!("../../../../../release/minisign.pub")
        .lines()
        .find(|l| !l.starts_with("untrusted comment:") && !l.trim().is_empty())
        .unwrap();
    assert!(VerifiedManifest::verify(&bytes, &signature, production).is_err());
}

#[test]
fn signed_v2_rejects_duplicate_capability_keys_before_value_normalization() {
    let source = serde_json::to_string(&manifest()).unwrap();
    for (before, after) in [
        ("\"clock\":", "\"clock\":null,\"clock\":"),
        ("\"protocol\":", "\"protocol\":\"unknown\",\"protocol\":"),
    ] {
        assert!(source.contains(before));
        let duplicate = source.replace(before, after);
        assert!(
            VerifiedManifest::parse_verified(duplicate.as_bytes()).is_err(),
            "signed raw capabilities cannot lose duplicate fields during normalization"
        );
    }
}
