//! 会话标题生成（能力批次 1 / F）。
//!
//! `core.session_title` 服务：用已配置模型把近期对话提炼为简短标
//! 题，替换 storage 落库时的"首行截断"默认值。单次小请求（无工具、
//! 短 deadline、单次尝试）；失败返回 None，调用方静默保留既有标题
//! （INV-F1）。标题契约（TITLE-CUT-1，2026-10-18）：上限按显示宽度
//! （列）计量——48 列（24 汉字 / 48 拉丁字母），grapheme 边界截断不劈
//! emoji；词内 run 不得切断，无合规前缀则拒绝而非存残词（依据见
//! docs/todo/ag-dogfood9-titlecut1-research-design-2026-10-18.md）。

use crate::model::{CancelToken, ModelConfig, ProviderCredentials};
use crate::plugin::{Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind};
use crate::plugins::services::{
    GeneratedTitle, SESSION_TITLE_SERVICE, SESSION_TITLE_SERVICE_ID, SessionTitler,
    UTILITY_MODEL_SERVICE, UTILITY_MODEL_SERVICE_ID, UtilityFailure, UtilityModel, UtilityTask,
};
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const ID: PluginId = PluginId::new("builtin.session_title");
const REQUIRES: &[crate::plugin::ServiceId] = &[UTILITY_MODEL_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: &[SESSION_TITLE_SERVICE_ID],
    requires: REQUIRES,
    optional: &[],
};

/// 标题硬上限：显示宽度 48 列（24 个汉字 / 48 个拉丁字母）。合规输出
/// （提示词目标 6 词 / 12 汉字）远小于它，截断只作兜底。
const TITLE_MAX_WIDTH: usize = 48;
/// 独立资源护栏：原始模型输出超过此字节数直接拒绝，不进入清洗。
const TITLE_RAW_MAX_BYTES: usize = 4096;
/// 切点后剥除的悬挂分隔符：空白与中西文逗号/冒号/连接符等。
const TRAILING_SEPARATORS: &[char] = &[
    ',', ';', ':', '/', '-', '—', '–', '·', '、', '，', '；', '：',
];

pub(crate) struct SessionTitlePlugin;

impl Plugin for SessionTitlePlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let utility = context
            .require(UTILITY_MODEL_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        context
            .provide(
                SESSION_TITLE_SERVICE,
                Arc::new(DefaultSessionTitler { utility }) as Arc<dyn SessionTitler>,
            )
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct DefaultSessionTitler {
    utility: Arc<dyn UtilityModel>,
}

impl SessionTitler for DefaultSessionTitler {
    fn enabled(&self) -> bool {
        self.utility.enabled(UtilityTask::SessionTitle)
    }

    fn generate_title(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
        conversation: &str,
        cancel: &CancelToken,
    ) -> Result<GeneratedTitle, UtilityFailure> {
        let output = self.utility.generate(
            UtilityTask::SessionTitle,
            config,
            credentials,
            conversation,
            cancel,
        )?;
        // 拒绝（超长原始输出/清洗为空/无合规前缀）时返回空标题，
        // 调用方保留旧标题并记 evidence（INV-F1 语义）。
        Ok(GeneratedTitle {
            title: sanitize_title(&output.text),
            provider: output.provider,
            model: output.model,
        })
    }
}

/// 取首个非空行、剥包裹引号/Markdown、剥控制序列并折叠空白；宽度合规
/// 的标题原样通过，超限按 grapheme/词边界截断，无合规前缀返回空（调用
/// 方保留旧标题并记一行有界 evidence）。
pub(crate) fn sanitize_title(raw: &str) -> String {
    sanitize_title_outcome(raw).unwrap_or_default()
}

/// 清洗结果：`Ok` 携带合规标题；`Err` 携带自有拒绝原因（进 evidence）。
pub(crate) type TitleSanitizeResult = Result<String, &'static str>;

pub(crate) fn sanitize_title_outcome(raw: &str) -> TitleSanitizeResult {
    // 资源护栏独立于宽度契约：超长原始输出不进入任何处理。
    if raw.len() > TITLE_RAW_MAX_BYTES {
        return Err("raw title output exceeds the bounded size");
    }
    let first = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut inner = first;
    while let Some(stripped) = strip_wrapping_markup(inner) {
        inner = stripped;
    }
    let cleaned = scrub_title_text(inner);
    if cleaned.is_empty() {
        return Err("generated title sanitized to empty");
    }
    cut_within_width(&cleaned, TITLE_MAX_WIDTH)
        .ok_or("no compliant prefix within the title width cap")
}

/// 剥终端控制序列（OSC/CSI/ESC）与双向/不可见控制字符，空白折叠为单空格
/// （清单对齐 DSH `normalizeSessionTitle` 的 `cleanTitleText`）。
fn scrub_title_text(text: &str) -> String {
    #[derive(PartialEq)]
    enum Scan {
        Text,
        Esc,
        Osc,
        OscEsc,
        Csi,
    }
    struct Sink {
        out: String,
        pending_space: bool,
        seen_any: bool,
    }
    impl Sink {
        /// 正文态的接收逻辑：序列引导符转序列态，不可见/控制丢弃，
        /// 空白折叠，其余保留。
        fn accept(&mut self, character: char) -> Scan {
            if character.is_whitespace() {
                if self.seen_any {
                    self.pending_space = true;
                }
                return Scan::Text;
            }
            match character {
                '\u{1b}' => return Scan::Esc,
                '\u{9b}' => return Scan::Csi, // 8 位 CSI
                '\u{9d}' => return Scan::Osc, // 8 位 OSC
                _ => {}
            }
            if character.is_control() || is_invisible(character) {
                return Scan::Text;
            }
            if self.pending_space {
                self.out.push(' ');
                self.pending_space = false;
            }
            self.out.push(character);
            self.seen_any = true;
            Scan::Text
        }
    }
    let mut sink = Sink {
        out: String::with_capacity(text.len()),
        pending_space: false,
        seen_any: false,
    };
    let mut scan = Scan::Text;
    for character in text.chars() {
        scan = match scan {
            Scan::Text => sink.accept(character),
            // ESC 引导：']' 进 OSC、'[' 进 CSI、@..=_ 消耗两字节序列；
            // 其余仅丢弃 ESC 本身，字符按正文重新处理。
            Scan::Esc => match character {
                ']' => Scan::Osc,
                '[' => Scan::Csi,
                '\u{40}'..='\u{5f}' => Scan::Text,
                other => sink.accept(other),
            },
            // OSC 直到 BEL 或 ESC \（ST）结束；载荷全吞。
            Scan::Osc => match character {
                // R1 非阻断：C1 ST（U+009C）同样结束 8 位 OSC（U+009D）。
                '\u{7}' | '\u{9c}' => Scan::Text,
                '\u{1b}' => Scan::OscEsc,
                _ => Scan::Osc,
            },
            Scan::OscEsc => Scan::Text,
            // CSI 参数/中间字节全吞，最终字节（0x40..=0x7E）结束序列。
            Scan::Csi => match character {
                '\u{40}'..='\u{7e}' => Scan::Text,
                _ => Scan::Csi,
            },
        };
    }
    sink.out
}

/// DSH 清单中的双向/不可见控制：ZWSP、LRM/RLM、bidi 覆盖、词连接符、
/// BOM——不可见字符不得参与标题。
fn is_invisible(character: char) -> bool {
    matches!(character,
        '\u{200b}' | '\u{200e}' | '\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206f}'
        | '\u{feff}')
}

/// 宽度合规判定与兜底截断：≤ `max` 列原样通过；超限取最大 grapheme
/// 前缀，且切点只能落在**词内 run 之外**（R1-F1/F2：无空格 CJK 连续
/// 串、含组合附标的词、字母上下文中的撇号词都是不可断 run——无法证明
/// 安全的超限输出没有合规前缀，返回 None 拒绝，绝不保存残词/残句）；
/// 尾部悬挂分隔符剥除。
fn cut_within_width(text: &str, max: usize) -> Option<String> {
    let flags = wordish_flags(text);
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    let mut total = 0usize;
    let mut widths = Vec::with_capacity(graphemes.len());
    let mut char_starts = Vec::with_capacity(graphemes.len());
    let mut char_index = 0usize;
    for grapheme in &graphemes {
        total += UnicodeWidthStr::width(*grapheme);
        widths.push(total);
        char_starts.push(char_index);
        char_index += grapheme.chars().count();
    }
    if total <= max {
        return Some(text.to_owned());
    }
    let mut cut = widths.partition_point(|width| *width <= max);
    while cut > 0 {
        let previous = flags[char_starts[cut] - 1];
        let next = flags.get(char_starts[cut]).copied().unwrap_or(false);
        if !previous || !next {
            break;
        }
        cut -= 1;
    }
    if cut == 0 {
        return None;
    }
    let prefix: String = graphemes[..cut].concat();
    let trimmed = prefix.trim_end_matches(|character: char| {
        character.is_whitespace() || TRAILING_SEPARATORS.contains(&character)
    });
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// 词内 run 的 char 级分类（R1-F1/F2 返工，含上下文归并）。返回与
/// `text.chars()` 一一对应的词内标志：
/// - 基础词内：字母数字（**含 CJK 字形**——`is_alphanumeric` 对汉字为
///   真，无空格书写系统的连续串与拉丁词同等不可从中间断）、技术连接
///   符 `_-./`、全角/CJK 标点；
/// - 附标跟随：簇内非首 char（组合附标、变体选择符、ZWJ、emoji 修饰
///   符）继承簇首判定——`e + U+0301` 不会让词在附标处"合法"断开；
/// - 字母上下文撇号（`'`/`’`）继承左侧：`can't` 是一个词，而引用式
///   开头的撇号不是。
fn wordish_flags(text: &str) -> Vec<bool> {
    let mut flags = Vec::with_capacity(text.chars().count());
    let mut previous_wordish = false;
    for cluster in text.graphemes(true) {
        let head = cluster.chars().next().unwrap_or_default();
        let base = if matches!(head, '\'' | '\u{2019}') {
            previous_wordish
        } else {
            is_word_internal_char(head)
        };
        for _ in cluster.chars() {
            flags.push(base);
        }
        previous_wordish = base;
    }
    flags
}

/// 词内字符（char 级，无上下文）：字母数字、`_-./` 与全角/CJK 标点。
/// CJK 字形经 `is_alphanumeric` 天然并入——R1-F1 的教训是"无空格所以
/// 可切"不成立：`…的问` 与 `…login b` 是同一类残词。
fn is_word_internal_char(character: char) -> bool {
    character.is_alphanumeric()
        || matches!(character, '_' | '-' | '.' | '/')
        || matches!(character,
            '\u{3000}'..='\u{303f}' // CJK 标点：，。、等并入 run，不切残句
            | '\u{ff01}'..='\u{ff64}' // 全角标点：！？：；（）
        )
}

fn strip_wrapping_markup(text: &str) -> Option<&str> {
    const PAIRS: [(&str, &str); 7] = [
        ("\"", "\""),
        ("「", "」"),
        ("『", "』"),
        ("**", "**"),
        ("__", "__"),
        ("`", "`"),
        ("~~", "~~"),
    ];
    for (open, close) in PAIRS {
        if let Some(body) = text
            .strip_prefix(open)
            .and_then(|body| body.strip_suffix(close))
        {
            return Some(body);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        FinishReason, Model, ModelError, ModelEventSink, ModelFactory, ModelProtocol, ModelRequest,
        ModelResponse,
    };
    use crate::plugin::{PluginId, PluginManager, PluginOwner};
    use crate::plugins::services::{PROVIDER_SERVICE, ProviderLease};
    use crate::plugins::{CompanionUtilityPlugin, ProviderRegistryPlugin, SessionTitlePlugin};
    use std::time::Duration;

    /// R1-F1（2026-10-18 返工）：无空格 CJK 超限输出没有可证明安全的切点
    /// ——不得把「问题」切成「问」落库。确定、有界、不存残词的策略是拒绝
    /// 本次自动命名（返回空，调用方保留旧标题 + evidence），而不是机械
    /// 按字形截到 48 列。以乙方 R1 原反例（明确中文短语）验证。
    #[test]
    fn sanitizer_rejects_over_limit_cjk_phrases_instead_of_cutting_mid_word() {
        // 乙方 R1 反例：25 汉字（50 列）→ 返工前实现返回前 24 字，把
        // 「问题」切成「问」真实落库；返工后必须拒绝。
        assert_eq!(
            sanitize_title("修复生产环境中用户登录失败以及无法访问数据库的问题"),
            ""
        );
        assert_eq!(sanitize_title(&"中".repeat(25)), "");
        // 合规中文短语（≤48 列）仍完整保存。
        assert_eq!(
            sanitize_title("修复生产环境登录失败及数据库访问问题"),
            "修复生产环境登录失败及数据库访问问题"
        );
        // 带空格的中英混排超限：只能在空格边界切，保留前一词块，后续整块
        // 不可断词不产生半个词。
        let mixed = format!("修复 {}", "中".repeat(30));
        assert_eq!(sanitize_title(&mixed), "修复");
    }

    /// R1-F2（2026-10-18 返工）：词完整独立于字形完整——grapheme 末码点是
    /// 组合附标时不得放行词中截断；字母上下文中的撘号属词内。均为乙方 R1
    /// 原反例。
    #[test]
    fn sanitizer_keeps_words_whole_across_marks_and_apostrophes() {
        // NFD 组合附标：a×47 + e + U+0301 + xyz 是一个连续词，超限时必须
        // 拒绝，而不是存下 `a…é` 这个词中残片。
        let nfd = format!("{}e\u{0301}xyz", "a".repeat(47));
        assert_eq!(sanitize_title(&nfd), "");
        // 撘号上下文：`can't` 是一个词；切点只能落在它之前的空格，剥尾
        // 空格后是 `a×45`，绝不能存 `a×45 can`。
        let apostrophe = format!("{} can't", "a".repeat(45));
        assert_eq!(sanitize_title(&apostrophe), "a".repeat(45));
        // 混合技术 token（下划线+CJK+版本号）超限：整 token 不可断 → 拒绝。
        assert_eq!(
            sanitize_title("fix_修复_v2_lockfile_corruption_regression_scenario_overflow"),
            ""
        );
    }

    /// R1 非阻断补充（2026-10-18）：C1 OSC（U+009D）须由对应 C1 ST（U+009C）
    /// 结束，不得把后续正文整段吞掉。
    #[test]
    fn sanitizer_ends_c1_osc_at_c1_st() {
        assert_eq!(sanitize_title("\u{9d}0;fake\u{9c}Fix login"), "Fix login");
    }

    /// R1 非阻断补充：三类拒绝原因在 outcome 层可区分（evidence 层按合同
    /// 统一记 sanitize 拒绝一行，见执行报告勘误）。
    #[test]
    fn sanitize_outcome_distinguishes_rejection_reasons() {
        assert_eq!(
            sanitize_title_outcome(&"x".repeat(4097)),
            Err("raw title output exceeds the bounded size")
        );
        assert_eq!(
            sanitize_title_outcome("   \"\"   "),
            Err("generated title sanitized to empty")
        );
        assert_eq!(
            sanitize_title_outcome(&"a".repeat(64)),
            Err("no compliant prefix within the title width cap")
        );
        assert_eq!(
            sanitize_title_outcome("Fix the login bug"),
            Ok("Fix the login bug".to_owned())
        );
    }

    #[test]
    fn sanitizer_uses_first_line_strips_markup_and_keeps_compliant_titles_whole() {
        // 首个非空行 + 剥引号。
        assert_eq!(
            sanitize_title("\n  \"Fix login bug\"  \nignored"),
            "Fix login bug"
        );
        assert_eq!(sanitize_title("「修复登录」"), "修复登录");
        assert_eq!(
            sanitize_title("\n\n**修复登录竞态**\nignored"),
            "修复登录竞态"
        );
        // 纯引号输出清洗后为空。
        assert_eq!(sanitize_title("   \"\"   "), "");
        // TITLE-CUT-1（2026-10-18）：合规标题必须完整保存，永不截断。
        // 完整英文短语 17 列 ≤ 上限 → 不再把 "Fix the login bug" 截成
        // "Fix the login bu"（旧码把残词当真值，本断言在旧码上红）。
        assert_eq!(sanitize_title("Fix the login bug"), "Fix the login bug");
        // 16 / 24 个汉字（32 / 48 列）完整保留——旧码 16 chars 一刀切。
        assert_eq!(sanitize_title(&"中".repeat(16)), "中".repeat(16));
        assert_eq!(sanitize_title(&"中".repeat(24)), "中".repeat(24));
        // 恰在上限：48 个拉丁字母（48 列）原样通过。
        let exact = "a".repeat(48);
        assert_eq!(sanitize_title(&exact), exact);
        // 上限内完整中文多词短语与中英混排（型号/文件名）不动。
        assert_eq!(
            sanitize_title("修复 WebSocket 重连问题"),
            "修复 WebSocket 重连问题"
        );
    }

    /// TITLE-CUT-1：超限标题按显示宽度上限截断，切点只落在词边界/
    /// grapheme 边界，绝不把半个词或词尾残片当合格标题保存。
    #[test]
    fn sanitizer_cuts_over_limit_titles_at_word_boundaries_never_mid_word() {
        // 56 列多词英文 → 46 列最大整词前缀（"pipeline" 后），无悬挂分隔符。
        assert_eq!(
            sanitize_title("Refactor the session title truncation pipeline carefully"),
            "Refactor the session title truncation pipeline"
        );
        // 25 个汉字（50 列）：R1-F1 裁定"CJK 无空格所以可在字形间切"
        // 不成立——无空格 CJK 连续串是不可断 run，超限拒绝而非截 24 字
        // （`…的问` 与 `…login b` 是同一类残词）。
        assert_eq!(sanitize_title(&"中".repeat(25)), "");
        // 中英混排：切点不得落进拉丁词内部——回退到 "修复" 整词。
        let mixed = format!("修复 {}", "a".repeat(50));
        assert_eq!(sanitize_title(&mixed), "修复");
        // 49 列但词边界恰在 48 列：保留整词，丢弃 " tail"。
        let boundary = format!("{} tail", "a".repeat(48));
        assert_eq!(sanitize_title(&boundary), "a".repeat(48));
        // 切点后的悬挂逗号剥除（46 个 a + "," = 47 列，下一词超限）。
        let dangling = format!("{}, more words", "a".repeat(46));
        assert_eq!(sanitize_title(&dangling), "a".repeat(46));
    }

    /// TITLE-CUT-1：单个不可断的超长 token（词内字符从 0 位起即超限）没有
    /// 合规前缀——拒绝本次自动命名（返回空，调用方保留旧标题），绝不
    /// 保存 16/48 个字符的词中残片。
    #[test]
    fn sanitizer_rejects_unbreakable_over_limit_tokens_instead_of_fragments() {
        assert_eq!(sanitize_title(&"a".repeat(64)), "");
        assert_eq!(
            sanitize_title("fix_session_v2_lockfile_corruption_regression_in_picker_rows"),
            ""
        );
    }

    /// TITLE-CUT-1：资源安全护栏（独立于宽度契约）——终端控制序列
    /// （OSC/CSI/ESC）与双向/不可见控制字符剥除（对齐 DSH
    /// normalizeSessionTitle 清单），恶意超长原始输出直接拒绝。
    #[test]
    fn sanitizer_scrubs_terminal_and_directional_controls_and_bounds_raw_output() {
        // CSI 整段剥除（连参数一起），不像用户标题清洗只剥 ESC 引导符。
        assert_eq!(sanitize_title("\u{1b}[31mFix login\u{1b}[0m"), "Fix login");
        // OSC 带 BEL 结束符整段剥除。
        assert_eq!(sanitize_title("\u{1b}]0;fake\u{7}修复"), "修复");
        // ZWSP/RLM/RLO 剥除：不可见字符不得参与标题。
        assert_eq!(
            sanitize_title("Fix\u{200b} login\u{202e}bug"),
            "Fix loginbug"
        );
        // 原始输出超过护栏上限 → 拒绝（不处理超长串）。
        assert_eq!(sanitize_title(&"x".repeat(4097)), "");
    }

    /// TITLE-CUT-1：emoji 组合字形（ZWJ 序列）是一个 grapheme（宽度按整
    /// 簇计量为 2），超限截断不得把它劈成两半。
    #[test]
    fn sanitizer_never_splits_emoji_grapheme_clusters() {
        let family = "\u{1f46d}\u{200d}\u{1f3fe}"; // couple + ZWJ + skin tone
        let short = format!("{family} fix login");
        assert_eq!(sanitize_title(&short), short);
        // 每单元（emoji 簇 2 列 + 空格 1 列）3 列；17 单元 51 列超限 →
        // 保留 16 个完整单元（48 列内最大前缀），剥尾空格，绝不劈开簇。
        let over = format!("{family} ").repeat(17);
        let expected = format!("{family} ").repeat(16);
        let expected = expected.trim_end();
        assert_eq!(sanitize_title(&over), expected);
    }

    struct CannedFactory(&'static str);

    impl ModelFactory for CannedFactory {
        fn protocol(&self) -> ModelProtocol {
            ModelProtocol::OpenAiCompatible
        }

        fn describe(&self, _credentials: &ProviderCredentials) -> crate::model::ProviderDescriptor {
            unimplemented!("not needed for title tests")
        }

        fn build(
            &self,
            _config: &ModelConfig,
            _credentials: &ProviderCredentials,
        ) -> Result<Box<dyn Model>, ModelError> {
            Ok(Box::new(CannedModel(self.0)))
        }
    }

    struct CannedModel(&'static str);

    impl Model for CannedModel {
        fn provider(&self) -> &str {
            "title-fake"
        }

        fn model_id(&self) -> &str {
            "title-fake"
        }

        fn stream(
            &mut self,
            _request: ModelRequest<'_>,
            _events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            Ok(ModelResponse {
                text: self.0.into(),
                tool_calls: Vec::new(),
                finish_reason: FinishReason::Completed,
                usage: None,
                provider_response_id: None,
                provider_state: Vec::new(),
                reasoning: None,
            })
        }
    }

    fn titler_with(factory: CannedFactory) -> Arc<dyn SessionTitler> {
        titler_with_factory(factory)
    }

    fn titler_with_factory(factory: impl ModelFactory + 'static) -> Arc<dyn SessionTitler> {
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(ProviderRegistryPlugin),
                Arc::new(CompanionUtilityPlugin),
                Arc::new(SessionTitlePlugin),
            ])
            .expect("mount");
        let providers = manager.require(PROVIDER_SERVICE).expect("providers");
        let _lease: ProviderLease = providers
            .register(
                PluginOwner::for_test(PluginId::new("test.title")),
                Arc::new(factory),
            )
            .map_err(|error| error.to_string())
            .expect("register");
        manager.require(SESSION_TITLE_SERVICE).expect("titler")
    }

    #[test]
    fn successful_generation_returns_sanitized_title() {
        let titler = titler_with(CannedFactory("  \"Fix the login bug\" "));
        let config = ModelConfig::default();
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        let title = titler
            .generate_title(
                &config,
                &credentials,
                "login fails on Safari",
                &CancelToken::new(),
            )
            .expect("title");
        // TITLE-CUT-1：完整短语（17 列 ≤ 上限）必须原样通过；旧码在此
        // 断言 "Fix the login bu"（把错截当真值），本断言在旧码上红。
        assert_eq!(title.title, "Fix the login bug");
    }

    /// 不变量（2026-08-19 退出延迟）：自动标题请求必须携带 deadline
    /// ——`remaining()` 有值时 provider 会把它配置成请求级 HTTP
    /// timeout，connect（30s）与等响应头（60s）阶段才有界；worker 的
    /// 取消令牌本身无 deadline，pre-fix 此处为 None，退出撞上在途标
    /// 题请求可被拖住数十秒。
    #[test]
    fn title_request_carries_a_deadline_so_http_phases_are_bounded() {
        use std::sync::Mutex;
        let seen: Arc<Mutex<Option<Duration>>> = Arc::new(Mutex::new(None));

        struct DeadlineProbingFactory(Arc<Mutex<Option<Duration>>>);
        impl ModelFactory for DeadlineProbingFactory {
            fn protocol(&self) -> ModelProtocol {
                ModelProtocol::OpenAiCompatible
            }
            fn describe(
                &self,
                _credentials: &ProviderCredentials,
            ) -> crate::model::ProviderDescriptor {
                unimplemented!("not needed for title tests")
            }
            fn build(
                &self,
                _config: &ModelConfig,
                _credentials: &ProviderCredentials,
            ) -> Result<Box<dyn Model>, ModelError> {
                Ok(Box::new(DeadlineProbingModel(Arc::clone(&self.0))))
            }
        }

        struct DeadlineProbingModel(Arc<Mutex<Option<Duration>>>);
        impl Model for DeadlineProbingModel {
            fn provider(&self) -> &str {
                "title-fake"
            }
            fn model_id(&self) -> &str {
                "title-fake"
            }
            fn stream(
                &mut self,
                request: ModelRequest<'_>,
                _events: &mut dyn ModelEventSink,
            ) -> Result<ModelResponse, ModelError> {
                *self.0.lock().expect("seen") = request.cancel.remaining();
                Ok(ModelResponse {
                    text: String::new(),
                    tool_calls: Vec::new(),
                    finish_reason: FinishReason::Completed,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
        }

        let titler = titler_with_factory(DeadlineProbingFactory(Arc::clone(&seen)));
        let config = ModelConfig::default();
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        // 空正文清洗为空标题：Err（"empty response"）——失败可诊断而非静默。
        let _ = titler.generate_title(&config, &credentials, "hello", &CancelToken::new());
        let remaining = *seen.lock().expect("seen");
        assert!(
            remaining.is_some(),
            "the title request must carry a deadline (pre-fix: the worker token has none)"
        );
        assert!(
            remaining.expect("checked Some") <= Duration::from_secs(15),
            "the deadline must be TITLE_DEADLINE, got {remaining:?}"
        );
    }

    struct FailingFactory(Arc<std::sync::atomic::AtomicUsize>);

    impl ModelFactory for FailingFactory {
        fn protocol(&self) -> ModelProtocol {
            ModelProtocol::OpenAiCompatible
        }

        fn describe(&self, _credentials: &ProviderCredentials) -> crate::model::ProviderDescriptor {
            unimplemented!("not needed for title tests")
        }

        fn build(
            &self,
            _config: &ModelConfig,
            _credentials: &ProviderCredentials,
        ) -> Result<Box<dyn Model>, ModelError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(ModelError::transport("title provider down"))
        }
    }

    #[test]
    fn generation_failure_returns_the_structured_category() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(ProviderRegistryPlugin),
                Arc::new(CompanionUtilityPlugin),
                Arc::new(SessionTitlePlugin),
            ])
            .expect("mount");
        let providers = manager.require(PROVIDER_SERVICE).expect("providers");
        let _lease: ProviderLease = providers
            .register(
                PluginOwner::for_test(PluginId::new("test.title")),
                Arc::new(FailingFactory(Arc::clone(&attempts))),
            )
            .map_err(|error| error.to_string())
            .expect("register");
        let titler = manager.require(SESSION_TITLE_SERVICE).expect("titler");
        let config = ModelConfig::default();
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        let failure =
            match titler.generate_title(&config, &credentials, "anything", &CancelToken::new()) {
                Ok(_) => panic!("a provider failure must surface a category"),
                Err(failure) => failure,
            };
        // R1-F1：错误只保留自有类别码；工厂消息 "title provider down"
        // 属于不可信正文形状，绝不出现在安全文本里。
        assert_eq!(
            failure,
            UtilityFailure::RequestFailed(crate::model::ModelErrorKind::Transport)
        );
        let safe = failure.safe_text();
        assert_eq!(safe, "request failed (transport)");
        assert!(!safe.contains("title provider down"));
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one budget reservation must permit only one provider attempt"
        );
    }
}
