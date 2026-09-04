use super::*;

/// Spinner frames for the "thinking" indicator（状态栏唯一旋转元素）。
/// 2026-08-19 起帧步进为每 [`SPINNER_STEP_TICKS`] 个渲染 tick（160ms/帧
/// @80ms 唤醒）：80ms 下盲文旋转快得看不清。
pub(super) const SPINNER_FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
/// spinner 帧步进（渲染 tick 数）。
pub(super) const SPINNER_STEP_TICKS: u64 = 2;

/// 动画帧号换算（纯函数，供 [`App::animation_tick`] 与不变量测试）：
/// 帧号 = 流逝时间 / 帧周期。不变量 A-CLK：同一时刻任意次重绘得到
/// 同一帧；帧差只由时间差决定——与重绘次数、绘制耗时、内容长度
/// 全部无关（2026-08-19 用户三次反馈的根因是 draw() 自增帧号）。
pub(super) fn animation_tick_for(elapsed: Duration) -> u64 {
    elapsed.as_millis() as u64 / SPINNER_FRAME.as_millis() as u64
}

/// 当前 spinner 帧字形（阶段行专用）。
pub(super) fn spinner_frame(tick: u64) -> &'static str {
    SPINNER_FRAMES[((tick / SPINNER_STEP_TICKS) % SPINNER_FRAMES.len() as u64) as usize]
}

/// 会话区流式 assistant 前缀的"太阳"帧：四分圆旋转——保持圆形字形
///（用户要求：原来的点是圆的，替代品也应是圆的），灰色与落定 ⏺ 同色
/// 族、与状态栏的蓝色盲文 spinner 不同形不同色，不构成重复（2026-08-19
/// 第二轮反馈：两个盲文旋转并排不好看）。
pub(super) const MARKER_FRAMES: [&str; 4] = ["◐", "◓", "◑", "◒"];

/// 当前流式前缀帧（会话区专用，与 spinner 同步进、不同字形）。
pub(super) fn marker_frame(tick: u64) -> &'static str {
    MARKER_FRAMES[((tick / SPINNER_STEP_TICKS) % MARKER_FRAMES.len() as u64) as usize]
}

/// run 内当前派生阶段（phase-1 P1-5）：从既有事件流派生，非独立状态机
/// 输入；每个模型步（ModelRequested）重开 Waiting，步内只前进不回退。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Phase {
    WaitingFirstToken,
    Thinking,
    Responding,
    ExecutingTools,
}

impl Phase {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::WaitingFirstToken => "Waiting first token",
            Self::Thinking => "Thinking…",
            Self::Responding => "Responding",
            Self::ExecutingTools => "Executing tools",
        }
    }
}

/// 阶段与双时钟的纯状态机（G6 可直接单测）。
#[derive(Default)]
pub(super) struct PhaseTracker {
    pub(super) phase: Option<Phase>,
    pub(super) phase_started: Option<Instant>,
    pub(super) run_started: Option<Instant>,
}

impl PhaseTracker {
    /// 新模型步：阶段重开为 Waiting（DSH ttft 语义），run 钟只启一次。
    pub(super) fn model_requested(&mut self) {
        self.phase = Some(Phase::WaitingFirstToken);
        self.phase_started = Some(Instant::now());
        self.run_started.get_or_insert_with(Instant::now);
    }

    /// 步内只前进：Waiting→Thinking→Responding→ExecutingTools。
    pub(super) fn advance(&mut self, target: Phase) {
        if self.phase.is_none() {
            return;
        }
        if self.phase.is_some_and(|current| target > current) {
            self.phase = Some(target);
            self.phase_started = Some(Instant::now());
        }
    }

    /// run 终态：全部计时状态清空，不留活计时器（G6）。
    pub(super) fn finish(&mut self) {
        self.phase = None;
        self.phase_started = None;
        self.run_started = None;
    }
}

/// 双时钟格式：<1 分钟 `8s`，≥1 分钟 `1m05s`。
fn format_clock(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}
/// 探照灯单字符驻留（渲染 tick）：光带每 [`SWEEP_STEP_TICKS`] 个 tick
/// 前进**一个字符**（2026-08-19 第五轮：驻留减半提速一倍——160ms 太
/// 慢，人类体感拖沓）。锚点仍是单字符照亮时长，不锚定整圈周期：标签
/// 长则整圈按比例长，视觉速度恒定。转圈不再与光带同步：spinner 保持
/// 每 [`SPINNER_STEP_TICKS`]（=2）tick 一帧——即**每照亮 2 个字符转圈
/// 换一帧**（8 帧一圈 = 16 字符 × 80ms = 1.28s），转速既不随标签长度
/// 变，也不随探照灯提速变。旧两版各自的病根：整圈周期恒定 → 每字速
/// 度随字数变（v0.6.1 的 bug）；整词呼吸 → 移动消失成霓虹。tick 墙钟
/// 驱动（A-CLK），驻留与重绘频率无关。
pub(super) const SWEEP_STEP_TICKS: u64 = 1;
/// 光带进出余量（字符）：出光后灯光完全离开尾字符、再从首字符进入，
/// 换圈处没有亮度跳变；余量足以把高斯尾压到熄灭阈之下。
pub(super) const SWEEP_MARGIN_CHARS: u64 = 3;
/// 光带柔和度（字符）。
pub(super) const SHIMMER_SIGMA: f64 = 1.2;
/// 熄灭阈：高斯尾低于此值按 0 处理——"灯光过去后回原色"是精确的
/// 基色，而不是差 1 个 RGB 值的残影。
pub(super) const SHIMMER_UNLIT_FLOOR: f64 = 0.03;

/// 派生阶段状态行（phase-1 P1-5）：spinner + 探照灯阶段标签 + 双时钟
/// `<phase> <phase-elapsed> · total <run-elapsed>`；Waiting 只报总计。
pub(super) fn phase_line(
    tick: u64,
    phase: Phase,
    phase_elapsed: Option<Duration>,
    run_elapsed: Option<Duration>,
    steering_queued: usize,
) -> Line<'static> {
    let frame = spinner_frame(tick);
    let base = theme::style(theme::Role::ThinkingGlyph);

    let mut spans = vec![
        Span::styled(frame, base.add_modifier(Modifier::BOLD)),
        Span::styled(" ", base),
    ];

    // 探照灯：光带中心每 SWEEP_STEP_TICKS（1 tick）前进一个字符，
    // 范围 [-margin, len+margin)。先照亮的字符在灯光走过后回基色；
    // 高斯尾给出柔和的边缘。转圈独立节律（每 2 字符一帧，见常量注
    // 释）。
    let label = phase.label();
    let len = label.chars().count() as u64;
    let cycle = len + 2 * SWEEP_MARGIN_CHARS;
    let center = ((tick / SWEEP_STEP_TICKS) % cycle) as f64 - SWEEP_MARGIN_CHARS as f64;
    for (index, ch) in label.chars().enumerate() {
        let distance = (index as f64 + 0.5 - center).abs();
        let intensity = (-(distance * distance) / (2.0 * SHIMMER_SIGMA * SHIMMER_SIGMA)).exp();
        let intensity = if intensity < SHIMMER_UNLIT_FLOOR {
            0.0
        } else {
            intensity
        };
        spans.push(Span::styled(
            ch.to_string(),
            Style::default().fg(theme::blend(
                theme::BRAND_SHIMMER_LOW,
                theme::BRAND_SHIMMER_HIGH,
                intensity,
            )),
        ));
    }

    if let Some(run_elapsed) = run_elapsed {
        let clocks = match (phase, phase_elapsed) {
            (Phase::WaitingFirstToken, _) => format!(" {}", format_clock(run_elapsed)),
            (_, Some(phase_elapsed)) => format!(
                " {} · total {}",
                format_clock(phase_elapsed),
                format_clock(run_elapsed)
            ),
            (_, None) => format!(" · total {}", format_clock(run_elapsed)),
        };
        spans.push(Span::styled(clocks, theme::style(theme::Role::Faint)));
    }
    if steering_queued > 0 {
        // DSH `N queued` 徽标：advisory 实时状态，claim 后随
        // SteeringApplied 回收。
        spans.push(Span::styled(
            format!(" · steering·{steering_queued}"),
            theme::style(theme::Role::Warning),
        ));
    }
    Line::from(spans)
}

/// 会话累计的缓存命中百分比文本（如 "99.99%"，两位小数）。无输入
/// token 或服务端未上报缓存命中时不显示（返回 None）。
pub(super) fn cache_hit_percent(usage: &Usage) -> Option<String> {
    let cached = usage.cached_input_tokens?;
    if usage.input_tokens == 0 {
        return None;
    }
    // Some(0) 是"服务端上报了零命中"——真实的 0.00%，不是未知。
    let percent = cached as f64 / usage.input_tokens as f64 * 100.0;
    Some(format!("{percent:.2}%"))
}

/// token 数的紧凑展示：`1M` / `1.5M` / `120k` / `999`。千位以上四舍
/// 五入到 k，百万以上保留一位小数（整数则省略小数部分）。
pub(super) fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        // 一位小数，整数值省略小数部分（1.0M → 1M）。
        let millions = format!("{:.1}", tokens as f64 / 1_000_000.0);
        format!("{}M", millions.trim_end_matches(".0"))
    } else if tokens >= 1_000 {
        format!("{}k", (tokens + 500) / 1_000)
    } else {
        tokens.to_string()
    }
}

/// 缩短路径用于状态栏展示：home 前缀替换为 `~`
/// （如 `~/Documents/GitHub/clat`），非 home 路径原样返回。
pub(super) fn abbreviate_home(path: &Path) -> String {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    match home {
        Some(home) => abbreviate_with(path, &home),
        None => path.display().to_string(),
    }
}

pub(super) fn abbreviate_with(path: &Path, home: &Path) -> String {
    // 退化 HOME（空串或 "/"——无 passwd 条目的容器环境常见）会让
    // strip_prefix 匹配一切绝对路径，把所有路径都吞成 `~/…`；退化为
    // 不缩写。
    if home.as_os_str().is_empty() || home == Path::new("/") {
        return path.display().to_string();
    }
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// 状态栏右侧遥测段，按优先级降序（额度 > Cache > Context）。Wallet/
/// Token 段随余额查询就绪；Cache/Context 对 DeepSeek/GLM **常驻**——
/// 无数据时显示 `--%` / `0`（2026-08-19 用户反馈：启动/首跑中途三段
/// 必须齐全，布局不跳变）。journal 还原 + 流式实时累计让真实值尽早
/// 出现。渲染时 `fit_status_suffix` 在窄终端从尾部（最低优先）开始
/// 让位。
///
/// - DeepSeek：`Wallet: ￥89.35 · Cache: 99.99% · Context: 120k/1M`
/// - GLM Coding Plan：`Token: 87% · Cache: 99.99% · Context: 120k/1M`
///   （Token 段是 5 小时窗口剩余额度）
///
/// INV-C1（2026-08-21，用户报告切模型 Cache 残留）：Cache 口径按模型
/// 路由分桶——`route_usage` 是**当前配置路由**的桶，切换模型后不与
/// 其它模型的累计混合；桶缺席（该路由还没跑过）显示 `--%`。来回切换
/// 不清零：服务端缓存跨往返存活，各路由的口径也跨往返保留。
///
/// 思考档位不在这里——它属于标题栏（`compose_header_rest`）。
/// INV-C1：取"当前配置路由"的用量桶；键与 journal `source {provider,
/// model}` 同口径（provider = `protocol.to_string()`，agent 运行时
/// 同源传参），三端（折叠/活账/显示）共用防漂移。
pub(super) fn current_route_usage<'a>(
    routes: &'a BTreeMap<String, Usage>,
    config: &ModelConfig,
) -> Option<&'a Usage> {
    routes.get(&crate::model::model_route_key(
        &config.protocol.to_string(),
        &config.model,
    ))
}

pub(super) fn status_suffix_segments(
    config: &ModelConfig,
    balance: &Option<String>,
    route_usage: Option<&Usage>,
    last_turn_usage: Option<&Usage>,
) -> Vec<String> {
    let mut parts = Vec::new();
    if config.vendor() == ModelVendor::Other {
        return parts;
    }
    // DeepSeek 槽位存余额文本，加 Wallet 标签与货币符号；GLM 槽位存
    // 5 小时窗口剩余额度百分比（如 "87%"），加 Token 标签。
    if let Some(balance) = balance {
        if config.vendor() == ModelVendor::DeepSeek {
            parts.push(format!("Wallet: ￥{balance}"));
        } else {
            parts.push(format!("Token: {balance}"));
        }
    }
    // Cache 段对 DeepSeek/GLM 常驻：无数据时显示 `--%`（该路由尚未跑
    // 过、或适配器未上报），三段布局自启动起稳定。
    let cache = route_usage
        .and_then(cache_hit_percent)
        .unwrap_or_else(|| "--%".into());
    parts.push(format!("Cache: {cache}"));
    // Context 当前值 ≈ 最近一次模型请求的 input+output（下一次请求
    // 的近似起点）；分母是预设的官方上下文窗口，自定义端点未知则
    // 省略整段。新会话无请求历史时按 0 计。
    let window = config
        .preset
        .as_deref()
        .and_then(preset_by_id)
        .map(|preset| preset.context_window);
    if let Some(window) = window {
        let current = last_turn_usage
            .map(|usage| usage.input_tokens.saturating_add(usage.output_tokens))
            .unwrap_or(0);
        parts.push(format!(
            "Context: {}/{}",
            format_tokens(current),
            format_tokens(window as u64)
        ));
    }
    parts
}

/// 左侧常规状态（错误/取消/权限提示）的最小保留宽度（TUI-L02）：
/// 右侧遥测宁可整段省略也不挤掉左侧。
pub(super) const MIN_STATUS_LEFT: u16 = 20;

/// 在 `budget` 显示宽度内按优先级保留遥测段：装不下低优先段时，它
/// 及其后继全部省略；首段都装不下则整体让位（左侧状态优先）。
pub(super) fn fit_status_suffix(segments: &[String], budget: usize) -> String {
    let mut kept: Vec<&str> = Vec::new();
    for segment in segments {
        let width = kept
            .iter()
            .map(|text| UnicodeWidthStr::width(*text))
            .chain(std::iter::once(UnicodeWidthStr::width(segment.as_str())))
            .sum::<usize>()
            + 3 * kept.len();
        if width > budget {
            break;
        }
        kept.push(segment.as_str());
    }
    kept.join(" · ")
}

/// 标题栏首行的数据源参数（D-2 §2.1：local 与 dsh 两模式共用同一
/// 组合器——model 来自 config 预设名或 `DshState.model_label`，level
/// 来自本地 ThinkingLevel 或 dsh 档位展示名（档位接入 2026-08-23），
/// 退化规则与组内间距两模式逐字节一致）。
pub(super) struct HeaderModel<'a> {
    pub(super) version: &'a str,
    pub(super) state: &'a str,
    pub(super) model: &'a str,
    pub(super) level: Option<&'a str>,
    /// VP-3：local 由当前 config 的能力方法注入；dsh 没有 CLAT 能力
    /// 矩阵事实，调用方固定传 false。
    pub(super) image_input: bool,
}

/// 标题栏首行在 "CLAT" 之后的内容，按可用显示宽度逐级退化（TUI-L02），
/// 保证档位在窄终端仍可见：
///
/// 1. 完整：` v0.5.1  ready  ·  {model} · Thinking · {level}`
/// 2. 紧凑：` v0.5.1 ready · {model} · {level}`（压缩间距、省略
///    "Thinking · " 文案）
/// 3. 最小：` v0.5.1 ready · Thinking · {level}`（省略模型名）
///
/// 模型+思考+强度是一个整体：组内分隔符统一窄间距 ` · `，与主分段
/// 的宽间距 `  ·  ` 区分。无档位（未配置 / 非 DeepSeek/GLM / 手工
/// 关闭 / dsh 模式）时各层级不含档位片段；三级都放不下交由终端截断。
/// 返回带样式的段序列：模型名段走 `Role::ModelAccent`（D-2 闪光点 b，
/// local 与 dsh 同色），其余为原色。
pub(super) fn compose_header_rest(header: &HeaderModel<'_>, width: usize) -> Vec<Span<'static>> {
    let HeaderModel {
        version,
        state,
        model,
        level,
        image_input,
    } = header;
    let model = if *image_input {
        format!("{model} ⧉")
    } else {
        (*model).to_owned()
    };
    let full_prefix = format!(" v{version}  {state}  ·  ");
    let full_suffix = level
        .map(|level| format!(" · Thinking · {level}"))
        .unwrap_or_default();
    if spans_width(&full_prefix, &model, &full_suffix) <= width {
        return styled_spans(full_prefix, &model, full_suffix);
    }
    let compact_prefix = format!(" v{version} {state} · ");
    let compact_suffix = level.map(|level| format!(" · {level}")).unwrap_or_default();
    if spans_width(&compact_prefix, &model, &compact_suffix) <= width {
        return styled_spans(compact_prefix, &model, compact_suffix);
    }
    vec![Span::raw(match level {
        Some(level) => format!(" v{version} {state} · Thinking · {level}"),
        None => format!(" v{version} {state}"),
    })]
}

fn spans_width(prefix: &str, model: &str, suffix: &str) -> usize {
    UnicodeWidthStr::width(prefix) + UnicodeWidthStr::width(model) + UnicodeWidthStr::width(suffix)
}

fn styled_spans(prefix: String, model: &str, suffix: String) -> Vec<Span<'static>> {
    vec![
        Span::raw(prefix),
        Span::styled(
            model.to_owned(),
            super::theme::style(super::theme::Role::ModelAccent),
        ),
        Span::raw(suffix),
    ]
}

/// 瞬时提示是否已到期：无过期时刻（常驻状态）视为未到期。
pub(super) fn status_expired(until: Option<Instant>, now: Instant) -> bool {
    until.is_some_and(|until| now >= until)
}
