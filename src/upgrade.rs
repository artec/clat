//! `clat upgrade` — 从 GitHub Releases 自更新。
//!
//! 流程：查询 `repos/{repo}/releases/latest` → 比较版本 → 下载匹配
//! 当前平台的资产 → 写入同目录临时文件 → 原子 rename 覆盖自身。
//! Unix 上覆盖正在运行的二进制是安全的（inode 替换，旧进程继续
//! 使用旧映像）；Windows 先把旧文件改名 `.old` 再放入新文件。
//!
//! 仓库无 release 或资产缺失时提示源码构建路径，不视为错误。

use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use ureq::Agent;

/// CLAT 的官方仓库（owner/repo）。
pub const REPO: &str = "artec/clat";

fn agent() -> Agent {
    Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .build()
        .new_agent()
}

/// 编译目标三元组，用于在 release 资产中挑出当前平台的产物。
/// 与 release 工作流的构建矩阵一致：macOS（aarch64/x86_64）与
/// Windows（x86_64/aarch64）各两组；Linux 三元组保留识别能力，
/// 资产待后续上架。
pub fn target_triple() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64-apple-darwin"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64-apple-darwin"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "aarch64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "x86_64-pc-windows-msvc"
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        "aarch64-pc-windows-msvc"
    }
}

/// 一个 release 资产：文件名与浏览器下载直链。
#[derive(Debug)]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
}

/// GitHub 最新 release 的元数据。
#[derive(Debug)]
pub struct Release {
    pub tag: String,
    pub assets: Vec<ReleaseAsset>,
}

/// 查询 GitHub 最新 release。`token`（通常取 `GITHUB_TOKEN` 环境变量）
/// 用于提升 API 限流，公开仓库匿名查询亦可（60 次/小时）。
pub fn fetch_latest_release(token: Option<&str>) -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let mut request = agent()
        .get(url)
        // GitHub API 强制要求 User-Agent。
        .header("User-Agent", format!("clat/{}", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json");
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let mut response = request
        .call()
        .map_err(|error| format!("GitHub API: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("GitHub API returned {}", response.status()));
    }
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("read release metadata: {error}"))?;
    parse_release(&body)
}

/// 解析 release 元数据：tag 与资产清单。
fn parse_release(body: &str) -> Result<Release, String> {
    let value: Value =
        serde_json::from_str(body).map_err(|error| format!("parse release: {error}"))?;
    if value
        .get("message")
        .and_then(Value::as_str)
        .is_some_and(|m| !m.is_empty())
    {
        // 仓库尚无 release 时 API 返回 404 + {"message": "Not Found"}。
        return Err(format!(
            "GitHub: {}",
            value["message"].as_str().unwrap_or_default()
        ));
    }
    let tag = value
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or("release metadata missing tag_name")?
        .to_owned();
    let mut assets = Vec::new();
    if let Some(list) = value.get("assets").and_then(Value::as_array) {
        for asset in list {
            let (Some(name), Some(url)) = (
                asset.get("name").and_then(Value::as_str),
                asset.get("browser_download_url").and_then(Value::as_str),
            ) else {
                continue;
            };
            assets.push(ReleaseAsset {
                name: name.to_owned(),
                url: url.to_owned(),
            });
        }
    }
    Ok(Release { tag, assets })
}

/// 在资产中挑出当前平台的产物：文件名包含目标三元组，优先裸二进制
/// （名字就是 clat / clat.exe），其次压缩包。
pub fn select_asset(assets: &[ReleaseAsset]) -> Option<&ReleaseAsset> {
    let triple = target_triple();
    let matches = assets.iter().filter(|asset| asset.name.contains(triple));
    matches
        .clone()
        .find(|asset| is_bare_binary(&asset.name))
        .or_else(|| matches.into_iter().next())
}

fn is_bare_binary(name: &str) -> bool {
    !name.ends_with(".tar.gz")
        && !name.ends_with(".tgz")
        && !name.ends_with(".zip")
        && !name.ends_with(".gz")
}

/// 语义化版本比较：release tag（可能带 `v` 前缀）是否比当前版本新。
/// 仅比较数字段（`0.2.0` > `0.1.9`），无法解析时保守返回 false。
pub fn is_newer(tag: &str, current: &str) -> bool {
    let parse = |version: &str| -> Vec<u64> {
        version
            .trim_start_matches('v')
            .split('.')
            .map(|part| {
                part.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    };
    let (tag, current) = (parse(tag), parse(current));
    for index in 0..tag.len().max(current.len()) {
        let a = tag.get(index).copied().unwrap_or(0);
        let b = current.get(index).copied().unwrap_or(0);
        if a != b {
            return a > b;
        }
    }
    false
}

/// 下载资产到临时文件；压缩包用系统 `tar`/`unzip` 解出二进制。
/// 返回待安装二进制的临时路径。
fn download_asset(asset: &ReleaseAsset, work_dir: &Path) -> Result<PathBuf, String> {
    let response = agent()
        .get(&asset.url)
        .header("User-Agent", format!("clat/{}", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|error| format!("download {}: {error}", asset.name))?;
    if !response.status().is_success() {
        return Err(format!(
            "download {}: HTTP {}",
            asset.name,
            response.status()
        ));
    }
    let mut bytes = Vec::new();
    let (_, mut body) = response.into_parts();
    body.as_reader()
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {error}", asset.name))?;

    if is_bare_binary(&asset.name) {
        let target = work_dir.join("clat-download");
        fs::write(&target, &bytes).map_err(|error| format!("write download: {error}"))?;
        return Ok(target);
    }

    // 压缩资产：先落盘再用系统工具解包。zip 在 Windows 上用系统自带
    // 的 bsdtar（Windows 10+ 预装，可解 zip），Unix 上用 unzip。
    let archive = work_dir.join(&asset.name);
    fs::write(&archive, &bytes).map_err(|error| format!("write archive: {error}"))?;
    let output = if asset.name.ends_with(".zip") && !cfg!(windows) {
        Command::new("unzip")
            .arg("-o")
            .arg(&archive)
            .arg("-d")
            .arg(work_dir)
            .output()
    } else if asset.name.ends_with(".zip") {
        Command::new("tar")
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(work_dir)
            .output()
    } else {
        Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(work_dir)
            .output()
    }
    .map_err(|error| format!("launch extractor for {}: {error}", asset.name))?;
    if !output.status.success() {
        return Err(format!(
            "extract {}: {}",
            asset.name,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // 解包后的二进制叫 clat / clat.exe，或与资产同名去扩展名。
    let stem = asset.name.split('.').next().unwrap_or("clat");
    for candidate in ["clat.exe", "clat", stem] {
        let path = work_dir.join(candidate);
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(format!(
        "extract {}: binary not found in archive",
        asset.name
    ))
}

/// 用 `source` 原子替换 `destination`（当前可执行文件）。
fn replace_binary(source: &Path, destination: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(source, fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("chmod new binary: {error}"))?;
        fs::rename(source, destination)
            .map_err(|error| format!("replace {}: {error}", destination.display()))?;
    }
    #[cfg(windows)]
    {
        // Windows 不允许覆盖运行中的 exe：先移走旧的，再放入新的。
        let old = destination.with_extension("old");
        let _ = fs::rename(destination, &old);
        fs::rename(source, destination)
            .map_err(|error| format!("replace {}: {error}", destination.display()))?;
        let _ = fs::remove_file(&old);
    }
    Ok(())
}

/// `upgrade()` 的结果。
pub enum UpgradeOutcome {
    /// 当前已是最新；`latest` 为最新 release tag（无 release 时为
    /// 提示文本）。
    UpToDate { latest: String },
    /// `--check` 模式发现有新版本。
    Available { tag: String },
    /// 已下载并替换，`tag` 为新版本。
    Installed { tag: String },
}

/// 完整升级流程：查最新 release → 比较版本 →（除非 check_only）
/// 下载资产并原子替换当前可执行文件。
pub fn upgrade(check_only: bool) -> Result<UpgradeOutcome, String> {
    let token = std::env::var("GITHUB_TOKEN").ok();
    let release = fetch_latest_release(token.as_deref())?;
    let current = env!("CARGO_PKG_VERSION");
    if !is_newer(&release.tag, current) {
        return Ok(UpgradeOutcome::UpToDate {
            latest: release.tag,
        });
    }
    if check_only {
        return Ok(UpgradeOutcome::Available { tag: release.tag });
    }
    let Some(asset) = select_asset(&release.assets) else {
        // 只支持二进制更新：release 未提供当前平台的产物即失败。
        return Err(format!(
            "release {} has no {} binary asset",
            release.tag,
            target_triple()
        ));
    };
    let exe = std::env::current_exe().map_err(|error| format!("locate clat binary: {error}"))?;
    let work_dir = std::env::temp_dir().join(format!("clat-upgrade-{}", std::process::id()));
    fs::create_dir_all(&work_dir).map_err(|error| format!("create work dir: {error}"))?;
    let result = download_asset(asset, &work_dir).and_then(|binary| replace_binary(&binary, &exe));
    let _ = fs::remove_dir_all(&work_dir);
    result.map(|()| UpgradeOutcome::Installed { tag: release.tag })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_semantically() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("0.1.2", "0.1.1"));
        assert!(is_newer("1.0.0", "0.9.99"));
        assert!(!is_newer("v0.1.1", "0.1.1"));
        assert!(!is_newer("v0.1.0", "0.1.1"));
        // 长度不等按缺省 0 处理。
        assert!(is_newer("0.2", "0.1.9"));
        assert!(!is_newer("0.1", "0.1.1"));
        // 非数字尾巴不参与比较。
        assert!(is_newer("v0.2.0-rc1", "0.1.0"));
    }

    #[test]
    fn selects_the_matching_platform_asset_preferring_bare_binaries() {
        // 与 release 工作流的资产命名一致：clat-{tag}-{triple}.tar.gz/.zip。
        let assets = vec![
            ReleaseAsset {
                name: "clat-v0.2.0-x86_64-pc-windows-msvc.zip".into(),
                url: "u1".into(),
            },
            ReleaseAsset {
                name: "clat-v0.2.0-aarch64-apple-darwin.tar.gz".into(),
                url: "u2".into(),
            },
            ReleaseAsset {
                name: "clat-v0.2.0-aarch64-apple-darwin".into(),
                url: "u3".into(),
            },
            // GitHub 自动生成的源码包不含目标三元组，永不匹配。
            ReleaseAsset {
                name: "Source code (tar.gz)".into(),
                url: "u4".into(),
            },
        ];
        let selected = select_asset(&assets).expect("asset");
        // 同平台下裸二进制优先于压缩包。
        assert_eq!(selected.name, "clat-v0.2.0-aarch64-apple-darwin");
        // 没有当前平台资产时返回 None。
        let others = vec![ReleaseAsset {
            name: "clat-v0.2.0-x86_64-pc-windows-msvc.zip".into(),
            url: "u".into(),
        }];
        #[cfg(not(target_os = "windows"))]
        assert!(select_asset(&others).is_none());
    }

    #[test]
    fn parses_release_metadata_and_reports_missing_releases() {
        let body = serde_json::json!({
            "tag_name": "v0.2.0",
            "assets": [
                {"name": "clat-aarch64-apple-darwin", "browser_download_url": "https://example/clat"},
                {"name": "notes.txt", "browser_download_url": "https://example/notes"}
            ]
        })
        .to_string();
        let release = parse_release(&body).expect("release");
        assert_eq!(release.tag, "v0.2.0");
        assert_eq!(release.assets.len(), 2);
        assert_eq!(release.assets[0].url, "https://example/clat");

        // 仓库无 release：GitHub 返回 message。
        let missing = parse_release(r#"{"message":"Not Found"}"#).unwrap_err();
        assert!(missing.contains("Not Found"));
    }
}
