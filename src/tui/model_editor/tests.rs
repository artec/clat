use super::*;
#[cfg(feature = "runtime-tests")]
use crate::test_support::roots;
#[cfg(feature = "runtime-tests")]
use crate::{BootstrapApplication, Project};
#[cfg(feature = "runtime-tests")]
use std::fs;

fn editor() -> ModelEditor {
    let config = ModelConfig::default();
    let credentials = ProviderCredentials::for_protocol(config.protocol);
    ModelEditor::new_with_descriptors(&config, credentials, Vec::new())
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// 实机回归：编辑弹窗宽度此前只按 `area.width - 2` 收上限，窄
/// 分屏里左右只剩 1 列、几乎贴墙。现在必须为 POPUP_H_MARGIN
/// 的两侧留白让位；宽终端维持 68 列上限不变。
#[test]
fn edit_popup_width_reserves_horizontal_margins() {
    assert_eq!(
        edit_popup_width(Rect::new(0, 0, 50, 24)),
        50 - 2 * crate::tui::POPUP_H_MARGIN,
        "narrow panes shrink the popup, not its margins"
    );
    assert_eq!(edit_popup_width(Rect::new(0, 0, 80, 24)), 68);
    assert_eq!(edit_popup_width(Rect::new(0, 0, 200, 24)), 68);
}

fn select(editor: &mut ModelEditor, kind: RowKind) {
    editor.selected = editor
        .visible_rows()
        .iter()
        .position(|candidate| *candidate == kind)
        .expect("row kind is visible");
}

fn commit_popup(editor: &mut ModelEditor, text: &str) {
    commit_popup_on(editor, RowKind::Model, text);
}

/// 在指定行上打开编辑弹窗并提交文本；高级区行（如 ExtraBody）
/// 先自动展开 Advanced。
fn commit_popup_on(editor: &mut ModelEditor, kind: RowKind, text: &str) {
    if !editor.visible_rows().contains(&kind) {
        select(editor, RowKind::Advanced);
        editor.handle_key(key(KeyCode::Enter));
    }
    select(editor, kind);
    editor.handle_key(key(KeyCode::Enter));
    editor.handle_key(key(KeyCode::Delete));
    for ch in text.chars() {
        editor.handle_key(key(KeyCode::Char(ch)));
    }
    editor.handle_key(key(KeyCode::Enter));
}

#[test]
fn supports_openai_compatible_custom_parameters() {
    let mut editor = editor();
    editor.model = "third-party-model".into();
    editor.endpoint = "https://gateway.example/v1".into();
    editor.extra_headers = r#"{"X-Tenant":"abc"}"#.into();
    editor.extra_body = r#"{"top_p":0.9}"#.into();
    let (config, _) = editor.build().unwrap();
    assert_eq!(config.protocol, ModelProtocol::OpenAiCompatible);
    assert_eq!(config.request_path, "/chat/completions");
    assert_eq!(config.extra_headers["X-Tenant"], "abc");
    assert_eq!(config.extra_body["top_p"], 0.9);
}

#[test]
fn enter_opens_input_popup_and_commits() {
    let mut editor = editor();
    select(&mut editor, RowKind::Model);
    assert!(matches!(
        editor.handle_key(key(KeyCode::Enter)),
        EditorAction::Continue
    ));
    assert!(editor.editing.is_some());

    for ch in "deepseek-v4-flash".chars() {
        editor.handle_key(key(KeyCode::Char(ch)));
    }
    editor.handle_key(key(KeyCode::Enter));

    assert!(editor.editing.is_none());
    assert_eq!(editor.model, "deepseek-v4-flash");
}

#[test]
fn escape_cancels_input_popup_without_change() {
    let mut editor = editor();
    select(&mut editor, RowKind::Model);
    editor.handle_key(key(KeyCode::Enter));
    editor.handle_key(key(KeyCode::Char('x')));
    editor.handle_key(key(KeyCode::Esc));

    assert!(editor.editing.is_none());
    assert_eq!(editor.model, "");
}

#[test]
fn typing_directly_on_a_row_opens_the_popup() {
    let mut editor = editor();
    select(&mut editor, RowKind::Endpoint);
    editor.handle_key(key(KeyCode::Char('h')));

    assert!(editor.editing.is_some());
    editor.handle_key(key(KeyCode::Enter));
    assert_eq!(editor.endpoint, "h");
}

/// INV-E：编辑器没有思考档位行，保存必须原样带回用户已选档位。
#[test]
fn build_preserves_persisted_thinking_level() {
    let config = ModelConfig {
        model: "custom-model".into(),
        endpoint: "https://api.deepseek.com".into(),
        thinking_level: Some(ThinkingLevel::Max),
        ..ModelConfig::default()
    };
    let credentials = ProviderCredentials::for_protocol(config.protocol);
    let editor = ModelEditor::new_with_descriptors(&config, credentials, Vec::new());
    let (built, _) = editor.build().unwrap();
    assert_eq!(built.thinking_level, Some(ThinkingLevel::Max));
}

/// INV-E：切换预设意味着换模型，旧档位不跨模型携带——归位
/// `None`（新模型跟随预设默认），避免把 DeepSeek 档位错带给
/// GLM 或反之。
#[test]
fn cycling_preset_resets_thinking_level() {
    let config = ModelConfig {
        preset: Some("deepseek-v4-pro".into()),
        model: "deepseek-v4-pro".into(),
        endpoint: "https://api.deepseek.com".into(),
        thinking_level: Some(ThinkingLevel::Max),
        ..ModelConfig::default()
    };
    let credentials = ProviderCredentials::for_protocol(config.protocol);
    let mut editor = ModelEditor::new_with_descriptors(&config, credentials, Vec::new());
    select(&mut editor, RowKind::Preset);
    // 从 pro 起步，一步右移到 GLM（SF-1 删除 vision-exp 后的轮转序）。
    editor.handle_key(key(KeyCode::Right));
    assert_eq!(editor.preset.map(|preset| preset.id), Some("glm-5.3"));
    let (built, _) = editor.build().unwrap();
    assert_eq!(built.thinking_level, None);
}

/// INV-MM2-3（MM-2 W2 红测）：编辑器保存写 typed overrides——
/// 缓冲值与 preset-managed 默认精确相等 → Inherit（不粘滞），
/// 不等 → Set；thinking 档位 Some → Set。pre-fix（无 overrides
/// 推导）编译级红。
#[test]
fn editor_build_derives_non_sticky_overrides() {
    // 生产路径：编辑器拿 model_state 的 effective 配置（preset 已
    // stamp）——缓冲显示 128K/1M。
    let mut config = ModelConfig {
        preset: Some("glm-5.3".into()),
        model: "glm-5.3".into(),
        endpoint: "https://open.bigmodel.cn/api/coding/paas/v4".into(),
        ..ModelConfig::default()
    };
    crate::presets::preset_by_id("glm-5.3")
        .unwrap()
        .apply(&mut config);
    let credentials = ProviderCredentials::for_protocol(config.protocol);
    let mut editor = ModelEditor::new_with_descriptors(&config, credentials, Vec::new());
    // 编辑器从 effective 值构造：预设 stamp 后 output=128K、
    // 窗口=1M。原样保存 → Inherit（跟随预设，不粘滞）。
    let (built, _) = editor.build().unwrap();
    assert_eq!(built.overrides.output_limit, crate::Override::Inherit);
    assert_eq!(built.overrides.max_context_tokens, crate::Override::Inherit);
    assert_eq!(
        built.overrides.parallel_tool_calls,
        crate::Override::Inherit
    );
    assert_eq!(built.overrides.thinking_level, crate::Override::Inherit);
    assert_eq!(built.overrides_version, Some(1));

    // 用户改 output 缓冲 → Set（预设切换后仍存活）。
    editor.output_limit = "100000".into();
    let (built, _) = editor.build().unwrap();
    assert_eq!(built.overrides.output_limit, crate::Override::Set(100_000));
    assert_eq!(built.overrides.max_context_tokens, crate::Override::Inherit);

    // thinking 档位（Shift+Tab 隐藏字段）→ Set。
    editor.thinking_level = Some(ThinkingLevel::Max);
    let (built, _) = editor.build().unwrap();
    assert_eq!(
        built.overrides.thinking_level,
        crate::Override::Set(ThinkingLevel::Max)
    );
}

/// W2b：Clear 有独立可见入口，不能再与空缓冲/Inherit 混同。
/// Ctrl+D 在受控字段上切换 tombstone；重新编辑/循环该字段会解除
/// Clear。profile 的 Thinking 行覆盖隐藏 thinking override 的入口。
#[test]
fn editor_ctrl_d_roundtrips_clear_overrides() {
    let mut config = ModelConfig {
        preset: Some("glm-5.3".into()),
        model: "glm-5.3".into(),
        endpoint: "https://open.bigmodel.cn/api/coding/paas/v4".into(),
        ..ModelConfig::default()
    };
    crate::presets::preset_by_id("glm-5.3")
        .unwrap()
        .apply(&mut config);
    let credentials = ProviderCredentials::for_protocol(config.protocol);
    let mut editor = ModelEditor::new_with_descriptors(&config, credentials, Vec::new());
    select(&mut editor, RowKind::Advanced);
    editor.handle_key(key(KeyCode::Enter));

    let clear = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
    for kind in [
        RowKind::OutputLimit,
        RowKind::ContextWindow,
        RowKind::Temperature,
        RowKind::Parallel,
    ] {
        select(&mut editor, kind);
        editor.handle_key(clear);
        assert!(
            editor.row_label(kind).1.contains("cleared"),
            "{kind:?} exposes the tombstone state"
        );
    }
    let (built, _) = editor.build().unwrap();
    assert_eq!(built.overrides.output_limit, crate::Override::Clear);
    assert_eq!(built.overrides.max_context_tokens, crate::Override::Clear);
    assert_eq!(built.overrides.temperature, crate::Override::Clear);
    assert_eq!(built.overrides.parallel_tool_calls, crate::Override::Clear);

    // Editing a cleared field is an explicit replacement, not a sticky
    // tombstone. This existing editor path leaves the preset, therefore
    // the numeric value is an explicit Set.
    commit_popup_on(&mut editor, RowKind::OutputLimit, "131072");
    let (built, _) = editor.build().unwrap();
    assert_eq!(built.overrides.output_limit, crate::Override::Set(131_072));

    let profile_config = ModelConfig {
        model: "custom-model".into(),
        endpoint: "https://api.deepseek.com".into(),
        thinking_level: Some(ThinkingLevel::High),
        output_limit: Some(32 * 1024),
        max_context_tokens: Some(128 * 1024),
        ..ModelConfig::default()
    };
    let mut profile = ModelEditor::for_profile(
        "daily",
        &profile_config,
        ProviderCredentials::for_protocol(profile_config.protocol),
        Vec::new(),
    );
    select(&mut profile, RowKind::Thinking);
    profile.handle_key(clear);
    assert!(profile.row_label(RowKind::Thinking).1.contains("cleared"));
    let (built, _) = profile.build().unwrap();
    assert_eq!(built.overrides.thinking_level, crate::Override::Clear);
}

/// TUI-L01：Extra Body 是思考参数的原始事实源。手工提交新 JSON
/// 必须废除隐藏的档位字段——否则 model_state 的二次应用会在下一
/// 次 run 静默否决用户刚保存的内容（如手工 disabled）。
#[test]
fn manual_extra_body_edit_clears_thinking_level() {
    let config = ModelConfig {
        model: "deepseek-v4-pro".into(),
        endpoint: "https://api.deepseek.com".into(),
        thinking_level: Some(ThinkingLevel::Max),
        ..ModelConfig::default()
    };
    let credentials = ProviderCredentials::for_protocol(config.protocol);
    let mut editor = ModelEditor::new_with_descriptors(&config, credentials, Vec::new());
    commit_popup_on(
        &mut editor,
        RowKind::ExtraBody,
        r#"{"thinking":{"type":"disabled"}}"#,
    );
    let (built, _) = editor.build().unwrap();
    assert_eq!(
        built.thinking_level, None,
        "hand-committed extra body must revoke the hidden level field"
    );
    assert_eq!(built.extra_body["thinking"]["type"], "disabled");
    assert_eq!(
        built.preset, None,
        "raw extra body must also leave the preset or model_state will overwrite it"
    );
}

/// TUI-L01：预设控制字段一旦由用户手工修改，配置就必须转为
/// Custom。否则 `ModelPreset::apply` 会在下一次加载时覆盖编辑值。
#[test]
fn manual_preset_controlled_fields_mark_the_config_custom() {
    let mut preset_config = ModelConfig::default();
    preset_by_id("deepseek-v4-pro")
        .expect("preset")
        .apply(&mut preset_config);
    let credentials = ProviderCredentials::for_protocol(preset_config.protocol);

    for (row, value) in [
        (RowKind::RequestPath, "/custom/chat"),
        (RowKind::ExtraBody, r#"{"top_p":0.5}"#),
        (RowKind::OutputLimit, "1234"),
        (RowKind::Temperature, "0.4"),
    ] {
        let mut editor =
            ModelEditor::new_with_descriptors(&preset_config, credentials.clone(), Vec::new());
        commit_popup_on(&mut editor, row, value);
        assert_eq!(
            editor.build().expect("build").0.preset,
            None,
            "editing {row:?} must leave the preset"
        );
    }

    for key_code in [KeyCode::Enter, KeyCode::Char(' ')] {
        let mut editor =
            ModelEditor::new_with_descriptors(&preset_config, credentials.clone(), Vec::new());
        select(&mut editor, RowKind::Advanced);
        editor.handle_key(key(KeyCode::Enter));
        select(&mut editor, RowKind::Parallel);
        editor.handle_key(key(key_code));
        assert_eq!(editor.build().expect("build").0.preset, None);
    }
}

/// 跨层状态序列：预设 → 手工 Extra Body → 持久化 → application
/// 重载。修复前编辑器测试会绿，但 `model_state()` 会把 disabled
/// 静默改回预设的 enabled。
#[test]
#[cfg(feature = "runtime-tests")]
fn manual_extra_body_survives_application_model_state_reload() {
    let mut preset_config = ModelConfig::default();
    preset_by_id("deepseek-v4-pro")
        .expect("preset")
        .apply(&mut preset_config);
    preset_config.thinking_level = Some(ThinkingLevel::Max);
    let credentials = ProviderCredentials::for_protocol(preset_config.protocol);
    let mut editor = ModelEditor::new_with_descriptors(&preset_config, credentials, Vec::new());
    commit_popup_on(
        &mut editor,
        RowKind::ExtraBody,
        r#"{"thinking":{"type":"disabled"},"top_p":0.5}"#,
    );
    let (edited, credentials) = editor.build().expect("build edited config");
    assert_eq!(edited.preset, None);
    assert_eq!(edited.thinking_level, None);

    let (storage_root, project_root) = roots("manual-extra-body-reload");
    fs::create_dir_all(&project_root).expect("project");
    let project = Project::new(&project_root);
    let bootstrap = BootstrapApplication::open(project, storage_root.clone()).expect("bootstrap");
    let application = bootstrap
        .authorize_and_mount(crate::ProjectAuthorization::grant())
        .expect("authorize and mount");
    application
        .save_model_state(&edited, &credentials)
        .expect("save model state");

    let (reloaded, _) = application.model_state().expect("reload model state");
    assert_eq!(reloaded.preset, None);
    assert_eq!(reloaded.extra_body["thinking"]["type"], "disabled");
    assert_eq!(reloaded.extra_body["top_p"], 0.5);

    application.close().expect("close");
    fs::remove_dir_all(storage_root).expect("remove storage");
    fs::remove_dir_all(project_root).expect("remove project");
}

/// TUI-L01：手工改 Model/Endpoint 意味着离开原模型，旧档位不得
/// 跨厂商携带（DeepSeek 的 Max 不能在改到 GLM 端点后无提示复活）。
#[test]
fn manual_model_or_endpoint_edit_clears_thinking_level() {
    let config = ModelConfig {
        preset: Some("deepseek-v4-pro".into()),
        model: "deepseek-v4-pro".into(),
        endpoint: "https://api.deepseek.com".into(),
        thinking_level: Some(ThinkingLevel::Max),
        ..ModelConfig::default()
    };
    let credentials = ProviderCredentials::for_protocol(config.protocol);

    let mut editor =
        ModelEditor::new_with_descriptors(&config.clone(), credentials.clone(), Vec::new());
    commit_popup_on(
        &mut editor,
        RowKind::Endpoint,
        "https://open.bigmodel.cn/api/coding/paas/v4",
    );
    let (built, _) = editor.build().unwrap();
    assert_eq!(built.thinking_level, None);
    assert_eq!(built.preset, None);

    let mut editor = ModelEditor::new_with_descriptors(&config, credentials, Vec::new());
    commit_popup_on(&mut editor, RowKind::Model, "glm-5.3");
    let (built, _) = editor.build().unwrap();
    assert_eq!(built.thinking_level, None);
}

#[test]
fn cycling_preset_applies_official_deepseek_parameters() {
    let mut editor = editor();
    select(&mut editor, RowKind::Preset);
    editor.handle_key(key(KeyCode::Right));
    assert_eq!(
        editor.preset.map(|preset| preset.id),
        Some("deepseek-flash")
    );

    let (config, _) = editor.build().unwrap();
    assert_eq!(config.preset.as_deref(), Some("deepseek-flash"));
    assert_eq!(config.model, "deepseek-flash");
    assert_eq!(config.endpoint, "https://api.deepseek.com");
    assert_eq!(config.protocol, ModelProtocol::OpenAiCompatible);
    assert_eq!(config.request_path, "/chat/completions");
    assert_eq!(config.output_limit, Some(384 * 1024));
    assert_eq!(config.temperature, None);
    assert_eq!(config.extra_body["reasoning_effort"], "high");
    assert_eq!(config.extra_body["thinking"]["type"], "enabled");

    // Next step lands on Pro, then GLM, then GLM Flash, then
    // Qwen, then Kimi, then Tencent, then back to Custom.
    // （SF-1 2026-09-10：vision-exp 下架，Pro 的下一步直达 GLM。）
    editor.handle_key(key(KeyCode::Right));
    assert_eq!(
        editor.preset.map(|preset| preset.id),
        Some("deepseek-v4-pro")
    );
    editor.handle_key(key(KeyCode::Right));
    assert_eq!(editor.preset.map(|preset| preset.id), Some("glm-5.3"));
    let (config, _) = editor.build().unwrap();
    assert_eq!(
        config.endpoint,
        "https://open.bigmodel.cn/api/coding/paas/v4"
    );
    assert_eq!(config.extra_body["thinking"]["clear_thinking"], false);
    editor.handle_key(key(KeyCode::Right));
    assert_eq!(editor.preset.map(|preset| preset.id), Some("glm-5.3-flash"));
    editor.handle_key(key(KeyCode::Right));
    assert_eq!(editor.preset.map(|preset| preset.id), Some("qwen3.8-max"));
    editor.handle_key(key(KeyCode::Right));
    // VP-2B：Qwen Token Plan 两模型（max + flash）连续排布。
    assert_eq!(editor.preset.map(|preset| preset.id), Some("qwen3.8-flash"));
    editor.handle_key(key(KeyCode::Right));
    assert_eq!(editor.preset.map(|preset| preset.id), Some("kimi-k3"));
    editor.handle_key(key(KeyCode::Right));
    assert_eq!(editor.preset.map(|preset| preset.id), Some("hy4-preview"));
    // TC-1：循环经过 Tencent 预设——endpoint 为 Hy Token Plan 专用
    // 端点、extra_body 干净（探针实证不发无效果参数）。
    let (config, _) = editor.build().unwrap();
    assert_eq!(
        config.endpoint,
        "https://api.lkeap.cloud.tencent.com/plan/v3"
    );
    assert_eq!(config.extra_body, serde_json::json!({}));
    editor.handle_key(key(KeyCode::Right));
    assert_eq!(editor.preset, None);
}

#[test]
fn editing_model_or_endpoint_marks_preset_as_custom() {
    let mut editor = editor();
    select(&mut editor, RowKind::Preset);
    editor.handle_key(key(KeyCode::Right));
    assert!(editor.preset.is_some());

    commit_popup(&mut editor, "my-custom-model");
    assert_eq!(editor.model, "my-custom-model");
    assert_eq!(editor.preset, None);
    assert_eq!(editor.build().unwrap().0.preset, None);
}

#[test]
fn advanced_rows_are_hidden_until_toggled() {
    let mut editor = editor();
    assert!(!editor.visible_rows().contains(&RowKind::Protocol));
    assert!(!editor.visible_rows().contains(&RowKind::Temperature));

    select(&mut editor, RowKind::Advanced);
    editor.handle_key(key(KeyCode::Enter));

    assert!(editor.visible_rows().contains(&RowKind::Protocol));
    assert!(editor.visible_rows().contains(&RowKind::Temperature));

    // The selection stays on the Advanced row; toggling off from there
    // keeps it in bounds.
    editor.handle_key(key(KeyCode::Enter));
    assert!(!editor.visible_rows().contains(&RowKind::Protocol));
    assert!(editor.selected < editor.row_count());
    assert_eq!(editor.row_count(), 7);
}

#[test]
fn basic_layout_keeps_the_form_short() {
    let editor = editor();
    // Preset, Model, Endpoint, API Key, Advanced, Save, Cancel.
    assert_eq!(editor.row_count(), 7);
}

fn new_picker() -> ModelPicker {
    ModelPicker::new(&ModelConfig::default(), Vec::new())
}

fn profile_summary(name: &str) -> ProfileSummary {
    ProfileSummary {
        name: name.to_owned(),
        endpoint: "https://api.example.com/v1".into(),
        model: "model-x".into(),
        active: false,
        image_input: false,
    }
}

fn picker_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn picker_lists_vendors_then_models_in_two_levels() {
    let mut picker = new_picker();
    // 一级：五个厂商 + Custom。
    assert_eq!(picker.row_count(), 6);

    // Enter 进入 DeepSeek 二级（V4.1 Flash / Pro；SF-1 后两模型）。
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Enter)),
        PickerAction::Continue
    ));
    assert_eq!(picker.row_count(), 2);

    // 确认第一个模型。
    let action = picker.handle_key(picker_key(KeyCode::Enter));
    let PickerAction::SelectPreset(preset) = action else {
        panic!("expected SelectPreset, got {action:?}");
    };
    assert_eq!(preset.id, "deepseek-flash");
}

#[test]
fn picker_esc_backtracks_one_level_then_cancels() {
    let mut picker = new_picker();
    picker.handle_key(picker_key(KeyCode::Down));
    picker.handle_key(picker_key(KeyCode::Enter));
    // MM-2：GLM Coding Plan 下有两个模型。
    assert_eq!(picker.row_count(), 2);

    // 二级 Esc 返回一级。
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Esc)),
        PickerAction::Continue
    ));
    assert_eq!(picker.row_count(), 6);
    // 一级 Esc 关闭。
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Esc)),
        PickerAction::Cancel
    ));
}

/// U1（INV-U1 原位返回，用户反馈 2026-08-22）：从一级第 i 行进入
/// 二级厂商列表，Esc 返回一级时光标必须停在第 i 行（进入时的行），
/// 不重置到首行。删除 home_row 记忆（Esc 恢复 selected=0）→ 红。
#[test]
fn vendor_escape_restores_the_entered_row() {
    let mut picker = new_picker();
    for _ in 0..2 {
        picker.handle_key(picker_key(KeyCode::Down));
    }
    picker.handle_key(picker_key(KeyCode::Enter)); // 进入第 3 行 Qwen
    // VP-2B：Qwen Token Plan 两模型（max + flash）。
    assert_eq!(picker.row_count(), 2);
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Esc)),
        PickerAction::Continue
    ));
    assert_eq!(picker.row_count(), 6, "Esc backtracks to level 1");
    assert_eq!(
        picker.selected_index(),
        2,
        "Esc restores the row we entered from"
    );
}

/// U1（INV-U1 原位返回）：Custom 档案列表 Esc 返回一级时，光标
/// 停在 Custom 行（进入档案列表前的位置）。删除 home_row → 红。
#[test]
fn custom_list_escape_restores_the_custom_row() {
    let profiles = vec![profile_summary("work"), profile_summary("personal")];
    let mut picker = ModelPicker::new(&ModelConfig::default(), profiles);
    for _ in 0..5 {
        picker.handle_key(picker_key(KeyCode::Down));
    }
    picker.handle_key(picker_key(KeyCode::Enter)); // Custom 行 → 档案列表
    assert_eq!(picker.row_count(), 3);
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Esc)),
        PickerAction::Continue
    ));
    assert_eq!(picker.row_count(), 6, "Esc backtracks to level 1");
    assert_eq!(picker.selected_index(), 5, "Esc restores the Custom row");
}

/// U1（INV-U1 原位返回）：快照往返——从 Custom 档案列表进入编辑器
/// 后取消，重建的 picker 必须回到档案列表内原光标行（而非一级）。
/// 删除 restore_snapshot 恢复逻辑 → 红。
#[test]
fn snapshot_restore_roundtrips_level_and_cursor() {
    let profiles = vec![profile_summary("work"), profile_summary("personal")];
    let mut picker = ModelPicker::new(&ModelConfig::default(), profiles);
    for _ in 0..5 {
        picker.handle_key(picker_key(KeyCode::Down));
    }
    picker.handle_key(picker_key(KeyCode::Enter)); // Custom → 档案列表
    picker.handle_key(picker_key(KeyCode::Down)); // 光标落第二个档案
    let snapshot = picker.snapshot();

    // App 侧重建路径：新实例 + 快照恢复（编辑器取消时同款）。
    let mut restored = ModelPicker::new(
        &ModelConfig::default(),
        vec![profile_summary("work"), profile_summary("personal")],
    );
    restored.restore_snapshot(snapshot);
    assert_eq!(restored.row_count(), 3, "back inside the custom list");
    assert_eq!(
        restored.selected_index(),
        1,
        "cursor back on the 2nd profile"
    );
}

/// U3（排序约定，用户反馈 2026-08-22）：枚举档位从小到大——缺省位
/// 在序中不抢首位，Off 殿后。回退为「缺省优先」旧序（32K/8K/128K
/// 或 10M/1M/50M）→ 红。
#[test]
fn profile_enum_choices_are_sorted_small_to_large() {
    assert!(
        CONTEXT_CHOICES.windows(2).all(|pair| pair[0] < pair[1]),
        "context choices ascend: {CONTEXT_CHOICES:?}"
    );
    assert!(
        OUTPUT_CHOICES.windows(2).all(|pair| pair[0] < pair[1]),
        "output choices ascend: {OUTPUT_CHOICES:?}"
    );
    let numeric: Vec<u64> = BUDGET_CHOICES
        .iter()
        .filter_map(|choice| choice.filter(|budget| *budget > 0))
        .collect();
    assert!(
        numeric.windows(2).all(|pair| pair[0] < pair[1]),
        "budget choices ascend: {numeric:?}"
    );
    // 特殊位殿后：off 是最后一个档；缺省位落 32K / 系统缺省。
    assert_eq!(BUDGET_CHOICES.last(), Some(&Some(0)), "off trails");
    assert_eq!(OUTPUT_CHOICES[OUTPUT_DEFAULT], 32 * 1024);
    assert_eq!(BUDGET_CHOICES[BUDGET_DEFAULT], None);
    let template = ModelEditor::new_profile_template(Vec::new());
    assert_eq!(template.output_choice, OUTPUT_DEFAULT);
    assert_eq!(template.budget_choice, BUDGET_DEFAULT);
}

#[test]
fn picker_digits_quick_select_models_and_custom() {
    let mut picker = new_picker();
    // 数字 2 直接进入第二个厂商（GLM Coding Plan）。
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Char('2'))),
        PickerAction::Continue
    ));
    // MM-2：GLM Coding Plan 下有两个模型（5.3 与 5.3 Flash）。
    assert_eq!(picker.row_count(), 2);
    // 数字 1 确认第一个模型，数字 2 选 Flash。
    let PickerAction::SelectPreset(preset) = picker.handle_key(picker_key(KeyCode::Char('1')))
    else {
        panic!("expected SelectPreset");
    };
    assert_eq!(preset.id, "glm-5.3");

    // 一级数字 3 进入 Qwen Token Plan（VP-2B：两模型），5 快选
    // Custom。
    let mut picker = new_picker();
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Char('3'))),
        PickerAction::Continue
    ));
    assert_eq!(picker.row_count(), 2);
    let PickerAction::SelectPreset(preset) = picker.handle_key(picker_key(KeyCode::Char('1')))
    else {
        panic!("expected SelectPreset");
    };
    assert_eq!(preset.id, "qwen3.8-max");

    // B9：零档案时数字 6（Custom）直进新建页（TC-1 后一级为
    // 五厂商 + Custom）。
    let mut picker = new_picker();
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Char('6'))),
        PickerAction::OpenProfileEditor { edit: None }
    ));
}

// ---- B9：档案模板与 picker 三态（验收①②⑤⑦⑧）。

/// 验收⑦（防雷回归）：档案编辑器没有 Preset 循环行——覆写地雷在
/// 自定义入口不存在（INV-M5）。删除档案模式分支（行集回落预设
/// 形态）→ 本测试红。
#[test]
fn profile_editor_has_no_preset_row() {
    let editor = ModelEditor::new_profile_template(Vec::new());
    let labels: Vec<String> = editor.rows().into_iter().map(|(label, _)| label).collect();
    assert!(
        !labels.iter().any(|label| label == "Preset"),
        "the profile editor never shows the Preset cycle row: {labels:?}"
    );
    // Name/Model/Endpoint/ApiKey/Context/Output/Budget/Thinking/
    // Advanced/Save/Cancel = 11 行（U2：+Thinking 枚举行）。
    assert_eq!(editor.row_count(), 11);
}

/// 验收②（INV-M4）：只填四个必填文本即可保存——持久化值为保守
/// 缺省集（context 128K / output 32K / budget 系统缺省 10M /
/// request_path 与 auth 默认 / parallel on / protocol compatible）。
#[test]
fn profile_template_saves_with_only_required_fields_filled() {
    let mut editor = ModelEditor::new_profile_template(Vec::new());
    commit_popup_on(&mut editor, RowKind::Name, "work");
    commit_popup_on(&mut editor, RowKind::Model, "my-model");
    commit_popup_on(&mut editor, RowKind::Endpoint, "https://api.example.com/v1");
    commit_popup_on(&mut editor, RowKind::ApiKey, "sk-work");
    let EditorAction::SaveProfile(saved) =
        editor.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
    else {
        panic!("profile save action");
    };
    let ProfileSave {
        name,
        original_name,
        config,
        credentials,
    } = *saved;
    assert_eq!(name, "work");
    assert_eq!(original_name, None);
    assert_eq!(config.preset, None);
    assert_eq!(config.protocol, ModelProtocol::OpenAiCompatible);
    assert_eq!(config.model, "my-model");
    assert_eq!(config.endpoint, "https://api.example.com/v1");
    assert_eq!(config.request_path, "/chat/completions");
    assert_eq!(config.output_limit, Some(32 * 1024));
    assert_eq!(config.max_context_tokens, Some(128 * 1024));
    assert_eq!(config.run_token_budget, None, "default 10M = None");
    assert!(config.parallel_tool_calls);
    assert_eq!(credentials.value(0), Some("sk-work"));
}

/// 验收②延伸：必填缺失（Name 空）拒绝保存并提示。
#[test]
fn profile_template_requires_a_name() {
    let mut editor = ModelEditor::new_profile_template(Vec::new());
    commit_popup_on(&mut editor, RowKind::Model, "my-model");
    commit_popup_on(&mut editor, RowKind::Endpoint, "https://api.example.com/v1");
    let EditorAction::Continue =
        editor.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
    else {
        panic!("missing name must not save");
    };
    assert!(editor.error_text().contains("Profile name is required"));
}

/// U2（INV-M6，负责人拍板 2026-08-22）：档案模板缺省思考档位
/// **High**——与四个内置预设全部 pin `reasoning_effort: high` 的
/// 口径对齐（此前档案强制 None，两套口径不一致）。删除模板缺省
/// 或 build() 的档位携带 → 红。
#[test]
fn profile_template_defaults_thinking_to_high() {
    let mut editor = ModelEditor::new_profile_template(Vec::new());
    assert_eq!(editor.thinking_level, Some(ThinkingLevel::High));
    commit_popup_on(&mut editor, RowKind::Name, "work");
    commit_popup_on(&mut editor, RowKind::Model, "my-model");
    commit_popup_on(&mut editor, RowKind::Endpoint, "https://api.example.com/v1");
    let EditorAction::SaveProfile(saved) =
        editor.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
    else {
        panic!("profile save action");
    };
    assert_eq!(
        saved.config.thinking_level,
        Some(ThinkingLevel::High),
        "the profile row carries the default thinking level"
    );
}

/// U2（INV-M6）：档位随档案往返——带入 Max 保存仍 Max；循环到
/// off 保存为 None（= 不注入 = 跟随厂商缺省）。删除 build() 档案
/// 分支的 thinking_level 携带 → 红。
#[test]
fn profile_thinking_enum_roundtrip_off_and_max() {
    // 真实档案形态（模板创建的保守缺省集）+ 思考档位 Max。
    let config = ModelConfig {
        model: "my-model".into(),
        endpoint: "https://api.example.com/v1".into(),
        max_context_tokens: Some(128 * 1024),
        output_limit: Some(32 * 1024),
        run_token_budget: None,
        thinking_level: Some(ThinkingLevel::Max),
        ..ModelConfig::default()
    };
    let mut editor = ModelEditor::for_profile(
        "work",
        &config,
        ProviderCredentials::for_protocol(config.protocol),
        Vec::new(),
    );
    let EditorAction::SaveProfile(saved) =
        editor.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
    else {
        panic!("profile save action");
    };
    assert_eq!(saved.config.thinking_level, Some(ThinkingLevel::Max));

    // Max 位 → 一步 = off（None）。循环序：Low → High → Max → Off。
    select(&mut editor, RowKind::Thinking);
    editor.handle_key(key(KeyCode::Right));
    assert_eq!(editor.thinking_level, None);
    let EditorAction::SaveProfile(saved) =
        editor.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
    else {
        panic!("profile save action");
    };
    assert_eq!(saved.config.thinking_level, None);
}

/// U2（INV-M6 × TUI-L01 定向豁免）：档案模式下行可见——改
/// Endpoint 不再静默清档位；手工提交 Extra Body 仍清（raw 参数是
/// 事实源，行显示 off 是可见反馈）。删除豁免 → 红。
#[test]
fn profile_extra_body_edit_clears_thinking_but_endpoint_edit_keeps_it() {
    let mut editor = ModelEditor::new_profile_template(Vec::new());
    select(&mut editor, RowKind::Thinking);
    editor.handle_key(key(KeyCode::Right)); // High → Max
    assert_eq!(editor.thinking_level, Some(ThinkingLevel::Max));

    commit_popup_on(&mut editor, RowKind::Endpoint, "https://api.example.com/v1");
    assert_eq!(
        editor.thinking_level,
        Some(ThinkingLevel::Max),
        "editing the endpoint keeps the visible Thinking row"
    );

    commit_popup_on(
        &mut editor,
        RowKind::ExtraBody,
        "{\"reasoning_effort\": \"xhigh\"}",
    );
    assert_eq!(
        editor.thinking_level, None,
        "hand-written Extra Body wins; the visible row flips to off"
    );
}

/// 验收①⑧：零档案 → Custom Enter 直进新建页；≥1 档案 → 档案
/// 列表（档案行 + New… 底行）。
#[test]
fn custom_entry_three_states() {
    // 零档案：Enter 直进新建页。
    let mut picker = ModelPicker::new(&ModelConfig::default(), Vec::new());
    for _ in 0..5 {
        picker.handle_key(picker_key(KeyCode::Down));
    }
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Enter)),
        PickerAction::OpenProfileEditor { edit: None }
    ));

    // 一个档案：Custom 行 Enter → 列表（1 档案行 + New… = 2 行）。
    let profiles = vec![profile_summary("work")];
    let mut picker = ModelPicker::new(&ModelConfig::default(), profiles);
    for _ in 0..5 {
        picker.handle_key(picker_key(KeyCode::Down));
    }
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Enter)),
        PickerAction::Continue
    ));
    assert_eq!(picker.row_count(), 2, "profile row + New…");

    // Enter 档案行 = 切换；New… 行 = 新建模板。
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Enter)),
        PickerAction::SwitchProfile(name) if name == "work"
    ));
    let mut picker = ModelPicker::new(&ModelConfig::default(), vec![profile_summary("work")]);
    for _ in 0..5 {
        picker.handle_key(picker_key(KeyCode::Down));
    }
    picker.handle_key(picker_key(KeyCode::Enter));
    picker.handle_key(picker_key(KeyCode::Down));
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Enter)),
        PickerAction::OpenProfileEditor { edit: None }
    ));

    // `e` = 编辑既有档案。
    let mut picker = ModelPicker::new(&ModelConfig::default(), vec![profile_summary("work")]);
    for _ in 0..5 {
        picker.handle_key(picker_key(KeyCode::Down));
    }
    picker.handle_key(picker_key(KeyCode::Enter));
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Char('e'))),
        PickerAction::OpenProfileEditor { edit: Some(name) } if name == "work"
    ));
}

/// 验收⑤（picker 腿）：删除两步确认——首按 d 只布防、再按 d 执行、
/// 其余键取消；New… 行不可删。
#[test]
fn profile_delete_requires_double_confirmation() {
    let profiles = vec![profile_summary("work"), profile_summary("personal")];
    let mut picker = ModelPicker::new(&ModelConfig::default(), profiles);
    for _ in 0..5 {
        picker.handle_key(picker_key(KeyCode::Down));
    }
    picker.handle_key(picker_key(KeyCode::Enter));

    // 首按 d：布防，不执行。
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Char('d'))),
        PickerAction::Continue
    ));
    // 非 d 键取消布防（选择不移动——取消键被确认态吞掉）。
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Down)),
        PickerAction::Continue
    ));
    // 重新布防 → 第二次 d 执行删除当前行档案。
    picker.handle_key(picker_key(KeyCode::Char('d')));
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Char('d'))),
        PickerAction::DeleteProfile(name) if name == "work"
    ));

    // New… 行按 d 无操作。
    let mut picker = ModelPicker::new(&ModelConfig::default(), vec![profile_summary("work")]);
    for _ in 0..5 {
        picker.handle_key(picker_key(KeyCode::Down));
    }
    picker.handle_key(picker_key(KeyCode::Enter));
    picker.handle_key(picker_key(KeyCode::Down));
    assert!(matches!(
        picker.handle_key(picker_key(KeyCode::Char('d'))),
        PickerAction::Continue
    ));
}

/// 档位接入（2026-08-23）判别：dsh picker 二级在模型行上
/// Shift+Tab 循环宿主 efforts 表——从当前档位的下一档起步、回绕，
/// pending 只属于高亮行（换行/退级即弃），Enter 随行提交；数字快选
/// 他行不带档位；无档位模型不可循环。删掉循环/pending 即红。
#[test]
fn dsh_picker_cycles_efforts_and_carries_them_on_enter() {
    let data = dsh_model_data_from(&serde_json::json!({
        "groups": [{"id": "deepseek", "name": "DeepSeek", "models": [
            {"id": "deepseek-chat", "name": "DeepSeek Chat",
             "reasoning": {"efforts": [
                 {"id": "off", "name": "Off"},
                 {"id": "low", "name": "Low"},
                 {"id": "high", "name": "High"},
                 {"id": "max", "name": "Max"}
             ]}},
            {"id": "deepseek-coder", "name": "DeepSeek Coder"}
        ]}],
        "failures": [],
        "current": {"provider": "deepseek", "model": "deepseek-chat",
                    "reasoningEffort": "low"}
    }))
    .expect("catalog parses");
    assert_eq!(data.current_effort.as_deref(), Some("low"));
    let mut picker = ModelPicker::new_dsh(data);
    // 进第一组二级。
    picker.handle_key(picker_key(KeyCode::Enter));
    // 当前档位 low → 首个 Shift+Tab 到 high；再按到 max、回绕 off
    //（Enter 确认携带 pending；单元里 picker 不被关闭，连续验证）。
    picker.handle_key(picker_key(KeyCode::BackTab));
    for expected in ["high", "max", "off"] {
        match picker.handle_key(picker_key(KeyCode::Enter)) {
            PickerAction::SelectDshModel { effort, .. } => {
                assert_eq!(effort.as_deref(), Some(expected));
            }
            other => panic!("enter confirms with the pending effort: {other:?}"),
        }
        picker.handle_key(picker_key(KeyCode::BackTab));
    }
    // 数字快选他行（deepseek-coder，无档位）：不带档位。
    let mut picker = ModelPicker::new_dsh(
        dsh_model_data_from(&serde_json::json!({
            "groups": [{"id": "deepseek", "name": "DeepSeek", "models": [
                {"id": "deepseek-chat", "name": "DeepSeek Chat",
                 "reasoning": {"efforts": [{"id": "low", "name": "Low"}]}},
                {"id": "deepseek-coder", "name": "DeepSeek Coder"}
            ]}],
            "current": {"provider": "deepseek", "model": "deepseek-chat"}
        }))
        .expect("catalog parses"),
    );
    picker.handle_key(picker_key(KeyCode::Enter));
    picker.handle_key(picker_key(KeyCode::BackTab));
    match picker.handle_key(picker_key(KeyCode::Char('2'))) {
        PickerAction::SelectDshModel { model, effort, .. } => {
            assert_eq!(model, "deepseek-coder");
            assert_eq!(
                effort, None,
                "digit quick-pick of another row carries no effort"
            );
        }
        other => panic!("digit activates the model row: {other:?}"),
    }
}

#[test]
fn editor_prefills_preset_and_focuses_the_api_key_row() {
    let config = ModelConfig::default();
    let credentials = ProviderCredentials::for_protocol(config.protocol);
    let mut editor = ModelEditor::new_with_descriptors(&config, credentials, Vec::new());
    editor.credentials.set_value(0, "old-vendor-key".into());

    let preset = preset_by_id("glm-5.3").expect("preset");
    editor.apply_preset_and_focus_key(preset);

    // 预设参数就位，旧厂商密钥被清空，焦点落在 API Key 行。
    assert_eq!(editor.model, "glm-5.3");
    assert_eq!(editor.credentials.value(0), Some(""));
    assert_eq!(editor.visible_rows()[editor.selected], RowKind::ApiKey);
}
