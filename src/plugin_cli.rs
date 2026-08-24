//! `clat plugin`: frontend-thin local package lifecycle commands.
//!
//! Parsing/rendering live here; transaction, verification and persistent
//! state semantics remain in `plugin::store` so future desktop/IDE clients can
//! call the same core without shelling out.

use crate::control_storage::sentinel;
use crate::plugin::{
    InstallKind, PackageInspection, PackageMutation, PackageStore, PluginRuntimeKind, TrustLabel,
    installed_packages,
};
use serde_json::Value;
use std::io::Read as _;
use std::path::{Path, PathBuf};

const MAX_CONFIG_FILE_BYTES: u64 = 64 * 1024;

pub struct PluginCliOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: u8,
}

impl PluginCliOutcome {
    fn success(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            exit_code: 0,
        }
    }

    fn failure(message: impl Into<String>, usage: bool) -> Self {
        let mut stderr = format!("clat: plugin: {}\n", message.into());
        if usage {
            stderr.push_str("Run `clat plugin --help` for usage.\n");
        }
        Self {
            stdout: String::new(),
            stderr,
            exit_code: if usage { 2 } else { 1 },
        }
    }
}

pub fn run<I>(args: I) -> PluginCliOutcome
where
    I: IntoIterator<Item = String>,
{
    let args: Vec<String> = args.into_iter().collect();
    if args
        .first()
        .is_none_or(|command| matches!(command.as_str(), "inspect" | "-h" | "--help" | "help"))
    {
        return run_at(Path::new("."), args);
    }
    let root = match sentinel::default_storage_root() {
        Ok(root) => root,
        Err(error) => return PluginCliOutcome::failure(error, false),
    };
    run_at(&root, args)
}

fn run_at<I>(storage_root: &Path, args: I) -> PluginCliOutcome
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        return PluginCliOutcome::success(help());
    };
    if matches!(command.as_str(), "-h" | "--help" | "help") {
        if args.next().is_some() {
            return PluginCliOutcome::failure("help takes no arguments", true);
        }
        return PluginCliOutcome::success(help());
    }
    match command.as_str() {
        "inspect" => {
            let path = match one_argument(args, "inspect requires a package directory") {
                Ok(path) => PathBuf::from(path),
                Err(error) => return PluginCliOutcome::failure(error, true),
            };
            match PackageStore::inspect(&path) {
                Ok(inspection) => PluginCliOutcome::success(render_inspection(&inspection)),
                Err(error) => PluginCliOutcome::failure(error, false),
            }
        }
        "install" | "update" => {
            let parsed = match parse_install_args(args) {
                Ok(parsed) => parsed,
                Err(error) => return PluginCliOutcome::failure(error, true),
            };
            let mut store = match PackageStore::open(storage_root) {
                Ok(store) => store,
                Err(error) => return PluginCliOutcome::failure(error, false),
            };
            let kind = if command == "install" {
                InstallKind::Install
            } else {
                InstallKind::Update
            };
            match store.install(
                &parsed.path,
                parsed.config,
                parsed.accept_capabilities,
                kind,
            ) {
                Ok(mutation) => PluginCliOutcome::success(render_mutation(&mutation)),
                Err(error) => PluginCliOutcome::failure(error, false),
            }
        }
        "list" => {
            if args.next().is_some() {
                return PluginCliOutcome::failure("list takes no arguments", true);
            }
            match installed_packages(storage_root) {
                Ok(entries) if entries.is_empty() => {
                    PluginCliOutcome::success("No installed plugins.\n")
                }
                Ok(entries) => {
                    let mut output = "ID\tNAME\tVERSION\tRUNTIME\tSTATE\tTRUST\tPUBLISHER\tHEALTH\tDIGEST\tROLLBACK\n".to_owned();
                    for entry in entries {
                        let digest = &entry.tree_sha256[..entry.tree_sha256.len().min(12)];
                        output.push_str(&format!(
                            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                            entry.id,
                            entry.name,
                            entry.version,
                            runtime_name(entry.runtime),
                            if entry.enabled { "enabled" } else { "disabled" },
                            trust_name(entry.trust),
                            entry.publisher.unwrap_or_else(|| "publisher=-".into()),
                            entry
                                .health
                                .map(|error| format!("health=error:{error}"))
                                .unwrap_or_else(|| "health=ok".into()),
                            digest,
                            entry
                                .rollback_version
                                .map(|version| format!("rollback={version}"))
                                .unwrap_or_else(|| "rollback=-".into()),
                        ));
                    }
                    PluginCliOutcome::success(output)
                }
                Err(error) => PluginCliOutcome::failure(error, false),
            }
        }
        "enable" | "disable" | "rollback" | "uninstall" => {
            let id = match one_argument(args, &format!("{command} requires a plugin id")) {
                Ok(id) => id,
                Err(error) => return PluginCliOutcome::failure(error, true),
            };
            let mut store = match PackageStore::open(storage_root) {
                Ok(store) => store,
                Err(error) => return PluginCliOutcome::failure(error, false),
            };
            let result = match command.as_str() {
                "enable" => store.set_enabled(&id, true),
                "disable" => store.set_enabled(&id, false),
                "rollback" => store.rollback(&id),
                "uninstall" => store.uninstall(&id),
                _ => unreachable!(),
            };
            match result {
                Ok(mutation) => PluginCliOutcome::success(render_mutation(&mutation)),
                Err(error) => PluginCliOutcome::failure(error, false),
            }
        }
        other => PluginCliOutcome::failure(format!("unknown command `{other}`"), true),
    }
}

struct InstallArgs {
    path: PathBuf,
    config: Option<Value>,
    accept_capabilities: bool,
}

fn parse_install_args<I>(args: I) -> Result<InstallArgs, String>
where
    I: IntoIterator<Item = String>,
{
    let mut path = None;
    let mut config = None;
    let mut accept_capabilities = false;
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--accept-capabilities" => accept_capabilities = true,
            "--config-json" => {
                if config.is_some() {
                    return Err("--config-json may be provided only once".into());
                }
                let raw = args
                    .next()
                    .ok_or_else(|| "--config-json requires a JSON value".to_owned())?;
                config = Some(
                    serde_json::from_str(&raw)
                        .map_err(|error| format!("invalid --config-json: {error}"))?,
                );
            }
            "--config-file" => {
                if config.is_some() {
                    return Err("--config-json and --config-file are mutually exclusive".into());
                }
                let path = PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--config-file requires a path".to_owned())?,
                );
                let raw = read_config_file(&path)?;
                config = Some(
                    serde_json::from_str(&raw)
                        .map_err(|error| format!("invalid --config-file JSON: {error}"))?,
                );
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown option `{value}`"));
            }
            value => {
                if path.is_some() {
                    return Err(format!("unexpected extra argument `{value}`"));
                }
                path = Some(PathBuf::from(value));
            }
        }
    }
    Ok(InstallArgs {
        path: path.ok_or_else(|| "install/update requires a package directory".to_owned())?,
        config,
        accept_capabilities,
    })
}

fn read_config_file(path: &Path) -> Result<String, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect --config-file: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("--config-file must be a regular non-symlink file".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("open --config-file: {error}"))?;
    let opened = file
        .metadata()
        .map_err(|error| format!("inspect opened --config-file: {error}"))?;
    if !opened.is_file() || opened.len() > MAX_CONFIG_FILE_BYTES {
        return Err(format!(
            "--config-file must be a regular file of at most {MAX_CONFIG_FILE_BYTES} bytes"
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read --config-file: {error}"))?;
    if bytes.len() as u64 > MAX_CONFIG_FILE_BYTES {
        return Err(format!(
            "--config-file exceeds the {MAX_CONFIG_FILE_BYTES} byte cap"
        ));
    }
    String::from_utf8(bytes).map_err(|_| "--config-file is not UTF-8".into())
}

fn one_argument<I>(args: I, missing: &str) -> Result<String, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let value = args.next().ok_or_else(|| missing.to_owned())?;
    if let Some(extra) = args.next() {
        return Err(format!("unexpected extra argument `{extra}`"));
    }
    Ok(value)
}

fn render_inspection(inspection: &PackageInspection) -> String {
    let capabilities = if inspection.capabilities.is_empty() {
        "none".into()
    } else {
        inspection.capabilities.join(", ")
    };
    format!(
        "id: {}\nname: {}\nversion: {}\nruntime: {}\nmanifest: {}\n\
         treeSha256: {}\nfiles: {}\nbytes: {}\ntrust: {}\npublisher: {}\ncapabilities: {}\n",
        inspection.manifest.id,
        inspection.manifest.name,
        inspection.manifest.version,
        runtime_name(inspection.manifest.runtime.kind),
        inspection.manifest_path.display(),
        inspection.tree_sha256,
        inspection.files,
        inspection.total_bytes,
        trust_name(inspection.trust),
        inspection
            .publisher
            .as_ref()
            .map(|publisher| publisher.publisher.as_str())
            .unwrap_or("-"),
        capabilities,
    )
}

fn render_mutation(mutation: &PackageMutation) -> String {
    format!(
        "{} {} {} ({}, {}, {}){}\n",
        mutation.id,
        mutation.version,
        mutation.note,
        runtime_name(mutation.runtime),
        if mutation.enabled {
            "enabled"
        } else {
            "disabled"
        },
        &mutation.tree_sha256[..mutation.tree_sha256.len().min(12)],
        mutation
            .rollback_version
            .as_ref()
            .map(|version| format!(", rollback={version}"))
            .unwrap_or_default(),
    )
}

fn runtime_name(runtime: PluginRuntimeKind) -> &'static str {
    match runtime {
        PluginRuntimeKind::WasmComponent => "wasm-component",
        PluginRuntimeKind::McpStdio => "mcp-stdio",
    }
}

fn trust_name(trust: TrustLabel) -> &'static str {
    match trust {
        TrustLabel::LocalUnverified => "local/unverified",
        TrustLabel::PublisherVerified => "publisher/verified",
    }
}

fn help() -> String {
    "Usage: clat plugin <COMMAND>\n\n\
     Commands:\n\
       inspect <package>                         Verify manifest and package tree\n\
       install <package> [OPTIONS]               Install and activate a new package\n\
       update <package> [OPTIONS]                Install and activate an update\n\
       list                                      List installed packages\n\
       enable <id>                               Enable an installed package\n\
       disable <id>                              Disable an installed package\n\
       rollback <id>                             Swap active and rollback versions\n\
       uninstall <id>                            Remove a package and its artifacts\n\n\
     Install/update options:\n\
       --config-json <json>                      Store package-private configuration\n\
       --config-file <path>                      Read package-private JSON from a file\n\
       --accept-capabilities                     Accept first/new capabilities\n"
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest as _, Sha256};

    fn roots() -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "clat-plugin-cli-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let storage = base.join("storage");
        let package = base.join("package");
        std::fs::create_dir_all(&package).expect("package");
        std::fs::write(package.join("plugin.wasm"), b"fixture").expect("entry");
        let digest = Sha256::digest(b"fixture")
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        std::fs::write(
            package.join("clat-plugin.json"),
            serde_json::to_vec(&serde_json::json!({
                "manifestVersion": 1,
                "id": "dev.clat.cli",
                "name": "CLI Fixture",
                "version": "1.0.0",
                "runtime": {
                    "kind": "wasm-component",
                    "entry": "plugin.wasm",
                    "sha256": digest,
                },
                "capabilities": { "tools": true },
            }))
            .expect("manifest"),
        )
        .expect("write manifest");
        (storage, package)
    }

    #[test]
    fn cli_inspect_install_list_disable_enable_and_uninstall() {
        let (storage, package) = roots();
        let inspected = run_at(&storage, ["inspect".into(), package.display().to_string()]);
        assert_eq!(inspected.exit_code, 0);
        assert!(inspected.stdout.contains("dev.clat.cli"));
        let denied = run_at(&storage, ["install".into(), package.display().to_string()]);
        assert_eq!(denied.exit_code, 1);
        assert!(denied.stderr.contains("--accept-capabilities"));
        let installed = run_at(
            &storage,
            [
                "install".into(),
                package.display().to_string(),
                "--accept-capabilities".into(),
            ],
        );
        assert_eq!(installed.exit_code, 0, "{}", installed.stderr);
        let listed = run_at(&storage, ["list".into()]);
        assert!(listed.stdout.contains("enabled"));
        assert_eq!(
            run_at(&storage, ["disable".into(), "dev.clat.cli".into()]).exit_code,
            0
        );
        assert_eq!(
            run_at(&storage, ["enable".into(), "dev.clat.cli".into()]).exit_code,
            0
        );
        assert_eq!(
            run_at(&storage, ["uninstall".into(), "dev.clat.cli".into()]).exit_code,
            0
        );
        assert!(
            run_at(&storage, ["list".into()])
                .stdout
                .contains("No installed")
        );
        std::fs::remove_dir_all(storage.parent().expect("base")).expect("cleanup");
    }

    #[test]
    fn usage_errors_are_exit_two_and_do_not_write() {
        let (storage, package) = roots();
        let outcome = run_at(&storage, ["install".into(), "--wat".into()]);
        assert_eq!(outcome.exit_code, 2);
        assert!(!storage.exists());
        std::fs::remove_dir_all(package.parent().expect("base")).expect("cleanup");
    }
}
