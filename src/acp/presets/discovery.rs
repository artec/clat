use super::{Preset, UiMessage, state};
use crate::acp::launch_path::LaunchPath;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, Metadata},
    path::{Path, PathBuf},
};

pub(super) struct Candidate {
    pub path: PathBuf,
    pub token: String,
}

pub(super) enum Detection {
    Ready(Candidate),
    MissingRuntime(Candidate, UiMessage),
    MissingAdapter,
    Incompatible,
    Missing,
}

pub(super) struct Search<'a> {
    pub dirs: Vec<PathBuf>,
    pub project: &'a Path,
    pub windows: bool,
    pub launch: LaunchPath,
}

impl<'a> Search<'a> {
    pub fn host(project: &'a Path) -> Self {
        let launch = LaunchPath::host();
        let mut dirs = launch
            .path
            .iter()
            .flat_map(std::env::split_paths)
            .filter(|path| path.is_absolute())
            .take(128)
            .collect::<Vec<_>>();
        dirs.extend(launch.fallbacks.iter().cloned());
        Self {
            dirs,
            project,
            windows: cfg!(windows),
            launch,
        }
    }

    pub fn detect(&self, preset: &Preset) -> Detection {
        let detection = self.locate(preset.command);
        let detection = match detection {
            Detection::Ready(candidate) if !self.windows => {
                match self.launch.for_program(&candidate.path, self.project) {
                    Ok(_) => Detection::Ready(candidate),
                    Err(error) => Detection::MissingRuntime(candidate, error),
                }
            }
            detection => detection,
        };
        if matches!(detection, Detection::Missing)
            && preset
                .base_command
                .is_some_and(|name| !matches!(self.locate(name), Detection::Missing))
        {
            return Detection::MissingAdapter;
        }
        detection
    }

    pub fn exact(&self, preset: &Preset, token: &str) -> Result<Candidate, UiMessage> {
        if token.len() == 64 {
            match self.detect(preset) {
                Detection::Ready(candidate) if candidate.token == token => return Ok(candidate),
                Detection::MissingRuntime(candidate, error) if candidate.token == token => {
                    return Err(error);
                }
                _ => {}
            }
        }
        Err(UiMessage::new("acp.preset_changed"))
    }

    fn locate(&self, name: &str) -> Detection {
        let mut incompatible = false;
        for dir in &self.dirs {
            if !dir.is_absolute() || dir.starts_with(self.project) {
                continue;
            }
            let direct = if self.windows {
                dir.join(format!("{name}.exe"))
            } else {
                dir.join(name)
            };
            if let Some((path, metadata)) = self.file(&direct) {
                if is_direct(&path, &metadata, self.windows) {
                    let token = identity(&path, &metadata);
                    return Detection::Ready(Candidate { path, token });
                }
                incompatible = true;
            }
            if self.windows {
                incompatible |= ["cmd", "bat", "ps1"]
                    .into_iter()
                    .any(|suffix| self.file(&dir.join(format!("{name}.{suffix}"))).is_some());
            }
        }
        if incompatible {
            Detection::Incompatible
        } else {
            Detection::Missing
        }
    }

    fn file(&self, path: &Path) -> Option<(PathBuf, Metadata)> {
        let path = path.canonicalize().ok()?;
        if path.starts_with(self.project) || path.to_str().is_none() {
            return None;
        }
        let metadata = fs::metadata(&path).ok()?;
        metadata.is_file().then_some((path, metadata))
    }
}

fn is_direct(path: &Path, metadata: &Metadata, windows: bool) -> bool {
    if metadata.len() > state::MAX_EXECUTABLE_BYTES {
        return false;
    }
    if windows {
        return path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
    }
    if path.extension().is_some_and(|ext| {
        ["cmd", "bat", "ps1"]
            .into_iter()
            .any(|script| ext.eq_ignore_ascii_case(script))
    }) {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    false
}

fn identity(path: &Path, metadata: &Metadata) -> String {
    let mut hash = Sha256::new();
    hash.update(path.as_os_str().as_encoded_bytes());
    hash.update(metadata.len().to_le_bytes());
    hash.update(format!("{:?}", metadata.modified()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        for value in [
            metadata.dev(),
            metadata.ino(),
            metadata.ctime() as u64,
            metadata.ctime_nsec() as u64,
        ] {
            hash.update(value.to_le_bytes());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        hash.update(metadata.creation_time().to_le_bytes());
        hash.update(metadata.last_write_time().to_le_bytes());
    }
    format!("{:x}", hash.finalize())
}
