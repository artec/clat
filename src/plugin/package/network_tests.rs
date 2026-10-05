use super::*;
use serde_json::json;
pub(super) fn candidate() -> Value {
    json!({"manifestVersion":2,"id":"io.artec.dsh-official-web-wasm","name":"Web WASM","version":"0.1.0",
      "runtime":{"kind":"wasm-component","entry":"official.wasm","sha256":"00".repeat(32)},
      "capabilities":{"tools":true,"network":{"protocol":"clat:net-task@0.1.0",
       "origins":[{"scheme":"https","host":"api.deepseek.com","port":443,"methods":["POST"]}]},
       "clock":{"protocol":"wasi:clocks@0.2.10"}}})
}
pub(super) fn parsed(value: Value) -> Result<PluginPackageManifest, String> {
    let m: PluginPackageManifest = serde_json::from_value(value).map_err(|e| e.to_string())?;
    m.validate()?;
    Ok(m)
}
#[test]
fn plg4_v2_manifest_accepts_fenced_wasm_only() {
    parsed(candidate()).expect("v2 semantic network WASM manifest must be installable");
    for patch in [
        json!({"manifestVersion":1}),
        json!({"runtime":{"kind":"mcp-stdio","entry":"official.wasm","sha256":"00".repeat(32)}}),
        json!({"capabilities":{"tools":true,"hostTools":["shell"],"network":candidate()["capabilities"]["network"]}}),
    ] {
        let mut value = candidate();
        for (k, v) in patch.as_object().unwrap() {
            value[k] = v.clone();
        }
        assert!(parsed(value).is_err());
    }
}
#[test]
fn plg4_v2_manifest_rejects_invalid_network_and_unknown_capabilities() {
    for network in [
        Value::Null,
        json!({"protocol":"future","origins":[]}),
        json!({"protocol":"clat:net-task@0.1.0","origins":[{"scheme":"https","host":"127.0.0.1","port":443,"methods":["GET"]}]}),
        json!({"protocol":"clat:net-task@0.1.0","origins":[],"proxy":"secret"}),
    ] {
        let mut value = candidate();
        value["capabilities"]["network"] = network;
        assert!(parsed(value).is_err());
    }
    let mut value = candidate();
    value["capabilities"]["preopens"] = json!(["/"]);
    assert!(parsed(value).is_err());
}
#[test]
fn plg4_v2_configuration_can_only_narrow_and_cannot_supply_host_tools() {
    let m = parsed(candidate()).unwrap();
    m.validate_config(Some(&json!({"apiKey":"do-not-log", "networkPolicy":{"network":{"protocol":"clat:net-task@0.1.0","origins":[]},"clock":false}}))).unwrap();
    for policy in [
        Value::Null,
        json!({"hostTools":["shell"]}),
        json!({"preopens":["/"]}),
        json!({"network":{"protocol":"future","origins":[]}}),
    ] {
        assert!(
            m.validate_config(Some(&json!({"networkPolicy":policy})))
                .is_err()
        );
    }
}
