//! One owner for summary request policy, map/reduce, recovery and attempt accounting.
use super::*;
use crate::i18n::{PresentedText, UiMessage};
use crate::providers::{ModelBuildFn, RetryPolicy, retry_model_with};
use std::sync::Mutex;
use std::time::Duration;

pub(super) const FALLBACK_INPUT_TOKENS: usize = 24_000;
const GENERATION_LIMIT: u32 = 8192;
const RECOVERY_LIMIT: u32 = 16_384;

fn instructions(characters: usize) -> String {
    format!(
        "{SUMMARY_INSTRUCTIONS} Keep the final summary within {characters} characters, including spaces."
    )
}

#[derive(Clone, Copy)]
pub(super) struct Policy {
    input: usize,
    summary_limit: usize,
    output: u32,
    window: Option<u32>,
}
impl Policy {
    pub(super) fn new(config: &ModelConfig) -> Result<Self, String> {
        // Generation includes reasoning; the visible summary keeps its separate bound.
        let output = config
            .max_context_tokens
            .map(|window| (window / 4).clamp(256, GENERATION_LIMIT))
            .unwrap_or(GENERATION_LIMIT);
        let input_ceiling = config
            .max_context_tokens
            .map(|window| {
                (window as usize).saturating_sub(
                    output as usize
                        + estimate_tokens_str(&instructions(SUMMARY_OUTPUT_LIMIT as usize))
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
            summary_limit: (SUMMARY_OUTPUT_LIMIT as usize + MARGIN_TOKENS).min(input / 2 - 64),
            output,
            window: config.max_context_tokens,
        })
    }
    fn instructions(&self) -> String {
        instructions((SUMMARY_OUTPUT_LIMIT as usize).min(self.summary_limit.saturating_sub(8)))
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
}
impl SummaryJob {
    pub(super) fn new(
        providers: &Arc<ProviderRegistry>,
        request: &CompactionRequest<'_>,
    ) -> Result<Self, PresentedText> {
        let policy = Policy::new(request.config)?;
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
        })
    }
    pub(super) fn execute(mut self, region: &[ModelItem]) -> SummaryResult {
        let mut run = SummaryRun::new(self.model.as_mut(), &self.cancel, self.policy);
        let summary = run.summarize(region, self.active);
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
}
impl<'a> SummaryRun<'a> {
    pub(super) fn new(model: &'a mut dyn Model, cancel: &'a CancelToken, policy: Policy) -> Self {
        Self {
            model,
            cancel,
            policy,
            max_output: policy.output,
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
            return Err(format!(
                "summary input needs {} tokens, above the per-request budget of {}",
                estimate_tokens_str(text),
                self.policy.input
            )
            .into());
        }
        let mut input = text.to_owned();
        let instructions = self.policy.instructions();
        let mut output = self.policy.output;
        for recovery in 0..2 {
            if self.cancel.is_cancelled() {
                return Err(UiMessage::new("compaction.cancelled").into());
            }
            let items = [ModelItem::user_text(&input)];
            let options = ModelOptions {
                output_limit: Some(output),
                ..Default::default()
            };
            self.max_output = self.max_output.max(output);
            let response = self.model.stream(
                ModelRequest {
                    instructions: Some(&instructions),
                    items: &items,
                    tools: &[],
                    options: &options,
                    cancel: self.cancel,
                },
                &mut Vec::new(),
            );
            if self.cancel.is_cancelled() {
                return Err(UiMessage::new("compaction.cancelled").into());
            }
            let response = response.map_err(|error| format!("summary request failed: {error}"))?;
            if let Some(answer) = self.accept(response, recovery, &mut input, &mut output)? {
                return Ok(answer);
            }
        }
        unreachable!("each recovery attempt either returns or exhausts its bound")
    }
    fn accept(
        &self,
        response: crate::model::ModelResponse,
        recovery: usize,
        input: &mut String,
        output: &mut u32,
    ) -> Result<Option<String>, PresentedText> {
        if response.finish_reason == FinishReason::Cancelled {
            return Err(UiMessage::new("compaction.provider_cancelled").into());
        }
        let answer = response.text.trim();
        if response.finish_reason == FinishReason::Completed && !answer.is_empty() {
            let tokens = estimate_tokens_str(answer);
            if answer.chars().count() <= SUMMARY_MAX_CHARS && tokens <= self.policy.summary_limit {
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
