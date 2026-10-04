use super::*;
use serde_json::{Value, json};
pub(crate) fn descriptor(origin: &str, clock: bool) -> Value {
    let origin = Origin::parse(origin).unwrap();
    let mut caps = json!({"network":{"protocol":"clat:net-task@0.1.0","origins":[{
        "scheme":origin.scheme(),"host":origin.host(),"port":origin.port(),"methods":["GET","POST"]
    }]}});
    if clock {
        caps["clock"] = json!({"protocol":"wasi:clocks@0.2.10"});
    }
    json!({"manifestVersion":2,"capabilities":caps})
}
pub(crate) fn policy(origin: &str, clock: bool) -> Policy {
    Policy::parse(
        &serde_json::to_vec(&descriptor(origin, clock)).unwrap(),
        None,
    )
    .unwrap()
}
fn parse(value: &Value, config: Option<&Value>) -> Result<Policy, Error> {
    Policy::parse(
        &serde_json::to_vec(value).unwrap(),
        config
            .map(serde_json::to_vec)
            .transpose()
            .unwrap()
            .as_deref(),
    )
}
#[test]
fn capability_descriptor_is_versioned_closed_and_duplicate_fields_are_rejected() {
    let mut value = descriptor("https://example.com", true);
    for version in [0, 1, 3] {
        value["manifestVersion"] = json!(version);
        assert!(matches!(
            parse(&value, None),
            Err(Error::UnsupportedVersion)
        ));
    }
    value["manifestVersion"] = json!(2);
    for key in ["process", "pure", "filesystem", "socket", "random"] {
        let mut bad = value.clone();
        bad["capabilities"][key] = json!(true);
        assert!(matches!(parse(&bad, None), Err(Error::InvalidDescriptor)));
    }
    for bytes in [
        r#"{"manifestVersion":2,"manifestVersion":1,"capabilities":{}}"#,
        r#"{"manifestVersion":2,"capabilities":{"sampling":true,"sampling":false}}"#,
    ] {
        assert!(matches!(
            Policy::parse(bytes.as_bytes(), None),
            Err(Error::InvalidDescriptor)
        ));
    }
}
#[test]
fn capability_network_requires_declaration_and_rejects_protocol_and_origin_alias_attacks() {
    assert!(matches!(
        parse(&json!({"manifestVersion":2,"capabilities":{}}), None),
        Err(Error::MissingNetwork)
    ));
    for bad in ["clat:net@0.1.0", "wasi:http@0.2.10", "clat:net-task@0.2.0"] {
        let mut value = descriptor("https://example.com", false);
        value["capabilities"]["network"]["protocol"] = json!(bad);
        assert!(matches!(
            parse(&value, None),
            Err(Error::UnsupportedProtocol)
        ));
    }
    for host in [
        "*",
        "127.0.0.1",
        "0x7f000001",
        "example.com.",
        "a@example.com",
        "example.com/path",
    ] {
        let mut value = descriptor("https://example.com", false);
        value["capabilities"]["network"]["origins"][0]["host"] = json!(host);
        assert!(
            matches!(parse(&value, None), Err(Error::InvalidFence)),
            "{host}"
        );
    }
}
#[test]
fn capability_network_rejects_all_process_and_preopen_combinations_before_construction() {
    for field in ["hostTools", "preopens"] {
        for item in ["run_command", "unknown_future_tool", "/", "project"] {
            let mut value = descriptor("https://example.com", true);
            value["capabilities"][field] = json!([item]);
            assert!(matches!(
                parse(&value, None),
                Err(Error::DangerousCombination)
            ));
            let value = descriptor("https://example.com", true);
            let mut config = json!({});
            config[field] = json!([item]);
            assert!(matches!(
                parse(&value, Some(&config)),
                Err(Error::DangerousCombination)
            ));
        }
    }
}
#[test]
fn capability_config_only_intersects_origins_methods_and_empty_denies_all() {
    let value = descriptor("https://example.com", false);
    let mut config = json!({"network":value["capabilities"]["network"]});
    config["network"]["origins"][0]["methods"] = json!(["POST"]);
    let policy = parse(&value, Some(&config)).unwrap();
    assert!(
        policy
            .http
            .prepare("https://example.com", "POST", &[], &[])
            .is_ok()
    );
    assert!(
        policy
            .http
            .prepare("https://example.com", "GET", &[], &[])
            .is_err()
    );
    let mut other = config.clone();
    other["network"]["origins"][0]["host"] = json!("other.example");
    let policy = parse(&value, Some(&other)).unwrap();
    for origin in ["https://example.com", "https://other.example"] {
        assert!(policy.dns.check(&Origin::parse(origin).unwrap()).is_err());
        assert!(policy.http.prepare(origin, "POST", &[], &[]).is_err());
    }
    config["network"]["origins"] = json!([]);
    let policy = parse(&value, Some(&config)).unwrap();
    assert!(
        policy
            .dns
            .check(&Origin::parse("https://example.com").unwrap())
            .is_err()
    );
    assert!(
        policy
            .http
            .prepare("https://example.com", "GET", &[], &[])
            .is_err()
    );
}
#[test]
fn capability_invalid_configuration_never_falls_back_to_manifest() {
    let value = descriptor("https://example.com", false);
    for config in [
        json!({"network":null}),
        json!({"clock":null}),
        json!({"clock":"true"}),
        json!({"unknown":true}),
        json!({"network":{"protocol":"wrong","origins":[]}}),
    ] {
        assert!(parse(&value, Some(&config)).is_err());
    }
    let mut config = json!({"network":value["capabilities"]["network"]});
    config["network"]["origins"][0]["methods"] = json!([]);
    assert!(matches!(
        parse(&value, Some(&config)),
        Err(Error::InvalidFence)
    ));
    let mut value = value;
    let duplicate = value["capabilities"]["network"]["origins"][0].clone();
    value["capabilities"]["network"]["origins"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(matches!(parse(&value, None), Err(Error::InvalidFence)));
}
#[test]
fn capability_clock_is_independent_config_cannot_grant_or_change_protocol() {
    assert!(policy("https://example.com", false).clock.is_none());
    assert!(policy("https://example.com", true).clock.is_some());
    assert!(matches!(
        parse(
            &descriptor("https://example.com", false),
            Some(&json!({"clock":true}))
        ),
        Err(Error::ClockEscalation)
    ));
    assert!(
        parse(
            &descriptor("https://example.com", true),
            Some(&json!({"clock":false}))
        )
        .unwrap()
        .clock
        .is_none()
    );
    let mut value = descriptor("https://example.com", true);
    value["capabilities"]["clock"]["protocol"] = json!("wasi:clocks@0.2.12");
    assert!(matches!(
        parse(&value, None),
        Err(Error::UnsupportedProtocol)
    ));
}
#[test]
fn capability_sampling_label_never_claims_complete_network_fence() {
    let mut value = descriptor("https://example.com", false);
    assert_eq!(
        parse(&value, None).unwrap().egress_label(),
        "restricted-http-dns"
    );
    value["capabilities"]["sampling"] = json!(true);
    assert_eq!(
        parse(&value, None).unwrap().egress_label(),
        "restricted-http-dns-and-model-service"
    );
    assert!(parse(&value, Some(&json!({"sampling":false}))).is_err());
}
#[test]
fn capability_large_descriptor_and_origin_list_fail_before_resources_exist() {
    assert!(matches!(
        Policy::parse(&vec![b' '; 262145], None),
        Err(Error::InvalidDescriptor)
    ));
    let mut value = descriptor("https://example.com", true);
    let first = value["capabilities"]["network"]["origins"][0].clone();
    value["capabilities"]["network"]["origins"] = json!(vec![first; 65]);
    assert!(matches!(parse(&value, None), Err(Error::InvalidFence)));
}
