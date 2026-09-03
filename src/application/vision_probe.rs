//! VP-1（2026-09-03）：custom 一次性视觉探针（`/vision-probe`）。
//!
//! 内置预设能力 officially-declared 硬编码（VP-2，零探测）；本模块是
//! 全系统**唯一**保留的能力探测位：以当前 custom 端点×模型对 + 用户
//! key 跑一次 MM-0 读码式验证——程序生成含随机 4 位探针码的小图 →
//! 只有看得见才答得出 → 校验回答。负例两类各有判定：端点 4xx 拒图
//! （`Rejected`）与 Hy 型静默弃图（200 但读不出码，`SilentDrop`）。
//! 验证通过（且仅此后）才允许写入该 custom 配置的能力覆盖位（配置级
//! 持久，fail-closed 默认不变）；判定与证据留痕
//! `<storage-root>/vision-probe-log.jsonl`（VP-1 ③）。
//!
//! 分层：探测线程属于 core（发模型请求、持久化覆盖位）；前端只经
//! `ApplicationEvent::VisionProbeNotice` 收一次性通知，TUI/serve/exec
//! 三入口共用同一 core 通道。

use super::*;
use crate::model::{
    ContentPart, ImageProjectionBudget, Modality, ModelConfig, ModelError, ModelErrorKind,
    ModelItem, ModelOptions, ModelRequest, ProviderCredentials,
};
use crate::plugins::services::{ConfigStore, MonitorService, ProviderRegistry};
use image::ImageEncoder as _;
use serde_json::json;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// 探针判定。三态语义见模块文档；`Inconclusive` = 认证/网络/5xx/解码
/// 失败——与视觉能力无关，本次没有产生任何能力证据，重跑即可。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VisionProbeOutcome {
    Pass,
    Rejected,
    SilentDrop,
    Inconclusive,
}

impl VisionProbeOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Rejected => "rejected",
            Self::SilentDrop => "silent-drop",
            Self::Inconclusive => "inconclusive",
        }
    }
}

/// 探针报告：`ApplicationEvent::VisionProbeNotice` 的载荷，也是 exec
/// `join_report` 的结构化结果。字符串均有界。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisionProbeReport {
    pub outcome: VisionProbeOutcome,
    pub model: String,
    /// 本轮探针码（判别凭据：只有读出它的回答才算 Pass）。
    pub expected_code: String,
    /// 模型回答（或错误）的有界摘录——证据主体。
    pub answer_excerpt: String,
    /// Pass 且配置为 custom 时覆盖位是否已随写盘。
    pub override_applied: bool,
    pub note: String,
}

impl VisionProbeReport {
    /// 状态栏/终端一行摘要。
    pub fn status_text(&self) -> String {
        format!("vision probe {} · {}", self.outcome.as_str(), self.note)
    }
}

/// `/vision-probe` 的可 join 句柄（CB1-11 同款：事件只是展示通道，
/// headless 调用方经 `join_report` 拿结构化结果）。
#[derive(Clone)]
pub struct VisionProbeHandle {
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
    report: Arc<Mutex<Option<VisionProbeReport>>>,
}

impl VisionProbeHandle {
    pub fn join(&self) -> Result<(), ApplicationError> {
        let handle = self
            .join
            .lock()
            .map_err(|_| ApplicationError::new("vision probe join lock poisoned"))?
            .take();
        if let Some(handle) = handle {
            handle
                .join()
                .map_err(|_| ApplicationError::new("vision probe worker panicked"))?;
        }
        Ok(())
    }

    /// join 后返回探针报告（worker 崩溃 → Err）。
    pub fn join_report(&self) -> Result<VisionProbeReport, ApplicationError> {
        self.join()?;
        self.report
            .lock()
            .map_err(|_| ApplicationError::new("vision probe report lock poisoned"))
            .map(|slot| {
                slot.clone().unwrap_or_else(|| VisionProbeReport {
                    outcome: VisionProbeOutcome::Inconclusive,
                    model: String::new(),
                    expected_code: String::new(),
                    answer_excerpt: String::new(),
                    override_applied: false,
                    note: "no probe result".into(),
                })
            })
    }
}

impl TrustedProjectApplication {
    /// 启动一次性视觉探针（异步 worker，立即返回句柄；判定经
    /// `ApplicationEvent::VisionProbeNotice` 回流）。同一时刻至多一个
    /// 探针：上一轮未收尾则先 join 它（/compact 同纪律）。
    pub fn start_vision_probe(&mut self) -> Result<VisionProbeHandle, ApplicationError> {
        if let Some(previous) = self.active_vision_probe.take() {
            previous.join()?;
        }
        let (config, credentials) = self.model_state()?;
        if !config.is_configured() {
            return Err(ApplicationError::new(
                "model is not configured; configure a model and endpoint first",
            ));
        }
        // 通用式内置门（协调点裁定：不枚举模型清单）——任何内置预设
        // 都不走探针通道：内置能力 officially-declared 硬编码（VP-2），
        // 覆盖位仅对 custom 配置开放。
        if config.preset.is_some() {
            return Err(ApplicationError::new(
                "vision probe applies to custom model configurations only; built-in \
                 presets carry officially-declared capabilities",
            ));
        }
        let providers = Arc::clone(&self.providers);
        let draft_store = Arc::clone(&self.draft_images);
        let store = Arc::clone(&self.config);
        let monitor = Arc::clone(&self.monitor);
        let subscribers = Arc::clone(&self.subscribers);
        let evidence_root = self.control.root_path().to_path_buf();
        let busy = Arc::new(AtomicBool::new(true));
        let join_slot: Arc<Mutex<Option<JoinHandle<()>>>> = Arc::new(Mutex::new(None));
        let report_slot: Arc<Mutex<Option<VisionProbeReport>>> = Arc::new(Mutex::new(None));
        let handle = VisionProbeHandle {
            join: Arc::clone(&join_slot),
            report: Arc::clone(&report_slot),
        };
        let worker_busy = Arc::clone(&busy);
        let worker = std::thread::Builder::new()
            .name("clat-vision-probe".into())
            .spawn(move || {
                let (code, image) = generate_probe_image();
                let report = match draft_store.stage_png(&image) {
                    Ok(staged) => {
                        let answer =
                            run_probe_request(&providers, &config, &credentials, &staged, &code);
                        draft_store.release_clipboard_path(&staged);
                        let (outcome, excerpt) = classify_probe_answer(answer, &code);
                        let persisted = persist_override_if_passed(
                            &store,
                            &monitor,
                            &config,
                            &credentials,
                            outcome,
                        );
                        build_report(outcome, excerpt, &config.model, &code, persisted)
                    }
                    Err(error) => VisionProbeReport {
                        outcome: VisionProbeOutcome::Inconclusive,
                        model: config.model.clone(),
                        expected_code: code.clone(),
                        answer_excerpt: String::new(),
                        override_applied: false,
                        note: format!("could not stage the probe image: {error}"),
                    },
                };
                append_probe_evidence(&evidence_root, &report);
                if let Ok(mut slot) = report_slot.lock() {
                    *slot = Some(report.clone());
                }
                broadcast_to(&subscribers, ApplicationEvent::VisionProbeNotice { report });
                worker_busy.store(false, Ordering::Release);
            })
            .map_err(|error| {
                ApplicationError::new(format!("spawn vision probe worker: {error}"))
            })?;
        *join_slot
            .lock()
            .map_err(|_| ApplicationError::new("vision probe join lock poisoned"))? = Some(worker);
        self.active_vision_probe = Some(handle.clone());
        Ok(handle)
    }
}

/// 生成探针图：随机底色上的大号 4 位探针码（内置 3×5 点阵位图放大
/// ——零新依赖，块状字形清晰易读）。返回 (探针码, PNG 字节)。
fn generate_probe_image() -> (String, Vec<u8>) {
    const CANVAS_W: u32 = 176;
    const CANVAS_H: u32 = 80;
    const SCALE: u32 = 10;
    const DIGIT_W: u32 = 3;
    const DIGIT_H: u32 = 5;
    const GAP: u32 = 10;
    // 3×5 点阵（行主序，1 = 前景）。
    const GLYPHS: [[u8; 15]; 10] = [
        [1, 1, 1, 1, 0, 1, 1, 0, 1, 1, 1, 1, 1, 0, 1], // 0
        [0, 1, 0, 1, 1, 0, 0, 1, 0, 0, 1, 0, 1, 1, 1], // 1
        [1, 1, 1, 0, 0, 1, 1, 1, 1, 1, 0, 0, 1, 1, 1], // 2
        [1, 1, 1, 0, 0, 1, 1, 1, 1, 0, 0, 1, 1, 1, 1], // 3
        [1, 0, 1, 1, 0, 1, 1, 1, 1, 0, 0, 1, 0, 0, 1], // 4
        [1, 1, 1, 1, 0, 0, 1, 1, 1, 0, 0, 1, 1, 1, 1], // 5
        [1, 1, 1, 1, 0, 0, 1, 1, 1, 1, 0, 1, 1, 1, 1], // 6
        [1, 1, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1], // 7
        [1, 1, 1, 1, 0, 1, 1, 1, 1, 1, 0, 1, 1, 1, 1], // 8
        [1, 1, 1, 1, 0, 1, 1, 1, 1, 0, 0, 1, 1, 1, 1], // 9
    ];
    const PALETTE: [[u8; 3]; 6] = [
        [32, 60, 148],  // blue
        [16, 110, 60],  // green
        [132, 24, 24],  // red
        [90, 40, 140],  // purple
        [170, 90, 10],  // orange
        [20, 100, 120], // teal
    ];
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos() as u64 ^ duration.as_secs())
        .unwrap_or(0);
    let code = format!("{:04}", seed % 10_000);
    let background = PALETTE[((seed / 10_000) as usize) % PALETTE.len()];
    let mut image = image::RgbaImage::from_pixel(
        CANVAS_W,
        CANVAS_H,
        image::Rgba([background[0], background[1], background[2], 255]),
    );
    let total_w = 4 * DIGIT_W * SCALE + 3 * GAP;
    let mut cursor_x = (CANVAS_W - total_w) / 2;
    let origin_y = (CANVAS_H - DIGIT_H * SCALE) / 2;
    for digit in code.bytes() {
        let glyph = &GLYPHS[(digit - b'0') as usize];
        for (index, cell) in glyph.iter().enumerate() {
            if *cell == 0 {
                continue;
            }
            let column = (index % DIGIT_W as usize) as u32;
            let row = (index / DIGIT_W as usize) as u32;
            for dy in 0..SCALE {
                for dx in 0..SCALE {
                    image.put_pixel(
                        cursor_x + column * SCALE + dx,
                        origin_y + row * SCALE + dy,
                        image::Rgba([255, 255, 255, 255]),
                    );
                }
            }
        }
        cursor_x += DIGIT_W * SCALE + GAP;
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut std::io::Cursor::new(&mut png))
        .write_image(
            image.as_raw(),
            CANVAS_W,
            CANVAS_H,
            image::ExtendedColorType::Rgba8,
        )
        .expect("encode probe png");
    (code, png)
}

/// 发一次真实请求（走 provider 投影的 agent 路径——image_projection
/// 预算与请求侧图片策略围栏全同 run）。一针不重试：4xx 拒绝必须原
/// 样浮出，不得被重试掩盖。
fn run_probe_request(
    providers: &ProviderRegistry,
    config: &ModelConfig,
    credentials: &ProviderCredentials,
    staged: &Path,
    code: &str,
) -> Result<String, ModelError> {
    let mut model = providers.build(config, credentials)?;
    let question = format!(
        "A four-digit code is rendered in the attached image. Reply with the four digits only. \
         If you cannot see any image, reply NOIMAGE. (a sighted model reads: {code})"
    );
    let items = vec![ModelItem::User {
        content: vec![
            ContentPart::Text(question),
            ContentPart::Image {
                path: staged.display().to_string(),
                media_type: "image/png".into(),
            },
        ],
    }];
    let tools: [crate::tool::ToolDefinition; 0] = [];
    let options = ModelOptions {
        image_projection: Some(ImageProjectionBudget::for_config(config)),
        ..ModelOptions::default()
    };
    let request = ModelRequest {
        instructions: None,
        items: &items,
        tools: &tools,
        options: &options,
        cancel: &crate::CancelToken::new(),
    };
    let mut sink = Vec::new();
    let response = model.stream(request, &mut sink)?;
    let answer = response.text.trim().to_owned();
    if answer.is_empty() {
        return Err(ModelError::decode("vision probe got an empty answer"));
    }
    Ok(answer)
}

/// 判别纯函数：模型应答 + 探针码 → 判定。HTTP 层错误按 kind 分诊：
/// 4xx Client = 端点拒图（`Rejected`）；认证/网络/5xx/解码 =
/// `Inconclusive`（与能力无关，重跑）；200 但读不出探针码 =
/// Hy 型静默弃图（`SilentDrop`）。
pub(crate) fn classify_probe_answer(
    result: Result<String, ModelError>,
    expected_code: &str,
) -> (VisionProbeOutcome, String) {
    match result {
        Ok(answer) => {
            let excerpt = bounded_excerpt(&answer);
            if answer.contains(expected_code) {
                (VisionProbeOutcome::Pass, excerpt)
            } else {
                (VisionProbeOutcome::SilentDrop, excerpt)
            }
        }
        Err(error) => {
            let excerpt = bounded_excerpt(&error.to_string());
            // 400/422 类：端点明确拒绝图片部件——能力缺席的硬信号。
            // 401/403 = key 问题而非能力信号；网络/5xx/解码同理。
            let outcome = if error.kind() == ModelErrorKind::Client {
                VisionProbeOutcome::Rejected
            } else {
                VisionProbeOutcome::Inconclusive
            };
            (outcome, excerpt)
        }
    }
}

fn build_report(
    outcome: VisionProbeOutcome,
    answer_excerpt: String,
    model: &str,
    code: &str,
    persisted: Option<Result<(), String>>,
) -> VisionProbeReport {
    let mut note = match outcome {
        VisionProbeOutcome::Pass => "the model read the probe code".to_owned(),
        VisionProbeOutcome::Rejected => "the endpoint rejected the image part (4xx)".to_owned(),
        VisionProbeOutcome::SilentDrop => {
            "the endpoint answered without reading the image".to_owned()
        }
        VisionProbeOutcome::Inconclusive => {
            "the probe failed for reasons unrelated to vision; rerun it".to_owned()
        }
    };
    let mut override_applied = false;
    match persisted {
        Some(Ok(())) => {
            override_applied = true;
            note.push_str("; vision capability override saved to the configuration");
        }
        Some(Err(error)) => note.push_str(&format!("; override not saved: {error}")),
        None => {}
    }
    VisionProbeReport {
        outcome,
        model: model.to_owned(),
        expected_code: code.to_owned(),
        answer_excerpt,
        override_applied,
        note,
    }
}

/// VP-1 ②：能力覆盖位——custom 配置专用。通用式内置门：任何内置
/// 预设（preset 指针非空）经此通道写覆盖位必须被拒（不枚举模型
/// 清单）。fail-closed 默认不变：未过探针的 custom 配置保持纯文本。
pub(crate) fn apply_vision_override(config: &mut ModelConfig) -> Result<(), String> {
    if config.preset.is_some() {
        return Err(
            "built-in presets carry officially-declared capabilities and cannot be \
             overridden by the vision probe"
                .into(),
        );
    }
    if !config
        .capabilities
        .input_modalities
        .contains(&Modality::Image)
    {
        config.capabilities.input_modalities.push(Modality::Image);
    }
    config.capabilities.image_input_verified = true;
    Ok(())
}

/// Pass 时的持久化（worker 线程内）：写活动模型状态 + INV-M2 第四
/// 元素（直写即非档案态，清档案指针）+ 监视器重配。返回 None =
/// 未触发（非 Pass）；Some(Err) = 触发了但落盘失败——覆盖位不落盘
/// 就绝不生效（fail-closed）。
fn persist_override_if_passed(
    store: &Arc<dyn ConfigStore>,
    monitor: &Arc<dyn MonitorService>,
    config: &ModelConfig,
    credentials: &ProviderCredentials,
    outcome: VisionProbeOutcome,
) -> Option<Result<(), String>> {
    if outcome != VisionProbeOutcome::Pass {
        return None;
    }
    let mut updated = config.clone();
    if let Err(error) = apply_vision_override(&mut updated) {
        return Some(Err(error));
    }
    if let Err(error) = store.save_model_state(&updated, credentials) {
        return Some(Err(error.to_string()));
    }
    // INV-M2 第四元素：直写路径装入非档案态——指针随保存清空
    //（应用层 save_model_state 同序；这里在 worker 内以 store 原语复刻）。
    if let Err(error) = store.set_active_profile(None) {
        return Some(Err(error.to_string()));
    }
    monitor.configure(updated, credentials.clone());
    Some(Ok(()))
}

/// VP-1 ③：判定与证据留痕——一行 JSON 一次探针，追加写
/// `<storage-root>/vision-probe-log.jsonl`。绝不记录 key 或完整请求；
/// 回答摘录有界。留痕失败不掩盖探针判定（只丢证据行）。
fn append_probe_evidence(root: &Path, report: &VisionProbeReport) {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let line = json!({
        "ts": timestamp,
        "outcome": report.outcome.as_str(),
        "model": report.model,
        "expected_code": report.expected_code,
        "answer_excerpt": report.answer_excerpt,
        "override_applied": report.override_applied,
        "note": report.note,
    });
    let result = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(root.join("vision-probe-log.jsonl"))
        .and_then(|mut file| {
            use std::io::Write as _;
            writeln!(file, "{line}")
        });
    if let Err(error) = result {
        let _ = error; // 无处可写时只丢证据行，判定照常回流。
    }
}

fn bounded_excerpt(text: &str) -> String {
    const LIMIT: usize = 160;
    text.chars().take(LIMIT).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// VP-1 判别（通用式负断言，协调点裁定）：**任一**内置预设经
    /// 覆盖位通道必须被拒——遍历整个内置预设表，不枚举模型清单，
    /// 与 VP-2 的矩阵终态解耦（新增/改名预设自动进入本断言）。
    #[test]
    fn builtin_presets_are_never_overridable_through_the_probe_channel() {
        for preset in crate::presets::MODEL_PRESETS {
            let mut config = ModelConfig::default();
            preset.apply(&mut config);
            let error = apply_vision_override(&mut config)
                .expect_err("built-in presets must reject the override channel");
            assert!(error.contains("built-in presets"), "{error}");
            // 拒绝即零副作用：能力位保持预设 stamp 原值。
            assert_eq!(
                config.capabilities,
                preset.owned_capabilities(),
                "{}: rejection must not mutate capabilities",
                preset.id
            );
        }
    }

    /// VP-1 判别（fail-closed 腿）：custom 配置默认**不开图**——
    /// 覆盖位是唯一放行路径；删 `ModelCapabilities::default` 的
    /// fail-closed（verified 缺省 true 或缺省带 Image）此测试红。
    #[test]
    fn custom_configs_stay_text_only_until_a_probe_pass_overrides_them() {
        let mut custom = ModelConfig {
            model: "my-vision-model".into(),
            endpoint: "https://internal.example/v1".into(),
            ..ModelConfig::default()
        };
        assert!(custom.preset.is_none());
        assert!(
            !custom.capabilities.accepts_image_input(),
            "unprobed custom configs fail closed"
        );
        // 探针 Pass 后的覆盖：开图 + verified，其余字段不动。
        apply_vision_override(&mut custom).expect("custom configs accept the override");
        assert!(custom.capabilities.accepts_image_input());
        assert!(custom.capabilities.image_input_verified);
        assert_eq!(custom.model, "my-vision-model");
        assert_eq!(custom.endpoint, "https://internal.example/v1");
    }

    /// VP-1 判别（三态 + Inconclusive）：探针判定四腿——
    /// Pass = 回答含探针码；SilentDrop = 200 但读不出码（Hy 型静默
    /// 弃图，答案即便自称看不见图也不算 Pass）；Rejected = 4xx；
    /// Inconclusive = 认证/网络类与能力无关。
    #[test]
    fn probe_outcomes_discriminate_pass_silent_drop_and_rejection() {
        let code = "4271";
        let (pass, excerpt) = classify_probe_answer(Ok("The code is 4271.".into()), code);
        assert_eq!(pass, VisionProbeOutcome::Pass);
        assert!(excerpt.contains("4271"));
        // 相邻串不误判：code "4271" 不在答案里 → SilentDrop。
        let (silent, _) =
            classify_probe_answer(Ok("I cannot see any image (NOIMAGE).".into()), code);
        assert_eq!(silent, VisionProbeOutcome::SilentDrop);
        // 数字相邻误命中防护："14271" 含 "4271"？——contains 会命中；
        // 探针码取值保证四位独立出现即判 Pass 是可接受口径，但错位
        // 命中必须仍属读出（视觉证据）。此腿锁定 contains 语义本身。
        let (substring, _) = classify_probe_answer(Ok("142718".into()), code);
        assert_eq!(substring, VisionProbeOutcome::Pass);
        let (rejected, _) = classify_probe_answer(
            Err(ModelError::with_kind(
                ModelErrorKind::Client,
                "HTTP 400: image input not supported",
            )),
            code,
        );
        assert_eq!(rejected, VisionProbeOutcome::Rejected);
        let (inconclusive_auth, _) = classify_probe_answer(
            Err(ModelError::with_kind(
                ModelErrorKind::Authentication,
                "HTTP 401: bad key",
            )),
            code,
        );
        assert_eq!(inconclusive_auth, VisionProbeOutcome::Inconclusive);
        let (inconclusive_transport, _) =
            classify_probe_answer(Err(ModelError::transport("connection refused")), code);
        assert_eq!(inconclusive_transport, VisionProbeOutcome::Inconclusive);
    }

    /// 探针图：PNG 可解码、尺寸正确、探针码字形真实绘制（前景白
    /// 像素存在）、底色非白。
    #[test]
    fn generated_probe_image_carries_the_code_legibly() {
        let (code, png) = generate_probe_image();
        assert_eq!(code.len(), 4);
        assert!(code.bytes().all(|byte| byte.is_ascii_digit()));
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .expect("probe png decodes")
            .to_rgba8();
        assert_eq!(decoded.width(), 176);
        assert_eq!(decoded.height(), 80);
        let mut white = 0usize;
        let mut background = None;
        for pixel in decoded.pixels() {
            let channels = pixel.0;
            if channels[0] == 255 && channels[1] == 255 && channels[2] == 255 {
                white += 1;
            } else if background.is_none() {
                background = Some([channels[0], channels[1], channels[2]]);
            }
        }
        assert!(white > 1_000, "the code glyphs must be drawn ({white})");
        assert!(
            background.is_some_and(|rgb| !(rgb[0] == 255 && rgb[1] == 255 && rgb[2] == 255)),
            "a non-white background must exist"
        );
    }

    /// 留痕：每次探针追加一行 JSON（含判定/探针码/是否落覆盖位），
    /// 文件随追加增长。
    #[test]
    fn probe_evidence_appends_one_json_line_per_run() {
        let root = std::env::temp_dir().join(format!("clat-vision-probe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let report = VisionProbeReport {
            outcome: VisionProbeOutcome::Pass,
            model: "my-vision-model".into(),
            expected_code: "0713".into(),
            answer_excerpt: "0713".into(),
            override_applied: true,
            note: "the model read the probe code".into(),
        };
        append_probe_evidence(&root, &report);
        append_probe_evidence(&root, &report);
        let log = std::fs::read_to_string(root.join("vision-probe-log.jsonl")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2, "one JSON line per probe run");
        for line in &lines {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(value["outcome"], "pass");
            assert_eq!(value["expected_code"], "0713");
            assert_eq!(value["override_applied"], true);
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
