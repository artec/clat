//! One owner for summary request policy, map/reduce, recovery and attempt accounting.
use super::*;
use crate::i18n::{PresentedText, UiMessage};
use crate::providers::{ModelBuildFn, RetryPolicy, retry_model_with};
use std::sync::Mutex;
use std::time::Duration;

pub(super) const FALLBACK_INPUT_TOKENS: usize = 24_000;
const GENERATION_LIMIT: u32 = 65_536;
const RECOVERY_LIMIT: u32 = 65_536;

fn instructions_for(guidance: &str, tokens: usize) -> String {
    format!(
        "{guidance} Keep the complete checkpoint within approximately {tokens} tokens; preserve all eight sections."
    )
}

#[cfg(all(test, feature = "runtime-tests"))]
pub(crate) fn directive_for_test(config: &ModelConfig) -> String {
    Policy::new(config).unwrap().instructions()
}

#[derive(Clone)]
pub(super) struct Policy {
    input: usize,
    summary_limit: usize,
    output: u32,
    window: Option<u32>,
    guidance: Arc<str>,
}
impl Policy {
    #[cfg(test)]
    pub(super) fn new(config: &ModelConfig) -> Result<Self, String> {
        Self::with_guidance(config, Arc::from(SUMMARY_INSTRUCTIONS))
    }
    pub(super) fn with_guidance(config: &ModelConfig, guidance: Arc<str>) -> Result<Self, String> {
        // Generation includes reasoning; the visible summary keeps its separate bound.
        let output = config
            .max_context_tokens
            .map(|window| (window / 4).min(GENERATION_LIMIT))
            .unwrap_or(8192);
        let input_ceiling = config
            .max_context_tokens
            .map(|window| {
                (window as usize).saturating_sub(
                    output as usize
                        + estimate_tokens_str(&instructions_for(&guidance, output as usize))
                        + MARGIN_TOKENS
                        + 32,
                )
            })
            .unwrap_or(FALLBACK_INPUT_TOKENS);
        let input = input_ceiling;
        if input < 256 {
            return Err(format!(
                "model context window {} is too small for compaction reserves",
                config.max_context_tokens.unwrap_or_default()
            ));
        }
        Ok(Self {
            input,
            // Two accepted summaries plus headings must fit the next reduction.
            summary_limit: (output as usize).min(input / 3 - 64),
            output,
            window: config.max_context_tokens,
            guidance,
        })
    }
    fn instructions(&self) -> String {
        instructions_for(&self.guidance, self.summary_limit.saturating_sub(8))
    }
    fn recovery_output(&self, text: &str) -> u32 {
        let items = [ModelItem::user_text(text)];
        let input = crate::model::estimate_request_tokens(Some(&self.instructions()), &items, &[]);
        let headroom = self
            .window
            .map(|window| {
                u64::from(window).saturating_sub(input.saturating_add(MARGIN_TOKENS as u64))
            })
            .unwrap_or(u64::from(RECOVERY_LIMIT));
        self.output
            .saturating_mul(2)
            .min(RECOVERY_LIMIT)
            .min(headroom.min(u64::from(u32::MAX)) as u32)
    }
}

pub(super) struct SummaryResult {
    pub summary: Result<String, PresentedText>,
    pub usage: Usage,
    pub output_limit: u32,
}

pub(super) struct SummaryJob {
    model: Box<dyn Model>,
    usage: Arc<Mutex<Usage>>,
    policy: Policy,
    cancel: CancelToken,
    active: bool,
    config: ModelConfig,
    instructions: String,
    tools: Vec<crate::tool::ToolDefinition>,
}
impl SummaryJob {
    pub(super) fn new(
        providers: &Arc<ProviderRegistry>,
        request: &CompactionRequest<'_>,
        guidance: Arc<str>,
    ) -> Result<Self, PresentedText> {
        let policy = Policy::with_guidance(request.config, guidance)?;
        let usage = Arc::new(Mutex::new(Usage::default()));
        let build: ModelBuildFn = {
            let providers = providers.clone();
            let config = crate::model::internal_text_config(request.config);
            let credentials = request.credentials.clone();
            let ledger = request.spend_ledger.clone();
            let usage = usage.clone();
            Box::new(move || {
                Ok(Box::new(accounting::SummaryModel {
                    inner: providers.build(&config, &credentials)?,
                    ledger: ledger.clone(),
                    usage: usage.clone(),
                }))
            })
        };
        let model = retry_model_with(
            request.config.protocol.to_string(),
            request.config.model.clone(),
            build,
            RetryPolicy {
                max_attempts: 2,
                backoff: vec![Duration::from_secs(1)],
                // Retry only this failed request; history length never limits the job.
                ..Default::default()
            },
        );
        Ok(Self {
            model,
            usage,
            policy,
            cancel: request.cancel.clone(),
            active: request.active_checkpoints.is_some(),
            config: request.config.clone(),
            instructions: request.instructions.clone(),
            tools: request.tool_definitions.clone(),
        })
    }
    pub(super) fn execute(mut self, region: &[ModelItem]) -> SummaryResult {
        let mut run = SummaryRun::new(self.model.as_mut(), &self.cancel, self.policy);
        let summary =
            crate::model::modality_preflight(region, &self.config.capabilities, &self.config.model)
                .map_err(Into::into)
                .and_then(|()| {
                    run.summarize_context(region, self.active, &self.instructions, &self.tools)
                });
        let output_limit = run.max_output;
        let usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        SummaryResult {
            summary,
            usage,
            output_limit,
        }
    }
}

pub(super) struct SummaryRun<'a> {
    model: &'a mut dyn Model,
    cancel: &'a CancelToken,
    policy: Policy,
    max_output: u32,
    system: Option<String>,
    pub(super) tools: Vec<crate::tool::ToolDefinition>,
}
impl<'a> SummaryRun<'a> {
    pub(super) fn new(model: &'a mut dyn Model, cancel: &'a CancelToken, policy: Policy) -> Self {
        let max_output = policy.output;
        Self {
            model,
            cancel,
            policy,
            max_output,
            system: None,
            tools: Vec::new(),
        }
    }
    pub(super) fn summarize(
        &mut self,
        region: &[ModelItem],
        active: bool,
    ) -> Result<String, PresentedText> {
        if self.cancel.is_cancelled() {
            return Err(UiMessage::new("compaction.cancelled").into());
        }
        let mut groups = self.prepare(region, active)?;
        loop {
            let mut summaries = Vec::new();
            for group in groups {
                summaries.push(self.request(&group.join("\n\n"))?);
            }
            if summaries.len() == 1 {
                return Ok(summaries.remove(0));
            }
            if summaries.is_empty() {
                return Err("empty summary reduction".into());
            }
            let count = summaries.len();
            let labelled = summaries
                .into_iter()
                .enumerate()
                .map(|(index, text)| format!("## Part {}\n{text}", index + 1))
                .collect();
            groups = chunks::groups(labelled, self.policy.input)?;
            if groups.len() >= count {
                return Err("summary parts cannot be reduced within the model window".into());
            }
        }
    }
    fn prepare(
        &self,
        region: &[ModelItem],
        active: bool,
    ) -> Result<Vec<Vec<String>>, PresentedText> {
        let units = chunks::rendered_units(region, active)
            .iter()
            .flat_map(|text| chunks::text_units(text, self.policy.input))
            .collect();
        Ok(chunks::groups(units, self.policy.input)?)
    }
    fn request(&mut self, text: &str) -> Result<String, PresentedText> {
        if estimate_tokens_str(text) > self.policy.input {
            return Err("summary input exceeds the per-request capacity".into());
        }
        self.request_items(
            vec![ModelItem::user_text(text)],
            self.system.clone().as_deref(),
            &self.tools.clone(),
        )
    }
    pub(super) fn accept(
        &self,
        response: crate::model::ModelResponse,
        recovery: usize,
        input: &mut String,
        output: &mut u32,
    ) -> Result<Option<String>, PresentedText> {
        if !response.tool_calls.is_empty() {
            return Err("compaction summary must not call tools".into());
        }
        if response.finish_reason == FinishReason::Cancelled {
            return Err(UiMessage::new("compaction.provider_cancelled").into());
        }
        let answer = response.text.trim();
        if response.finish_reason == FinishReason::Completed && !answer.is_empty() {
            if !super::structure::valid(answer) {
                return Err(UiMessage::new("compaction.invalid_structure").into());
            }
            let tokens = estimate_tokens_str(answer);
            if tokens <= self.policy.summary_limit {
                return Ok(Some(answer.to_owned()));
            }
            if recovery == 0 && tokens <= self.policy.input {
                // A complete candidate may be condensed; an incomplete answer never replaces its source.
                *input = answer.to_owned();
                *output = self.policy.output;
                return Ok(None);
            }
            return Err(UiMessage::new("compaction.summary_too_large")
                .arg("tokens", tokens as u64)
                .arg("limit", self.policy.summary_limit as u64)
                .into());
        }
        let recoverable = response.finish_reason == FinishReason::MaxTokens
            || (response.finish_reason == FinishReason::Completed && answer.is_empty());
        if recovery == 0 && recoverable {
            if response.finish_reason == FinishReason::MaxTokens {
                *output = self.policy.recovery_output(input);
            }
            return Ok(None);
        }
        if answer.is_empty() {
            return Err(UiMessage::new("compaction.empty_summary").into());
        }
        Err(UiMessage::new("compaction.incomplete_summary")
            .arg("finish", format!("{:?}", response.finish_reason))
            .into())
    }
}

impl SummaryRun<'_> {
    pub(super) fn directive(&self) -> String {
        self.policy.instructions()
    }
    pub(super) fn output(&self) -> u32 {
        self.policy.output
    }
    pub(super) fn bind_prefix(
        &mut self,
        system: &str,
        tools: &[crate::tool::ToolDefinition],
    ) -> Result<(), PresentedText> {
        self.system = (!system.is_empty()).then(|| system.to_owned());
        self.tools = tools.to_vec();
        let cost =
            crate::model::estimate_request_tokens(self.system.as_deref(), &[], tools) as usize;
        self.policy.input = self
            .policy
            .input
            .checked_sub(cost)
            .filter(|n| *n >= 256)
            .ok_or("summary guidance exceeds capacity")?;
        self.policy.summary_limit = self.policy.summary_limit.min(self.policy.input / 3 - 64);
        Ok(())
    }
    pub(super) fn input_budget(&self) -> usize {
        self.policy.input
    }
    pub(super) fn cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
    pub(super) fn fits(
        &self,
        items: &[ModelItem],
        system: Option<&str>,
        tools: &[crate::tool::ToolDefinition],
        output: u32,
    ) -> bool {
        match self.policy.window {
            Some(window) => {
                crate::model::estimate_request_tokens(system, items, tools)
                    .saturating_add(u64::from(output))
                    .saturating_add(MARGIN_TOKENS as u64)
                    <= u64::from(window)
            }
            None => {
                crate::model::estimate_request_tokens(system, items, tools)
                    <= (FALLBACK_INPUT_TOKENS + estimate_tokens_str(&self.directive()) + 32) as u64
            }
        }
    }
    pub(super) fn available_generation(
        &self,
        items: &[ModelItem],
        system: Option<&str>,
        tools: &[crate::tool::ToolDefinition],
        output: u32,
    ) -> u32 {
        self.policy.window.map_or(output, |window| {
            u64::from(window)
                .saturating_sub(
                    crate::model::estimate_request_tokens(system, items, tools)
                        .saturating_add(MARGIN_TOKENS as u64),
                )
                .min(u64::from(output)) as u32
        })
    }
    pub(super) fn stream_summary(
        &mut self,
        system: Option<&str>,
        items: &[ModelItem],
        tools: &[crate::tool::ToolDefinition],
        output: u32,
    ) -> Result<crate::model::ModelResponse, PresentedText> {
        self.max_output = self.max_output.max(output);
        self.model
            .stream(
                ModelRequest {
                    instructions: system,
                    items,
                    tools,
                    options: &ModelOptions {
                        output_limit: Some(output),
                        ..Default::default()
                    },
                    cancel: self.cancel,
                },
                &mut Vec::new(),
            )
            .map_err(|error| format!("summary request failed: {error}").into())
    }
}

#[cfg(test)]
#[path = "guidance_tests.rs"]
mod guidance_tests;
