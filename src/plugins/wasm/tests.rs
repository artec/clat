/// A1：decide 携带 `&CancelToken` 后的具名 allow-all（闭包有 HRTB
/// 推断限制）。
fn allow_all_approver(
    _request: crate::permission::PermissionRequest,
    _cancel: &crate::model::CancelToken,
) -> PermissionDecision {
    PermissionDecision::Allow
}

struct AllowHostPolicy;

impl crate::permission::PermissionPolicy for AllowHostPolicy {
    fn check(
        &self,
        _project: &crate::project::Project,
        _tool: &crate::tool::ToolDefinition,
        _call: &crate::tool::ToolCall,
    ) -> PermissionDecision {
        PermissionDecision::Allow
    }
}

struct AllowHostPolicyFactory;

impl crate::plugins::services::PermissionPolicyFactory for AllowHostPolicyFactory {
    fn create(
        &self,
        _approver: std::sync::Arc<dyn PermissionApprover>,
        _cancel: &CancelToken,
    ) -> Box<dyn crate::permission::PermissionPolicy> {
        Box::new(AllowHostPolicy)
    }
}

use super::grants as wasm_grants;
use super::*;
use std::time::Duration;

#[test]
fn config_path_resolution_expands_tilde_and_relatives() {
    let root = std::path::Path::new("/home/user/.clat");
    assert_eq!(
        resolve_component_path(root, "~/plugins/d.wasm"),
        PathBuf::from("/home/user/plugins/d.wasm")
    );
    assert_eq!(
        resolve_component_path(root, "plugins/d.wasm"),
        PathBuf::from("/home/user/.clat/plugins/d.wasm")
    );
    assert_eq!(
        resolve_component_path(root, "/opt/d.wasm"),
        PathBuf::from("/opt/d.wasm")
    );
}

#[test]
fn missing_config_file_means_no_plugins() {
    let root = std::env::temp_dir().join("clat-wasm-missing-config");
    std::fs::create_dir_all(&root).expect("root");
    assert!(load_wasm_config(&root).expect("load").is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn malformed_config_file_fails_fast() {
    let root = std::env::temp_dir().join("clat-wasm-bad-config");
    std::fs::create_dir_all(&root).expect("root");
    std::fs::write(root.join("plugins.json"), b"{not json").expect("write");
    assert!(load_wasm_config(&root).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn manifest_package_resolves_relative_entry_identity_digest_and_prompts() {
    let root = unique_root("manifest-resolve");
    let package = root.join("package");
    std::fs::create_dir_all(&package).expect("package");
    std::fs::copy(fixture("digest.wasm"), package.join("plugin.wasm")).expect("component");
    let digest = fixture_sha256("digest.wasm");
    let manifest_path = package.join("clat-plugin.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec(&serde_json::json!({
            "manifestVersion": 1,
            "id": "dev.clat.digest",
            "name": "Digest",
            "version": "1.2.3",
            "runtime": {
                "kind": "wasm-component",
                "entry": "plugin.wasm",
                "sha256": digest,
            },
            "capabilities": { "tools": true, "prompts": true },
            "prompts": [{ "name": "digest", "system": "Prefer concise digests." }],
        }))
        .expect("manifest json"),
    )
    .expect("manifest");
    let config = WasmPluginConfig {
        manifest: Some(manifest_path.display().to_string()),
        ..WasmPluginConfig::default()
    };
    let resolved = resolve_package(&root, "dev.clat.digest", &config).expect("resolve");
    assert_eq!(
        resolved.path,
        package
            .join("plugin.wasm")
            .canonicalize()
            .expect("canonical entry")
    );
    assert_eq!(resolved.version.as_deref(), Some("1.2.3"));
    assert_eq!(resolved.prompts[0].system, "Prefer concise digests.");
    assert!(resolve_package(&root, "wrong.id", &config).is_err());

    let mut escape: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read manifest"))
            .expect("parse manifest");
    escape["runtime"]["entry"] = serde_json::json!("../escape.wasm");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec(&escape).expect("escape json"),
    )
    .expect("escape manifest");
    assert!(resolve_package(&root, "dev.clat.digest", &config).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn manifest_package_contributes_static_system_prompt() {
    let root = unique_root("manifest-prompt");
    let package = root.join("package");
    std::fs::create_dir_all(&package).expect("package");
    std::fs::copy(fixture("digest.wasm"), package.join("plugin.wasm")).expect("component");
    let manifest_path = package.join("clat-plugin.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec(&serde_json::json!({
            "manifestVersion": 1,
            "id": "dev.clat.digest",
            "name": "Digest",
            "version": "1.2.3",
            "runtime": {
                "kind": "wasm-component",
                "entry": "plugin.wasm",
                "sha256": fixture_sha256("digest.wasm"),
            },
            "capabilities": { "tools": true, "prompts": true },
            "prompts": [{ "name": "digest", "system": "Prefer concise digests." }],
        }))
        .expect("manifest json"),
    )
    .expect("manifest");
    write_config_json(
        &root,
        &[(
            "dev.clat.digest".to_owned(),
            serde_json::json!({ "manifest": manifest_path.display().to_string() }),
        )],
    );
    let mcp_root = unique_root("manifest-prompt-mcp");
    let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
        std::sync::Arc::new(ToolRegistryPlugin),
        std::sync::Arc::new(McpAdapterPlugin::new(
            mcp_root.clone(),
            Vec::new(),
            crate::plugin_host::PluginHostBridge::shared(),
        )),
        std::sync::Arc::new(crate::plugins::PromptRegistryPlugin),
        std::sync::Arc::new(WasmAdapterPlugin::new(
            root.clone(),
            crate::plugin_host::PluginHostBridge::shared(),
            root.clone(),
            None,
        )),
    ];
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    manager.mount_all(catalog).expect("mount package");
    let prompts = manager
        .require(crate::plugins::services::PROMPT_SERVICE)
        .expect("prompt registry");
    assert_eq!(prompts.instructions(), "Prefer concise digests.");
    let status = manager.require(MCP_STATUS_SERVICE).expect("status");
    assert!(
        status
            .snapshot()
            .servers
            .iter()
            .any(|server| { server.name == "dev.clat.digest" && server.server_version == "1.2.3" })
    );
    manager.close().expect("close");
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(mcp_root);
}

#[test]
fn installed_wasm_package_enters_the_normal_adapter_pipeline() {
    let storage = unique_root("installed-wasm");
    let package = unique_root("installed-wasm-source");
    std::fs::copy(fixture("digest.wasm"), package.join("plugin.wasm")).expect("component");
    std::fs::write(
        package.join("clat-plugin.json"),
        serde_json::to_vec(&serde_json::json!({
            "manifestVersion": 1,
            "id": "dev.clat.installed-digest",
            "name": "Installed Digest",
            "version": "1.0.0",
            "runtime": {
                "kind": "wasm-component",
                "entry": "plugin.wasm",
                "sha256": fixture_sha256("digest.wasm"),
            },
            "capabilities": { "tools": true },
        }))
        .expect("manifest"),
    )
    .expect("write manifest");
    let bad_package = unique_root("installed-wasm-bad-source");
    std::fs::copy(fixture("digest.wasm"), bad_package.join("plugin.wasm")).expect("bad component");
    std::fs::write(
        bad_package.join("clat-plugin.json"),
        serde_json::to_vec(&serde_json::json!({
            "manifestVersion": 1,
            "id": "dev.clat.installed-bad",
            "name": "Installed Bad",
            "version": "1.0.0",
            "runtime": {
                "kind": "wasm-component",
                "entry": "plugin.wasm",
                "sha256": fixture_sha256("digest.wasm"),
            },
            "capabilities": { "tools": true },
        }))
        .expect("bad manifest"),
    )
    .expect("write bad manifest");
    let bad_digest;
    {
        let mut store = crate::plugin::PackageStore::open(&storage).expect("store");
        store
            .install(&package, None, true, crate::plugin::InstallKind::Install)
            .expect("install");
        bad_digest = store
            .install(
                &bad_package,
                None,
                true,
                crate::plugin::InstallKind::Install,
            )
            .expect("install bad peer before tamper")
            .tree_sha256;
    }
    std::fs::write(
        storage
            .join("plugin-store/artifacts/dev.clat.installed-bad")
            .join(&bad_digest)
            .join("tampered"),
        "tampered",
    )
    .expect("tamper one installed package");
    let mcp_root = unique_root("installed-wasm-mcp");
    let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
        std::sync::Arc::new(ToolRegistryPlugin),
        std::sync::Arc::new(McpAdapterPlugin::new(
            mcp_root.clone(),
            Vec::new(),
            crate::plugin_host::PluginHostBridge::shared(),
        )),
        std::sync::Arc::new(WasmAdapterPlugin::new(
            storage.clone(),
            crate::plugin_host::PluginHostBridge::shared(),
            storage.clone(),
            None,
        )),
    ];
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    manager.mount_all(catalog).expect("mount installed package");
    let registry = manager.require(TOOL_SERVICE).expect("tools");
    let output = registry
        .get("wasm_dev_clat_installed_digest_digest")
        .expect("installed wasm tool")
        .invoke(
            &serde_json::json!({"op": "sha256", "text": "abc"}),
            &crate::project::Project::new(&storage),
            &CancelToken::new(),
        )
        .expect("invoke");
    assert_eq!(
        output["sha256"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(
        registry.get("wasm_dev_clat_installed_bad_digest").is_none(),
        "the tampered peer must not register"
    );
    let status = manager.require(MCP_STATUS_SERVICE).expect("status");
    assert!(
        status
            .snapshot()
            .failures
            .iter()
            .any(|failure| failure.contains("dev.clat.installed-bad")),
        "{:?}",
        status.snapshot()
    );
    manager.close().expect("close");
    std::fs::remove_dir_all(storage).expect("cleanup storage");
    std::fs::remove_dir_all(package).expect("cleanup package");
    std::fs::remove_dir_all(bad_package).expect("cleanup bad package");
    std::fs::remove_dir_all(mcp_root).expect("cleanup mcp");
}

// ---- 门控端到端（需要 tests/fixtures/wasm/ 的组件；本地构建插件
// 需 wasm32-wasip2 target，见 plugins/digest 与 plugins/probe）。

use super::super::services::{MCP_STATUS_SERVICE, PROVIDER_SERVICE, TOOL_SERVICE};
use crate::interaction::{AskAnswer, AskQuestion, UserAsker};
use crate::model::{
    CancelToken, FinishReason, Model, ModelConfig, ModelError, ModelEventSink, ModelFactory,
    ModelProtocol, ModelRequest, ModelResponse, ProviderCredentials, ProviderDescriptor,
    Usage as ModelUsage,
};
use crate::permission::{PermissionApprover, PermissionDecision};
use crate::plugin::{PluginId, PluginManager, PluginOwner, ScopeKind};
use crate::plugins::{McpAdapterPlugin, ProviderRegistryPlugin, ToolRegistryPlugin};

/// INV-G1 纯函数：授予 = min(档位, 插件能力上限, 写授予门裁决)。
#[test]
fn grant_matrix_maps_mode_and_capability() {
    use crate::permission::PermissionMode;
    let cell = std::sync::Arc::new(std::sync::RwLock::new(PermissionMode::ReadOnly));
    let policy = |fs_cap: bool, extra: Vec<PathBuf>| GrantPolicy {
        mode: Some(std::sync::Arc::clone(&cell)),
        project_root: PathBuf::from("/proj"),
        fs_cap,
        extra_dirs: extra,
    };
    let mode = |expected: PermissionMode| {
        *cell.write().expect("mode") = expected;
        Some(expected)
    };

    // Read Only：恒 RO（即使插件声明了写工具）。
    let grants = policy(true, Vec::new()).grants(Some(PermissionMode::ReadOnly), true);
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].guest, "project");
    assert!(!grants[0].read_write);

    // Project Write：有 fs 上限 → RW；纯读插件 → RO。
    mode(PermissionMode::ProjectWrite);
    assert!(
        policy(true, Vec::new()).grants(Some(PermissionMode::ProjectWrite), true)[0].read_write
    );
    assert!(
        !policy(false, Vec::new()).grants(Some(PermissionMode::ProjectWrite), true)[0].read_write
    );

    // Full Access：项目根 RW + 额外目录 RW（guest = 清洗目录名）。
    mode(PermissionMode::FullAccess);
    let grants = policy(
        true,
        vec![
            PathBuf::from("/Volumes/Data 1"),
            PathBuf::from("/x/project"),
        ],
    )
    .grants(Some(PermissionMode::FullAccess), true);
    assert_eq!(grants.len(), 3);
    assert!(grants.iter().all(|grant| grant.read_write));
    assert_eq!(grants[1].guest, "Data-1");
    assert_eq!(
        grants[2].guest, "dir1",
        "collision with `project` falls back"
    );
    // 纯读插件在 FA 也不得 RW/额外目录。
    let grants =
        policy(false, vec![PathBuf::from("/extra")]).grants(Some(PermissionMode::FullAccess), true);
    assert_eq!(grants.len(), 1);
    assert!(!grants[0].read_write);

    // B5（INV-W2）：写授予门未过（无记录且未获批/被拒）→ 即使档位
    // 与 fs_cap 都满足，一切 preopen 物理只读。
    mode(PermissionMode::ProjectWrite);
    let grants = policy(true, Vec::new()).grants(Some(PermissionMode::ProjectWrite), false);
    assert_eq!(grants.len(), 1);
    assert!(!grants[0].read_write);
    mode(PermissionMode::FullAccess);
    let grants =
        policy(true, vec![PathBuf::from("/extra")]).grants(Some(PermissionMode::FullAccess), false);
    assert_eq!(
        grants.len(),
        1,
        "extras are part of the write grant; without it only the project root mounts, read-only"
    );
    assert!(!grants[0].read_write);

    // B5：write_dirs = 审批面（档位 × fs_cap 求值，与 grants 的 RW
    // 分支同源）。RO/Classic/纯读 → 空；PW → [根]；FA → [根+extras]。
    mode(PermissionMode::ReadOnly);
    assert!(
        policy(true, vec![PathBuf::from("/extra")])
            .write_dirs(Some(PermissionMode::ReadOnly))
            .is_empty()
    );
    assert!(
        policy(false, vec![PathBuf::from("/extra")])
            .write_dirs(Some(PermissionMode::ProjectWrite))
            .is_empty()
    );
    assert_eq!(
        policy(true, Vec::new()).write_dirs(Some(PermissionMode::ProjectWrite)),
        vec![PathBuf::from("/proj")]
    );
    mode(PermissionMode::FullAccess);
    assert_eq!(
        policy(true, vec![PathBuf::from("/extra")]).write_dirs(Some(PermissionMode::FullAccess)),
        vec![PathBuf::from("/proj"), PathBuf::from("/extra")]
    );
    assert!(
        GrantPolicy {
            mode: None,
            project_root: PathBuf::from("/proj"),
            fs_cap: true,
            extra_dirs: vec![PathBuf::from("/extra")],
        }
        .write_dirs(None)
        .is_empty()
    );

    // Classic（exec，无档位 cell）：恒 RO。
    let grants = GrantPolicy {
        mode: None,
        project_root: PathBuf::from("/proj"),
        fs_cap: true,
        extra_dirs: vec![PathBuf::from("/extra")],
    }
    .grants(None, true);
    assert_eq!(grants.len(), 1);
    assert!(!grants[0].read_write);
}

/// INV-G1/G3 门控：read 插件在 RO 档读得到、写被拒；切 PW 后同一
/// 挂载重建实例可写；FA 档额外目录按 guest 名可写。
#[test]
#[ignore = "loads the wasm fixture; run explicitly with --ignored"]
fn fs_grants_follow_the_permission_mode_and_rebuild() {
    use crate::permission::PermissionMode;
    let root = unique_root("fs");
    let project = unique_root("fs-project");
    std::fs::write(project.join("hello.txt"), b"hi wasm").expect("seed");
    let extra = unique_root("fs-extra");
    let extra_guest = extra
        .file_name()
        .and_then(|name| name.to_str())
        .expect("extra name")
        .to_owned();
    let map: BTreeMap<String, WasmPluginConfig> = BTreeMap::from([(
        "read".to_owned(),
        WasmPluginConfig {
            path: fixture("read.wasm").display().to_string(),
            manifest: None,
            dirs: vec![extra.display().to_string()],
            config: None,
            sha256: None,
        },
    )]);
    std::fs::write(
        root.join("plugins.json"),
        serde_json::to_vec(&map).expect("serialize"),
    )
    .expect("config");
    // B5：本测试不装桥上下文（无审批面）→ 写授予门 fail-closed。
    // 成功腿预置一份覆盖 FA 全集（根 + extra）的授权记录：RO 腿
    // 依旧失败（记录永不越过档位），PW/FA 腿静默 RW——本测试聚焦
    // 档位 × 物理授予，审批语义由下方 write_grants_* 四条钉住。
    let mut records = Vec::new();
    wasm_grants::upsert(
        &mut records,
        "read",
        &fixture_sha256("read.wasm"),
        &[project.clone(), extra.clone()],
    );
    wasm_grants::save_grants(&wasm_grants::grants_path(&root), &records).expect("seed grants");

    let mode = std::sync::Arc::new(std::sync::RwLock::new(PermissionMode::ReadOnly));
    let mcp_root = unique_root("fs-mcp");
    let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
        std::sync::Arc::new(ToolRegistryPlugin),
        std::sync::Arc::new(McpAdapterPlugin::new(
            mcp_root.clone(),
            Vec::new(),
            crate::plugin_host::PluginHostBridge::shared(),
        )),
        std::sync::Arc::new(WasmAdapterPlugin::new(
            root.clone(),
            crate::plugin_host::PluginHostBridge::shared(),
            project.clone(),
            Some(std::sync::Arc::clone(&mode)),
        )),
    ];
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    manager.mount_all(catalog).expect("mount");
    let registry = manager.require(TOOL_SERVICE).expect("registry");
    let project_view = crate::project::Project::new(&project);
    let cancel = CancelToken::new();

    // RO：读放行（对齐原生读工具），写被 capability 边界拒绝。
    let output = registry
        .get("wasm_read_read_file")
        .expect("tool")
        .invoke(
            &serde_json::json!({"path": "hello.txt"}),
            &project_view,
            &cancel,
        )
        .expect("read under Read Only");
    assert_eq!(output["content"], "hi wasm");
    let output = registry
        .get("wasm_read_list_dir")
        .expect("tool")
        .invoke(&serde_json::json!({"path": ""}), &project_view, &cancel)
        .expect("list under Read Only");
    assert!(
        output["entries"]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| entry["name"] == "hello.txt")),
        "unexpected listing: {output}"
    );
    let _error = registry
        .get("wasm_read_write_file")
        .expect("tool")
        .invoke(
            &serde_json::json!({"path": "out.txt", "content": "denied"}),
            &project_view,
            &cancel,
        )
        .expect_err("write must be refused under Read Only");
    assert!(
        !project.join("out.txt").exists(),
        "no file may escape a read-only grant"
    );

    // PW：同一挂载下档位变更 → 实例重建 → 项目根可写。
    *mode.write().expect("mode") = PermissionMode::ProjectWrite;
    registry
        .get("wasm_read_write_file")
        .expect("tool")
        .invoke(
            &serde_json::json!({"path": "out.txt", "content": "pw"}),
            &project_view,
            &cancel,
        )
        .expect("write after mode switch");
    assert_eq!(
        std::fs::read_to_string(project.join("out.txt")).expect("written"),
        "pw"
    );

    // FA：额外目录按 guest 名可写（显式 /<根>/ 寻址）。
    *mode.write().expect("mode") = PermissionMode::FullAccess;
    let output = registry
        .get("wasm_read_write_file")
        .expect("tool")
        .invoke(
            &serde_json::json!({
                "path": format!("/{extra_guest}/note.txt"),
                "content": "fa",
            }),
            &project_view,
            &cancel,
        )
        .expect("write into the extra grant");
    assert_eq!(output["bytes"], 2);
    assert_eq!(
        std::fs::read_to_string(extra.join("note.txt")).expect("extra"),
        "fa"
    );

    manager.close().expect("close");
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(project);
    let _ = std::fs::remove_dir_all(extra);
    let _ = std::fs::remove_dir_all(mcp_root);
}

// ---- B5：写授予审批化（INV-W1..W6；验收①—④判别测试）。

/// 组件 fixture 的 sha256（与 compile_plugin 的无条件摘要同一算法）。
fn fixture_sha256(name: &str) -> String {
    use sha2::Digest as _;
    let bytes = std::fs::read(fixture(name)).expect("fixture bytes");
    sha2::Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 记录所见审批请求、按固定脚本裁决的 approver（B5 判别测试）。
struct ScriptedGrantApprover {
    seen: Mutex<Vec<crate::permission::PermissionRequest>>,
    verdict: PermissionDecision,
}

impl ScriptedGrantApprover {
    fn with_verdict(verdict: PermissionDecision) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            verdict,
        })
    }

    fn seen_count(&self) -> usize {
        self.seen.lock().expect("seen log").len()
    }
}

impl PermissionApprover for ScriptedGrantApprover {
    fn decide(
        &self,
        request: crate::permission::PermissionRequest,
        _cancel: &CancelToken,
    ) -> PermissionDecision {
        self.seen.lock().expect("seen log").push(request);
        self.verdict.clone()
    }
}

/// 挂载 read 插件（带档位 cell）；桥由调用方自持——不装上下文
/// 即无审批面（INV-W6 的 fail-closed 腿）。
fn mount_read_plugin(
    root: &std::path::Path,
    mcp_root: &std::path::Path,
    project: &std::path::Path,
    mode: crate::permission::PermissionMode,
    bridge: std::sync::Arc<crate::plugin_host::PluginHostBridge>,
) -> (
    PluginManager,
    std::sync::Arc<std::sync::RwLock<crate::permission::PermissionMode>>,
) {
    write_config(
        root,
        &[("read", fixture("read.wasm").display().to_string())],
    );
    let cell = std::sync::Arc::new(std::sync::RwLock::new(mode));
    let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
        std::sync::Arc::new(ToolRegistryPlugin),
        std::sync::Arc::new(McpAdapterPlugin::new(
            mcp_root.to_owned(),
            Vec::new(),
            crate::plugin_host::PluginHostBridge::shared(),
        )),
        std::sync::Arc::new(WasmAdapterPlugin::new(
            root.to_owned(),
            bridge,
            project.to_owned(),
            Some(std::sync::Arc::clone(&cell)),
        )),
    ];
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    manager.mount_all(catalog).expect("mount");
    (manager, cell)
}

/// 装一个带指定 approver 的桥上下文（等价 start_run 的 install；
/// 每次 install 分配新纪元）。
fn install_grant_context(
    bridge: &crate::plugin_host::PluginHostBridge,
    approver: std::sync::Arc<dyn PermissionApprover>,
) {
    bridge.install(crate::plugin_host::RunHostContext {
        providers: fake_providers(),
        model_config: ModelConfig {
            model: "fake-model".into(),
            ..Default::default()
        },
        credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
        approver,
        permission_mode: None,
        asker: None,
        cancel: CancelToken::new(),
        usage_cell: std::sync::Arc::new(Mutex::new(ModelUsage::default())),
        budget: std::sync::Arc::new(Mutex::new(crate::plugin_host::SamplingBudget::per_run())),
    });
}

/// 验收①（INV-W1/W3）：首调弹一次审批（参数列出目录集与组件
/// 摘要）→ Allow 落记录；新 run（新纪元）同 hash 静默授予。
/// pre-fix 判别力：无写授予门——0 次审批、无记录文件、写静默成功
///（见实施补记的门删除复验）。
#[test]
#[ignore = "loads the wasm fixture; run explicitly with --ignored"]
fn write_grants_ask_once_then_persist_silence_for_the_same_hash() {
    use crate::permission::PermissionMode;
    let root = unique_root("grant-ask");
    let project = unique_root("grant-ask-project");
    let mcp_root = unique_root("grant-ask-mcp");
    let bridge = crate::plugin_host::PluginHostBridge::shared();
    let (mut manager, _cell) = mount_read_plugin(
        &root,
        &mcp_root,
        &project,
        PermissionMode::ProjectWrite,
        std::sync::Arc::clone(&bridge),
    );
    let registry = manager.require(TOOL_SERVICE).expect("registry");
    let project_view = crate::project::Project::new(&project);
    let cancel = CancelToken::new();
    let approver = ScriptedGrantApprover::with_verdict(PermissionDecision::Allow);
    install_grant_context(&bridge, approver.clone());

    // 首调：恰好一问，审批面 = 实际授予面（目录集 + 组件摘要）。
    registry
        .get("wasm_read_write_file")
        .expect("tool")
        .invoke(
            &serde_json::json!({"path": "out.txt", "content": "granted"}),
            &project_view,
            &cancel,
        )
        .expect("write after an explicit grant");
    {
        let seen = approver.seen.lock().expect("seen log");
        assert_eq!(seen.len(), 1, "exactly one approval per plugin per run");
        assert_eq!(seen[0].tool, "wasm:read");
        assert_eq!(seen[0].effect, crate::tool::ToolEffect::Write);
        assert_eq!(
            seen[0].arguments["component_sha256"],
            fixture_sha256("read.wasm")
        );
        assert_eq!(
            seen[0].arguments["write_dirs"],
            serde_json::json!([project.display().to_string()])
        );
    }
    assert_eq!(
        std::fs::read_to_string(project.join("out.txt")).expect("written"),
        "granted"
    );

    // 记录落盘（三要素齐全）。
    let records = wasm_grants::load_grants(&wasm_grants::grants_path(&root));
    assert!(wasm_grants::covers(
        &records,
        "read",
        &fixture_sha256("read.wasm"),
        std::slice::from_ref(&project)
    ));

    // 新 run（新纪元）：有效记录 → 0 问、写照常。
    bridge.clear();
    let silent_approver = ScriptedGrantApprover::with_verdict(PermissionDecision::Allow);
    install_grant_context(&bridge, silent_approver.clone());
    registry
        .get("wasm_read_write_file")
        .expect("tool")
        .invoke(
            &serde_json::json!({"path": "out2.txt", "content": "silent"}),
            &project_view,
            &cancel,
        )
        .expect("write with a valid record stays silent");
    assert_eq!(
        silent_approver.seen_count(),
        0,
        "a valid record must not re-ask"
    );

    manager.close().expect("close");
    bridge.clear();
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(project);
    let _ = std::fs::remove_dir_all(mcp_root);
}

/// 验收②（INV-W1）：组件 hash 失配的记录不覆盖 → 重问。
/// pre-fix 判别力：无门——0 问且写静默成功。
#[test]
#[ignore = "loads the wasm fixture; run explicitly with --ignored"]
fn write_grants_reask_when_the_component_hash_changes() {
    use crate::permission::PermissionMode;
    let root = unique_root("grant-stale");
    let project = unique_root("grant-stale-project");
    let mcp_root = unique_root("grant-stale-mcp");
    // 预置 hash 不符的记录（目录集是对的也不行——三要素缺一不可）。
    let mut records = Vec::new();
    wasm_grants::upsert(
        &mut records,
        "read",
        &"f".repeat(64),
        std::slice::from_ref(&project),
    );
    wasm_grants::save_grants(&wasm_grants::grants_path(&root), &records)
        .expect("seed stale record");

    let bridge = crate::plugin_host::PluginHostBridge::shared();
    let (mut manager, _cell) = mount_read_plugin(
        &root,
        &mcp_root,
        &project,
        PermissionMode::ProjectWrite,
        std::sync::Arc::clone(&bridge),
    );
    let registry = manager.require(TOOL_SERVICE).expect("registry");
    let approver = ScriptedGrantApprover::with_verdict(PermissionDecision::Allow);
    install_grant_context(&bridge, approver.clone());
    registry
        .get("wasm_read_write_file")
        .expect("tool")
        .invoke(
            &serde_json::json!({"path": "out.txt", "content": "re-asked"}),
            &crate::project::Project::new(&project),
            &CancelToken::new(),
        )
        .expect("write after re-asking");
    assert_eq!(
        approver.seen_count(),
        1,
        "a stale-hash record must not silently cover"
    );

    manager.close().expect("close");
    bridge.clear();
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(project);
    let _ = std::fs::remove_dir_all(mcp_root);
}

/// 验收③（INV-W2/W3）：Deny → 本 run 物理只读（写工具失败、磁盘
/// 无文件）、不落记录、同 run 不再问；下一 run（新纪元）重问。
#[test]
#[ignore = "loads the wasm fixture; run explicitly with --ignored"]
fn write_grants_denial_downgrades_to_read_only_for_the_run() {
    use crate::permission::PermissionMode;
    let root = unique_root("grant-deny");
    let project = unique_root("grant-deny-project");
    let mcp_root = unique_root("grant-deny-mcp");
    let bridge = crate::plugin_host::PluginHostBridge::shared();
    let (mut manager, _cell) = mount_read_plugin(
        &root,
        &mcp_root,
        &project,
        PermissionMode::ProjectWrite,
        std::sync::Arc::clone(&bridge),
    );
    let registry = manager.require(TOOL_SERVICE).expect("registry");
    let project_view = crate::project::Project::new(&project);
    let cancel = CancelToken::new();
    let approver = ScriptedGrantApprover::with_verdict(PermissionDecision::Deny {
        reason: "not today".into(),
    });
    install_grant_context(&bridge, approver.clone());

    // 拒绝 → 物理只读：写失败、磁盘无文件。
    let _error = registry
        .get("wasm_read_write_file")
        .expect("tool")
        .invoke(
            &serde_json::json!({"path": "out.txt", "content": "denied"}),
            &project_view,
            &cancel,
        )
        .expect_err("denied write grant must fail physically");
    assert!(
        !project.join("out.txt").exists(),
        "no file may escape a read-only grant"
    );
    assert_eq!(approver.seen_count(), 1);

    // 同 run 二次调用：不重复问，依旧只读。
    let _error = registry
        .get("wasm_read_write_file")
        .expect("tool")
        .invoke(
            &serde_json::json!({"path": "out.txt", "content": "still denied"}),
            &project_view,
            &cancel,
        )
        .expect_err("denial sticks for the run");
    assert_eq!(approver.seen_count(), 1, "no re-ask within the same run");

    // 不落记录。
    let records = wasm_grants::load_grants(&wasm_grants::grants_path(&root));
    assert!(!wasm_grants::covers(
        &records,
        "read",
        &fixture_sha256("read.wasm"),
        std::slice::from_ref(&project)
    ));

    // 下一 run（新纪元）重问（per-run 拒绝语义）。
    bridge.clear();
    let approver2 = ScriptedGrantApprover::with_verdict(PermissionDecision::Deny {
        reason: "still no".into(),
    });
    install_grant_context(&bridge, approver2.clone());
    let _error = registry
        .get("wasm_read_write_file")
        .expect("tool")
        .invoke(
            &serde_json::json!({"path": "out.txt", "content": "denied again"}),
            &project_view,
            &cancel,
        )
        .expect_err("denied again");
    assert_eq!(approver2.seen_count(), 1, "a new run must re-ask");

    manager.close().expect("close");
    bridge.clear();
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(project);
    let _ = std::fs::remove_dir_all(mcp_root);
}

/// 验收④（INV-W2/W6）：无活动 run（headless 无桥上下文）→
/// fail-closed 只读；Unavailable（exec 非交互先例）同样拒写——
/// 两腿都不落记录、不 panic。
#[test]
#[ignore = "loads the wasm fixture; run explicitly with --ignored"]
fn write_grants_fail_closed_without_an_interactive_run() {
    use crate::permission::PermissionMode;
    let root = unique_root("grant-headless");
    let project = unique_root("grant-headless-project");
    let mcp_root = unique_root("grant-headless-mcp");
    let bridge = crate::plugin_host::PluginHostBridge::shared();
    let (mut manager, _cell) = mount_read_plugin(
        &root,
        &mcp_root,
        &project,
        PermissionMode::ProjectWrite,
        std::sync::Arc::clone(&bridge),
    );
    let registry = manager.require(TOOL_SERVICE).expect("registry");
    let project_view = crate::project::Project::new(&project);
    let cancel = CancelToken::new();

    // 腿一：无桥上下文（boot/mount 期、run 间隙）——无审批面即无
    // 写授予。
    let _error = registry
        .get("wasm_read_write_file")
        .expect("tool")
        .invoke(
            &serde_json::json!({"path": "out.txt", "content": "headless"}),
            &project_view,
            &cancel,
        )
        .expect_err("no active run means no write grant");
    assert!(!project.join("out.txt").exists());

    // 腿二：Unavailable（exec NonInteractive 形态）——有审批面但
    // fail-closed。
    let approver = ScriptedGrantApprover::with_verdict(PermissionDecision::Unavailable {
        reason: "non-interactive run denied `wasm:read`".into(),
    });
    install_grant_context(&bridge, approver.clone());
    let _error = registry
        .get("wasm_read_write_file")
        .expect("tool")
        .invoke(
            &serde_json::json!({"path": "out.txt", "content": "unavailable"}),
            &project_view,
            &cancel,
        )
        .expect_err("unavailable approval means no write grant");
    assert!(!project.join("out.txt").exists());
    assert_eq!(approver.seen_count(), 1);

    // 两腿都不落记录。
    let records = wasm_grants::load_grants(&wasm_grants::grants_path(&root));
    assert!(records.is_empty());

    manager.close().expect("close");
    bridge.clear();
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(project);
    let _ = std::fs::remove_dir_all(mcp_root);
}

/// INV-K2/K4 门控：greeter 经 SDK DSL 声明，config 从 plugins.json
/// 流入组件；未配置时报错而非静默空值。
#[test]
#[ignore = "loads the wasm fixture; run explicitly with --ignored"]
fn greeter_reads_its_config_through_the_sdk() {
    fn mount_greeter(
        root: &std::path::Path,
        mcp_root: &std::path::Path,
        config: Option<serde_json::Value>,
    ) -> PluginManager {
        let map: BTreeMap<String, WasmPluginConfig> = BTreeMap::from([(
            "greeter".to_owned(),
            WasmPluginConfig {
                path: fixture("greeter.wasm").display().to_string(),
                manifest: None,
                dirs: Vec::new(),
                config,
                sha256: None,
            },
        )]);
        std::fs::write(
            root.join("plugins.json"),
            serde_json::to_vec(&map).expect("serialize"),
        )
        .expect("config");
        let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
            std::sync::Arc::new(ToolRegistryPlugin),
            std::sync::Arc::new(McpAdapterPlugin::new(
                mcp_root.to_owned(),
                Vec::new(),
                crate::plugin_host::PluginHostBridge::shared(),
            )),
            std::sync::Arc::new(WasmAdapterPlugin::new(
                root.to_owned(),
                crate::plugin_host::PluginHostBridge::shared(),
                root.to_owned(),
                None,
            )),
        ];
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager.mount_all(catalog).expect("mount");
        manager
    }

    // 配置流入：greeting + upper 生效。
    let root = unique_root("greeter");
    let mcp_root = unique_root("greeter-mcp");
    let mut manager = mount_greeter(
        &root,
        &mcp_root,
        Some(serde_json::json!({ "greeting": "Hola", "upper": true })),
    );
    let registry = manager.require(TOOL_SERVICE).expect("registry");
    let project = crate::project::Project::new(&root);
    let output = registry
        .get("wasm_greeter_greet")
        .expect("tool")
        .invoke(
            &serde_json::json!({"name": "clat"}),
            &project,
            &CancelToken::new(),
        )
        .expect("greet");
    assert_eq!(output["greeting"], "Hola, CLAT!");
    manager.close().expect("close");
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(mcp_root);

    // 未配置：报错而非静默（INV-K2）。
    let root = unique_root("greeter-nocfg");
    let mcp_root = unique_root("greeter-nocfg-mcp");
    let mut manager = mount_greeter(&root, &mcp_root, None);
    let registry = manager.require(TOOL_SERVICE).expect("registry");
    let project = crate::project::Project::new(&root);
    let error = registry
        .get("wasm_greeter_greet")
        .expect("tool")
        .invoke(
            &serde_json::json!({"name": "clat"}),
            &project,
            &CancelToken::new(),
        )
        .expect_err("unconfigured plugin must fail loudly");
    assert!(
        error.to_string().contains("no config provided"),
        "unexpected error: {error}"
    );
    manager.close().expect("close");
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(mcp_root);
}

/// INV-W1/G2 门控（2d 测试化）：网络全地址拒绝、环境变量不可达、
/// 内存 256MB 上限——三条沙箱声明从"文档声称"升为测试钉住。
#[test]
#[ignore = "loads the wasm fixture; run explicitly with --ignored"]
fn sandbox_claims_are_pinned_by_tests() {
    let root = unique_root("sandbox");
    let mcp_root = unique_root("sandbox-mcp");
    let bridge = crate::plugin_host::PluginHostBridge::shared();
    let mut manager = mount_probe(&root, &mcp_root, bridge, CALL_FUEL);
    let registry = manager.require(TOOL_SERVICE).expect("registry");
    let project = crate::project::Project::new(&root);
    let cancel = CancelToken::new();

    // 网络：连接必须失败（sockets 可建、全地址拒绝）。
    let output = registry
        .get("wasm_probe_net")
        .expect("tool")
        .invoke(
            &serde_json::json!({"host": "127.0.0.1", "port": 9}),
            &project,
            &cancel,
        )
        .expect("net probe returns data, not a trap");
    assert_eq!(
        output["connected"], false,
        "no address may be reachable from a plugin: {output}"
    );
    assert!(
        output["error"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "the denial must be observable: {output}"
    );

    // 环境变量：可见集必须为空（宿主环境不透传）。
    let output = registry
        .get("wasm_probe_env")
        .expect("tool")
        .invoke(&serde_json::json!({}), &project, &cancel)
        .expect("env probe");
    assert_eq!(
        output["count"], 0,
        "no host environment variable may leak into a plugin: {output}"
    );

    // 内存：撑到 256MB 上限必须 trap 成工具错误（及时返回）。
    let started = std::time::Instant::now();
    let _error = registry
        .get("wasm_probe_alloc")
        .expect("tool")
        .invoke(&serde_json::json!({}), &project, &cancel)
        .expect_err("the memory cap must trap the allocation loop");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "memory-cap trap must be prompt: {:?}",
        started.elapsed()
    );

    manager.close().expect("close");
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(mcp_root);
}

/// INV-W3（fuel 修订）的正面对照：等人的时间不烧燃料——小额燃料
/// 下 elicitation 慢应答（2×1s 睡眠）照常成功，同额燃料跑死循环
/// 立刻耗尽。epoch 语义（壁钟刻度）在此测试上会红。
#[test]
#[ignore = "loads the wasm fixture; run explicitly with --ignored"]
fn waiting_for_the_user_does_not_burn_fuel() {
    struct SlowAsker {
        asked: Mutex<u32>,
    }
    impl UserAsker for SlowAsker {
        fn ask(&self, _question: AskQuestion, _cancel: &CancelToken) -> AskAnswer {
            std::thread::sleep(Duration::from_secs(1));
            let mut asked = self.asked.lock().expect("asker state");
            *asked += 1;
            // probe 表单两字段：flavor（choice）→ vanilla；servings
            //（number）→ 2。
            if *asked == 1 {
                AskAnswer::Selected("vanilla".into())
            } else {
                AskAnswer::Custom("2".into())
            }
        }
    }

    let root = unique_root("fuelwait");
    let mcp_root = unique_root("fuelwait-mcp");
    let bridge = crate::plugin_host::PluginHostBridge::shared();
    // 小额燃料：够 probe 的胶水逻辑，绝不够秒级死循环。
    let mut manager = mount_probe(&root, &mcp_root, std::sync::Arc::clone(&bridge), 20_000_000);
    let registry = manager.require(TOOL_SERVICE).expect("registry");

    bridge.install(crate::plugin_host::RunHostContext {
        providers: fake_providers(),
        model_config: crate::model::ModelConfig {
            model: "fake-model".into(),
            ..Default::default()
        },
        credentials: crate::model::ProviderCredentials::for_protocol(
            crate::model::ModelProtocol::OpenAiCompatible,
        ),
        approver: std::sync::Arc::new(allow_all_approver) as std::sync::Arc<dyn PermissionApprover>,
        permission_mode: None,
        asker: Some(std::sync::Arc::new(SlowAsker {
            asked: Mutex::new(0),
        })),
        cancel: CancelToken::new(),
        usage_cell: std::sync::Arc::new(Mutex::new(ModelUsage::default())),
        budget: std::sync::Arc::new(Mutex::new(crate::plugin_host::SamplingBudget::per_run())),
    });

    let project = crate::project::Project::new(&root);
    let output = registry
        .get("wasm_probe_probe")
        .expect("tool")
        .invoke(
            &serde_json::json!({"elicit": true}),
            &project,
            &CancelToken::new(),
        )
        .expect("elicitation wait must not consume fuel");
    assert!(
        output["elicit"]["flavor"].is_string() || output["elicit"].is_string(),
        "unexpected output: {output}"
    );

    // 对照：同额燃料，真实执行立刻耗尽。
    let _error = registry
        .get("wasm_probe_spin")
        .expect("tool")
        .invoke(&serde_json::json!({}), &project, &CancelToken::new())
        .expect_err("the same fuel must exhaust on real execution");

    manager.close().expect("close");
    bridge.clear();
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(mcp_root);
}

fn fixture(name: &str) -> PathBuf {
    let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("tests/fixtures/wasm")
        .join(name);
    assert!(path.is_file(), "missing fixture: {}", path.display());
    path
}

fn write_config(root: &std::path::Path, entries: &[(&str, String)]) {
    write_config_json(
        root,
        &entries
            .iter()
            .map(|(name, path)| {
                (
                    (*name).to_owned(),
                    serde_json::json!({ "path": path.clone() }),
                )
            })
            .collect::<Vec<_>>(),
    );
}

fn write_config_json(root: &std::path::Path, entries: &[(String, serde_json::Value)]) {
    let map: BTreeMap<String, WasmPluginConfig> = entries
        .iter()
        .map(|(name, value)| {
            (
                (*name).to_owned(),
                serde_json::from_value(value.clone()).expect("config shape"),
            )
        })
        .collect();
    std::fs::write(
        root.join("plugins.json"),
        serde_json::to_vec(&map).expect("serialize"),
    )
    .expect("write config");
}

fn unique_root(tag: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("clat-wasm-{tag}-{unique}"));
    std::fs::create_dir_all(&root).expect("root");
    root
}

/// INV-W2/W6：digest 注册为 `wasm_digest_digest`、状态面板呈现
/// transport `wasm`；teardown 撤销 lease（close 后注册表为空）。
#[test]
#[ignore = "loads the wasm fixture; run explicitly with --ignored"]
fn digest_plugin_registers_invokes_and_revokes() {
    let root = unique_root("digest");
    write_config(
        &root,
        &[("digest", fixture("digest.wasm").display().to_string())],
    );
    // 空 MCP 根：状态服务由 McpAdapter 提供（settled 立即）。
    let mcp_root = unique_root("digest-mcp");

    let bridge = crate::plugin_host::PluginHostBridge::shared();
    let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
        std::sync::Arc::new(ToolRegistryPlugin),
        std::sync::Arc::new(McpAdapterPlugin::new(
            mcp_root.clone(),
            Vec::new(),
            crate::plugin_host::PluginHostBridge::shared(),
        )),
        std::sync::Arc::new(WasmAdapterPlugin::new(
            root.clone(),
            bridge,
            root.clone(),
            None,
        )),
    ];
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    manager.mount_all(catalog).expect("mount");
    let registry = manager.require(TOOL_SERVICE).expect("registry");
    let status = manager.require(MCP_STATUS_SERVICE).expect("status");

    let definitions = registry.definitions();
    assert_eq!(
        definitions
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["wasm_digest_digest"],
        "the digest tool must be registered under its qualified name"
    );
    assert_eq!(definitions[0].effect, ToolEffect::Pure);
    assert!(definitions[0].description.contains("[wasm:digest]"));

    let snapshot = status.snapshot();
    assert_eq!(snapshot.configured, 1, "wasm entries extend the panel");
    assert_eq!(snapshot.connected, 1);
    assert_eq!(snapshot.servers[0].transport, "wasm");
    assert_eq!(snapshot.servers[0].tools, 1);
    assert_eq!(snapshot.servers[0].protocol_version, WIT_PROTOCOL);

    // 调用：sha256 / base64 两条真实路径。
    let project = crate::project::Project::new(&root);
    let cancel = CancelToken::new();
    let tool = registry.get("wasm_digest_digest").expect("tool handle");
    let output = tool
        .invoke(
            &serde_json::json!({"op": "sha256", "text": "abc"}),
            &project,
            &cancel,
        )
        .expect("sha256");
    assert_eq!(
        output["sha256"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let output = tool
        .invoke(
            &serde_json::json!({"op": "base64-encode", "text": "hello"}),
            &project,
            &cancel,
        )
        .expect("base64");
    assert_eq!(output["base64"], "aGVsbG8=");
    // 组件内错误按工具错误返回（run 不死）。
    let error = tool
        .invoke(&serde_json::json!({"op": "sha256"}), &project, &cancel)
        .expect_err("missing text must fail");
    assert!(!error.to_string().is_empty());

    manager.close().expect("close project scope");
    assert!(
        registry.is_empty(),
        "wasm tool lease was not revoked on teardown"
    );
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(mcp_root);
}

/// INV-W5：坏组件只隔离自己——digest 照常注册，坏条目进状态面板。
#[test]
#[ignore = "loads the wasm fixture; run explicitly with --ignored"]
fn broken_component_is_isolated() {
    let root = unique_root("isolation");
    let garbage = root.join("garbage.wasm");
    std::fs::write(&garbage, b"definitely not a component").expect("write garbage");
    write_config(
        &root,
        &[
            ("digest", fixture("digest.wasm").display().to_string()),
            ("garbage", garbage.display().to_string()),
        ],
    );
    let mcp_root = unique_root("isolation-mcp");
    let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
        std::sync::Arc::new(ToolRegistryPlugin),
        std::sync::Arc::new(McpAdapterPlugin::new(
            mcp_root.clone(),
            Vec::new(),
            crate::plugin_host::PluginHostBridge::shared(),
        )),
        std::sync::Arc::new(WasmAdapterPlugin::new(
            root.clone(),
            crate::plugin_host::PluginHostBridge::shared(),
            root.clone(),
            None,
        )),
    ];
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    manager.mount_all(catalog).expect("mount");
    let registry = manager.require(TOOL_SERVICE).expect("registry");
    let status = manager.require(MCP_STATUS_SERVICE).expect("status");
    assert_eq!(
        registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>(),
        ["wasm_digest_digest"],
        "the good plugin must survive the broken one"
    );
    let snapshot = status.snapshot();
    assert_eq!(snapshot.configured, 2);
    assert_eq!(snapshot.connected, 1);
    assert!(
        snapshot
            .failures
            .iter()
            .any(|message| message.contains("wasm `garbage`"))
    );
    manager.close().expect("close");
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(mcp_root);
}

struct FakeFactory;

impl ModelFactory for FakeFactory {
    fn protocol(&self) -> ModelProtocol {
        ModelProtocol::OpenAiCompatible
    }

    fn describe(&self, _credentials: &ProviderCredentials) -> ProviderDescriptor {
        unimplemented!("not needed for wasm plugin tests")
    }

    fn build(
        &self,
        _config: &ModelConfig,
        _credentials: &ProviderCredentials,
    ) -> Result<Box<dyn Model>, ModelError> {
        Ok(Box::new(FakeModel))
    }
}

struct FakeModel;

impl Model for FakeModel {
    fn provider(&self) -> &str {
        "wasm-test-fake"
    }

    fn model_id(&self) -> &str {
        "fake-model"
    }

    fn stream(
        &mut self,
        _request: ModelRequest<'_>,
        _events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        Ok(ModelResponse {
            text: "fake".into(),
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Completed,
            usage: Some(ModelUsage {
                input_tokens: 11,
                output_tokens: 4,
                ..ModelUsage::default()
            }),
            provider_response_id: None,
            provider_state: Vec::new(),
            reasoning: None,
        })
    }
}

fn fake_providers() -> std::sync::Arc<crate::plugins::services::ProviderRegistry> {
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    manager
        .mount_all(vec![
            std::sync::Arc::new(ToolRegistryPlugin),
            std::sync::Arc::new(ProviderRegistryPlugin),
        ])
        .expect("mount");
    let providers = manager.require(PROVIDER_SERVICE).expect("providers");
    providers
        .register(
            PluginOwner::for_test(PluginId::new("test.wasm_plugins")),
            std::sync::Arc::new(FakeFactory),
        )
        .expect("register fake factory");
    providers
}

/// 桥的假 asker：按脚本逐字段作答（vanilla / 2）。
struct ScriptedAsker {
    answers: Mutex<std::collections::VecDeque<AskAnswer>>,
}

impl UserAsker for ScriptedAsker {
    fn ask(&self, _question: AskQuestion, _cancel: &CancelToken) -> AskAnswer {
        self.answers
            .lock()
            .expect("asker script")
            .pop_front()
            .expect("scripted answer exhausted")
    }
}

fn mount_probe(
    root: &std::path::Path,
    mcp_root: &std::path::Path,
    bridge: std::sync::Arc<crate::plugin_host::PluginHostBridge>,
    fuel: u64,
) -> PluginManager {
    write_config(
        root,
        &[("probe", fixture("probe.wasm").display().to_string())],
    );
    let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
        std::sync::Arc::new(ToolRegistryPlugin),
        std::sync::Arc::new(McpAdapterPlugin::new(
            mcp_root.to_owned(),
            Vec::new(),
            crate::plugin_host::PluginHostBridge::shared(),
        )),
        std::sync::Arc::new(
            WasmAdapterPlugin::new(root.to_owned(), bridge, root.to_owned(), None).with_fuel(fuel),
        ),
    ];
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    manager.mount_all(catalog).expect("mount");
    manager
}

/// INV-W4：probe 经 WIT 调 sampling（过权限门 + 计账）与
/// elicitation（顺序单问）——与 MCP 路径同一桥。
#[test]
#[ignore = "loads the wasm fixture; run explicitly with --ignored"]
fn probe_round_trips_sampling_and_elicitation_through_the_bridge() {
    let root = unique_root("probe");
    let mcp_root = unique_root("probe-mcp");
    let bridge = crate::plugin_host::PluginHostBridge::shared();
    let usage_cell = std::sync::Arc::new(Mutex::new(ModelUsage::default()));

    let mut manager = mount_probe(&root, &mcp_root, std::sync::Arc::clone(&bridge), CALL_FUEL);
    let registry = manager.require(TOOL_SERVICE).expect("registry");
    std::fs::write(root.join("host.txt"), "from host\n").expect("host fixture");
    let read_lease = registry
        .register(
            PluginOwner::for_test(PluginId::new("test.wasm_host_read")),
            std::sync::Arc::new(crate::native_tools::ReadFileTool),
        )
        .expect("register native read");
    bridge.configure_host_services(
        crate::project::Project::new(&root),
        std::sync::Arc::clone(&registry),
        std::sync::Arc::new(crate::tool::ToolExecutionPipeline::new()),
        std::sync::Arc::new(AllowHostPolicyFactory),
    );

    // 安装桥上下文（等价 start_run 的 install）：fake 模型 + Allow
    // 审批 + 脚本 asker。
    let providers = fake_providers();
    let config = ModelConfig {
        model: "fake-model".into(),
        ..ModelConfig::default()
    };
    bridge.install(crate::plugin_host::RunHostContext {
        providers,
        model_config: config,
        credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
        approver: std::sync::Arc::new(allow_all_approver) as std::sync::Arc<dyn PermissionApprover>,
        permission_mode: None,
        asker: Some(std::sync::Arc::new(ScriptedAsker {
            answers: Mutex::new(
                vec![
                    AskAnswer::Selected("vanilla".into()),
                    AskAnswer::Custom("2".into()),
                ]
                .into(),
            ),
        })),
        cancel: CancelToken::new(),
        usage_cell: std::sync::Arc::clone(&usage_cell),
        budget: std::sync::Arc::new(Mutex::new(crate::plugin_host::SamplingBudget::per_run())),
    });
    bridge.update_run_metadata(
        "wasm-host-session",
        &[crate::model::ModelItem::user_text("hi")],
    );

    let project = crate::project::Project::new(&root);
    let output = registry
        .get("wasm_probe_probe")
        .expect("probe tool")
        .invoke(
            &serde_json::json!({
                "sample": true,
                "elicit": true,
                "context": true,
                "read_path": "host.txt",
                "text": "hi"
            }),
            &project,
            &CancelToken::new(),
        )
        .expect("probe output");
    assert_eq!(output["sampling"]["model"], "fake-model");
    assert_eq!(output["sampling"]["text"], "fake");
    assert_eq!(output["elicit"]["flavor"], "vanilla");
    // WIT 的 number 是 f64（契约层形状）：整数经往返变 2.0——
    // INV-W4 允许的 wire 层差异，语义等价。
    assert_eq!(output["elicit"]["servings"].as_f64(), Some(2.0));
    assert_eq!(output["context"]["run"]["sessionId"], "wasm-host-session");
    assert_eq!(output["host_read"]["content"], "1 | from host\n");
    read_lease.revoke().expect("revoke native read");
    manager.close().expect("close");
    bridge.clear();
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(mcp_root);
}

/// INV-W1（fail-closed 复用）：无 run 上下文时，probe 的 sampling
/// 拿到 no-active-run 错误（elicitation 直接使工具失败）。
#[test]
#[ignore = "loads the wasm fixture; run explicitly with --ignored"]
fn probe_without_a_run_context_fails_closed() {
    let root = unique_root("norun");
    let mcp_root = unique_root("norun-mcp");
    let bridge = crate::plugin_host::PluginHostBridge::shared();
    let mut manager = mount_probe(&root, &mcp_root, std::sync::Arc::clone(&bridge), CALL_FUEL);
    let registry = manager.require(TOOL_SERVICE).expect("registry");
    let project = crate::project::Project::new(&root);
    let cancel = CancelToken::new();
    let output = registry
        .get("wasm_probe_probe")
        .expect("probe tool")
        .invoke(&serde_json::json!({"sample": true}), &project, &cancel)
        .expect("sampling error is data, not a trap");
    assert!(
        output["sampling"]["error"]
            .as_str()
            .is_some_and(|message| message.contains("no active run")),
        "unexpected output: {output}"
    );
    let error = registry
        .get("wasm_probe_probe")
        .expect("probe tool")
        .invoke(&serde_json::json!({"elicit": true}), &project, &cancel)
        .expect_err("elicitation without a frontend must fail the tool");
    assert!(
        error.to_string().contains("no active run"),
        "unexpected error: {error}"
    );
    manager.close().expect("close");
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(mcp_root);
}

/// INV-W3：死循环组件被燃料预算打断成工具错误（小额燃料下秒
/// 级返回，run 不死）。
#[test]
#[ignore = "loads the wasm fixture; run explicitly with --ignored"]
fn spinning_component_is_interrupted_by_the_fuel_budget() {
    let root = unique_root("spin");
    let mcp_root = unique_root("spin-mcp");
    let bridge = crate::plugin_host::PluginHostBridge::shared();
    // 小额燃料：死循环很快耗尽 → trap（无壁钟 ticker，2d 起为 fuel）。
    let mut manager = mount_probe(&root, &mcp_root, bridge, 5_000_000);
    let registry = manager.require(TOOL_SERVICE).expect("registry");
    let project = crate::project::Project::new(&root);
    let started = std::time::Instant::now();
    let _error = registry
        .get("wasm_probe_spin")
        .expect("spin tool")
        .invoke(&serde_json::json!({}), &project, &CancelToken::new())
        .expect_err("spin must be interrupted");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "interruption must be prompt: {:?}",
        started.elapsed()
    );
    // 具体文案随 wasmtime 版本变化（v48 是 wasm backtrace 形
    // 态）；不变量是"及时被打断成工具错误"，不锁实现文案。

    manager.close().expect("close");
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(mcp_root);
}

/// W1-01：取消令牌是执行期能力。全额燃料（纯执行 ≈百秒量级）下，
/// spin 组件开始执行后触发同一个 `CancelToken`，invoke 必须在秒级
/// 返回"被中断"的工具错误——而不是等燃料耗尽。pre-fix 红：`_cancel`
/// 被忽略，本测试要跑满燃料预算（分钟级）且报 fuel trap 文案。
#[test]
#[ignore = "loads the wasm fixture; run explicitly with --ignored"]
fn spinning_component_is_interrupted_by_the_run_cancel_token() {
    let root = unique_root("cancel");
    let mcp_root = unique_root("cancel-mcp");
    let bridge = crate::plugin_host::PluginHostBridge::shared();
    let mut manager = mount_probe(&root, &mcp_root, bridge, CALL_FUEL);
    let registry = manager.require(TOOL_SERVICE).expect("registry");
    let cancel = CancelToken::new();
    let worker_cancel = cancel.clone();
    let worker_root = root.clone();

    let started = std::time::Instant::now();
    let invoke = std::thread::spawn(move || {
        registry
            .get("wasm_probe_spin")
            .expect("spin tool")
            .invoke(
                &serde_json::json!({}),
                &crate::project::Project::new(&worker_root),
                &worker_cancel,
            )
            .expect_err("a cancelled spin must fail, not return")
            .to_string()
    });
    // 等组件确实进入执行（略宽于挂载 + 实例化），再 Esc。
    std::thread::sleep(Duration::from_millis(500));
    cancel.cancel();
    let message = invoke.join().expect("invoke thread");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "cancellation must interrupt promptly (fuel alone would burn for ~a minute): {:?}",
        started.elapsed()
    );
    assert!(
        message.contains("interrupted"),
        "the error must attribute the trap to cancellation: {message}"
    );

    manager.close().expect("close");
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(mcp_root);
}

// ---- W1-10：有界时钟订阅（monotonic-clock 宿主直达） ----

/// 测试用 PluginState（不挂组件，宿主函数可直接调用）。
fn clock_state(cancel: &CancelToken) -> PluginState {
    PluginState {
        bridge: crate::plugin_host::PluginHostBridge::shared(),
        source: PluginSource::Wasm("clock-test".into()),
        limits: StoreLimitsBuilder::new().memory_size(MEMORY_LIMIT).build(),
        wasi: WasiCtxBuilder::new().build(),
        table: ResourceTable::new(),
        config: None,
        capabilities: None,
        clock: ClockShared::begin_invoke(cancel),
    }
}

#[test]
fn manifest_capability_ceiling_guards_every_host_import() {
    use clat::plugin::elicitation::Host as _;
    use clat::plugin::host::Host as _;
    use clat::plugin::sampling::Host as _;

    let cancel = CancelToken::new();
    let mut state = clock_state(&cancel);
    state.capabilities = Some(PluginCapabilities::default());

    let sampling = state
        .create_message(clat::plugin::sampling::Request {
            system_prompt: None,
            messages: Vec::new(),
            max_tokens: 1,
            temperature: None,
        })
        .expect_err("sampling must be manifest-gated");
    assert!(sampling.contains("capability `sampling`"));

    let elicitation = state
        .elicit(clat::plugin::elicitation::Form {
            message: "question".into(),
            fields: Vec::new(),
        })
        .expect_err("elicitation must be manifest-gated");
    assert!(elicitation.contains("capability `elicitation`"));

    let context = state.context().expect_err("context must be manifest-gated");
    assert!(context.contains("capability `hostContext`"));

    let call = state
        .call_tool("read_file".into(), "{}".into())
        .expect_err("host tools must be manifest-gated");
    assert!(call.contains("capability `hostTools.read_file`"));

    state.capabilities = Some(PluginCapabilities {
        sampling: true,
        elicitation: true,
        host_context: true,
        host_tools: vec!["read_file".into()],
        ..PluginCapabilities::default()
    });
    let permitted_but_unavailable = state
        .call_tool("read_file".into(), "{}".into())
        .expect_err("the detached test bridge has no active run");
    assert!(!permitted_but_unavailable.contains("does not declare capability"));
}

/// 经同步 poll 宿主驱动一个订阅的就绪（同步 `wasi:io/poll` 的
/// Host 直接实现在 ResourceTable 上）。
/// 返回就绪列表长度（wasi 0.3 语义：poll 返回就绪 pollable 的
/// 下标列表；单订阅就绪即 `[0]`）。
fn poll_ready(state: &mut PluginState, pollable: Resource<DynPollable>) -> usize {
    use wasmtime_wasi::p2::bindings::sync::io::poll::Host as _;
    ResourceTable::poll(&mut state.table, vec![pollable])
        .expect("poll")
        .len()
}

/// W1-10 判别：`u64::MAX` 时长（wasmtime-wasi 默认实现落
/// `Deadline::Never` 永久阻塞）的订阅在取消令牌置位后必须秒级
/// 返回控制——epoch/燃料防线因此恢复可达。若回归到默认实现，
/// 本测试会挂死在 poll 上（默认 `pending().await` 不可中断），
/// 即为红。
#[test]
fn never_tier_clock_wait_is_interruptible_by_the_cancel_token() {
    let cancel = CancelToken::new();
    let mut state = clock_state(&cancel);
    let pollable =
        monotonic_clock::Host::subscribe_duration(&mut state, u64::MAX).expect("subscribe");
    cancel.cancel();
    let started = Instant::now();
    let ready = poll_ready(&mut state, pollable);
    assert_eq!(ready, 1, "the cancelled sleep reports ready");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "cancel must reach the wait within a bounded interval: {:?}",
        started.elapsed()
    );
}

/// 调用级预算耗尽后的新订阅立即就绪（组件回到执行点，忙转由
/// 燃料收尾）——恶意组件不能用"N 个小睡眠"或单个超长睡眠拖垮
/// 调用。
#[test]
fn clock_wait_budget_exhaustion_makes_new_subscriptions_ready_at_once() {
    let cancel = CancelToken::new();
    let mut state = clock_state(&cancel);
    state.clock.exhaust_for_test();
    let pollable =
        monotonic_clock::Host::subscribe_duration(&mut state, u64::MAX).expect("subscribe");
    let started = Instant::now();
    let ready = poll_ready(&mut state, pollable);
    assert_eq!(ready, 1);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "an exhausted budget must not wait: {:?}",
        started.elapsed()
    );
}

/// 合法短睡眠的时长语义保持：不提前（切片只中断不缩短——
/// ready 内部循环补满剩余时长）、不明显拖后。
#[test]
fn short_clock_waits_keep_their_duration_semantics() {
    let cancel = CancelToken::new();
    let mut state = clock_state(&cancel);
    // 600ms：跨两个 250ms 切片，验证片间续等。
    let pollable =
        monotonic_clock::Host::subscribe_duration(&mut state, 600_000_000).expect("subscribe");
    let started = Instant::now();
    let ready = poll_ready(&mut state, pollable);
    let elapsed = started.elapsed();
    assert_eq!(ready, 1);
    assert!(
        elapsed >= Duration::from_millis(500) && elapsed < Duration::from_secs(5),
        "a legal sleep keeps its duration (got {elapsed:?})"
    );
}

/// A4-3（W1-20）：组件文件大小闸——超 32MiB 拒载（加载前判定，
/// 不进编译器）。pre-fix 红：垃圾大文件走到 compile 错误（不同文案）。
#[test]
fn oversized_components_are_refused_before_compilation() {
    let root = unique_root("oversize");
    let big = root.join("big.wasm");
    let mut blob = vec![0u8; 33 * 1024 * 1024];
    blob[..9].copy_from_slice(b"garbage!!");
    std::fs::write(&big, &blob).expect("write big garbage");
    write_config(&root, &[("big", big.display().to_string())]);
    let mcp_root = unique_root("oversize-mcp");
    let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
        std::sync::Arc::new(ToolRegistryPlugin),
        std::sync::Arc::new(McpAdapterPlugin::new(
            mcp_root.clone(),
            Vec::new(),
            crate::plugin_host::PluginHostBridge::shared(),
        )),
        std::sync::Arc::new(WasmAdapterPlugin::new(
            root.clone(),
            crate::plugin_host::PluginHostBridge::shared(),
            root.clone(),
            None,
        )),
    ];
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    manager.mount_all(catalog).expect("mount");
    let status = manager.require(MCP_STATUS_SERVICE).expect("status");
    let snapshot = status.snapshot();
    assert!(
        snapshot
            .failures
            .iter()
            .any(|message| message.contains("32 MiB")),
        "the size cap must refuse the load: {snapshot:?}"
    );
    manager.close().expect("close");
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(mcp_root);
}

/// A4-3：sha256 钉扎——失配拒载，匹配放行。
#[test]
fn sha256_pins_are_verified_at_load() {
    use sha2::Digest as _;
    let root = unique_root("pinning");
    let component = fixture("digest.wasm");
    let digest_hex = {
        let bytes = std::fs::read(&component).expect("fixture");
        let digest = sha2::Sha256::digest(&bytes);
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    // 失配：错误钉扎拒载（好组件也不放行）。
    write_config_json(
        &root,
        &[(
            "digest".to_owned(),
            serde_json::json!({
                "path": component.display().to_string(),
                "sha256": "deadbeef".to_owned(),
            }),
        )],
    );
    let mcp_root = unique_root("pinning-mcp");
    let mount_and_status = || {
        let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
            std::sync::Arc::new(ToolRegistryPlugin),
            std::sync::Arc::new(McpAdapterPlugin::new(
                mcp_root.clone(),
                Vec::new(),
                crate::plugin_host::PluginHostBridge::shared(),
            )),
            std::sync::Arc::new(WasmAdapterPlugin::new(
                root.clone(),
                crate::plugin_host::PluginHostBridge::shared(),
                root.clone(),
                None,
            )),
        ];
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager.mount_all(catalog).expect("mount");
        let status = manager.require(MCP_STATUS_SERVICE).expect("status");
        (manager, status)
    };
    let (mut manager, status) = mount_and_status();
    let snapshot = status.snapshot();
    assert!(
        snapshot
            .failures
            .iter()
            .any(|message| message.contains("sha256")),
        "a mismatched pin must refuse the load: {snapshot:?}"
    );
    assert_eq!(snapshot.connected, 0);
    manager.close().expect("close");

    // 匹配：正确钉扎放行。
    write_config_json(
        &root,
        &[(
            "digest".to_owned(),
            serde_json::json!({
                "path": component.display().to_string(),
                "sha256": digest_hex,
            }),
        )],
    );
    let (mut manager, status) = mount_and_status();
    let snapshot = status.snapshot();
    assert_eq!(
        snapshot.connected, 1,
        "a matching pin must load the plugin: {snapshot:?}"
    );
    manager.close().expect("close");
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(mcp_root);
}

/// A4-2（W1-19）：元数据消毒——超长 description 截断并记诊断；
/// 非法 input_schema 拒注册（不再静默降级为空 schema）。
#[test]
fn tool_metadata_is_sanitized_with_diagnostics() {
    use exports::clat::plugin::tools::{Definition, Effect};
    let long_description: String = "x".repeat(5000);
    let definitions = vec![
        Definition {
            name: "ok".into(),
            description: "fine".into(),
            input_schema: r#"{"type":"object"}"#.into(),
            effect: Effect::Pure,
        },
        Definition {
            name: "verbose".into(),
            description: long_description,
            input_schema: r#"{"type":"object"}"#.into(),
            effect: Effect::Pure,
        },
        Definition {
            name: "badschema".into(),
            description: "schema is broken".into(),
            input_schema: "{not json".into(),
            effect: Effect::Pure,
        },
    ];
    let (parsed, diagnostics) = sanitize_definitions("test", definitions);
    assert_eq!(parsed.len(), 2, "only the valid-schema tools register");
    assert!(
        parsed.iter().any(|definition| definition.name == "ok"
            && definition.description.contains("[wasm:test] fine")),
        "the healthy tool passes through: {parsed:?}"
    );
    let verbose = parsed
        .iter()
        .find(|definition| definition.name == "verbose")
        .expect("verbose still registers (truncated, not dropped)");
    assert!(
        verbose.description.chars().count() < 4200,
        "the description is truncated: {}",
        verbose.description.chars().count()
    );
    assert!(verbose.description.contains("[truncated by host]"));
    assert_eq!(
        diagnostics.len(),
        2,
        "one diagnostic per violation: {diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("verbose") && message.contains("truncated"))
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("badschema") && message.contains("not registered"))
    );
}
