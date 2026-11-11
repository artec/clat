use super::*;
use crate::ProjectAuthorization;
use crate::application::host_tests::fixture;

#[test]
fn host_mcp_save_and_remove_reload_all_mounted_projects() {
    let (root, mut host, first, second) = fixture("mcp-host-manage");
    let first_app = host.attach(first.clone()).unwrap();
    let second_app = host
        .authorize_and_attach(second.clone(), ProjectAuthorization::grant())
        .unwrap();
    let first_root = first.root().canonicalize().unwrap().display().to_string();
    let second_root = second.root().canonicalize().unwrap().display().to_string();
    let baseline = host.mcp_manage_list(&first_root).unwrap();
    assert_eq!(baseline["revision"], "missing");
    let saved = host
        .mcp_manage_save(
            &first_root,
            "user",
            "disabled-fixture",
            "missing",
            false,
            Some(json!({"command":"not-a-real-server"})),
        )
        .unwrap();
    let revision = saved["revision"].as_str().unwrap();
    assert_eq!(
        host.mcp_manage_list(&second_root).unwrap()["user"][0]["name"],
        "disabled-fixture"
    );
    assert_eq!(first_app.lock().unwrap().mcp_status().configured, 0);
    assert_eq!(second_app.lock().unwrap().mcp_status().configured, 0);
    assert!(
        host.mcp_manage_remove(&first_root, "user", "disabled-fixture", "missing")
            .is_err()
    );
    host.mcp_manage_remove(&first_root, "user", "disabled-fixture", revision)
        .unwrap();
    assert!(
        host.mcp_manage_list(&first_root).unwrap()["user"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        host.mcp_manage_get(&first_root, "user", "disabled-fixture")
            .unwrap()["entry"]
            .is_null()
    );
    drop(first_app);
    drop(second_app);
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
#[ignore = "starts a local Python MCP fixture; run explicitly"]
fn host_managed_mcp_real_stdio_tool_connect_call_and_disable() {
    let (root, mut host, first, _) = fixture("mcp-live-managed");
    let app = host.attach(first.clone()).unwrap();
    let project = first.root().canonicalize().unwrap().display().to_string();
    let script = r#"
import json,sys
for line in sys.stdin:
  data=json.loads(line)
  if "id" not in data: continue
  method=data.get("method","")
  if method=="server/discover":
    result={"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"_meta":{"io.modelcontextprotocol/serverInfo":{"name":"managed-fixture","version":"1"}}}
  elif method=="tools/list":
    result={"resultType":"complete","tools":[{"name":"echo","description":"Echo provided value","inputSchema":{"type":"object","properties":{"value":{"type":"string"}}}}]}
  elif method=="tools/call":
    result={"resultType":"complete","content":[{"type":"text","text":"managed fixture replied"}]}
  else:
    print(json.dumps({"jsonrpc":"2.0","id":data["id"],"error":{"code":-32601,"message":"unsupported"}}),flush=True)
    continue
  print(json.dumps({"jsonrpc":"2.0","id":data["id"],"result":result}),flush=True)
"#;
    let saved = host
        .mcp_manage_save(
            &project,
            "project",
            "my-test-mcp",
            "missing",
            true,
            Some(json!({"command":"python3","args":["-u","-c",script]})),
        )
        .unwrap();
    assert!(saved["runtime_warnings"].as_array().unwrap().is_empty());
    let listed = host.mcp_manage_list(&project).unwrap();
    assert_eq!(listed["project"][0]["name"], "my-test-mcp");
    let mut merged = crate::mcp::client::McpConfig::new();
    crate::mcp::manage::McpManaged::merge(
        &root.join("state"),
        first.root(),
        &mut merged,
        &Default::default(),
    )
    .unwrap();
    assert!(
        merged.contains_key("my-test-mcp"),
        "managed registry must merge into runtime"
    );
    let current = app.lock().unwrap();
    assert!(current.builtin_available("builtin.mcp_adapter"));
    let status = current.mcp_status();
    assert_eq!(status.configured, 1, "Host mounted requested managed MCP");
    assert_eq!(
        status.connected, 1,
        "MCP handshake and tools/list succeeded"
    );
    let tool = current
        .tools
        .definitions()
        .into_iter()
        .find(|info| info.name == "mcp_my_test_mcp_echo")
        .expect("tool is registered");
    let output = current
        .tools
        .get(&tool.name)
        .unwrap()
        .invoke(
            &json!({"value":"hello"}),
            current.project(),
            &crate::CancelToken::new(),
        )
        .unwrap();
    assert!(output.to_string().contains("managed fixture replied"));
    drop(current);
    let next = host.mcp_manage_list(&project).unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    host.mcp_manage_save(&project, "project", "my-test-mcp", &next, false, None)
        .unwrap();
    assert_eq!(app.lock().unwrap().mcp_status().configured, 0);
    assert!(
        !app.lock()
            .unwrap()
            .tools
            .definitions()
            .iter()
            .any(|info| info.name == tool.name)
    );
    drop(app);
    drop(host);
    crate::test_support::cleanup_tree(&root);
}
