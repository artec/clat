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
        let mut batch = Vec::new();
        let mut tokens = 0;
        let mut batch_images = 0u64;
        let mut batch_bytes = 0u64;
        let tools = self.tools.clone();
        let system = (!system.is_empty()).then_some(system);
        for item in region {
            if self.cancelled() {
                return Err(crate::i18n::UiMessage::new("compaction.cancelled").into());
            }
            for part in self.multimodal_parts(item)? {
                let (images, bytes, _) =
                    crate::model::image_projection_totals(std::slice::from_ref(&part));
                let cost = estimate_item(&part);
                let exceeds_images = images > 0
                    && batch_images > 0
                    && (batch_images.saturating_add(images)
                        > crate::model::ImageProjectionBudget::MAX_REQUEST_IMAGES as u64
                        || batch_bytes.saturating_add(bytes)
                            > crate::model::ImageProjectionBudget::MAX_REQUEST_IMAGE_BYTES);
                if !batch.is_empty() && (tokens + cost > self.input_budget() || exceeds_images) {
                    summaries.push(self.request_items(
                        std::mem::take(&mut batch),
                        system,
                        &tools,
                    )?);
                    tokens = 0;
                    batch_images = 0;
                    batch_bytes = 0;
                }
                batch.push(part);
                tokens += cost;
                batch_images += images;
                batch_bytes = batch_bytes.saturating_add(bytes);
            }
        }
        if !batch.is_empty() {
            summaries.push(self.request_items(batch, system, &tools)?);
        }
        if summaries.len() == 1 {
            return Ok(summaries.remove(0));
        }
        let merged = summaries
            .into_iter()
            .map(ModelItem::user_text)
            .collect::<Vec<_>>();
        self.summarize(&merged, true)
    }
    fn multimodal_parts(
        &self,
        item: &ModelItem,
    ) -> Result<Vec<ModelItem>, crate::i18n::PresentedText> {
        let mut images: Vec<_> = crate::model::model_item_image_parts(item)
            .cloned()
            .collect();
        let visual_cost = estimate_item(&ModelItem::User {
            content: images.clone(),
        });
        let budget = self
            .input_budget()
            .checked_sub(visual_cost)
            .filter(|n| *n >= 64)
            .ok_or("summary images exceed model capacity")?;
        let rendered = render_group(&[std::slice::from_ref(item)]);
        Ok(chunks::text_units(&rendered, budget)
            .into_iter()
            .map(|text| {
                let mut content = vec![crate::model::ContentPart::Text(text)];
                // Retain the original images once, with the first fragment of their message.
                content.append(&mut images);
                ModelItem::User { content }
            })
            .collect())
    }
    pub(super) fn request_items(
        &mut self,
        mut items: Vec<ModelItem>,
        system: Option<&str>,
        tools: &[crate::tool::ToolDefinition],
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
                if recovery == 0 {
                    // INV-CP3（CP-C3）：请求本就零工具——工具调用是幻觉，
                    // 同源重试一次；再犯降级（degraded outcome），不产生
                    // 运行级硬错。
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
