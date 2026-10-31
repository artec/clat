use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupRequest {
    operation_id: String,
    expected_revision: String,
    #[serde(default)]
    verify: bool,
    preset: Option<String>,
    api_key: Option<String>,
    profile: Option<ModelProfileEdit>,
}
#[derive(Clone, Serialize)]
pub struct VendorHelp {
    pub vendor: &'static str,
    pub keys_url: &'static str,
    pub help_url: &'static str,
}
pub(super) fn vendor_help() -> Vec<VendorHelp> {
    [
        (
            "DeepSeek",
            "https://platform.deepseek.com/api_keys",
            "https://api-docs.deepseek.com/",
        ),
        (
            "GLM Coding Plan",
            "https://open.bigmodel.cn/usercenter/proj-mgmt/apikeys",
            "https://docs.bigmodel.cn/cn/coding-plan/overview",
        ),
        (
            "Qwen Token Plan",
            "https://bailian.console.aliyun.com/",
            "https://help.aliyun.com/zh/model-studio/",
        ),
        (
            "Kimi Coding Plan",
            "https://www.kimi.com/code/console",
            "https://www.kimi.com/code/docs/",
        ),
        (
            "Hy Token Plan",
            "https://console.cloud.tencent.com/lkeap/api",
            "https://cloud.tencent.com/document/product/1772",
        ),
    ]
    .map(|(vendor, keys_url, help_url)| VendorHelp {
        vendor,
        keys_url,
        help_url,
    })
    .to_vec()
}
impl TrustedProjectApplication {
    /// A retry finds its receipt before checking the now-advanced revision.
    /// Receipts are bounded and never evicted while this Host is alive.
    pub fn model_setup_start(
        &self,
        value: Value,
        apply: bool,
    ) -> Result<ModelSetupOperation, ApplicationError> {
        let signature = format!("{apply}:{:x}", Sha256::digest(value.to_string()));
        let mut req: SetupRequest =
            serde_json::from_value(value).map_err(|_| setup_error("invalid"))?;
        if uuid::Uuid::parse_str(&req.operation_id).is_err() {
            return Err(setup_error("invalid"));
        }
        let _guard = self
            .host_storage
            .model_updates
            .lock()
            .expect("model updates");
        let mut operations = self
            .host_storage
            .model_setup
            .operations
            .lock()
            .expect("setup operations");
        if let Some(op) = operations.get(&req.operation_id) {
            if op.signature != signature {
                return Err(setup_error("operation_conflict"));
            }
            let mut view = op.status.lock().expect("setup status").clone();
            if view.saved && view.revision != self.setup_revision() {
                view.state = "stale".into();
            }
            return Ok(view);
        }
        if req.expected_revision != self.setup_revision() {
            return Err(setup_error("changed"));
        }
        if operations.len() >= 1024 {
            return Err(setup_error("capacity"));
        }
        let result = self.apply_setup_request(&mut req, apply);
        let (state, failure) = match result {
            Ok(()) => ("saved", None),
            Err(error) => ("failed", Some(error.presentation().key)),
        };
        let (config, credentials) = self.model_state()?;
        let op = Arc::new(Operation {
            signature,
            cancel: CancelToken::new(),
            status: Mutex::new(ModelSetupOperation {
                operation_id: req.operation_id.clone(),
                revision: self.setup_revision(),
                model: config.model.clone(),
                saved: failure.is_none(),
                state: state.into(),
                failure,
            }),
        });
        operations.insert(req.operation_id.clone(), op.clone());
        drop(operations);
        if op.status.lock().expect("setup status").saved && (req.verify || !apply) {
            if configuration_state(&config, &credentials) != "configured" {
                worker::finish(&op, "failed", Some("setup.missing_credentials"));
            } else {
                worker::start(op.clone(), self.providers.clone(), config, credentials);
            }
        }
        Ok(op.status.lock().expect("setup status").clone())
    }
    fn apply_setup_request(
        &self,
        req: &mut SetupRequest,
        apply: bool,
    ) -> Result<(), ApplicationError> {
        if !apply {
            if req.preset.is_some() || req.profile.is_some() || req.api_key.is_some() {
                return Err(setup_error("invalid"));
            }
            let (config, credentials) = self.model_state()?;
            return if configuration_state(&config, &credentials) == "configured" {
                Ok(())
            } else {
                Err(setup_error("missing_credentials"))
            };
        }
        match (req.preset.take(), req.profile.take()) {
            (Some(id), None) => self.setup_preset(&id, req.api_key.take()),
            (None, Some(edit)) if req.api_key.is_none() => {
                let (name, config, credentials) = self
                    .prepare_model_profile(edit)
                    .map_err(|_| setup_error("invalid"))?;
                self.control
                    .save_and_activate_profile(&name, &config, &credentials)
                    .map_err(|_| setup_error("save_failed"))?;
                self.host_storage.publish_models(&config, &credentials);
                Ok(())
            }
            _ => Err(setup_error("invalid")),
        }
    }
    fn setup_preset(&self, id: &str, key: Option<String>) -> Result<(), ApplicationError> {
        super::super::validate_key(key.as_deref()).map_err(|_| setup_error("invalid"))?;
        let preset = crate::presets::preset_by_id(id).ok_or_else(|| setup_error("invalid"))?;
        let mut config = ModelConfig::default();
        preset.apply(&mut config);
        let mut credentials = self
            .vendor_key(config.protocol, &config.endpoint)
            .unwrap_or_else(|| ProviderCredentials::for_protocol(config.protocol));
        if let Some(key) = key {
            credentials.set_value(0, key);
        }
        if configuration_state(&config, &credentials) != "configured" {
            return Err(setup_error("missing_credentials"));
        }
        self.save_model_state_locked(&config, &credentials)
            .map_err(|_| setup_error("save_failed"))
    }
}
