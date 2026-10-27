//! 跨模块共享的测试基座（仅测试构建）。
//!
//! application 集成测试与 exec（headless CLI）测试都需要向真实
//! `PROVIDER_SERVICE` 注册脚本化模型，并使用同样的临时目录/模型配置
//! 帮手。集中在此，避免复制维护两份行为脚本。

use crate::model::{
    FinishReason, Modality, Model, ModelCapabilities, ModelConfig, ModelError, ModelEvent,
    ModelFactory, ModelProtocol, ModelRequest, ModelResponse, ProviderCredentials, Usage,
};
use crate::plugin::{
    DisposeError, PluginContext, PluginDescriptor, PluginError, PluginId, ServiceId,
};
use crate::plugins::services::{PROVIDER_SERVICE, PROVIDER_SERVICE_ID};
use crate::tool::ToolCall;
use crate::{
    EventSink, PermissionApprover, PermissionDecision, PermissionRequest, ProviderDescriptor,
    RunEvent, TrustedProjectApplication,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

mod tool_once;
pub use tool_once::ToolOnceScript;
pub mod mcp_discovery;
pub mod netiso;
pub mod network;

/// Small fully decodable PNG for provider/session tests. Image projection now
/// verifies MIME against magic, so tests that claim `image/png` must use an
/// image fixture instead of arbitrary text bytes.
pub fn png_bytes(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
    let mut output = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        width,
        height,
        image::Rgb(color),
    ))
    .write_to(&mut output, image::ImageFormat::Png)
    .expect("encode test PNG");
    output.into_inner()
}

const TEST_PROVIDER_ID: PluginId = PluginId::new("test.application_provider");
const TEST_PROVIDER_REQUIRES: &[ServiceId] = &[PROVIDER_SERVICE_ID];
const TEST_PROVIDER_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: TEST_PROVIDER_ID,
    scope: crate::plugin::ScopeKind::TrustedProject,
    provides: &[],
    requires: TEST_PROVIDER_REQUIRES,
    optional: &[],
};

const LIVE_GLM_PROVIDER_ID: PluginId = PluginId::new("test.live_glm_provider");
const LIVE_GLM_PROVIDER_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: LIVE_GLM_PROVIDER_ID,
    scope: crate::plugin::ScopeKind::TrustedProject,
    provides: &[],
    requires: TEST_PROVIDER_REQUIRES,
    optional: &[],
};

/// 注册脚本化 TestModel 的 provider 插件。
pub struct TestProviderPlugin {
    pub behavior: TestBehavior,
}

/// Explicitly armed paid-test provider. The Coding Plan key is read only when
/// a model instance is built and never enters persisted ProviderCredentials.
/// Tests using this plugin must remain ignored by default.
pub struct LiveGlmProviderPlugin;

impl crate::plugin::Plugin for LiveGlmProviderPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &LIVE_GLM_PROVIDER_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let providers = context
            .require(PROVIDER_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let lease = providers
            .register(context.owner(), Arc::new(LiveGlmFactory))
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        Ok(())
    }
}

struct LiveGlmFactory;

impl ModelFactory for LiveGlmFactory {
    fn protocol(&self) -> ModelProtocol {
        ModelProtocol::OpenAiCompatible
    }

    fn describe(&self, _credentials: &ProviderCredentials) -> ProviderDescriptor {
        ProviderDescriptor {
            protocol: self.protocol(),
            display_name: "GLM live test (process-local credential)".into(),
            fields: Vec::new(),
        }
    }

    fn build(
        &self,
        config: &ModelConfig,
        _credentials: &ProviderCredentials,
    ) -> Result<Box<dyn Model>, ModelError> {
        let key = std::env::var("CLAT_GLM_CODING_PLAN_KEY")
            .map_err(|_| ModelError::request("CLAT_GLM_CODING_PLAN_KEY is not set"))?;
        if key.is_empty() {
            return Err(ModelError::request("CLAT_GLM_CODING_PLAN_KEY is empty"));
        }
        Ok(Box::new(
            crate::providers::OpenAiCompatibleModel::from_runtime_fields(vec![key], config)?,
        ))
    }
}

/// 可组合 scripted model 接缝：Scenario Harness 经生产 Provider/Run
/// 路径驱动任意确定步骤，而不是为每个新病历继续扩张 `TestBehavior`
/// 枚举。实现必须自行串行化 cursor；RetryModel 会为每次请求重建
/// `TestModel`，所以脚本状态不能放在 model 实例里。
pub trait TestModelScript: Send + Sync {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        events: &mut dyn crate::model::ModelEventSink,
    ) -> Result<ModelResponse, ModelError>;
}

impl crate::plugin::Plugin for TestProviderPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &TEST_PROVIDER_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let providers = context
            .require(PROVIDER_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let lease = providers
            .register(
                context.owner(),
                Arc::new(TestFactory {
                    behavior: self.behavior.clone(),
                }),
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        Ok(())
    }
}

#[derive(Clone)]
#[allow(dead_code)]
pub enum TestBehavior {
    Success,
    /// 普通对话快速完成；compaction summary 持续到取消或 3 秒，用于
    /// 浏览器 F5 恢复活动压缩槽与取消控制的确定性验收。
    SlowCompaction,
    /// 标题请求慢 3s、普通对话快返回（验证旁路命名不阻塞 run）。
    SlowTitle,
    Failure,
    Cancel,
    Panic,
    /// 第一轮调用 todo_write 工具，第二轮完成：锁定 marker 落盘顺序。
    Todo,
    /// 内部摘要请求失败、正常 agent 请求成功：验证自动压缩降级不会
    /// 被误报为成功，也不会阻断本轮 run。
    CompactionFailure,
    /// 第一轮调用 write_file（Write 效果，必过权限门），拿到工具结果后
    /// 第二轮完成：供 exec 的权限批准/拒绝路径测试使用。
    WriteFile,
    /// 先立即输出 delta，再稍等后请求 write_file：用于锁定 stdout 在
    /// handle 发布前失败时，取消必须先于后续副作用生效。
    DeltaThenWrite,
    /// 第一轮调用 run_command，拿到工具结果后第二轮完成。
    /// 审批测试必须显式使用 ReadOnly，不能依赖 PW 执行逐次审。
    RunCommand,
    /// 显式请求命令网络，用于 Fresh IM 会话的升级审批边界。
    RunCommandNetwork,
    /// Gate every human turn independently for queue lifecycle verification.
    RunCommandPerTurn,
    /// 发出 `count` 个 128KiB 的 TextDelta 再完成：单请求制造 MB 级
    /// 事件流，确定性打满任何「内核缓冲 + SSE 队列」（INV-S7 慢消费
    /// 腿——轮数堆量会先撞 B1 花费护栏的每请求预留）。
    HugeDeltas {
        count: usize,
    },
    /// 每 `interval_ms` 发一个 16KiB TextDelta，共 `count` 个：秒级
    /// 持续流（PWA-4 刷新重建腿——run 进行中 F5，视图恢复且流式
    /// 继续不丢帧）；总字节数温和（journal 重放友好）。
    TimedDeltas {
        count: usize,
        interval_ms: u64,
    },
    /// Two reasoning lines with a visible streaming interval, followed by a
    /// settled answer. The PWA uses this to discriminate latest-line live
    /// disclosure from first-line settled disclosure.
    ReasoningDeltas,
    /// steering 确定性门闩：第一次模型调用阻塞到测试线程 steer() 并
    /// 放行（否则按取消退出），第二次调用观测 steering 用户项是否已
    /// 并入 items。
    Steer(Arc<SteerGate>),
    /// 连续 `calls` 次模型调用都请求 list_files（Read 效果，免审批），
    /// 之后的调用输出文本完成：驱动长工具循环场景（无轮次预算回归）。
    /// 计数经 `seen` 跨模型构建共享——RetryModel 每次请求都经工厂重建
    /// 模型，实例内状态不可靠；标题旁路请求直接返回短文本，不消耗
    /// 计数。工具往返各计 usage (1,1)，完成调用经 response() 计 (2,3)。
    ToolLoop {
        calls: usize,
        seen: Arc<std::sync::atomic::AtomicUsize>,
    },
    /// 第一轮调用 ask_user 工具（Pure 效果，免审批），拿到答案后第二
    /// 轮完成：端到端验证 ask-user 端口（asker 由 run 请求安装）。
    AskUser(Arc<ScriptedAsker>),
    /// 阶段 0 Agent Scenario Harness 使用的通用脚本。它仍经真实
    /// TestProviderPlugin → ModelFactory → RetryModel → Run 路径；
    /// 这里只提供确定 Provider 响应，不绕开工具/权限/持久化。
    Scripted(Arc<dyn TestModelScript>),
}

/// 脚本化 UserAsker：固定回传一个选项，并记录收到的问题供断言。
pub struct ScriptedAsker {
    pub selected: String,
    pub asked: Mutex<Vec<String>>,
}

impl crate::interaction::UserAsker for ScriptedAsker {
    fn ask(
        &self,
        question: crate::interaction::AskQuestion,
        _cancel: &crate::CancelToken,
    ) -> crate::interaction::AskAnswer {
        if let Ok(mut asked) = self.asked.lock() {
            asked.push(question.question.clone());
        }
        crate::interaction::AskAnswer::Selected(self.selected.clone())
    }
}

/// steering 测试的门闩：`entered` 标记第一次模型调用已开始（此后
/// steer() 必然晚于第一轮 drain），`released` 放行第一次调用返回，
/// `saw_steering` 由第二次调用回填。
#[derive(Default)]
pub struct SteerGate {
    entered: std::sync::atomic::AtomicBool,
    released: std::sync::atomic::AtomicBool,
    pub saw_steering: std::sync::atomic::AtomicBool,
}

impl SteerGate {
    pub fn wait_entered(&self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !self.entered.load(std::sync::atomic::Ordering::Acquire) {
            assert!(std::time::Instant::now() < deadline, "model never started");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    pub fn release(&self) {
        self.released
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

struct TestFactory {
    behavior: TestBehavior,
}

impl ModelFactory for TestFactory {
    fn protocol(&self) -> ModelProtocol {
        ModelConfig::default().protocol
    }

    fn describe(&self, _credentials: &ProviderCredentials) -> ProviderDescriptor {
        ProviderDescriptor {
            protocol: self.protocol(),
            display_name: "Application test".into(),
            fields: Vec::new(),
        }
    }

    fn build(
        &self,
        _config: &ModelConfig,
        _credentials: &ProviderCredentials,
    ) -> Result<Box<dyn Model>, ModelError> {
        Ok(Box::new(TestModel {
            behavior: self.behavior.clone(),
        }))
    }
}

struct TestModel {
    behavior: TestBehavior,
}

impl Model for TestModel {
    fn provider(&self) -> &str {
        "application-test"
    }

    fn model_id(&self) -> &str {
        "deterministic"
    }

    fn stream(
        &mut self,
        request: ModelRequest<'_>,
        events: &mut dyn crate::model::ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        // Auxiliary followups never consume the scripted foreground run.
        if request.tools.is_empty()
            && request
                .instructions
                .is_some_and(|text| text.starts_with("Suggest up to three distinct"))
        {
            return Ok(response(
                r#"["Review the changes","Add a focused check","Explain the result"]"#,
                FinishReason::Completed,
            ));
        }
        match &self.behavior {
            TestBehavior::Success => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                events.emit(ModelEvent::TextDelta {
                    delta: "done".into(),
                });
                // 流末 usage（真实 provider 的同时点，DeepSeek 经
                // stream_options.include_usage / GLM 默认）：驱动
                // journal 的 assistant/message.usage 与状态栏实时累计。
                events.emit(ModelEvent::Usage(Usage {
                    input_tokens: 120,
                    output_tokens: 30,
                    cached_input_tokens: Some(100),
                    reasoning_tokens: None,
                }));
                Ok(response("done", FinishReason::Completed))
            }
            TestBehavior::SlowCompaction => {
                let is_summary = request.tools.is_empty()
                    && request
                        .instructions
                        .is_some_and(|text| text.contains("context compaction"));
                if is_summary {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                    while std::time::Instant::now() < deadline && !request.cancel.is_cancelled() {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    if request.cancel.is_cancelled() {
                        Ok(response("", FinishReason::Cancelled))
                    } else {
                        Ok(response("slow compacted history", FinishReason::Completed))
                    }
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    events.emit(ModelEvent::TextDelta {
                        delta: "done".into(),
                    });
                    Ok(response("done", FinishReason::Completed))
                }
            }
            TestBehavior::SlowTitle => {
                // 按标题任务前缀识别，不绑定词数/CJK 限长措辞；普通对话快返回。
                let is_title = request.tools.is_empty()
                    && request
                        .instructions
                        .is_some_and(|text| text.starts_with("Generate a concise title"));
                if is_title {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    Ok(response("slow title", FinishReason::Completed))
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    events.emit(ModelEvent::TextDelta {
                        delta: "done".into(),
                    });
                    Ok(response("done", FinishReason::Completed))
                }
            }
            TestBehavior::Failure => {
                events.emit(ModelEvent::TextDelta {
                    delta: "partial".into(),
                });
                Err(ModelError::new("intentional failure"))
            }
            TestBehavior::Cancel => {
                events.emit(ModelEvent::TextDelta {
                    delta: "partial-cancel".into(),
                });
                while !request.cancel.is_cancelled() {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Ok(response("partial-cancel", FinishReason::Cancelled))
            }
            TestBehavior::Panic => {
                events.emit(ModelEvent::TextDelta {
                    delta: "partial-panic".into(),
                });
                panic!("intentional provider panic");
            }
            TestBehavior::Todo => {
                let has_todo_result = request.items.iter().any(|item| {
                    matches!(item, crate::model::ModelItem::ToolResult(result) if result.tool_name == "todo_write")
                });
                if has_todo_result {
                    events.emit(ModelEvent::TextDelta {
                        delta: "todos updated".into(),
                    });
                    Ok(response("todos updated", FinishReason::Completed))
                } else {
                    Ok(ModelResponse {
                        text: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "call-todo".into(),
                            name: "todo_write".into(),
                            arguments: json!({
                                "todos": [
                                    {"content": "write tests", "status": "in_progress"},
                                    {"content": "ship release", "status": "pending"},
                                ],
                            }),
                        }],
                        finish_reason: FinishReason::ToolCalls,
                        usage: None,
                        provider_response_id: None,
                        provider_state: Vec::new(),
                        reasoning: None,
                    })
                }
            }
            TestBehavior::CompactionFailure => {
                let is_summary = request.tools.is_empty()
                    && request
                        .instructions
                        .is_some_and(|text| text.contains("context compaction"));
                if is_summary {
                    Err(ModelError::transport("intentional compaction failure"))
                } else {
                    events.emit(ModelEvent::TextDelta {
                        delta: "done".into(),
                    });
                    Ok(response("done", FinishReason::Completed))
                }
            }
            TestBehavior::Steer(gate) => {
                let has_steering = request.items.iter().any(|item| {
                    matches!(
                        item,
                        crate::model::ModelItem::User { content }
                            if content
                                .iter()
                                .any(|part| matches!(part, crate::model::ContentPart::Text(text) if text == "also run the tests"))
                    )
                });
                if has_steering {
                    gate.saw_steering
                        .store(true, std::sync::atomic::Ordering::Release);
                    events.emit(ModelEvent::TextDelta {
                        delta: "steering handled".into(),
                    });
                    return Ok(response("steering handled", FinishReason::Completed));
                }
                gate.entered
                    .store(true, std::sync::atomic::Ordering::Release);
                while !gate.released.load(std::sync::atomic::Ordering::Acquire)
                    && !request.cancel.is_cancelled()
                {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                events.emit(ModelEvent::TextDelta {
                    delta: "first answer".into(),
                });
                Ok(response("first answer", FinishReason::Completed))
            }
            TestBehavior::ToolLoop { calls, seen } => {
                let is_title = request
                    .instructions
                    .is_some_and(|text| text.contains("Generate a concise title"));
                if is_title {
                    return Ok(response("loop title", FinishReason::Completed));
                }
                let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                if n > *calls {
                    events.emit(ModelEvent::TextDelta {
                        delta: "loop complete".into(),
                    });
                    return Ok(response("loop complete", FinishReason::Completed));
                }
                Ok(ModelResponse {
                    text: String::new(),
                    tool_calls: vec![ToolCall {
                        id: format!("call-loop-{n}"),
                        name: "list_files".into(),
                        arguments: json!({ "path": "." }),
                    }],
                    finish_reason: FinishReason::ToolCalls,
                    // 每次工具往返计 (1,1)：让续跑段的 usage 累计可断言
                    //（完成调用经 response() 计 (2,3)）。
                    usage: Some(Usage {
                        input_tokens: 1,
                        output_tokens: 1,
                        ..Usage::default()
                    }),
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            TestBehavior::AskUser(_) => {
                let has_ask_result = request.items.iter().any(|item| {
                    matches!(
                        item,
                        crate::model::ModelItem::ToolResult(result) if result.tool_name == "ask_user"
                    )
                });
                if has_ask_result {
                    events.emit(ModelEvent::TextDelta {
                        delta: "decision recorded".into(),
                    });
                    return Ok(response("decision recorded", FinishReason::Completed));
                }
                Ok(ModelResponse {
                    text: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call-ask".into(),
                        name: "ask_user".into(),
                        arguments: json!({
                            "question": "Which release channel should we ship?",
                            "options": [
                                { "label": "stable", "description": "recommended" },
                                { "label": "beta" },
                            ],
                        }),
                    }],
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            TestBehavior::Scripted(script) => {
                // 标题生成是 Application 的旁路内部模型请求，不属于
                // Scenario 声明的 agent step。与 ToolLoop 同纪律：不
                // 消耗脚本 cursor，否则首轮成功后的异步标题会让稳定
                // scenario 偶发多一步。
                // 只识别任务前缀，不绑定可能随标题策略变化的长度提示。
                let is_title = request.tools.is_empty()
                    && request
                        .instructions
                        .is_some_and(|text| text.starts_with("Generate a concise title"));
                if is_title {
                    // Report 不能被标题 worker 的调度时机污染：close
                    // 可能在它开始前取消，也可能让它先提交标题。固定
                    // 返回 Cancelled，使两条调度都不产生 session/title。
                    Ok(response("", FinishReason::Cancelled))
                } else {
                    script.stream(request, events)
                }
            }
            TestBehavior::WriteFile | TestBehavior::DeltaThenWrite => {
                let has_write_result = request.items.iter().any(|item| {
                    matches!(item, crate::model::ModelItem::ToolResult(result) if result.tool_name == "write_file")
                });
                if has_write_result {
                    events.emit(ModelEvent::TextDelta {
                        delta: "write attempted".into(),
                    });
                    Ok(response("write attempted", FinishReason::Completed))
                } else {
                    if matches!(self.behavior, TestBehavior::DeltaThenWrite) {
                        events.emit(ModelEvent::TextDelta {
                            delta: "partial-before-tool".into(),
                        });
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Ok(ModelResponse {
                        text: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "call-write".into(),
                            name: "write_file".into(),
                            arguments: json!({
                                "path": "generated.txt",
                                "content": "from headless test",
                                // This shared fixture intentionally replaces whole files;
                                // implicit-create/refusal controls use independent scripts.
                                "overwrite": true,
                            }),
                        }],
                        finish_reason: FinishReason::ToolCalls,
                        usage: None,
                        provider_response_id: None,
                        provider_state: Vec::new(),
                        reasoning: None,
                    })
                }
            }
            TestBehavior::HugeDeltas { count } => {
                let blob = "x".repeat(128 * 1024);
                for _ in 0..*count {
                    events.emit(ModelEvent::TextDelta {
                        delta: blob.clone(),
                    });
                    // 5ms 间隔：快消费者（毫秒级排空一帧）队列不积压，
                    // 慢消费者（零排空）必然溢出——两侧结果确定性分离。
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Ok(response("done", FinishReason::Completed))
            }
            TestBehavior::TimedDeltas { count, interval_ms } => {
                let blob = "x".repeat(16 * 1024);
                for _ in 0..*count {
                    events.emit(ModelEvent::TextDelta {
                        delta: blob.clone(),
                    });
                    std::thread::sleep(std::time::Duration::from_millis(*interval_ms));
                }
                Ok(response("done", FinishReason::Completed))
            }
            TestBehavior::ReasoningDeltas => {
                events.emit(ModelEvent::ReasoningDelta {
                    delta: "first thought\n".into(),
                });
                std::thread::sleep(std::time::Duration::from_millis(240));
                events.emit(ModelEvent::ReasoningDelta {
                    delta: "latest thought".into(),
                });
                std::thread::sleep(std::time::Duration::from_millis(500));
                events.emit(ModelEvent::TextDelta {
                    delta: "done".into(),
                });
                let mut result = response("done", FinishReason::Completed);
                result.reasoning = Some("first thought\nlatest thought".into());
                Ok(result)
            }
            TestBehavior::RunCommand
            | TestBehavior::RunCommandNetwork
            | TestBehavior::RunCommandPerTurn => {
                let current_items = if matches!(self.behavior, TestBehavior::RunCommandPerTurn) {
                    let start = request
                        .items
                        .iter()
                        .rposition(|item| matches!(item, crate::model::ModelItem::User { .. }))
                        .unwrap_or(0);
                    &request.items[start..]
                } else {
                    request.items
                };
                let has_command_result = current_items.iter().any(|item| {
                    matches!(item, crate::model::ModelItem::ToolResult(result) if result.tool_name == "run_command")
                });
                if has_command_result {
                    events.emit(ModelEvent::TextDelta {
                        delta: "command attempted".into(),
                    });
                    Ok(response("command attempted", FinishReason::Completed))
                } else {
                    Ok(ModelResponse {
                        text: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "call-run".into(),
                            name: "run_command".into(),
                            arguments: if matches!(self.behavior, TestBehavior::RunCommandNetwork) {
                                json!({"command": "echo from-serve-test", "network": true})
                            } else {
                                json!({"command": "echo from-serve-test"})
                            },
                        }],
                        finish_reason: FinishReason::ToolCalls,
                        usage: None,
                        provider_response_id: None,
                        provider_state: Vec::new(),
                        reasoning: None,
                    })
                }
            }
        }
    }
}

pub fn response(text: &str, finish_reason: FinishReason) -> ModelResponse {
    ModelResponse {
        text: text.into(),
        tool_calls: Vec::new(),
        finish_reason,
        usage: Some(Usage {
            input_tokens: 2,
            output_tokens: 3,
            ..Usage::default()
        }),
        provider_response_id: None,
        provider_state: Vec::new(),
        reasoning: None,
    }
}

/// 临时 storage/project 目录对（时间戳 + 纳秒保证唯一）。
pub fn roots(name: &str) -> (PathBuf, PathBuf) {
    // FL 族根因（2026-09-08 定案，96 线程放大环 + 全进程探针实证）：
    // 唯一性曾只靠 name+纳秒时钟——高并发下兄弟测试读到同一纳秒即
    // **共享临时根**（同 SessionId 断言、互删目录、根租约 base 冲突
    // 三证齐）。纳秒之外再叠进程内原子单调计数：同纳秒也不撞。
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("clat-application-{name}-{nanos}-{sequence}"));
    (base.join("storage"), base.join("project"))
}

/// Keep the JSON-escape regression dense on both platforms without creating
/// Windows-forbidden quote names. Repeated Windows separators are legal; the
/// project resolver normalizes them while the result retains the raw request.
pub fn escaped_read_fixture(root: &std::path::Path) -> (PathBuf, String) {
    if cfg!(windows) {
        return (root.join("f.txt"), format!(".{}f.txt", "\\".repeat(640)));
    }
    let mut dir = root.to_path_buf();
    for _ in 0..4 {
        dir = dir.join("\"".repeat(160));
    }
    std::fs::create_dir_all(&dir).expect("escaped fixture directories");
    let target = dir.join("f.txt");
    let requested = target.to_str().expect("UTF-8 fixture path").to_owned();
    (target, requested)
}

// Share the exact bounded cold-reopen policy with out-of-process Host tests.
#[path = "../tests/support/reopen_after_close.rs"]
mod cold_reopen;
pub use cold_reopen::reopen_after_close;

#[test]
fn reopen_after_close_retries_only_the_visibility_window() {
    use crate::application::ApplicationError;
    let attempts = std::sync::atomic::AtomicU8::new(0);
    // 前两次模拟内核可见性窗口内的误拒，第三次放行。
    let value = reopen_after_close(|| {
        if attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
            Err(ApplicationError::from_message(
                "another CLAT process holds this storage root; close it first",
            ))
        } else {
            Ok(7)
        }
    });
    assert_eq!(value, 7);
    // 其他错误立即透传，绝不轮询（计数 =1；若被当作窗口误拒重试，
    // 计数会走到 100）。
    let unrelated = std::sync::atomic::AtomicU8::new(0);
    let result = std::panic::catch_unwind(|| {
        reopen_after_close(|| -> Result<u8, ApplicationError> {
            unrelated.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(ApplicationError::from_message("storage is uninitialized"))
        })
    });
    assert!(result.is_err(), "unrelated errors must surface");
    assert_eq!(
        unrelated.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "unrelated errors must not be retried"
    );
}

pub fn configure_test_model(application: &TrustedProjectApplication) {
    let config = ModelConfig {
        model: "deterministic".into(),
        endpoint: "https://application-test.invalid".into(),
        // MM-2 W1：attach 门只放行 verified 视觉能力——附件路径的
        // 应用测试统一用已验证视觉的测试模型（等价 GLM flash 探针
        // 证据位），文本路径的拒绝语义由专门红测覆盖。
        capabilities: ModelCapabilities {
            input_modalities: vec![Modality::Text, Modality::Image],
            tool_result_modalities: vec![Modality::Text, Modality::Image],
            image_input_verified: true,
        },
        ..ModelConfig::default()
    };
    let credentials = ProviderCredentials::for_protocol(config.protocol);
    application
        .save_model_state(&config, &credentials)
        .expect("save test model");
}

#[allow(dead_code)]
pub fn configure_test_model_with_budget(
    application: &TrustedProjectApplication,
    max_context_tokens: u32,
) {
    let config = ModelConfig {
        model: "deterministic".into(),
        endpoint: "https://application-test.invalid".into(),
        output_limit: Some(256),
        max_context_tokens: Some(max_context_tokens),
        ..ModelConfig::default()
    };
    let credentials = ProviderCredentials::for_protocol(config.protocol);
    application
        .save_model_state(&config, &credentials)
        .expect("save test model");
}

#[derive(Clone)]
pub struct SharedEvents(pub Arc<Mutex<Vec<RunEvent>>>);

impl EventSink for SharedEvents {
    fn emit(&mut self, event: RunEvent) {
        self.0.lock().expect("events").push(event);
    }
}

/// 计数 approver：SessionWrite 免审意味着 todo run 期间零调用。
pub struct CountingApprover(pub Arc<std::sync::atomic::AtomicUsize>);

impl PermissionApprover for CountingApprover {
    fn decide(
        &self,
        _request: PermissionRequest,
        _cancel: &crate::model::CancelToken,
    ) -> PermissionDecision {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        PermissionDecision::Allow
    }
}

/// 测试临时目录清理（F-W1-b，CI run 32582790210）：cap-std 在 Windows
/// 打开目录时刻意不带 `FILE_SHARE_DELETE`（cap-primitives dir_options，
/// 防目录在沙箱路径查找下被改名/删除）——任何仍存活的 Dir capability
///（backend/service/lease 持有）都会让 `remove_dir_all` 报 os error 32。
/// Unix 允许删除打开中的文件/目录，历史测试因此在句柄仍持有时清理。
/// 语义：Unix 保持严格（清理失败即 panic，与原 `.expect("cleanup")`
/// 一致）；Windows 先短暂重试（后台线程异步关句柄），仍失败则容忍
/// 残留——清理是卫生不是验证，测试的有效断言此刻已全部通过，一次性
/// CI runner 上的残留无害。
pub fn cleanup_tree(root: &std::path::Path) {
    for attempt in 0..5 {
        match std::fs::remove_dir_all(root) {
            Ok(()) => return,
            Err(error) if attempt == 4 => {
                if !cfg!(windows) {
                    panic!("cleanup {}: {error}", root.display());
                }
                // Windows：句柄仍被存活对象持有——容忍残留（见上）。
                return;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20u64 << attempt)),
        }
    }
}

pub fn load_replay(storage_root: &std::path::Path) -> Vec<crate::session::replay::ReplayEvent> {
    let backend = crate::session::persistence::JsonlBackend::new(
        storage_root.join("sessions"),
        crate::session::persistence::JsonlCompression::Zstd,
        false,
    );
    let headers = backend.list_headers().expect("headers");
    let header = headers.first().expect("one session");
    let key = crate::session::key::SessionKey {
        project: crate::session::key::ProjectKey::from_cwd(&header.cwd.clone().expect("cwd")),
        id: header.id.clone(),
    };
    let mut events = Vec::new();
    backend
        .visit_from(&key, 0, &mut |event| {
            events.push(event.clone());
            Ok(())
        })
        .expect("visit");
    crate::session::replay::ReplayAdapter::fold(&events)
}

// ---------------------------------------------------------------------------
// AG-DOGFOOD-5 / J1：ORDER-1 + CONT-1 联合脚本（同一 Session 多步任务 +
// 用户明确续办）。供 serve Playwright 宿主与 TUI App 级测试共用。
//
// Run 1（用户要求两个具体产物 a.md / b.md）：
//   请求1 → todo_write 登记 4 项（write a、write b、verify、report） +
//           write_file(a.md)；
//   请求2（见 a.md 结果，延迟 250ms 的延迟结果腿）→ write_file(b.md) +
//           edit_file(b.md, 不存在 old_str)（可控制失败，is_error=true
//           但主 Run 继续）；
//   请求3（见失败结果）→ 正常 Completed，verify/report 两项保持 open——
//           「Run 完成 ≠ 任务已核验完成」的可观察未尽工作。
// Run 2（用户明确说 continue）：
//   请求4 → todo_write 把 4 项全部置 completed（仅剩余动作）；
//   请求5 → Completed "all artifacts verified and reported"。
// ---------------------------------------------------------------------------

pub struct Ag5JointScript;

impl Default for Ag5JointScript {
    fn default() -> Self {
        Self::new()
    }
}

impl Ag5JointScript {
    pub fn new() -> Self {
        Self
    }

    /// `todo_write` 的完整参数形状：`{"todos": [...]}`（裸数组会被工具
    /// 校验拒绝——"requires a `todos` array"）。
    fn todo_args(args: &[(&str, &str)]) -> serde_json::Value {
        serde_json::json!({"todos": args
            .iter()
            .map(|(content, status)| {
                serde_json::json!({"content": content, "status": status})
            })
            .collect::<Vec<_>>()})
    }

    fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments,
        }
    }

    fn tool_response(calls: Vec<ToolCall>) -> Result<ModelResponse, ModelError> {
        Ok(ModelResponse {
            text: String::new(),
            tool_calls: calls,
            finish_reason: FinishReason::ToolCalls,
            usage: Some(Usage {
                input_tokens: 1,
                output_tokens: 1,
                ..Usage::default()
            }),
            provider_response_id: None,
            provider_state: Vec::new(),
            reasoning: None,
        })
    }
}

impl TestModelScript for Ag5JointScript {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        events: &mut dyn crate::model::ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        // 标题旁路：不消耗脚本步。
        if request.tools.is_empty()
            && request
                .instructions
                .is_some_and(|text| text.starts_with("Generate a concise title"))
        {
            return Ok(response("ag5 joint title", FinishReason::Completed));
        }
        let has_result = |name: &str| {
            request.items.iter().any(|item| {
                matches!(item, crate::model::ModelItem::ToolResult(result) if result.tool_name == name)
            })
        };
        let latest_user = request
            .items
            .iter()
            .rev()
            .find_map(|item| match item {
                crate::model::ModelItem::User { content } => content.iter().find_map(|part| {
                    matches!(part, crate::model::ContentPart::Text(text) if !text.is_empty())
                        .then(|| part)
                        .and_then(|_| match part {
                            crate::model::ContentPart::Text(text) => Some(text.clone()),
                            _ => None,
                        })
                }),
                _ => None,
            })
            .unwrap_or_default();

        let todo_results = request
            .items
            .iter()
            .filter(|item| {
                matches!(item, crate::model::ModelItem::ToolResult(result) if result.tool_name == "todo_write")
            })
            .count();

        // —— Run 2：用户明确续办，仅执行剩余动作（todo 收口）——
        // 注意：跨 run 的工具结果会从模型表面修剪（transient context），
        // 续办首请求看到的 todo_results 从 0 重新计数；因此按当前计数
        // 生成唯一 call id（真实模型每次发射唯一 id），直到累计第 3 次
        // todo_write 结果到达才完成。
        if latest_user.contains("continue") {
            if todo_results >= 3 {
                let text = "all artifacts verified and reported";
                events.emit(ModelEvent::TextDelta {
                    delta: text.to_owned(),
                });
                return Ok(response(text, FinishReason::Completed));
            }
            return Self::tool_response(vec![Self::tool_call(
                &format!("call-ag5-cont-todos-{todo_results}"),
                "todo_write",
                Self::todo_args(&[
                    ("write a.md", "completed"),
                    ("write b.md", "completed"),
                    ("verify outputs", "completed"),
                    ("report results", "completed"),
                ]),
            )]);
        }

        // —— Run 1 ——
        if has_result("edit_file") {
            // 第二批已执行（含可控制失败）：先收口已完成项（第 2 次
            // todo_write），再正常完成——verify/report 保持 open，J1 的
            // 「Run 完成但仍有可观察未尽工作」。
            if todo_results >= 2 {
                let text = "phase one done — a.md and b.md exist; the b.md edit attempt failed";
                events.emit(ModelEvent::TextDelta {
                    delta: text.to_owned(),
                });
                return Ok(response(text, FinishReason::Completed));
            }
            return Self::tool_response(vec![Self::tool_call(
                "call-ag5-close-todos",
                "todo_write",
                Self::todo_args(&[
                    ("write a.md", "completed"),
                    ("write b.md", "completed"),
                    ("verify outputs", "in_progress"),
                    ("report results", "pending"),
                ]),
            )]);
        }
        if has_result("write_file") {
            // 延迟结果腿：第二批响应前先等 250ms。
            std::thread::sleep(std::time::Duration::from_millis(250));
            return Self::tool_response(vec![
                Self::tool_call(
                    "call-ag5-write-b",
                    "write_file",
                    serde_json::json!({
                        "path": "b.md",
                        "content": "# B\n\nsecond artifact\n",
                        // 共享宿主的多次 E2E 里 b.md 可能已存在：显式
                        // 覆盖写保持幂等（fresh project 目录行为不变）。
                        "overwrite": true,
                    }),
                ),
                Self::tool_call(
                    "call-ag5-edit-fail",
                    "edit_file",
                    serde_json::json!({
                        "path": "b.md",
                        "old_str": "THIS-MARKER-DOES-NOT-EXIST",
                        "new_str": "edited",
                    }),
                ),
            ]);
        }
        // 首请求：登记待办 + 产物一。
        Self::tool_response(vec![
            Self::tool_call(
                "call-ag5-todos",
                "todo_write",
                Self::todo_args(&[
                    ("write a.md", "completed"),
                    ("write b.md", "in_progress"),
                    ("verify outputs", "pending"),
                    ("report results", "pending"),
                ]),
            ),
            Self::tool_call(
                "call-ag5-write-a",
                "write_file",
                serde_json::json!({
                    "path": "a.md",
                    "content": "# A\n\nfirst artifact\n",
                    "overwrite": true,
                }),
            ),
        ])
    }
}

// ---------------------------------------------------------------------------
// AG-DOGFOOD-8 / ORDER-LIVE-1：PWA live 模型步/工具因果顺序脚本。
//
// 单个 Run（用户提示词含 "orderlive"；含 "again" 的第二跑换终文案）：
//   请求1 → 文本 delta（"opening two work files"）+ write_file(ag8-x.md) +
//           write_file(ag8-y.md)（两张卡）；
//   请求2（见 2 个结果）→ **无正文**，仅 read_file(ag8-x.md)（第三张卡；
//           锁定“无正文的中间模型步不得渲染空回复气泡”）；
//   请求3（见全部 3 个结果）→ 文本 delta（"final answer after tools"/
//           "final answer after rerun"）+ Completed。
// 期望 live/replay DOM：user → assistant(opening…) → 3 张工具卡 →
// assistant(final)。把 final 回填进工具前旧回复气泡的实现在此形状下暴露
// （AG-DOGFOOD-7 隔离 Chromium 红的复现形状）。
// ---------------------------------------------------------------------------

pub struct Ag8OrderLiveScript;

impl Default for Ag8OrderLiveScript {
    fn default() -> Self {
        Self::new()
    }
}

impl Ag8OrderLiveScript {
    pub fn new() -> Self {
        Self
    }

    fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments,
        }
    }

    fn tool_response(calls: Vec<ToolCall>) -> Result<ModelResponse, ModelError> {
        Ok(ModelResponse {
            text: String::new(),
            tool_calls: calls,
            finish_reason: FinishReason::ToolCalls,
            usage: Some(Usage {
                input_tokens: 1,
                output_tokens: 1,
                ..Usage::default()
            }),
            provider_response_id: None,
            provider_state: Vec::new(),
            reasoning: None,
        })
    }

    fn latest_user_text(request: &ModelRequest<'_>) -> String {
        request
            .items
            .iter()
            .rev()
            .find_map(|item| match item {
                crate::model::ModelItem::User { content } => content.iter().find_map(|part| {
                    matches!(part, crate::model::ContentPart::Text(text) if !text.is_empty())
                        .then(|| part)
                        .and_then(|_| match part {
                            crate::model::ContentPart::Text(text) => Some(text.clone()),
                            _ => None,
                        })
                }),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// 本轮（最后一个 user 消息之后）的工具结果数：同 Session 第二跑不把
    /// 上一跑的旧结果计入（脚本状态跨请求无状态，只能从模型表面推导）。
    fn turn_tool_results(request: &ModelRequest<'_>) -> usize {
        let mut count = 0usize;
        for item in request.items.iter().rev() {
            match item {
                crate::model::ModelItem::ToolResult(_) => count += 1,
                crate::model::ModelItem::User { .. } => break,
                _ => {}
            }
        }
        count
    }
}

impl TestModelScript for Ag8OrderLiveScript {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        events: &mut dyn crate::model::ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        // 标题旁路：不消耗脚本步。
        if request.tools.is_empty()
            && request
                .instructions
                .is_some_and(|text| text.starts_with("Generate a concise title"))
        {
            return Ok(response("ag8 orderlive title", FinishReason::Completed));
        }
        let latest_user = Self::latest_user_text(&request);
        if !latest_user.contains("orderlive") {
            return Ok(response("acknowledged", FinishReason::Completed));
        }
        // AG-DOGFOOD-8 / HIST-PWA-1 T2 慢驻留腿：首请求先流正文再驻留 3s，
        // 给浏览器侧在真实活跃 Run 内 queue steering 并在下一步边界被 claim
        // 的确定性窗口；见 ag8-hist-pwa.spec.js 的 steering 测试。
        if latest_user.contains("slowsteer") {
            let has_slow_result = request.items.iter().any(|item| {
                matches!(item, crate::model::ModelItem::ToolResult(result)
                    if result.tool_name == "write_file")
            });
            if has_slow_result {
                let text = "slow steer run finished";
                events.emit(ModelEvent::TextDelta {
                    delta: text.to_owned(),
                });
                return Ok(response(text, FinishReason::Completed));
            }
            let text = "holding the run open";
            events.emit(ModelEvent::TextDelta {
                delta: text.to_owned(),
            });
            std::thread::sleep(std::time::Duration::from_secs(3));
            return Self::tool_response(vec![Self::tool_call(
                "call-ag8-slow-write",
                "write_file",
                serde_json::json!({
                    "path": "ag8-slow.md",
                    "content": "# S\n\nslow steer artifact\n",
                    "overwrite": true,
                }),
            )]);
        }
        let tool_results = Self::turn_tool_results(&request);
        if tool_results == 0 {
            let text = "opening two work files";
            events.emit(ModelEvent::TextDelta {
                delta: text.to_owned(),
            });
            return Self::tool_response(vec![
                Self::tool_call(
                    "call-ag8-write-x",
                    "write_file",
                    serde_json::json!({
                        "path": "ag8-x.md",
                        "content": "# X\n\norderlive artifact one\n",
                        "overwrite": true,
                    }),
                ),
                Self::tool_call(
                    "call-ag8-write-y",
                    "write_file",
                    serde_json::json!({
                        "path": "ag8-y.md",
                        "content": "# Y\n\norderlive artifact two\n",
                        "overwrite": true,
                    }),
                ),
            ]);
        }
        if tool_results <= 2 {
            // 无正文中间步：只有第三张卡，不产生回复气泡。
            return Self::tool_response(vec![Self::tool_call(
                "call-ag8-read-x",
                "read_file",
                serde_json::json!({"path": "ag8-x.md"}),
            )]);
        }
        let text = if latest_user.contains("again") {
            "final answer after rerun"
        } else {
            "final answer after tools"
        };
        events.emit(ModelEvent::TextDelta {
            delta: text.to_owned(),
        });
        Ok(response(text, FinishReason::Completed))
    }
}
