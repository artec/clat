use super::*;

/// 提醒铃（2026-08-19，AFK 场景：对话结束/需要批准时人可能不在屏
/// 前）。run 结束（用户主动取消除外）、权限与 ask 弹框打开时响一声。
///
/// 四种模式：
/// - `Sound`（默认，2026-08-21）：系统播放器放一段系统提示音——macOS
///   `afplay` 放 `Funk.aiff`（`-v 2.0` 略放大），Linux 依次试
///   `paplay`/`aplay`，Windows 走 PowerShell SoundPlayer。零音频依赖
///   （cpal 在 Linux 要 ALSA 头文件，会破坏"Rust 工具链是唯一要求"）；
///   播放器或声音文件不存在时**回落 BEL**；
/// - `Terminal`（BEL `\x07`）：声音由终端模拟器决定——应用只能触发，
///   换音效去终端设置改（iTerm2/Warp 等支持自选铃声音效文件）；
/// - `Off`（`CLAT_NO_BELL=1`）：静音；
/// - `Command`（`CLAT_BELL_COMMAND="..."`）：任意 shell 命令（macOS
///   `afplay ~/Sounds/ding.aiff`、Linux `paplay ding.ogg`），完全自定
///   义。后台执行、stdio 全断开、失败静默——提醒是尽力而为，绝不影
///   响主流程。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum BellMode {
    Sound,
    Off,
    Command(String),
}

/// 环境变量 → 模式（纯函数，测试从这里推导）。默认 `Sound`。
pub(super) fn bell_mode_from_env(no_bell: Option<String>, command: Option<String>) -> BellMode {
    let silenced = no_bell
        .map(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    if silenced {
        return BellMode::Off;
    }
    match command {
        Some(command) if !command.trim().is_empty() => BellMode::Command(command),
        _ => BellMode::Sound,
    }
}

/// 平台默认提示音（路线 A：系统播放器 + 系统自带声音，零新依赖）。
/// 返回 `(程序, 参数)`；播放器对应的声音文件不存在（或平台未适配）
/// 时返回 `None`——调用方回落 BEL。提示是尽力而为。
pub(super) fn system_sound_command() -> Option<(std::path::PathBuf, Vec<String>)> {
    let exists = |path: &str| std::path::Path::new(path).exists();
    #[cfg(target_os = "macos")]
    {
        // Funk：比 iTerm 默认 Boop 长且低沉；`-v 2.0` 略放大（>1.0 是
        // 放大区间，截断失真由用户经 CLAT_BELL_COMMAND 自调）。
        let sound = "/System/Library/Sounds/Funk.aiff";
        if !exists(sound) {
            return None;
        }
        Some((
            std::path::PathBuf::from("afplay"),
            vec!["-v".into(), "2.0".into(), sound.into()],
        ))
    }
    #[cfg(target_os = "linux")]
    {
        // freedktop 主题音优先（paplay）；ALSA 样例 wav 兜底（aplay）。
        let ogg = "/usr/share/sounds/freedesktop/stereo/complete.oga";
        if exists(ogg) {
            return Some((std::path::PathBuf::from("paplay"), vec![ogg.into()]));
        }
        let wav = "/usr/share/sounds/alsa/Front_Center.wav";
        if exists(wav) {
            return Some((std::path::PathBuf::from("aplay"), vec![wav.into()]));
        }
        None
    }
    #[cfg(target_os = "windows")]
    {
        let wav = "C:\\Windows\\Media\\Windows Notify System Generic.wav";
        if !exists(wav) {
            return None;
        }
        Some((
            std::path::PathBuf::from("powershell"),
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                format!("(New-Object System.Media.SoundPlayer '{wav}').PlaySync()"),
            ],
        ))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

/// 响一声。BEL 直写 stdout：raw 模式只改输入侧，模拟器收到 BEL 按
/// 自己的铃设置发声（或视觉闪铃）。声音/命令模式 detached spawn
/// （不 wait、不接管 stdio）。
pub(super) fn ring_bell(mode: &BellMode) {
    match mode {
        BellMode::Off => {}
        BellMode::Command(command) => {
            spawn_detached_and_reap("sh", vec!["-c".to_owned(), command.clone()]);
        }
        BellMode::Sound => match system_sound_command() {
            Some((program, args)) => spawn_detached_and_reap(&program.display().to_string(), args),
            // 播放器/声音不可用：回落终端铃（用户侧仍可换终端音效）。
            None => {
                let _ = write!(stdout(), "\x07");
                let _ = stdout().flush();
            }
        },
    }
}

/// detached spawn + 专属收割线程。Child drop 是 detach 不是 reap：不
/// wait 的话每次响铃留一个僵尸到进程退出（对抗审计 2026-08-19）。提
/// 醒命令都是短命进程，一个收割线程足够。spawn 失败静默（提醒尽力
/// 而为）。
fn spawn_detached_and_reap(program: &str, args: Vec<String>) {
    if let Ok(mut child) = std::process::Command::new(program)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}
