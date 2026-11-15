use super::*;

const FILE: &str = "git-settings.json";
const MAX_INSTRUCTIONS_BYTES: usize = 4096;

#[derive(Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct GitSettings {
    language: String,
    pub(super) auto_stage: bool,
    pub(super) auto_push: bool,
    #[serde(alias = "pushInstructions")]
    pub(super) commit_instructions: String,
}

impl Default for GitSettings {
    fn default() -> Self {
        Self {
            language: "system".into(),
            auto_stage: false,
            auto_push: false,
            commit_instructions: String::new(),
        }
    }
}

impl GitSettings {
    fn patch(&mut self, value: &Value) -> Result<(), String> {
        let fields = value.as_object().ok_or("invalid_request")?;
        for (key, value) in fields {
            match key.as_str() {
                "language" => {
                    let locale = value.as_str().ok_or("invalid_language")?;
                    if !["system", "en", "zh-Hans", "zh-Hant"].contains(&locale) {
                        return Err("invalid_language".into());
                    }
                    self.language = locale.into();
                }
                "autoStage" => self.auto_stage = value.as_bool().ok_or("invalid_request")?,
                "autoPush" => self.auto_push = value.as_bool().ok_or("invalid_request")?,
                "commitInstructions" | "pushInstructions" => {
                    let instructions = value.as_str().ok_or("invalid_commit_instructions")?;
                    if instructions.len() > MAX_INSTRUCTIONS_BYTES || instructions.contains('\0') {
                        return Err("invalid_commit_instructions".into());
                    }
                    self.commit_instructions = instructions.into();
                }
                _ => return Err("invalid_request".into()),
            }
        }
        Ok(())
    }
}

impl GitService {
    pub(super) fn preferences(&self) -> Result<GitSettings, String> {
        self.store.load_existing(FILE)
    }

    pub(super) fn settings(&self, method: &str, value: &Value) -> Result<Value, String> {
        if method == "git.settings.set" {
            let _guard = self.settings_write.lock().map_err(|_| "git_busy")?;
            let mut settings = self.preferences()?;
            settings.patch(value)?;
            self.store.save(FILE, &settings)?;
            return Ok(json!(settings));
        }
        Ok(json!(self.preferences()?))
    }
}
