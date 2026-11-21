use crate::app::Desktop;
use clat_core::client_ports::host::{
    BackendConnection, HostClient, HostNotification, NotificationOutcome,
};
use clat_core::client_ports::presentation::{LocaleContext, UiMessage};
use std::sync::atomic::Ordering;
use tauri::Manager;
use tauri_plugin_notification::{NotificationExt, PermissionState};

#[derive(Clone, Debug, Eq, PartialEq)]
enum DesktopSoundMode {
    Sound(String),
    Off,
    Command(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BannerAudio {
    Sound(String),
    Silent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeliveryPlan {
    banner_audio: BannerAudio,
    run_custom_command: Option<String>,
    fallback_sound: Option<String>,
}

pub fn start(app: tauri::AppHandle) {
    let clicked = app.clone();
    let _ = app.notification().on_action(move |action| {
        if action.action_id() != "tap" {
            return;
        }
        let target = action.notification().and_then(|notice| {
            clicked
                .state::<Desktop>()
                .notification_targets
                .lock()
                .ok()?
                .remove(&notice.id())
        });
        if let Some((client, notice)) = target {
            let app = clicked.clone();
            std::thread::spawn(move || focus_notification(&app, client, notice));
        } else {
            crate::app::show(&clicked);
        }
    });
    std::thread::spawn(move || {
        crate::startup::restore(&app);
        let _ = ensure_notification_permission(&app);
        while !app.state::<Desktop>().quitting.load(Ordering::Acquire) {
            observe_host(&app);
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    });
}

fn focus_notification(app: &tauri::AppHandle, client: HostClient, notice: HostNotification) {
    let state = app.state::<Desktop>();
    let _connection = state.connection.lock().expect("connection state");
    if state.updates.lock().expect("updates").preserve_page() {
        crate::app::show(app);
        return;
    }
    let current = state.host.lock().expect("host state").clone();
    if current.is_some_and(|h| h.instance_id() == client.instance_id())
        && let Ok(client) = client.focus_notification(&notice)
    {
        let _ = crate::app::connected(app, client);
    }
    crate::app::show(app);
}

fn observe_host(app: &tauri::AppHandle) {
    let state = app.state::<Desktop>();
    if state.startup.blocked() {
        return;
    }
    let Some(backend) = state.backend.as_ref() else {
        return;
    };
    let _connection = state.connection.lock().expect("connection state");
    let current = state.host.lock().expect("host state").clone();
    match backend.reconcile_presented(current.as_ref()) {
        Ok(BackendConnection::Connected(client)) => observe_connected(app, client),
        Ok(BackendConnection::Missing) => {
            if state.host.lock().expect("host state").take().is_some() {
                *state.connection_error.lock().expect("connection state") = None;
                let _ = crate::app::welcome(app);
            }
        }
        Err(error) => *state.connection_error.lock().expect("connection state") = Some(error),
    }
}

fn observe_connected(app: &tauri::AppHandle, client: HostClient) {
    let state = app.state::<Desktop>();
    let changed = state
        .host
        .lock()
        .expect("host state")
        .as_ref()
        .map(|h| h.instance_id())
        != Some(client.instance_id());
    if changed && let Err(error) = crate::app::connected(app, client.clone()) {
        *state.connection_error.lock().expect("connection state") = Some(error.into());
        return;
    }
    *state.connection_error.lock().expect("connection state") = None;
    refresh(app, &client);
}

fn refresh(app: &tauri::AppHandle, host: &HostClient) {
    let key = host.instance_id().to_owned();
    let state = app.state::<Desktop>();
    let mut streams = state.streams.lock().expect("notification streams");
    if streams.contains_key(&key) {
        return;
    }
    let Ok(mut events) = host.notification_events() else {
        return;
    };
    let Ok(interrupt) = events.interrupt_handle() else {
        return;
    };
    streams.insert(key.clone(), interrupt);
    drop(streams);
    crate::tray::sync_project_titles(app, host);
    let app = app.clone();
    let client = host.clone();
    std::thread::spawn(move || {
        let mut seen = std::collections::VecDeque::new();
        while let Ok((kind, value)) = events.next_frame() {
            if app.state::<Desktop>().quitting.load(Ordering::Acquire) {
                break;
            }
            if kind != "notification" {
                continue;
            }
            let Ok(notice) = serde_json::from_value::<HostNotification>(value) else {
                continue;
            };
            if seen.contains(&notice.id) {
                continue;
            }
            if seen.len() >= 256 {
                seen.pop_front();
            }
            seen.push_back(notice.id.clone());
            publish(&app, &client, notice);
        }
        app.state::<Desktop>()
            .streams
            .lock()
            .expect("notification streams")
            .remove(&key);
    });
}

fn is_window_in_foreground(visible: bool, minimized: bool, focused: bool) -> bool {
    visible && !minimized && focused
}

fn main_window_in_foreground(app: &tauri::AppHandle) -> bool {
    let Some(window) = app.get_webview_window("main") else {
        return false;
    };
    let visible = window.is_visible().unwrap_or(false);
    let minimized = window.is_minimized().unwrap_or(false);
    let focused = window.is_focused().unwrap_or(false);
    is_window_in_foreground(visible, minimized, focused)
}

fn ensure_notification_permission(app: &tauri::AppHandle) -> bool {
    match app.notification().permission_state() {
        Ok(PermissionState::Granted) => true,
        Ok(PermissionState::Denied) => false,
        Ok(_) => matches!(
            app.notification().request_permission(),
            Ok(PermissionState::Granted)
        ),
        Err(error) => {
            eprintln!("[CLAT] notification permission check failed: {error}");
            false
        }
    }
}

fn sound_mode_from_sources(
    no_bell: Option<String>,
    command: Option<String>,
    selected: &str,
) -> DesktopSoundMode {
    let silenced = no_bell
        .map(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    if silenced {
        return DesktopSoundMode::Off;
    }
    if let Some(command) = command
        && !command.trim().is_empty()
    {
        return DesktopSoundMode::Command(command);
    }
    let normalized = crate::notification_sounds::parse_saved_sound(Some(selected));
    if normalized == crate::notification_sounds::OFF {
        DesktopSoundMode::Off
    } else {
        DesktopSoundMode::Sound(normalized)
    }
}

fn desktop_sound_mode(app: &tauri::AppHandle) -> DesktopSoundMode {
    let selected = app.state::<Desktop>().notification_sounds.selected();
    sound_mode_from_sources(
        std::env::var("CLAT_NO_BELL").ok(),
        std::env::var("CLAT_BELL_COMMAND").ok(),
        &selected,
    )
}

fn delivery_plan(mode: &DesktopSoundMode) -> DeliveryPlan {
    match mode {
        DesktopSoundMode::Off => DeliveryPlan {
            banner_audio: BannerAudio::Silent,
            run_custom_command: None,
            fallback_sound: None,
        },
        DesktopSoundMode::Command(command) => DeliveryPlan {
            banner_audio: BannerAudio::Silent,
            run_custom_command: Some(command.clone()),
            fallback_sound: None,
        },
        DesktopSoundMode::Sound(sound) => DeliveryPlan {
            banner_audio: BannerAudio::Sound(sound.clone()),
            run_custom_command: None,
            fallback_sound: Some(sound.clone()),
        },
    }
}

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

fn play_fallback_sound(sound: &str) {
    if let Some((program, args)) = crate::notification_sounds::playback_command(sound) {
        spawn_detached_and_reap(&program.display().to_string(), args);
    }
}

fn play_custom_sound(command: &str) {
    #[cfg(target_os = "macos")]
    spawn_detached_and_reap("sh", vec!["-c".to_owned(), command.to_owned()]);
    #[cfg(windows)]
    spawn_detached_and_reap("cmd", vec!["/C".to_owned(), command.to_owned()]);
}

fn publish(app: &tauri::AppHandle, client: &HostClient, notice: HostNotification) {
    if notice.kind == "workspace.updated" {
        crate::tray::sync_project_titles(app, client);
        return;
    }
    if main_window_in_foreground(app) {
        return;
    }
    static NEXT_ID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);
    let Some((title, body)) = notification_content(crate::localization::context(app), &notice)
    else {
        return;
    };
    let plan = delivery_plan(&desktop_sound_mode(app));
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let state = app.state::<Desktop>();
    if let Ok(mut targets) = state.notification_targets.lock() {
        if targets.len() >= 64 {
            targets.pop_first();
        }
        targets.insert(id, (client.clone(), notice));
    }
    let banner_shown = show_banner(app, id, title, body, &plan.banner_audio);
    if let Some(command) = &plan.run_custom_command {
        play_custom_sound(command);
    } else if !banner_shown
        && let Some(sound) = &plan.fallback_sound
    {
        play_fallback_sound(sound);
    }
}

fn show_banner(
    app: &tauri::AppHandle,
    id: i32,
    title: &str,
    body: String,
    audio: &BannerAudio,
) -> bool {
    if !ensure_notification_permission(app) {
        return false;
    }
    let builder = app.notification().builder().id(id).title(title).body(body);
    let builder = match audio {
        BannerAudio::Sound(sound) => builder.sound(sound),
        BannerAudio::Silent => builder.silent(),
    };
    match builder.show() {
        Ok(()) => true,
        Err(error) => {
            eprintln!("[CLAT] desktop notification failed: {error}");
            false
        }
    }
}

fn notification_content(
    locale: LocaleContext,
    notice: &HostNotification,
) -> Option<(&'static str, String)> {
    let outcome = notice.completion.as_ref().map(|result| result.outcome);
    let title = match (notice.kind.as_str(), outcome) {
        ("completed", Some(NotificationOutcome::Succeeded)) => "desktop.notification_run_succeeded",
        ("completed", Some(NotificationOutcome::Failed)) => "desktop.notification_run_failed",
        ("completed", _) => "desktop.notification_completed",
        ("job_finished", Some(NotificationOutcome::Succeeded)) => {
            "desktop.notification_job_succeeded"
        }
        ("job_finished", Some(NotificationOutcome::Failed)) => "desktop.notification_job_failed",
        ("job_finished", Some(NotificationOutcome::Stopped)) => "desktop.notification_job_stopped",
        ("job_finished", Some(NotificationOutcome::Interrupted)) => {
            "desktop.notification_job_interrupted"
        }
        ("job_finished", _) => "job.notification",
        ("approval", _) => "desktop.notification_approval",
        ("question", _) => "desktop.notification_question",
        _ => return None,
    };
    let body = if matches!(notice.kind.as_str(), "completed" | "job_finished") {
        completion_body(locale, notice)
    } else {
        locale.text("desktop.notification_body").into()
    };
    Some((locale.text(title), body))
}

fn completion_body(locale: LocaleContext, notice: &HostNotification) -> String {
    // The breadcrumb here must not expose parent paths or prompt-derived titles.
    let project = notice.project_root.file_name().map(|name| {
        let name = name.to_string_lossy();
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        clat_core::client_ports::redact_secrets(&name)
    });
    let duration = notice
        .completion
        .as_ref()
        .and_then(|result| result.elapsed_ms)
        .map(|ms| completion_duration(locale, ms));
    match (project.filter(|name| !name.is_empty()), duration) {
        (Some(project), Some(duration)) => locale.render(
            &UiMessage::new("desktop.notification_project_elapsed")
                .arg("project", project)
                .arg("duration", duration),
        ),
        (None, Some(duration)) => {
            locale.render(&UiMessage::new("desktop.notification_elapsed").arg("duration", duration))
        }
        (Some(project), None) => project,
        (None, None) => locale.text("desktop.notification_body").into(),
    }
}

fn completion_duration(locale: LocaleContext, elapsed_ms: u64) -> String {
    let seconds = elapsed_ms / 1000;
    match seconds {
        0 => locale.text("desktop.notification_under_second").into(),
        1..=59 => locale.render(
            &UiMessage::new("desktop.notification_seconds").arg("seconds", seconds.to_string()),
        ),
        _ => locale.render(
            &UiMessage::new("round.duration")
                .arg("minutes", (seconds / 60).to_string())
                .arg("seconds", (seconds % 60).to_string()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clat_core::client_ports::presentation::Locale;
    use serde_json::json;

    #[test]
    fn completion_banner_keeps_outcome_and_hides_parent_path() {
        let notice = serde_json::from_value(json!({
            "id": "fixture", "kind": "job_finished", "workspace_id": "workspace",
            "project_root": "private-parent/fixture-project", "session_id": "session",
            "completion": {"outcome": "stopped", "elapsed_ms": 75000},
        }))
        .unwrap();
        for language in [Locale::English, Locale::Simplified, Locale::Traditional] {
            let locale = LocaleContext::new(language);
            let (title, body) = notification_content(locale, &notice).unwrap();
            assert_eq!(title, locale.text("desktop.notification_job_stopped"));
            assert!(body.contains("fixture-project"));
            assert!(!body.contains("private-parent"));
            assert!(body.contains(&completion_duration(locale, 75000)));
        }
    }

    #[test]
    fn completion_banner_accepts_old_and_future_hosts() {
        let locale = LocaleContext::new(Locale::English);
        let legacy = json!({
            "id": "fixture", "kind": "completed", "workspace_id": "workspace",
            "project_root": "private-parent/fixture-project", "session_id": "session",
        });
        let mut future = legacy.clone();
        future["completion"] = json!({"outcome": "future_outcome"});
        for event in [legacy, future] {
            let notice = serde_json::from_value(event).unwrap();
            assert_eq!(
                notification_content(locale, &notice),
                Some((
                    locale.text("desktop.notification_completed"),
                    "fixture-project".into(),
                ))
            );
        }
    }

    #[test]
    fn foreground_check_requires_visible_unminimized_focused_window() {
        assert!(is_window_in_foreground(true, false, true));
        assert!(!is_window_in_foreground(true, false, false));
        assert!(!is_window_in_foreground(false, false, true));
        assert!(!is_window_in_foreground(true, true, true));
        assert!(!is_window_in_foreground(false, true, false));
    }

    #[test]
    fn sound_mode_resolves_env_and_preference_precedence() {
        assert_eq!(
            sound_mode_from_sources(None, None, "Glass"),
            DesktopSoundMode::Sound("Glass".into())
        );
        assert_eq!(
            sound_mode_from_sources(None, None, "off"),
            DesktopSoundMode::Off
        );
        assert_eq!(
            sound_mode_from_sources(Some("0".into()), None, "Ping"),
            DesktopSoundMode::Sound("Ping".into())
        );
        for flag in ["1", "true", "yes", "on", " 1 "] {
            assert_eq!(
                sound_mode_from_sources(Some(flag.into()), Some("afplay x".into()), "Glass"),
                DesktopSoundMode::Off,
                "CLAT_NO_BELL={flag:?} must silence custom commands and preference"
            );
        }
        assert_eq!(
            sound_mode_from_sources(None, Some("   ".into()), "Hero"),
            DesktopSoundMode::Sound("Hero".into())
        );
        assert_eq!(
            sound_mode_from_sources(None, Some("afplay ~/ding.aiff".into()), "Glass"),
            DesktopSoundMode::Command("afplay ~/ding.aiff".into())
        );
    }

    #[test]
    fn delivery_plan_attaches_selected_sound_and_avoids_double_audio() {
        let selected_plan = delivery_plan(&DesktopSoundMode::Sound("Glass".into()));
        assert_eq!(
            selected_plan.banner_audio,
            BannerAudio::Sound("Glass".into())
        );
        assert!(selected_plan.run_custom_command.is_none());
        assert_eq!(selected_plan.fallback_sound.as_deref(), Some("Glass"));

        let off_plan = delivery_plan(&DesktopSoundMode::Off);
        assert_eq!(off_plan.banner_audio, BannerAudio::Silent);
        assert!(off_plan.run_custom_command.is_none());
        assert!(off_plan.fallback_sound.is_none());

        let cmd_plan = delivery_plan(&DesktopSoundMode::Command("echo ding".into()));
        assert_eq!(cmd_plan.banner_audio, BannerAudio::Silent);
        assert_eq!(cmd_plan.run_custom_command.as_deref(), Some("echo ding"));
        assert!(cmd_plan.fallback_sound.is_none());
    }
}
