use super::*;
use serde_json::json;

#[test]
fn model_setup_rpc_is_authenticated_declared_and_preserves_old_profile_save() {
    let (handle, storage, project) = spawn_serve("model-setup-rpc", TestBehavior::Success);
    assert_eq!(
        post(handle.addr, "wrong-token", "model.setup.get", "{}").0,
        401
    );
    let (_, info) = post(handle.addr, TEST_TOKEN, "workbench.info", "{}");
    let info = info.unwrap();
    for method in ["get", "apply", "verify", "status", "cancel"] {
        assert!(
            info["methods"]
                .as_array()
                .unwrap()
                .contains(&json!(format!("model.setup.{method}")))
        );
    }
    let (_, view) = post(handle.addr, TEST_TOKEN, "model.setup.get", "{}");
    let view = view.unwrap();
    assert_eq!(view["state"], "configured");
    let previous_model = view["settings"]["current"]["model"].clone();
    let profile = json!({"name":"Local", "protocol":"open_ai_compatible", "model":"local",
        "endpoint":"http://127.0.0.1:11434/v1", "request_path":"/chat/completions"});
    let (_, saved) = post(
        handle.addr,
        TEST_TOKEN,
        "model.profile.save",
        &profile.to_string(),
    );
    saved.unwrap();
    let (_, view) = post(handle.addr, TEST_TOKEN, "model.setup.get", "{}");
    assert_eq!(
        view.unwrap()["settings"]["current"]["model"],
        previous_model,
        "legacy save must not activate"
    );
    cleanup(handle, &storage, &project);
}
