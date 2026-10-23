use super::*;

const INDEX: &[u8] = include_bytes!("../../../tests/fixtures/release-index.json");
const ASSET: &str = "clat-v2.0.0-aarch64-apple-darwin.tar.gz";

fn check(bytes: &[u8], checksum: &[u8], hash: &str, asset: &str, tag: &str) -> Result<(), String> {
    let key = minisign_verify::PublicKey::decode(include_str!(
        "../../../tests/fixtures/release-index-cli.pub"
    ))
    .unwrap();
    verify_with_key(bytes, checksum, hash, asset, tag, &key)
}

#[test]
fn compact_index_authenticates_bytes_names_version_and_separate_trust_root() {
    let hash = sha256_hex(b"abc");
    let checksum = format!("{hash}  {ASSET}\n");
    check(INDEX, checksum.as_bytes(), &hash, ASSET, "v2.0.0").unwrap();
    for (bytes, actual, asset, tag) in [
        (checksum.as_bytes(), sha256_hex(b"changed"), ASSET, "v2.0.0"),
        (checksum.as_bytes(), hash.clone(), ASSET, "v2.0.1"),
        (checksum.as_bytes(), hash.clone(), "other.tar.gz", "v2.0.0"),
        (b"changed".as_slice(), hash.clone(), ASSET, "v2.0.0"),
    ] {
        assert!(check(INDEX, bytes, &actual, asset, tag).is_err());
    }
    assert!(verify_index_checksum(INDEX, checksum.as_bytes(), &hash, ASSET, "v2.0.0").is_err());
}

#[test]
fn compact_index_rejects_tampering_missing_signature_schema_and_wrong_key() {
    let hash = sha256_hex(b"abc");
    let checksum = format!("{hash}  {ASSET}\n");
    for change in ["payload", "signature", "schema", "wrong-key", "version"] {
        let mut value: Value = serde_json::from_slice(INDEX).unwrap();
        match change {
            "payload" => value["payload"] = Value::String("{}".into()),
            "version" => {
                let payload = value["payload"]
                    .as_str()
                    .unwrap()
                    .replace("2.0.0", "99.0.0");
                value["payload"] = Value::String(payload);
            }
            "signature" => value["signatures"].as_object_mut().unwrap().clear(),
            "schema" => value["schema_version"] = Value::from(2),
            _ => value["signatures"]["cli"] = value["signatures"]["desktop"].clone(),
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(
            check(&bytes, checksum.as_bytes(), &hash, ASSET, "v2.0.0").is_err(),
            "{change}"
        );
    }
}

#[test]
fn compact_index_rejects_authentic_but_misbound_versions_and_comments() {
    let cases: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/release-index-cases.json"
    ))
    .unwrap();
    let hash = sha256_hex(b"abc");
    let checksum = format!("{hash}  {ASSET}\n");
    for (name, error) in [
        ("wrong_version", "version mismatch"),
        ("wrong_comment", "possible replay"),
    ] {
        let bytes = serde_json::to_vec(&cases[name]).unwrap();
        assert!(
            check(&bytes, checksum.as_bytes(), &hash, ASSET, "v2.0.0")
                .unwrap_err()
                .contains(error)
        );
    }
}
