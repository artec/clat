//! 跨模块共享的测试基座（仅测试构建）。
//!
//! application 集成测试与 exec（headless CLI）测试都需要向真实
//! `PROVIDER_SERVICE` 注册脚本化模型，并使用同样的临时目录/模型配置
//! 帮手。集中在此，避免复制维护两份行为脚本。

use crate::model::{
    FinishReason, Model, ModelConfig, ModelError, ModelEvent, ModelFactory, ModelProtocol,
    ModelRequest, ModelResponse, ProviderCredentials, Usage,
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

const TEST_PROVIDER_ID: PluginId = PluginId::new("test.application_provider");
const TEST_PROVIDER_REQUIRES: &[ServiceId] = &[PROVIDER_SERVICE_ID];
const TEST_PROVIDER_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: TEST_PROVIDER_ID,
    scope: crate::plugin::ScopeKind::TrustedProject,
    provides: &[],
    requires: TEST_PROVIDER_REQUIRES,
    optional: &[],
};

/// 注册脚本化 TestModel 的 provider 插件。
pub(crate) struct TestProviderPlugin {
    pub(crate) behavior: TestBehavior,
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
pub(crate) enum TestBehavior {
    Success,
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
}

/// 脚本化 UserAsker：固定回传一个选项，并记录收到的问题供断言。
pub(crate) struct ScriptedAsker {
    pub(crate) selected: String,
    pub(crate) asked: Mutex<Vec<String>>,
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
pub(crate) struct SteerGate {
    entered: std::sync::atomic::AtomicBool,
    released: std::sync::atomic::AtomicBool,
    pub(crate) saw_steering: std::sync::atomic::AtomicBool,
}

impl SteerGate {
    pub(crate) fn wait_entered(&self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !self.entered.load(std::sync::atomic::Ordering::Acquire) {
            assert!(std::time::Instant::now() < deadline, "model never started");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    pub(crate) fn release(&self) {
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
            TestBehavior::SlowTitle => {
                // 标题请求（由 instructions 识别）慢 3s；普通对话快返回。
                let is_title = request
                    .instructions
                    .is_some_and(|text| text.contains("Generate a concise title"));
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
                let is_summary = request
                    .instructions
                    .is_some_and(|text| text.contains("summarizing a coding-agent conversation"));
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
        }
    }
}

pub(crate) fn response(text: &str, finish_reason: FinishReason) -> ModelResponse {
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
pub(crate) fn roots(name: &str) -> (PathBuf, PathBuf) {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let base = std::env::temp_dir().join(format!("clat-application-{name}-{unique}"));
    (base.join("storage"), base.join("project"))
}

pub(crate) fn configure_test_model(application: &TrustedProjectApplication) {
    let config = ModelConfig {
        model: "deterministic".into(),
        endpoint: "https://application-test.invalid".into(),
        ..ModelConfig::default()
    };
    let credentials = ProviderCredentials::for_protocol(config.protocol);
    application
        .save_model_state(&config, &credentials)
        .expect("save test model");
}

#[allow(dead_code)]
pub(crate) fn configure_test_model_with_budget(
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
pub(crate) struct SharedEvents(pub(crate) Arc<Mutex<Vec<RunEvent>>>);

impl EventSink for SharedEvents {
    fn emit(&mut self, event: RunEvent) {
        self.0.lock().expect("events").push(event);
    }
}

/// 计数 approver：SessionWrite 免审意味着 todo run 期间零调用。
pub(crate) struct CountingApprover(pub(crate) Arc<std::sync::atomic::AtomicUsize>);

impl PermissionApprover for CountingApprover {
    fn decide(&self, _request: PermissionRequest) -> PermissionDecision {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        PermissionDecision::Allow
    }
}
