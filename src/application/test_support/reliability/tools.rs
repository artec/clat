use super::*;
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
pub(super) struct State {
    pub reads: Mutex<Vec<u64>>,
    pub artifact: Mutex<Option<Value>>,
    pub publishes: AtomicUsize,
    pub hold: AtomicBool,
    pub entered: AtomicBool,
    pub released: AtomicBool,
    pub ledger: AtomicBool,
    pub trial: AtomicUsize,
}
pub(super) struct Resource(pub Arc<State>);
pub(super) struct Publish(pub Arc<State>);
impl crate::Tool for Resource {
    fn definition(&self) -> crate::ToolDefinition {
        crate::ToolDefinition { name:"phase9_resource".into(),description:"Read one synthetic evidence record by fixed numeric ID, 0 through 7. No filesystem or network access.".into(),
            input_schema:json!({"type":"object","properties":{"id":{"type":"integer","minimum":0,"maximum":7}},"required":["id"],"additionalProperties":false}),effect:crate::tool::ToolEffect::Pure,strict:true }
    }
    fn invoke(
        &self,
        args: &Value,
        _: &Project,
        cancel: &crate::CancelToken,
    ) -> Result<Value, crate::ToolError> {
        let id = args
            .get("id")
            .and_then(Value::as_u64)
            .filter(|id| *id < 8)
            .ok_or_else(|| crate::ToolError::new("invalid fixed resource ID"))?;
        if args.as_object().is_none_or(|o| o.len() != 1) || cancel.is_cancelled() {
            return Err(crate::ToolError::new(
                "invalid or cancelled resource request",
            ));
        }
        if id == 4 && self.0.hold.load(Ordering::SeqCst) {
            self.0.entered.store(true, Ordering::SeqCst);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !self.0.released.load(Ordering::SeqCst) && !cancel.is_cancelled() {
                if std::time::Instant::now() >= deadline {
                    return Err(crate::ToolError::new("steering witness not released"));
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        self.0.reads.lock().unwrap().push(id);
        let mut result = json!({"id":id,"evidence":format!("HEAD_31\n{}\nTAIL_97",corpus::noise(3000)),"status":"observation_only_not_acceptance"});
        if self.0.ledger.load(Ordering::SeqCst) {
            result["amount"] = json!(17 + id * 3 + self.0.trial.load(Ordering::SeqCst) as u64 * 10);
            result["confirmation"] = json!(if matches!(id, 3 | 6) {
                "unconfirmed"
            } else {
                "local_checked"
            });
        }
        Ok(result)
    }
}
impl crate::Tool for Publish {
    fn definition(&self) -> crate::ToolDefinition {
        crate::ToolDefinition{name:"phase9_publish".into(),description:"Save one synthetic JSON report to a fixed in-memory artifact. No path or command accepted. Publish only when explicitly asked.".into(),
            input_schema:json!({"type":"object","properties":{"report":{"type":"string"}},"required":["report"],"additionalProperties":false}),effect:crate::tool::ToolEffect::SessionWrite,strict:true}
    }
    fn invoke(
        &self,
        args: &Value,
        _: &Project,
        cancel: &crate::CancelToken,
    ) -> Result<Value, crate::ToolError> {
        if args.as_object().is_none_or(|o| o.len() != 1) || cancel.is_cancelled() {
            return Err(crate::ToolError::new("invalid or cancelled publication"));
        }
        let value: Value = serde_json::from_str(
            args["report"]
                .as_str()
                .ok_or_else(|| crate::ToolError::new("missing report"))?,
        )
        .map_err(|_| crate::ToolError::new("invalid report JSON"))?;
        let mut artifact = self.0.artifact.lock().unwrap();
        if artifact.is_some() {
            return Err(crate::ToolError::new("artifact already published"));
        }
        *artifact = Some(value);
        self.0.publishes.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"saved":true,"scope":"synthetic_artifact_only"}))
    }
}
