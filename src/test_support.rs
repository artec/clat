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
                    behavior: self.behavior,
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

#[derive(Clone, Copy)]
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
            behavior: self.behavior,
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
        match self.behavior {
            TestBehavior::Success => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                events.emit(ModelEvent::TextDelta {
                    delta: "done".into(),
                });
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
