//! Replay the effective request before the final directive; split losslessly only when necessary.
use super::summary::SummaryRun;
use super::*;

impl SummaryRun<'_> {
    pub(super) fn summarize_context(
        &mut self,
        region: &[ModelItem],
        active: bool,
        system: &str,
        tools: &[crate::tool::ToolDefinition],
    ) -> Result<String, crate::i18n::PresentedText> {
        let mut items = region.to_vec();
        items.push(ModelItem::user_text(self.directive()));
        if self.fits(
            &items,
            (!system.is_empty()).then_some(system),
            tools,
            self.output(),
        ) {
            return self.request_items(
                region.to_vec(),
                (!system.is_empty()).then_some(system),
                tools,
            );
        }
        // Preserve actual visual parts in fallback instead of converting them into invented descriptions.
        if region
            .iter()
            .any(|item| crate::model::model_item_image_parts(item).next().is_some())
        {
            self.bind_prefix(system, tools)?;
            return self.multimodal_chunks(region, system);
        }
        self.bind_prefix(system, tools)?;
        self.summarize(region, active)
    }
    fn multimodal_chunks(
        &mut self,
        region: &[ModelItem],
        system: &str,
    ) -> Result<String, crate::i18n::PresentedText> {
        let mut summaries = Vec::new();
        for item in region {
            let images = crate::model::model_item_image_parts(item)
                .cloned()
                .collect::<Vec<_>>();
            let rendered = render_group(&[std::slice::from_ref(item)]);
            let image_items = [ModelItem::User {
                content: images.clone(),
            }];
            let visual_cost =
                crate::model::estimate_request_tokens(None, &image_items, &[]) as usize;
            let budget = self
                .input_budget()
                .checked_sub(visual_cost)
                .filter(|n| *n >= 64)
                .ok_or("summary images exceed model capacity")?;
            let units = chunks::text_units(&rendered, budget);
            for (index, text) in units.into_iter().enumerate() {
                let mut content = vec![crate::model::ContentPart::Text(text)];
                if index == 0 {
                    content.extend(images.clone());
                }
                summaries.push(self.request_items(
                    vec![ModelItem::User { content }],
                    (!system.is_empty()).then_some(system),
                    &self.tools.clone(),
                )?);
            }
        }
        let merged = summaries
            .into_iter()
            .map(ModelItem::user_text)
            .collect::<Vec<_>>();
        self.summarize(&merged, true)
    }
    pub(super) fn request_items(
        &mut self,
        mut items: Vec<ModelItem>,
        system: Option<&str>,
        mut tools: &[crate::tool::ToolDefinition],
    ) -> Result<String, crate::i18n::PresentedText> {
        let mut output = self.output();
        let mut recovery_text = String::new();
        for recovery in 0..2 {
            if self.cancelled() {
                return Err(crate::i18n::UiMessage::new("compaction.cancelled").into());
            }
            let mut request_items = items.clone();
            request_items.push(ModelItem::user_text(self.directive()));
            if !self.fits(&request_items, system, tools, output) {
                return Err("summary request exceeds model capacity".into());
            }
            let response = self.stream_summary(system, &request_items, tools, output)?;
            if self.cancelled() {
                return Err(crate::i18n::UiMessage::new("compaction.cancelled").into());
            }
            if !response.tool_calls.is_empty() || response.finish_reason == FinishReason::ToolCalls
            {
                if recovery == 0 && !tools.is_empty() {
                    // The historical tools help maintain the request prefix. If
                    // the model proposes a new call, retry from the same source
                    // with no callable tools and discard that response.
                    tools = &[];
                    continue;
                }
                return Err(crate::i18n::UiMessage::new("compaction.summary_tool_call").into());
            }
            if let Some(answer) =
                self.accept(response, recovery, &mut recovery_text, &mut output)?
            {
                return Ok(answer);
            }
            if !recovery_text.is_empty() {
                items = vec![ModelItem::user_text(recovery_text.clone())];
            }
            let mut next = items.clone();
            next.push(ModelItem::user_text(self.directive()));
            output = self.available_generation(&next, system, tools, output);
        }
        Err("summary recovery exhausted".into())
    }
}
