use super::mcp_wire::{parse_elicitation_params, parse_sampling_params};
use super::*;
use crate::mcp::client::McpServerRequestHandler;
use crate::model::{Model, ModelError, ModelEventSink, ModelFactory, ModelProtocol, ModelResponse};
use crate::plugin::{PluginId, PluginManager, PluginOwner, ScopeKind};
use crate::plugins::ProviderRegistryPlugin;
use crate::plugins::services::PROVIDER_SERVICE;

struct AllowPolicy;

impl crate::permission::PermissionPolicy for AllowPolicy {
    fn check(
        &self,
        _project: &Project,
        _tool: &ToolDefinition,
        _call: &ToolCall,
    ) -> PermissionDecision {
        PermissionDecision::Allow
    }
}

struct AllowPolicyFactory;

impl PermissionPolicyFactory for AllowPolicyFactory {
    fn create(
        &self,
        _approver: Arc<dyn PermissionApprover>,
        _cancel: &CancelToken,
    ) -> Box<dyn crate::permission::PermissionPolicy> {
        Box::new(AllowPolicy)
    }
}

// ---- 假件：provider / approver / asker ----

struct CannedFactory {
    text: &'static str,
    usage: Option<Usage>,
}

impl ModelFactory for CannedFactory {
    fn protocol(&self) -> ModelProtocol {
        ModelProtocol::OpenAiCompatible
    }

    fn describe(&self, _credentials: &ProviderCredentials) -> crate::model::ProviderDescriptor {
        unimplemented!("not needed for plugin_host tests")
    }

    fn build(
        &self,
        _config: &ModelConfig,
        _credentials: &ProviderCredentials,
    ) -> Result<Box<dyn Model>, ModelError> {
        Ok(Box::new(CannedModel {
            text: self.text,
            usage: self.usage.clone(),
        }))
    }
}

struct CannedModel {
    text: &'static str,
    usage: Option<Usage>,
}

impl Model for CannedModel {
    fn provider(&self) -> &str {
        "plugin-host-fake"
    }

    fn model_id(&self) -> &str {
        "plugin-host-fake"
    }

    fn stream(
        &mut self,
        _request: ModelRequest<'_>,
        _events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        Ok(ModelResponse {
            text: self.text.into(),
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Completed,
            usage: self.usage.clone(),
            provider_response_id: None,
            provider_state: Vec::new(),
            reasoning: None,
        })
    }
}

/// 记录所见请求并按脚本应答的假 approver。
struct ScriptedApprover {
    decisions: Mutex<Vec<PermissionRequest>>,
    verdict: PermissionDecision,
}

impl PermissionApprover for ScriptedApprover {
    fn decide(&self, request: PermissionRequest, _cancel: &CancelToken) -> PermissionDecision {
        if let Ok(mut seen) = self.decisions.lock() {
            seen.push(request);
        }
        self.verdict.clone()
    }
}

/// 按脚本逐条作答的假 asker（每字段一次 ask）。
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

fn providers_with(factory: impl ModelFactory + 'static) -> Arc<ProviderRegistry> {
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    manager
        .mount_all(vec![Arc::new(ProviderRegistryPlugin)])
        .expect("mount");
    let providers = manager.require(PROVIDER_SERVICE).expect("providers");
    let _lease = providers
        .register(
            PluginOwner::for_test(PluginId::new("test.plugin_host")),
            Arc::new(factory),
        )
        .expect("register factory");
    // manager 在此丢弃（触发 close），providers/lease 的 Arc 存活到
    // 测试结束——title.rs 同款姿势。
    providers
}

fn installed_bridge(
    providers: Arc<ProviderRegistry>,
    approver: Arc<dyn PermissionApprover>,
    asker: Option<Arc<dyn UserAsker>>,
) -> (Arc<PluginHostBridge>, Arc<Mutex<Usage>>) {
    installed_bridge_with_mode(providers, approver, asker, None)
}

fn installed_bridge_with_mode(
    providers: Arc<ProviderRegistry>,
    approver: Arc<dyn PermissionApprover>,
    asker: Option<Arc<dyn UserAsker>>,
    permission_mode: Option<crate::permission::PermissionMode>,
) -> (Arc<PluginHostBridge>, Arc<Mutex<Usage>>) {
    installed_bridge_with(
        providers,
        approver,
        asker,
        permission_mode,
        SamplingBudget::per_run(),
    )
}

fn installed_bridge_with(
    providers: Arc<ProviderRegistry>,
    approver: Arc<dyn PermissionApprover>,
    asker: Option<Arc<dyn UserAsker>>,
    permission_mode: Option<crate::permission::PermissionMode>,
    budget: SamplingBudget,
) -> (Arc<PluginHostBridge>, Arc<Mutex<Usage>>) {
    let bridge = PluginHostBridge::shared();
    let usage_cell = Arc::new(Mutex::new(Usage::default()));
    let config = ModelConfig {
        model: "fake-model".into(),
        ..ModelConfig::default()
    };
    bridge.install(RunHostContext {
        providers,
        model_config: config,
        credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
        approver,
        permission_mode: permission_mode.map(|mode| Arc::new(std::sync::RwLock::new(mode))),
        asker,
        cancel: CancelToken::new(),
        usage_cell: Arc::clone(&usage_cell),
        budget: Arc::new(Mutex::new(budget)),
    });
    (bridge, usage_cell)
}

fn sampling_request() -> SamplingRequest {
    SamplingRequest {
        system_prompt: Some("be brief".into()),
        messages: vec![SamplingMessage {
            role: SamplingRole::User,
            text: "translate hi to french".into(),
        }],
        max_tokens: 64,
        stop_sequences: Vec::new(),
        temperature: None,
    }
}

// ---- INV-S1：无免费通道 ----

#[test]
fn sampling_and_elicitation_without_a_run_fail_closed() {
    let bridge = PluginHostBridge::shared();
    let error = bridge
        .sample(PluginSource::Mcp("srv".into()), sampling_request())
        .unwrap_err();
    assert!(matches!(error, PluginHostError::NoActiveRun));
    let error = bridge
        .elicit(ElicitForm {
            message: "hi".into(),
            fields: vec![],
        })
        .unwrap_err();
    assert!(matches!(error, PluginHostError::NoActiveRun));
}

#[test]
fn clear_uninstalls_the_context_between_runs() {
    let approver = Arc::new(ScriptedApprover {
        decisions: Mutex::new(Vec::new()),
        verdict: PermissionDecision::Allow,
    });
    let (bridge, _cell) = installed_bridge(
        providers_with(CannedFactory {
            text: "ok",
            usage: None,
        }),
        approver,
        None,
    );
    bridge.clear();
    assert!(matches!(
        bridge.sample(PluginSource::Mcp("srv".into()), sampling_request()),
        Err(PluginHostError::NoActiveRun)
    ));
}

#[test]
fn host_context_and_tool_call_share_run_scoped_permission_pipeline() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("clat-plugin-host-{unique}"));
    std::fs::create_dir_all(&root).expect("root");
    std::fs::write(root.join("note.txt"), "hello\n").expect("fixture");
    let approver = Arc::new(ScriptedApprover {
        decisions: Mutex::new(Vec::new()),
        verdict: PermissionDecision::Allow,
    });
    let (bridge, _cell) = installed_bridge(
        providers_with(CannedFactory {
            text: "ok",
            usage: None,
        }),
        approver,
        None,
    );
    let tools = Arc::new(ToolRegistry::new());
    let _lease = tools
        .register(
            PluginOwner::for_test(PluginId::new("test.host-tool")),
            Arc::new(crate::native_tools::ReadFileTool),
        )
        .expect("register read tool");
    bridge.configure_host_services(
        Project::new(&root),
        Arc::clone(&tools),
        Arc::new(ToolExecutionPipeline::new()),
        Arc::new(AllowPolicyFactory),
    );
    bridge.update_run_metadata("session-host", &[ModelItem::user_text("hello")]);

    let context = bridge.host_context().expect("host context");
    assert_eq!(context["run"]["sessionId"], "session-host");
    assert_eq!(context["run"]["messages"].as_array().map(Vec::len), Some(1));
    let output = bridge
        .call_host_tool(
            PluginSource::Wasm("fixture".into()),
            "read_file",
            json!({"path": "note.txt"}),
        )
        .expect("host read");
    assert_eq!(output["content"], "1 | hello\n");
    assert!(matches!(
        bridge.call_host_tool(PluginSource::Wasm("fixture".into()), "ask_user", json!({})),
        Err(PluginHostError::UnknownHostTool(_))
    ));
    let outside = std::env::temp_dir().join(format!("clat-plugin-host-outside-{unique}"));
    std::fs::write(&outside, "secret\n").expect("outside fixture");
    let error = bridge
        .call_host_tool(
            PluginSource::Wasm("fixture".into()),
            "read_file",
            json!({"path": outside}),
        )
        .expect_err("external plugins must not use ambient absolute reads");
    assert!(error.to_string().contains("outside project"));
    bridge.clear();
    assert!(matches!(
        bridge.host_context(),
        Err(PluginHostError::NoActiveRun)
    ));
    std::fs::remove_file(outside).expect("cleanup outside fixture");
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn host_context_drops_a_single_oversized_history_item() {
    let history = vec![ModelItem::user_text("x".repeat(HOST_CONTEXT_MAX_BYTES + 1))];
    assert!(bounded_history(&history).is_empty());
}

/// B7（C2 定案）：非空 stop_sequences → 一次/run 的诊断标志置位，
/// 同 run 第二次不再置；空值零触碰。判别面 = budget 标志（stderr
/// 输出为胶水）；pre-fix 判别力：标志字段不存在（前置编译不可达，
/// 按惯例文档化）+ 解析腿 `sampling_params_parse_stop_sequences`
/// 已前置红钉住解析缺口。
#[test]
fn stop_sequences_warning_fires_once_per_run() {
    let bridge = PluginHostBridge::shared();
    let usage_cell = Arc::new(Mutex::new(Usage::default()));
    let budget = Arc::new(Mutex::new(SamplingBudget::per_run()));
    bridge.install(RunHostContext {
        providers: providers_with(CannedFactory {
            text: "ok",
            usage: None,
        }),
        model_config: ModelConfig {
            model: "fake-model".into(),
            ..ModelConfig::default()
        },
        credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
        approver: Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        }),
        permission_mode: None,
        asker: None,
        cancel: CancelToken::new(),
        usage_cell: Arc::clone(&usage_cell),
        budget: Arc::clone(&budget),
    });
    let mut request = sampling_request();
    request.stop_sequences = vec!["\\n\\nUser:".into()];
    bridge
        .sample(PluginSource::Mcp("srv".into()), request)
        .expect("sample");
    assert!(
        budget.lock().expect("budget").stop_sequences_warned,
        "a non-empty stop_sequences list must set the once-per-run flag"
    );
    assert!(
        !budget.lock().expect("budget").warn_stop_sequences_once(),
        "the warning fires at most once per run"
    );
    // 空值零噪音：全新预算从未置位。
    assert!(!SamplingBudget::per_run().stop_sequences_warned);
}

// ---- INV-S2：过门 + 记账 ----

#[test]
fn sampling_passes_the_permission_gate_and_accounts_usage() {
    let approver = Arc::new(ScriptedApprover {
        decisions: Mutex::new(Vec::new()),
        verdict: PermissionDecision::Allow,
    });
    let (bridge, cell) = installed_bridge(
        providers_with(CannedFactory {
            text: "bonjour",
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 3,
                ..Usage::default()
            }),
        }),
        approver.clone(),
        None,
    );
    let outcome = bridge
        .sample(PluginSource::Mcp("srv".into()), sampling_request())
        .expect("sample");
    assert_eq!(outcome.text, "bonjour");
    let seen = approver.decisions.lock().expect("decisions");
    let request = seen.last().expect("one approval request");
    assert_eq!(request.tool, "mcp:srv:sampling");
    assert_eq!(request.effect, ToolEffect::Execute);
    assert!(request.reason.contains("srv"));
    let usage = cell.lock().expect("usage cell");
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 3);
}

/// FA 档免弹框（对齐 ModePolicy 的 FullAccess 语义）：approver 即便
/// 脚本化为 Deny 也不被咨询——档位 cell 是唯一的免门依据。
#[test]
fn full_access_mode_skips_the_sampling_approval_dialog() {
    let approver = Arc::new(ScriptedApprover {
        decisions: Mutex::new(Vec::new()),
        verdict: PermissionDecision::Deny {
            reason: "must not be consulted under Full Access".into(),
        },
    });
    let (bridge, cell) = installed_bridge_with_mode(
        providers_with(CannedFactory {
            text: "fa",
            usage: Some(Usage {
                input_tokens: 7,
                ..Usage::default()
            }),
        }),
        approver.clone(),
        None,
        Some(crate::permission::PermissionMode::FullAccess),
    );
    let outcome = bridge
        .sample(PluginSource::Mcp("srv".into()), sampling_request())
        .expect("sample");
    assert_eq!(outcome.text, "fa");
    assert!(
        approver.decisions.lock().expect("decisions").is_empty(),
        "Full Access must not consult the approver for sampling"
    );
    assert_eq!(cell.lock().expect("usage cell").input_tokens, 7);
}

/// 构建计数工厂：断言"预算拒绝先于 provider 工厂调用"的假件。
struct CountingFactory {
    text: &'static str,
    usage: Option<Usage>,
    builds: Arc<std::sync::atomic::AtomicUsize>,
}

impl ModelFactory for CountingFactory {
    fn protocol(&self) -> ModelProtocol {
        ModelProtocol::OpenAiCompatible
    }

    fn describe(&self, _credentials: &ProviderCredentials) -> crate::model::ProviderDescriptor {
        unimplemented!("not needed for plugin_host tests")
    }

    fn build(
        &self,
        _config: &ModelConfig,
        _credentials: &ProviderCredentials,
    ) -> Result<Box<dyn Model>, ModelError> {
        self.builds.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(CannedModel {
            text: self.text,
            usage: self.usage.clone(),
        }))
    }
}

// ---- W1-02：审批参数 = 权威出站正文 ----

/// 四个哨兵（systemPrompt、第二条 user、assistant、首条 160 字
/// 之后）必须全部出现在 approver 拿到的 arguments 里——审批框展示
/// 的是将真实送出模型的内容，不是摘要。
#[test]
fn sampling_approval_arguments_expose_the_full_outbound_payload() {
    let approver = Arc::new(ScriptedApprover {
        decisions: Mutex::new(Vec::new()),
        verdict: PermissionDecision::Allow,
    });
    let (bridge, _cell) = installed_bridge(
        providers_with(CannedFactory {
            text: "ok",
            usage: None,
        }),
        approver.clone(),
        None,
    );
    let request = SamplingRequest {
        system_prompt: Some("SECRET-SYSTEM-PROMPT".into()),
        messages: vec![
            SamplingMessage {
                role: SamplingRole::User,
                text: format!("benign {}SECRET-BEYOND-160", "x".repeat(200)),
            },
            SamplingMessage {
                role: SamplingRole::Assistant,
                text: "SECRET-ASSISTANT".into(),
            },
            SamplingMessage {
                role: SamplingRole::User,
                text: "SECRET-SECOND-USER".into(),
            },
        ],
        max_tokens: 512,
        stop_sequences: Vec::new(),
        temperature: Some(0.3),
    };
    bridge
        .sample(PluginSource::Wasm("probe".into()), request)
        .expect("sample");
    let seen = approver.decisions.lock().expect("decisions");
    let arguments = &seen.last().expect("one approval request").arguments;
    assert_eq!(
        arguments["systemPrompt"], "SECRET-SYSTEM-PROMPT",
        "systemPrompt is the most direct hiding channel and must be reviewable"
    );
    assert_eq!(arguments["messages"][0]["role"], "user");
    assert!(
        arguments["messages"][0]["text"]
            .as_str()
            .expect("full text")
            .contains("SECRET-BEYOND-160"),
        "content past char 160 of the first message must survive for review"
    );
    assert_eq!(arguments["messages"][1]["role"], "assistant");
    assert_eq!(arguments["messages"][1]["text"], "SECRET-ASSISTANT");
    assert_eq!(arguments["messages"][2]["text"], "SECRET-SECOND-USER");
    assert_eq!(arguments["temperature"], 0.3);
    assert_eq!(arguments["maxTokens"], 512);
}

/// 防洪边界（W1-02）：超总字符上限的请求整单拒绝，不进权限门、
/// 不碰 provider 工厂。
#[test]
fn oversized_sampling_payload_is_rejected_without_a_model_call() {
    let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let approver = Arc::new(ScriptedApprover {
        decisions: Mutex::new(Vec::new()),
        verdict: PermissionDecision::Allow,
    });
    let (bridge, _cell) = installed_bridge(
        providers_with(CountingFactory {
            text: "never",
            usage: None,
            builds: Arc::clone(&builds),
        }),
        approver.clone(),
        None,
    );
    let mut request = sampling_request();
    request.messages[0].text = "x".repeat(SAMPLING_MAX_TOTAL_CHARS + 1);
    let error = bridge
        .sample(PluginSource::Mcp("srv".into()), request)
        .unwrap_err();
    assert!(matches!(error, PluginHostError::PayloadTooLarge(_)));
    assert_eq!(
        builds.load(Ordering::SeqCst),
        0,
        "the provider factory must not be reached"
    );
    assert!(
        approver.decisions.lock().expect("decisions").is_empty(),
        "no approval dialog for a doomed request"
    );
}

// ---- W1-03：per-run sampling 预算 ----

fn tight_budget(tokens_cap: u64) -> SamplingBudget {
    SamplingBudget {
        requests_used: 0,
        tokens_used: 0,
        requests_cap: 64,
        tokens_cap,
        elicits_used: 0,
        elicits_cap: ELICIT_MAX_PER_RUN,
        stop_sequences_warned: false,
    }
}

#[test]
fn sampling_budget_reserve_and_reconcile_math() {
    let mut budget = SamplingBudget {
        requests_used: 0,
        tokens_used: 0,
        requests_cap: 2,
        tokens_cap: 30,
        elicits_used: 0,
        elicits_cap: ELICIT_MAX_PER_RUN,
        stop_sequences_warned: false,
    };
    assert!(budget.reserve(20).is_ok());
    assert!(budget.reserve(11).is_err(), "token cap fails closed");
    budget.reconcile(20, 5);
    assert!(
        budget.reserve(11).is_ok(),
        "reconcile swaps the reservation for actual usage (5 + 11 <= 30)"
    );
    assert!(
        budget.reserve(1).is_err(),
        "the request cap binds independently of tokens"
    );
    let mut budget = SamplingBudget {
        requests_used: 0,
        tokens_used: 0,
        requests_cap: 1,
        tokens_cap: u64::MAX,
        elicits_used: 0,
        elicits_cap: ELICIT_MAX_PER_RUN,
        stop_sequences_warned: false,
    };
    assert!(
        budget.reserve(u64::MAX).is_ok(),
        "reserve saturates at the cap"
    );
}

#[test]
fn input_token_estimate_covers_system_prompt_and_rounds_up() {
    // sampling_request：system "be brief"(9) + "translate hi to french"(22)
    // = 31 chars → ceil(31/4) = 8。
    let mut request = sampling_request();
    assert_eq!(estimate_input_tokens(&request), 8);
    request.system_prompt = None;
    assert_eq!(estimate_input_tokens(&request), 6);
}

/// 预算先于权限门、先于 provider 工厂：fail-closed 且结构化。
#[test]
fn sampling_budget_fails_closed_before_the_model_call() {
    let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let approver = Arc::new(ScriptedApprover {
        decisions: Mutex::new(Vec::new()),
        verdict: PermissionDecision::Allow,
    });
    let (bridge, cell) = installed_bridge_with(
        providers_with(CountingFactory {
            text: "never",
            usage: None,
            builds: Arc::clone(&builds),
        }),
        approver.clone(),
        None,
        None,
        tight_budget(0),
    );
    let error = bridge
        .sample(PluginSource::Mcp("srv".into()), sampling_request())
        .unwrap_err();
    assert!(matches!(error, PluginHostError::BudgetExhausted(_)));
    assert!(
        error.to_string().contains("resets on the next run"),
        "structured failure the plugin/agent can act on: {error}"
    );
    assert_eq!(builds.load(Ordering::SeqCst), 0);
    assert!(
        approver.decisions.lock().expect("decisions").is_empty(),
        "a doomed request must not cost the user an approval dialog"
    );
    assert_eq!(cell.lock().expect("usage cell").input_tokens, 0);
}

/// Full Access 免弹框，不免预算（W1-03：权限档位 ≠ 财务额度）。
#[test]
fn full_access_sampling_still_consumes_budget() {
    let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let approver = Arc::new(ScriptedApprover {
        decisions: Mutex::new(Vec::new()),
        verdict: PermissionDecision::Deny {
            reason: "must not be consulted under Full Access".into(),
        },
    });
    let (bridge, _cell) = installed_bridge_with(
        providers_with(CountingFactory {
            text: "never",
            usage: None,
            builds: Arc::clone(&builds),
        }),
        approver.clone(),
        None,
        Some(crate::permission::PermissionMode::FullAccess),
        tight_budget(0),
    );
    let error = bridge
        .sample(PluginSource::Mcp("srv".into()), sampling_request())
        .unwrap_err();
    assert!(matches!(error, PluginHostError::BudgetExhausted(_)));
    assert_eq!(
        builds.load(Ordering::SeqCst),
        0,
        "Full Access must not unlock an unlimited spend path"
    );
    assert!(approver.decisions.lock().expect("decisions").is_empty());
}

/// W1-09：预算闸门对毒锁 fail-closed——持锁 panic 的残余（中毒
/// mutex）必须拒绝采样，而不是静默跳过预留让模型调用免检进闸。
#[test]
fn sampling_budget_lock_poisoning_fails_closed() {
    let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let approver = Arc::new(ScriptedApprover {
        decisions: Mutex::new(Vec::new()),
        verdict: PermissionDecision::Allow,
    });
    let budget = Arc::new(Mutex::new(SamplingBudget::per_run()));
    // 持锁 panic 一次，毒化 mutex（catch_unwind 收住 panic 本身）。
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let poison_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = budget.lock().expect("lock before poisoning");
        panic!("poison the sampling budget lock");
    }));
    std::panic::set_hook(previous_hook);
    assert!(poison_result.is_err(), "the poisoning panic must be caught");

    let bridge = PluginHostBridge::shared();
    let usage_cell = Arc::new(Mutex::new(Usage::default()));
    bridge.install(RunHostContext {
        providers: providers_with(CountingFactory {
            text: "never",
            usage: None,
            builds: Arc::clone(&builds),
        }),
        model_config: ModelConfig {
            model: "fake-model".into(),
            ..ModelConfig::default()
        },
        credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
        approver,
        permission_mode: None,
        asker: None,
        cancel: CancelToken::new(),
        usage_cell: Arc::clone(&usage_cell),
        budget: Arc::clone(&budget),
    });

    let error = bridge
        .sample(PluginSource::Mcp("srv".into()), sampling_request())
        .unwrap_err();
    assert!(
        matches!(error, PluginHostError::BudgetExhausted(_)),
        "a poisoned budget lock must refuse sampling, got: {error}"
    );
    assert!(
        error.to_string().contains("poisoned"),
        "the failure must name the cause: {error}"
    );
    assert_eq!(
        builds.load(Ordering::SeqCst),
        0,
        "no model call may happen past a poisoned gate"
    );
    assert_eq!(usage_cell.lock().expect("usage cell").input_tokens, 0);
}

/// provider 不回 usage → 预留保留（保守账本）：同预算的第二次
/// sampling 被拒；回 usage → 按实际对账，第二次放行。
#[test]
fn sampling_budget_reconciles_only_when_usage_is_reported() {
    // 预留 = estimate(31/4→8) + max_tokens 64 = 72；对账后实际 13。
    let reservation = estimate_input_tokens(&sampling_request()) + 64;
    let cap = reservation + 28; // 100：预留保留则第二次超限，对账后放行
    let usage = Some(Usage {
        input_tokens: 10,
        output_tokens: 3,
        ..Usage::default()
    });

    // 无 usage：预留保留 → 第二次拒绝。
    let (bridge, _cell) = installed_bridge_with(
        providers_with(CannedFactory {
            text: "ok",
            usage: None,
        }),
        Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        }),
        None,
        None,
        tight_budget(cap),
    );
    bridge
        .sample(PluginSource::Mcp("srv".into()), sampling_request())
        .expect("first call within budget");
    assert!(matches!(
        bridge.sample(PluginSource::Mcp("srv".into()), sampling_request()),
        Err(PluginHostError::BudgetExhausted(_))
    ));

    // 有 usage：对账释放差额 → 第二次放行。
    let (bridge, _cell) = installed_bridge_with(
        providers_with(CannedFactory { text: "ok", usage }),
        Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        }),
        None,
        None,
        tight_budget(cap),
    );
    bridge
        .sample(PluginSource::Mcp("srv".into()), sampling_request())
        .expect("first call within budget");
    bridge
        .sample(PluginSource::Mcp("srv".into()), sampling_request())
        .expect("reconciled usage frees the difference for the next call");
}

#[test]
fn sampling_denied_or_unavailable_fails_closed_without_a_model_call() {
    for verdict in [
        PermissionDecision::Deny {
            reason: "no".into(),
        },
        PermissionDecision::Unavailable {
            reason: "headless".into(),
        },
        PermissionDecision::Ask {
            reason: "unresolved".into(),
        },
    ] {
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict,
        });
        let (bridge, cell) = installed_bridge(
            providers_with(CannedFactory {
                text: "never",
                usage: None,
            }),
            approver,
            None,
        );
        let error = bridge
            .sample(PluginSource::Mcp("srv".into()), sampling_request())
            .unwrap_err();
        assert!(matches!(error, PluginHostError::PermissionDenied(_)));
        let usage = cell.lock().expect("usage cell");
        assert_eq!(
            usage.input_tokens, 0,
            "denied sampling must not burn tokens"
        );
    }
}

#[test]
fn headless_elicitation_reports_no_frontend() {
    let approver = Arc::new(ScriptedApprover {
        decisions: Mutex::new(Vec::new()),
        verdict: PermissionDecision::Allow,
    });
    let (bridge, _cell) = installed_bridge(
        providers_with(CannedFactory {
            text: "ok",
            usage: None,
        }),
        approver,
        None,
    );
    let form = parse_elicitation_params(&json!({
        "message": "pick one",
        "requestedSchema": {
            "type": "object",
            "properties": { "flavor": { "type": "string" } },
            "required": ["flavor"],
        },
    }))
    .expect("form");
    assert!(matches!(
        bridge.elicit(form),
        Err(PluginHostError::NoInteractiveFrontend)
    ));
}

// ---- elicitation 顺序单问 ----

#[test]
fn elicitation_asks_fields_in_order_and_assembles_content() {
    let asker: Arc<dyn UserAsker> = Arc::new(ScriptedAsker {
        answers: Mutex::new(
            vec![
                AskAnswer::Custom("vanilla".into()),
                AskAnswer::Selected("yes".into()),
                AskAnswer::Custom("2".into()),
                AskAnswer::Selected("red".into()),
            ]
            .into(),
        ),
    });
    let approver = Arc::new(ScriptedApprover {
        decisions: Mutex::new(Vec::new()),
        verdict: PermissionDecision::Allow,
    });
    let (bridge, _cell) = installed_bridge(
        providers_with(CannedFactory {
            text: "ok",
            usage: None,
        }),
        approver,
        Some(asker),
    );
    let form = parse_elicitation_params(&json!({
        "message": "configure the widget",
        "requestedSchema": {
            "type": "object",
            "properties": {
                "name": { "type": "string", "title": "Name" },
                "bold": { "type": "boolean" },
                "size": { "type": "number" },
                "color": { "enumValues": ["red", "green"] },
            },
            "required": ["name", "bold", "size", "color"],
        },
    }))
    .expect("form");
    let ElicitOutcome::Accepted(content) = bridge.elicit(form).expect("elicited") else {
        panic!("expected an accepted form");
    };
    assert_eq!(content["name"], json!("vanilla"));
    assert_eq!(content["bold"], json!(true));
    assert_eq!(content["size"], json!(2));
    assert_eq!(content["color"], json!("red"));
    // 字段序即提交序（preserve_order）。
    assert_eq!(
        content.keys().collect::<Vec<_>>(),
        ["name", "bold", "size", "color"]
    );
}

/// W1-14（A1）：尺寸闸必须住在**桥层**——MCP JSON 解析路径的
/// 16 字段/16 选项上限对 WASM WIT 直通无效（wasm.rs 的 Host 直转
/// ElicitForm，不经过 parse_elicitation_params）。无 asker 时旧代码
/// 以 NoInteractiveFrontend 失败（闸不存在），本测试在修复前即红。
#[test]
fn elicitation_size_gates_live_in_the_bridge_not_the_mcp_parser() {
    let bridge = PluginHostBridge::shared();
    bridge.install(RunHostContext {
        providers: providers_with(CannedFactory {
            text: "never",
            usage: None,
        }),
        model_config: ModelConfig {
            model: "fake-model".into(),
            ..ModelConfig::default()
        },
        credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
        approver: Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        }),
        permission_mode: None,
        asker: Some(Arc::new(ScriptedAsker {
            answers: Mutex::new(Vec::new().into()),
        })),
        cancel: CancelToken::new(),
        usage_cell: Arc::new(Mutex::new(Usage::default())),
        budget: Arc::new(Mutex::new(SamplingBudget::per_run())),
    });
    // 17 个字段（> MAX_ELICIT_FIELDS）：即便有 asker 也必须被拒，
    // 而不是开始逐字段弹 17 个问题。
    let too_many_fields = ElicitForm {
        message: "form".into(),
        fields: (0..17)
            .map(|index| ElicitField {
                name: format!("f{index}"),
                title: None,
                description: None,
                kind: ElicitFieldKind::Text,
                required: true,
            })
            .collect(),
    };
    let error = bridge
        .elicit(too_many_fields)
        .expect_err("17 fields must be rejected at the bridge");
    assert!(
        error.to_string().contains("16"),
        "the rejection must name the field limit: {error}"
    );
    // message 文案超长（> 4096 字符）：钓鱼洪水面。
    let huge_message = ElicitForm {
        message: "x".repeat(4097),
        fields: Vec::new(),
    };
    let error = bridge
        .elicit(huge_message)
        .expect_err("an oversized message must be rejected at the bridge");
    assert!(
        error.to_string().contains("message"),
        "the rejection must name the message cap: {error}"
    );
    // 单字段选项 > 16：同闸。
    let too_many_options = ElicitForm {
        message: "form".into(),
        fields: vec![ElicitField {
            name: "pick".into(),
            title: None,
            description: None,
            kind: ElicitFieldKind::Choice((0..17).map(|i| format!("o{i}")).collect()),
            required: true,
        }],
    };
    let error = bridge
        .elicit(too_many_options)
        .expect_err("17 options must be rejected at the bridge");
    assert!(
        error.to_string().contains("16"),
        "the rejection must name the option limit: {error}"
    );
}

#[test]
fn elicitation_number_field_retries_then_fails_with_invalid_answer() {
    let asker: Arc<dyn UserAsker> = Arc::new(ScriptedAsker {
        answers: Mutex::new(
            vec![
                AskAnswer::Custom("not-a-number".into()),
                AskAnswer::Custom("still-not".into()),
                AskAnswer::Custom("nope".into()),
            ]
            .into(),
        ),
    });
    let approver = Arc::new(ScriptedApprover {
        decisions: Mutex::new(Vec::new()),
        verdict: PermissionDecision::Allow,
    });
    let (bridge, _cell) = installed_bridge(
        providers_with(CannedFactory {
            text: "ok",
            usage: None,
        }),
        approver,
        Some(asker),
    );
    let form = parse_elicitation_params(&json!({
        "message": "how many",
        "requestedSchema": {
            "type": "object",
            "properties": { "count": { "type": "number" } },
            "required": ["count"],
        },
    }))
    .expect("form");
    let error = bridge.elicit(form).unwrap_err();
    assert!(matches!(error, PluginHostError::InvalidAnswer(_)));
}

/// A4-5（W1-25）：非有限数字（"NaN"/"inf"）不得作为数字答案通过
/// ——serde_json 会把非有限 f64 序列化成 Null（类型违约）。pre-fix
/// 红：parse_number 的 f64 分支放行 NaN → 值以 Null 落 content。
#[test]
fn elicitation_number_fields_reject_non_finite_answers() {
    for bad in ["NaN", "inf", "-inf", "infinity"] {
        let asker: Arc<dyn UserAsker> = Arc::new(ScriptedAsker {
            answers: Mutex::new(
                vec![
                    AskAnswer::Custom(bad.into()),
                    AskAnswer::Custom(bad.into()),
                    AskAnswer::Custom(bad.into()),
                ]
                .into(),
            ),
        });
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        });
        let (bridge, _cell) = installed_bridge(
            providers_with(CannedFactory {
                text: "ok",
                usage: None,
            }),
            approver,
            Some(asker),
        );
        let form = parse_elicitation_params(&json!({
            "message": "how many",
            "requestedSchema": {
                "type": "object",
                "properties": { "count": { "type": "number" } },
                "required": ["count"],
            },
        }))
        .expect("form");
        let error = bridge.elicit(form).unwrap_err();
        assert!(
            matches!(error, PluginHostError::InvalidAnswer(_)),
            "{bad} must never pass as a number: {error}"
        );
    }
}

#[test]
fn elicitation_declined_and_cancelled_map_to_actions() {
    let form = || {
        parse_elicitation_params(&json!({
            "message": "m",
            "requestedSchema": {
                "type": "object",
                "properties": { "a": { "type": "string" } },
                "required": ["a"],
            },
        }))
        .expect("form")
    };
    let approver = || {
        Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        }) as Arc<dyn PermissionApprover>
    };
    let providers = || {
        providers_with(CannedFactory {
            text: "ok",
            usage: None,
        })
    };

    // Declined（取消令牌未触发）→ declined。
    let asker: Arc<dyn UserAsker> = Arc::new(ScriptedAsker {
        answers: Mutex::new(vec![AskAnswer::Declined].into()),
    });
    let (bridge, _cell) = installed_bridge(providers(), approver(), Some(asker));
    assert!(matches!(bridge.elicit(form()), Ok(ElicitOutcome::Declined)));

    // Declined 且取消令牌已触发 → cancelled（Esc/断连路径）。
    let bridge = PluginHostBridge::shared();
    let cancel = CancelToken::new();
    cancel.cancel();
    bridge.install(RunHostContext {
        providers: providers(),
        model_config: ModelConfig::default(),
        credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
        approver: approver(),
        permission_mode: None,
        asker: Some(Arc::new(ScriptedAsker {
            answers: Mutex::new(vec![AskAnswer::Declined].into()),
        })),
        cancel,
        usage_cell: Arc::new(Mutex::new(Usage::default())),
        budget: Arc::new(Mutex::new(SamplingBudget::per_run())),
    });
    // A1/INV-S8：已取消的 run 在入口即以 Err(Cancelled) 收束——
    // 连第一个问题都不弹（旧路径先弹框再靠 Declined 映射）。
    assert!(matches!(
        bridge.elicit(form()),
        Err(PluginHostError::Cancelled)
    ));
}

// ---- wire 解析 ----

#[test]
fn sampling_params_parse_and_reject_non_text_content() {
    let params = json!({
        "systemPrompt": "sys",
        "messages": [
            { "role": "user", "content": { "type": "text", "text": "hello" } },
            { "role": "assistant", "content": [
                { "type": "text", "text": "hi" },
            ] },
        ],
        "maxTokens": 100000,
    });
    let request = parse_sampling_params(&params).expect("parse");
    assert_eq!(request.messages.len(), 2);
    assert_eq!(request.messages[1].text, "hi");
    assert_eq!(request.max_tokens, SAMPLING_MAX_OUTPUT, "maxTokens clamped");

    let bad = json!({
        "messages": [
            { "role": "user", "content": { "type": "image", "data": "…" } },
        ],
        "maxTokens": 10,
    });
    let error = parse_sampling_params(&bad).unwrap_err();
    assert_eq!(error.0, -32602);

    let missing = json!({ "maxTokens": 10 });
    assert!(parse_sampling_params(&missing).is_err());
}

/// B7（C2 定案）：`stopSequences`（DSH shim 从 `options.stop` 映射
/// 而来）必须被解析进请求——宿主接受但忽略它，解析层不再丢弃
///（丢弃会让桥层的"ignored"一次性诊断永不触发，作者不可见）。
/// pre-fix 红：旧实现硬编码 `stop_sequences: Vec::new()`。
#[test]
fn sampling_params_parse_stop_sequences() {
    let params = json!({
        "messages": [
            { "role": "user", "content": { "type": "text", "text": "hi" } },
        ],
        "maxTokens": 64,
        "stopSequences": ["\n\nUser:", "\n\nAssistant:"],
    });
    let request = parse_sampling_params(&params).expect("parse");
    assert_eq!(request.stop_sequences, ["\n\nUser:", "\n\nAssistant:"]);

    // 缺席 → 空；非字符串成员被宽松过滤（与 DSH 桥的宽松解析风格
    // 一致，不因坏成员拒绝整次请求）。
    let absent = json!({
        "messages": [
            { "role": "user", "content": { "type": "text", "text": "hi" } },
        ],
        "maxTokens": 64,
    });
    assert!(
        parse_sampling_params(&absent)
            .expect("parse")
            .stop_sequences
            .is_empty()
    );
    let tolerant = json!({
        "messages": [
            { "role": "user", "content": { "type": "text", "text": "hi" } },
        ],
        "maxTokens": 64,
        "stopSequences": ["stop", 42, null],
    });
    assert_eq!(
        parse_sampling_params(&tolerant)
            .expect("parse")
            .stop_sequences,
        ["stop"]
    );
}

#[test]
fn elicitation_params_parse_primitive_subset_and_reject_the_rest() {
    let params = json!({
        "message": "form",
        "requestedSchema": {
            "type": "object",
            "properties": {
                "s": { "type": "string", "title": "S" },
                "n": { "type": "integer" },
                "b": { "type": "boolean" },
                "e": { "enumValues": ["a", "b"] },
            },
            "required": ["s", "e"],
        },
    });
    let form = parse_elicitation_params(&params).expect("parse");
    assert_eq!(form.fields.len(), 4);
    assert!(form.fields[0].required);
    assert!(!form.fields[2].required);

    // url 模式、嵌套类型、空 properties、超量字段均拒绝。
    assert!(
        parse_elicitation_params(&json!({
            "mode": "url", "message": "m", "elicitationId": "e", "url": "https://x"
        }))
        .is_err()
    );
    assert!(
        parse_elicitation_params(&json!({
            "message": "m",
            "requestedSchema": {
                "type": "object",
                "properties": { "tags": { "type": "array" } },
            },
        }))
        .is_err()
    );
    assert!(
        parse_elicitation_params(&json!({
            "message": "m",
            "requestedSchema": { "type": "object", "properties": {} },
        }))
        .is_err()
    );
    let many: serde_json::Map<String, Value> = (0..MAX_ELICIT_FIELDS + 1)
        .map(|index| (format!("f{index}"), json!({ "type": "string" })))
        .collect();
    assert!(
        parse_elicitation_params(&json!({
            "message": "m",
            "requestedSchema": { "type": "object", "properties": many },
        }))
        .is_err()
    );
}

// ---- MCP 处理器（wire 出入口） ----

#[test]
fn mcp_handler_routes_methods_and_reports_pending() {
    let approver = Arc::new(ScriptedApprover {
        decisions: Mutex::new(Vec::new()),
        verdict: PermissionDecision::Allow,
    });
    let (bridge, _cell) = installed_bridge(
        providers_with(CannedFactory {
            text: "answer",
            usage: None,
        }),
        approver,
        None,
    );
    let handler = McpHostHandler::new(Arc::clone(&bridge), "srv");
    let host_error = handler
        .handle("io.artec.clat/context/get", json!({}))
        .expect_err("private extension requires negotiated capability");
    assert_eq!(host_error.0, -32601);
    let result = handler
        .handle(
            "sampling/createMessage",
            json!({
                "messages": [
                    { "role": "user", "content": { "type": "text", "text": "q" } },
                ],
                "maxTokens": 16,
            }),
        )
        .expect("sampling");
    assert_eq!(result["content"]["text"], "answer");
    assert_eq!(result["model"], "fake-model");
    assert_eq!(result["stopReason"], "endTurn");
    assert_eq!(handler.pending_requests(), 0);
    // 未知方法 → -32601（INV-S4 的处理器侧；ping 在 dispatcher）。
    let error = handler.handle("roots/list", json!({})).unwrap_err();
    assert_eq!(error.0, -32601);
}

/// 卡在权限门内的 approver：entered 置位后阻塞到 release。
struct GateApprover {
    entered: Arc<std::sync::atomic::AtomicBool>,
    release: Arc<std::sync::atomic::AtomicBool>,
}

impl PermissionApprover for GateApprover {
    fn decide(&self, _request: PermissionRequest, _cancel: &CancelToken) -> PermissionDecision {
        self.entered.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.release.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "approver never released");
            std::thread::sleep(Duration::from_millis(2));
        }
        PermissionDecision::Allow
    }
}

/// W1-05：在途计数 per-handler。server A 的 sampling 进行中，共用
/// 同一座桥的 server B 的 `pending_requests()` 必须仍为 0——A 的
/// 等待不得延长 B 的 tools/call 截止。pre-fix 红：B 读到的是桥级
/// 全局计数 1。
#[test]
fn pending_counts_are_per_server_not_shared_through_the_bridge() {
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (bridge, _cell) = installed_bridge(
        providers_with(CannedFactory {
            text: "answer",
            usage: None,
        }),
        Arc::new(GateApprover {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        }),
        None,
    );
    let a = Arc::new(McpHostHandler::new(Arc::clone(&bridge), "a"));
    let b = McpHostHandler::new(Arc::clone(&bridge), "b");
    let dispatcher = {
        let a = Arc::clone(&a);
        std::thread::spawn(move || {
            a.handle(
                "sampling/createMessage",
                json!({
                    "messages": [
                        { "role": "user", "content": { "type": "text", "text": "q" } },
                    ],
                    "maxTokens": 16,
                }),
            )
            .is_ok()
        })
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while !entered.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "handler never entered the gate");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        a.pending_requests(),
        1,
        "the in-flight call counts on its own handler"
    );
    assert_eq!(
        b.pending_requests(),
        0,
        "an unrelated server sharing the bridge must not inherit A's pending count"
    );
    release.store(true, Ordering::Release);
    assert!(dispatcher.join().expect("dispatcher thread"));
    assert_eq!(a.pending_requests(), 0);
    assert_eq!(b.pending_requests(), 0);
}

/// 插件桥 Phase 3 e2e（INV-D7）：`@artec/clat-dsh-adapter` 的 demo 插件作为
/// 真实 MCP stdio server 被 CLAT 客户端挂载——echo 纯路径、
/// sample_roundtrip 过本桥的权限门 + 假模型 + usage 记账、
/// ask_roundtrip 过顺序单问（含 enumValues 选择与 multiSelect 降级），
/// host_roundtrip 把 DSH 形状的 fs/shell/sessions/agents 打到同一 Rust
/// 宿主桥与原生工具管线。
/// 需 node ≥22.19 与已构建的适配器（`dist/` 是 gitignore 的构建产物，
/// 缺失时 skip 而非误报——CI 的门控腿先构建 adapter，见 ci.yml），
/// `cargo test -- --ignored` 显式跑。
#[test]
#[ignore = "spawns the node dsh-adapter demo; run explicitly with --ignored"]
fn dsh_adapter_demo_end_to_end_over_mcp() {
    use crate::mcp::client::{McpServer, McpServerConfig};
    use std::path::Path;

    let bin = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("sdk/dsh-adapter/tools/demo-bin.mjs");
    assert!(
        bin.exists(),
        "missing {} — build the adapter first: cd sdk/dsh-adapter && npm install && npm run build",
        bin.display()
    );
    // 启动器已提交、真正的运行时依赖是构建产物：新鲜克隆/未构建时
    // skip（守卫必须查 import 的目标，而不是启动器本身——否则 node
    // 起来后才在 ERR_MODULE_NOT_FOUND 上炸出误报）。
    let dist = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("sdk/dsh-adapter/dist/src/demo.js");
    if !dist.exists() {
        eprintln!(
            "skipped: adapter not built ({}); build with: \
             cd sdk/dsh-adapter && npm install && npm run build",
            dist.display()
        );
        return;
    }
    let config = McpServerConfig {
        command: "node".into(),
        args: vec![bin.display().to_string()],
        ..Default::default()
    };

    // ask_roundtrip 的三字段按序作答：单选（Choice）→ multiSelect 降级
    // 文本 → 自由文本。
    let asker: Arc<dyn UserAsker> = Arc::new(ScriptedAsker {
        answers: Mutex::new(
            vec![
                AskAnswer::Selected("pistachio".into()),
                AskAnswer::Custom("sprinkles, fudge, extra".into()),
                AskAnswer::Custom("no sugar".into()),
            ]
            .into(),
        ),
    });
    let approver = Arc::new(ScriptedApprover {
        decisions: Mutex::new(Vec::new()),
        verdict: PermissionDecision::Allow,
    });
    let (bridge, usage_cell) = installed_bridge(
        providers_with(CannedFactory {
            text: "bonjour",
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 3,
                ..Usage::default()
            }),
        }),
        approver,
        Some(asker),
    );
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let project_root = std::env::temp_dir().join(format!("clat-dsh-host-e2e-{unique}"));
    std::fs::create_dir_all(&project_root).expect("project root");
    std::fs::write(project_root.join("host.txt"), "from dsh host\n").expect("host fixture");
    let host_tools = Arc::new(ToolRegistry::new());
    let host_owner = PluginOwner::for_test(PluginId::new("test.dsh_host_e2e"));
    let _read_lease = host_tools
        .register(host_owner, Arc::new(crate::native_tools::ReadFileTool))
        .expect("register read_file");
    let _run_lease = host_tools
        .register(host_owner, Arc::new(crate::native_tools::RunCommandTool))
        .expect("register run_command");
    bridge.configure_host_services(
        Project::new(&project_root),
        Arc::clone(&host_tools),
        Arc::new(ToolExecutionPipeline::new()),
        Arc::new(AllowPolicyFactory),
    );
    bridge.update_run_metadata("dsh-host-session", &[ModelItem::user_text("hello")]);
    let handler = Arc::new(McpHostHandler::new(Arc::clone(&bridge), "demo"));
    let server = McpServer::connect("demo", &config, &project_root, Some(handler.clone()))
        .expect("connect to the dsh-adapter demo");
    assert!(server.supports_clat_host_services());
    handler.enable_host_services();

    let tools = server.list_tools().expect("tools");
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    for expected in [
        "echo",
        "sample_roundtrip",
        "ask_roundtrip",
        "host_roundtrip",
    ] {
        assert!(names.contains(&expected), "tools: {names:?}");
    }
    let prompts = server.list_system_prompts().expect("system prompts");
    assert_eq!(prompts.len(), 1);
    let mut prompt_arguments = std::collections::BTreeMap::new();
    prompt_arguments.insert("cwd".to_owned(), "/tmp/project".to_owned());
    let prompt = server
        .get_system_prompt(&prompts[0], &prompt_arguments)
        .expect("resolve DSH system prompt");
    assert_eq!(prompt, "Demo plugin is active in /tmp/project.");

    let cancel = CancelToken::new();
    let echo = server
        .call_tool_for_test("echo", &json!({"text": "hi", "times": 2}), &cancel)
        .expect("echo");
    assert!(echo.as_str().unwrap_or_default().contains(r#""lines""#));

    // sampling 全链：适配器 → sampling/createMessage → 权限门（Allow）→
    // 假模型 → usage 记账（INV-S6）。
    let sampled = server
        .call_tool_for_test(
            "sample_roundtrip",
            &json!({"prompt": "translate hi"}),
            &cancel,
        )
        .expect("sample_roundtrip");
    assert!(sampled.as_str().unwrap_or_default().contains("bonjour"));
    let usage = usage_cell.lock().expect("usage cell");
    assert_eq!(usage.input_tokens, 10, "sampling must account usage");
    assert_eq!(usage.output_tokens, 3);
    drop(usage);

    // elicitation 全链：顺序单问（Choice + 两个文本）→ 结构化应答回填。
    let asked = server
        .call_tool_for_test("ask_roundtrip", &json!({}), &cancel)
        .expect("ask_roundtrip");
    let answer = asked.as_str().unwrap_or_default();
    assert!(answer.contains("pistachio"), "answer: {answer}");
    assert!(answer.contains("sprinkles"), "answer: {answer}");
    assert!(answer.contains("fudge"), "answer: {answer}");
    assert!(answer.contains("no sugar"), "answer: {answer}");

    // 宿主服务全链：DSH ctx.fs/ctx.shell/ctx.sessions/ctx.agents →
    // adapter experimental RPC → PluginHostBridge → 原生 Rust 工具。
    let hosted = server
        .call_tool_for_test("host_roundtrip", &json!({"path": "host.txt"}), &cancel)
        .expect("host_roundtrip");
    let hosted = hosted.as_str().unwrap_or_default();
    assert!(hosted.contains("from dsh host"), "hosted: {hosted}");
    assert!(hosted.contains("dsh-shell"), "hosted: {hosted}");
    assert!(hosted.contains("dsh-host-session"), "hosted: {hosted}");

    server.shutdown().expect("shutdown reaps the node process");
    bridge.clear();
    std::fs::remove_dir_all(project_root).expect("cleanup project root");
}

/// 插件桥 Phase 3b e2e：npm 真实发布物 `dsh-web-search-exa@0.0.1-rc.1`
/// 原样挂载（examples/exa），CLAT 客户端断言内置 `web_search` 出现、
/// annotations 正确（ro+ow），无 API key 时 WEB_PROVIDER_UNAVAILABLE
/// 以 isError 返回（免网络）。需 examples/exa 已 `npm install`（缺失时
/// skip——真实发布物验收是本地显式跑的门面，CI 不装这一步）。
#[test]
#[ignore = "spawns node with the real exa plugin; run explicitly with --ignored"]
fn dsh_adapter_real_web_search_exa_end_to_end() {
    use crate::mcp::client::{McpServer, McpServerConfig};
    use std::path::Path;

    let bin = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("sdk/dsh-adapter/examples/exa/bin.mjs");
    assert!(
        bin.exists(),
        "missing {} — build it first: cd sdk/dsh-adapter/examples/exa && npm install --legacy-peer-deps \
         (after building the adapter: cd sdk/dsh-adapter && npm install && npm run build)",
        bin.display()
    );
    // 真实前置 = examples/exa 的 file: 安装（node_modules 里的 adapter
    // dist 一并就位）；未安装时 skip 而非 ERR_MODULE_NOT_FOUND 误报。
    let installed = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join(
        "sdk/dsh-adapter/examples/exa/node_modules/@artec/clat-dsh-adapter/dist/src/index.js",
    );
    if !installed.exists() {
        eprintln!(
            "skipped: examples/exa not installed ({}); install with: \
             cd sdk/dsh-adapter && npm install && npm run build && \
             cd examples/exa && npm install --legacy-peer-deps",
            installed.display()
        );
        return;
    }
    // 本测试断言「无 key → 默认解析路径 → WEB_PROVIDER_UNAVAILABLE」。
    // 子进程继承父环境（stdio 全继承，D1 披露），而 adapter 的
    // web_search 会读运维旋钮 DSH_WEB_SEARCH_PROVIDER：ambient 设置
    // 会改写错误形状（CONFIGURED_MISSING/UNAVAILABLE），断言以误导
    // 性文案失败（2026-08-22 实测）。旋钮语义属 adapter 单测面；
    // 这里对被 ambient 覆盖的环境 skip。
    if std::env::var_os("DSH_WEB_SEARCH_PROVIDER").is_some() {
        eprintln!(
            "skipped: DSH_WEB_SEARCH_PROVIDER is set in the environment; \
             this acceptance pins the default provider resolution \
             (unset it to run)"
        );
        return;
    }
    let config = McpServerConfig {
        command: "node".into(),
        args: vec![bin.display().to_string()],
        ..Default::default()
    };
    let server =
        McpServer::connect("web-search-exa", &config, Path::new("/tmp"), None).expect("connect");

    let tools = server.list_tools().expect("tools");
    assert_eq!(tools.len(), 1, "only the built-in web_search is exposed");
    assert_eq!(tools[0].name, "web_search");
    // effect_from_annotations：readOnly+openWorld → Network。
    assert_eq!(
        crate::mcp::client::effect_from_annotations_for_test(tools[0].annotations),
        crate::tool::ToolEffect::Network
    );

    // isError 结果在 CLAT 侧映射为 Err（消息携带适配器原样的
    // WEB_PROVIDER_UNAVAILABLE）。
    let error = server
        .call_tool_for_test(
            "web_search",
            &json!({"queries": ["clat"]}),
            &crate::model::CancelToken::new(),
        )
        .expect_err("no API key must fail the call");
    assert!(
        error.to_string().contains("WEB_PROVIDER_UNAVAILABLE"),
        "seam error must surface verbatim: {error}"
    );
    server.shutdown().expect("shutdown reaps the node process");
}
/// W1-17/A1（INV-S8）：run 在审批等待期间结束后（bridge clear），迟
/// 到的 Allow 不再产生任何模型调用——审批返回后复查纪元与取消。
/// 判别：删除复查（回到旧形状）即以"模型被调用"而红。
#[test]
fn allow_arriving_after_the_run_ends_makes_no_model_call() {
    let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // 审批人：被咨询的瞬间结束当前 run（模拟收尾窗口），再 Allow。
    struct EndRunThenAllow {
        bridge: Arc<PluginHostBridge>,
    }
    impl PermissionApprover for EndRunThenAllow {
        fn decide(&self, _request: PermissionRequest, _cancel: &CancelToken) -> PermissionDecision {
            self.bridge.clear();
            PermissionDecision::Allow
        }
    }
    let bridge = PluginHostBridge::shared();
    bridge.install(RunHostContext {
        providers: providers_with(CountingFactory {
            text: "never",
            usage: None,
            builds: Arc::clone(&builds),
        }),
        model_config: ModelConfig {
            model: "fake-model".into(),
            ..ModelConfig::default()
        },
        credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
        approver: Arc::new(EndRunThenAllow {
            bridge: Arc::clone(&bridge),
        }),
        permission_mode: None,
        asker: None,
        cancel: CancelToken::new(),
        usage_cell: Arc::new(Mutex::new(Usage::default())),
        budget: Arc::new(Mutex::new(SamplingBudget::per_run())),
    });
    let error = bridge
        .sample(PluginSource::Mcp("srv".into()), sampling_request())
        .unwrap_err();
    assert!(
        matches!(error, PluginHostError::Cancelled),
        "a stale Allow must refuse to sample, got: {error}"
    );
    assert_eq!(
        builds.load(Ordering::SeqCst),
        0,
        "no model call may happen after the run ended"
    );
}

/// W1-14/A1：per-run elicitation 计数预算——触顶后的弹框被结构化
/// 拒绝（fail-closed），恶意组件不能用无限表单钓鱼。
#[test]
fn elicitation_budget_rejects_prompts_past_the_run_cap() {
    let asker: Arc<dyn UserAsker> = Arc::new(ScriptedAsker {
        answers: Mutex::new(
            vec![
                AskAnswer::Declined,
                AskAnswer::Declined,
                AskAnswer::Declined,
                AskAnswer::Declined,
                AskAnswer::Declined,
                AskAnswer::Declined,
                AskAnswer::Declined,
                AskAnswer::Declined,
            ]
            .into(),
        ),
    });
    let approver = Arc::new(ScriptedApprover {
        decisions: Mutex::new(Vec::new()),
        verdict: PermissionDecision::Allow,
    });
    let small_cap = SamplingBudget {
        requests_used: 0,
        tokens_used: 0,
        requests_cap: 64,
        tokens_cap: u64::MAX,
        elicits_used: 0,
        elicits_cap: 2,
        stop_sequences_warned: false,
    };
    let (bridge, _cell) = installed_bridge_with(
        providers_with(CannedFactory {
            text: "never",
            usage: None,
        }),
        approver,
        Some(asker),
        None,
        small_cap,
    );
    let form = || ElicitForm {
        message: "m".into(),
        fields: vec![ElicitField {
            name: "a".into(),
            title: None,
            description: None,
            kind: ElicitFieldKind::Text,
            required: true,
        }],
    };
    assert!(matches!(bridge.elicit(form()), Ok(ElicitOutcome::Declined)));
    assert!(matches!(bridge.elicit(form()), Ok(ElicitOutcome::Declined)));
    let error = bridge.elicit(form()).unwrap_err();
    assert!(
        matches!(error, PluginHostError::BudgetExhausted(_)),
        "the third prompt must exhaust the elicit budget, got: {error}"
    );
}
