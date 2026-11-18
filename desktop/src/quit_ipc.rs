//! Private macOS channel from the menu companion to an already running desktop.
use clat_core::client_ports::presentation::{UiMessage, load_ui_preferences, save_ui_preferences};
use std::{
    fs::{self, DirBuilder},
    io::{self, Read, Write},
    os::unix::{
        fs::DirBuilderExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::Manager;

const PREFIX: &str = "clatq-";
const SOCKET: &str = "quit.sock";
const REQUEST: u8 = 1;
const ACCEPTED: u8 = 1;
const DECLINED: u8 = 2;
const TIMEOUT: Duration = Duration::from_secs(2);

/// The socket belongs to one desktop lifetime, including across same-build restarts.
struct Endpoint(PathBuf);

impl Endpoint {
    fn bind() -> io::Result<(Self, UnixListener)> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?;
        let name = format!("{PREFIX}{}-{}", std::process::id(), stamp.as_nanos());
        // A short absolute address fits macOS sockaddr_un even with a long home path.
        let directory = Path::new("/private/tmp").join(name);
        DirBuilder::new().mode(0o700).create(&directory)?;
        let endpoint = Self(directory);
        let listener = UnixListener::bind(endpoint.0.join(SOCKET))?;
        Ok((endpoint, listener))
    }

    fn remove(&self) {
        let _ = fs::remove_file(self.0.join(SOCKET));
        let _ = fs::remove_dir(&self.0);
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.remove();
    }
}

fn record(preferences: &Path) -> PathBuf {
    preferences.with_file_name("desktop-quit.json")
}

fn publish(preferences: &Path, endpoint: &Endpoint) -> Result<(), String> {
    let name = endpoint
        .0
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("invalid quit endpoint name")?;
    save_ui_preferences(&record(preferences), &serde_json::json!({"endpoint":name}))
}

pub(super) fn start(app: &tauri::AppHandle) -> Result<(), String> {
    let preferences = crate::localization::path(app)?;
    let (endpoint, listener) = Endpoint::bind().map_err(|error| error.to_string())?;
    publish(&preferences, &endpoint)?;
    app.manage(endpoint);
    let app = app.clone();
    std::thread::spawn(move || {
        match receive(listener, || crate::quit::prepare_exit(&app).is_ok()) {
            Ok(()) => app.exit(0),
            Err(error) => eprintln!("[CLAT] local desktop quit channel: {error}"),
        }
    });
    Ok(())
}

pub(super) fn close(app: &tauri::AppHandle) {
    if let Some(endpoint) = app.try_state::<Endpoint>() {
        endpoint.remove();
    }
}

fn receive(listener: UnixListener, accept: impl Fn() -> bool) -> io::Result<()> {
    loop {
        let (mut stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let mut request = [0];
        if stream.set_read_timeout(Some(TIMEOUT)).is_err()
            || stream.set_write_timeout(Some(TIMEOUT)).is_err()
            || stream.read_exact(&mut request).is_err()
            || request != [REQUEST]
        {
            continue;
        }
        let accepted = accept();
        let _ = stream.write_all(&[if accepted { ACCEPTED } else { DECLINED }]);
        if accepted {
            return Ok(());
        }
    }
}

pub(super) struct Target(PathBuf);

impl Target {
    pub(super) fn discover(preferences: &Path) -> Result<Option<Self>, String> {
        let value = load_ui_preferences(&record(preferences))?;
        let Some(name) = value["endpoint"].as_str() else {
            return Ok(None);
        };
        let valid = name
            .strip_prefix(PREFIX)
            .and_then(|name| name.split_once('-'))
            .is_some_and(|(pid, stamp)| {
                pid.parse::<u32>().is_ok() && stamp.parse::<u128>().is_ok()
            });
        if !valid {
            return Err(io::ErrorKind::InvalidData.to_string());
        }
        Ok(Some(Self(
            Path::new("/private/tmp").join(name).join(SOCKET),
        )))
    }

    pub(super) fn quit(self) -> Result<(), UiMessage> {
        match self.request().map_err(quit_error)? {
            true => Ok(()),
            false => Err(UiMessage::new("update.cannot_cancel")),
        }
    }

    fn request(self) -> io::Result<bool> {
        let mut stream = match UnixStream::connect(self.0) {
            Ok(stream) => stream,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Ok(true);
            }
            Err(error) => return Err(error),
        };
        stream.set_write_timeout(Some(TIMEOUT))?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.write_all(&[REQUEST])?;
        let mut response = [0];
        stream.read_exact(&mut response)?;
        match response {
            [ACCEPTED] => Ok(true),
            [DECLINED] => Ok(false),
            _ => Err(io::ErrorKind::InvalidData.into()),
        }
    }
}

fn quit_error(error: io::Error) -> UiMessage {
    UiMessage::new("desktop.tray_stop_failed").arg("reason", error.to_string())
}

#[cfg(test)]
mod tests;
