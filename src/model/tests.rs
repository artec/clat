use super::*;
use serde_json::json;
use std::time::{Duration, Instant};

#[test]
fn request_estimator_counts_tool_result_images_in_projection_order() {
    let dir = std::env::temp_dir().join(format!(
        "clat-model-image-walker-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let first = dir.join("first.png");
    let second = dir.join("second.png");
    let png_header = |width: u32, height: u32| {
        let mut bytes = vec![
            0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13, b'I', b'H', b'D', b'R',
        ];
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    };
    std::fs::write(&first, png_header(500, 500)).unwrap();
    std::fs::write(&second, png_header(513, 513)).unwrap();

    let plain = ModelItem::ToolResult(crate::tool::ToolResult {
        call_id: "call-1".into(),
        tool_name: "view_image".into(),
        output: json!({"ok": true}),
        is_error: false,
        blocks: Vec::new(),
        image_parts: Vec::new(),
    });
    let mut visual = plain.clone();
    let ModelItem::ToolResult(result) = &mut visual else {
        unreachable!()
    };
    result.image_parts = vec![
        ContentPart::Image {
            path: first.to_string_lossy().into_owned(),
            media_type: "image/png".into(),
        },
        ContentPart::Image {
            path: second.to_string_lossy().into_owned(),
            media_type: "image/png".into(),
        },
    ];

    let walked = model_item_image_parts(&visual)
        .map(|part| match part {
            ContentPart::Image { path, .. } => path.as_str(),
            ContentPart::Text(_) => unreachable!(),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        walked,
        vec![first.to_str().unwrap(), second.to_str().unwrap()],
        "typed tool-result images preserve recursive provider order"
    );
    let expected_visual =
        crate::media::estimate_image_tokens(&first) + crate::media::estimate_image_tokens(&second);
    assert_eq!(
        estimate_model_item_tokens(&visual) - estimate_model_item_tokens(&plain),
        expected_visual,
        "tool-result images consume the same visual budget as top-level images"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// MS-1：发前模态预检的判定面——用户历史图像与工具结果图像都
/// 计入；纯文本能力拒绝且错误可行动（点名模型、指路视觉模型与
/// /new）；已验证视觉能力放行；纯文本请求不误伤。
#[test]
fn modality_preflight_counts_user_and_tool_result_images() {
    let vision = ModelCapabilities {
        input_modalities: vec![Modality::Text, Modality::Image],
        tool_result_modalities: vec![Modality::Text],
        image_input_verified: true,
    };
    let image = ContentPart::Image {
        path: "/tmp/clat-modality-probe.png".into(),
        media_type: "image/png".into(),
    };
    let history = vec![ModelItem::User {
        content: vec![ContentPart::Text("look".into()), image.clone()],
    }];
    // 纯文本请求 × 默认（fail-closed 纯文本）能力：放行。
    modality_preflight(
        &[ModelItem::user_text("hi")],
        &ModelCapabilities::default(),
        "m",
    )
    .expect("a text-only request never trips the preflight");
    // 历史图像 × 纯文本能力：明确拒绝，错误指路。
    let error = modality_preflight(&history, &ModelCapabilities::default(), "text-model")
        .expect_err("a text-only model must not receive images");
    assert!(error.contains("text-model"), "{error}");
    assert!(error.contains("1 image part"), "{error}");
    assert!(error.contains("vision model"), "{error}");
    assert!(error.contains("/new"), "{error}");
    // 同一请求 × 已验证视觉能力：放行。
    modality_preflight(&history, &vision, "vision-model")
        .expect("a verified vision model receives the image");
    // 工具结果图像同样计入（行走器覆盖递归视觉成本缺口）。
    let tool_result = ModelItem::ToolResult(crate::tool::ToolResult {
        call_id: "call-1".into(),
        tool_name: "view_image".into(),
        output: json!({"ok": true}),
        is_error: false,
        blocks: Vec::new(),
        image_parts: vec![image],
    });
    let error = modality_preflight(&[tool_result], &ModelCapabilities::default(), "text-model")
        .expect_err("tool-result images are model-facing input too");
    assert!(error.contains("1 image part"), "{error}");
}

/// Request-wide estimation must not repeat for text-only history entries.
/// Count estimator invocations rather than relying on machine timing.
#[test]
fn image_projection_text_history_does_not_repeat_request_estimation() {
    let items = (0..512)
        .map(|index| ModelItem::user_text(format!("history {index}")))
        .collect::<Vec<_>>();
    let original = items.clone();
    assert_eq!(items, original);
    let options = ModelOptions {
        image_projection: Some(ImageProjectionBudget {
            max_context_tokens: Some(4096),
            max_request_images: 12,
            max_request_image_bytes: 20_000_000,
        }),
        ..ModelOptions::default()
    };
    assert!(
        project_items_for_image_budget(&items, None, &[], &options).is_err(),
        "read-only estimates must not relax actual request admission"
    );
}

#[test]
fn image_projection_cannot_implicitly_offload_any_turn() {
    let dir = std::env::temp_dir().join(format!(
        "clat-image-offload-order-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let image = dir.join("image.png");
    let mut header = vec![
        0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13, b'I', b'H', b'D', b'R',
    ];
    header.extend_from_slice(&500u32.to_be_bytes());
    header.extend_from_slice(&500u32.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    std::fs::write(&image, header).unwrap();
    let image_part = || ContentPart::Image {
        path: image.to_string_lossy().into_owned(),
        media_type: "image/png".into(),
    };
    let items = vec![
        ModelItem::User {
            content: vec![image_part()],
        },
        ModelItem::ToolResult(crate::tool::ToolResult {
            call_id: "view-1".into(),
            tool_name: "view_image".into(),
            output: json!({"ok": true}),
            is_error: false,
            blocks: Vec::new(),
            image_parts: vec![image_part()],
        }),
        ModelItem::User {
            content: vec![ContentPart::Text("latest".into()), image_part()],
        },
    ];
    let options = ModelOptions {
        image_projection: Some(ImageProjectionBudget {
            max_context_tokens: None,
            max_request_images: 1,
            max_request_image_bytes: u64::MAX,
        }),
        ..ModelOptions::default()
    };
    assert!(
        project_items_for_image_budget(&items, None, &[], &options).is_err(),
        "adapters cannot silently offload historical images"
    );
    let (projected, report) = preview_items_for_image_budget(&items, None, &[], &options);
    assert_eq!(projected, items);
    assert_eq!(report.offloaded_images, 0);
    let latest_only = vec![ModelItem::User {
        content: vec![image_part(), image_part()],
    }];
    let error = project_items_for_image_budget(&latest_only, None, &[], &options)
        .expect_err("the current turn is never silently degraded");
    assert!(error.contains("current turn: 2 images"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn image_projection_capacity_rejects_without_durable_offload() {
    let dir = std::env::temp_dir().join(format!(
        "clat-image-offload-quantum-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let image = dir.join("image.png");
    let mut header = vec![
        0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13, b'I', b'H', b'D', b'R',
    ];
    header.extend_from_slice(&500u32.to_be_bytes());
    header.extend_from_slice(&500u32.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    std::fs::write(&image, header).unwrap();
    let items = (0..5)
        .map(|index| ModelItem::User {
            content: vec![
                ContentPart::Text(format!("turn {index}")),
                ContentPart::Image {
                    path: image.to_string_lossy().into_owned(),
                    media_type: "image/png".into(),
                },
            ],
        })
        .collect::<Vec<_>>();
    let options = |window| ModelOptions {
        output_limit: Some(256),
        image_projection: Some(ImageProjectionBudget {
            max_context_tokens: Some(window),
            max_request_images: ImageProjectionBudget::MAX_REQUEST_IMAGES,
            max_request_image_bytes: ImageProjectionBudget::MAX_REQUEST_IMAGE_BYTES,
        }),
        ..ModelOptions::default()
    };
    assert!(project_items_for_image_budget(&items, None, &[], &options(2048)).is_err());
    assert!(project_items_for_image_budget(&items, None, &[], &options(2100)).is_err());
    assert_eq!(
        preview_items_for_image_budget(&items, None, &[], &options(5120)).0,
        items
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 不变量（2026-08-19 退出延迟）：`child_with_deadline` 聚合父取消
/// 与 deadline——父取消即时传播（退出/Esc 不被 deadline 挡住），
/// deadline 到期独立生效，两者皆无时 token 干净。
#[test]
fn child_deadline_token_inherits_parent_cancellation() {
    let parent = CancelToken::new();
    let child = parent.child_with_deadline(Instant::now() + Duration::from_secs(15));
    assert!(!child.is_cancelled(), "fresh child of a live parent");
    assert!(
        child
            .remaining()
            .is_some_and(|remaining| remaining <= Duration::from_secs(15)),
        "remaining comes from the deadline"
    );

    parent.cancel();
    assert!(
        child.is_cancelled(),
        "parent cancellation propagates instantly to the child"
    );

    let strict = CancelToken::new().child_with_deadline(Instant::now());
    assert!(
        strict.is_cancelled(),
        "an expired deadline cancels on its own"
    );

    let parent_deadline = CancelToken::with_deadline(Instant::now() + Duration::from_secs(1));
    let child = parent_deadline.child_with_deadline(Instant::now() + Duration::from_secs(15));
    let grandchild = child.child_with_deadline(Instant::now() + Duration::from_secs(30));
    assert!(
        grandchild
            .remaining()
            .is_some_and(|remaining| remaining <= Duration::from_secs(1)),
        "the shortest ancestor deadline constrains every descendant"
    );
}

#[test]
fn provider_credentials_preserve_the_legacy_json_array_contract() {
    for protocol in ModelProtocol::ALL {
        let legacy = json!(["legacy-secret"]);
        let credentials = ProviderCredentials::from_json(protocol, &legacy);
        assert_eq!(credentials.value(0), Some("legacy-secret"));
        assert_eq!(credentials.to_json(), legacy);
    }
}

/// INV-MM2-3（MM-2 W2 红测）：typed overrides 三态——Set 覆盖
/// preset-managed 默认、Clear 抑制字段、Inherit 跟随；thinking_level
/// 的厂商映射在 `apply_overrides` 内一次完成（Qwen Max→xhigh），
/// TC-3（2026-09-02）判别：Tencent Hy 思考服务端常开——标题栏
/// 显示 "Thinking · Server"（删 thinking_display 的 Tencent 分支
/// 即红）；Shift+Tab 不可循环（空档位 next 返回 None）；wire 零
/// 参数（thinking_level override 不注入 reasoning_effort/thinking
/// ——删 merge 的空档位门即红）。对照腿：DeepSeek 同 override 正常
/// 注入。
#[test]
fn server_always_on_thinking_displays_without_ladder_or_wire_params() {
    let preset = crate::presets::preset_by_id("hy4").unwrap();
    let mut config = ModelConfig::default();
    preset.apply(&mut config);

    // 显示：常开标记而非整段省略。
    assert_eq!(thinking_display(&config), Some("Server"));
    assert_eq!(effective_thinking_level(&config), None);

    // 循环键无操作：空档位下 next_thinking_level 返回 None。
    assert_eq!(
        next_thinking_level(config.vendor(), ThinkingLevel::High),
        None
    );

    // wire 零参数：即使 override Set 了档位（编辑器/旧配置路径），
    // merge 也不给无档位厂商注入任何思考参数。
    config.overrides.thinking_level = Override::Set(ThinkingLevel::Max);
    config.apply_overrides();
    assert_eq!(config.thinking_level, Some(ThinkingLevel::Max));
    assert!(
        config.extra_body.get("reasoning_effort").is_none(),
        "the effort param has no effect on this vendor (TC-0); it must never be sent"
    );
    assert!(config.extra_body.get("thinking").is_none());
    // 显示仍是 Server（一等字段在，但厂商无档位 → 常开口径）。
    assert_eq!(thinking_display(&config), Some("Server"));

    // 对照腿：DeepSeek 上同样的 override 正常注入。
    let deepseek = crate::presets::preset_by_id("deepseek-flash").unwrap();
    let mut config = ModelConfig::default();
    deepseek.apply(&mut config);
    config.overrides.thinking_level = Override::Set(ThinkingLevel::Max);
    config.apply_overrides();
    assert_eq!(config.extra_body["reasoning_effort"], "max");
    assert_eq!(config.extra_body["thinking"]["type"], "enabled");
    assert_eq!(thinking_display(&config), Some("Max"));
}

/// INV-MM2-3（MM-2 W2 红测）：typed overrides 三态——Set 覆盖
/// preset-managed 默认、Clear 抑制字段、Inherit 跟随；thinking_level
/// 的厂商映射在 `apply_overrides` 内一次完成（Qwen Max→xhigh），
/// Clear 连 reasoning_effort 一起摘除。pre-fix（无 overrides 层）
/// 本测试编译级红。
#[test]
fn overrides_merge_tri_state_with_vendor_mapping_inside() {
    let glm = crate::presets::preset_by_id("glm-5.3").unwrap();
    let mut config = ModelConfig::default();
    glm.apply(&mut config);
    assert_eq!(config.output_limit, Some(128 * 1024), "preset default");

    // Set 覆盖预设。
    config.overrides.output_limit = Override::Set(65_536);
    config.overrides.temperature = Override::Set(0.2);
    config.apply_overrides();
    assert_eq!(config.output_limit, Some(65_536));
    assert_eq!(config.temperature, Some(0.2));

    // Clear 抑制：max_tokens 完全不发（None）、温度不发。
    config.overrides.output_limit = Override::Clear;
    config.overrides.temperature = Override::Clear;
    config.overrides.parallel_tool_calls = Override::Clear;
    config.apply_overrides();
    assert_eq!(config.output_limit, None);
    assert_eq!(config.temperature, None);
    assert_eq!(
        config.request_parallel_tool_calls(),
        None,
        "Clear omits parallel_tool_calls from provider options"
    );

    // thinking_level Set：厂商映射在 merge 内完成（Qwen 端点；旗舰已
    // 更名 qwen4-max，511524fc）。
    let qwen = crate::presets::preset_by_id("qwen4-max").unwrap();
    let mut config = ModelConfig::default();
    qwen.apply(&mut config);
    assert_eq!(
        config.extra_body["reasoning_effort"], "medium",
        "preset pin"
    );
    config.overrides.thinking_level = Override::Set(ThinkingLevel::Max);
    config.apply_overrides();
    assert_eq!(config.extra_body["reasoning_effort"], "xhigh");
    assert_eq!(config.thinking_level, Some(ThinkingLevel::Max));

    // thinking Clear：reasoning_effort 从 extra_body 摘除。
    config.overrides.thinking_level = Override::Clear;
    config.apply_overrides();
    assert!(config.extra_body.get("reasoning_effort").is_none());
    assert_eq!(config.thinking_level, None);

    // run_token_budget 是纯用户 run policy：merge 与预设都不碰。
    config.overrides.output_limit = Override::Set(1_000);
    config.run_token_budget = Some(123_456);
    config.apply_overrides();
    assert_eq!(config.run_token_budget, Some(123_456));
}

/// INV-MM2-3 迁移（W2 红测）：旧配置逐字段——与当时 preset-managed
/// 值精确相等 → Inherit；不等 → Set；版本写 1 且幂等。
#[test]
fn legacy_overrides_migration_is_field_wise_and_idempotent() {
    let glm = crate::presets::preset_by_id("glm-5.3").unwrap();
    let mut config = ModelConfig {
        preset: Some("glm-5.3".into()),
        ..ModelConfig::default()
    };
    // 预设 stamp 的等值（Inherit 候选）与用户值（Set 候选）混排。
    config.output_limit = Some(glm.output_limit);
    config.temperature = Some(0.3);
    config.parallel_tool_calls = true;
    config.thinking_level = Some(ThinkingLevel::Max);
    config.max_context_tokens = Some(glm.context_window);
    config.migrate_legacy_overrides();
    assert_eq!(config.overrides.output_limit, Override::Inherit);
    assert_eq!(config.overrides.temperature, Override::Set(0.3));
    assert_eq!(config.overrides.parallel_tool_calls, Override::Inherit);
    assert_eq!(
        config.overrides.thinking_level,
        Override::Set(ThinkingLevel::Max)
    );
    assert_eq!(config.overrides.max_context_tokens, Override::Inherit);
    assert_eq!(config.overrides_version, Some(1));

    // 幂等：再次迁移不改动（版本门）。
    config.temperature = Some(0.9); // 迁移后被（模拟的）后续编辑改值
    config.migrate_legacy_overrides();
    assert_eq!(
        config.overrides.temperature,
        Override::Set(0.3),
        "version gate: the second migration must not run"
    );

    // 等值上下文窗口的种子值（apply 语义种入 1M）→ Inherit；
    // 手填 500K → Set。
    let mut config = ModelConfig {
        preset: Some("glm-5.3".into()),
        max_context_tokens: Some(500_000),
        ..ModelConfig::default()
    };
    config.migrate_legacy_overrides();
    assert_eq!(config.overrides.max_context_tokens, Override::Set(500_000));

    // 无预设的 custom：false（异于缺省 true）→ Set，true → Inherit。
    let mut config = ModelConfig {
        parallel_tool_calls: false,
        ..ModelConfig::default()
    };
    config.migrate_legacy_overrides();
    assert_eq!(config.overrides.parallel_tool_calls, Override::Set(false));
}

/// OC-18 判别（真机毒化现场复刻，2026-11-19 负责人 settings.json）：
/// 旧二进制按旧预设表种下 grok output_limit 500,000 → 钉值修正
/// （262,144）后 legacy 迁移把 500,000 误判为用户 Set → 每次加载
/// `apply_overrides` 把新钉值盖回 → 容量预检永久拒绝（提问气泡都发
/// 不出）。修复后：迁移把历史种值归 Inherit；**已毒化**的 version-1
/// 配置（Set(500,000) 已落盘）在 apply_overrides 加载即自愈。pre-fix
/// 两条腿全红（迁移产出 Set；自愈腿 output_limit 仍 500,000 且预算
/// Err）。
#[test]
fn historical_preset_seeds_never_become_user_overrides() {
    // 腿一：legacy 迁移认得历史种值（grok 500,000 / kimi 1M×2 /
    // deepseek 393,216 / kimi 旧窗口 1M 整）——归 Inherit，不再升格
    // 为 Set。
    let mut grok = ModelConfig {
        preset: Some("opencode-grok-4.7".into()),
        output_limit: Some(500_000),
        max_context_tokens: Some(500_000),
        ..ModelConfig::default()
    };
    grok.migrate_legacy_overrides();
    assert_eq!(
        grok.overrides.output_limit,
        Override::Inherit,
        "a value CLAT itself stamped under the old preset table is not a user choice"
    );
    assert_eq!(grok.overrides.max_context_tokens, Override::Inherit);
    let mut kimi = ModelConfig {
        preset: Some("opencode-kimi-k3.1-pro".into()),
        output_limit: Some(1_000_000),
        max_context_tokens: Some(1_000_000),
        ..ModelConfig::default()
    };
    kimi.migrate_legacy_overrides();
    assert_eq!(kimi.overrides.output_limit, Override::Inherit);
    assert_eq!(kimi.overrides.max_context_tokens, Override::Inherit);

    // 腿二：已毒化的 version-1 现场（与负责人 settings.json 同形：
    // Set(500,000)+Set(Max)）——加载路径 apply → apply_overrides 自愈：
    // output 回落当前预设 262,144，容量预算通过；用户真值不受影响。
    let preset = crate::presets::preset_by_id("opencode-grok-4.7").unwrap();
    let mut poisoned = ModelConfig::default();
    preset.apply(&mut poisoned);
    poisoned.overrides_version = Some(1);
    poisoned.overrides.output_limit = Override::Set(500_000);
    poisoned.overrides.thinking_level = Override::Set(ThinkingLevel::Max);
    poisoned.apply_overrides();
    assert_eq!(poisoned.output_limit, Some(262_144));
    assert!(
        crate::model::ContextBudget::resolve(&poisoned).is_ok(),
        "the healed config must pass the capacity budget (run preparation unblocked)"
    );
    // 非种值用户选择照旧生效（500,000 不是 glm 的种值）。
    let mut glm = ModelConfig::default();
    crate::presets::preset_by_id("glm-5.3")
        .unwrap()
        .apply(&mut glm);
    glm.overrides.output_limit = Override::Set(500_000);
    glm.apply_overrides();
    assert_eq!(glm.output_limit, Some(500_000));
}

/// INV-B：`apply_thinking_level` 是线上思考参数的唯一写入口，
/// 任何输入都产出 `enabled` + 三档之一，且保留 thinking 对象内
/// 的其它键（GLM 的 `clear_thinking`）。Kimi/Qwen 不携带 thinking
/// 对象、reasoning_effort 按厂商+模型映射（Qwen 自家三档
/// low/medium/xhigh；共享端点上的第三方家族见下一测试）。
#[test]
fn apply_thinking_level_always_enables_and_keeps_foreign_keys() {
    for level in [ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max] {
        let mut glm = json!({"thinking": {"type": "enabled", "clear_thinking": false}});
        apply_thinking_level(&mut glm, ModelVendor::Glm, "glm-5.3", level);
        assert_eq!(glm["thinking"]["type"], "enabled");
        assert_eq!(glm["thinking"]["clear_thinking"], false);
        assert_eq!(glm["reasoning_effort"], level.wire_effort(ModelVendor::Glm));
    }
    // 空 extra_body 从零构造 thinking 对象（DeepSeek/GLM 风格厂商）。
    let mut empty = json!({});
    apply_thinking_level(
        &mut empty,
        ModelVendor::DeepSeek,
        "deepseek-flash",
        ThinkingLevel::Low,
    );
    assert_eq!(empty["thinking"]["type"], "enabled");
    assert_eq!(empty["reasoning_effort"], "low");
    // 非 object 的 extra_body 被替换为 object，不会 panic。
    let mut bogus = json!("not an object");
    apply_thinking_level(
        &mut bogus,
        ModelVendor::DeepSeek,
        "deepseek-v4-pro",
        ThinkingLevel::Max,
    );
    assert_eq!(bogus["reasoning_effort"], "max");
    // Kimi/Qwen：顶层 reasoning_effort，不注入 thinking 对象（未定义
    // 参数不发给严格网关）。
    let mut kimi = json!({});
    apply_thinking_level(
        &mut kimi,
        ModelVendor::Kimi,
        "k3.1-pro",
        ThinkingLevel::High,
    );
    assert_eq!(kimi["reasoning_effort"], "high");
    assert!(kimi.get("thinking").is_none());
    // Qwen 自家模型三档映射：Low→low、High→medium、Max→xhigh——三档
    // 各有效果（官方兼容表 high/max 均归并为 xhigh，透传会令两档
    // 无差别）。
    let mut qwen = json!({});
    apply_thinking_level(
        &mut qwen,
        ModelVendor::Qwen,
        "qwen4-max",
        ThinkingLevel::Low,
    );
    assert_eq!(qwen["reasoning_effort"], "low");
    apply_thinking_level(
        &mut qwen,
        ModelVendor::Qwen,
        "qwen4-max",
        ThinkingLevel::High,
    );
    assert_eq!(qwen["reasoning_effort"], "medium");
    assert!(qwen.get("thinking").is_none());
    apply_thinking_level(
        &mut qwen,
        ModelVendor::Qwen,
        "qwen4-max",
        ThinkingLevel::Max,
    );
    assert_eq!(qwen["reasoning_effort"], "xhigh");
    // 逆映射：Qwen 的 medium 解析回 High（其它厂商归并为 High）。
    assert_eq!(
        ThinkingLevel::from_wire_effort(ModelVendor::Qwen, "xhigh"),
        ThinkingLevel::Max
    );
    assert_eq!(
        ThinkingLevel::from_wire_effort(ModelVendor::Qwen, "medium"),
        ThinkingLevel::High
    );
}

/// Qwen Token Plan / 百炼代管的第三方 DeepSeek 系列（2026-11-28 新增
/// deepseek-v4.1-flash-1017 预设）：官方 `reasoning_effort` 档位是
/// DeepSeek 拼写 low/high/max、默认 high（help.aliyun.com/zh/
/// model-studio/deepseek-api）——不是 Qwen 自家的 low/medium/xhigh。
/// 判别：删 `wire_effort_for` 的 deepseek 分支（回落纯厂商映射）此
/// 测试红——High→medium、Max→xhigh 都在百炼 DeepSeek 档位表之外。
#[test]
fn qwen_endpoint_deepseek_series_keeps_the_deepseek_effort_spelling() {
    for (level, wire) in [
        (ThinkingLevel::Low, "low"),
        (ThinkingLevel::High, "high"),
        (ThinkingLevel::Max, "max"),
    ] {
        // 快照 id（预设的 model）与稳定 id 同样命中。
        for model in ["deepseek-v4.1-flash-1017", "deepseek-v4.1-flash"] {
            let mut body = json!({});
            apply_thinking_level(&mut body, ModelVendor::Qwen, model, level);
            assert_eq!(body["reasoning_effort"], wire, "{model} {level:?}");
            // 不注入 thinking 对象（未定义参数不发给严格网关）。
            assert!(body.get("thinking").is_none(), "{model}");
        }
    }
    // Qwen 自家模型不受该分支影响（厂商默认映射照旧）。
    let mut qwen = json!({});
    apply_thinking_level(
        &mut qwen,
        ModelVendor::Qwen,
        "qwen4-max",
        ThinkingLevel::High,
    );
    assert_eq!(qwen["reasoning_effort"], "medium");
    // 其它厂商的 deepseek 模型走各自厂商默认（DeepSeek 拼写本就一致）。
    let mut direct = json!({});
    apply_thinking_level(
        &mut direct,
        ModelVendor::DeepSeek,
        "deepseek-v4.1-flash-1017",
        ThinkingLevel::High,
    );
    assert_eq!(direct["reasoning_effort"], "high");
    assert_eq!(direct["thinking"]["type"], "enabled");
}

/// 新厂商端点识别：Kimi Coding 会员端点 / 开放平台端点、Qwen
/// Token Plan 专用 MaaS 域名 / 百炼按量域名。
#[test]
fn endpoint_vendor_recognizes_kimi_and_qwen() {
    assert_eq!(
        endpoint_vendor("https://api.kimi.com/coding/v1"),
        ModelVendor::Kimi
    );
    assert_eq!(
        endpoint_vendor("https://api.moonshot.cn/v1"),
        ModelVendor::Kimi
    );
    assert_eq!(
        endpoint_vendor("https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"),
        ModelVendor::Qwen
    );
    // VP-2C：千问 AI 平台 Token Plan 入口——同厂商同 Key 槽同阶梯。
    assert_eq!(
        endpoint_vendor("https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1"),
        ModelVendor::Qwen
    );
    assert_eq!(ModelVendor::Qwen.storage_key(), Some("Qwen"));
    assert_eq!(
        endpoint_vendor("https://dashscope.aliyuncs.com/compatible-mode/v1"),
        ModelVendor::Qwen
    );
    assert_eq!(
        endpoint_vendor("https://api.example.com"),
        ModelVendor::Other
    );
    // Kimi/Qwen 也提供思考档位（Shift+Tab 可用）。
    assert!(!thinking_levels(ModelVendor::Kimi).is_empty());
    assert!(!thinking_levels(ModelVendor::Qwen).is_empty());
}

/// OC-Go（2026-11-18）：OpenCode 合规网关（opencode.ai/inference/go）
/// 的端点识别、key 记忆槽、三档线上映射（网关统一档位
/// low/medium/high/max——Low→low、High→high、Max→max 三档直取）与
/// 逆映射（medium 解析回 High，与 Qwen 的 medium≡High 显示约定同款）。
#[test]
fn opencode_vendor_maps_three_levels_directly() {
    assert_eq!(
        endpoint_vendor("https://opencode.ai/inference/go/v1"),
        ModelVendor::OpenCode
    );
    assert_eq!(
        endpoint_vendor("https://OPencode.AI/inference/go/v1"),
        ModelVendor::OpenCode,
        "endpoint matching is case-insensitive like every other vendor arm"
    );
    assert_eq!(ModelVendor::OpenCode.storage_key(), Some("OpenCode"));
    assert_eq!(
        thinking_levels(ModelVendor::OpenCode),
        &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max]
    );
    assert_eq!(ThinkingLevel::Low.wire_effort(ModelVendor::OpenCode), "low");
    assert_eq!(
        ThinkingLevel::High.wire_effort(ModelVendor::OpenCode),
        "high"
    );
    assert_eq!(ThinkingLevel::Max.wire_effort(ModelVendor::OpenCode), "max");
    assert_eq!(
        ThinkingLevel::from_wire_effort(ModelVendor::OpenCode, "medium"),
        ThinkingLevel::High
    );
    // 网关无 thinking 对象：apply 不注入（未定义参数不发给严格网关）。
    let mut body = serde_json::json!({});
    apply_thinking_level(
        &mut body,
        ModelVendor::OpenCode,
        "minimax-m3",
        ThinkingLevel::High,
    );
    assert_eq!(body["reasoning_effort"], "high");
    assert!(body.get("thinking").is_none());
}

/// OC-17 哨兵：OpenCode 网关的**逐模型**思考阶梯与 2026-11-19
/// `/v1/models` 快照的 effort 数组逐条等价——判据：CLAT 档位 L 可用
/// ⇔ `wire_effort(L)` ∈ 该模型数组。真机红类：厂商级三档让 grok/
/// gemini/qwen4-max 可选 Max（数组无 max）、deepseek-v4-pro 可选 Low
/// （数组无 low）、kimi-k3 可选 Low/High（数组仅 max）、minimax 无
/// 档位却三档全开。网关数组漂移时本表与 OC-14 的 effort pin 表一起
/// 人工复核。
#[test]
fn opencode_model_ladders_match_the_gateway_arrays() {
    let vendor = ModelVendor::OpenCode;
    for (model, array) in [
        ("deepseek-v4-pro", &["high", "max"][..]),
        ("deepseek-v4.1-flash", &["low", "high", "max"]),
        ("kimi-k3.1-pro", &["low", "high", "max"]),
        ("kimi-k3", &["max"]),
        ("glm-5.3", &["low", "high", "max"]),
        ("glm-5.3-flash", &["low", "high", "max"]),
        ("qwen4-max", &["low", "high", "xhigh"]),
        ("minimax-m3", &[]),
        (
            "claude-haiku-5-5",
            &["low", "medium", "high", "xhigh", "max"],
        ),
        (
            "gpt-6-luna",
            &["none", "low", "medium", "high", "xhigh", "max"],
        ),
        ("gemini-4-argon", &["low", "medium", "high"]),
        ("grok-4.7", &["low", "medium", "high", "xhigh"]),
    ] {
        let levels = model_thinking_levels(vendor, model);
        for level in [ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max] {
            let selectable = levels.contains(&level);
            let wire_valid = array.contains(&level.wire_effort(vendor));
            assert_eq!(
                selectable, wire_valid,
                "{model}: {level:?} selectability must equal wire validity (array {array:?})"
            );
        }
    }
    // 未知网关模型 fail-closed：无档位（不提供无证据的循环）。
    assert!(model_thinking_levels(vendor, "mimo-v2.6-pro").is_empty());
    // 非网关厂商逐模型表 == 厂商表（不改变既有语义）。
    assert_eq!(
        model_thinking_levels(ModelVendor::DeepSeek, "deepseek-flash"),
        thinking_levels(ModelVendor::DeepSeek)
    );
}

/// OC-17：陈旧/跨模型携带的一等 thinking_level 不显示、不发送——
/// effective 回落预设 pin 解析（显示 == 实际发送），override merge
/// 不把阶梯外的档位写进 extra_body。
#[test]
fn out_of_ladder_thinking_level_falls_back_to_the_preset_pin() {
    // grok（阶梯 [Low, High]）+ 持久化 Max（OC-16 真机现场）：显示回落
    // 预设 pin medium → High；merge 不把 max 写进 extra_body。
    let mut config = ModelConfig::default();
    crate::preset_by_id("opencode-grok-4.7")
        .expect("grok preset")
        .apply(&mut config);
    config.overrides.thinking_level = Override::Set(ThinkingLevel::Max);
    config.thinking_level = Some(ThinkingLevel::Max);
    config.apply_overrides();
    assert_eq!(
        config.extra_body.get("reasoning_effort"),
        Some(&serde_json::json!("medium")),
        "an out-of-ladder level must not override the preset pin"
    );
    assert_eq!(
        effective_thinking_level(&config),
        Some(ThinkingLevel::High),
        "display falls back to the actually-sent preset pin (medium ≡ High)"
    );
    // 循环从回落档继续，且只在 [Low, High] 内 wrap——Max 永不可达。
    assert_eq!(
        next_thinking_level_for(ModelVendor::OpenCode, "grok-4.7", ThinkingLevel::High),
        Some(ThinkingLevel::Low)
    );
    assert_eq!(
        next_thinking_level_for(ModelVendor::OpenCode, "grok-4.7", ThinkingLevel::Max),
        Some(ThinkingLevel::Low),
        "an out-of-ladder current restarts from the first in-ladder level"
    );
    // 阶梯内档位照常生效（kimi-k3 仅 Max：pin max 显示 Max）。
    let mut kimi = ModelConfig::default();
    crate::preset_by_id("opencode-kimi-k3")
        .expect("kimi preset")
        .apply(&mut kimi);
    assert_eq!(effective_thinking_level(&kimi), Some(ThinkingLevel::Max));
    // minimax 无档位：显示 None，循环不可用。
    let mut minimax = ModelConfig::default();
    crate::preset_by_id("opencode-minimax-m3")
        .expect("minimax preset")
        .apply(&mut minimax);
    assert_eq!(effective_thinking_level(&minimax), None);
    assert_eq!(
        next_thinking_level_for(ModelVendor::OpenCode, "minimax-m3", ThinkingLevel::High),
        None
    );
}

/// INV-D：循环只在厂商支持的档位集合内 wrap；Other 厂商无档位。
#[test]
fn next_thinking_level_wraps_within_vendor_levels() {
    assert_eq!(
        thinking_levels(ModelVendor::DeepSeek),
        &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max]
    );
    assert_eq!(
        thinking_levels(ModelVendor::Glm),
        &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max]
    );
    assert_eq!(
        next_thinking_level(ModelVendor::DeepSeek, ThinkingLevel::Low),
        Some(ThinkingLevel::High)
    );
    assert_eq!(
        next_thinking_level(ModelVendor::Glm, ThinkingLevel::Low),
        Some(ThinkingLevel::High)
    );
    assert_eq!(
        next_thinking_level(ModelVendor::Glm, ThinkingLevel::High),
        Some(ThinkingLevel::Max)
    );
    assert_eq!(
        next_thinking_level(ModelVendor::Glm, ThinkingLevel::Max),
        Some(ThinkingLevel::Low)
    );
    assert_eq!(
        next_thinking_level(ModelVendor::DeepSeek, ThinkingLevel::Max),
        Some(ThinkingLevel::Low)
    );
    assert_eq!(thinking_levels(ModelVendor::Other), &[]);
    assert_eq!(
        next_thinking_level(ModelVendor::Other, ThinkingLevel::High),
        None
    );
}

/// GLM 5.3 的 `low` 是真实档位（Lightweight Reasoning），字段与
/// 线上值两条路径都如实解析。
#[test]
fn glm_low_effort_is_a_real_level() {
    let mut config = ModelConfig {
        endpoint: "https://open.bigmodel.cn/api/coding/paas/v4".into(),
        ..ModelConfig::default()
    };
    config.extra_body = json!({"reasoning_effort": "low"});
    assert_eq!(effective_thinking_level(&config), Some(ThinkingLevel::Low));
    config.thinking_level = Some(ThinkingLevel::Low);
    assert_eq!(effective_thinking_level(&config), Some(ThinkingLevel::Low));
}

#[test]
fn effective_thinking_level_prefers_the_field_then_parses_extra_body() {
    let mut config = ModelConfig {
        endpoint: "https://api.deepseek.com".into(),
        ..ModelConfig::default()
    };
    // 无字段、无 extra_body：服务端默认按 high。
    assert_eq!(effective_thinking_level(&config), Some(ThinkingLevel::High));
    // 字段优先于 extra_body。
    config.thinking_level = Some(ThinkingLevel::Max);
    config.extra_body = json!({"reasoning_effort": "low"});
    assert_eq!(effective_thinking_level(&config), Some(ThinkingLevel::Max));
    // 无字段时解析线上值：medium/xhigh 官方归并到 high。
    config.thinking_level = None;
    for (effort, expected) in [
        ("low", ThinkingLevel::Low),
        ("high", ThinkingLevel::High),
        ("medium", ThinkingLevel::High),
        ("xhigh", ThinkingLevel::High),
        ("max", ThinkingLevel::Max),
    ] {
        config.extra_body = json!({"reasoning_effort": effort});
        assert_eq!(effective_thinking_level(&config), Some(expected));
    }
    // 手工编辑成 disabled：视为明确关闭，UI 不显示。
    config.extra_body = json!({"thinking": {"type": "disabled"}});
    assert_eq!(effective_thinking_level(&config), None);
    // 非 DeepSeek/GLM 端点一律无档位。
    config.endpoint = "https://api.openai.com/v1".into();
    config.thinking_level = Some(ThinkingLevel::Max);
    assert_eq!(effective_thinking_level(&config), None);
}

#[test]
fn thinking_level_serializes_snake_case_for_the_config_blob() {
    let config = ModelConfig {
        thinking_level: Some(ThinkingLevel::Max),
        ..ModelConfig::default()
    };
    let blob = serde_json::to_value(&config).expect("serialize");
    assert_eq!(blob["thinking_level"], "max");
    // 旧库无该字段：反序列化得到 None（serde default）。
    let legacy = serde_json::from_value::<ModelConfig>(json!({
        "protocol": "open_ai_compatible",
        "model": "m",
        "endpoint": "https://e.test",
        "request_path": "/chat/completions",
        "auth_header": "Authorization",
        "auth_prefix": "Bearer ",
        "extra_headers": {},
        "extra_body": {},
        "parallel_tool_calls": true
    }))
    .expect("deserialize legacy");
    assert_eq!(legacy.thinking_level, None);
}
