//! context implementation behind the stable model contract.
use super::*;

/// 保守 token 估算（INV-C8 同规：ASCII ~4 chars/token，非 ASCII
/// ≥1 token/char，每串 +8 常数）。
fn estimate_tokens_conservative(text: &str) -> usize {
    let ascii = text.chars().filter(char::is_ascii).count();
    let other = text.chars().count() - ascii;
    ascii / 4 + other + 8
}

/// Model-facing image parts in their provider projection order. Tool-result
/// images are deliberately included: they are emitted as a following user
/// image message by both provider adapters and must consume the same context
/// budget as top-level user/assistant images.
pub(crate) fn model_item_image_parts(item: &ModelItem) -> impl Iterator<Item = &ContentPart> {
    let parts = match item {
        ModelItem::User { content } | ModelItem::Assistant { content, .. } => content.as_slice(),
        ModelItem::ToolResult(result) => result.image_parts.as_slice(),
        ModelItem::ToolCall(_) | ModelItem::ProviderState(_) => &[],
    };
    parts
        .iter()
        .filter(|part| matches!(part, ContentPart::Image { .. }))
}

/// MS-1 发前模态预检：投影后的请求仍含图片部件而活跃模型不收图时
/// 立即明确失败（DSH `llm-deepseek/adapter.ts` 的 UNSUPPORTED_CONTENT
/// 同型）——绝不把厂商 400 透传给用户，也绝不静默把图换成文本
/// （替换文本 = 篡改用户消息，与 `PendingMessage::model_parts` 同一
/// 立场）。工具结果图像经 [`model_item_image_parts`] 一并计入。
/// 调用点在图片预算投影**之后**：已被投影卸载为占位文本的旧图不在
/// 请求里，不触发预检——预算卸载是 MM-2 既有语义，不因纯文本模型
/// 而收紧。
pub(crate) fn modality_preflight(
    items: &[ModelItem],
    capabilities: &ModelCapabilities,
    model_id: &str,
) -> Result<(), String> {
    let images = items.iter().flat_map(model_item_image_parts).count();
    if images > 0 && !capabilities.accepts_image_input() {
        return Err(format!(
            "this model ({model_id}) does not accept image input but the request still carries \
             {images} image part(s) (images already in this session's history included); switch \
             back to a vision model (e.g. GLM 5.3 Flash) or start a fresh session with /new"
        ));
    }
    Ok(())
}

/// Conservative per-item estimate shared by request preflight, compaction,
/// `/context`, steering, and goal continuation. Keeping ToolResult images in
/// this walker closes the otherwise easy-to-miss recursive visual-cost gap.
pub(crate) fn estimate_model_item_tokens(item: &ModelItem) -> u64 {
    let mut tokens = 16usize;
    match item {
        ModelItem::User { content } | ModelItem::Assistant { content, .. } => {
            for part in content {
                match part {
                    ContentPart::Text(text) => {
                        tokens += estimate_tokens_conservative(text);
                    }
                    ContentPart::Image { path, .. } => {
                        tokens += crate::media::estimate_image_tokens(std::path::Path::new(path))
                            as usize;
                    }
                }
            }
        }
        ModelItem::ToolResult(result) => {
            tokens += serde_json::to_string(item)
                .map(|text| estimate_tokens_conservative(&text))
                .unwrap_or(64);
            for part in &result.image_parts {
                match part {
                    ContentPart::Text(text) => {
                        tokens += estimate_tokens_conservative(text);
                    }
                    ContentPart::Image { path, .. } => {
                        tokens += crate::media::estimate_image_tokens(std::path::Path::new(path))
                            as usize;
                    }
                }
            }
        }
        ModelItem::ToolCall(_) | ModelItem::ProviderState(_) => {
            tokens += serde_json::to_string(item)
                .map(|text| estimate_tokens_conservative(&text))
                .unwrap_or(64);
        }
    }
    tokens as u64
}

/// FP-01 预留制的请求估算（非 tokenizer，宁可高估）：instructions +
/// 对话 items + 工具定义全部计入——兼容端点每轮请求都会真实计费
/// 这些 input（累计制账单的同构面）。
pub fn estimate_request_tokens(
    instructions: Option<&str>,
    items: &[ModelItem],
    tools: &[crate::tool::ToolDefinition],
) -> u64 {
    let mut tokens = 0usize;
    if let Some(text) = instructions {
        tokens += estimate_tokens_conservative(text);
    }
    for item in items {
        tokens = tokens.saturating_add(estimate_model_item_tokens(item) as usize);
    }
    for definition in tools {
        let schema = serde_json::to_string(&definition.input_schema)
            .map(|text| estimate_tokens_conservative(&text))
            .unwrap_or(256);
        tokens += estimate_tokens_conservative(&definition.name)
            + estimate_tokens_conservative(&definition.description)
            + schema;
    }
    tokens as u64
}

fn image_projection_totals(items: &[ModelItem]) -> (u64, u64, u64) {
    let mut images = 0u64;
    let mut bytes = 0u64;
    let mut tokens = 0u64;
    for part in items.iter().flat_map(model_item_image_parts) {
        let ContentPart::Image { path, .. } = part else {
            unreachable!("image walker yields only image parts")
        };
        images = images.saturating_add(1);
        tokens = tokens.saturating_add(crate::media::estimate_image_tokens(std::path::Path::new(
            path,
        )));
        // An unreadable image is not credited with zero bytes. The adapter
        // will turn it into a path-free unavailable notice, while projection
        // treats it as over-budget and removes it when it is old.
        bytes = bytes.saturating_add(
            std::fs::symlink_metadata(path)
                .ok()
                .filter(|metadata| metadata.file_type().is_file())
                .map_or(u64::MAX, |metadata| metadata.len()),
        );
    }
    (images, bytes, tokens)
}

fn image_projection_token_limit(
    budget: &ImageProjectionBudget,
    output_limit: Option<u32>,
) -> Option<u64> {
    let window = u64::from(budget.max_context_tokens?);
    // Quantize the 80% pressure line so small estimator/config changes do not
    // churn the provider prefix. Output reserve belongs inside the same line.
    let pressure = window.saturating_mul(8) / 10;
    let quantized = pressure / IMAGE_OFFLOAD_QUANTUM_TOKENS * IMAGE_OFFLOAD_QUANTUM_TOKENS;
    Some(quantized.saturating_sub(u64::from(output_limit.unwrap_or(4096))))
}

fn image_projection_is_over_budget(
    items: &[ModelItem],
    instructions: Option<&str>,
    tools: &[crate::tool::ToolDefinition],
    options: &ModelOptions,
    budget: &ImageProjectionBudget,
) -> bool {
    let (images, bytes, _) = image_projection_totals(items);
    if images > budget.max_request_images as u64 || bytes > budget.max_request_image_bytes {
        return true;
    }
    image_projection_token_limit(budget, options.output_limit)
        .is_some_and(|limit| estimate_request_tokens(instructions, items, tools) > limit)
}

fn replace_oldest_image(item: &mut ModelItem) -> bool {
    let parts = match item {
        ModelItem::User { content } | ModelItem::Assistant { content, .. } => content,
        ModelItem::ToolResult(result) => &mut result.image_parts,
        ModelItem::ToolCall(_) | ModelItem::ProviderState(_) => return false,
    };
    let Some(part) = parts
        .iter_mut()
        .find(|part| matches!(part, ContentPart::Image { .. }))
    else {
        return false;
    };
    *part = ContentPart::Text(IMAGE_OFFLOAD_PLACEHOLDER.into());
    true
}

/// Produce the exact model-facing item view for a run boundary. Older images
/// are replaced in recursive provider order until all three request budgets
/// fit. Images at and after the latest user turn are protected: if that new
/// turn cannot fit on its own, fail before provider I/O rather than silently
/// degrading the user's just-submitted content.
pub(crate) fn project_items_for_image_budget(
    items: &[ModelItem],
    instructions: Option<&str>,
    tools: &[crate::tool::ToolDefinition],
    options: &ModelOptions,
) -> Result<(Vec<ModelItem>, ImageProjectionReport), String> {
    let Some(budget) = options.image_projection.as_ref() else {
        let (images, bytes, tokens) = image_projection_totals(items);
        return Ok((
            items.to_vec(),
            ImageProjectionReport {
                original_images: images,
                retained_images: images,
                retained_bytes: bytes,
                retained_tokens: tokens,
                ..ImageProjectionReport::default()
            },
        ));
    };
    let (original_images, _, _) = image_projection_totals(items);
    let mut projected = items.to_vec();
    let protected_start = projected
        .iter()
        .rposition(|item| matches!(item, ModelItem::User { .. }))
        .unwrap_or(0);
    let mut offloaded_images = 0u64;
    let mut first_offloaded_image = None;
    let mut ordinal = 0u64;

    if image_projection_is_over_budget(&projected, instructions, tools, options, budget) {
        for item_index in 0..protected_start {
            loop {
                let images_before = model_item_image_parts(&projected[item_index]).count() as u64;
                if images_before == 0 {
                    break;
                }
                ordinal = ordinal.saturating_add(1);
                if !replace_oldest_image(&mut projected[item_index]) {
                    break;
                }
                first_offloaded_image.get_or_insert(ordinal);
                offloaded_images = offloaded_images.saturating_add(1);
                if !image_projection_is_over_budget(
                    &projected,
                    instructions,
                    tools,
                    options,
                    budget,
                ) {
                    break;
                }
            }
            if !image_projection_is_over_budget(&projected, instructions, tools, options, budget) {
                break;
            }
        }
    }

    if image_projection_is_over_budget(&projected, instructions, tools, options, budget) {
        let (images, bytes, _) = image_projection_totals(&projected[protected_start..]);
        return Err(format!(
            "the current request remains above the frozen visual/context budget after all older images were omitted (current turn: {images} images, {bytes} bytes); remove images, reduce the prompt, or choose a model with a larger context window"
        ));
    }
    let (retained_images, retained_bytes, retained_tokens) = image_projection_totals(&projected);
    Ok((
        projected,
        ImageProjectionReport {
            original_images,
            retained_images,
            retained_bytes,
            retained_tokens,
            offloaded_images,
            first_offloaded_image,
        },
    ))
}

/// Frozen per-run bounds for deterministic image offload. These are distinct
/// from per-message admission and the final serialized HTTP-body fence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageProjectionBudget {
    pub max_context_tokens: Option<u32>,
    pub max_request_images: usize,
    pub max_request_image_bytes: u64,
}

impl ImageProjectionBudget {
    pub const MAX_REQUEST_IMAGES: usize = 12;
    pub const MAX_REQUEST_IMAGE_BYTES: u64 = 20_000_000;

    pub fn for_config(config: &ModelConfig) -> Self {
        Self {
            max_context_tokens: config.max_context_tokens,
            max_request_images: Self::MAX_REQUEST_IMAGES,
            max_request_image_bytes: Self::MAX_REQUEST_IMAGE_BYTES,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ImageProjectionReport {
    pub original_images: u64,
    pub retained_images: u64,
    pub retained_bytes: u64,
    pub retained_tokens: u64,
    pub offloaded_images: u64,
    pub first_offloaded_image: Option<u64>,
}

pub(crate) const IMAGE_OFFLOAD_PLACEHOLDER: &str =
    "[older image omitted from this request: visual context budget exceeded]";
const IMAGE_OFFLOAD_QUANTUM_TOKENS: u64 = 1024;
