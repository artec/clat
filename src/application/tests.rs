use super::trusted::glm_mcp_pack_from_control;
use super::*;
use crate::RunEvent;
use crate::control_storage::ControlStorage;
use crate::event::EventSink;
use crate::model::ModelConfig;
use crate::permission::PermissionApprover;
use crate::presets::preset_by_id;
use crate::session::key::{ProjectKey, SessionKey};
#[cfg(unix)]
use crate::session::persistence::JsonlCompression;
use crate::test_support::{
    CountingApprover, LiveGlmProviderPlugin, SharedEvents, TestBehavior, TestModelScript,
    TestProviderPlugin, configure_test_model, configure_test_model_with_budget, roots,
};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 探查某项目路径的持久化当前会话指针（workspace.json 记录内
/// `activeSessionId`；None = Fresh/未注册）。
fn persisted_pointer(
    storage_root: &std::path::Path,
    project_root: &std::path::Path,
) -> Option<String> {
    let control = ControlStorage::open_ready(storage_root).expect("open ready");
    control.workspace_pointer(&crate::control_storage::sentinel::project_key(project_root))
}

#[test]
fn trusted_application_is_send() {
    // 回归锁（Windows CI v0.6.3 编译失败）：TUI 异步加载把整个挂载
    // 结果从加载线程搬进主线程，要求 TrustedProjectApplication:
    // Send。Unix 租约字段（Vec<File>）天然满足；Windows 的 HANDLE
    // 原始指针不是——root_lease 里以安全论证补了 unsafe impl Send。
    // 此断言在任何平台的编译期锁死该契约：pre-fix 的 Windows 构建
    // 在这里编译失败（即该 bug 的"先红"）。
    fn assert_send<T: Send>() {}
    assert_send::<TrustedProjectApplication>();
}

/// 不变量（2026-08-19 退出延迟）：`join_with_grace` 对卡住的线程
/// 在 `grace` 内返回 Ok（放弃而非挂起调用方），对正常退出的线程
/// 保持 join 语义（含 panic 映射）。pre-fix 的 shutdown 是无界
/// join——在途 HTTP 阶段不可中断时退出被拖到请求超时。
#[test]
fn join_with_grace_bounds_stuck_workers_and_joins_fast_ones() {
    // 快路径：立即退出的线程被正常 join。
    let fast = std::thread::spawn(|| ());
    join_with_grace(fast, Duration::from_millis(500), "fast").expect("fast join");

    // panic 路径：映射为错误字符串。
    let panicked = std::thread::spawn(|| panic!("boom"));
    assert!(join_with_grace(panicked, Duration::from_millis(500), "panic").is_err());

    // 卡住路径：10s 沉睡的线程在 200ms 宽限内被放弃，调用方不挂。
    let stuck = std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(10));
    });
    let started = std::time::Instant::now();
    join_with_grace(stuck, Duration::from_millis(200), "stuck").expect("abandon is Ok");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the caller must not wait for the stuck worker, took {:?}",
        started.elapsed()
    );
}

fn allow_all_approver() -> Arc<dyn PermissionApprover> {
    Arc::new(allow_all)
}

/// 具名 allow-all 审批人（闭包对带引用参数的 trait blanket impl 有
/// HRTB 推断限制，A1 起 decide 携带 `&CancelToken`）。
fn allow_all(
    _request: crate::PermissionRequest,
    _cancel: &crate::model::CancelToken,
) -> crate::PermissionDecision {
    crate::PermissionDecision::Allow
}

fn mount(
    project: &Project,
    storage_root: &std::path::Path,
    behavior: TestBehavior,
) -> TrustedProjectApplication {
    let bootstrap =
        BootstrapApplication::open(project.clone(), storage_root.to_path_buf()).unwrap();
    bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        .unwrap()
}

fn run(
    application: &mut TrustedProjectApplication,
    prompt: &str,
) -> Result<ApplicationRunDone, ApplicationRunFailure> {
    run_with_attachments(application, prompt, Vec::new())
}

fn run_with_attachments(
    application: &mut TrustedProjectApplication,
    prompt: &str,
    attachments: Vec<std::path::PathBuf>,
) -> Result<ApplicationRunDone, ApplicationRunFailure> {
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::from_front_end(prompt, None, attachments),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    receiver.recv().unwrap()
}

/// FP-09（前置红，2026-08-22 审计）：frontend sink 在非终态事件上
/// panic——panic 穿过 recorder mutex 内的 `inner.emit` → 锁中毒 →
/// 收尾 `finish()` 被 `if let Ok` 静默跳过 → journal 缺 closing
/// turn/end（durable terminal closure 丢失、事件继续静默丢弃）。
/// 修复不变量：frontend 故障不得把核心 persistence 变成不可收尾
/// 状态——catch_unwind 收尾后 journal 仍有 turn/end（error reason），
/// run 返回显式失败。
#[test]
fn sink_panic_does_not_swallow_the_terminal_closure() {
    struct PanicOnDeltaSink;
    impl crate::EventSink for PanicOnDeltaSink {
        fn emit(&mut self, event: crate::RunEvent) {
            if matches!(
                event,
                crate::RunEvent::ModelStream {
                    event: crate::model::ModelEvent::TextDelta { .. },
                    ..
                }
            ) {
                panic!("frontend sink exploded (FP-09 fixture)");
            }
        }
    }

    let (storage_root, project_root) = roots("fp09-sink-panic");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("say something"),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(PanicOnDeltaSink),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let failure = receiver
        .recv()
        .unwrap()
        .expect_err("a panicking sink must fail the run explicitly");
    assert!(
        failure.error.contains("partial output"),
        "the run-worker panic path formats the failure (message + partial): {}",
        failure.error
    );
    assert!(
        failure.error.contains("frontend sink exploded"),
        "the sink panic payload surfaces verbatim: {}",
        failure.error
    );
    application.close().unwrap();

    let events = load_events(&storage_root);
    let types: Vec<&str> = events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect();
    assert!(
        types.contains(&"turn/end"),
        "the journal must still carry the closing turn/end: {types:?}"
    );
}

/// B9 验收③（根因杀测试）：双档案互切后两边 key/endpoint 均完好。
/// 切换 = 激活原语（load → save_model_state → set_active），档案行
/// 自身永不被切换改写——pre-fix 单槽世界里「切走即丢」的根因在此
/// 断言面上消灭（判别力：激活原语不再从档案行 load credentials /
/// 回写覆盖档案行 → 本测试红；pre-fix 无档案概念属编译级，文档化）。
#[test]
fn custom_profiles_keep_their_keys_across_switches() {
    use crate::model::{ModelConfig, ProviderCredentials};

    let (storage_root, project_root) = roots("b9-switch");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);

    let profile = |endpoint: &str, model: &str, key: &str| {
        let config = ModelConfig {
            preset: None,
            endpoint: endpoint.into(),
            model: model.into(),
            ..ModelConfig::default()
        };
        let mut credentials = ProviderCredentials::for_protocol(config.protocol);
        credentials.set_value(0, key.to_owned());
        (config, credentials)
    };
    let (config_a, credentials_a) =
        profile("https://api.example.com/v1", "model-a", "sk-profile-a");
    let (config_b, credentials_b) =
        profile("https://other.example.org/v1", "model-b", "sk-profile-b");
    application
        .save_model_profile("work", &config_a, &credentials_a)
        .unwrap();
    application
        .save_model_profile("personal", &config_b, &credentials_b)
        .unwrap();

    // A → B → A 往返：两边完好。
    application.activate_model_profile("work").unwrap().unwrap();
    application
        .activate_model_profile("personal")
        .unwrap()
        .unwrap();
    let (active_config, active_credentials) = application.model_state().unwrap();
    assert_eq!(active_config.endpoint, "https://other.example.org/v1");
    assert_eq!(active_credentials.value(0), Some("sk-profile-b"));
    assert_eq!(
        application.active_model_profile().unwrap().as_deref(),
        Some("personal")
    );

    application.activate_model_profile("work").unwrap().unwrap();
    let (active_config, active_credentials) = application.model_state().unwrap();
    assert_eq!(active_config.endpoint, "https://api.example.com/v1");
    assert_eq!(
        active_credentials.value(0),
        Some("sk-profile-a"),
        "switching back restores the profile's own key (root cause killed)"
    );

    // 档案行自身未被切换改写（INV-M3）。
    let (stored_a, stored_credentials_a) = application
        .load_model_profile("work")
        .unwrap()
        .expect("profile persists");
    assert_eq!(stored_a.endpoint, "https://api.example.com/v1");
    assert_eq!(stored_credentials_a.value(0), Some("sk-profile-a"));
    application.close().unwrap();
}

/// B9 验收④：预设→档案往返 key 不重填——档案 key 跟档案走
/// （INV-M2），预设 key 走厂商记忆（INV-VK1/VK2 原样保留：Other
/// 端点不入厂商库），两边各自完好、互不污染。
#[test]
fn preset_profile_roundtrip_never_refills_keys() {
    use crate::model::{ModelConfig, ProviderCredentials};
    use crate::presets::preset_by_id;

    let (storage_root, project_root) = roots("b9-roundtrip");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);

    // 档案：Other 端点 + 自有 key。
    let profile_config = ModelConfig {
        preset: None,
        endpoint: "https://api.example.com/v1".into(),
        model: "my-model".into(),
        ..ModelConfig::default()
    };
    let mut profile_credentials = ProviderCredentials::for_protocol(profile_config.protocol);
    profile_credentials.set_value(0, "sk-profile".to_owned());
    application
        .save_model_profile("work", &profile_config, &profile_credentials)
        .unwrap();

    // 预设：DeepSeek 端点 + key（写入厂商记忆库）。
    let preset = preset_by_id("deepseek-v4-flash").expect("preset");
    let mut preset_config = ModelConfig::default();
    preset.apply(&mut preset_config);
    let mut preset_credentials = ProviderCredentials::for_protocol(preset_config.protocol);
    preset_credentials.set_value(0, "sk-deepseek".to_owned());
    application
        .save_model_state(&preset_config, &preset_credentials)
        .unwrap();

    // 切到档案：活动 key = 档案 key；预设 key 留在厂商记忆。
    application.activate_model_profile("work").unwrap().unwrap();
    let (_, active) = application.model_state().unwrap();
    assert_eq!(active.value(0), Some("sk-profile"));
    let remembered = application
        .vendor_key(preset_config.protocol, &preset_config.endpoint)
        .expect("vendor memory holds the preset key");
    assert_eq!(remembered.value(0), Some("sk-deepseek"));

    // 切回预设（模拟 picker 的厂商记忆回填分支）：key 免重填。
    let restored = application
        .vendor_key(preset_config.protocol, &preset_config.endpoint)
        .unwrap();
    application
        .save_model_state(&preset_config, &restored)
        .unwrap();
    let (_, active) = application.model_state().unwrap();
    assert_eq!(active.value(0), Some("sk-deepseek"));

    // 再切回档案：档案 key 原样（INV-M2/M3）。
    application.activate_model_profile("work").unwrap().unwrap();
    let (_, active) = application.model_state().unwrap();
    assert_eq!(
        active.value(0),
        Some("sk-profile"),
        "the profile keeps its own key across the roundtrip"
    );
    // Other 端点从未进厂商库（INV-VK1）。
    assert!(
        application
            .vendor_key(profile_config.protocol, &profile_config.endpoint)
            .is_none()
    );
}

/// B9 验收⑤（应用腿）：删除活动档案 → 活动指针回退（首个可用档案）；
/// 删到最后一个 → 活动态重置为出厂默认（endpoint 空、无残留 key）。
#[test]
fn deleting_the_active_profile_falls_back_cleanly() {
    use crate::model::{ModelConfig, ProviderCredentials};

    let (storage_root, project_root) = roots("b9-delete");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);

    let profile = |endpoint: &str, key: &str| {
        let config = ModelConfig {
            preset: None,
            endpoint: endpoint.into(),
            model: "m".into(),
            ..ModelConfig::default()
        };
        let mut credentials = ProviderCredentials::for_protocol(config.protocol);
        credentials.set_value(0, key.to_owned());
        (config, credentials)
    };
    let (config_a, credentials_a) = profile("https://a.example.com/v1", "sk-a");
    let (config_b, credentials_b) = profile("https://b.example.com/v1", "sk-b");
    application
        .save_model_profile("first", &config_a, &credentials_a)
        .unwrap();
    application
        .save_model_profile("second", &config_b, &credentials_b)
        .unwrap();
    application
        .activate_model_profile("first")
        .unwrap()
        .unwrap();

    // 删活动档案 → 回退到首个可用（second）。
    application
        .delete_model_profile_with_fallback("first")
        .unwrap();
    assert_eq!(
        application.active_model_profile().unwrap().as_deref(),
        Some("second")
    );
    let (active_config, _) = application.model_state().unwrap();
    assert_eq!(active_config.endpoint, "https://b.example.com/v1");

    // 删最后一个 → 出厂默认（endpoint 空、无残留 key、指针空）。
    application
        .delete_model_profile_with_fallback("second")
        .unwrap();
    assert_eq!(application.active_model_profile().unwrap(), None);
    let (active_config, active_credentials) = application.model_state().unwrap();
    assert!(
        active_config.endpoint.trim().is_empty(),
        "factory default after the last profile is deleted"
    );
    assert!(
        active_credentials
            .value(0)
            .is_none_or(|value| value.trim().is_empty()),
        "no key residue from the deleted profile"
    );
    assert!(application.list_model_profiles().unwrap().is_empty());
    application.close().unwrap();
}

/// B9 验收⑥（前置红）：旧单槽自定义态（preset=None 且 endpoint 非空）
/// 在重挂载时自动转为第一个档案；预设态不迁移；幂等（二次挂载不重复
/// 建档）。pre-fix：`list_model_profiles` 恒空（本测试只用既有 API 构造
/// ——首个断言红）。
#[test]
fn legacy_single_slot_custom_state_migrates_to_first_profile() {
    use crate::model::{ModelConfig, ProviderCredentials};

    let (storage_root, project_root) = roots("b9-migrate");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);

    let application = mount(&project, &storage_root, TestBehavior::Success);
    // 旧世界唯一的自定义持久化形态：单槽 model_state 直写。
    let custom = ModelConfig {
        preset: None,
        endpoint: "https://api.example.com/v1".into(),
        model: "my-model".into(),
        ..ModelConfig::default()
    };
    let mut credentials = ProviderCredentials::for_protocol(custom.protocol);
    credentials.set_value(0, "sk-single-slot".to_owned());
    application.save_model_state(&custom, &credentials).unwrap();
    application.close().unwrap();

    // 重挂载：迁移腿把单槽态转为档案 #1。
    let application = mount(&project, &storage_root, TestBehavior::Success);
    let profiles = application.list_model_profiles().unwrap();
    assert!(
        profiles.iter().any(|profile| profile.name == "Custom"),
        "the legacy single-slot custom state becomes profile #1: {profiles:?}"
    );
    let (config, migrated) = application
        .load_model_profile("Custom")
        .unwrap()
        .expect("profile loads");
    assert_eq!(config.endpoint, "https://api.example.com/v1");
    assert_eq!(config.model, "my-model");
    assert_eq!(migrated.value(0), Some("sk-single-slot"));
    // F-B9-1：迁移建档即接管活动指针——迁移档案真在活动，Custom 列表
    // 的 ● 必须出现。pre-fix 红：迁移腿从不设指针。
    assert_eq!(
        application.active_model_profile().unwrap().as_deref(),
        Some("Custom"),
        "the migrated profile is the active one and carries the pointer"
    );

    // 幂等：再次重挂载不重复建档。
    application.close().unwrap();
    let application = mount(&project, &storage_root, TestBehavior::Success);
    let profiles = application.list_model_profiles().unwrap();
    assert_eq!(profiles.len(), 1, "migration is idempotent: {profiles:?}");
    application.close().unwrap();

    // 预设态不迁移（INV-M3 升级腿只吃自定义态）。
    let (storage_root, project_root) = roots("b9-migrate-preset");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);
    let preset = ModelConfig {
        preset: Some("deepseek-v4-flash".into()),
        ..ModelConfig::default()
    };
    application
        .save_model_state(&preset, &ProviderCredentials::for_protocol(preset.protocol))
        .unwrap();
    application.close().unwrap();
    let application = mount(&project, &storage_root, TestBehavior::Success);
    assert!(
        application.list_model_profiles().unwrap().is_empty(),
        "preset states never migrate into profiles"
    );
    application.close().unwrap();
}

/// F-B9-1（复核必修 2026-08-22）：legacy 路径（预设切换/经典编辑器
/// 保存）经 `save_model_state` 直写——收尾必须清 `active_profile` 指针
///（INV-M2 第四元素：指针随换装走）。两条前置红腿：①激活档案 work →
/// 切 DeepSeek 预设后指针仍 Some("work")（Custom 列表 ● 标错）；②删
/// 这个「陈旧指针」档案误触 was_active 回退，**活预设被静默换成出厂
/// 空态**（INV-M3 破坏——本测试第二断言红）。
#[test]
fn preset_switch_after_profile_activation_clears_the_active_pointer() {
    use crate::model::{ModelConfig, ProviderCredentials};

    let (storage_root, project_root) = roots("b9-pointer");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);

    // 建档并激活 work。
    let work = ModelConfig {
        preset: None,
        endpoint: "https://work.example.com/v1".into(),
        model: "m".into(),
        ..ModelConfig::default()
    };
    let credentials = ProviderCredentials::for_protocol(work.protocol);
    application
        .save_model_profile("work", &work, &credentials)
        .unwrap();
    application.activate_model_profile("work").unwrap().unwrap();
    assert_eq!(
        application.active_model_profile().unwrap().as_deref(),
        Some("work")
    );

    // 切预设（actions SelectPreset 同端点路径同形态：preset.apply +
    // save_model_state 直写）。
    let mut preset_config = ModelConfig::default();
    crate::presets::preset_by_id("deepseek-v4-flash")
        .expect("preset exists")
        .apply(&mut preset_config);
    application
        .save_model_state(&preset_config, &credentials)
        .unwrap();
    // 红腿①：指针随 legacy 换装清空。
    assert_eq!(
        application.active_model_profile().unwrap(),
        None,
        "a preset switch installs non-profile state; the pointer must clear"
    );

    // 红腿②：删除已非活动的 work 不得触碰活预设。
    application
        .delete_model_profile_with_fallback("work")
        .unwrap();
    let (active, _) = application.model_state().unwrap();
    assert_eq!(
        active.endpoint, preset_config.endpoint,
        "the live preset survives deleting the stale-pointer profile"
    );
    assert_eq!(active.preset.as_deref(), Some("deepseek-v4-flash"));
    assert!(
        application.list_model_profiles().unwrap().is_empty(),
        "the deleted profile is gone"
    );
    application.close().unwrap();
}

/// Load the durable events of the storage root's only session.
fn load_events(storage_root: &std::path::Path) -> Vec<crate::session::event::SessionEvent> {
    let backend = crate::session::persistence::JsonlBackend::new(
        storage_root.join("sessions"),
        crate::session::persistence::JsonlCompression::Zstd,
        false,
    );
    let headers = backend.list_headers().unwrap();
    let header = headers.first().expect("one session header");
    let key = SessionKey {
        project: ProjectKey::from_cwd(&header.cwd.clone().expect("header carries the project cwd")),
        id: header.id.clone(),
    };
    backend.load(&key, false).unwrap().events
}

/// INV-MM2-6 测试辅助：按 attachmentId 在 storage root 的会话附件域
/// 里定位内容寻址 blob（store 布局
/// `sessions/<project>/<session>/attachments/blobs/<id>`，递归查找）。
fn find_blob(storage_root: &std::path::Path, attachment_id: &str) -> std::path::PathBuf {
    fn walk(dir: &std::path::Path, attachment_id: &str) -> Option<std::path::PathBuf> {
        for entry in std::fs::read_dir(dir).ok()? {
            let path = entry.ok()?.path();
            if path.is_dir() {
                if path.file_name()?.to_str()? == "blobs" {
                    let candidate = path.join(attachment_id);
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                } else if let Some(found) = walk(&path, attachment_id) {
                    return Some(found);
                }
            }
        }
        None
    }
    walk(&storage_root.join("sessions"), attachment_id)
        .unwrap_or_else(|| panic!("blob {attachment_id} not found under the storage root"))
}

/// Load the durable events of one specific session by id.
fn load_events_for(
    storage_root: &std::path::Path,
    id: &crate::session::id::SessionId,
) -> Vec<crate::session::event::SessionEvent> {
    let backend = crate::session::persistence::JsonlBackend::new(
        storage_root.join("sessions"),
        crate::session::persistence::JsonlCompression::Zstd,
        false,
    );
    let headers = backend.list_headers().unwrap();
    let header = headers
        .iter()
        .find(|header| &header.id == id)
        .expect("session header");
    let key = SessionKey {
        project: ProjectKey::from_cwd(&header.cwd.clone().expect("header carries the project cwd")),
        id: header.id.clone(),
    };
    backend.load(&key, false).unwrap().events
}

#[test]
fn authorize_and_mount_initializes_fresh_storage_and_rejects_old_state() {
    let (storage_root, project_root) = roots("cutover-init");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);

    // Fresh → authorize → trust 行 + 哨兵（MP-1：Fresh 只写 config.json，
    // 其余文件惰性诞生）。
    {
        let application = mount(&project, &storage_root, TestBehavior::Success);
        assert!(storage_root.join("config.json").exists());
        assert!(storage_root.join("trust.json").exists());
        application.close().unwrap();
    }
    // Reopen without authorization: already trusted.
    {
        let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
        assert!(bootstrap.is_trusted().unwrap());
        bootstrap.into_trusted().unwrap().close().unwrap();
    }
    // Old pre-release config is rejected with zero writes.
    let (old_root, old_project_root) = roots("cutover-old");
    std::fs::create_dir_all(&old_project_root).unwrap();
    std::fs::create_dir_all(&old_root).unwrap();
    std::fs::write(
        old_root.join("config.json"),
        serde_json::json!({"version": 3, "database": "clat.db"}).to_string(),
    )
    .unwrap();
    let before = std::fs::read_to_string(old_root.join("config.json")).unwrap();
    let error = BootstrapApplication::open(Project::new(&old_project_root), old_root.clone())
        .err()
        .expect("old config must be rejected");
    assert!(
        error.to_string().contains("unsupported or unreadable"),
        "{error}"
    );
    let after = std::fs::read_to_string(old_root.join("config.json")).unwrap();
    assert_eq!(before, after, "rejection must not touch the old state");

    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    std::fs::remove_dir_all(old_root.parent().unwrap()).ok();
}

#[test]
fn dual_stream_run_produces_the_dsh_event_family() {
    let (storage_root, project_root) = roots("cutover-dual-stream");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::WriteFile);
    configure_test_model(&application);

    let done = run(&mut application, "please write the file").expect("write-file run completes");
    assert_eq!(done.output, "write attempted");
    application.close().unwrap();

    let events = load_events(&storage_root);
    let types: Vec<&str> = events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect();
    // approval barrier (asked → decided+call atomic) precedes invoke.
    let expected = [
        "turn/start",
        "user/message",
        "step/start",
        "request/header",
        "assistant/message",
        "approval/asked",
        "approval/decided",
        "tool/call",
        "tool/result",
        "step/end",
        "step/start",
        "assistant/chunk",
        "assistant/message",
        "step/end",
        "turn/end",
    ];
    assert_eq!(types, expected, "the durable event family is exact");
    // Surface semantics: user/message and assistant/message and
    // tool/result carry surfaceOp append.
    for event in &events {
        if matches!(
            event.event_type.as_str(),
            "user/message" | "assistant/message" | "tool/result"
        ) {
            assert!(
                event.surface_op.is_some(),
                "{} must be surface",
                event.event_type
            );
        } else if matches!(event.event_type.as_str(), "step/start" | "turn/start") {
            assert!(
                event.surface_op.is_none(),
                "{} must be log-only",
                event.event_type
            );
        }
    }
    // turn/end reason is completed.
    let turn_end = events.last().unwrap();
    assert_eq!(turn_end.data["reason"]["kind"], "completed");
    // seq contiguity from 0.
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.seq, index as u64);
    }
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// I1 同形性对拍的规范形：live `RunEvent` 流与 journal 回放各自投影
/// 到同一组"前端可见事实"后必须相等。时间戳/turn 编号（两套协议的
/// 计数基准不同：live turn = 模型轮，journal turn = 用户轮/step）与
/// 已文档化的写侧不可恢复项不参与比较。
#[derive(Clone, Debug, PartialEq)]
enum Canon {
    User(String),
    Assistant {
        reasoning: Option<String>,
        text: String,
        tool_calls: Vec<crate::ToolCall>,
        provider: String,
        model: String,
    },
    ToolCall(crate::ToolCall),
    ToolDone {
        call_id: String,
        tool: String,
        output_text: String,
        is_error: bool,
        /// A permission denial (no executed call behind it): the two
        /// protocols carry different non-comparable text (live: the
        /// approver reason; journal: the fixed policy message), so only
        /// (call_id, tool, is_error) compare.
        denied: bool,
    },
    Permission(String, &'static str),
    TurnEnd(&'static str),
}

fn decision_discriminant(decision: &crate::PermissionDecision) -> &'static str {
    match decision {
        crate::PermissionDecision::Allow => "allow",
        crate::PermissionDecision::Ask { .. } => "ask",
        crate::PermissionDecision::Deny { .. } => "deny",
        crate::PermissionDecision::Unavailable { .. } => "unavailable",
    }
}

fn output_text(output: &Value) -> String {
    match output {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn canon_live(events: &[crate::RunEvent]) -> Vec<Canon> {
    use crate::ModelEvent;
    use crate::RunEvent;
    let mut out = Vec::new();
    let mut reasoning = String::new();
    let mut text = String::new();
    let mut tool_calls: Vec<crate::ToolCall> = Vec::new();
    let mut provider = String::new();
    let mut model = String::new();
    // The deny path emits no ToolFinished; the journal records an
    // isError tool/result instead. Pair it with the last requested call
    // so both protocols reduce to the same fact.
    let mut last_call_id = String::new();
    for event in events {
        match event {
            RunEvent::RunStarted { message, .. } => out.push(Canon::User(message.plain_text())),
            RunEvent::ModelRequested {
                provider: p,
                model: m,
                ..
            } => {
                provider = p.clone();
                model = m.clone();
            }
            RunEvent::ModelStream { event, .. } => match event {
                ModelEvent::TextDelta { delta } | ModelEvent::RefusalDelta { delta } => {
                    text.push_str(delta);
                }
                ModelEvent::ReasoningDelta { delta }
                | ModelEvent::ReasoningSummaryDelta { delta } => reasoning.push_str(delta),
                ModelEvent::ToolCallCompleted { call } => tool_calls.push(call.clone()),
                _ => {}
            },
            RunEvent::ModelResponded { .. } => {
                if !text.is_empty() || !reasoning.is_empty() || !tool_calls.is_empty() {
                    out.push(Canon::Assistant {
                        reasoning: (!reasoning.is_empty()).then(|| std::mem::take(&mut reasoning)),
                        text: std::mem::take(&mut text),
                        tool_calls: std::mem::take(&mut tool_calls),
                        provider: provider.clone(),
                        model: model.clone(),
                    });
                }
            }
            RunEvent::ToolRequested { call } => {
                last_call_id = call.id.clone();
                out.push(Canon::ToolCall(call.clone()));
            }
            RunEvent::PermissionChecked { tool, decision } => {
                // Policy-direct Allow leaves no journal trace (DSH
                // semantics); the replay side only produces approval
                // round trips. Compared as multisets in the test body.
                // The approver's deny/unavailable reason is physically
                // absent from the journal (decided carries only the
                // outcome — pinned DSH payload), so parity compares the
                // decision discriminant; replay offers the asked reason.
                out.push(Canon::Permission(
                    tool.clone(),
                    decision_discriminant(decision),
                ));
            }
            RunEvent::PermissionDenied { tool, .. } => {
                // The journal never records a denied call's arguments;
                // parity for it is (id, name) only. The Permission item
                // for this denial already sits in between, so search
                // backwards for the call instead of taking the tail.
                if let Some(Canon::ToolCall(call)) =
                    out.iter_mut().rev().find_map(|item| match item {
                        Canon::ToolCall(call) if call.id == last_call_id => Some(item),
                        _ => None,
                    })
                {
                    call.arguments = Value::Null;
                }
                out.push(Canon::ToolDone {
                    call_id: last_call_id.clone(),
                    tool: tool.clone(),
                    output_text: String::new(),
                    is_error: true,
                    denied: true,
                });
            }
            RunEvent::ToolStarted { .. } => {}
            RunEvent::SteeringApplied { message, .. } => {
                out.push(Canon::User(message.plain_text()))
            }
            RunEvent::ToolFinished { result } => out.push(Canon::ToolDone {
                call_id: result.call_id.clone(),
                tool: result.tool_name.clone(),
                output_text: output_text(&result.output),
                is_error: result.is_error,
                denied: false,
            }),
            RunEvent::RunCompleted { .. } => out.push(Canon::TurnEnd("completed")),
            RunEvent::RunCancelled { .. } => out.push(Canon::TurnEnd("aborted:user")),
            RunEvent::RunFailed { .. } => {
                // The recorder appends a settled assistant item for
                // partial stream output before the failure, mirroring it.
                if !text.is_empty() || !reasoning.is_empty() || !tool_calls.is_empty() {
                    out.push(Canon::Assistant {
                        reasoning: (!reasoning.is_empty()).then(|| std::mem::take(&mut reasoning)),
                        text: std::mem::take(&mut text),
                        tool_calls: std::mem::take(&mut tool_calls),
                        provider: provider.clone(),
                        model: model.clone(),
                    });
                }
                out.push(Canon::TurnEnd("error"));
            }
        }
    }
    out
}

fn canon_replay(items: &[crate::session::replay::ReplayEvent]) -> Vec<Canon> {
    use crate::session::replay::{ReplayEvent, ReplayTurnEnd};
    use std::collections::HashSet;
    // A denial shows up in the journal as PermissionChecked(deny) right
    // after the (synthesized, argument-less) call header, or as an
    // orphan isError result (policy deny). Executed tools always carry
    // their real tool/call first, so they never classify as denied.
    let requested: HashSet<&str> = items
        .iter()
        .filter_map(|item| match item {
            ReplayEvent::ToolRequested { call, .. } => Some(call.id.as_str()),
            _ => None,
        })
        .collect();
    let mut denied_calls: HashSet<String> = HashSet::new();
    let mut last_call_id = String::new();
    for item in items {
        match item {
            ReplayEvent::ToolRequested { call, .. } => last_call_id = call.id.clone(),
            ReplayEvent::PermissionChecked { decision, .. } => {
                if matches!(
                    decision,
                    crate::PermissionDecision::Deny { .. }
                        | crate::PermissionDecision::Unavailable { .. }
                ) {
                    denied_calls.insert(last_call_id.clone());
                }
            }
            _ => {}
        }
    }
    items
        .iter()
        .filter_map(|item| match item {
            ReplayEvent::UserMessage { text, .. } => Some(Canon::User(text.clone())),
            ReplayEvent::AssistantMessage {
                reasoning,
                text,
                tool_calls,
                provider,
                model,
                ..
            } => (!text.is_empty() || reasoning.is_some() || !tool_calls.is_empty()).then_some(
                Canon::Assistant {
                    reasoning: reasoning.clone(),
                    text: text.clone(),
                    tool_calls: tool_calls.clone(),
                    provider: provider.clone(),
                    model: model.clone(),
                },
            ),
            ReplayEvent::PermissionChecked { tool, decision, .. } => Some(Canon::Permission(
                tool.clone(),
                decision_discriminant(decision),
            )),
            ReplayEvent::ToolRequested { call, .. } => Some(Canon::ToolCall(call.clone())),
            ReplayEvent::ToolFinished {
                call_id,
                tool,
                output,
                is_error,
                ..
            } => {
                let denied = *is_error
                    && (denied_calls.contains(call_id) || !requested.contains(call_id.as_str()));
                Some(Canon::ToolDone {
                    call_id: call_id.clone(),
                    tool: tool.clone(),
                    // Denial texts are protocol presentation, not facts.
                    output_text: if denied {
                        String::new()
                    } else {
                        output_text(output)
                    },
                    is_error: *is_error,
                    denied,
                })
            }
            ReplayEvent::TurnEnded { reason, .. } => Some(match reason {
                ReplayTurnEnd::Completed => Canon::TurnEnd("completed"),
                ReplayTurnEnd::Aborted { cause } if cause == "user" => {
                    Canon::TurnEnd("aborted:user")
                }
                ReplayTurnEnd::Aborted { cause } => {
                    Canon::TurnEnd(Box::leak(format!("aborted:{cause}").into_boxed_str()))
                }
                ReplayTurnEnd::Error { .. } => Canon::TurnEnd("error"),
                ReplayTurnEnd::Blocked => Canon::TurnEnd("blocked"),
                ReplayTurnEnd::MaxTokens => Canon::TurnEnd("max-tokens"),
                ReplayTurnEnd::Interrupted => Canon::TurnEnd("interrupted"),
            }),
            ReplayEvent::RetryScheduled { .. } | ReplayEvent::Compaction { .. } => None,
        })
        .collect()
}

fn assert_replay_parity(behavior: TestBehavior, prompt: &str) {
    assert_replay_parity_with_approver(behavior, prompt, Arc::new(allow_all));
}

/// 对拍断言（共享）：权限事实按多重集比较——replay 侧必须全部在
/// live 侧出现；live 侧富余只允许是政策直放行的 allow（Pure/Read
/// 自动放行在 journal 无痕，DSH 语义，ask_user 首次触发该路径）。
/// 会话事实严格保序相等。
fn assert_conversation_parity(
    live_events: &[crate::RunEvent],
    events: &[crate::session::event::SessionEvent],
) {
    let replay = crate::session::replay::ReplayAdapter::fold(events);
    let mut from_live = canon_live(live_events);
    let mut from_replay = canon_replay(&replay);
    // The durable approval barrier orders asked→decided→tool/call while
    // Run emits ToolRequested before the permission check, so permission
    // items compare as multisets, not positions.
    fn permissions(items: &mut Vec<Canon>) -> Vec<Canon> {
        let mut perms = Vec::new();
        let mut rest = Vec::new();
        for item in items.drain(..) {
            match item {
                Canon::Permission(..) => perms.push(item),
                other => rest.push(other),
            }
        }
        *items = rest;
        perms
    }
    let mut live_perms = permissions(&mut from_live);
    let mut replay_perms = permissions(&mut from_replay);
    replay_perms.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    for perm in replay_perms {
        match live_perms.iter().position(|candidate| *candidate == perm) {
            Some(index) => {
                live_perms.remove(index);
            }
            None => panic!("replay permission fact missing from live: {perm:?}"),
        }
    }
    for surplus in &live_perms {
        assert!(
            matches!(surplus, Canon::Permission(_, "allow")),
            "live-only permission facts must be policy-direct allows: {surplus:?}"
        );
    }
    assert_eq!(from_live, from_replay, "conversation facts (strict order)");
}

fn assert_replay_parity_with_approver(
    behavior: TestBehavior,
    prompt: &str,
    approver: Arc<dyn PermissionApprover>,
) {
    let (storage_root, project_root) = roots("replay-parity");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, behavior);
    configure_test_model(&application);

    let live = std::sync::Arc::new(Mutex::new(Vec::new()));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text(prompt),
            asker: None,
            approver,
            events: Box::new(SharedEvents(std::sync::Arc::clone(&live))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let _ = receiver.recv().unwrap();
    application.close().unwrap();

    let live_events = live.lock().unwrap().clone();
    assert_conversation_parity(&live_events, &load_events(&storage_root));
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// I1：完整工具往返（审批→调用→结果→第二轮回答）的 live↔回放对拍。
#[test]
fn replay_matches_the_live_stream_for_a_tool_run() {
    assert_replay_parity(TestBehavior::WriteFile, "please write the file");
}

/// I1：模型中途失败（partial 文本补落盘 + error 终态）同样对拍。
#[test]
fn replay_matches_the_live_stream_for_a_failed_run() {
    assert_replay_parity(TestBehavior::Failure, "this will fail");
}

/// 对抗审计 F3：审批**拒绝**路径的 live↔回放对拍。journal 侧该路径
/// 没有 tool/call（decided+isError tool/result 原子批），工具名只能
/// 从 approval/asked.callId 恢复——此前 T1 只测了 allow 路径，恰好
/// 漏掉这条分歧最大的通路。
#[test]
fn replay_matches_the_live_stream_for_a_denied_tool_run() {
    assert_replay_parity_with_approver(
        TestBehavior::WriteFile,
        "please write the file",
        Arc::new(
            |_request: crate::PermissionRequest, _cancel: &crate::model::CancelToken| {
                crate::PermissionDecision::Deny {
                    reason: "not allowed".into(),
                }
            },
        ),
    );
}

/// 权限三档挂载（TUI 路径）：`with_permission_modes` 后策略读共享
/// cell。与 exec 用的 `mount`（Classic）相对。`mode` 在挂载后显式
/// 设置——此时通常无活跃会话（PS7：只改 cell，物化时落为出生档）；
/// 活跃会话存在时则向其 journal 追加切换事件。
fn mount_with_permission_modes(
    project: &Project,
    storage_root: &std::path::Path,
    behavior: TestBehavior,
    mode: crate::permission::PermissionMode,
) -> TrustedProjectApplication {
    let application = mount_modes_from_storage(project, storage_root, behavior);
    application.set_permission_mode(mode).expect("set mode");
    application
}

/// 同上但不显式设置档位——模拟新进程启动：cell 从 workspace 自动
/// 恢复的会话自己的 fold 初始化（无活跃会话/遗留会话 → 默认档）。
fn mount_modes_from_storage(
    project: &Project,
    storage_root: &std::path::Path,
    behavior: TestBehavior,
) -> TrustedProjectApplication {
    let bootstrap =
        BootstrapApplication::open(project.clone(), storage_root.to_path_buf()).unwrap();
    bootstrap
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        .unwrap()
}

fn run_with_approver(
    application: &mut TrustedProjectApplication,
    prompt: &str,
    approver: Arc<dyn PermissionApprover>,
) -> Result<ApplicationRunDone, ApplicationRunFailure> {
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text(prompt),
            asker: None,
            approver,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    receiver.recv().unwrap()
}

/// 不变量 P2/P3：默认档 Project Write；`set_permission_mode` 的切换
/// 对下一次 run 的权限检查即时生效——Write 工具在 PW/FA 下零询问
/// 自动放行，在 RO 下回到逐次询问。pre-fix（无档位系统）上
/// approver 在三档下都会被询问，PW/FA 断言必红。
#[test]
fn permission_modes_gate_write_tools_by_mode() {
    use crate::permission::PermissionMode;
    let (storage_root, project_root) = roots("permission-modes");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = {
        let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::WriteFile,
            }))
            .unwrap();
        assert_eq!(
            application.permission_mode(),
            PermissionMode::ProjectWrite,
            "the mode system boots at the default mode"
        );
        application
    };
    configure_test_model(&application);

    // Project Write：文件写自动放行，approver 零调用，工具照常执行。
    let project_write_counter = Arc::new(AtomicUsize::new(0));
    let done = run_with_approver(
        &mut application,
        "please write the file",
        Arc::new(CountingApprover(Arc::clone(&project_write_counter))),
    )
    .expect("project-write run completes");
    assert_eq!(done.output, "write attempted");
    assert_eq!(
        project_write_counter.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "Project Write auto-allows file writes"
    );

    // ReadOnly：同一会话同一工具回到询问。
    application
        .set_permission_mode(PermissionMode::ReadOnly)
        .expect("persist mode");
    let read_only_counter = Arc::new(AtomicUsize::new(0));
    let done = run_with_approver(
        &mut application,
        "write it again",
        Arc::new(CountingApprover(Arc::clone(&read_only_counter))),
    )
    .expect("read-only run completes");
    assert_eq!(done.output, "write attempted");
    assert_eq!(
        read_only_counter.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "Read Only asks before every file write"
    );

    // FullAccess：零询问。
    application
        .set_permission_mode(PermissionMode::FullAccess)
        .expect("persist mode");
    let full_counter = Arc::new(AtomicUsize::new(0));
    let done = run_with_approver(
        &mut application,
        "and again",
        Arc::new(CountingApprover(Arc::clone(&full_counter))),
    )
    .expect("full-access run completes");
    assert_eq!(done.output, "write attempted");
    assert_eq!(
        full_counter.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "Full Access never asks"
    );

    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 不变量 PS1（会话独立，2026-08-19 用户报告的泄漏 bug）：档位是
/// 会话属性，绝不跨会话携带。会话 A 设 Full Access 后：(a) /new
/// 回到默认档；(b) 重启（workspace 自动恢复 A）仍恢复 Full Access；
/// (c) resume 到档位系统之前创建的遗留会话 B → 默认档（PS3）。
/// pre-fix（全局 cell 无 reseed）上 (a)/(c) 断言必红。
#[test]
fn permission_mode_travels_with_the_session_not_the_process() {
    use crate::permission::PermissionMode;
    let (storage_root, project_root) = roots("perm-session");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);

    // 遗留会话 B：Classic 挂载（exec 路径）创建——journal 无任何
    // `sandbox/mode` 事件（PS4 的写侧）。
    let legacy_id = {
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);
        run(&mut application, "legacy session").expect("legacy run");
        let id = application.snapshot().unwrap().session_id.expect("session");
        application.close().unwrap();
        id
    };

    // 会话 A：档位系统挂载，出生 FA（物化前设置）。
    let full_access_id = {
        let mut application =
            mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);
        // 当前活跃会话是遗留的 B（workspace 恢复）——先 /new 再设档。
        application.new_session().unwrap();
        assert_eq!(
            application.permission_mode(),
            PermissionMode::ProjectWrite,
            "/new resets to the default (in-process leak variant)"
        );
        application
            .set_permission_mode(PermissionMode::FullAccess)
            .expect("set full access");
        run(&mut application, "full access session").expect("run");
        let id = application.snapshot().unwrap().session_id.expect("session");
        // A 活跃且为 FA 时 /new：档位不跨会话携带（判别性场景——
        // 没有 reset 代码时这里读到 FA，必红）。
        application.new_session().unwrap();
        assert_eq!(
            application.permission_mode(),
            PermissionMode::ProjectWrite,
            "/new while Full Access is active restarts at the default"
        );
        // 回到 A，workspace 指针钉住它（供重启场景恢复 FA）。
        application.switch_session(id.clone()).unwrap();
        assert_eq!(
            application.permission_mode(),
            PermissionMode::FullAccess,
            "switching back to A restores its own mode before close"
        );
        application.close().unwrap();
        id
    };

    // 重启：workspace 自动恢复 A → 档位随日志回来（替代旧的项目级
    // 持久化诉求）。
    {
        let mut application =
            mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
        assert_eq!(
            application.permission_mode(),
            PermissionMode::FullAccess,
            "restarting resumes the same session and its own mode"
        );
        // 用户报告的确切序列：resume 到另一个会话 B。
        application.switch_session(legacy_id.clone()).unwrap();
        assert_eq!(
            application.permission_mode(),
            PermissionMode::ProjectWrite,
            "a legacy session (no mode events) falls back to the default"
        );
        // 再切回 A：档位跟着各自的日志走。
        application.switch_session(full_access_id.clone()).unwrap();
        assert_eq!(
            application.permission_mode(),
            PermissionMode::FullAccess,
            "switching back restores A's own mode"
        );
        application.close().unwrap();
    }

    // journal 侧：A 有出生事件，B 一个都没有（PS4）。
    let a_events = load_events_for(&storage_root, &full_access_id);
    assert_eq!(a_events[0].event_type, "sandbox/mode");
    assert_eq!(
        a_events[0].data.get("mode").and_then(|v| v.as_str()),
        Some("danger-full-access")
    );
    let b_events = load_events_for(&storage_root, &legacy_id);
    assert!(
        !b_events
            .iter()
            .any(|event| event.event_type == "sandbox/mode"),
        "classic (exec-style) sessions never journal mode events"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 不变量 PS2（journal 形状）：出生档是会话首条事件（先于
/// turn/start，同批原子落盘）；会话中切换追加事件（DSH 词汇）；
/// 同值重复切换零事件。
#[test]
fn permission_mode_birth_and_switch_journal_shape() {
    use crate::permission::PermissionMode;
    let (storage_root, project_root) = roots("perm-journal");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    // 物化前设 RO：成为出生档。
    application
        .set_permission_mode(PermissionMode::ReadOnly)
        .expect("set read only");
    run(&mut application, "first").expect("run");
    let events = load_events(&storage_root);
    assert_eq!(events[0].event_type, "sandbox/mode");
    assert_eq!(
        events[0].data.get("mode").and_then(|value| value.as_str()),
        Some("read-only"),
        "journal values use the DSH vocabulary"
    );
    assert_eq!(
        events[1].event_type, "turn/start",
        "the birth mode precedes the first turn"
    );

    // 会话中切换 FA：追加一条；同值再切：零事件。
    application
        .set_permission_mode(PermissionMode::FullAccess)
        .expect("switch to full access");
    application
        .set_permission_mode(PermissionMode::FullAccess)
        .expect("same-value switch is a no-op");
    let events = load_events(&storage_root);
    let mode_events: Vec<_> = events
        .iter()
        .filter(|event| event.event_type == "sandbox/mode")
        .collect();
    assert_eq!(mode_events.len(), 2, "birth + one switch, nothing more");
    assert_eq!(
        mode_events[1]
            .data
            .get("mode")
            .and_then(|value| value.as_str()),
        Some("danger-full-access")
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 不变量 PS7（无会话切换）：会话物化前 `/perm` 只改内存 cell——
/// 零 journal 写、零会话目录；该值随后成为出生档。
#[test]
fn sessionless_mode_switch_journals_nothing() {
    use crate::permission::PermissionMode;
    let (storage_root, project_root) = roots("perm-sessionless");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    application
        .set_permission_mode(PermissionMode::FullAccess)
        .expect("set full access");
    assert!(
        application.list_sessions().unwrap().is_empty(),
        "a sessionless switch writes nothing durable"
    );
    run(&mut application, "materialize").expect("run");
    let events = load_events(&storage_root);
    assert_eq!(events[0].event_type, "sandbox/mode");
    assert_eq!(
        events[0].data.get("mode").and_then(|value| value.as_str()),
        Some("danger-full-access"),
        "the pre-materialization choice becomes the birth mode"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 不变量 PS6（文件退役）：v0.7.0 的项目级 `permission_modes.json`
/// 已无人读取——遗留该文件不影响重新 mount。pre-fix 上 mount 从文件
/// 载入 FullAccess，断言必红。
#[test]
fn stale_permission_modes_file_is_ignored() {
    use crate::permission::PermissionMode;
    let (storage_root, project_root) = roots("perm-stale-file");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);

    // 先挂载一次建立 Ready 存储根，再落下 v0.7.0 的遗留文件
    //（classify 只看 config.json + clat.db，容忍额外根文件）。
    {
        let application = mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
        application.close().unwrap();
    }
    std::fs::write(
        storage_root.join("permission_modes.json"),
        format!(
            "{{\"version\":1,\"modes\":{{\"{}\":\"full-access\"}}}}",
            crate::control_storage::sentinel::project_key(&project_root),
        ),
    )
    .unwrap();

    let application = mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
    assert_eq!(
        application.permission_mode(),
        PermissionMode::ProjectWrite,
        "the retired project-level file no longer feeds the mode cell"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 不变量 PS5（回放对拍）：出生事件 + 会话中切换都进 journal，
/// live 流与回放的对拍不受影响——档位事件不产生会话事实，且
/// ReplayAdapter 的 fold 容忍它们。
#[test]
fn mode_switches_replay_identically() {
    use crate::permission::PermissionMode;
    let (storage_root, project_root) = roots("perm-parity");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application =
        mount_modes_from_storage(&project, &storage_root, TestBehavior::WriteFile);
    configure_test_model(&application);
    application
        .set_permission_mode(PermissionMode::ReadOnly)
        .expect("birth mode read-only");

    let live = Arc::new(Mutex::new(Vec::new()));
    let run_with_events = |application: &mut TrustedProjectApplication,
                           live: Arc<Mutex<Vec<crate::RunEvent>>>,
                           prompt: &str,
                           approver: Arc<dyn PermissionApprover>|
     -> Result<ApplicationRunDone, ApplicationRunFailure> {
        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                message: crate::message::PendingMessage::text(prompt),
                asker: None,
                approver,
                events: Box::new(SharedEvents(live)),
                completion,
            })
            .unwrap();
        handle.join().unwrap();
        receiver.recv().unwrap()
    };

    // Run 1（RO）：询问一次后放行。
    let asked = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&asked);
    run_with_events(
        &mut application,
        Arc::clone(&live),
        "please write the file",
        Arc::new(
            move |_request: crate::PermissionRequest, _cancel: &crate::model::CancelToken| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                crate::PermissionDecision::Allow
            },
        ),
    )
    .expect("read-only run");
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);

    // 会话中切换 FA（journal 一条切换事件），Run 2 零询问。
    application
        .set_permission_mode(PermissionMode::FullAccess)
        .expect("mid-session switch");
    let asked_again = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&asked_again);
    run_with_events(
        &mut application,
        Arc::clone(&live),
        "write it again",
        Arc::new(
            move |_request: crate::PermissionRequest, _cancel: &crate::model::CancelToken| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                crate::PermissionDecision::Allow
            },
        ),
    )
    .expect("full-access run");
    assert_eq!(asked_again.load(std::sync::atomic::Ordering::SeqCst), 0);

    application.close().unwrap();
    let live_events = live.lock().unwrap().clone();
    assert_conversation_parity(&live_events, &load_events(&storage_root));
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 不变量 P6：档位驱动的决策（RO 询问路径）live 流与 journal 回放
/// 对拍相等——档位只改变决策来源，不改 journal 形状。
#[test]
fn mode_driven_decisions_replay_identically() {
    use crate::permission::PermissionMode;
    let (storage_root, project_root) = roots("mode-parity");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount_with_permission_modes(
        &project,
        &storage_root,
        TestBehavior::WriteFile,
        PermissionMode::ReadOnly,
    );
    configure_test_model(&application);

    let live = Arc::new(Mutex::new(Vec::new()));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("please write the file"),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(std::sync::Arc::clone(&live))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let _ = receiver.recv().unwrap();
    application.close().unwrap();

    let live_events = live.lock().unwrap().clone();
    assert_conversation_parity(&live_events, &load_events(&storage_root));
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// N2/N3/N4/N6（/rename 门面 + 标题管线）：
/// - 拒绝路径（NoSession / 清洗空 Invalid）零 journal 写入；
/// - **门槛已放宽（2026-08-19）**：改名不再要求 LLM 已起名——run
///   建会话后立刻可改（首轮自动命名失败/旧会话自愈路径），CAS
///   保证改名压制迟到的自动命名；
/// - 改名以 `Force + User` 落 journal（source.kind=user，N3）并广播
///   `TitleUpdated`（N2）；
/// - resume 快照带回存储标题（N6）；
/// - N5 的 CAS 机制由 use_cases `title_cas_rejects_stale_and_
///   accepts_force` 锁定：迟到的自动命名对 NoTitle/Exact 必败。
///
/// 自动命名与本次改名的先后存在竞争（title worker 异步）：无论谁
/// 先落盘，journal 的**最后一条** session/title 必须是用户标题。
#[test]
fn rename_facade_gates_journals_and_broadcasts() {
    let (storage_root, project_root) = roots("rename-facade");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    let (event_tx, event_rx) = mpsc::channel();
    application.subscribe(event_tx);

    // fresh 状态无活动会话：NoSession，且不触及清洗。
    assert!(matches!(
        application.rename_session("whatever").unwrap(),
        RenameOutcome::NoSession
    ));

    // run 建会话。不等自动命名（title worker 异步、与本测试存在
    // 竞争）——放宽后的门槛下，无显式标题也必须能立刻改名；若自动
    // 命名恰好先落盘，Force 语义照样覆盖它。
    let done = run(&mut application, "please fix the login bug").expect("run");
    assert_eq!(done.output, "done");
    assert_eq!(
        application
            .rename_session("  Renamed\tby hand\nsecond line ")
            .unwrap(),
        RenameOutcome::Renamed {
            title: "Renamed by hand".into()
        },
        "rename works before any automatic title lands (self-heal path)"
    );
    // 广播必然携带用户标题；先到的自动命名广播（"done"，若有）是
    // 噪音，跳过。
    let next_user_title_event = |receiver: &mpsc::Receiver<ApplicationEvent>| {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(ApplicationEvent::TitleUpdated { title }) if title == "Renamed by hand" => {
                    return ApplicationEvent::TitleUpdated { title };
                }
                Ok(
                    ApplicationEvent::MonitorUpdated(_)
                    | ApplicationEvent::CompactionUpdated(_)
                    | ApplicationEvent::TitleUpdated { .. }
                    | ApplicationEvent::McpStartupNotice { .. }
                    | ApplicationEvent::LanguageIntelligenceNotice { .. }
                    | ApplicationEvent::ProcessFinished { .. },
                ) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("application event channel closed")
                }
            }
        }
        panic!("no TitleUpdated for the rename within 5s");
    };
    assert_eq!(
        next_user_title_event(&event_rx),
        ApplicationEvent::TitleUpdated {
            title: "Renamed by hand".into()
        }
    );

    // 清洗后为空：Invalid，零 journal 写入。
    assert!(matches!(
        application.rename_session(" \n\t ").unwrap(),
        RenameOutcome::Invalid
    ));

    // 竞争沉淀：给 title worker 一点时间排空可能的迟到任务（用户
    // 标题已落盘，NoTitle 期望必然失败——静默 no-op）。
    for _ in 0..50 {
        if application.session_has_explicit_title() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // N3：journal 形状——1 或 2 条 session/title（改名必然在；自动
    // 命名在与谁先），最后一条是用户标题；Invalid 拒绝零写入。
    let events = load_events(&storage_root);
    let title_events: Vec<&crate::session::event::SessionEvent> = events
        .iter()
        .filter(|event| event.event_type == "session/title")
        .collect();
    assert!(
        !title_events.is_empty() && title_events.len() <= 2,
        "rename (and optionally the raced autotitle), refusals wrote nothing"
    );
    let manual = title_events.last().expect("at least the rename event");
    assert_eq!(
        manual
            .data
            .pointer("/source/kind")
            .and_then(serde_json::Value::as_str),
        Some("user")
    );
    assert_eq!(
        manual.data.get("title").and_then(serde_json::Value::as_str),
        Some("Renamed by hand")
    );

    // N6：新会话无标题；resume 原会话，快照带回存储标题。
    application.new_session().unwrap();
    assert_eq!(application.snapshot().unwrap().session_title, None);
    let summaries = application.list_sessions().unwrap();
    let target = summaries
        .iter()
        .find(|summary| summary.title.as_deref() == Some("Renamed by hand"))
        .expect("the renamed session summary");
    let resumed = application.switch_session(target.id.clone()).unwrap();
    assert_eq!(resumed.session_title.as_deref(), Some("Renamed by hand"));
    assert_eq!(
        application.snapshot().unwrap().session_title.as_deref(),
        Some("Renamed by hand")
    );

    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// M2/M4（图片附件管线）：带附件的 run——
/// - journal 的 user/message content = 文本 part + image part（引用
///   指向会话 attachments/ 目录内的副本，字节永不进日志）；
/// - 副本文件真实存在且内容与原件一致（原件此后可删，会话自包含）；
/// - 切走再切回（冷恢复重放整条日志）无错——admission/fold/投影
///   全链路接受 image part。
#[test]
fn image_attachments_journal_references_and_survive_resume() {
    let (storage_root, project_root) = roots("image-attach");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    // 原件：真实可解码的 1024×768 PNG（MM-1 起接纳做完整解码，
    // 头件不再能过闸）。
    let source = {
        let canvas = image::RgbImage::from_pixel(1024, 768, image::Rgb([255, 0, 0]));
        let mut encoded = Vec::new();
        image::DynamicImage::ImageRgb8(canvas)
            .write_to(
                &mut std::io::Cursor::new(&mut encoded),
                image::ImageFormat::Png,
            )
            .unwrap();
        let source = std::env::temp_dir().join(format!(
            "clat-source-{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&source, &encoded).unwrap();
        source
    };

    let done = run_with_attachments(&mut application, "look at this", vec![source.clone()])
        .expect("run completes");
    assert_eq!(done.output, "done");

    // journal 形状：文本 part + image part。MM-1 S2/S3 后引用指向
    // 内容寻址 blob（attachments/blobs/<digest>），存放规范化
    // 字节（同像素、合法 PNG、非逐字节恒等——原 assert 作废）。
    let events = load_events(&storage_root);
    let user_event = events
        .iter()
        .find(|event| event.event_type == "user/message")
        .expect("user message");
    let content = user_event.data["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], json!("text"));
    assert_eq!(content[0]["text"], json!("look at this"));
    assert_eq!(content[1]["type"], json!("image"));
    assert_eq!(content[1]["mediaType"], json!("image/png"));
    // INV-MM2-6：journal 图块 ref-only——不持久化绝对 path；身份是
    // attachmentId（规范化字节 sha256），blob 按 store 布局解析。
    assert!(
        content[1].get("path").is_none(),
        "the durable image block carries no absolute path"
    );
    let attachment_id = content[1]["attachmentId"].as_str().unwrap();
    assert_eq!(attachment_id.len(), 64, "sha256 hex id");
    let blob_path = find_blob(&storage_root, attachment_id);
    let blob = std::fs::read(&blob_path).unwrap();
    // 规范化不变量：blob 是可解码的合法 PNG，尺寸与源一致（≤2048 不缩）。
    {
        let decoded = image::ImageReader::new(std::io::Cursor::new(&blob))
            .with_guessed_format()
            .unwrap()
            .decode()
            .expect("the stored blob is a decodable image");
        assert_eq!((decoded.width(), decoded.height()), (1024, 768));
    }
    assert_eq!(
        content[1]["bytes"].as_u64().unwrap(),
        blob.len() as u64,
        "descriptor byte count matches the normalized blob"
    );

    // 原件删除后 resume：重放整条日志（含 image part）无错——
    // 会话自包含（blob 独立于原件）。
    std::fs::remove_file(&source).unwrap();
    let summary = application.list_sessions().unwrap();
    let target = summary.first().expect("session").id.clone();
    application.new_session().unwrap();
    let resumed = application.switch_session(target).unwrap();
    assert!(
        !resumed.replay.is_empty(),
        "the replay of the resumed session carries its events (incl. the image part)"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

struct RejectVisualPayloadScript {
    calls: AtomicUsize,
}

impl TestModelScript for RejectVisualPayloadScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        if request
            .instructions
            .is_some_and(|text| text.starts_with("Generate a concise title (at most 8 words)"))
        {
            events.emit(crate::ModelEvent::TextDelta {
                delta: "visual test".into(),
            });
            return Ok(crate::ModelResponse {
                text: "visual test".into(),
                tool_calls: Vec::new(),
                finish_reason: crate::FinishReason::Completed,
                usage: None,
                provider_response_id: None,
                provider_state: Vec::new(),
                reasoning: None,
            });
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(
            request
                .items
                .iter()
                .any(|item| { crate::model::model_item_image_parts(item).next().is_some() }),
            "the configured visual route receives the admitted image once"
        );
        Err(crate::ModelError::with_kind(
            crate::model::ModelErrorKind::Client,
            "compatible API returned 400 Bad Request: image_url is unsupported",
        ))
    }
}

/// MM-I1/MM-I3 adversarial regression: explicit capabilities replace the old
/// paid 400 probe. If a supposedly visual endpoint rejects the payload, the
/// client error surfaces after exactly one request. In particular there is no
/// second request whose text contains CLAT's local attachment path.
#[test]
fn visual_provider_400_fails_closed_without_path_bearing_retry() {
    let (storage_root, project_root) = roots("visual-400-fail-closed");
    std::fs::create_dir_all(&project_root).unwrap();
    let source = project_root.join("source.png");
    let canvas = image::RgbImage::from_pixel(8, 6, image::Rgb([20, 120, 220]));
    image::DynamicImage::ImageRgb8(canvas)
        .save_with_format(&source, image::ImageFormat::Png)
        .unwrap();
    let script = Arc::new(RejectVisualPayloadScript {
        calls: AtomicUsize::new(0),
    });
    let project = Project::new(&project_root);
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Scripted(script.clone()),
    );
    configure_test_model(&application);

    let failure = run_with_attachments(&mut application, "inspect", vec![source])
        .expect_err("the provider rejection must surface");
    assert_eq!(
        script.calls.load(Ordering::SeqCst),
        1,
        "client errors are never retried through a text/path downgrade"
    );
    assert!(failure.error.contains("image_url is unsupported"));
    assert!(!failure.error.contains("attachments"));
    let journal = serde_json::to_string(&load_events(&storage_root)).unwrap();
    assert!(!journal.contains("image attachment:"));
    assert!(!journal.contains(project_root.to_string_lossy().as_ref()));

    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

/// INV-MM2-3（MM-2 W2 红测）：model_state 全链路——旧配置 load 即
/// 迁移（版本门）→ preset stamp → typed overrides 合并；
/// apply→persist→reload 的 effective 值稳定；DeepSeek→GLM 的预设
/// 切换不留 stale 键（stream_options 随预设整体重置消失），Set
/// override 在切换后存活；run_token_budget 不受任何一层影响。
/// pre-fix（无 overrides 层）Set-存活腿红。
#[test]
fn model_state_migrates_merges_and_survives_reload_and_switches() {
    use crate::model::ProviderCredentials;

    let (storage_root, project_root) = roots("mm2-merge");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);

    // 旧 schema 配置（无 overrides 字段）：DeepSeek 预设 + 用户值。
    let legacy = serde_json::json!({
        "preset": "deepseek-v4-pro",
        "protocol": "open_ai_compatible",
        "model": "deepseek-v4-pro",
        "endpoint": "https://api.deepseek.com",
        "request_path": "/chat/completions",
        "auth_header": "Authorization",
        "auth_prefix": "Bearer ",
        "extra_headers": {},
        "extra_body": {},
        "output_limit": 100_000,
        "parallel_tool_calls": true,
        "run_token_budget": 777_777,
    });
    let config: crate::model::ModelConfig =
        serde_json::from_value(legacy).expect("legacy config parses");
    let credentials = ProviderCredentials::for_protocol(config.protocol);
    application
        .save_model_state(&config, &credentials)
        .expect("save legacy");

    // load → 迁移 → stamp → merge。
    let (effective, _) = application.model_state().expect("state");
    assert_eq!(
        effective.overrides.output_limit,
        crate::Override::Set(100_000),
        "user value != preset 384K migrates to Set"
    );
    assert_eq!(effective.output_limit, Some(100_000));
    assert_eq!(
        effective.overrides.max_context_tokens,
        crate::Override::Inherit,
        "unset window inherits (apply seeded 1M)"
    );
    assert_eq!(effective.max_context_tokens, Some(1_000_000));
    assert_eq!(effective.run_token_budget, Some(777_777));
    assert_eq!(
        effective.extra_body["stream_options"]["include_usage"], true,
        "DeepSeek preset carries its streaming usage switch"
    );

    // 持久化迁移产物 → reload：effective 稳定（版本门 + 同一合并）。
    application
        .save_model_state(&effective, &credentials)
        .expect("save migrated");
    let (reloaded, _) = application.model_state().expect("reload");
    assert_eq!(reloaded.output_limit, effective.output_limit);
    assert_eq!(reloaded.overrides, effective.overrides);
    assert_eq!(reloaded.overrides_version, Some(1));

    // A→B：换 GLM 预设（保留 Set override 与 budget）——DeepSeek 的
    // stream_options 不残留；Set(output_limit) 存活。
    let mut switched = reloaded.clone();
    switched.preset = Some("glm-5.3".into());
    application
        .save_model_state(&switched, &credentials)
        .expect("save switched");
    let (on_glm, _) = application.model_state().expect("state");
    assert!(
        on_glm.extra_body.get("stream_options").is_none(),
        "no stale DeepSeek keys on GLM"
    );
    assert_eq!(
        on_glm.extra_body["thinking"]["clear_thinking"], false,
        "GLM preset shape applied"
    );
    assert_eq!(
        on_glm.output_limit,
        Some(100_000),
        "the user's Set override survives the preset switch"
    );
    assert_eq!(on_glm.overrides.output_limit, crate::Override::Set(100_000));
    assert_eq!(on_glm.run_token_budget, Some(777_777));

    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// INV-MM2-2（MM-2 W1 attach 门，红测——pre-fix 无门全绿）：
/// - fail-closed 文本模型（无 preset 的 custom 配置）+ 图片 → 整轮
///   失败，错误可行动（点名换视觉模型 GLM 5.3 Flash / 移除图片），
///   且**任何 journal 写入之前**失败（load_events 为空——零痕迹）；
/// - unverified 的 doc 声明视觉（kimi-k3 预设）同样拒绝；
/// - probe-verified 的 glm-5.3-flash 放行（run 完成且 journal 带图）。
///
/// 删 prepare_run 的能力门即红（前两腿变成功）。
#[test]
fn image_attachments_are_gated_by_verified_model_capability() {
    use crate::model::ProviderCredentials;

    let (storage_root, project_root) = roots("mm2-attach-gate");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);

    // 可解码的真实 PNG 源。
    let source = std::env::temp_dir().join(format!(
        "clat-mm2-gate-{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    {
        let canvas = image::RgbImage::from_pixel(32, 32, image::Rgb([0, 120, 240]));
        let mut encoded = Vec::new();
        image::DynamicImage::ImageRgb8(canvas)
            .write_to(
                &mut std::io::Cursor::new(&mut encoded),
                image::ImageFormat::Png,
            )
            .unwrap();
        std::fs::write(&source, &encoded).unwrap();
    }

    let save_config = |application: &mut crate::TrustedProjectApplication,
                       config: crate::model::ModelConfig| {
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        application
            .save_model_state(&config, &credentials)
            .expect("save model");
    };

    // —— 1. fail-closed 文本（custom 无能力选择 = 缺省纯文本）。——
    // 能力门在 prepare_run 最前（start_run 同步返回 Err——任何会话
    // 物化/spawn 之前）。
    let session_count = |storage_root: &std::path::Path| {
        let backend = crate::session::persistence::JsonlBackend::new(
            storage_root.join("sessions"),
            crate::session::persistence::JsonlCompression::Zstd,
            false,
        );
        backend.list_headers().unwrap().len()
    };
    let attempt = |application: &mut crate::TrustedProjectApplication, prompt: &str| {
        let (completion, _receiver) = std::sync::mpsc::channel();
        application.start_run(crate::ApplicationRunRequest {
            message: crate::message::PendingMessage::from_front_end(
                prompt,
                None,
                vec![source.clone()],
            ),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
    };
    save_config(
        &mut application,
        crate::model::ModelConfig {
            model: "text-only".into(),
            endpoint: "https://application-test.invalid".into(),
            ..crate::model::ModelConfig::default()
        },
    );
    let error = match attempt(&mut application, "look") {
        Ok(_) => panic!("text-capability model rejects image attachments"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("does not accept image input"),
        "actionable gate reason: {error}"
    );
    assert!(
        error.contains("GLM 5.3 Flash"),
        "the error names a vision alternative: {error}"
    );
    // 零痕迹：能力拒绝发生在任何会话物化/journal 写入之前——连
    // session header 都不存在。
    assert_eq!(
        session_count(&storage_root),
        0,
        "the rejected round materializes no session at all"
    );

    // —— 2. unverified 视觉声明（kimi-k3：doc-only）同样拒绝。——
    save_config(
        &mut application,
        crate::model::ModelConfig {
            preset: Some("kimi-k3".into()),
            ..crate::model::ModelConfig::default()
        },
    );
    let error = match attempt(&mut application, "look") {
        Ok(_) => panic!("unverified (doc-only) vision capability stays closed"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("does not accept image input"));
    assert_eq!(session_count(&storage_root), 0, "still zero sessions");

    // —— 3. verified（glm-5.3-flash，MM-0 探针）放行。 ——
    save_config(
        &mut application,
        crate::model::ModelConfig {
            preset: Some("glm-5.3-flash".into()),
            ..crate::model::ModelConfig::default()
        },
    );
    let (completion, _receiver) = std::sync::mpsc::channel();
    let too_many = match application.start_run(crate::ApplicationRunRequest {
        message: crate::message::PendingMessage::from_front_end(
            "six images exceed the frozen route policy",
            None,
            vec![source.clone(); 6],
        ),
        asker: None,
        approver: allow_all_approver(),
        events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
        completion,
    }) {
        Ok(_) => panic!("GLM's five-image route limit must fail before provider I/O"),
        Err(error) => error,
    };
    assert!(too_many.to_string().contains("at most 5 images"));
    assert_eq!(
        session_count(&storage_root),
        0,
        "route-count rejection happens before session materialization"
    );
    let done = run_with_attachments(&mut application, "look at this", vec![source.clone()])
        .expect("the probe-verified vision preset admits images");
    assert_eq!(done.output, "done");
    let image_only = run_with_attachments(&mut application, "", vec![source.clone()])
        .expect("image-only prompts are valid once the route admits images");
    assert_eq!(image_only.output, "done");
    let events = load_events(&storage_root);
    let user_event = events
        .iter()
        .find(|event| event.event_type == "user/message")
        .expect("the admitted round journals the user message");
    assert_eq!(
        user_event.data["content"].as_array().unwrap()[1]["type"],
        json!("image")
    );

    std::fs::remove_file(&source).ok();
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// MM-1A 判别测试簇（不变量见 `src/message.rs` 模块文档；断言从
/// 不变量推导，pre-fix——无 descriptor 元数据/无幂等键/无回执——
/// 全部红）：
/// 1. journal image block 携带耐久 descriptor 元数据（attachmentId =
///    副本文件名主干、宽高/字节 = 导入实测、displayName = 原件名）与
///    `clientMessageId`/`requestDigest`（提交幂等 digest：文本 + staged
///    引用，不掺导入后重铸的 attachmentId）。
/// 2. live `RunStarted.message.blocks` 与回放 `UserMessage.content_blocks`
///    逐字段相等。**两条构造路径不同**（live 由 prepare_run 从导入结果
///    构造、回放由 adapter::content_blocks 解析 journal 词汇），本测试
///    用同一份 journal 把两份实现证等——是测试维持的等价，不是单一
///    实现的结构性保证；改任一侧都必须在此对拍（M-05，审查
///    2026-08-27）。
/// 3. completion 携带 `Committed` 回执；`committed_receipt` 在
///    **close → 重挂 → 冷恢复**之后给出同一答案（journal 投影重建，
///    非进程内状态——INV-M1A-4）。
/// 4. wire v1 additive：live `run_started` 行携带 content_blocks +
///    client_message_id；纯文本 run 的行与旧字节完全一致。
#[test]
fn admitted_images_carry_durable_metadata_and_rebuild_receipts_after_restart() {
    let (storage_root, project_root) = roots("mm1a-parity");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    // 原件：真实可解码的 1024×768 PNG（MM-1 起接纳做完整解码）。
    let source = std::env::temp_dir().join(format!(
        "clat-mm1a-{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let bytes = {
        let canvas = image::RgbImage::from_pixel(1024, 768, image::Rgb([255, 0, 0]));
        let mut encoded = Vec::new();
        image::DynamicImage::ImageRgb8(canvas)
            .write_to(
                &mut std::io::Cursor::new(&mut encoded),
                image::ImageFormat::Png,
            )
            .unwrap();
        std::fs::write(&source, &encoded).unwrap();
        encoded
    };

    let client_message_id = "mm1a-client-1".to_owned();
    let submission = crate::message::PendingMessage::from_front_end(
        "look at this",
        Some(client_message_id.clone()),
        vec![source.clone()],
    );
    let expected_digest = submission.request_digest();

    let live = Arc::new(Mutex::new(Vec::new()));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: submission,
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::clone(&live))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let done = receiver.recv().unwrap().expect("run completes");

    // —— 1. journal 事实 ——
    let events = load_events(&storage_root);
    let user_event = events
        .iter()
        .find(|event| event.event_type == "user/message")
        .expect("user message");
    assert_eq!(
        user_event.data["clientMessageId"].as_str(),
        Some(client_message_id.as_str()),
        "the client id rides the durable user/message event"
    );
    assert_eq!(
        user_event.data["requestDigest"].as_str(),
        Some(expected_digest.as_str()),
        "the journal digest is the submission (pre-admission) digest"
    );
    let image_block = &user_event.data["content"].as_array().unwrap()[1];
    assert_eq!(image_block["type"], json!("image"));
    // INV-MM2-6：ref-only——journal 不携带 path，blob 按 id 经 store
    // 布局解析，id 与 blob 文件名主干一致（内容寻址）。
    assert!(image_block.get("path").is_none());
    let attachment_id = image_block["attachmentId"].as_str().unwrap().to_owned();
    let blob_path = find_blob(&storage_root, &attachment_id);
    assert_eq!(
        attachment_id,
        blob_path.file_stem().unwrap().to_str().unwrap(),
        "the durable attachmentId is the blob's content-address id (blobs/<sha256> stem)"
    );
    assert_eq!(image_block["width"], json!(1024));
    assert_eq!(image_block["height"], json!(768));
    assert_eq!(
        image_block["bytes"],
        json!(bytes.len() as u64),
        "byte count is the normalized blob's length (the fixture re-encodes to identical bytes — byte-identity with the source is not an invariant)"
    );
    assert_eq!(
        image_block["displayName"].as_str(),
        source.file_name().unwrap().to_str(),
        "displayName is the original file name (no path semantics)"
    );
    let journal_message_id = user_event.data["id"].as_str().unwrap().to_owned();

    // —— 2. live / replay 逐字段相同 ——
    let live_blocks = {
        let guard = live.lock().unwrap();
        guard
            .iter()
            .find_map(|event| match event {
                RunEvent::RunStarted { message, .. } => Some(message.blocks.clone()),
                _ => None,
            })
            .expect("RunStarted was emitted")
    };
    assert_eq!(
        live_blocks.len(),
        2,
        "admitted content = text block + image block"
    );
    let replay = {
        let mut adapter = crate::session::replay::ReplayAdapter::new();
        let mut out = Vec::new();
        for event in &events {
            adapter.push(event, &mut out);
        }
        out
    };
    let replay_blocks = replay
        .iter()
        .find_map(|event| match event {
            crate::session::replay::ReplayEvent::UserMessage {
                content_blocks,
                client_message_id: replayed_id,
                receipt,
                ..
            } => {
                assert_eq!(
                    replayed_id.as_deref(),
                    Some(client_message_id.as_str()),
                    "the replayed user message carries the same client id"
                );
                let receipt = receipt
                    .as_deref()
                    .expect("keyed replay user message carries committed receipt");
                assert_eq!(receipt.state, crate::message::AdmissionState::Committed);
                assert_eq!(
                    receipt.committed_message_id.as_deref(),
                    Some(journal_message_id.as_str())
                );
                Some(content_blocks.clone())
            }
            _ => None,
        })
        .expect("replayed UserMessage");
    assert_eq!(
        live_blocks, replay_blocks,
        "live RunStarted blocks and replayed blocks are field-for-field identical"
    );
    assert!(matches!(
        &live_blocks[1],
        crate::message::ContentBlock::Image { attachment }
            if attachment.attachment_id == attachment_id
                && attachment.width == 1024
                && attachment.height == 768
                && attachment.bytes == bytes.len() as u64
    ));

    // —— 3. 回执：完成携带 Committed；重启后同答案 ——
    let receipt = done
        .receipt
        .as_ref()
        .expect("a client-keyed run carries its committed receipt");
    assert_eq!(receipt.state, crate::message::AdmissionState::Committed);
    assert_eq!(
        receipt.committed_message_id.as_deref(),
        Some(journal_message_id.as_str())
    );
    assert_eq!(receipt.attachment_ids, vec![attachment_id.clone()]);
    let before_restart = application
        .committed_receipt(&client_message_id)
        .expect("receipt is queryable before restart");
    assert_eq!(&before_restart, receipt.as_ref());
    assert!(
        application.committed_receipt("never-submitted").is_none(),
        "an unknown key has no receipt"
    );

    application.close().unwrap();
    // 冷重启：同一 storage root 全新进程语义（重挂 + 冷恢复整条日志）。
    let mut reopened = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&reopened);
    let summary = reopened.list_sessions().unwrap();
    let target = summary.first().expect("session survives").id.clone();
    let snapshot = reopened.switch_session(target).expect("resume");
    assert!(!snapshot.replay.is_empty());
    let after_restart = reopened
        .committed_receipt(&client_message_id)
        .expect("the journal projection rebuilds the committed receipt");
    assert_eq!(
        after_restart, before_restart,
        "the receipt answer is identical after a cold restart (journal is the authority)"
    );
    reopened.close().unwrap();
    std::fs::remove_file(&source).ok();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// MM-1A：commit point 之后的 run 失败仍携带 `Committed` 回执——
/// run 失败 ≠ 消息未送达，前端不得重新入箱（MM-I11）。同时锁定纯文本
/// wire 的字节稳定（INV-M1A-6：无 content_blocks 字段）。
#[test]
fn failed_run_after_commit_still_carries_the_committed_receipt() {
    let (storage_root, project_root) = roots("mm1a-failure-receipt");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Failure);
    configure_test_model(&application);

    let client_message_id = "mm1a-fail-1".to_owned();
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::from_front_end(
                "try and fail",
                Some(client_message_id.clone()),
                Vec::new(),
            ),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let failure = receiver.recv().unwrap().expect_err("the run fails");

    let receipt = failure
        .receipt
        .expect("a post-commit failure carries the receipt");
    assert_eq!(receipt.state, crate::message::AdmissionState::Committed);
    assert!(
        !receipt.retryable,
        "the message is durable; resending would duplicate it"
    );
    assert_eq!(
        &application
            .committed_receipt(&client_message_id)
            .expect("queryable"),
        receipt.as_ref(),
        "the completion receipt and the journal projection agree"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// MM-3：staged 图片 steering 由 core 规范化后与文本走同一
/// Reserved → claim-time Committed 状态机；伪造 descriptor 仍 fail closed。
#[test]
fn image_and_text_steering_share_durable_admission_and_forged_blocks_are_refused() {
    let (storage_root, project_root) = roots("mm1a-steer");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let gate = Arc::new(crate::test_support::SteerGate::default());
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Steer(Arc::clone(&gate)),
    );
    configure_test_model(&application);

    // 图片 descriptor 不接受前端自铸；必须有 core 的 staged admission。
    let with_image_block = crate::message::PendingMessage {
        client_message_id: None,
        content: crate::message::MessageContent::from_blocks(vec![
            crate::message::ContentBlock::Image {
                attachment: crate::message::AttachmentDescriptor {
                    attachment_id: "img".into(),
                    media_type: "image/png".into(),
                    width: 1,
                    height: 1,
                    bytes: 1,
                    display_name: None,
                    original_width: None,
                    original_height: None,
                },
            },
        ]),
        staged_attachments: Vec::new(),
        admitted_images: Vec::new(),
        submission_digest: None,
    };
    assert!(matches!(
        application.steer(with_image_block),
        SteerOutcome::Refused { .. }
    ));

    let source = project_root.join("steering.png");
    let canvas = image::RgbImage::from_pixel(24, 12, image::Rgb([20, 80, 160]));
    let mut encoded = Vec::new();
    image::DynamicImage::ImageRgb8(canvas)
        .write_to(
            &mut std::io::Cursor::new(&mut encoded),
            image::ImageFormat::Png,
        )
        .unwrap();
    std::fs::write(&source, encoded).unwrap();

    // 图片与纯文本 steering：幂等键随 mid-turn user/message 落盘。
    let live = Arc::new(Mutex::new(Vec::new()));
    let handle = {
        let (completion, _receiver) = mpsc::channel();
        application
            .start_run(ApplicationRunRequest {
                message: crate::message::PendingMessage::text("start work"),
                asker: None,
                approver: allow_all_approver(),
                events: Box::new(SharedEvents(Arc::clone(&live))),
                completion,
            })
            .unwrap()
    };
    gate.wait_entered();
    let image_steered = crate::message::PendingMessage::from_front_end(
        "inspect this",
        Some("mm3-image-steer-1".into()),
        vec![source],
    );
    let image_digest = image_steered.request_digest();
    let image_reserved = match application.steer(image_steered) {
        SteerOutcome::Queued {
            receipt: Some(receipt),
        } => receipt,
        outcome => panic!("image steering must reserve queue ownership: {outcome:?}"),
    };
    assert_eq!(
        image_reserved.state,
        crate::message::AdmissionState::Reserved
    );
    assert_eq!(image_reserved.attachment_ids.len(), 1);

    let steered = crate::message::PendingMessage::from_front_end(
        "also run the tests",
        Some("mm1a-steer-1".into()),
        Vec::new(),
    );
    let digest = steered.request_digest();
    let reserved = match application.steer(steered) {
        SteerOutcome::Queued {
            receipt: Some(receipt),
        } => receipt,
        outcome => panic!("client-keyed steering must return Reserved: {outcome:?}"),
    };
    assert_eq!(reserved.state, crate::message::AdmissionState::Reserved);
    assert!(!reserved.retryable, "the queued draft is owned by the run");
    gate.release();
    handle.join().unwrap();

    let image_live = live
        .lock()
        .unwrap()
        .iter()
        .find_map(|event| match event {
            RunEvent::SteeringApplied {
                message,
                receipt: Some(receipt),
                ..
            } if message.has_images() => Some((message.clone(), receipt.clone())),
            _ => None,
        })
        .expect("claimed image steering emits descriptor content and receipt");
    assert_eq!(image_live.0.attachment_ids(), image_live.1.attachment_ids);
    assert_eq!(
        image_live.1.state,
        crate::message::AdmissionState::Committed
    );

    let committed_live = live
        .lock()
        .unwrap()
        .iter()
        .find_map(|event| match event {
            RunEvent::SteeringApplied {
                receipt: Some(receipt),
                ..
            } if receipt.client_message_id == "mm1a-steer-1" => Some(receipt.clone()),
            _ => None,
        })
        .expect("claimed steering emits its committed receipt after journal flush");
    assert_eq!(
        committed_live.state,
        crate::message::AdmissionState::Committed
    );
    assert_eq!(committed_live.client_message_id, "mm1a-steer-1");
    assert!(!committed_live.retryable);

    let events = load_events(&storage_root);
    let steered_event = events
        .iter()
        .find(|event| {
            event.event_type == "user/message"
                && event.data["clientMessageId"].as_str() == Some("mm1a-steer-1")
        })
        .expect("the steering message is journaled with its client id");
    assert_eq!(
        steered_event.data["requestDigest"].as_str(),
        Some(digest.as_str()),
        "the steered message's digest covers its text payload"
    );
    let image_event = events
        .iter()
        .find(|event| {
            event.event_type == "user/message"
                && event.data["clientMessageId"].as_str() == Some("mm3-image-steer-1")
        })
        .expect("image steering is durable");
    assert_eq!(
        image_event.data["requestDigest"].as_str(),
        Some(image_digest.as_str()),
        "claim preserves the pre-normalization submission digest"
    );
    assert_eq!(image_event.data["content"][1]["type"], json!("image"));
    assert!(matches!(
        application
            .committed_receipt("mm1a-steer-1")
            .map(|receipt| receipt.state),
        Some(crate::message::AdmissionState::Committed)
    ));
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// M4：附件校验在 journal 写入**之前**整体失败——坏附件（不存在的
/// 文件）不产生任何事件，会话保持干净。
#[test]
fn invalid_attachments_fail_before_any_journal_write() {
    let (storage_root, project_root) = roots("image-invalid");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    let result = application.start_run(ApplicationRunRequest {
        message: crate::message::PendingMessage::from_front_end(
            "look",
            None,
            vec![std::path::PathBuf::from("/nonexistent/probe.png")],
        ),
        asker: None,
        approver: allow_all_approver(),
        events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
        completion: mpsc::channel().0,
    });
    assert!(result.is_err(), "the run refuses to start");
    // 校验先于会话使用：无日志头的会话不进列表——零 journal 痕迹。
    assert!(
        application.list_sessions().unwrap().is_empty(),
        "no journal trace of the refused run"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// MM-1 S1（INV-MM1-1/2，应用级红测——pre-fix 的 import 只查扩展名，
/// 伪扩展/超像素文件通过并复制进会话目录，本测试红）：magic 不符与
/// 超像素头在任何复制之前整体拒绝，零 journal 痕迹、零附件目录残留。
#[test]
fn forged_and_superpixel_attachments_fail_before_any_copy() {
    let (storage_root, project_root) = roots("mm1-s1-validate");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    // 伪扩展：JPEG SOI 挂 .png 后缀。
    let forged = std::env::temp_dir().join(format!(
        "clat-mm1-forged-{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&forged, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
    // 超像素：合法 PNG 头声明 5000×5000（25M px > 16M 上限）。
    let superpixel = std::env::temp_dir().join(format!(
        "clat-mm1-huge-{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut huge = vec![
        0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13, b'I', b'H', b'D', b'R',
    ];
    huge.extend_from_slice(&5000u32.to_be_bytes());
    huge.extend_from_slice(&5000u32.to_be_bytes());
    huge.extend_from_slice(&[8, 6, 0, 0, 0]);
    std::fs::write(&superpixel, &huge).unwrap();

    for source in [&forged, &superpixel] {
        let result = application.start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::from_front_end(
                "look",
                None,
                vec![source.clone()],
            ),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion: mpsc::channel().0,
        });
        assert!(result.is_err(), "{} must be refused", source.display());
    }
    assert!(
        application.list_sessions().unwrap().is_empty(),
        "no journal trace of the refused runs"
    );
    application.close().unwrap();
    std::fs::remove_file(&forged).ok();
    std::fs::remove_file(&superpixel).ok();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// S3/S5：运行中插话端到端。steer() 在第一次模型调用进行中入队；
/// run 因 pending steering 延长；第二个请求携带 steering 用户项；
/// journal 落 mid-turn user/message；live 流与 journal 回放对拍相等。
#[test]
fn steered_run_replays_identically() {
    let (storage_root, project_root) = roots("steer-parity");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let gate = Arc::new(crate::test_support::SteerGate::default());
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Steer(Arc::clone(&gate)),
    );
    configure_test_model(&application);

    let live = Arc::new(Mutex::new(Vec::new()));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("start work"),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::clone(&live))),
            completion,
        })
        .unwrap();

    gate.wait_entered();
    assert!(matches!(
        application.steer(crate::message::PendingMessage::text("also run the tests")),
        SteerOutcome::Queued { receipt: None }
    ));
    gate.release();
    handle.join().unwrap();
    let done = receiver.recv().unwrap().unwrap();

    assert!(!done.cancelled);
    assert_eq!(done.output, "steering handled");
    assert_eq!(done.turns, 2, "steering extends the run");
    assert!(
        gate.saw_steering.load(std::sync::atomic::Ordering::Acquire),
        "the second model request must carry the steering message"
    );
    application.close().unwrap();

    let events = load_events(&storage_root);
    let types: Vec<&str> = events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect();
    let steering_index = events
        .iter()
        .position(|event| {
            event.event_type == "user/message"
                && event.data["content"][0]["text"] == "also run the tests"
        })
        .expect("steering user/message journaled");
    let first_assistant = types
        .iter()
        .position(|kind| *kind == "assistant/message")
        .expect("first assistant");
    let last_assistant = types
        .iter()
        .rposition(|kind| *kind == "assistant/message")
        .expect("last assistant");
    assert!(
        first_assistant < steering_index && steering_index < last_assistant,
        "steering lands mid-turn: {types:?}"
    );

    let live_events = live.lock().unwrap().clone();
    assert_conversation_parity(&live_events, &events);
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// W1-04 红测门闩：recorder 在 `finish()` 收尾期才把终态事件转发给
/// UI sink——在 RunCompleted 的 emit 内阻塞，把 worker 精确卡在
/// "终态已判定、busy 仍为 true"的竞争窗口里。
struct TerminalGateSink {
    events: Arc<Mutex<Vec<RunEvent>>>,
    release: Arc<std::sync::atomic::AtomicBool>,
}

impl EventSink for TerminalGateSink {
    fn emit(&mut self, event: RunEvent) {
        let terminal = matches!(event, RunEvent::RunCompleted { .. });
        self.events.lock().unwrap().push(event);
        if terminal {
            // 有界等待：测试线程若在 release 前失败/panic，worker 也能
            // 自行脱困，不挂死测试进程。
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !self.release.load(std::sync::atomic::Ordering::Acquire) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "terminal gate was never released"
                );
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }
}

/// W1-04：终态判定与 steering 入队必须原子。模型已给出最终回答、
/// run 收尾（busy=false）尚未完成时，steer() 绝不能返回 Queued——
/// 那条消息永远不会被 claim，只能被丢弃。要么消息入队并延长 run，
/// 要么 NotRunning 回退普通提交。pre-fix 本测试红：窗口内 steer
/// 返回 Queued，消息成为孤儿。
#[test]
fn steer_at_the_terminal_boundary_never_queues_an_orphan_message() {
    let (storage_root, project_root) = roots("steer-terminal-race");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    let shared = Arc::new(Mutex::new(Vec::new()));
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("quick question"),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(TerminalGateSink {
                events: Arc::clone(&shared),
                release: Arc::clone(&release),
            }),
            completion,
        })
        .unwrap();

    // 等 RunCompleted 到达：此刻 worker 卡在收尾期的终态转发内，
    // busy 仍为 true——正是审计构造的窗口。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !shared
        .lock()
        .unwrap()
        .iter()
        .any(|event| matches!(event, RunEvent::RunCompleted { .. }))
    {
        assert!(std::time::Instant::now() < deadline, "run never completed");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(
        matches!(
            application.steer(crate::message::PendingMessage::text("important addendum")),
            SteerOutcome::NotRunning { receipt: None }
        ),
        "a run past its terminal decision must not accept steering"
    );
    release.store(true, std::sync::atomic::Ordering::Release);
    handle.join().unwrap();
    receiver.recv().unwrap().unwrap();
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 召回（2026-08-21，INV-SV3）：未 claim 的插话可 LIFO 召回且不留
/// 任何 journal 痕迹；召回不取消 run（剩余消息照常 claim 并延长
/// run）；run 结束后无可召回。
#[test]
fn steering_recall_is_lifo_silent_and_never_cancels() {
    let (storage_root, project_root) = roots("steer-recall");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let gate = Arc::new(crate::test_support::SteerGate::default());
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Steer(Arc::clone(&gate)),
    );
    configure_test_model(&application);

    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("start work"),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();

    gate.wait_entered();
    // 空队列召回 → None（前端 ESC 此时回落到取消语义）。
    assert_eq!(application.recall_pending_steering(), None);
    assert!(matches!(
        application.steer(crate::message::PendingMessage::text("kept message")),
        SteerOutcome::Queued { receipt: None }
    ));
    let recalled_pending = crate::message::PendingMessage::from_front_end(
        "recalled message",
        Some("mm2-w7-recalled-steer".into()),
        Vec::new(),
    );
    assert!(matches!(
        application.steer(recalled_pending.clone()),
        SteerOutcome::Queued {
            receipt: Some(receipt)
        } if receipt.state == crate::message::AdmissionState::Reserved
            && !receipt.retryable
    ));
    // LIFO：召回最后一条。
    assert_eq!(
        application.recall_pending_steering(),
        Some(crate::RecalledSteering {
            message: recalled_pending,
            receipt: Some(Box::new(crate::message::AdmissionReceipt::rolled_back(
                "mm2-w7-recalled-steer".into(),
                Vec::new(),
                "steering-recall",
            ))),
        })
    );
    // 召回不取消 run：放行后 run 继续，claim 的是剩余那条。
    gate.release();
    handle.join().unwrap();
    let done = receiver.recv().unwrap().unwrap();
    assert!(!done.cancelled, "recall must not cancel the run");
    assert_eq!(done.turns, 2, "the kept steering still extends the run");
    // run 结束后无可召回。
    assert_eq!(application.recall_pending_steering(), None);
    application.close().unwrap();

    // journal：kept 落盘（mid-turn user/message）；recalled 零痕迹。
    let events = load_events(&storage_root);
    let texts: Vec<String> = events
        .iter()
        .filter(|event| event.event_type == "user/message")
        .map(|event| {
            event.data["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert!(
        texts.iter().any(|text| text == "kept message"),
        "the kept steering is journaled: {texts:?}"
    );
    assert!(
        !texts.iter().any(|text| text == "recalled message"),
        "a recalled steering message must leave no durable trace: {texts:?}"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// S4：取消时未被 claim 的 steering 不落任何 journal 事件，live 与
/// 回放同样对拍（两侧都没有这条消息）。
#[test]
fn steering_during_a_cancelled_run_leaves_no_durable_trace() {
    let (storage_root, project_root) = roots("steer-cancel");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let gate = Arc::new(crate::test_support::SteerGate::default());
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Steer(Arc::clone(&gate)),
    );
    configure_test_model(&application);

    let live = Arc::new(Mutex::new(Vec::new()));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("start work"),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::clone(&live))),
            completion,
        })
        .unwrap();

    gate.wait_entered();
    assert!(matches!(
        application.steer(crate::message::PendingMessage::text("too late")),
        SteerOutcome::Queued { receipt: None }
    ));
    application.cancel_active_run();
    gate.release();
    handle.join().unwrap();
    let done = receiver.recv().unwrap().unwrap();
    assert!(done.cancelled, "cancel wins over the steering extension");
    application.close().unwrap();

    let events = load_events(&storage_root);
    assert!(
        !events.iter().any(|event| {
            event.event_type == "user/message" && event.data["content"][0]["text"] == "too late"
        }),
        "unclaimed steering must leave no journal trace"
    );
    let live_events = live.lock().unwrap().clone();
    assert_conversation_parity(&live_events, &events);
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// S4 契约：没有活动 run 时 steer 回 NotRunning，调用方据此回退为
/// 普通提交。
#[test]
fn steer_without_an_active_run_reports_not_running() {
    let (storage_root, project_root) = roots("steer-idle");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    assert!(matches!(
        application.steer(crate::message::PendingMessage::text("anyone there?")),
        SteerOutcome::NotRunning { receipt: None }
    ));
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// S3/S6/S7：ask_user 端到端。Pure 效果免审批（journal 无 approval
/// 事件）；tool/call 先于 tool/result（等待应答期间问题已耐久）；
/// 答案进结果；live 流与 journal 回放对拍。
#[test]
fn ask_user_tool_round_trips_through_the_journal() {
    let (storage_root, project_root) = roots("ask-user");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let asker = Arc::new(crate::test_support::ScriptedAsker {
        selected: "stable".into(),
        asked: Mutex::new(Vec::new()),
    });
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::AskUser(Arc::clone(&asker)),
    );
    configure_test_model(&application);

    let live = Arc::new(Mutex::new(Vec::new()));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("pick a channel"),
            approver: allow_all_approver(),
            asker: Some(Arc::clone(&asker) as Arc<dyn crate::interaction::UserAsker>),
            events: Box::new(SharedEvents(Arc::clone(&live))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let done = receiver.recv().unwrap().unwrap();
    application.close().unwrap();

    assert_eq!(done.output, "decision recorded");
    assert_eq!(
        *asker.asked.lock().unwrap(),
        vec!["Which release channel should we ship?".to_owned()]
    );

    let events = load_events(&storage_root);
    let types: Vec<&str> = events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect();
    assert!(
        !types
            .iter()
            .any(|kind| *kind == "approval/asked" || *kind == "approval/decided"),
        "Pure ask_user must not trip the approval flow: {types:?}"
    );
    let call_index = events
        .iter()
        .position(|event| event.event_type == "tool/call" && event.data["name"] == "ask_user")
        .expect("ask_user tool/call journaled");
    let result_index = events
        .iter()
        .position(|event| {
            event.event_type == "tool/result"
                && event.data["message"]["source"]["callId"] == "call-ask"
        })
        .expect("ask_user tool/result journaled");
    assert!(call_index < result_index);
    assert_eq!(
        events[result_index].data["message"]["content"][0]["isError"],
        false
    );
    let answer_text = events[result_index].data["message"]["content"][0]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        answer_text.contains("stable"),
        "answer in result: {answer_text}"
    );

    let live_events = live.lock().unwrap().clone();
    assert_conversation_parity(&live_events, &events);
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// S8：headless（asker: None）——ask_user 返回结构化错误结果，模型
/// 看到"没有交互前端"后继续，run 正常完成。
#[test]
fn ask_user_without_a_frontend_degrades_to_an_error_result() {
    let (storage_root, project_root) = roots("ask-headless");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let asker = Arc::new(crate::test_support::ScriptedAsker {
        selected: "stable".into(),
        asked: Mutex::new(Vec::new()),
    });
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::AskUser(Arc::clone(&asker)),
    );
    configure_test_model(&application);

    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("pick a channel"),
            approver: allow_all_approver(),
            asker: None,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let done = receiver.recv().unwrap().unwrap();
    application.close().unwrap();

    assert_eq!(done.output, "decision recorded");
    assert!(
        asker.asked.lock().unwrap().is_empty(),
        "no frontend installed — the asker must never be called"
    );

    let events = load_events(&storage_root);
    let result = events
        .iter()
        .find(|event| {
            event.event_type == "tool/result"
                && event.data["message"]["source"]["callId"] == "call-ask"
        })
        .expect("headless ask_user error result journaled");
    assert_eq!(result.data["message"]["content"][0]["isError"], true);
    let message = result.data["message"]["content"][0]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        message.contains("no interactive frontend"),
        "structured headless error: {message}"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 回归（设计变更 2026-08-19，DSH 范式）：agent 循环无轮次预算。
/// 此前 32 轮硬中断（"run exceeded the maximum of 32 model
/// turns"）与随后的有界自动续跑（[auto-continue] 注记）都是应急
/// 方案，已一并移除——DSH 的 kick() 即 `while (await turn())`，
/// 终态只有完成/错误/用户取消；上下文压力归 pruning/compaction
/// 管，轮数不是边界。ToolLoop(40)：40 次工具往返 + 1 次完成 =
/// 41 轮，远超旧 32 轮上限。预变更代码上本测试失败（run 中断或
/// journal 出现续跑注记）。
#[test]
fn long_tool_loops_run_uninterrupted_without_a_turn_budget() {
    let (storage_root, project_root) = roots("unbounded-loop");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::ToolLoop {
            calls: 40,
            seen: Arc::new(AtomicUsize::new(0)),
        },
    );
    configure_test_model(&application);

    let live = Arc::new(Mutex::new(Vec::new()));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("work far past the old 32-turn cap"),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::clone(&live))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let done = receiver.recv().unwrap().unwrap();

    assert_eq!(done.output, "loop complete");
    // 41 次模型调用 = 40 次工具往返（各计 1/1）+ 1 次完成（计
    // 2/3）：单次 run 内完整累计，无分段。
    assert_eq!(
        done.turns, 41,
        "the loop crosses the old 32-turn cap in one run"
    );
    assert_eq!(done.usage.input_tokens, 42);
    assert_eq!(done.usage.output_tokens, 43);
    application.close().unwrap();

    let events = load_events(&storage_root);
    assert_conversation_parity(&live.lock().unwrap(), &events);
    let count = |kind: &str| {
        events
            .iter()
            .filter(|event| event.event_type == kind)
            .count()
    };
    assert_eq!(count("tool/call"), 40);
    assert_eq!(count("tool/result"), 40);
    assert_eq!(count("turn/start"), 1);
    assert_eq!(count("turn/end"), 1);
    // 无续跑注记：journal 里不得出现任何合成 [auto-continue] 消息
    //（旧应急方案的存在痕迹）。
    assert!(
        !events.iter().any(|event| {
            event.event_type == "user/message"
                && event.data["content"][0]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("[auto-continue]"))
        }),
        "no synthetic continuation note may appear in the journal"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 自动压缩的 Application 级回归：历史跨过模型窗口的 80% 水位后，
/// worker 必须经真实 compactor/provider 接缝生成摘要，把四事件族原子
/// 落盘，并让 replace surface 在冷重启后继续可用。纯 `choose_cut`
/// 单测无法覆盖 run_lifecycle 接线、flush 或恢复投影中的任一断路。
#[test]
fn automatic_compaction_is_durable_and_survives_cold_reopen() {
    let (storage_root, project_root) = roots("automatic-compaction");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    // 12k window leaves enough room for the fixed summary reserves while a
    // handful of deliberately long turns deterministically cross 80%.
    configure_test_model_with_budget(&application, 12_000);

    let mut submitted = 0usize;
    for turn in 0..12 {
        let prompt = format!(
            "AUTO_COMPACT_SENTINEL_{turn}: {}",
            char::from(b'a' + (turn % 26) as u8)
                .to_string()
                .repeat(3_800)
        );
        run(&mut application, &prompt).expect("long-history turn completes");
        submitted += 1;
        if load_events(&storage_root)
            .iter()
            .any(|event| event.event_type == "compaction/summary")
        {
            break;
        }
    }
    assert!(
        submitted < 12,
        "the configured context pressure must trigger automatic compaction"
    );
    application.close().unwrap();

    let events = load_events(&storage_root);
    let family = events
        .windows(4)
        .find(|window| {
            window[0].event_type == "compaction/start"
                && window[1].event_type == "compaction/summary"
                && window[2].event_type == "user/message"
                && window[3].event_type == "compaction/end"
        })
        .expect("compaction family is contiguous and durable");
    assert!(
        family[2].data["source"]["plugin"] == json!("compaction"),
        "the replace carrier is distinguishable from a human message"
    );
    assert!(
        family[2].surface_op.is_some(),
        "the carrier replaces a prefix"
    );
    assert!(
        family[2]
            .source_event_seqs
            .as_ref()
            .is_some_and(|seqs| !seqs.is_empty()),
        "the replacement names every shadowed source event"
    );

    // A cold mount must restore the replacement surface and still accept a
    // normal next turn. The human transcript remains an audit view, so the
    // original sentinels stay visible rather than being destructively erased.
    let mut reopened = mount(&project, &storage_root, TestBehavior::Success);
    let snapshot = reopened.snapshot().expect("cold snapshot");
    assert!(
        snapshot
            .transcript
            .iter()
            .any(|line| { line.text.contains("AUTO_COMPACT_SENTINEL_0") }),
        "compaction never deletes the auditable human transcript"
    );
    run(&mut reopened, "continue after the durable summary")
        .expect("the compacted surface remains runnable after cold reopen");
    reopened.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// GLM 专属 MCP 包的判定（2026-08-19）：激活厂商为 GLM 且配置了
/// API Key 才产出四件套；密钥只进内存配置（服务端地址/鉴权形态
/// 见 glm_mcp_pack 测试），非 GLM 或无 key 一律空包——MCP 挂载
/// 永不因此失败。
#[test]
fn glm_mcp_pack_follows_the_active_vendor_and_key() {
    let (storage_root, project_root) = roots("glm-mcp-pack");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    // 默认（非 GLM）：空包。
    assert!(glm_mcp_pack_from_control(&application.control).is_empty());

    // GLM 预设 + key：四件套。
    let mut config = ModelConfig {
        preset: Some("glm-5.3".into()),
        ..ModelConfig::default()
    };
    preset_by_id("glm-5.3").expect("preset").apply(&mut config);
    let mut credentials = crate::model::ProviderCredentials::for_protocol(config.protocol);
    credentials.set_value(0, "glm-coding-key".into());
    application
        .save_model_state(&config, &credentials)
        .expect("save");
    let pack = glm_mcp_pack_from_control(&application.control);
    assert_eq!(pack.len(), 4);
    assert!(pack.iter().all(|(name, _)| name.starts_with("glm-")));

    // GLM 但无 key：空包。
    let empty = crate::model::ProviderCredentials::for_protocol(config.protocol);
    application.save_model_state(&config, &empty).expect("save");
    assert!(glm_mcp_pack_from_control(&application.control).is_empty());

    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// INV-VK1/VK2（厂商 key 记忆库，2026-08-21 用户报告：GLM↔DeepSeek
/// 来回切换反复被要求输入 key——单槽凭证切走即丢）：
/// `save_model_state` 输入即记忆；切换按目标端点厂商回填；空 key
/// 不抹记忆；`Other` 端点不入库；`vendor:` 保留行对用户档不可见。
#[test]
fn vendor_keys_survive_model_switches() {
    let (storage_root, project_root) = roots("vendor-keys");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);

    // GLM 带 key 保存 → 记忆。
    let mut glm_config = ModelConfig {
        preset: Some("glm-5.3".into()),
        ..ModelConfig::default()
    };
    preset_by_id("glm-5.3")
        .expect("preset")
        .apply(&mut glm_config);
    let mut glm_credentials = crate::model::ProviderCredentials::for_protocol(glm_config.protocol);
    glm_credentials.set_value(0, "glm-coding-key".into());
    application
        .save_model_state(&glm_config, &glm_credentials)
        .expect("save glm");

    // 切到 DeepSeek：单槽被覆盖（旧行为），但 GLM 的 key 已进记忆库。
    let mut ds_config = ModelConfig {
        preset: Some("deepseek-v4-flash".into()),
        ..ModelConfig::default()
    };
    preset_by_id("deepseek-v4-flash")
        .expect("preset")
        .apply(&mut ds_config);
    let mut ds_credentials = crate::model::ProviderCredentials::for_protocol(ds_config.protocol);
    ds_credentials.set_value(0, "deepseek-key".into());
    application
        .save_model_state(&ds_config, &ds_credentials)
        .expect("save deepseek");

    // 切回 GLM：厂商记忆回填旧 key（修复前无此路径，测试即红）。
    let restored_glm = application
        .vendor_key(glm_config.protocol, &glm_config.endpoint)
        .expect("glm key remembered across the switch");
    assert_eq!(restored_glm.value(0), Some("glm-coding-key"));
    let restored_ds = application
        .vendor_key(ds_config.protocol, &ds_config.endpoint)
        .expect("deepseek key remembered");
    assert_eq!(restored_ds.value(0), Some("deepseek-key"));

    // 空 key 保存不抹记忆（清空不是换 key）。
    let empty = crate::model::ProviderCredentials::for_protocol(glm_config.protocol);
    application
        .save_model_state(&glm_config, &empty)
        .expect("save empty");
    assert_eq!(
        application
            .vendor_key(glm_config.protocol, &glm_config.endpoint)
            .expect("glm key survives an empty save")
            .value(0),
        Some("glm-coding-key")
    );

    // Other 端点不入库、不回填（自定义端点互不相干）。
    let mut custom = glm_config.clone();
    custom.preset = None;
    custom.endpoint = "https://my-proxy.example/v1".into();
    assert!(
        application
            .vendor_key(custom.protocol, &custom.endpoint)
            .is_none()
    );
    application
        .save_model_state(&custom, &glm_credentials)
        .expect("save custom");
    assert!(
        application
            .vendor_key(custom.protocol, &custom.endpoint)
            .is_none()
    );

    // vendor: 保留行对用户档列表不可见；用户档不得占用该前缀。
    let profiles = application.list_model_profiles().expect("list");
    assert!(
        profiles
            .iter()
            .all(|profile| !profile.name.starts_with("vendor:"))
    );
    assert!(
        application
            .save_model_profile("vendor:Fake", &glm_config, &glm_credentials)
            .is_err()
    );

    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 状态栏 Cache/Context 启动即有值（2026-08-19 用户反馈）：journal
/// 的 assistant/message.usage 在挂载回放的同一遍流里折叠（不多流
/// 一遍日志），snapshot 还原会话累计与最近一次请求——不待首次
/// run 上报。TestModel::Success 每次完成上报 (120/30/100)。
#[test]
fn snapshot_restores_usage_stats_from_the_journal() {
    let (storage_root, project_root) = roots("usage-restore");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "one").unwrap();
    run(&mut application, "two").unwrap();
    application.close().unwrap();

    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    let snapshot = application.snapshot().expect("snapshot");
    assert_eq!(snapshot.session_usage.input_tokens, 240);
    assert_eq!(snapshot.session_usage.output_tokens, 60);
    assert_eq!(snapshot.session_usage.cached_input_tokens, Some(200));
    let last = snapshot.last_request_usage.expect("last request usage");
    assert_eq!(
        (last.input_tokens, last.output_tokens),
        (120, 30),
        "the context watermark is the most recent report"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 启动性能回归：挂载路径 resume 时已经全量流式回放过一次日志
/// （arm_session），但 `snapshot()` 又从 0 重放一遍——大会话（MB 级
/// zstd）+ debug 构建下即用户实测的"启动好几秒才见 TUI"。
/// 不变量：mount 产出的 replay 必须被随后的 snapshot() 复用（同
/// `switch_session` 复用 view 的既有先例），不得再触发全量流。
/// 验证：stream_events（全量流唯一入口）的测试计数器在 snapshot()
/// 前后必须相等。预修复代码上本测试失败（计数 +1）。
/// 注：不能用"移走会话目录"来断绝盘读——SessionRootDir 持有打开
/// 的目录 fd，路径 rename 对已挂载进程不可见（capability-held 设计）。
#[test]
fn mount_time_snapshot_reuses_the_resume_replay_without_restreaming() {
    let (storage_root, project_root) = roots("startup-replay");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "hello clat").unwrap();
    application.close().unwrap();

    let expected = crate::session::replay::ReplayAdapter::fold(&load_events(&storage_root));
    assert!(!expected.is_empty());

    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    let streams_before = application.sessions.stream_probe();
    let snapshot = application.snapshot().expect("mount-time snapshot");
    let streams_after = application.sessions.stream_probe();
    assert_eq!(
        snapshot.replay, expected,
        "mount-time snapshot must carry the resume replay"
    );
    assert_eq!(
        streams_before, streams_after,
        "snapshot() right after mount must not re-stream the log"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// T5：门面两条出口（mount 恢复的 `snapshot()`、`switch_session` 含
/// 同 id 快路径）携带的回放 == 直接折叠 journal；懒会话回放为空。
#[test]
fn snapshots_carry_the_structured_replay() {
    let (storage_root, project_root) = roots("replay-facade");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "hello clat").unwrap();
    let id = application.current_session_id().expect("session id");
    application.close().unwrap();

    let expected = crate::session::replay::ReplayAdapter::fold(&load_events(&storage_root));
    assert!(!expected.is_empty());

    // Mount-time resume: snapshot() carries the full replay (the resume
    // seed marker skipped by the fold never shows up).
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    assert_eq!(application.snapshot().unwrap().replay, expected);

    // Same-id fast path through switch_session.
    let switched = application.switch_session(id).unwrap();
    assert_eq!(switched.replay, expected);

    // A lazy fresh session (no log yet) replays empty.
    application.new_session().unwrap();
    assert!(application.snapshot().unwrap().replay.is_empty());
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn new_run_resume_exit_reopen_user_sequence() {
    let (storage_root, project_root) = roots("cutover-sequence");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    // /new 后不输入 → 磁盘零会话（懒物化）。
    application.new_session().unwrap();
    assert!(application.list_sessions().unwrap().is_empty());

    run(&mut application, "hello clat").unwrap();
    let id = application.current_session_id().expect("session id");
    application.close().unwrap();

    // 重开：workspace 选择自动恢复该会话。
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    assert_eq!(application.current_session_id(), Some(id));
    let transcript = application.snapshot().unwrap().transcript;
    let user_lines: Vec<&str> = transcript
        .iter()
        .filter(|line| line.kind == "user")
        .map(|line| line.text.as_str())
        .collect();
    assert_eq!(user_lines, vec!["hello clat"]);

    // 第二轮追加进同一会话；resume 列表出现一次。
    run(&mut application, "second turn").unwrap();
    application.close().unwrap();
    let application = mount(&project, &storage_root, TestBehavior::Success);
    let sessions = application.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].turns, 2);
    assert_eq!(sessions[0].message_count, 4);
    application.close().unwrap();

    // /new 后退出重启为 Fresh（有意变更：不悄悄重开旧会话）。
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    application.new_session().unwrap();
    application.close().unwrap();
    let application = mount(&project, &storage_root, TestBehavior::Success);
    assert!(
        application.current_session_id().is_none(),
        "Fresh selection survives a reopen with no prompt"
    );
    application.close().unwrap();

    // end-seed：每个携带内容的重开恰好一条；无新内容的重开不增长。
    let events = load_events(&storage_root);
    let seed_count = |events: &[crate::session::event::SessionEvent]| {
        events
            .iter()
            .filter(|event| event.event_type == "session/end-seed")
            .count()
    };
    assert_eq!(seed_count(&events), 2, "two content-bearing reopens");
    let application = mount(&project, &storage_root, TestBehavior::Success);
    application.close().unwrap();
    let events = load_events(&storage_root);
    assert_eq!(
        seed_count(&events),
        2,
        "an untouched reopen does not grow the log"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// INV-MP5（事实赢 + 保序修复）：会话物化后、投影写盘前崩溃的等价
/// 注入——手工把 id 从 sessionIds 剔除。重挂载后对账收编它；指针
/// 完好时恢复现场照常。取代旧 Materializing 归一化测试（该状态已被
/// 事实/投影二分结构性取代）。
#[test]
fn mount_reconciles_a_session_that_missed_the_ledger_write() {
    let (storage_root, project_root) = roots("mp1-reconcile");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "first session").unwrap();
    let first = application.current_session_id().unwrap();
    application.close().unwrap();

    // 注入漂移：sessionIds 少记了 first（崩溃窗口等价物）。
    remove_session_from_ledger(&storage_root, &project_root, &first);
    let application = mount(&project, &storage_root, TestBehavior::Success);
    // 指针仍指 first：恢复现场照常命中。
    assert_eq!(application.current_session_id(), Some(first.clone()));
    // 账本已被对账修复（目录赢）。
    let workspaces = application.workspaces().unwrap();
    let ledger = workspaces
        .iter()
        .find(|workspace| {
            workspace.path == crate::control_storage::sentinel::project_key(&project_root)
        })
        .expect("the workspace")
        .session_ids
        .clone();
    assert!(ledger.iter().any(|id| id == first.as_str()), "{ledger:?}");
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 直接改写 workspace.json：从 sessionIds 中剔除一个 id（漂移注入）。
fn remove_session_from_ledger(
    storage_root: &std::path::Path,
    project_root: &std::path::Path,
    session: &SessionId,
) {
    let path = storage_root.join("storages").join("workspace.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let key = crate::control_storage::sentinel::project_key(project_root);
    let record = value["tables"]["workspaces"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .find(|record| record["path"] == serde_json::json!(key))
        .expect("the workspace record");
    record["sessionIds"] = json!(
        record["sessionIds"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|id| id.as_str() != Some(session.as_str()))
            .collect::<Vec<_>>()
    );
    std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
}

/// MP-1 验收 §9-7 + 负责人拍板（每工作区各记各的）：A/B/A 交替使用时，
/// 重开 A 恢复 A 自己的上次会话（全局指针下会回到空白——本腿是
/// 该决策的判别锚）。同一存储根、两个项目、顺序挂载。
#[test]
fn alternating_projects_each_restore_their_own_session() {
    let (storage_root, project_a) = roots("mp1-aba-a");
    let (root_b, project_b) = roots("mp1-aba-b");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    // 两个项目共用 A 的存储根（B 自己的临时存储根弃用）。
    let project_a_handle = Project::new(&project_a);
    let project_b_handle = Project::new(&project_b);

    let mut application = mount(&project_a_handle, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "session in A").unwrap();
    let session_a = application.current_session_id().unwrap();
    application.close().unwrap();

    let mut application = mount(&project_b_handle, &storage_root, TestBehavior::Success);
    run(&mut application, "session in B").unwrap();
    let session_b = application.current_session_id().unwrap();
    assert_ne!(session_a, session_b);
    application.close().unwrap();

    // 回到 A：恢复 A 自己的会话（不是 B 的，也不是空白）。
    let application = mount(&project_a_handle, &storage_root, TestBehavior::Success);
    assert_eq!(
        application.current_session_id(),
        Some(session_a.clone()),
        "A/B/A 交替：每工作区指针独立（负责人拍板 2026-08-23）"
    );
    // 多项目地基 API：两个工作区都在册，全局现场指向当前进程的 B 交互
    // 之后……第三次挂载 enter A 已把 active 指向 A。
    let workspaces = application.workspaces().unwrap();
    assert_eq!(workspaces.len(), 2);
    let active = application.active_workspace().unwrap().expect("active");
    assert_eq!(
        active.path,
        crate::control_storage::sentinel::project_key(&project_a)
    );
    assert_eq!(
        active.active_session_id.as_deref(),
        Some(session_a.as_str())
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    std::fs::remove_dir_all(root_b.parent().unwrap()).ok();
}

/// MP-1 §4.4 两腿：首次进入 = 惰性建区（首条耐久会话落盘时注册，
/// title 取目录名）；二次进入 = 命中定位（不重复建区）。未物化前
/// （仅 /new）零写盘。
#[test]
fn first_entry_registers_lazily_and_second_entry_hits() {
    let (storage_root, project_root) = roots("mp1-lazy-register");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);

    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    // 仅挂载 + /new：无工作区注册（storages/workspace.json 不存在）。
    application.new_session().unwrap();
    assert!(application.workspaces().unwrap().is_empty());
    assert!(
        !storage_root
            .join("storages")
            .join("workspace.json")
            .exists()
    );
    // 首条耐久会话落盘 → 建区（title = 目录名）。
    configure_test_model(&application);
    run(&mut application, "materialize").unwrap();
    let id = application.current_session_id().unwrap();
    let workspaces = application.workspaces().unwrap();
    assert_eq!(workspaces.len(), 1);
    assert_eq!(
        workspaces[0].title,
        project_root.file_name().unwrap().to_string_lossy(),
        "title 取目录名（开放问题①默认）"
    );
    assert_eq!(workspaces[0].session_ids, vec![id.as_str().to_owned()]);
    application.close().unwrap();

    // 二次进入：命中既有工作区（同 id，不重复建区）。
    let application = mount(&project, &storage_root, TestBehavior::Success);
    assert_eq!(application.current_session_id(), Some(id));
    assert_eq!(application.workspaces().unwrap().len(), 1);
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// MP-1 §4.5 + INV-MP6：v4 旧库（clat.db）在场 → 挂载走升级路径：
/// 旧库改名保尸（字节原样）、新控制面诞生、既有会话（事实源）被
/// 收编进新注册表。
#[test]
fn legacy_sqlite_control_plane_is_upgraded_and_sessions_survive() {
    let (storage_root, project_root) = roots("mp1-legacy-upgrade");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::create_dir_all(&storage_root).unwrap();
    let project = Project::new(&project_root);
    // 旧世界尸体 + 一个既有会话日志（事实源不动）。
    std::fs::write(storage_root.join("clat.db"), b"legacy sqlite bytes").unwrap();
    let legacy_id = SessionId::new("session-legacy-survivor");
    let canonical = crate::control_storage::sentinel::project_key(&project_root);
    let legacy_dir = storage_root
        .join(crate::control_storage::sentinel::SESSION_ROOT_NAME)
        .join(crate::session::path_layout::project_key(&canonical))
        .join(crate::session::path_layout::encode_segment(
            legacy_id.as_str(),
        ));
    std::fs::create_dir_all(&legacy_dir).unwrap();
    {
        use std::io::Write as _;
        let header = crate::session::header::SessionHeader::new(
            legacy_id.clone(),
            Some(canonical.clone()),
            1_700_000_000_000,
        );
        let mut line = header.to_line();
        line.push('\n');
        let mut buffer = Vec::new();
        let mut encoder = zstd::stream::Encoder::new(&mut buffer, 3).unwrap();
        encoder.write_all(line.as_bytes()).unwrap();
        encoder.finish().unwrap();
        std::fs::write(legacy_dir.join("session.jsonl.zstd"), buffer).unwrap();
    }

    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    // 保尸 + 新控制面。
    let corpse = std::fs::read_dir(&storage_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .find(|name| {
            name.starts_with("clat.db.bak-") && !name.ends_with("-wal") && !name.ends_with("-shm")
        })
        .expect("the corpse is preserved");
    assert_eq!(
        std::fs::read(storage_root.join(&corpse)).unwrap(),
        b"legacy sqlite bytes"
    );
    assert!(!storage_root.join("clat.db").exists());
    assert!(storage_root.join("config.json").exists());
    // 事实源收编：旧会话出现在 /resume 列表（零迁移下唯一幸存面）。
    let sessions = application.list_sessions().unwrap();
    assert!(
        sessions.iter().any(|summary| summary.id == legacy_id),
        "{sessions:?}"
    );
    // 恢复旧会话 = 首次耐久激活 → 注册工作区并收编。
    application.switch_session(legacy_id.clone()).unwrap();
    let workspaces = application.workspaces().unwrap();
    assert_eq!(workspaces.len(), 1);
    assert!(
        workspaces[0]
            .session_ids
            .iter()
            .any(|id| id == legacy_id.as_str())
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// INV-MP3 原子写三腿（控制面级）：settings 撕裂 → 拒载进抢救路径
///（残件保留 + 空态 + 诊断）；projcache 撕裂 → 静默重建；workspace
/// 撕裂 → 抢救 + 残件保留。版本错位（INV-MP6）→ fail-closed 拒载。
#[test]
fn torn_control_files_are_salvaged_and_version_gates_fail_closed() {
    let root = std::env::temp_dir().join(format!(
        "clat-torn-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    crate::control_storage::sentinel::initialize(&root).expect("initialize");

    // —— 腿 1：settings.json 撕裂 → 抢救（残件 + 空态 + 诊断）。——
    std::fs::write(root.join("settings.json"), "{\"unit\": {\"name\": \"sett").unwrap();
    let storage = ControlStorage::open_ready(&root).expect("salvage opens");
    let diagnostics = storage.take_salvage_diagnostics();
    assert!(
        diagnostics
            .iter()
            .any(|line| line.contains("settings.json") && line.contains("torn")),
        "{diagnostics:?}"
    );
    let torn_remnant = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .find(|name| name.starts_with("settings.json.torn-"))
        .expect("the torn remnant is preserved");
    assert!(torn_remnant.starts_with("settings.json.torn-"));
    assert!(
        storage.load_model_state().unwrap().is_none(),
        "fresh empty state"
    );
    drop(storage);

    // —— 腿 2：版本错位 → fail-closed（拒载，不抢救）。——
    std::fs::write(
        root.join("trust.json"),
        serde_json::json!({"unit": {"name": "trust", "version": 99}, "projects": {}}).to_string(),
    )
    .unwrap();
    let error = match ControlStorage::open_ready(&root) {
        Ok(_) => panic!("version mismatch must fail closed"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("trust.json"), "{error}");
    assert!(error.to_string().contains("unit trust v99"), "{error}");
    std::fs::remove_file(root.join("trust.json")).unwrap();

    // —— 腿 3：workspace.json 撕裂 → 抢救 + 残件保留（tables 是事实，
    // 丢失响亮上报，会话由收编自愈）。——
    std::fs::create_dir_all(root.join("storages")).unwrap();
    std::fs::write(
        root.join("storages").join("workspace.json"),
        "{\"unit\": {\"name\": \"workspace\", \"versi",
    )
    .unwrap();
    let storage = ControlStorage::open_ready(&root).expect("salvage opens");
    let diagnostics = storage.take_salvage_diagnostics();
    assert!(
        diagnostics
            .iter()
            .any(|line| line.contains("workspace.json") && line.contains("torn")),
        "{diagnostics:?}"
    );
    assert!(storage.workspace_infos().is_empty());
    assert!(
        std::fs::read_dir(root.join("storages"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("workspace.json.torn-"))
    );
    drop(storage);

    // —— 腿 4：projcache 撕裂 → 静默重建（纯缓存，无诊断无残件）。——
    std::fs::write(
        root.join("storages").join("session_projcache.json"),
        "]{not json",
    )
    .unwrap();
    let storage = ControlStorage::open_ready(&root).expect("projcache rebuild opens");
    assert!(
        storage.take_salvage_diagnostics().is_empty(),
        "silent rebuild"
    );
    drop(storage);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn cancelled_run_closes_the_turn_as_aborted_by_user() {
    let (storage_root, project_root) = roots("cutover-cancel");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Cancel);
    configure_test_model(&application);

    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("cancel me"),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    // 等 provider 进入取消等待后取消。
    std::thread::sleep(Duration::from_millis(200));
    handle.cancel();
    handle.join().unwrap();
    let done = receiver.recv().unwrap().expect("cancelled run succeeds");
    assert!(done.cancelled);
    application.close().unwrap();

    let events = load_events(&storage_root);
    let turn_end = events.last().unwrap();
    assert_eq!(turn_end.event_type, "turn/end");
    assert_eq!(turn_end.data["reason"]["kind"], "aborted");
    assert_eq!(turn_end.data["reason"]["reason"], "user");
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn failed_stream_keeps_its_partial_assistant_message_durable() {
    let (storage_root, project_root) = roots("audit-partial-text");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Failure);
    configure_test_model(&application);

    let result = run(&mut application, "explode please");
    assert!(result.is_err(), "the provider failure must fail the run");
    application.close().unwrap();

    let events = load_events(&storage_root);
    // 部分文本必须耐久：UI 已展示的内容，resume 后仍在。
    let partial = events
        .iter()
        .find(|event| event.event_type == "assistant/message")
        .expect("partial assistant/message is durable");
    assert_eq!(partial.data["message"]["content"][0]["text"], "partial");
    let turn_end = events.last().unwrap();
    assert_eq!(turn_end.event_type, "turn/end");
    assert_eq!(turn_end.data["reason"]["kind"], "error");
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 审计 P1-08：目标日志存在但损坏——stage 阶段失败，指针与内存里的
/// 活动会话都保持原样（修复前：CAS 先落、旧会话先关，一次失败的
/// /resume 就能把指针指向坏目标并让进程失去活动会话）。
#[test]
fn switching_to_a_corrupt_session_leaves_the_pointer_and_active_session_intact() {
    let (storage_root, project_root) = roots("audit-switch-corrupt");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "anchor session").unwrap();
    let anchor = application.current_session_id().unwrap();
    application.close().unwrap();

    // A second, physically corrupt session in the same project.
    let corrupt_id = SessionId::new("corrupt-target");
    let corrupt_dir = storage_root
        .join("sessions")
        .join(crate::session::path_layout::project_key(
            // MP-1：bucket 从 realpath 规范形正向编码（macOS 临时目录
            // 是符号链接路径，raw 拼写与 canonical 不同）。
            &crate::control_storage::sentinel::project_key(&project_root),
        ))
        .join(crate::session::path_layout::encode_segment(
            corrupt_id.as_str(),
        ));
    std::fs::create_dir_all(&corrupt_dir).unwrap();
    std::fs::write(corrupt_dir.join("session.jsonl.zstd"), b"garbage bytes").unwrap();

    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    let error = application
        .switch_session(corrupt_id.clone())
        .expect_err("switching to a corrupt session must fail at the stage phase");
    assert!(error.to_string().contains("corrupt session log"), "{error}");
    assert_eq!(
        persisted_pointer(&storage_root, &project_root).as_deref(),
        Some(anchor.as_str()),
        "the pointer never moved to the corrupt target"
    );
    assert_eq!(
        application.current_session_id(),
        Some(anchor.clone()),
        "the old session is still active and untouched"
    );
    // And the anchor still works: a run appends into it.
    run(&mut application, "still usable").unwrap();
    assert_eq!(application.current_session_id(), Some(anchor));
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 审计 P1-08（MP-1 重述）：/new 的指针持久化失败时，旧会话不被销毁。
/// 原 CAS 竞态腿随 revision CAS 一起结构性消失（单写者互斥，设计
/// §4.6）；判别锚改为投影写失败注入（storages/ 只读，Unix）。
#[test]
#[cfg(unix)]
fn new_session_write_failure_keeps_the_old_session() {
    let (storage_root, project_root) = roots("audit-new-write");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "anchor session").unwrap();
    let anchor = application.current_session_id().unwrap();

    use std::os::unix::fs::PermissionsExt as _;
    let storages = storage_root.join("storages");
    std::fs::set_permissions(&storages, std::fs::Permissions::from_mode(0o500)).unwrap();
    let error = application
        .new_session()
        .expect_err("the pointer write must fail on a read-only storages dir");
    std::fs::set_permissions(&storages, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(error.to_string().contains("workspace.json"), "{error}");
    assert_eq!(
        application.current_session_id(),
        Some(anchor),
        "the old session survived the failed /new"
    );
    // 内存/磁盘不分叉（落盘失败回滚）：恢复可写后 /new 成功。
    application.new_session().unwrap();
    assert_eq!(application.current_session_id(), None);
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 复核 R5（MP-1 重述）：重新选择当前已活动的会话必须是无条件
/// no-op——不 stage、不 arm 第二个同会话 writer、不发生任何持久化
/// 写（双 writer 会打开同一日志的双写窗口）。指针写失败注入下对
/// 活动 id 的切换仍须成功并返回现场 transcript。
#[test]
fn switching_to_the_already_active_session_never_stages_a_second_writer() {
    let (storage_root, project_root) = roots("recheck-switch-active");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "anchor session").unwrap();
    let anchor = application.current_session_id().unwrap();

    // 任何持久化写都会失败（Unix 注入；Windows 腿退化为只验证
    // no-op 语义本身）。成功重选 = 提交路径零触碰。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let storages = storage_root.join("storages");
        std::fs::set_permissions(&storages, std::fs::Permissions::from_mode(0o500)).unwrap();
        let snapshot = application
            .switch_session(anchor.clone())
            .expect("re-selecting the active session must not commit anything");
        std::fs::set_permissions(&storages, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            snapshot
                .transcript
                .iter()
                .any(|line| line.text.contains("anchor session")),
            "the snapshot reflects the live transcript"
        );
    }

    let snapshot = application
        .switch_session(anchor.clone())
        .expect("re-selecting the active session must not commit anything");
    assert!(
        snapshot
            .transcript
            .iter()
            .any(|line| line.text.contains("anchor session")),
        "the snapshot reflects the live transcript"
    );

    run(&mut application, "still usable").unwrap();
    assert_eq!(application.current_session_id(), Some(anchor));
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 第三轮复审 S1：spawn/prepare 失败不得把 request/header 记为已发
/// ——否则该会话的第一个成功 run 会被去重抑制，永远没有 header
///（直到重开自愈）。
#[test]
fn failed_run_spawn_does_not_mark_the_request_header_emitted() {
    let (storage_root, project_root) = roots("audit-header-spawnfail");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    application.fail_next_run_spawn_for_test();
    let (completion, _receiver) = mpsc::channel();
    let error = match application.start_run(ApplicationRunRequest {
        message: crate::message::PendingMessage::text("doomed run"),
        asker: None,
        approver: allow_all_approver(),
        events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
        completion,
    }) {
        Ok(_handle) => panic!("injected spawn failure must fail the start"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("intentional"), "{error}");

    // The next, real run must still journal its request/header.
    run(&mut application, "real run").unwrap();
    let events = load_events(&storage_root);
    let headers: Vec<&crate::session::event::SessionEvent> = events
        .iter()
        .filter(|event| event.event_type == "request/header")
        .collect();
    assert_eq!(headers.len(), 1, "the header survived the failed spawn");
    assert_eq!(headers[0].data["reason"], json!("initial"));
    let tool_names = headers[0].data["header"]["tools"]
        .as_array()
        .expect("tool catalog")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(
        tool_names,
        [
            "list_files",
            "read_file",
            "search",
            "write_file",
            "edit_file",
            "apply_patch",
            "run_command",
            "exec_command",
            "write_stdin",
            "ask_user",
            "skill",
            "exit_plan_mode",
            "memory_search",
            "update_goal",
            "view_image",
            "todo_write",
        ],
        "request/header freezes the complete model-visible native catalog order"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 第三轮复审（catalog §2.7）：同会话内 header 未变化不追加
/// request/header；变化时以 reason "change" 追加。修复前每个 run 都
/// 写一条，且后续 run 的 reason 语义错误（既非 initial 也非 resume）。
#[test]
fn request_header_appends_once_and_only_again_on_change() {
    let (storage_root, project_root) = roots("audit-header-dedupe");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    run(&mut application, "first run").unwrap();
    run(&mut application, "second run, unchanged header").unwrap();
    let events = load_events(&storage_root);
    let headers: Vec<&crate::session::event::SessionEvent> = events
        .iter()
        .filter(|event| event.event_type == "request/header")
        .collect();
    assert_eq!(
        headers.len(),
        1,
        "an unchanged header appends nothing further"
    );
    assert_eq!(headers[0].data["reason"], json!("initial"));
    assert_eq!(
        headers[0].data["header"]["imageProjection"],
        json!({
            "route": "OpenAI Compatible/deterministic",
            "policy": {
                "mediaTypes": ["image/png", "image/jpeg"],
                "maxImages": 8,
                "maxBytes": 4 * 1024 * 1024,
            },
            "estimatorVersion": crate::media::IMAGE_TOKEN_ESTIMATOR_VERSION,
            "calibrationVersion": crate::media::IMAGE_TOKEN_CALIBRATION_VERSION,
            "encoderVersion": crate::session::attachments::ATTACHMENT_ENCODER_VERSION,
        }),
        "request/header freezes route, policy, estimator, calibration, and encoder identity"
    );

    // Change the model: the next run appends exactly one "change".
    let (mut config, credentials) = application.model_state().unwrap();
    config.model = "other-model".into();
    application.save_model_state(&config, &credentials).unwrap();
    run(&mut application, "third run, new model").unwrap();
    let events = load_events(&storage_root);
    let headers: Vec<&crate::session::event::SessionEvent> = events
        .iter()
        .filter(|event| event.event_type == "request/header")
        .collect();
    assert_eq!(headers.len(), 2, "a changed header appends once");
    assert_eq!(headers[1].data["reason"], json!("change"));
    assert_eq!(
        headers[1].data["header"]["config"]["model"],
        json!("other-model")
    );

    // A reopened session resumes with exactly one "resume" header.
    application.close().unwrap();
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    run(&mut application, "fourth run after reopen").unwrap();
    let events = load_events(&storage_root);
    let reasons: Vec<&str> = events
        .iter()
        .filter(|event| event.event_type == "request/header")
        .map(|event| event.data["reason"].as_str().unwrap())
        .collect();
    assert_eq!(reasons, vec!["initial", "change", "resume"]);
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn process_completion_is_a_frontend_neutral_application_event() {
    let (storage_root, project_root) = roots("process-completion-notice");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::RunCommand);
    configure_test_model(&application);
    let (sender, receiver) = mpsc::channel();
    application.subscribe(sender);

    run(&mut application, "run the command").expect("command run");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let notice = loop {
        assert!(
            std::time::Instant::now() < deadline,
            "process notice timeout"
        );
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(ApplicationEvent::ProcessFinished {
                session_id,
                exit_code,
                ..
            }) => {
                break (session_id, exit_code);
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("notice channel closed"),
        }
    };
    assert_eq!(notice.0, 1);
    assert_eq!(notice.1, Some(0));
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

struct CrossRunProcessScript {
    step: AtomicUsize,
}

impl TestModelScript for CrossRunProcessScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        let step = self.step.fetch_add(1, Ordering::SeqCst);
        let mut response = |text: &str| {
            if !text.is_empty() {
                events.emit(crate::ModelEvent::TextDelta { delta: text.into() });
            }
            crate::ModelResponse {
                text: text.into(),
                tool_calls: Vec::new(),
                finish_reason: crate::FinishReason::Completed,
                usage: None,
                provider_response_id: None,
                provider_state: Vec::new(),
                reasoning: None,
            }
        };
        Ok(match step {
            0 => crate::ModelResponse {
                text: String::new(),
                tool_calls: vec![crate::ToolCall {
                    id: "start-old-process".into(),
                    name: "exec_command".into(),
                    arguments: json!({
                        "cmd": "(sleep 1; printf orphan > cross-run-marker) & wait",
                        "yield_time_ms": 250
                    }),
                }],
                finish_reason: crate::FinishReason::ToolCalls,
                usage: None,
                provider_response_id: None,
                provider_state: Vec::new(),
                reasoning: None,
            },
            1 => response("first run complete"),
            2 => crate::ModelResponse {
                text: String::new(),
                tool_calls: vec![crate::ToolCall {
                    id: "poll-old-process".into(),
                    name: "write_stdin".into(),
                    arguments: json!({"session_id": 1}),
                }],
                finish_reason: crate::FinishReason::ToolCalls,
                usage: None,
                provider_response_id: None,
                provider_state: Vec::new(),
                reasoning: None,
            },
            3 => {
                let old_id_rejected = request.items.iter().any(|item| {
                    matches!(item, crate::ModelItem::ToolResult(result)
                        if result.tool_name == "write_stdin"
                            && result.is_error
                            && result.output.to_string().contains("not available in this run"))
                });
                if !old_id_rejected {
                    return Err(crate::ModelError::request(
                        "old process id was not fenced from the next run",
                    ));
                }
                response("old process fenced")
            }
            _ => return Err(crate::ModelError::request("unexpected scripted request")),
        })
    }
}

#[test]
fn process_session_ids_never_cross_application_run_ownership() {
    let (storage_root, project_root) = roots("process-cross-run-fence");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let script = Arc::new(CrossRunProcessScript {
        step: AtomicUsize::new(0),
    });
    let mut application = mount(&project, &storage_root, TestBehavior::Scripted(script));
    configure_test_model(&application);

    run(&mut application, "start a background process").expect("first run");
    // 收割腿依赖 POSIX shell 的后台孤儿语义（`& wait`）；Windows 的
    // cmd.exe 会因重定向副作用直接创建空标记文件，且交互会话的进程
    // 树隔离在 Windows 尚未毕业（PTY 路径已显式拒绝）。Windows 腿随
    // 进程树隔离落地后回归。
    #[cfg(unix)]
    {
        std::thread::sleep(Duration::from_millis(1300));
        assert!(
            !project_root.join("cross-run-marker").exists(),
            "run terminal must reap background descendants before the next run"
        );
    }
    let done = run(&mut application, "try the old process id").expect("second run");
    assert_eq!(done.output, "old process fenced");
    assert!(
        load_events(&storage_root).iter().any(|event| {
            event.event_type == "tool/result"
                && serde_json::to_string(&event.data)
                    .is_ok_and(|text| text.contains("not available in this run"))
        }),
        "durable replay must preserve a process-tool error, not rewrite it as an empty output summary"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn process_bind_failure_still_closes_the_turn_and_publishes_one_terminal() {
    let (storage_root, project_root) = roots("process-bind-failure");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    let occupied = application
        .process_service
        .bind_run("synthetic-owner", crate::CancelToken::new())
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("bind must fail"),
            approver: allow_all_approver(),
            asker: None,
            events: Box::new(SharedEvents(Arc::clone(&events))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let failure = receiver.recv().unwrap().expect_err("bind failure");
    assert!(failure.error.contains("process service could not bind"));
    let terminal = events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| {
            matches!(
                event,
                RunEvent::RunCompleted { .. }
                    | RunEvent::RunCancelled { .. }
                    | RunEvent::RunFailed { .. }
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(terminal.len(), 1);
    assert!(matches!(terminal[0], RunEvent::RunFailed { .. }));
    let durable = load_events(&storage_root);
    assert_eq!(durable.last().unwrap().event_type, "turn/end");
    assert_eq!(durable.last().unwrap().data["reason"]["kind"], "error");
    application.process_service.unbind_run(occupied).unwrap();
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// /resume 的提交失败（指针持久化写盘失败）必须显式关闭 unpublished
/// armed target：既不泄漏 writer，也不得把扣留的 resume seed 写入目标
/// 日志。原 CAS 竞态腿随 revision CAS 一起结构性消失；判别锚改为
/// 投影写失败注入（storages/ 只读，Unix）。
#[test]
#[cfg(unix)]
fn resume_commit_failure_drops_the_staged_target_without_leaking_a_writer() {
    let (storage_root, project_root) = roots("audit-resume-commit");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "first session").unwrap();
    let first = application.current_session_id().unwrap();
    application.new_session().unwrap();
    run(&mut application, "second session").unwrap();
    let second = application.current_session_id().unwrap();
    assert_ne!(first, second);
    let first_key = SessionKey {
        project: ProjectKey::from_cwd(&crate::control_storage::sentinel::project_key(
            &project_root,
        )),
        id: first.clone(),
    };
    let first_log = crate::session::persistence::JsonlBackend::new(
        storage_root.join(crate::control_storage::sentinel::SESSION_ROOT_NAME),
        JsonlCompression::Zstd,
        true,
    );
    let seeds_before = first_log
        .inspect(&first_key)
        .unwrap()
        .events
        .iter()
        .filter(|event| event.event_type == "session/end-seed")
        .count();

    // 指针写盘失败注入（staging 完成之后）。
    use std::os::unix::fs::PermissionsExt as _;
    let storages = storage_root.join("storages");
    std::fs::set_permissions(&storages, std::fs::Permissions::from_mode(0o500)).unwrap();
    let baseline = crate::session::write_behind::live_writers_for_test();
    let error = application
        .switch_session(first.clone())
        .expect_err("the pointer write must fail on a read-only storages dir");
    std::fs::set_permissions(&storages, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(error.to_string().contains("workspace.json"), "{error}");
    assert_eq!(application.current_session_id(), Some(second));
    let seeds_after = first_log
        .inspect(&first_key)
        .unwrap()
        .events
        .iter()
        .filter(|event| event.event_type == "session/end-seed")
        .count();
    assert_eq!(
        seeds_after, seeds_before,
        "a failed commit closes the armed target without publishing its seed"
    );
    // 30s 容忍窗口（并行套件里别家测试的 writer 会有瞬时存活）：
    // 真泄漏永不满足，瞬时 +1 在间隙处穿过。5s 窗口在慢 CI 上被
    // 邻测覆盖时会假红（2026-08-19 两次 CI 事故的方法论修正）。
    for _ in 0..1_200 {
        if crate::session::write_behind::live_writers_for_test() <= baseline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(
        crate::session::write_behind::live_writers_for_test() <= baseline,
        "dropping the staged target must not leak a writer thread (now {})",
        crate::session::write_behind::live_writers_for_test()
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 审计 P1-01（MP-1 重述）：非法 session root + 需要提交的控制面
/// （Fresh 或旧库升级）→ 挂载失败时 config.json 不存在、旧库原样
/// （提交发生在 preflight 通过之后——零写纪律）。原 PendingCommit
/// 状态已死（单文件哨兵无两写窗口），Fresh 与 LegacySQLite 两腿接棒。
#[test]
fn commit_over_an_invalid_session_root_publishes_nothing() {
    let (storage_root, project_root) = roots("audit-commit-preflight");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::create_dir_all(&storage_root).unwrap();
    let project = Project::new(&project_root);
    // 旧库在场：升级（改名保尸 + 新哨兵）同样必须等 preflight 通过。
    std::fs::write(storage_root.join("clat.db"), b"legacy sqlite bytes").unwrap();
    // An invalid session root: a bucket that is a symlink pointing out
    //（unix 逃逸攻击）；Windows 上 symlink 需要特权，以「bucket 不是
    // 目录」的 NotADirectory 形态攻击同一不变量（preflight 两种都拒）。
    let sessions = storage_root.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let outside = storage_root.parent().unwrap().join("outside-bucket");
    std::fs::create_dir_all(&outside).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, sessions.join("--tmp-evil--")).unwrap();
    #[cfg(windows)]
    std::fs::write(sessions.join("--tmp-evil--"), b"not a directory").unwrap();

    let mounted = BootstrapApplication::open(project.clone(), storage_root.clone())
        .and_then(|bootstrap| bootstrap.authorize_and_mount(crate::ProjectAuthorization::grant()));
    let error = match mounted {
        Ok(application) => panic!(
            "mount must fail the preflight, got {:?}",
            application.current_session_id()
        ),
        Err(error) => error,
    };
    // 两种攻击形态（unix symlink 逃逸 / Windows NotADirectory bucket）
    // 都必须被 preflight 拒绝——断言认形态族，不锁具体文案。
    let message = error.to_string();
    assert!(
        message.contains("symlink") || message.contains("not a directory"),
        "{error}"
    );
    assert!(
        !storage_root.join("config.json").exists(),
        "the upgrade must not publish the sentinel over an invalid session root"
    );
    assert_eq!(
        std::fs::read(storage_root.join("clat.db")).unwrap(),
        b"legacy sqlite bytes",
        "the legacy database is untouched (no rename before preflight)"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn switching_to_a_missing_session_errors_without_touching_the_pointer() {
    let (storage_root, project_root) = roots("audit-switch-missing");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "anchor session").unwrap();
    let anchor = application.current_session_id().unwrap();
    application.close().unwrap();

    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    let error = application
        .switch_session(SessionId::new("no-such-session"))
        .expect_err("switching to a missing session must fail");
    assert!(error.to_string().contains("no-such-session"), "{error}");
    // 指针未被污染：仍是 anchor。
    assert_eq!(
        persisted_pointer(&storage_root, &project_root).as_deref(),
        Some(anchor.as_str())
    );
    // 原会话仍是活动会话。
    assert_eq!(application.current_session_id(), Some(anchor));
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn worker_spawn_failure_leaves_no_durable_trace() {
    let (storage_root, project_root) = roots("audit-spawn-failure");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    application.fail_next_run_spawn_for_test();

    let (completion, _receiver) = mpsc::channel();
    let error = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("never persisted"),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .err()
        .expect("spawn failure surfaces");
    assert!(error.to_string().contains("intentional"));
    application.close().unwrap();

    // 无会话日志、无 workspace 指针行：失败路径不留半份状态。
    let sessions_dir = storage_root.join("sessions");
    assert!(
        !sessions_dir.exists() || std::fs::read_dir(&sessions_dir).unwrap().next().is_none(),
        "no session log may exist after a spawn failure"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// W7 receipt state machine: failure before the durable prelude returns a
/// retryable RolledBack receipt for a client-keyed message and leaves no
/// committed projection behind.
#[test]
fn mm2_w7_worker_spawn_failure_rolls_back_client_keyed_admission() {
    let (storage_root, project_root) = roots("mm2-w7-spawn-rollback");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    application.fail_next_run_spawn_for_test();

    let (completion, _receiver) = mpsc::channel();
    let error = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::from_front_end(
                "retry me",
                Some("mm2-w7-precommit".into()),
                Vec::new(),
            ),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .err()
        .expect("injected spawn failure");
    let receipt = error
        .admission_receipt()
        .expect("client-keyed startup failure has a receipt");
    assert_eq!(receipt.state, crate::message::AdmissionState::RolledBack);
    assert!(receipt.retryable);
    assert_eq!(
        receipt.failure_phase.as_deref(),
        Some("run-start-pre-commit")
    );
    assert!(
        application.committed_receipt("mm2-w7-precommit").is_none(),
        "pre-commit failure must not manufacture a committed projection"
    );

    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// W7 channel-close fault: the worker receiver disappears only after it has
/// spawned; the user event is then appended+flushed and delivery fails. The
/// error must say Committed/non-retryable and the journal projection must
/// agree, otherwise a frontend would duplicate the message on retry.
#[test]
fn mm2_w7_worker_start_channel_failure_after_commit_returns_committed_receipt() {
    let (storage_root, project_root) = roots("mm2-w7-channel-committed");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    application.fail_next_run_start_receive_for_test();

    let client_message_id = "mm2-w7-postcommit";
    let (completion, _receiver) = mpsc::channel();
    let error = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::from_front_end(
                "land exactly once",
                Some(client_message_id.into()),
                Vec::new(),
            ),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .err()
        .expect("closed start receiver must fail startup");
    let receipt = error
        .admission_receipt()
        .expect("post-commit startup failure has a receipt");
    assert_eq!(receipt.state, crate::message::AdmissionState::Committed);
    assert!(!receipt.retryable);
    assert_eq!(receipt.failure_phase.as_deref(), Some("worker-start-send"));
    let projected = application
        .committed_receipt(client_message_id)
        .expect("journal projection");
    assert_eq!(projected.state, receipt.state);
    assert_eq!(projected.committed_message_id, receipt.committed_message_id);
    assert_eq!(projected.attachment_ids, receipt.attachment_ids);
    assert_eq!(projected.retryable, receipt.retryable);
    assert_eq!(projected.failure_phase, None);
    assert_eq!(
        load_events(&storage_root)
            .iter()
            .filter(|event| {
                event.event_type == "user/message"
                    && event.data["clientMessageId"].as_str() == Some(client_message_id)
            })
            .count(),
        1
    );

    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn todo_write_lands_as_an_event_and_restores_on_reopen() {
    let (storage_root, project_root) = roots("cutover-todo");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Todo);
    configure_test_model(&application);

    let calls = Arc::new(AtomicUsize::new(0));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("track the work"),
            asker: None,
            approver: Arc::new(CountingApprover(Arc::clone(&calls))),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    receiver.recv().unwrap().expect("todo run completes");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "todo_write is SessionWrite: no approval round-trip"
    );
    let todo = application.todo_snapshot_for_test();
    assert_eq!(todo.len(), 2);
    application.close().unwrap();

    // 重开恢复 todo 快照（todo 投影，非 marker）。
    let application = mount(&project, &storage_root, TestBehavior::Todo);
    assert_eq!(application.todo_snapshot_for_test().len(), 2);
    application.close().unwrap();

    let events = load_events(&storage_root);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "todo/write")
            .count(),
        1,
        "exactly one todo/write event"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == "approval/asked"),
        "SessionWrite tools never hit the approval barrier"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// MM-2/W5 production-path harness: catalog gate → native invocation →
/// transient provider image → ref-only journal → cold replay. The adversarial
/// legs prove that FullAccess cannot turn the tool into an ambient file reader
/// and that a guessed attachment digest is not authority.
struct ViewImageLoopScript {
    step: AtomicUsize,
    attachment_id: Mutex<Option<String>>,
}

impl ViewImageLoopScript {
    fn completed(
        text: &str,
        events: &mut dyn crate::model::ModelEventSink,
    ) -> crate::ModelResponse {
        events.emit(crate::ModelEvent::TextDelta { delta: text.into() });
        crate::ModelResponse {
            text: text.into(),
            tool_calls: Vec::new(),
            finish_reason: crate::FinishReason::Completed,
            usage: None,
            provider_response_id: None,
            provider_state: Vec::new(),
            reasoning: None,
        }
    }

    fn tool_call(id: &str, arguments: Value) -> crate::ModelResponse {
        crate::ModelResponse {
            text: String::new(),
            tool_calls: vec![crate::ToolCall {
                id: id.into(),
                name: "view_image".into(),
                arguments,
            }],
            finish_reason: crate::FinishReason::ToolCalls,
            usage: None,
            provider_response_id: None,
            provider_state: Vec::new(),
            reasoning: None,
        }
    }
}

impl TestModelScript for ViewImageLoopScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        if request.tools.is_empty()
            && request
                .instructions
                .is_some_and(|text| text.starts_with("Generate a concise title (at most 8 words)"))
        {
            return Ok(Self::completed("visual test", events));
        }
        let step = self.step.fetch_add(1, Ordering::SeqCst);
        assert!(
            request.tools.iter().any(|tool| tool.name == "view_image"),
            "verified visual runs expose view_image in the per-run catalog"
        );
        Ok(match step {
            0 => Self::tool_call("view-project", json!({"project_relative_path": "shot.png"})),
            1 => {
                let result = request
                    .items
                    .iter()
                    .find_map(|item| match item {
                        crate::ModelItem::ToolResult(result)
                            if result.call_id == "view-project" =>
                        {
                            Some(result)
                        }
                        _ => None,
                    })
                    .expect("project image result reaches the next model request");
                assert!(!result.is_error);
                assert_eq!(result.blocks.len(), 1);
                assert!(matches!(
                    result.image_parts.as_slice(),
                    [crate::model::ContentPart::Image { path, media_type }]
                        if std::path::Path::new(path).is_file() && media_type == "image/png"
                ));
                let crate::message::ContentBlock::Image { attachment } = &result.blocks[0] else {
                    panic!("view_image must produce an image descriptor")
                };
                *self.attachment_id.lock().unwrap() = Some(attachment.attachment_id.clone());
                Self::tool_call(
                    "view-authorized-id",
                    json!({"attachment_id": attachment.attachment_id}),
                )
            }
            2 => {
                let result = request
                    .items
                    .iter()
                    .find_map(|item| match item {
                        crate::ModelItem::ToolResult(result)
                            if result.call_id == "view-authorized-id" =>
                        {
                            Some(result)
                        }
                        _ => None,
                    })
                    .expect("reachable attachment id result");
                assert!(!result.is_error);
                assert_eq!(result.image_parts.len(), 1);
                Self::tool_call(
                    "view-absolute",
                    json!({"project_relative_path": "/tmp/forbidden.png"}),
                )
            }
            3 => {
                let result = request
                    .items
                    .iter()
                    .find_map(|item| match item {
                        crate::ModelItem::ToolResult(result)
                            if result.call_id == "view-absolute" =>
                        {
                            Some(result)
                        }
                        _ => None,
                    })
                    .expect("absolute path result");
                assert!(result.is_error);
                assert!(
                    result
                        .output
                        .to_string()
                        .contains("must be project-relative")
                );
                Self::tool_call(
                    "view-orphan",
                    json!({"attachment_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}),
                )
            }
            4 => {
                let result = request
                    .items
                    .iter()
                    .find_map(|item| match item {
                        crate::ModelItem::ToolResult(result) if result.call_id == "view-orphan" => {
                            Some(result)
                        }
                        _ => None,
                    })
                    .expect("orphan id result");
                assert!(result.is_error);
                assert!(result.output.to_string().contains("not reachable"));
                Self::completed("visual loop complete", events)
            }
            5 => {
                let replayed = request.items.iter().find_map(|item| match item {
                    crate::ModelItem::ToolResult(result) if result.call_id == "view-project" => {
                        Some(result)
                    }
                    _ => None,
                });
                let replayed = replayed.expect("cold resume restores the visual tool result");
                assert_eq!(replayed.blocks.len(), 1);
                assert!(matches!(
                    replayed.image_parts.as_slice(),
                    [crate::model::ContentPart::Image { path, media_type }]
                        if std::path::Path::new(path).is_file() && media_type == "image/png"
                ));
                Self::completed("cold replay complete", events)
            }
            _ => return Err(crate::ModelError::request("unexpected visual-loop step")),
        })
    }
}

#[test]
fn view_image_is_fenced_provider_visible_and_cold_replayable() {
    let (storage_root, project_root) = roots("mm2-view-image-loop");
    std::fs::create_dir_all(&project_root).unwrap();
    let image_path = project_root.join("shot.png");
    let canvas = image::RgbImage::from_pixel(8, 6, image::Rgb([20, 120, 220]));
    image::DynamicImage::ImageRgb8(canvas)
        .save_with_format(&image_path, image::ImageFormat::Png)
        .unwrap();
    let project = Project::new(&project_root);
    let script = Arc::new(ViewImageLoopScript {
        step: AtomicUsize::new(0),
        attachment_id: Mutex::new(None),
    });
    let mut application = mount_with_permission_modes(
        &project,
        &storage_root,
        TestBehavior::Scripted(script.clone()),
        crate::permission::PermissionMode::FullAccess,
    );
    configure_test_model(&application);
    run(&mut application, "inspect the project image").expect("visual loop");
    let live_context = application.context_snapshot().unwrap();
    assert_eq!(
        live_context.image_count, 2,
        "both successful typed tool-result images are counted in request order"
    );
    assert!(live_context.image_bytes > 0);
    assert!(live_context.image_token_estimate > 0);
    application.close().unwrap();

    let events = load_events(&storage_root);
    let result_event = events
        .iter()
        .find(|event| {
            event.event_type == "tool/result"
                && event
                    .data
                    .pointer("/message/source/callId")
                    .and_then(Value::as_str)
                    == Some("view-project")
        })
        .expect("successful visual result journaled");
    let durable = &result_event.data["message"]["content"][0]["content"];
    let image = durable
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["type"] == json!("image"))
        .expect("descriptor image block");
    assert_eq!(
        image["attachmentId"].as_str(),
        script.attachment_id.lock().unwrap().as_deref()
    );
    assert!(image.get("path").is_none(), "journal is ref-only");
    assert!(
        !serde_json::to_string(result_event)
            .unwrap()
            .contains(project_root.to_string_lossy().as_ref()),
        "tool result journal never exposes a host path"
    );
    let replay = crate::session::replay::ReplayAdapter::fold(&events);
    assert!(replay.iter().any(|event| matches!(
        event,
        crate::session::replay::ReplayEvent::ToolFinished {
            call_id,
            content_blocks,
            ..
        } if call_id == "view-project" && content_blocks.len() == 1
    )));

    let mut reopened = mount_modes_from_storage(
        &project,
        &storage_root,
        TestBehavior::Scripted(script.clone()),
    );
    configure_test_model(&reopened);
    let cold_context = reopened.context_snapshot().unwrap();
    assert_eq!(
        (
            cold_context.image_count,
            cold_context.image_bytes,
            cold_context.image_token_estimate,
        ),
        (
            live_context.image_count,
            live_context.image_bytes,
            live_context.image_token_estimate,
        ),
        "cold replay reconstructs the same recursive image budget"
    );
    run(&mut reopened, "continue from the image").expect("cold visual replay");
    reopened.close().unwrap();
    assert_eq!(script.step.load(Ordering::SeqCst), 6);
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// MM-5 paid product-chain gate: real Application/agent catalog → model-chosen
/// `view_image` call → native fenced import → typed image tool result → second
/// live GLM request. The key is supplied only to LiveGlmProviderPlugin through
/// the process environment; persisted credentials stay empty.
#[test]
#[ignore = "paid GLM Application/view_image loop; set CLAT_GLM_CODING_PLAN_KEY explicitly"]
fn live_glm_application_calls_view_image_and_consumes_its_typed_result() {
    if std::env::var_os("CLAT_GLM_CODING_PLAN_KEY").is_none() {
        eprintln!("live GLM Application/view_image gate not armed; skipping");
        return;
    }
    let (storage_root, project_root) = roots("mm5-live-view-image-loop");
    std::fs::create_dir_all(&project_root).unwrap();
    let image_path = project_root.join("live-green.png");
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        128,
        128,
        image::Rgb([0, 220, 0]),
    ))
    .save_with_format(&image_path, image::ImageFormat::Png)
    .unwrap();
    let project = Project::new(&project_root);
    let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
    let mut application = bootstrap
        .authorize_and_mount_with_provider(Arc::new(LiveGlmProviderPlugin))
        .unwrap();
    let config = ModelConfig {
        preset: Some("glm-5.3-flash".into()),
        overrides: crate::model::ModelOverrides {
            output_limit: crate::Override::Set(512),
            ..crate::model::ModelOverrides::default()
        },
        overrides_version: Some(1),
        ..ModelConfig::default()
    };
    application
        .save_model_state(
            &config,
            &crate::model::ProviderCredentials::for_protocol(config.protocol),
        )
        .unwrap();

    let result = run(
        &mut application,
        "You must call view_image exactly once with project_relative_path set to live-green.png. Inspect the returned image, then reply exactly VIEW_IMAGE_OK_GREEN and nothing else.",
    )
    .expect("real GLM completes the typed view_image loop");
    assert!(
        result.output.contains("VIEW_IMAGE_OK_GREEN"),
        "live model must ground its final answer in the viewed image: {}",
        result.output
    );
    application.close().unwrap();

    let events = load_events(&storage_root);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "tool/call" && event.data["name"] == "view_image")
            .count(),
        1,
        "the live model must choose exactly one view_image call"
    );
    let result_event = events
        .iter()
        .find(|event| {
            event.event_type == "tool/result" && !event.data["isError"].as_bool().unwrap_or(false)
        })
        .expect("successful live view_image result is durable");
    let durable = serde_json::to_string(result_event).unwrap();
    assert!(durable.contains("\"type\":\"image\""));
    assert!(!durable.contains(project_root.to_string_lossy().as_ref()));
    assert!(
        result_event.data["message"]["content"][0]["content"]
            .as_array()
            .is_some_and(|blocks| blocks.iter().all(|block| block.get("path").is_none())),
        "typed result blocks remain ref-only even though the user-visible display name is durable"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// MM-5 paid long-history gate: seed an auditable journal through the normal
/// Application path, cold-remount it with the process-local GLM provider, and
/// force the next image turn across a deliberately small context window. The
/// same run must persist a real-model compaction family before GLM consumes
/// the retained image-bearing surface.
#[test]
#[ignore = "paid GLM auto-compaction/image run; set CLAT_GLM_CODING_PLAN_KEY explicitly"]
fn live_glm_auto_compacts_long_history_before_an_image_turn() {
    if std::env::var_os("CLAT_GLM_CODING_PLAN_KEY").is_none() {
        eprintln!("live GLM auto-compaction gate not armed; skipping");
        return;
    }
    let (storage_root, project_root) = roots("mm5-live-auto-compaction");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);

    // Seed through admission + Run + journal, but without paid requests. No
    // max-context value means the deterministic phase cannot compact early.
    let mut seeder = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&seeder);
    for turn in 0..10 {
        let prompt = format!(
            "LIVE_COMPACTION_SEED_{turn}: {}",
            char::from(b'a' + (turn % 26) as u8)
                .to_string()
                .repeat(3_800)
        );
        run(&mut seeder, &prompt).expect("seed turn");
    }
    seeder.close().unwrap();
    assert!(
        !load_events(&storage_root)
            .iter()
            .any(|event| event.event_type == "compaction/summary"),
        "the seed phase must leave uncompacted source history"
    );

    let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
    let mut application = bootstrap
        .authorize_and_mount_with_provider(Arc::new(LiveGlmProviderPlugin))
        .unwrap();
    let config = ModelConfig {
        preset: Some("glm-5.3-flash".into()),
        output_limit: Some(512),
        max_context_tokens: Some(12_000),
        overrides: crate::model::ModelOverrides {
            output_limit: crate::Override::Set(512),
            max_context_tokens: crate::Override::Set(12_000),
            ..crate::model::ModelOverrides::default()
        },
        overrides_version: Some(1),
        ..ModelConfig::default()
    };
    application
        .save_model_state(
            &config,
            &crate::model::ProviderCredentials::for_protocol(config.protocol),
        )
        .unwrap();
    let (effective, persisted_credentials) = application.model_state().unwrap();
    assert_eq!(effective.max_context_tokens, Some(12_000));
    assert!(
        persisted_credentials
            .values()
            .iter()
            .all(|value| value.is_empty()),
        "the paid test key remains process-local"
    );

    let green = project_root.join("post-compaction-green.png");
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        128,
        128,
        image::Rgb([0, 220, 0]),
    ))
    .save_with_format(&green, image::ImageFormat::Png)
    .unwrap();
    let done = run_with_attachments(
        &mut application,
        "Inspect the newest attached solid-color image. Reply exactly LIVE_COMPACTION_OK_GREEN and nothing else.",
        vec![green],
    )
    .expect("real GLM run after automatic compaction");
    assert!(
        done.output
            .to_ascii_uppercase()
            .contains("LIVE_COMPACTION_OK_GREEN"),
        "the retained image turn reaches the live provider after compaction: {}",
        done.output
    );
    application.close().unwrap();

    let events = load_events(&storage_root);
    let family = events
        .windows(4)
        .find(|window| {
            window[0].event_type == "compaction/start"
                && window[1].event_type == "compaction/summary"
                && window[2].event_type == "user/message"
                && window[3].event_type == "compaction/end"
        })
        .expect("real GLM compaction family is durable and contiguous");
    assert_eq!(family[1].data["provider"], json!("OpenAI Compatible"));
    assert_eq!(family[1].data["model"], json!("glm-5.3-flash"));
    assert!(
        family[2].surface_op.is_some(),
        "the live summary replaces the selected source prefix"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

struct TextCatalogScript;

impl TestModelScript for TextCatalogScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        assert!(
            request.tools.iter().all(|tool| tool.name != "view_image"),
            "text-only model catalogs must not mention view_image"
        );
        Ok(ViewImageLoopScript::completed("text only", events))
    }
}

#[test]
fn text_only_model_catalog_hides_view_image() {
    let (storage_root, project_root) = roots("mm2-view-image-text-gate");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Scripted(Arc::new(TextCatalogScript)),
    );
    let config = ModelConfig {
        model: "deterministic".into(),
        endpoint: "https://application-test.invalid".into(),
        ..ModelConfig::default()
    };
    application
        .save_model_state(
            &config,
            &crate::model::ProviderCredentials::for_protocol(config.protocol),
        )
        .unwrap();
    run(&mut application, "text request").expect("text-only run");
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}
