//! Artifact dispatch and read-only reports. Each row owns its matcher and handler.
use super::*;

type Action = fn(&RustSupport, &str, bool, &CancelToken, Option<&[u8]>) -> Result<Value, String>;
pub(super) struct Reaction {
    pub(super) artifact: &'static str,
    matches: fn(&Path) -> bool,
    pub(super) run: Action,
    pub(super) failure: &'static str,
}

static REACTIONS: &[Reaction] = &[
    Reaction {
        artifact: "cargo_manifest",
        matches: |path| path.file_name().is_some_and(|name| name == "Cargo.toml"),
        run: manifest,
        failure: "rust_support.failed",
    },
    Reaction {
        artifact: "toolchain",
        matches: |path| {
            path.file_name()
                .is_some_and(|name| name == "rust-toolchain.toml")
        },
        run: toolchain,
        failure: "rust_support.toolchain_failed",
    },
    Reaction {
        artifact: "cargo_config",
        matches: |path| path.ends_with(Path::new(".cargo/config.toml")),
        run: config,
        failure: "rust_support.config_failed",
    },
];

pub(super) fn find(path: &str) -> Option<&'static Reaction> {
    let path = Path::new(path);
    REACTIONS.iter().find(|reaction| (reaction.matches)(path))
}

fn manifest(
    service: &RustSupport,
    path: &str,
    human: bool,
    cancel: &CancelToken,
    _: Option<&[u8]>,
) -> Result<Value, String> {
    let lock = service.synchronize(path, human, cancel)?;
    Ok(json!({"status":"synchronized", "lockfile":lock,
        "presentation":crate::i18n::UiMessage::new("rust_support.synchronized")}))
}

fn read(service: &RustSupport, path: &str, cancel: &CancelToken) -> Result<toml::Value, String> {
    if service.closed.is_cancelled() || cancel.is_cancelled() {
        return Err("Rust report cancelled".into());
    }
    let (bytes, _) = service
        .project
        .file_snapshot(path, 1024 * 1024 + 1)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("Rust artifact exceeds byte limit".into());
    }
    parse(&bytes)
}
fn parse(bytes: &[u8]) -> Result<toml::Value, String> {
    let source = std::str::from_utf8(bytes).map_err(|_| "Rust artifact is not UTF-8")?;
    toml::from_str(source).map_err(|_: toml::de::Error| "Invalid TOML".into())
}

fn toolchain(
    service: &RustSupport,
    path: &str,
    _: bool,
    cancel: &CancelToken,
    _: Option<&[u8]>,
) -> Result<Value, String> {
    let source = read(service, path, cancel)?;
    let channel = source
        .get("toolchain")
        .and_then(|v| v.get("channel"))
        .and_then(toml::Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("Missing toolchain.channel")?;
    if channel.len() > 128
        || !channel
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'.')
    {
        return Err("Invalid toolchain channel".into());
    }
    let state = installed(channel)?;
    Ok(
        json!({"status":"reported", "artifact":"toolchain", "channel":channel,
        "installed":state, "presentation":crate::i18n::UiMessage::new(match state {
            "installed" => "rust_support.toolchain_installed",
            "missing" => "rust_support.toolchain_missing",
            _ => "rust_support.toolchain_unknown",
        }).arg("channel", channel)}),
    )
}
fn installed(channel: &str) -> Result<&'static str, String> {
    let home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|home| PathBuf::from(home).join(".rustup"))
        });
    let Some(home) = home else {
        return Ok("unknown");
    };
    if !home.is_absolute() {
        return Ok("unknown");
    }
    installed_in(&home, channel)
}

fn installed_in(home: &Path, channel: &str) -> Result<&'static str, String> {
    let entries = match std::fs::read_dir(home.join("toolchains")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok("unknown"),
        Err(e) => return Err(e.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if (name == channel
            || name
                .strip_prefix(channel)
                .is_some_and(|s| s.starts_with('-')))
            && entry.file_type().map_err(|e| e.to_string())?.is_dir()
        {
            return Ok("installed");
        }
    }
    Ok("missing")
}

fn config(
    service: &RustSupport,
    path: &str,
    _: bool,
    cancel: &CancelToken,
    previous: Option<&[u8]>,
) -> Result<Value, String> {
    let now = read(service, path, cancel)?;
    let before = previous.map(parse).transpose()?;
    let changes = changed_keys(before.as_ref(), &now);
    Ok(
        json!({"status":"reported", "artifact":"cargo_config", "changed_keys":changes,
        "presentation":crate::i18n::UiMessage::new("rust_support.config_report")
            .arg("count", changes.len())}),
    )
}
fn changed_keys(before: Option<&toml::Value>, now: &toml::Value) -> Vec<String> {
    fn walk(
        prefix: &str,
        before: Option<&toml::Value>,
        now: Option<&toml::Value>,
        out: &mut Vec<String>,
    ) {
        if before == now {
            return;
        }
        let a = before.and_then(toml::Value::as_table);
        let b = now.and_then(toml::Value::as_table);
        if a.is_some() || b.is_some() {
            let keys: std::collections::BTreeSet<_> = a
                .into_iter()
                .flat_map(|t| t.keys())
                .chain(b.into_iter().flat_map(|t| t.keys()))
                .collect();
            for key in keys {
                let name = if prefix.is_empty() {
                    key.to_string()
                } else {
                    format!("{prefix}.{key}")
                };
                walk(
                    &name,
                    a.and_then(|t| t.get(key)),
                    b.and_then(|t| t.get(key)),
                    out,
                );
            }
        } else {
            out.push(crate::redact::redact_secrets(prefix));
        }
    }
    let mut out = Vec::new();
    walk("", before, Some(now), &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_toolchain_inventory_handles_installed_missing_and_unavailable() {
        let root = std::env::temp_dir().join(format!("clat-rustup-{}", uuid::Uuid::new_v4()));
        assert_eq!(installed_in(&root, "stable").unwrap(), "unknown");
        std::fs::create_dir_all(root.join("toolchains/stable-test-host")).unwrap();
        assert_eq!(installed_in(&root, "stable").unwrap(), "installed");
        assert_eq!(installed_in(&root, "nightly").unwrap(), "missing");
        std::fs::write(root.join("toolchains/not-a-directory"), "").unwrap();
        assert_eq!(installed_in(&root, "not-a-directory").unwrap(), "missing");
        crate::test_support::cleanup_tree(&root);
    }
}
