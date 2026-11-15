use super::*;
use crate::{application::ApplicationError, i18n::UiMessage, session::path_layout};
use cap_std::fs::Dir;
use std::{
    io::Read,
    path::{Path, PathBuf},
};

/// Stage the bundle in a fresh DSH home. The caller explicitly chooses this
/// path and can inspect it independently of either user's live session store.
pub(crate) fn unpack_dsh_archive(
    zip: &Path,
    home: &Path,
    project: Option<&Path>,
) -> Result<(), ApplicationError> {
    restore(zip, home, project).map_err(|error| {
        ApplicationError::presented(UiMessage::new("history.archive_failed").arg("detail", error))
    })
}

fn restore(zip: &Path, home: &Path, project: Option<&Path>) -> Result<(), String> {
    let mut bytes = Vec::new();
    std::fs::File::open(zip)
        .and_then(|file| {
            file.take(MAX_ARCHIVE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|e| e.to_string())?;
    let incoming = Incoming::parse(&bytes, || Ok(()))?;
    let cwd = project.map(project_cwd).transpose()?;
    let session_paths = incoming
        .logs
        .iter()
        .map(|log| session_path(log, cwd.as_deref()))
        .collect::<Result<Vec<_>, _>>()?;
    let attachment_paths = incoming
        .objects
        .keys()
        .map(|name| object_path(name))
        .collect::<Result<Vec<_>, _>>()?;
    let parent = home
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = home
        .file_name()
        .ok_or("Invalid DSH destination directory")?;
    let parent =
        Dir::open_ambient_dir(parent, cap_std::ambient_authority()).map_err(|e| e.to_string())?;
    let stage = format!(".clat-dsh-{}", uuid::Uuid::new_v4());
    parent.create_dir(&stage).map_err(|e| e.to_string())?;
    let result = (|| {
        let dir = crate::session::root_dir::SessionRootDir::open_or_create_child(
            &parent,
            Path::new(&stage),
        )
        .map_err(|e| e.to_string())?;
        for ((_, object), path) in incoming.objects.into_iter().zip(attachment_paths) {
            write_file(&dir, &path, &object)?;
        }
        for (log, path) in incoming.logs.into_iter().zip(session_paths) {
            let data = compressed_log(log, cwd.as_deref())?;
            write_file(&dir, &path, &data)?;
        }
        install(&parent, &stage, name, &dir)
    })();
    let _ = parent.remove_dir_all(&stage);
    result
}

fn install(parent: &Dir, stage: &str, name: &std::ffi::OsStr, dir: &Dir) -> Result<(), String> {
    // Reserve the target exclusively. A failure cannot replace a live home.
    parent.create_dir(name).map_err(|e| e.to_string())?;
    let target =
        crate::session::root_dir::SessionRootDir::open_or_create_child(parent, Path::new(name))
            .map_err(|e| e.to_string())?;
    if dir.symlink_metadata("attachments").is_ok() {
        dir.rename("attachments", &target, "attachments")
            .map_err(|e| e.to_string())?;
    }
    dir.rename("sessions", &target, "sessions")
        .map_err(|e| e.to_string())?;
    crate::session::root_dir::sync_dir(&target)
        .and_then(|()| crate::session::root_dir::sync_dir(parent))
        .map_err(|e| format!("Cannot sync staged DSH home {stage}: {e}"))
}

fn project_cwd(path: &Path) -> Result<String, String> {
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    path.is_dir()
        .then(|| path.to_str())
        .flatten()
        .map(str::to_owned)
        .ok_or_else(|| "Project must be an existing UTF-8 directory".into())
}

fn session_path(log: &ImportedLog, cwd: Option<&str>) -> Result<PathBuf, String> {
    let dir = path_layout::project_dir(Path::new("sessions"), cwd.or(log.header.cwd.as_deref()));
    let id = path_layout::encode_segment(log.header.id.as_str());
    portable_component(&id)?;
    Ok(dir.join(id).join("session.v4.jsonl.zstd"))
}

fn object_path(name: &str) -> Result<PathBuf, String> {
    let root = Path::new("attachments").join("v1");
    if let Some(name) = name.strip_prefix("media/sha256:") {
        let (digest, _) = name.rsplit_once('.').ok_or("Invalid raster entry")?;
        return Ok(root.join("objects").join(&digest[..2]).join(digest));
    }
    let leaf = name.rsplit('/').next().unwrap_or("");
    portable_component(leaf)?;
    Ok(root.join(name))
}

fn portable_component(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 255 || name.contains([':', '\\', '/', '\0']) {
        return Err("Archive contains a name unsupported by the destination filesystem".into());
    }
    #[cfg(windows)]
    {
        let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        if name.ends_with(['.', ' '])
            || name.contains(['<', '>', '"', '|', '?', '*'])
            || matches!(
                stem.as_str(),
                "CON"
                    | "PRN"
                    | "AUX"
                    | "NUL"
                    | "COM1"
                    | "COM2"
                    | "COM3"
                    | "COM4"
                    | "COM5"
                    | "COM6"
                    | "COM7"
                    | "COM8"
                    | "COM9"
                    | "LPT1"
                    | "LPT2"
                    | "LPT3"
                    | "LPT4"
                    | "LPT5"
                    | "LPT6"
                    | "LPT7"
                    | "LPT8"
                    | "LPT9"
            )
        {
            return Err("Archive contains a name unsupported by Windows".into());
        }
    }
    Ok(())
}

fn compressed_log(mut log: ImportedLog, cwd: Option<&str>) -> Result<Vec<u8>, String> {
    let end = log
        .bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or("Malformed header")?
        + 1;
    let header = match cwd {
        Some(cwd) => {
            log.header.cwd = Some(cwd.into());
            let mut line = log.header.to_line().into_bytes();
            if !line.ends_with(b"\n") {
                line.push(b'\n');
            }
            line
        }
        None => log.bytes[..end].to_vec(),
    };
    let mut bytes = zstd::stream::encode_all(Cursor::new(header), 3).map_err(|e| e.to_string())?;
    if end < log.bytes.len() {
        bytes.extend(
            zstd::stream::encode_all(Cursor::new(&log.bytes[end..]), 3)
                .map_err(|e| e.to_string())?,
        );
    }
    Ok(bytes)
}

fn write_file(dir: &Dir, path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut target = dir.try_clone().map_err(|e| e.to_string())?;
    for component in path.parent().ok_or("Invalid staged path")?.components() {
        target = crate::session::root_dir::SessionRootDir::open_or_create_child(
            &target,
            Path::new(component.as_os_str()),
        )
        .map_err(|e| e.to_string())?;
    }
    let file = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("Invalid staged filename")?;
    crate::private_fs::write_bytes_new_atomic_in_dir(&target, file, bytes)
}
