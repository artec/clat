//! Read-only discovery of runtime installations kept outside the Host's PATH.
use super::unix::usable_dir;
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

const VERSION_LIMIT: usize = 64;

#[derive(Default)]
pub(super) struct Inventory {
    pub active: Vec<PathBuf>,
    pools: Vec<Pool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Node,
    Python,
}

struct Pool {
    family: Family,
    directory: PathBuf,
    suffix: &'static str,
    default_file: Option<PathBuf>,
}

pub(super) enum Lookup {
    Missing,
    Found(PathBuf),
    Ambiguous,
}

impl Inventory {
    pub(super) fn host() -> Self {
        let mut result = Self {
            active: active_dirs(),
            ..Self::default()
        };
        if let Some(home) = std::env::home_dir().filter(|home| home.is_absolute()) {
            result.pools = version_pools(&home);
        }
        result
    }

    pub(super) fn find(&self, name: &str, project: &Path) -> Lookup {
        let Some(family) = family(name) else {
            return Lookup::Missing;
        };
        let mut found = BTreeSet::new();
        let mut preferred = BTreeSet::new();
        let mut complete = true;
        for pool in self.pools.iter().filter(|pool| pool.family == family) {
            complete &= pool.collect(name, project, &mut found, &mut preferred);
        }
        if preferred.len() == 1 && complete {
            return Lookup::Found(preferred.into_iter().next().unwrap());
        }
        match found.len() {
            0 => Lookup::Missing,
            1 if complete => Lookup::Found(found.into_iter().next().unwrap()),
            _ => Lookup::Ambiguous,
        }
    }
}

impl Pool {
    fn collect(
        &self,
        name: &str,
        project: &Path,
        found: &mut BTreeSet<PathBuf>,
        preferred: &mut BTreeSet<PathBuf>,
    ) -> bool {
        let Ok(root) = self.directory.canonicalize() else {
            return true;
        };
        if root.starts_with(project) {
            return true;
        }
        let Ok(versions) = fs::read_dir(root) else {
            return true;
        };
        let default = self
            .default_file
            .as_ref()
            .and_then(|path| path.canonicalize().ok())
            .filter(|path| !path.starts_with(project))
            .as_deref()
            .and_then(read_default);
        for (index, entry) in versions.enumerate() {
            if index == VERSION_LIMIT {
                return false;
            }
            let Ok(entry) = entry else { continue };
            let Some(dir) = usable_dir(&entry.path().join(self.suffix), name, project) else {
                continue;
            };
            if default
                .as_deref()
                .is_some_and(|tag| version_matches(&entry.file_name().to_string_lossy(), tag))
            {
                preferred.insert(dir.clone());
            }
            found.insert(dir);
        }
        true
    }
}

fn family(name: &str) -> Option<Family> {
    match name {
        "node" | "npm" | "npx" => Some(Family::Node),
        "python" | "python3" => Some(Family::Python),
        name if name.starts_with("python3.") => Some(Family::Python),
        _ => None,
    }
}

fn active_dirs() -> Vec<PathBuf> {
    [
        ("NVM_BIN", ""),
        ("FNM_MULTISHELL_PATH", "bin"),
        ("VIRTUAL_ENV", "bin"),
        ("CONDA_PREFIX", "bin"),
    ]
    .into_iter()
    .filter_map(|(variable, suffix)| absolute_var(variable).map(|dir| dir.join(suffix)))
    .collect()
}

fn version_pools(home: &Path) -> Vec<Pool> {
    let data = rooted("XDG_DATA_HOME", home.join(".local/share"));
    let config = rooted("XDG_CONFIG_HOME", home.join(".config"));
    let nvm = rooted("NVM_DIR", home.join(".nvm"));
    let fnm = rooted("FNM_DIR", data.join("fnm"));
    let pyenv = rooted("PYENV_ROOT", home.join(".pyenv"));
    let uv = rooted("UV_PYTHON_INSTALL_DIR", data.join("uv/python"));
    let mise = rooted(
        "MISE_INSTALLS_DIR",
        rooted("MISE_DATA_DIR", data.join("mise")).join("installs"),
    );
    let asdf = rooted("ASDF_DATA_DIR", home.join(".asdf")).join("installs");
    let mut pools = vec![
        (
            Family::Node,
            nvm.join("versions/node"),
            "bin",
            Some(nvm.join("alias/default")),
        ),
        (
            Family::Node,
            fnm.join("node-versions"),
            "installation/bin",
            None,
        ),
        (
            Family::Python,
            pyenv.join("versions"),
            "bin",
            Some(pyenv.join("version")),
        ),
        (Family::Python, uv, "bin", None),
        (Family::Node, mise.join("node"), "bin", None),
        (Family::Python, mise.join("python"), "bin", None),
        (Family::Node, asdf.join("nodejs"), "bin", None),
        (Family::Python, asdf.join("python"), "bin", None),
        (
            Family::Node,
            config.join("nvm/versions/node"),
            "bin",
            Some(config.join("nvm/alias/default")),
        ),
    ];
    if cfg!(target_os = "macos") {
        pools.push((
            Family::Node,
            home.join("Library/Application Support/fnm/node-versions"),
            "installation/bin",
            None,
        ));
    }
    pools
        .into_iter()
        .map(|(family, directory, suffix, default_file)| Pool {
            family,
            directory,
            suffix,
            default_file,
        })
        .collect()
}

fn rooted(variable: &str, fallback: PathBuf) -> PathBuf {
    absolute_var(variable).unwrap_or(fallback)
}

fn absolute_var(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn read_default(path: &Path) -> Option<String> {
    if !fs::metadata(path).is_ok_and(|info| info.is_file()) {
        return None;
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let mut text = String::new();
    file.take(256).read_to_string(&mut text).ok()?;
    let mut values = text.split_whitespace();
    let value = values.next()?;
    (values.next().is_none() && value.len() < 128).then(|| value.to_owned())
}

fn version_matches(installed: &str, requested: &str) -> bool {
    let installed = installed.trim_start_matches('v');
    let requested = requested.trim_start_matches('v');
    installed == requested
        || (requested
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            && installed
                .strip_prefix(requested)
                .is_some_and(|rest| rest.starts_with('.')))
}

#[cfg(test)]
mod tests {
    use super::super::LaunchPath;
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn add_runtime(bin: &Path, name: &str) -> PathBuf {
        fs::create_dir_all(bin).unwrap();
        let exe = bin.join(name);
        fs::write(&exe, b"runtime fixture").unwrap();
        fs::set_permissions(exe, fs::Permissions::from_mode(0o700)).unwrap();
        bin.canonicalize().unwrap()
    }

    #[test]
    fn acp_launch_path_uses_existing_managed_interpreters_and_reports_multiple_candidates() {
        let (storage, project) = crate::test_support::roots("acp-installed-runtime");
        fs::create_dir_all(&storage).unwrap();
        fs::create_dir_all(&project).unwrap();
        let project = project.canonicalize().unwrap();
        let root = storage.parent().unwrap();
        let node_pool = root.join("nvm/versions/node");
        let default_node = root.join("nvm/default");
        let selected = add_runtime(&node_pool.join("v22.1.0/bin"), "node");
        add_runtime(&node_pool.join("v24.2.0/bin"), "node");
        let uv_pool = root.join("uv/python");
        let python = add_runtime(&uv_pool.join("cpython-3.13.0/bin"), "python3");
        let project_python = add_runtime(&project.join("bin"), "python3");
        let shadow = uv_pool.join("shadow");
        fs::create_dir(&shadow).unwrap();
        std::os::unix::fs::symlink(project_python, shadow.join("bin")).unwrap();

        let script = storage.join("agent");
        let mut launch = LaunchPath::fixture(Some("/fixture-system".into()), vec![]);
        launch.inventory.pools = vec![
            Pool {
                family: Family::Node,
                directory: node_pool,
                suffix: "bin",
                default_file: Some(default_node.clone()),
            },
            Pool {
                family: Family::Python,
                directory: uv_pool,
                suffix: "bin",
                default_file: None,
            },
        ];
        fs::write(&script, b"#!/usr/bin/env node\n").unwrap();
        let error = launch.for_program(&script, &project).unwrap_err();
        assert_eq!(error.message().key, "acp.runtime_ambiguous");
        fs::write(default_node, "22\n").unwrap();
        let path = launch.for_program(&script, &project).unwrap().unwrap();
        assert_eq!(std::env::split_paths(&path).last(), Some(selected));

        fs::write(&script, b"#!/usr/bin/env python3\n").unwrap();
        let path = launch.for_program(&script, &project).unwrap().unwrap();
        assert_eq!(std::env::split_paths(&path).last(), Some(python));
        crate::test_support::cleanup_tree(root);
    }
}
