use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::i18n::UiMessage;

#[cfg(unix)]
mod installed;

pub(super) struct LaunchPath {
    pub path: Option<OsString>,
    pub fallbacks: Vec<PathBuf>,
    #[cfg(unix)]
    inventory: installed::Inventory,
}

impl LaunchPath {
    pub(super) fn host() -> Self {
        Self {
            path: std::env::var_os("PATH"),
            fallbacks: common_dirs(),
            #[cfg(unix)]
            inventory: installed::Inventory::host(),
        }
    }

    #[cfg(test)]
    pub(super) fn fixture(path: Option<OsString>, fallbacks: Vec<PathBuf>) -> Self {
        Self {
            path,
            fallbacks,
            #[cfg(unix)]
            inventory: Default::default(),
        }
    }

    pub(super) fn for_program(
        &self,
        program: &Path,
        project: &Path,
    ) -> Result<Option<OsString>, UiMessage> {
        #[cfg(unix)]
        if let Some(interpreter) = unix::interpreter(program) {
            return unix::with_interpreter(self, &interpreter, project);
        }
        #[cfg(not(unix))]
        let _ = (program, project);
        Ok(self.path.clone())
    }
}

#[cfg(unix)]
fn missing_runtime(interpreter: &str) -> UiMessage {
    UiMessage::new("acp.runtime_unavailable").arg("runtime", interpreter)
}

fn common_dirs() -> Vec<PathBuf> {
    let home = std::env::home_dir().filter(|path| path.is_absolute());
    if cfg!(windows) {
        let mut dirs = Vec::new();
        for (variable, suffix) in [
            ("LOCALAPPDATA", "Microsoft/WinGet/Links"),
            ("LOCALAPPDATA", "agy/bin"),
            ("LOCALAPPDATA", "qodercli"),
            ("APPDATA", "npm"),
        ] {
            dirs.extend(
                std::env::var_os(variable)
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .map(|path| path.join(suffix)),
            );
        }
        dirs.extend(home.into_iter().flat_map(|path| {
            ["scoop/shims", ".local/bin", ".bun/bin", ".grok/bin"].map(|suffix| path.join(suffix))
        }));
        return dirs;
    }
    let mut dirs = home
        .into_iter()
        .flat_map(|path| {
            [
                ".local/bin",
                ".bun/bin",
                ".opencode/bin",
                ".grok/bin",
                ".npm-global/bin",
                ".volta/bin",
                ".local/share/pnpm",
            ]
            .map(|suffix| path.join(suffix))
        })
        .collect::<Vec<_>>();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    dirs
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::{
        fs,
        io::Read,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };

    pub(super) fn interpreter(program: &Path) -> Option<String> {
        // Read only the shebang of a regular file. Detection doesn't run it.
        if !fs::metadata(program).is_ok_and(|info| info.is_file()) {
            return None;
        }
        let mut prefix = [0; 512];
        let size = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(program)
            .and_then(|mut file| file.read(&mut prefix))
            .ok()?;
        if size == prefix.len() && !prefix.contains(&b'\n') {
            return None;
        }
        let line = prefix[..size].split(|&byte| byte == b'\n').next()?;
        let shebang = std::str::from_utf8(line.strip_prefix(b"#!")?).ok()?;
        let mut words = shebang.split_ascii_whitespace();
        match words.next()? {
            "/usr/bin/env" => {
                let program = match words.next()? {
                    "-S" => words.next()?,
                    command if words.next().is_none() => command,
                    _ => return None,
                };
                program
                    .as_bytes()
                    .first()
                    .filter(|c| c.is_ascii_alphanumeric())?;
                program
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.+".contains(&c))
                    .then(|| program.to_owned())
            }
            absolute
                if Path::new(absolute).is_absolute() && !absolute.chars().any(char::is_control) =>
            {
                Some(absolute.to_owned())
            }
            _ => None,
        }
    }

    pub(super) fn with_interpreter(
        launch: &LaunchPath,
        interpreter: &str,
        project: &Path,
    ) -> Result<Option<OsString>, UiMessage> {
        if Path::new(interpreter).is_absolute() {
            return executable(Path::new(interpreter))
                .then(|| launch.path.clone())
                .ok_or_else(|| missing_runtime(interpreter));
        }
        let mut dirs = launch.path.as_ref().map_or_else(
            || vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
            |path| std::env::split_paths(path).collect(),
        );
        if dirs
            .iter()
            .any(|dir| executable(&project.join(dir).join(interpreter)))
        {
            return Ok(launch.path.clone());
        }
        let common = launch
            .inventory
            .active
            .iter()
            .chain(&launch.fallbacks)
            .find_map(|dir| usable_dir(dir, interpreter, project));
        let dir = if let Some(dir) = common {
            dir
        } else {
            match launch.inventory.find(interpreter, project) {
                installed::Lookup::Found(dir) => dir,
                installed::Lookup::Missing => return Err(missing_runtime(interpreter)),
                installed::Lookup::Ambiguous => {
                    return Err(UiMessage::new("acp.runtime_ambiguous").arg("runtime", interpreter));
                }
            }
        };
        dirs.push(dir);
        std::env::join_paths(dirs)
            .map(Some)
            .map_err(|_| missing_runtime(interpreter))
    }

    pub(super) fn usable_dir(dir: &Path, interpreter: &str, project: &Path) -> Option<PathBuf> {
        if !dir.is_absolute() || dir.starts_with(project) {
            return None;
        }
        let dir = dir.canonicalize().ok()?;
        let file = dir.join(interpreter).canonicalize().ok()?;
        (!dir.starts_with(project) && !file.starts_with(project) && executable(&file))
            .then_some(dir)
    }

    fn executable(path: &Path) -> bool {
        fs::metadata(path)
            .is_ok_and(|info| info.is_file() && info.permissions().mode() & 0o111 != 0)
    }

    #[test]
    fn acp_launch_path_works_across_host_environments_without_using_project_fallback() {
        let (storage, project) = crate::test_support::roots("acp-runtime-launch");
        let root = project.parent().unwrap();
        let system = root.join("system");
        let custom = root.join("custom");
        for dir in [&storage, &project, &system, &custom] {
            fs::create_dir_all(dir).unwrap();
        }
        let project = project.canonicalize().unwrap();
        let launcher = custom.join("acp-agent");
        fs::write(&launcher, b"#!/usr/bin/env -S python3 -u\n").unwrap();
        let shadow = root.join("shadow");
        fs::create_dir(&shadow).unwrap();
        for dir in [&project, &custom] {
            let interpreter = dir.join("python3");
            fs::write(&interpreter, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&interpreter, fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::os::unix::fs::symlink(project.join("python3"), shadow.join("python3")).unwrap();
        let mut launch = LaunchPath::fixture(
            Some(std::env::join_paths([&system]).unwrap()),
            vec![project.clone(), shadow],
        );
        let missing = launch.for_program(&launcher, &project).unwrap_err();
        assert_eq!(missing.key, "acp.runtime_unavailable");

        launch.fallbacks.push(custom.clone());
        let path = launch.for_program(&launcher, &project).unwrap().unwrap();
        assert_eq!(
            std::env::split_paths(&path).collect::<Vec<_>>(),
            [system, custom.canonicalize().unwrap()]
        );

        launch.path = Some(std::env::join_paths([&custom]).unwrap());
        launch.fallbacks.clear();
        assert_eq!(
            launch.for_program(&launcher, &project).unwrap(),
            launch.path
        );
        fs::write(&launcher, b"native executable fixture").unwrap();
        launch.path = None;
        assert_eq!(launch.for_program(&launcher, &project).unwrap(), None);
        crate::test_support::cleanup_tree(root);
    }
}
