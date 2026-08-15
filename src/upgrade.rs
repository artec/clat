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
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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
/// （名字就是 clat / clat.exe），其次压缩包。`.sha256` 校验文件不是
/// 可执行资产，排除。
pub fn select_asset(assets: &[ReleaseAsset]) -> Option<&ReleaseAsset> {
    let triple = target_triple();
    let matches = assets
        .iter()
        .filter(|asset| asset.name.contains(triple) && !asset.name.ends_with(".sha256"));
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

/// 下载 URL 的主机白名单：GitHub release 资产只经这些域分发。
/// release 元数据被篡改（账户/流水线失陷）时，阻止把任意站点的
/// 二进制下载回来执行。
fn is_allowed_download_host(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "https" {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    host == "github.com"
        || host == "objects.githubusercontent.com"
        || host.ends_with(".githubusercontent.com")
        || host.ends_with(".github.net")
}

/// 计算字节的 SHA-256 摘要（十六进制）。
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 下载资产到临时文件；压缩包用系统 `tar`/`unzip` 解出二进制。
/// 返回待安装二进制的临时路径。
///
/// 完整性（A-10）：
/// - URL 主机必须在 GitHub 白名单内（防元数据被篡改后拉取任意站）；
/// - `{asset}.sha256` 是必需资产，缺失、格式错误或不匹配均中止；
/// - 解包前验证所有 entry 路径，解包后拒绝任意符号链接。
fn download_asset(
    asset: &ReleaseAsset,
    checksum_asset: &ReleaseAsset,
    work_dir: &Path,
) -> Result<PathBuf, String> {
    validate_asset_name(&asset.name)?;
    validate_asset_name(&checksum_asset.name)?;
    if !is_allowed_download_host(&asset.url) {
        let url = &asset.url;
        return Err(format!(
            "download {}: host not in the GitHub allowlist ({url})",
            asset.name
        ));
    }
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

    // 哈希校验文件是发布契约的一部分；缺失由调用方在下载前拒绝。
    verify_checksum(checksum_asset, &bytes, &asset.name)?;

    if is_bare_binary(&asset.name) {
        let target = work_dir.join("clat-download");
        fs::write(&target, &bytes).map_err(|error| format!("write download: {error}"))?;
        return Ok(target);
    }

    // 压缩资产：先落盘再用系统工具解包。zip 在 Windows 上用系统自带
    // 的 bsdtar（Windows 10+ 预装，可解 zip），Unix 上用 unzip。
    let archive = work_dir.join(&asset.name);
    fs::write(&archive, &bytes).map_err(|error| format!("write archive: {error}"))?;
    validate_archive(&archive, &asset.name)?;
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
    reject_extracted_links(work_dir)?;
    // 解包后的二进制叫 clat / clat.exe，或与资产同名去扩展名。
    // 符号链接一律拒绝：压缩包可携带指向任意目标的链接。
    let stem = asset.name.split('.').next().unwrap_or("clat");
    for candidate in ["clat.exe", "clat", stem] {
        let path = work_dir.join(candidate);
        let is_regular_file = fs::symlink_metadata(&path)
            .map(|metadata| metadata.is_file())
            .unwrap_or(false);
        if is_regular_file {
            return Ok(path);
        }
    }
    Err(format!(
        "extract {}: binary not found in archive",
        asset.name
    ))
}

fn validate_asset_name(name: &str) -> Result<(), String> {
    let path = Path::new(name);
    let bare = path.file_name().and_then(|value| value.to_str()) == Some(name);
    if name.is_empty() || !bare || name.contains(['/', '\\']) || name.contains(':') {
        return Err(format!("unsafe release asset name: {name:?}"));
    }
    Ok(())
}

/// 先让系统解包器只列出 entry，再逐项拒绝绝对路径、父级遍历、
/// Windows 盘符和反斜线路径。验证通过前不触碰解压目标目录。
fn validate_archive(archive: &Path, asset_name: &str) -> Result<(), String> {
    let output = if asset_name.ends_with(".zip") && !cfg!(windows) {
        Command::new("unzip").arg("-Z1").arg(archive).output()
    } else if asset_name.ends_with(".zip") {
        Command::new("tar").arg("-tf").arg(archive).output()
    } else {
        Command::new("tar").arg("-tzf").arg(archive).output()
    }
    .map_err(|error| format!("inspect archive {asset_name}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "inspect archive {asset_name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let listing = String::from_utf8(output.stdout)
        .map_err(|_| format!("inspect archive {asset_name}: entry names are not UTF-8"))?;
    for entry in listing.lines() {
        validate_archive_entry(entry)?;
    }
    // tar/bsdtar 的 verbose 列表首字符是 entry 类型。解压前拒绝
    // symlink/hardlink，避免 link target 在落盘阶段指向工作目录外。
    if !asset_name.ends_with(".zip") || cfg!(windows) {
        let verbose = if asset_name.ends_with(".zip") {
            Command::new("tar").arg("-tvf").arg(archive).output()
        } else {
            Command::new("tar").arg("-tvzf").arg(archive).output()
        }
        .map_err(|error| format!("inspect archive links {asset_name}: {error}"))?;
        if !verbose.status.success() {
            return Err(format!(
                "inspect archive links {asset_name}: {}",
                String::from_utf8_lossy(&verbose.stderr).trim()
            ));
        }
        let verbose = String::from_utf8_lossy(&verbose.stdout);
        if verbose.lines().any(archive_listing_line_is_link) {
            return Err(format!("archive {asset_name} contains a link entry"));
        }
    }
    Ok(())
}

fn archive_listing_line_is_link(line: &str) -> bool {
    matches!(line.as_bytes().first(), Some(b'l' | b'h')) || line.contains(" link to ")
}

fn validate_archive_entry(entry: &str) -> Result<(), String> {
    let entry = entry.trim_end_matches('\r');
    if entry.is_empty() || entry.contains('\\') {
        return Err(format!("unsafe archive entry path: {entry:?}"));
    }
    let path = Path::new(entry);
    if path.is_absolute()
        || entry.starts_with('/')
        || entry
            .split('/')
            .next()
            .is_some_and(|part| part.contains(':'))
        || entry.split('/').any(|part| part == "..")
    {
        return Err(format!("unsafe archive entry path: {entry:?}"));
    }
    Ok(())
}

fn reject_extracted_links(root: &Path) -> Result<(), String> {
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in
            fs::read_dir(&directory).map_err(|error| format!("inspect extracted files: {error}"))?
        {
            let entry = entry.map_err(|error| format!("inspect extracted files: {error}"))?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|error| format!("inspect extracted files: {error}"))?;
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "archive contains a symbolic link: {}",
                    entry.path().display()
                ));
            }
            if metadata.is_dir() {
                directories.push(entry.path());
            }
        }
    }
    Ok(())
}

/// 下载并比对 `{asset}.sha256`：文件内容应为 `<hex>  <name>` 格式
/// （sha256sum 输出）。不匹配即中止升级，保留旧二进制。
fn verify_checksum(
    checksum: &ReleaseAsset,
    asset_bytes: &[u8],
    asset_name: &str,
) -> Result<(), String> {
    if !is_allowed_download_host(&checksum.url) {
        return Err(format!(
            "checksum {}: host not in the GitHub allowlist",
            checksum.name
        ));
    }
    let response = agent()
        .get(&checksum.url)
        .header("User-Agent", format!("clat/{}", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|error| format!("download {}: {error}", checksum.name))?;
    if !response.status().is_success() {
        return Err(format!(
            "download {}: HTTP {}",
            checksum.name,
            response.status()
        ));
    }
    let mut text = String::new();
    let (_, mut body) = response.into_parts();
    body.as_reader()
        .read_to_string(&mut text)
        .map_err(|error| format!("read {}: {error}", checksum.name))?;
    let expected = parse_checksum_file(&text, &checksum.name, asset_name)?;
    let actual = sha256_hex(asset_bytes);
    if expected != actual {
        return Err(format!(
            "checksum mismatch for {asset_name}: expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

fn parse_checksum_file(
    text: &str,
    checksum_name: &str,
    asset_name: &str,
) -> Result<String, String> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let line = lines
        .next()
        .ok_or_else(|| format!("checksum {checksum_name} is empty"))?;
    if lines.next().is_some() {
        return Err(format!(
            "invalid checksum file {checksum_name} for {asset_name}"
        ));
    }
    let mut fields = line.split_whitespace();
    let expected = fields.next().unwrap_or_default().to_ascii_lowercase();
    let named_asset = fields.next().unwrap_or_default().trim_start_matches('*');
    if expected.len() != 64
        || !expected.bytes().all(|byte| byte.is_ascii_hexdigit())
        || named_asset != asset_name
        || fields.next().is_some()
    {
        return Err(format!(
            "invalid checksum file {checksum_name} for {asset_name}"
        ));
    }
    Ok(expected)
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
    // 校验文件与资产同名加 `.sha256`（release 工作流随资产发布）。
    // 在创建临时目录/发起下载前即 fail closed。
    let checksum_name = format!("{}.sha256", asset.name);
    let checksum_asset = release
        .assets
        .iter()
        .find(|candidate| candidate.name == checksum_name)
        .ok_or_else(|| {
            format!(
                "release {} is missing required checksum asset {checksum_name}",
                release.tag
            )
        })?;
    let exe = std::env::current_exe().map_err(|error| format!("locate clat binary: {error}"))?;
    // 临时目录带纳秒级随机后缀并以独占方式创建：同用户进程无法
    // 预建目录/符号链接制造竞争（A-10）。碰撞时换名重试。
    let mut work_dir = None;
    for attempt in 0..5 {
        let candidate = std::env::temp_dir().join(format!(
            "clat-upgrade-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(attempt as u128),
            attempt
        ));
        if fs::create_dir(&candidate).is_ok() {
            work_dir = Some(candidate);
            break;
        }
    }
    let Some(work_dir) = work_dir else {
        return Err("create work dir: all attempts failed".into());
    };
    let result = download_asset(asset, checksum_asset, &work_dir)
        .and_then(|binary| replace_binary(&binary, &exe));
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

    /// A-10：下载主机白名单——只有 GitHub 系域名 + HTTPS 放行。
    #[test]
    fn download_hosts_are_restricted_to_github() {
        assert!(is_allowed_download_host(
            "https://github.com/artec/clat/releases/download/v0.2.0/clat.tar.gz"
        ));
        assert!(is_allowed_download_host(
            "https://objects.githubusercontent.com/some-blob"
        ));
        assert!(is_allowed_download_host(
            "https://release-assets.githubusercontent.com/x"
        ));
        // 明文 HTTP、任意站点、以 GitHub 域名结尾的仿冒站全部拒绝。
        assert!(!is_allowed_download_host(
            "http://github.com/artec/clat/releases/download/v0.2.0/clat.tar.gz"
        ));
        assert!(!is_allowed_download_host(
            "https://evil.example.com/clat.tar.gz"
        ));
        assert!(!is_allowed_download_host(
            "https://evil.github.com.attacker.io/clat"
        ));
        assert!(!is_allowed_download_host("not a url"));
    }

    #[test]
    fn sha256_hex_matches_known_digest() {
        // sha256("") 的公认摘要。
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // sha256("abc") 的公认摘要。
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn rejects_unsafe_asset_and_archive_paths() {
        for name in [
            "../clat.tar.gz",
            "sub/clat.zip",
            "sub\\clat.zip",
            "C:clat.zip",
        ] {
            assert!(validate_asset_name(name).is_err(), "must reject {name:?}");
        }
        assert!(validate_asset_name("clat-v1-aarch64-apple-darwin.tar.gz").is_ok());

        for entry in [
            "../clat",
            "bin/../../clat",
            "/tmp/clat",
            "C:/clat.exe",
            "bin\\clat.exe",
            "",
        ] {
            assert!(
                validate_archive_entry(entry).is_err(),
                "must reject {entry:?}"
            );
        }
        for entry in ["clat", "./clat", "bin/clat.exe"] {
            validate_archive_entry(entry).expect("safe relative archive path");
        }
        assert!(archive_listing_line_is_link(
            "lrwxr-xr-x user/group 0 date clat -> /tmp/evil"
        ));
        assert!(archive_listing_line_is_link(
            "hrw-r--r-- user/group 0 date clat link to ../../evil"
        ));
        assert!(!archive_listing_line_is_link(
            "-rwxr-xr-x user/group 10 date clat"
        ));
    }

    #[test]
    fn checksum_files_require_a_valid_digest_and_exact_asset_name() {
        let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(
            parse_checksum_file(
                &format!("{digest}  clat.tar.gz\n"),
                "clat.tar.gz.sha256",
                "clat.tar.gz"
            )
            .unwrap(),
            digest
        );
        for invalid in [
            "",
            "not-a-hash  clat.tar.gz",
            &format!("{digest}  other.tar.gz"),
            &format!("{digest}  clat.tar.gz extra"),
            &format!("{digest}  clat.tar.gz\n{digest}  clat.tar.gz"),
        ] {
            assert!(parse_checksum_file(invalid, "clat.tar.gz.sha256", "clat.tar.gz").is_err());
        }
    }

    #[test]
    fn selects_the_matching_platform_asset_preferring_bare_binaries() {
        // 资产名按当前平台三元组动态构造，测试在任何 CI 平台都成立。
        // 命名与 release 工作流一致：clat-{tag}-{triple}[.tar.gz|.zip]。
        let triple = target_triple();
        let bare = format!("clat-v0.2.0-{triple}");
        let archived = format!("clat-v0.2.0-{triple}.tar.gz");
        let checksummed = format!("{archived}.sha256");
        let assets = vec![
            ReleaseAsset {
                name: "clat-v0.2.0-x86_64-pc-windows-msvc.zip".into(),
                url: "u1".into(),
            },
            ReleaseAsset {
                name: archived.clone(),
                url: "u2".into(),
            },
            // 校验文件含三元组但不是可执行资产，绝不能被选中。
            ReleaseAsset {
                name: checksummed,
                url: "u2c".into(),
            },
            ReleaseAsset {
                name: bare.clone(),
                url: "u3".into(),
            },
            // GitHub 自动生成的源码包不含目标三元组，永不匹配。
            ReleaseAsset {
                name: "Source code (tar.gz)".into(),
                url: "u4".into(),
            },
        ];
        let selected = select_asset(&assets).expect("asset for this platform");
        // 同平台下裸二进制优先于压缩包。
        assert_eq!(selected.name, bare);

        // 只有校验文件（无裸二进制/压缩包）时同样视为无资产。
        let checksums_only = vec![ReleaseAsset {
            name: format!("clat-v0.2.0-{triple}.tar.gz.sha256"),
            url: "c".into(),
        }];
        assert!(select_asset(&checksums_only).is_none());

        // 没有当前平台资产（只有别平台 + 源码包）时返回 None。
        let others = vec![
            ReleaseAsset {
                name: "clat-v0.2.0-x86_64-pc-windows-msvc.zip".into(),
                url: "u".into(),
            },
            ReleaseAsset {
                name: "Source code (zip)".into(),
                url: "s".into(),
            },
        ];
        #[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
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
