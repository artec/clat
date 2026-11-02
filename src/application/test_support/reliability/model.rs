use super::*;
use crate::model::{
    Model, ModelError, ModelEvent, ModelEventSink, ModelFactory, ModelRequest, ModelResponse,
};
use crate::plugin::{Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind};
use std::sync::atomic::AtomicUsize;

pub(super) struct Capture {
    pub out: PathBuf,
    pub guidance: Option<Arc<str>>,
    pub allowed_tools: Vec<String>,
    pub next: AtomicUsize,
    pub stage: Mutex<String>,
    pub cancelling: AtomicBool,
    pub entered: AtomicBool,
    pub safety_failure: AtomicBool,
}
impl Capture {
    pub fn new(out: PathBuf) -> Self {
        Self {
            out,
            guidance: None,
            allowed_tools: vec!["phase9_resource".into(), "phase9_publish".into()],
            next: AtomicUsize::new(0),
            stage: Mutex::new("setup".into()),
            cancelling: AtomicBool::new(false),
            entered: AtomicBool::new(false),
            safety_failure: AtomicBool::new(false),
        }
    }
    pub fn stage(&self, name: &str) {
        *self.stage.lock().unwrap() = name.into();
    }
}
pub(super) struct Provider {
    pub capture: Arc<Capture>,
    pub state: Arc<tools::State>,
}
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("test.phase9"),
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: &[
        crate::plugins::services::PROVIDER_SERVICE_ID,
        crate::plugins::services::TOOL_SERVICE_ID,
    ],
    optional: &[],
};
impl Plugin for Provider {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }
    fn mount(&self, c: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let providers = c
            .require(crate::plugins::services::PROVIDER_SERVICE)
            .map_err(|e| PluginError::new(e.to_string()))?;
        let lease = providers
            .register(c.owner(), Arc::new(Factory(self.capture.clone())))
            .map_err(|e| PluginError::new(e.to_string()))?;
        c.defer(move || {
            lease
                .revoke()
                .map_err(|e| crate::plugin::DisposeError::new(e.to_string()))
        });
        let registry = c
            .require(crate::plugins::services::TOOL_SERVICE)
            .map_err(|e| PluginError::new(e.to_string()))?;
        for tool in [
            Arc::new(tools::Resource(self.state.clone())) as Arc<dyn crate::Tool>,
            Arc::new(tools::Publish(self.state.clone())),
        ] {
            let lease = registry
                .register(c.owner(), tool)
                .map_err(|e| PluginError::new(e.to_string()))?;
            c.defer(move || {
                lease
                    .revoke()
                    .map_err(|e| crate::plugin::DisposeError::new(e.to_string()))
            });
        }
        Ok(())
    }
}
pub(super) struct Factory(pub Arc<Capture>);
pub(super) fn build(config: &crate::ModelConfig, capture: &Arc<Capture>) -> Box<dyn Model> {
    Factory(capture.clone())
        .build(
            config,
            &crate::ProviderCredentials::for_protocol(config.protocol),
        )
        .unwrap()
}
impl ModelFactory for Factory {
    fn protocol(&self) -> crate::ModelProtocol {
        crate::ModelProtocol::OpenAiCompatible
    }
    fn describe(&self, _: &crate::ProviderCredentials) -> crate::model::ProviderDescriptor {
        crate::model::ProviderDescriptor {
            protocol: self.protocol(),
            display_name: "Synthetic GLM acceptance".into(),
            fields: Vec::new(),
        }
    }
    fn build(
        &self,
        config: &crate::ModelConfig,
        _: &crate::ProviderCredentials,
    ) -> Result<Box<dyn Model>, ModelError> {
        let key_name = if config.model.starts_with("deepseek") {
            "CLAT_HARNESS2_DEEPSEEK_KEY"
        } else {
            "CLAT_GLM_CODING_PLAN_KEY"
        };
        let key = std::env::var(key_name)
            .map_err(|_| ModelError::request("explicit live credential missing"))?;
        if key.is_empty() {
            return Err(ModelError::request("explicit live credential empty"));
        }
        let inner =
            crate::providers::OpenAiCompatibleModel::from_runtime_fields(vec![key], config)?;
        Ok(Box::new(Observed {
            inner,
            capture: self.0.clone(),
            output_limit: config.output_limit,
        }))
    }
}
struct Observed {
    inner: crate::providers::OpenAiCompatibleModel,
    capture: Arc<Capture>,
    output_limit: Option<u32>,
}
impl Model for Observed {
    fn provider(&self) -> &str {
        self.inner.provider()
    }
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }
    fn stream(
        &mut self,
        r: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        if r.tools
            .iter()
            .any(|t| !self.capture.allowed_tools.contains(&t.name))
        {
            self.capture.safety_failure.store(true, Ordering::SeqCst);
            return Err(ModelError::request(
                "unsafe tool exposure in synthetic campaign",
            ));
        }
        let number = self.capture.next.fetch_add(1, Ordering::SeqCst);
        let stage = self.capture.stage.lock().unwrap().clone();
        let items = r
            .items
            .iter()
            .map(|item| format!("{item:?}"))
            .collect::<Vec<_>>();
        write_new(
            &self
                .capture
                .out
                .join(format!("request-{number:04}-start.json")),
            &json!({
                "stage":stage,"instructions":r.instructions,"items":items,
                "tools":r.tools.iter().map(|t|json!({"name":t.name,"description":t.description,"schema":t.input_schema,"effect":t.effect.to_string(),"strict":t.strict})).collect::<Vec<_>>(),
                "model":self.inner.model_id(),"provider_options":r.options.provider_options,"temperature":r.options.temperature,"parallel_tool_calls":r.options.parallel_tool_calls,
                "output_limit":r.options.output_limit,"estimate":crate::model::estimate_request_tokens(r.instructions,r.items,r.tools),
                "images":r.items.iter().flat_map(crate::model::model_item_image_parts).count(),
                "summary":crate::test_support::is_summary_request(&r)
            }),
        );
        let start = std::time::Instant::now();
        let mut tap = Tap {
            events,
            capture: &self.capture,
            usage: None,
            observations: Vec::new(),
        };
        let framing_limit = (r.options.output_limit.or(self.output_limit).unwrap_or(8192) as usize)
            .saturating_mul(1024)
            .clamp(4 * 1024 * 1024, 64 * 1024 * 1024);
        let (result, wire) =
            crate::providers::openai_compatible::observation_tests::observe_with_limit(
                framing_limit,
                || self.inner.stream(r, &mut tap),
            );
        let wire = json!({"request_body":wire.body,"consumed_sse":String::from_utf8_lossy(&wire.bytes),"overflow":wire.overflow,"framing_limit":framing_limit,"events":tap.observations});
        write_new(
            &self
                .capture
                .out
                .join(format!("request-{number:04}-wire.json")),
            &wire,
        );
        let usage = result
            .as_ref()
            .ok()
            .and_then(|r| r.usage.as_ref())
            .or(tap.usage.as_ref());
        let usage=usage.map(|u|json!({"input":u.input_tokens,"output":u.output_tokens,"cached":u.cached_input_tokens,"reasoning":u.reasoning_tokens}));
        let outcome = match &result {
            Ok(response) => {
                json!({"text":response.text,"reasoning":response.reasoning,"finish":format!("{:?}",response.finish_reason),"tool_calls":response.tool_calls})
            }
            Err(error) => {
                json!({"error":error.to_string(),"error_kind":format!("{:?}",error.kind())})
            }
        };
        write_new(
            &self
                .capture
                .out
                .join(format!("request-{number:04}-end.json")),
            &json!({"stage":stage,"elapsed_ms":start.elapsed().as_millis(),"usage":usage,"cost":null,"cancelled":r.cancel.is_cancelled(),"wire_comparison":super::wire::compare(&wire,&outcome),"outcome":outcome}),
        );
        result
    }
}
struct Tap<'a> {
    events: &'a mut dyn ModelEventSink,
    capture: &'a Capture,
    usage: Option<crate::Usage>,
    observations: Vec<Value>,
}
impl ModelEventSink for Tap<'_> {
    fn emit(&mut self, event: ModelEvent) {
        self.observations.push(match &event {
            ModelEvent::TextDelta { delta } => json!({"kind":"text","delta":delta}),
            ModelEvent::ReasoningDelta { delta } => json!({"kind":"reasoning","delta":delta}),
            ModelEvent::ToolCallCompleted { call } => json!({"kind":"tool_call","call":call}),
            _ => json!({"kind":"other","event":format!("{event:?}")}),
        });
        if let ModelEvent::Usage(usage) = &event {
            self.usage = Some(usage.clone());
        }
        if self.capture.cancelling.load(Ordering::SeqCst) {
            self.capture.entered.store(true, Ordering::SeqCst);
        }
        self.events.emit(event);
    }
}
