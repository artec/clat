//! MM-1A 冻结消息词汇（`docs/todo/glm-5.3-flash-multimodal.md` §前置协议
//! 冻结，2026-08-27）：多模态主线唯一 DTO 的定义处。TUI、PWA、
//! journal、wire v1、SSE、replay 对图片/内容的表达全部从这里投影，
//! 任何表面不得自带第二套图片词汇（MM-I2）。
//!
//! ## 本模块的不变量（测试从这些推导，不从实现反抄）
//!
//! - **INV-M1A-1｜词汇唯一**：`AttachmentDescriptor` / `ContentBlock` /
//!   `MessageContent` / `ToolResultContent` / `PendingMessage` /
//!   `AdmissionReceipt` / `DraftScope` 只在本文件定义一次。serde 形状由
//!   golden 测试钉死；改形状 = 改协议，需要按 v1 additive 政策评估。
//! - **INV-M1A-2｜描述符无字节无路径**：descriptor 的序列化输出永不
//!   包含 base64、图片字节或宿主机路径（MM-I3）。路径是桥接期（MM-1
//!   存储落地前）journal 块的私有字段，由 core 在写入侧合并、读取侧
//!   剥离，不进 wire/SSE/事件流。
//! - **INV-M1A-3｜digest 确定性**：`MessageContent::request_digest` 对
//!   同一内容恒等、对文本或附件集合的任何变化敏感（MM-1A 幂等键的
//!   payload 判别：同 clientMessageId 不同 digest = conflict）。
//! - **INV-M1A-4｜commit point = durable append+flush**：`clientMessageId`
//!   与 `requestDigest` 只随 user/steering durable event 落盘；重启后
//!   从 journal 投影重建 committed receipt。跨过 commit point 之后的
//!   worker 失败返回 `Committed` receipt + run failure（MM-I11）。
//! - **INV-M1A-5｜桥接期 id 规则**：新导入附件的 `attachmentId` = 导入
//!   时铸出的 uuid（= 会话附件目录内文件名主干）；MM-1A 之前的历史
//!   journal 图块没有该字段，回放按路径做确定性派生（FNV-1a 64，
//!   版本稳定，不依赖 std hasher）。两条规则互斥：有耐久 id 恒用耐久
//!   id，派生只服务旧会话。
//! - **INV-M1A-6｜wire additive**：一切新字段可选、缺省省略；旧字段
//!   （`prompt`/`text`/`output`）语义 = 文本 blocks 拼接，图片存在时
//!   它不是消息的完整表达，消费方应优先读 blocks。
//!
//! 桥接期定位：MM-1A 只冻结协议形状与 typed 管道；attachment store、
//! magic/decode 校验、规范化、upload/reserve 状态机分别属于 MM-1 与
//! MM-4。`DraftScope` 与 `AdmissionState` 的 `Uploaded`/`Reserved` 分支
//! 由 MM-4 的 serve 消费，此处先冻结形状。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 不可解析的附件身份。wire/UI 不得假定它是 digest、据此拼路径或当
/// 授权凭据（MM-1A 目标结构节）。桥接期取值规则见 INV-M1A-5。
pub type AttachmentId = String;

/// 客户端生成的消息身份（幂等键）：按 session + clientMessageId 定域。
/// `None` = core 合成消息（goal 轮、compaction、TUI 内部提交）。
pub type ClientMessageId = String;

/// 附件描述符：图片进入 journal/wire/SSE 的唯一形状——有界元数据，
/// 永不含字节（INV-M1A-2）。`width`/`height`/`bytes` 为 0 表示未知
/// （桥接期旧会话无可耐久元数据；MM-1 起新导入恒有实测值）。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AttachmentDescriptor {
    pub attachment_id: AttachmentId,
    pub media_type: String,
    pub width: u64,
    pub height: u64,
    /// 规范化后字节数（桥接期 = 原文件字节数）。
    pub bytes: u64,
    /// 用户可读的显示名（原始文件名），不含路径语义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// 规范化前原始宽高；未规范化时与 width/height 相同或省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_width: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_height: Option<u64>,
}

/// role-neutral 内容块。工具结果、用户消息、steering 共用（MM-I12）。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ContentBlock {
    Text { text: String },
    Image { attachment: AttachmentDescriptor },
}

/// 一条消息的完整内容。旧文本字段（wire `prompt`/`text`、journal text
/// block）是它的有界文本投影，不是平行表示。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MessageContent {
    pub blocks: Vec<ContentBlock>,
}

impl MessageContent {
    /// 纯文本消息。
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            blocks: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn from_blocks(blocks: Vec<ContentBlock>) -> Self {
        Self { blocks }
    }

    /// 文本 blocks 按序拼接——wire 旧字段（`prompt`/`text`）与 transcript
    /// 的取值口径。图片不以任何形式混入（字节/占位符都不属于 wire 层）。
    pub fn plain_text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn has_images(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| matches!(block, ContentBlock::Image { .. }))
    }

    pub fn image_descriptors(&self) -> Vec<&AttachmentDescriptor> {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Image { attachment } => Some(attachment),
                ContentBlock::Text { .. } => None,
            })
            .collect()
    }

    pub fn attachment_ids(&self) -> Vec<AttachmentId> {
        self.image_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.attachment_id.clone())
            .collect()
    }

    /// 纯文本且非空（steering admission 等价于旧 String 语义的判定）。
    pub fn text_only(&self) -> bool {
        !self.blocks.is_empty()
            && self
                .blocks
                .iter()
                .all(|b| matches!(b, ContentBlock::Text { .. }))
    }

    /// 请求 digest（INV-M1A-3）：内部 `hash_message_payload` over 本内容
    ///（无 staged 引用）。这是**已接纳内容**的身份——与无 staged 附件
    /// 的 [`PendingMessage::request_digest`] 恒同值（同一逻辑提交的
    /// steering/journal 判别共用一个答案）。
    pub fn request_digest(&self) -> String {
        hash_message_payload(&self.blocks, &[])
    }
}

/// digest 的唯一散列实现（INV-M1A-3）：显式规范形（不经 serde_json
/// map 排序），长度前缀封死拼接歧义。文本块、图片附件 id、staged
/// 引用各自计段；任一变化必然改变输出。前缀钉版本，演进语义时换
/// 前缀而不是改散列输入规则。
fn hash_message_payload(blocks: &[ContentBlock], staged: &[std::path::PathBuf]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"clat-mm1a-v1");
    for block in blocks {
        match block {
            ContentBlock::Text { text } => {
                hasher.update([0x74]); // 't'
                hasher.update(text.len().to_le_bytes());
                hasher.update(text.as_bytes());
            }
            ContentBlock::Image { attachment } => {
                hasher.update([0x69]); // 'i'
                hasher.update(attachment.attachment_id.len().to_le_bytes());
                hasher.update(attachment.attachment_id.as_bytes());
            }
        }
    }
    for reference in staged {
        let reference = reference.to_string_lossy();
        hasher.update([0x73]); // 's'
        hasher.update(reference.len().to_le_bytes());
        hasher.update(reference.as_bytes());
    }
    hex_digest(hasher.finalize())
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let mut hex = String::with_capacity(digest.as_ref().len() * 2);
    for byte in digest.as_ref() {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// wire `tool_finished` 的冻结投影形状：blocks 为空时旧 `output` 字段
/// 即完整内容；blocks 非空时 `legacy_output` 保留 JSON 摘要（MM-1 起
/// 工具结果图入 attachment 域后由 blocks 承载图片）。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolResultContent {
    pub call_id: String,
    pub tool_name: String,
    pub blocks: Vec<ContentBlock>,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_output: Option<Value>,
}

/// 入队待接纳的消息（frontend-neutral）。`staged_attachments` 是
/// **pre-admission** 的本地来源路径（TUI 粘贴/serve 传入）；接纳
/// （复制进会话附件目录）成功后它们变成 content 内的 descriptor 与
/// journal 引用，staged 本身不再有意义。MM-4 将把上传流的 staging
/// 换成 opaque upload id，字段语义不变：都是"尚未接纳的来源"。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingMessage {
    pub client_message_id: Option<ClientMessageId>,
    pub content: MessageContent,
    pub staged_attachments: Vec<std::path::PathBuf>,
    /// Core-admitted transient images for an in-run steering draft. The
    /// durable protocol never serializes their absolute paths: on claim the
    /// recorder writes descriptor-only refs, while `Run` consumes the paths
    /// only for the immediately following provider request. Ordinary initial
    /// prompts leave this empty because their model history is rebuilt from
    /// the committed journal through the session fence.
    pub(crate) admitted_images: Vec<JournalImage>,
    /// Frozen pre-admission digest for a steering message whose content is
    /// later enriched with normalized descriptors. Initial submissions do
    /// not need this because `prepare_run` computes before enrichment.
    pub(crate) submission_digest: Option<String>,
}

impl PendingMessage {
    /// 纯文本、无附件、无客户端 id（core 合成消息与既有前端的最小形态）。
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            client_message_id: None,
            content: MessageContent::text(text),
            staged_attachments: Vec::new(),
            admitted_images: Vec::new(),
            submission_digest: None,
        }
    }

    /// 供前端构造：文本 + 可选客户端 id + pre-admission 附件路径。
    pub fn from_front_end(
        text: impl Into<String>,
        client_message_id: Option<ClientMessageId>,
        staged_attachments: Vec<std::path::PathBuf>,
    ) -> Self {
        Self {
            client_message_id,
            content: MessageContent::text(text),
            staged_attachments,
            admitted_images: Vec::new(),
            submission_digest: None,
        }
    }

    /// Project an admitted message into provider-facing parts. Every image
    /// descriptor must have an exact admitted source match; substituting text
    /// would silently change the user's message and conceal an internal
    /// admission/projection invariant violation.
    pub(crate) fn model_parts(&self) -> Result<Vec<crate::model::ContentPart>, String> {
        let mut parts = Vec::with_capacity(self.content.blocks.len());
        for block in &self.content.blocks {
            match block {
                ContentBlock::Text { text } => {
                    parts.push(crate::model::ContentPart::Text(text.clone()))
                }
                ContentBlock::Image { attachment } => {
                    let image = self
                        .admitted_images
                        .iter()
                        .find(|image| image.descriptor.attachment_id == attachment.attachment_id)
                        .ok_or_else(|| {
                            format!(
                                "image attachment {} has no admitted provider source",
                                attachment.attachment_id
                            )
                        })?;
                    parts.push(crate::model::ContentPart::Image {
                        path: image.path.clone(),
                        media_type: image.descriptor.media_type.clone(),
                    });
                }
            }
        }
        Ok(parts)
    }

    /// **提交幂等 digest**（INV-M1A-3 的提交侧）：内容块 + staged 附件
    /// 引用（桥接期 = 路径字符串；MM-4 = 服务端上传 id，同样对同一次
    /// 提交稳定）。同一逻辑提交的重试恒同值；任一侧变化即 conflict。
    /// 不掺导入后重铸的 attachmentId——否则崩溃重试必然翻案，幂等失效。
    /// 纯文本且无 staged 时与 [`MessageContent::request_digest`] 同值。
    pub fn request_digest(&self) -> String {
        self.submission_digest
            .clone()
            .unwrap_or_else(|| hash_message_payload(&self.content.blocks, &self.staged_attachments))
    }

    pub(crate) fn freeze_request_digest(&mut self) {
        if self.submission_digest.is_none() {
            self.submission_digest = Some(hash_message_payload(
                &self.content.blocks,
                &self.staged_attachments,
            ));
        }
    }

    /// Replace frontend opaque upload ids with core-owned staging paths while
    /// preserving retry identity over the original ids. Only trusted ingress
    /// adapters may call this; paths never enter wire/SSE payloads.
    pub(crate) fn resolve_staged_attachments(&mut self, paths: Vec<std::path::PathBuf>) {
        self.freeze_request_digest();
        self.staged_attachments = paths;
    }
}

/// 接纳状态机的冻结分支（MM-I11）。MM-1A 只产生 `Committed`（commit
/// point 之后的一切完成/失败都携带它）；`Uploaded`/`Reserved` 是 MM-4
/// 上传流的 staging 状态（崩溃后回滚、由启动清理回收），此处冻结形状。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AdmissionState {
    #[serde(rename = "uploaded")]
    Uploaded,
    #[serde(rename = "reserved")]
    Reserved,
    #[serde(rename = "committed")]
    Committed,
    #[serde(rename = "rolled-back")]
    RolledBack,
}

/// 接纳回执：所有成功/错误响应携带的权威事实。服务端 receipt/state 是
/// 权威（MM-1A bullet）；重启后 `Committed` 由 journal 投影重建。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdmissionReceipt {
    pub client_message_id: ClientMessageId,
    pub state: AdmissionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_message_id: Option<String>,
    #[serde(default)]
    pub attachment_ids: Vec<AttachmentId>,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_phase: Option<String>,
}

impl AdmissionReceipt {
    /// A frontend-owned draft has been accepted into the active run queue but
    /// has not crossed the durable user-event barrier yet.
    pub fn reserved(client_message_id: ClientMessageId, attachment_ids: Vec<AttachmentId>) -> Self {
        Self {
            client_message_id,
            state: AdmissionState::Reserved,
            committed_message_id: None,
            attachment_ids,
            retryable: false,
            failure_phase: None,
        }
    }

    /// Admission failed before the durable commit point. The caller still
    /// owns the draft and may submit it again after addressing `phase`.
    pub fn rolled_back(
        client_message_id: ClientMessageId,
        attachment_ids: Vec<AttachmentId>,
        phase: impl Into<String>,
    ) -> Self {
        Self {
            client_message_id,
            state: AdmissionState::RolledBack,
            committed_message_id: None,
            attachment_ids,
            retryable: true,
            failure_phase: Some(phase.into()),
        }
    }

    /// commit point（user/steering event append+flush）跨过之后的回执：
    /// 即使后续 worker/审批/run 失败，消息已耐久、不得重复发送。
    pub fn committed(
        client_message_id: ClientMessageId,
        committed_message_id: String,
        attachment_ids: Vec<AttachmentId>,
    ) -> Self {
        Self {
            client_message_id,
            state: AdmissionState::Committed,
            committed_message_id: Some(committed_message_id),
            attachment_ids,
            retryable: false,
            failure_phase: None,
        }
    }

    /// Preserve the authoritative admission state while explaining a later
    /// failure. In particular, a post-commit worker failure remains
    /// non-retryable and must never be rewritten as a rollback.
    pub fn with_failure_phase(mut self, phase: impl Into<String>) -> Self {
        self.failure_phase = Some(phase.into());
        self
    }
}

/// 已接纳消息的权威事实：committed 回执 + 落盘的提交 digest。
/// M-02（审查 2026-08-27）的幂等重试判别输入——同 key 同 digest
/// 幂等成功（不重复 append），同 key 异 digest conflict。journal
/// 投影是唯一来源。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedAdmission {
    pub receipt: AdmissionReceipt,
    /// 落盘的 `requestDigest`；本仓库的写入方在客户端键存在时恒写
    /// digest，`None` 只可能来自被篡改/异常的日志——重试判别按
    /// "无法证伪即视为同一提交" 处理（键归属客户端，威胁模型是
    /// 事故性重试而非键盗用）。
    pub request_digest: Option<String>,
}

/// PWA 草稿作用域（MM-1A 冻结形状，MM-4 的 `draft.open` 消费）：
/// 服务端随机 id 绑定 token generation、selection generation 与目标
/// 会话（既有会话或 pending nonce）。客户端自报字符串不构成授权
///（MM-I10）。
#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DraftScope {
    pub draft_scope_id: String,
    pub selection_generation: u64,
    pub target: DraftTarget,
    /// Unix epoch 毫秒；过期后同 scope fail closed。
    pub expires_at: i64,
}

/// 草稿目标（MM-4 的 draft scope 绑定消费；MM-1A 仅冻结形状）。
#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DraftTarget {
    ExistingSession { session_id: String },
    PendingSession { nonce: String },
}

/// 桥接期 journal 图块的全部耐久事实（INV-M1A-2 的例外说明：`path`
/// 只出现在 core 的 journal 读写路径，永不进 wire/SSE/事件流；MM-1
/// 把它替换为 attachment store 引用）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JournalImage {
    pub descriptor: AttachmentDescriptor,
    /// 会话附件目录内的绝对引用（既有 v1 journal 词汇 `path`）。
    pub path: String,
}

/// MM-1A 之前的历史 journal 图块没有耐久 attachmentId：按路径做确定性
/// 派生（FNV-1a 64，显式实现保证跨版本稳定——std hasher 无此承诺）。
/// 只用于旧会话回放；有耐久 id 恒用耐久 id（INV-M1A-5）。
pub(crate) fn legacy_attachment_id(path: &str) -> AttachmentId {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("legacy-{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// INV-M1A-1/2：descriptor 的 golden serde 形状——字段名、可选省略、
    /// 永无字节/路径字段。任何形状漂移在此红。
    #[test]
    fn attachment_descriptor_serde_shape_is_frozen() {
        let descriptor = AttachmentDescriptor {
            attachment_id: "0f8c2a4e11112222".into(),
            media_type: "image/png".into(),
            width: 1024,
            height: 768,
            bytes: 2048,
            display_name: Some("screenshot.png".into()),
            original_width: Some(2048),
            original_height: Some(1536),
        };
        let wire = serde_json::to_string(&descriptor).expect("serialize");
        assert_eq!(
            wire,
            r#"{"attachment_id":"0f8c2a4e11112222","media_type":"image/png","width":1024,"height":768,"bytes":2048,"display_name":"screenshot.png","original_width":2048,"original_height":1536}"#
        );
        let back: AttachmentDescriptor = serde_json::from_str(&wire).expect("parse");
        assert_eq!(back, descriptor);

        // 可选字段缺省省略；旧读取方容忍未知字段。
        let minimal = AttachmentDescriptor {
            attachment_id: "a".into(),
            media_type: "image/jpeg".into(),
            width: 0,
            height: 0,
            bytes: 0,
            display_name: None,
            original_width: None,
            original_height: None,
        };
        let wire = serde_json::to_string(&minimal).expect("serialize");
        assert_eq!(
            wire,
            r#"{"attachment_id":"a","media_type":"image/jpeg","width":0,"height":0,"bytes":0}"#
        );
        let tolerated: AttachmentDescriptor =
            serde_json::from_str(r#"{"attachment_id":"a","media_type":"m","width":1,"height":1,"bytes":1,"future_field":true}"#)
                .expect("unknown fields are tolerated");
        assert_eq!(tolerated.width, 1);
    }

    /// INV-M1A-1：ContentBlock 的 discriminated 形状冻结。
    #[test]
    fn content_block_serde_shape_is_frozen() {
        let text = ContentBlock::Text {
            text: "hello".into(),
        };
        assert_eq!(
            serde_json::to_string(&text).expect("serialize"),
            r#"{"Text":{"text":"hello"}}"#
        );
        let image = ContentBlock::Image {
            attachment: AttachmentDescriptor {
                attachment_id: "img-1".into(),
                media_type: "image/png".into(),
                width: 10,
                height: 10,
                bytes: 20,
                display_name: None,
                original_width: None,
                original_height: None,
            },
        };
        let wire = serde_json::to_string(&image).expect("serialize");
        assert_eq!(
            wire,
            r#"{"Image":{"attachment":{"attachment_id":"img-1","media_type":"image/png","width":10,"height":10,"bytes":20}}}"#
        );
        // 往返。
        let back: ContentBlock = serde_json::from_str(&wire).expect("parse");
        assert_eq!(back, image);
    }

    /// INV-M1A-1：回执与状态机的 serde 词汇冻结（wire 单词按计划草图）。
    #[test]
    fn admission_receipt_serde_shape_is_frozen() {
        let receipt = AdmissionReceipt::committed(
            "client-1".into(),
            "0f8c2a4e-urn".into(),
            vec!["img-1".into()],
        );
        let wire = serde_json::to_string(&receipt).expect("serialize");
        assert_eq!(
            wire,
            r#"{"client_message_id":"client-1","state":"committed","committed_message_id":"0f8c2a4e-urn","attachment_ids":["img-1"],"retryable":false}"#
        );
        let rolled = AdmissionReceipt {
            client_message_id: "client-1".into(),
            state: AdmissionState::RolledBack,
            committed_message_id: None,
            attachment_ids: Vec::new(),
            retryable: true,
            failure_phase: Some("busy".into()),
        };
        assert_eq!(
            serde_json::to_string(&rolled).expect("serialize"),
            r#"{"client_message_id":"client-1","state":"rolled-back","attachment_ids":[],"retryable":true,"failure_phase":"busy"}"#
        );
    }

    /// INV-M1A-1：DraftScope 冻结（MM-4 消费前的形状证据）。
    #[test]
    fn draft_scope_serde_shape_is_frozen() {
        let scope = DraftScope {
            draft_scope_id: "scope-1".into(),
            selection_generation: 7,
            target: DraftTarget::ExistingSession {
                session_id: "s-1".into(),
            },
            expires_at: 1_756_000_000_000,
        };
        let wire = serde_json::to_string(&scope).expect("serialize");
        assert_eq!(
            wire,
            r#"{"draft_scope_id":"scope-1","selection_generation":7,"target":{"ExistingSession":{"session_id":"s-1"}},"expires_at":1756000000000}"#
        );
        let pending = DraftScope {
            draft_scope_id: "scope-2".into(),
            selection_generation: 1,
            target: DraftTarget::PendingSession { nonce: "n".into() },
            expires_at: 0,
        };
        assert_eq!(
            serde_json::to_string(&pending).expect("serialize"),
            r#"{"draft_scope_id":"scope-2","selection_generation":1,"target":{"PendingSession":{"nonce":"n"}},"expires_at":0}"#
        );
    }

    /// INV-M1A-3：digest 确定性与判别力（文本变化、附件 id 变化、块序
    /// 变化都必须改变 digest；同一内容跨调用恒等）。
    #[test]
    fn request_digest_is_deterministic_and_discriminating() {
        let image = |id: &str| ContentBlock::Image {
            attachment: AttachmentDescriptor {
                attachment_id: id.into(),
                media_type: "image/png".into(),
                width: 1,
                height: 1,
                bytes: 1,
                display_name: None,
                original_width: None,
                original_height: None,
            },
        };
        let base = MessageContent::from_blocks(vec![
            ContentBlock::Text {
                text: "look".into(),
            },
            image("img-1"),
        ]);
        // 恒等：digest 不含时间/随机成分。
        assert_eq!(base.request_digest(), base.request_digest());
        // 元数据（宽高/字节/MIME）不影响 digest——幂等键判别的是内容
        // 身份（文本 + 附件 id 集合），同一 id 的元数据修正不得翻案。
        let mut richer = base.clone();
        if let ContentBlock::Image { attachment } = &mut richer.blocks[1] {
            attachment.bytes = 999;
        }
        assert_eq!(richer.request_digest(), base.request_digest());
        // 文本变化。
        let mut other_text = base.clone();
        other_text.blocks[0] = ContentBlock::Text { text: "see".into() };
        assert_ne!(other_text.request_digest(), base.request_digest());
        // 附件 id 变化。
        let other_image = MessageContent::from_blocks(vec![
            ContentBlock::Text {
                text: "look".into(),
            },
            image("img-2"),
        ]);
        assert_ne!(other_image.request_digest(), base.request_digest());
        // 块序变化（文本在前/在后是不同消息）。
        let reordered = MessageContent::from_blocks(vec![
            image("img-1"),
            ContentBlock::Text {
                text: "look".into(),
            },
        ]);
        assert_ne!(reordered.request_digest(), base.request_digest());
        // 长度前缀封死 "ab"+"c" vs "a"+"bc" 同串歧义。
        let ab_c = MessageContent::from_blocks(vec![
            ContentBlock::Text { text: "ab".into() },
            ContentBlock::Text { text: "c".into() },
        ]);
        let a_bc = MessageContent::from_blocks(vec![
            ContentBlock::Text { text: "a".into() },
            ContentBlock::Text { text: "bc".into() },
        ]);
        assert_ne!(ab_c.request_digest(), a_bc.request_digest());
        // 输出形状：64 位十六进制。
        assert_eq!(base.request_digest().len(), 64);
        assert!(base.request_digest().chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// INV-M1A-6：plain_text 是文本 blocks 的拼接投影；图片不混入。
    #[test]
    fn plain_text_projects_text_blocks_only() {
        let content = MessageContent::from_blocks(vec![
            ContentBlock::Text { text: "a".into() },
            ContentBlock::Image {
                attachment: AttachmentDescriptor {
                    attachment_id: "i".into(),
                    media_type: "image/png".into(),
                    width: 0,
                    height: 0,
                    bytes: 0,
                    display_name: None,
                    original_width: None,
                    original_height: None,
                },
            },
            ContentBlock::Text { text: "b".into() },
        ]);
        assert_eq!(content.plain_text(), "ab");
        assert!(content.has_images());
        assert!(!content.text_only());
        assert_eq!(content.attachment_ids(), vec!["i".to_owned()]);
        assert!(MessageContent::text("hi").text_only());
    }

    /// INV-M1A-5：legacy id 派生跨调用稳定、对路径敏感、与 std hasher
    /// 解耦（显式常量在这里钉住）。
    #[test]
    fn legacy_attachment_id_is_stable_and_path_sensitive() {
        assert_eq!(
            legacy_attachment_id("/home/u/.clat/x/attachments/a.png"),
            legacy_attachment_id("/home/u/.clat/x/attachments/a.png")
        );
        assert_ne!(
            legacy_attachment_id("/a/b.png"),
            legacy_attachment_id("/a/c.png")
        );
        assert!(legacy_attachment_id("/a/b.png").starts_with("legacy-"));
        assert_eq!(legacy_attachment_id("/a/b.png").len(), "legacy-".len() + 16);
        // 已知向量：FNV-1a 64 空串 offset basis 的显式钉。
        assert_eq!(legacy_attachment_id(""), "legacy-cbf29ce484222325");
    }

    /// INV-M1A-1：ToolResultContent 形状（blocks 空 + legacy 输出 = 既有
    /// wire 语义；blocks 非空 = 多模态工具结果）。
    #[test]
    fn tool_result_content_shape_is_frozen() {
        let content = ToolResultContent {
            call_id: "c1".into(),
            tool_name: "read_file".into(),
            blocks: Vec::new(),
            is_error: false,
            legacy_output: Some(serde_json::json!({"ok": true})),
        };
        let wire = serde_json::to_string(&content).expect("serialize");
        assert_eq!(
            wire,
            r#"{"call_id":"c1","tool_name":"read_file","blocks":[],"is_error":false,"legacy_output":{"ok":true}}"#
        );
        let back: ToolResultContent = serde_json::from_str(&wire).expect("parse");
        assert_eq!(back, content);
    }
}
