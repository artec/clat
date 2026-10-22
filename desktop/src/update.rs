use crate::app::Desktop;
use std::time::Duration;
use tauri::Manager;
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::watch;

pub struct Updates {
    pub auto_check: bool,
    pub status: String,
    pub busy: bool,
    pub pending: Option<Update>,
    pub bytes: Option<Vec<u8>>,
    cancel: Option<watch::Sender<bool>>,
}
impl Updates {
    pub fn new(auto_check: bool) -> Self {
        Self {
            auto_check,
            status: String::new(),
            busy: false,
            pending: None,
            bytes: None,
            cancel: None,
        }
    }
}

fn begin(app: &tauri::AppHandle, message: &str) -> Result<watch::Receiver<bool>, String> {
    let state = app.state::<Desktop>();
    let mut updates = state
        .updates
        .lock()
        .map_err(|_| "update state unavailable")?;
    if updates.busy {
        return Err("已有更新操作进行中".into());
    }
    let (sender, receiver) = watch::channel(false);
    updates.cancel = Some(sender);
    updates.busy = true;
    updates.status = message.into();
    Ok(receiver)
}

fn finish(app: &tauri::AppHandle, result: &Result<(), String>) {
    let state = app.state::<Desktop>();
    let mut updates = state.updates.lock().expect("updates");
    updates.busy = false;
    updates.cancel = None;
    if let Err(error) = result {
        updates.status = error.clone();
    }
}

pub fn cancel(app: &tauri::AppHandle) {
    if let Ok(updates) = app.state::<Desktop>().updates.lock()
        && let Some(cancel) = &updates.cancel
    {
        let _ = cancel.send(true);
    }
}

fn target() -> String {
    format!(
        "{}-{}",
        if cfg!(target_os = "macos") {
            "darwin"
        } else {
            "windows"
        },
        std::env::consts::ARCH
    )
}

fn fetch_small(url: &str, cap: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .call()
        .map_err(|_| "无法获取更新索引，请检查网络后重试")?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取更新索引")?;
    if bytes.len() as u64 > cap {
        return Err("更新索引过大".into());
    }
    Ok(bytes)
}

fn verify_metadata(update: &Update) -> Result<(), String> {
    let version =
        crate::policy::validate_update(&update.raw_json, env!("CARGO_PKG_VERSION"), &target())?;
    let base = format!(
        "{}/releases/download/v{version}/desktop-latest.json",
        crate::policy::REPO
    );
    let bytes = fetch_small(&base, 1024 * 1024)?;
    let signature = fetch_small(&format!("{base}.sig"), 16384)?;
    crate::policy::verify_index(
        &bytes,
        std::str::from_utf8(&signature).map_err(|_| "invalid index signature")?,
        &update.raw_json,
    )
}

pub async fn check(app: &tauri::AppHandle) -> Result<(), String> {
    let mut cancelled = begin(app, "正在检查桌面更新…")?;
    let operation = async {
        let updater = app
            .updater_builder()
            .pubkey(crate::policy::PUBLIC_KEY.trim())
            .endpoints(vec![
                crate::policy::ENDPOINT
                    .parse()
                    .map_err(|_| "invalid update endpoint")?,
            ])
            .map_err(|e| e.to_string())?
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        let pending = updater.check().await.map_err(|e| e.to_string())?;
        if let Some(update) = &pending {
            let checked = update.clone();
            tauri::async_runtime::spawn_blocking(move || verify_metadata(&checked))
                .await
                .map_err(|_| "index verification worker failed")??;
        }
        let state = app.state::<Desktop>();
        let mut updates = state.updates.lock().expect("updates");
        updates.status = pending
            .as_ref()
            .map(|u| format!("桌面 {} 可用，点击下载更新", u.version))
            .unwrap_or_else(|| "桌面已是最新版本".into());
        updates.pending = pending;
        updates.bytes = None;
        Ok(())
    };
    let result = tokio::select! { result = operation => result, _ = cancelled.changed() => Err("已取消检查".into()) };
    finish(app, &result);
    result
}

pub async fn download(app: &tauri::AppHandle) -> Result<(), String> {
    let mut cancelled = begin(app, "正在下载桌面更新…")?;
    let operation = async {
        let update = app
            .state::<Desktop>()
            .updates
            .lock()
            .expect("updates")
            .pending
            .clone()
            .ok_or("请先检查更新")?;
        let mut received = 0u64;
        let bytes = update
            .download(
                |chunk, total| {
                    received += chunk as u64;
                    let state = app.state::<Desktop>();
                    let mut updates = state.updates.lock().expect("updates");
                    if (received > 512 * 1024 * 1024
                        || total.is_some_and(|n| n > 512 * 1024 * 1024))
                        && let Some(cancel) = &updates.cancel
                    {
                        let _ = cancel.send(true);
                    }
                    updates.status = format!("已下载 {:.1} MB", received as f64 / 1048576.0);
                },
                || {},
            )
            .await
            .map_err(|e| e.to_string())?;
        let state = app.state::<Desktop>();
        let mut updates = state.updates.lock().expect("updates");
        if bytes.len() > 512 * 1024 * 1024 {
            return Err("更新文件超出大小限制".into());
        }
        updates.bytes = Some(bytes);
        updates.status = "下载及验签完成，点击安装更新".into();
        Ok(())
    };
    let result = tokio::select! { result = operation => result, _ = cancelled.changed() => Err("已取消下载，或文件超出大小限制".into()) };
    finish(app, &result);
    result
}

pub async fn install(app: &tauri::AppHandle) -> Result<(), String> {
    let mut cancelled = begin(app, "等待所有项目空闲后安装；当前任务会继续…")?;
    let operation = async {
        {
            let state = app.state::<Desktop>();
            let updates = state.updates.lock().expect("updates");
            if updates.pending.is_none() || updates.bytes.is_none() {
                return Err("请先下载并验证更新包".into());
            }
        }
        loop {
            let status = tauri::async_runtime::spawn_blocking(
                clat_core::client_ports::host::HostClient::local_runtime_status,
            )
            .await
            .map_err(|_| "host status worker failed")??;
            match status {
                None => break,
                Some(value) if value["ready"] == true => break,
                Some(value) => {
                    app.state::<Desktop>()
                        .updates
                        .lock()
                        .expect("updates")
                        .status = format!("等待项目空闲：{}", value["busy_workspaces"])
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let (update, bytes) = {
            let state = app.state::<Desktop>();
            let mut updates = state.updates.lock().expect("updates");
            (
                updates.pending.clone().ok_or("请先检查更新")?,
                updates.bytes.take().ok_or("请先下载更新")?,
            )
        };
        app.state::<Desktop>()
            .quitting
            .store(true, std::sync::atomic::Ordering::Release);
        let result = update.install(&bytes).map_err(|e| e.to_string());
        if result.is_ok() {
            app.restart();
        }
        app.state::<Desktop>()
            .quitting
            .store(false, std::sync::atomic::Ordering::Release);
        result
    };
    let result = tokio::select! { result = operation => result, _ = cancelled.changed() => Err("已取消等待安装，原版本继续运行".into()) };
    finish(app, &result);
    result
}

pub fn start_automatic_checks(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            if app
                .state::<Desktop>()
                .quitting
                .load(std::sync::atomic::Ordering::Acquire)
            {
                break;
            }
            let should_check = {
                let state = app.state::<Desktop>();
                let updates = state.updates.lock().expect("updates");
                updates.auto_check && !updates.busy && updates.bytes.is_none()
            };
            if should_check {
                let _ = check(&app).await;
            }
            tokio::time::sleep(Duration::from_secs(24 * 60 * 60)).await;
        }
    });
}
