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

const RELEASE_PUBLIC_KEY_FILE: &str = include_str!("../release/minisign.pub");
const MAX_CHECKSUM_BYTES: usize = 4 * 1024;
const MAX_SIGNATURE_BYTES: usize = 16 * 1024;

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
    // FP-07：release metadata 有界读取（1MiB）——签名管不到这条路径，
    // 异常 CDN/代理不能先灌满内存。
    const MAX_RELEASE_JSON_BYTES: usize = 1024 * 1024;
    let mut body = String::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_RELEASE_JSON_BYTES as u64 + 1)
        .read_to_string(&mut body)
        .map_err(|error| format!("read release metadata: {error}"))?;
    if body.len() > MAX_RELEASE_JSON_BYTES {
        return Err(format!(
            "release metadata exceeds {MAX_RELEASE_JSON_BYTES} bytes"
        ));
    }
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
    let matches = assets.iter().filter(|asset| {
        asset.name.contains(triple)
            && !asset.name.ends_with(".sha256")
            && !asset.name.ends_with(".minisig")
    });
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
/// FP-11（2026-08-22 审计）：完整 SemVer precedence。旧实现逐段取
/// 前导数字、非数字尾巴静默当 0——prerelease 完全不参与比较，
/// `0.8.1` vs `0.8.1-rc.1` 判相等（RC 用户升不上同号 stable）。
/// 现在按 semver.org：三段数字 + prerelease（数字标识 < 字母数字；
/// 同前缀更短者小；无 prerelease > 有）；**不可解析显式拒绝**
///（候选 tag 解析失败 → 不视为更新 + stderr 警告，绝不静默当 0
/// 参与安全相关的升级判断）。
pub fn is_newer(tag: &str, current: &str) -> bool {
    let candidate = match parse_semver(tag) {
        Some(version) => version,
        None => {
            eprintln!("clat: warning: ignoring unparseable release tag {tag:?}");
            return false;
        }
    };
    let current = match parse_semver(current) {
        Some(version) => version,
        None => {
            eprintln!("clat: warning: cannot parse current version {current:?}");
            return false;
        }
    };
    compare_semver(&candidate, &current) == std::cmp::Ordering::Greater
}

/// `v?MAJOR.MINOR.PATCH(-prerelease)?`；段必须是无空数字的纯数字，
/// prerelease 是以 `.` 分隔的非空标识符（SemVer §9）。
fn parse_semver(version: &str) -> Option<(Vec<u64>, Option<Vec<PreIdentifier>>)> {
    let version = version.trim_start_matches('v');
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let mut numbers = Vec::with_capacity(3);
    for part in core.split('.') {
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        numbers.push(part.parse().ok()?);
    }
    if numbers.len() != 3 {
        return None;
    }
    let pre = match pre {
        Some(pre) => Some(
            pre.split('.')
                .map(|identifier| {
                    if identifier.is_empty() {
                        return None;
                    }
                    if identifier.bytes().all(|byte| byte.is_ascii_digit()) {
                        Some(PreIdentifier::Numeric(identifier.parse().ok()?))
                    } else {
                        // SemVer：字母数字标识不能含连字符以外的符号；
                        // 这里宽松接受（比较语义不受影响：非纯数字即字母数字）。
                        Some(PreIdentifier::Alpha(identifier.to_owned()))
                    }
                })
                .collect::<Option<Vec<_>>>()?,
        ),
        None => None,
    };
    Some((numbers, pre))
}

#[derive(Debug, Eq, PartialEq)]
enum PreIdentifier {
    Numeric(u64),
    Alpha(String),
}

impl std::cmp::PartialOrd for PreIdentifier {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// semver.org §11：数字标识总小于字母数字标识。
impl Ord for PreIdentifier {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (PreIdentifier::Numeric(a), PreIdentifier::Numeric(b)) => a.cmp(b),
            (PreIdentifier::Alpha(_), PreIdentifier::Numeric(_)) => std::cmp::Ordering::Greater,
            (PreIdentifier::Numeric(_), PreIdentifier::Alpha(_)) => std::cmp::Ordering::Less,
            (PreIdentifier::Alpha(a), PreIdentifier::Alpha(b)) => a.cmp(b),
        }
    }
}

/// `(numbers, prerelease)` 的 SemVer 偏序：core 段先比；同 core 时
/// **无 prerelease > 有**；prerelease 之间逐标识比（同前缀更短者小）。
fn compare_semver(
    a: &(Vec<u64>, Option<Vec<PreIdentifier>>),
    b: &(Vec<u64>, Option<Vec<PreIdentifier>>),
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match a.0.cmp(&b.0) {
        Ordering::Equal => {}
        other => return other,
    }
    match (&a.1, &b.1) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(pa), Some(pb)) => {
            for (left, right) in pa.iter().zip(pb.iter()) {
                match left.cmp(right) {
                    Ordering::Equal => continue,
                    other => return other,
                }
            }
            pa.len().cmp(&pb.len())
        }
    }
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
#[cfg(test)]
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 已知可执行格式的魔数：ELF、Mach-O（32/64 位两种字节序、fat）、
/// Windows PE（MZ）。下载或解包产物必须匹配其一，否则视为非二进制
/// （例如误抓的校验文本），在替换自身之前中止——0.2.0 曾因把
/// `.sha256` 文本当选"裸二进制"安装，把用户可执行文件变成一行哈希。
fn looks_like_executable(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x7f, b'E', b'L', b'F'])        // ELF
        || bytes.starts_with(&[0xFE, 0xED, 0xFA, 0xCE]) // Mach-O 32 BE
        || bytes.starts_with(&[0xFE, 0xED, 0xFA, 0xCF]) // Mach-O 64 BE
        || bytes.starts_with(&[0xCE, 0xFA, 0xED, 0xFE]) // Mach-O 32 LE
        || bytes.starts_with(&[0xCF, 0xFA, 0xED, 0xFE]) // Mach-O 64 LE
        || bytes.starts_with(&[0xCA, 0xFE, 0xBA, 0xBE]) // Mach-O universal
        || bytes.starts_with(&[0x4D, 0x5A]) // PE / MZ
}

/// 替换自身前的最后一道闸：产物必须是普通文件且带可执行魔数。
fn verify_executable_file(path: &Path) -> Result<(), String> {
    let mut header = [0u8; 4];
    fs::File::open(path)
        .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut header))
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if !looks_like_executable(&header) {
        return Err(format!(
            "{} is not an executable binary (unexpected magic bytes)",
            path.display()
        ));
    }
    Ok(())
}

/// 下载资产到临时文件；压缩包用系统 `tar`/`unzip` 解出二进制。
/// 返回待安装二进制的临时路径。
///
/// 完整性（A-10）：
/// - URL 主机必须在 GitHub 白名单内（防元数据被篡改后拉取任意站）；
/// - `{asset}.sha256` 及其 `.minisig` 签名均为必需资产；先用内置公钥
///   验签清单，再按清单核对 SHA-256，任一步失败都中止；
/// - 解包前验证所有 entry 路径，解包后拒绝任意符号链接；
/// - 替换前验证产物带可执行魔数（ELF/Mach-O/PE）。
fn download_asset(
    asset: &ReleaseAsset,
    checksum_asset: &ReleaseAsset,
    signature_asset: &ReleaseAsset,
    work_dir: &Path,
    release_tag: &str,
) -> Result<PathBuf, String> {
    validate_asset_name(&asset.name)?;
    validate_asset_name(&checksum_asset.name)?;
    validate_asset_name(&signature_asset.name)?;
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
    // FP-07：Content-Length 早期 sanity（仅 sanity——真实上限由读取
    // 时的累计帽执行，分块传输/无长度头不受骗）。
    const MAX_ASSET_BYTES: u64 = 256 * 1024 * 1024;
    if response
        .headers()
        .get("Content-Length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_ASSET_BYTES)
    {
        return Err(format!(
            "download {}: asset larger than {MAX_ASSET_BYTES} bytes",
            asset.name
        ));
    }
    // FP-07：流式下载到 work_dir + 增量哈希——签名/哈希保护完整性，
    // 不保护 pre-verification 资源消耗；旧实现 read_to_end 全量入内存
    // 后才发现哈希不匹配，被攻陷 CDN 可先 OOM 再谈验签。
    let slot = if is_bare_binary(&asset.name) {
        work_dir.join("clat-download")
    } else {
        work_dir.join(&asset.name)
    };
    let mut file = fs::File::create(&slot)
        .map_err(|error| format!("stage download {}: {error}", asset.name))?;
    let (_, mut body) = response.into_parts();
    let actual =
        match stream_body_to_file(body.as_reader(), &mut file, MAX_ASSET_BYTES, &asset.name) {
            Ok(hash) => hash,
            Err(error) => {
                let _ = fs::remove_file(&slot);
                return Err(error);
            }
        };
    drop(file);

    // 哈希校验文件是发布契约的一部分；缺失由调用方在下载前拒绝。
    verify_checksum(
        checksum_asset,
        signature_asset,
        &actual,
        &asset.name,
        release_tag,
    )?;

    if is_bare_binary(&asset.name) {
        verify_executable_file(&slot)?;
        return Ok(slot);
    }

    // 压缩资产：已流式落盘（slot 即 archive），用系统工具解包。zip 在
    // Windows 上用系统自带的 bsdtar（Windows 10+ 预装，可解 zip），
    // Unix 上用 unzip。
    let archive = slot;
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
            verify_executable_file(&path)?;
            return Ok(path);
        }
    }
    Err(format!(
        "extract {}: binary not found in archive",
        asset.name
    ))
}

/// FP-07：流式下载到文件 + 增量 SHA-256（64KiB 块 + 累计字节帽）。
/// 返回小写 hex 摘要；超帽 → 错误（调用方清理半成品）。
fn stream_body_to_file(
    mut body: impl Read,
    file: &mut fs::File,
    max_bytes: u64,
    asset_name: &str,
) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Write as _;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let read = body
            .read(&mut chunk)
            .map_err(|error| format!("read {asset_name}: {error}"))?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > max_bytes {
            return Err(format!(
                "download {asset_name}: asset exceeds {max_bytes} bytes"
            ));
        }
        hasher.update(&chunk[..read]);
        file.write_all(&chunk[..read])
            .map_err(|error| format!("write download: {error}"))?;
    }
    Ok(format!("{:x}", hasher.finalize()))
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
    // FP-07：entry listing 帽（更紧的显式拒绝；真实上界由资产帽传导
    // ——listing 字节数不超过资产字节数）。
    const MAX_ARCHIVE_LISTING_BYTES: usize = 4 * 1024 * 1024;
    if output.stdout.len() > MAX_ARCHIVE_LISTING_BYTES {
        return Err(format!(
            "archive {asset_name}: entry listing exceeds {MAX_ARCHIVE_LISTING_BYTES} bytes"
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

/// 下载、验签并比对 `{asset}.sha256`。签名覆盖清单的原始字节，公钥
/// 编译进 CLAT 二进制，因此 GitHub 账户/CDN 只能替换资产而无法伪造
/// 一份可通过验证的新清单。
fn verify_checksum(
    checksum: &ReleaseAsset,
    signature: &ReleaseAsset,
    actual_sha256: &str,
    asset_name: &str,
    release_tag: &str,
) -> Result<(), String> {
    let checksum_bytes = download_small_asset(checksum, "checksum", MAX_CHECKSUM_BYTES)?;
    let signature_bytes = download_small_asset(signature, "signature", MAX_SIGNATURE_BYTES)?;
    verify_release_assets(
        release_tag,
        &checksum.name,
        &checksum_bytes,
        &signature_bytes,
        actual_sha256,
        asset_name,
    )
}

/// FP-04：验签核心（纯函数——离线可测重放场景）。签名真实性之上，
/// trusted comment 必须绑定当前 release tag：签名证明「维护者签过这
/// 份 manifest」，不证明「它属于当前 release」；被攻陷的 GitHub
/// 控制面把历史合法签名的旧资产挂到新 tag 即构成 signed rollback。
fn verify_release_assets(
    release_tag: &str,
    checksum_name: &str,
    checksum_bytes: &[u8],
    signature_bytes: &[u8],
    actual_sha256: &str,
    asset_name: &str,
) -> Result<(), String> {
    let comment = verify_manifest_signature(checksum_bytes, signature_bytes)?;
    ensure_signature_bound_to_tag(&comment, release_tag)?;
    let text = std::str::from_utf8(checksum_bytes)
        .map_err(|_| format!("checksum {checksum_name} is not UTF-8"))?;
    let expected = parse_checksum_file(text, checksum_name, asset_name)?;
    let actual = actual_sha256;
    if expected != actual {
        return Err(format!(
            "checksum mismatch for {asset_name}: expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

fn download_small_asset(asset: &ReleaseAsset, kind: &str, cap: usize) -> Result<Vec<u8>, String> {
    if !is_allowed_download_host(&asset.url) {
        return Err(format!(
            "{kind} {}: host not in the GitHub allowlist",
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
    let (_, body) = response.into_parts();
    let mut bytes = Vec::new();
    body.into_reader()
        .take((cap + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {error}", asset.name))?;
    if bytes.len() > cap {
        return Err(format!("{kind} {} exceeds {cap} bytes", asset.name));
    }
    Ok(bytes)
}

fn release_public_key() -> Result<minisign_verify::PublicKey, String> {
    let encoded = RELEASE_PUBLIC_KEY_FILE
        .lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with("untrusted comment:"))
        .ok_or("embedded release public key is missing")?;
    minisign_verify::PublicKey::from_base64(encoded.trim())
        .map_err(|error| format!("invalid embedded release public key: {error}"))
}

fn verify_manifest_signature(manifest: &[u8], signature: &[u8]) -> Result<String, String> {
    let signature = std::str::from_utf8(signature)
        .map_err(|_| "release manifest signature is not UTF-8".to_owned())?;
    let signature = minisign_verify::Signature::decode(signature)
        .map_err(|error| format!("invalid release manifest signature: {error}"))?;
    release_public_key()?
        .verify(manifest, &signature, false)
        .map_err(|error| format!("release manifest signature verification failed: {error}"))?;
    // trusted comment 是签名覆盖的字段（global signature 覆盖
    // signature || trusted_comment——minisign-verify lib.rs:338-344，
    // 无私钥不可伪造）；publish 脚本写入 `CLAT $TAG release checksum
    // manifests`（发布侧既有惯例，零变更）。
    Ok(signature.trusted_comment().to_owned())
}

/// FP-04：签名身份绑定——trusted comment 必须精确等于 publish 惯例
/// 文案 `CLAT {release tag} release checksum manifests`。历史 release
/// 的注释 = 各自旧 tag，对被冒充的新 tag 天然失配 → 天然防重放。
fn ensure_signature_bound_to_tag(comment: &str, tag: &str) -> Result<(), String> {
    let expected = format!("CLAT {tag} release checksum manifests");
    if comment == expected {
        Ok(())
    } else {
        Err(format!(
            "asset signature is bound to {comment:?}; this release claims {tag:?} — \
             possible signed-asset replay (rollback); refusing to install"
        ))
    }
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
///
/// FP-05（2026-08-22 审计）：统一改为 **stage + 同目录 swap**——
/// 先把已验证的 source 复制到 destination **同目录**的唯一临时文件
/// （跨卷/跨文件系统的 rename 不可靠：Windows 跨卷必败；Linux
/// `/tmp` 为 tmpfs 时同样 EXDEV——旧实现 source 在系统临时目录，
/// 两平台都可能失败），再在同目录内完成替换。任何错误返回后
/// `destination` 必须仍是可执行的旧 CLAT：
/// - Unix：同目录 rename 原子生效，无需 `.old` 两步舞；
/// - Windows：不允许覆盖运行中的 exe → 两步替换；**首步错误不吞**；
///   第二步失败强制回滚 `.old`；回滚失败给出明确的 recovery 指引
///   （旧实现两步都可能失败且无回滚，可把安装"软砖化"）。
fn replace_binary(source: &Path, destination: &Path) -> Result<(), String> {
    replace_binary_with_hooks(source, destination, &SwapMode::host(), None)
}

/// swap 语义按平台选择；测试可显式指定以覆盖 Windows 分支
///（非 Windows 运行器上也可测回滚逻辑）。
#[derive(Clone, Copy, Debug, PartialEq)]
enum SwapMode {
    Unix,
    Windows,
}

impl SwapMode {
    fn host() -> Self {
        if cfg!(windows) {
            SwapMode::Windows
        } else {
            SwapMode::Unix
        }
    }
}

/// 故障注入钩子（session `FaultHooks` 先例）：`fail_swap_step` 在
/// Windows 两步替换的指定步骤强制失败（`"move_old"` / `"place_new"` /
/// `"rollback"`——最后一项模拟回滚自身失败），验证回滚与恢复指引。
#[cfg_attr(not(test), allow(dead_code))]
struct SwapHooks {
    fail_swap_step: Option<&'static str>,
    fail_rollback: bool,
}

#[allow(clippy::needless_pass_by_value)]
fn replace_binary_with_hooks(
    source: &Path,
    destination: &Path,
    mode: &SwapMode,
    hooks: Option<&SwapHooks>,
) -> Result<(), String> {
    // stage：destination 同目录的唯一临时文件（同目录 ⇒ 后续 rename
    // 不跨卷）。copy 后立即设置可执行位。
    let staged = destination.with_file_name(format!(
        ".clat-stage-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or(0)
    ));
    let result = swap_into_place(source, destination, &staged, mode, hooks);
    if result.is_err() {
        // 清理 staged；destination 保持旧内容（不变量）。
        let _ = fs::remove_file(&staged);
    }
    result
}

fn swap_into_place(
    source: &Path,
    destination: &Path,
    staged: &Path,
    mode: &SwapMode,
    hooks: Option<&SwapHooks>,
) -> Result<(), String> {
    fs::copy(source, staged).map_err(|error| format!("stage new binary: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(staged, fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("chmod new binary: {error}"))?;
    }
    match mode {
        SwapMode::Unix => {
            fs::rename(staged, destination)
                .map_err(|error| format!("replace {}: {error}", destination.display()))?;
        }
        SwapMode::Windows => {
            // Windows 不允许覆盖运行中的 exe：先移走旧的，再放入新的。
            let old = destination.with_extension("old");
            let moved_old = if hooks.and_then(|hooks| hooks.fail_swap_step) == Some("move_old") {
                Err("injected: move_old".to_owned())
            } else {
                fs::rename(destination, &old).map_err(|error| format!("park old binary: {error}"))
            };
            moved_old.map_err(|error| {
                format!(
                    "cannot park the running binary aside ({error}); \
                     aborting before any replacement — the installation is untouched"
                )
            })?;
            let placed = if hooks.and_then(|hooks| hooks.fail_swap_step) == Some("place_new") {
                Err("injected: place_new".to_owned())
            } else {
                fs::rename(staged, destination)
                    .map_err(|error| format!("replace {}: {error}", destination.display()))
            };
            if let Err(error) = placed {
                // 强制回滚：旧二进制必须回到原位，否则下次启动无入口。
                if hooks.is_some_and(|hooks| hooks.fail_rollback) {
                    let _ = fs::remove_file(&old);
                }
                return match fs::rename(&old, destination) {
                    Ok(()) => Err(format!(
                        "{error}; the previous binary was restored — the upgrade did not apply"
                    )),
                    Err(rollback) => Err(format!(
                        "{error} AND restoring the previous binary failed ({rollback}). \
                         Manual recovery: rename {old} back to {destination} before restarting",
                        old = old.display(),
                        destination = destination.display(),
                    )),
                };
            }
            let _ = fs::remove_file(&old);
        }
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
    let signature_name = format!("{checksum_name}.minisig");
    let signature_asset = release
        .assets
        .iter()
        .find(|candidate| candidate.name == signature_name)
        .ok_or_else(|| {
            format!(
                "release {} is missing required signature asset {signature_name}",
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
    let result = download_asset(
        asset,
        checksum_asset,
        signature_asset,
        &work_dir,
        &release.tag,
    )
    .and_then(|binary| replace_binary(&binary, &exe));
    let _ = fs::remove_dir_all(&work_dir);
    result.map(|()| UpgradeOutcome::Installed { tag: release.tag })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- FP-05：替换原子性（stage + 同目录 swap + 回滚）。判别力：
    // 删除回滚段 → restore/manual-recovery 两测试红（新函数前置编译
    // 不可达，按惯例文档化）。

    fn upgrade_temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "clat-upgrade-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn entries_of(dir: &std::path::Path) -> Vec<String> {
        fs::read_dir(dir)
            .expect("read_dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn replace_binary_swaps_in_place_and_leaves_no_residue() {
        let dest_dir = upgrade_temp("swap-dest");
        let source_dir = upgrade_temp("swap-src");
        let destination = dest_dir.join("clat");
        fs::write(&destination, "old-binary").expect("old");
        let source = source_dir.join("clat-new");
        fs::write(&source, "new-binary").expect("new");
        replace_binary(&source, &destination).expect("swap");
        assert_eq!(
            fs::read_to_string(&destination).expect("dest"),
            "new-binary"
        );
        assert_eq!(
            entries_of(&dest_dir),
            vec!["clat".to_owned()],
            "no stage/.old residue in the destination directory"
        );
        let _ = fs::remove_dir_all(&dest_dir);
        let _ = fs::remove_dir_all(&source_dir);
    }

    #[test]
    fn windows_swap_failure_restores_the_old_binary() {
        let dest_dir = upgrade_temp("swap-restore");
        let source_dir = upgrade_temp("swap-restore-src");
        let destination = dest_dir.join("clat.exe");
        fs::write(&destination, "old-binary").expect("old");
        let source = source_dir.join("clat-new");
        fs::write(&source, "new-binary").expect("new");
        let error = replace_binary_with_hooks(
            &source,
            &destination,
            &SwapMode::Windows,
            Some(&SwapHooks {
                fail_swap_step: Some("place_new"),
                fail_rollback: false,
            }),
        )
        .expect_err("the second step fails");
        assert!(
            error.contains("restored"),
            "the error must state the old binary was restored: {error}"
        );
        assert_eq!(
            fs::read_to_string(&destination).expect("dest"),
            "old-binary",
            "destination must still be the old executable (FP-05 invariant)"
        );
        assert_eq!(
            entries_of(&dest_dir),
            vec!["clat.exe".to_owned()],
            "rollback removes .old and the staged file"
        );
        let _ = fs::remove_dir_all(&dest_dir);
        let _ = fs::remove_dir_all(&source_dir);
    }

    #[test]
    fn windows_swap_double_failure_reports_manual_recovery() {
        let dest_dir = upgrade_temp("swap-recovery");
        let source_dir = upgrade_temp("swap-recovery-src");
        let destination = dest_dir.join("clat.exe");
        fs::write(&destination, "old-binary").expect("old");
        let source = source_dir.join("clat-new");
        fs::write(&source, "new-binary").expect("new");
        let error = replace_binary_with_hooks(
            &source,
            &destination,
            &SwapMode::Windows,
            Some(&SwapHooks {
                fail_swap_step: Some("place_new"),
                fail_rollback: true,
            }),
        )
        .expect_err("rollback also fails");
        assert!(
            error.contains("Manual recovery"),
            "double failure must escalate to explicit recovery guidance: {error}"
        );
        assert!(
            error.contains("clat.old"),
            "the guidance must name the parked binary: {error}"
        );
        let _ = fs::remove_dir_all(&dest_dir);
        let _ = fs::remove_dir_all(&source_dir);
    }

    #[test]
    fn windows_swap_move_old_failure_leaves_installation_untouched() {
        let dest_dir = upgrade_temp("swap-moveold");
        let source_dir = upgrade_temp("swap-moveold-src");
        let destination = dest_dir.join("clat.exe");
        fs::write(&destination, "old-binary").expect("old");
        let source = source_dir.join("clat-new");
        fs::write(&source, "new-binary").expect("new");
        let error = replace_binary_with_hooks(
            &source,
            &destination,
            &SwapMode::Windows,
            Some(&SwapHooks {
                fail_swap_step: Some("move_old"),
                fail_rollback: false,
            }),
        )
        .expect_err("the first step fails");
        assert!(
            error.contains("untouched"),
            "a failed park must abort before any replacement: {error}"
        );
        assert_eq!(
            fs::read_to_string(&destination).expect("dest"),
            "old-binary"
        );
        let _ = fs::remove_dir_all(&dest_dir);
        let _ = fs::remove_dir_all(&source_dir);
    }

    #[test]
    fn compares_versions_semantically() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("0.1.2", "0.1.1"));
        assert!(is_newer("1.0.0", "0.9.99"));
        assert!(!is_newer("v0.1.1", "0.1.1"));
        // FP-11（前置红腿）：完整 prerelease precedence——stable 高于
        // 同号 rc（旧实现判相等 → false，红）；beta < rc；非法 tag
        // 显式拒绝为「不是更新」而非静默当 0。
        assert!(
            is_newer("0.8.1", "0.8.1-rc.1"),
            "stable must outrank its own prerelease"
        );
        assert!(!is_newer("0.8.1-rc.1", "0.8.1"));
        assert!(
            is_newer("0.8.1-rc.1", "0.8.1-beta.2"),
            "rc outranks beta (alphanumeric precedence)"
        );
        assert!(
            is_newer("0.8.1-rc.2", "0.8.1-rc.1"),
            "numeric identifiers compare numerically"
        );
        assert!(
            is_newer("0.8.1-rc.1-alpha", "0.8.1-rc.1"),
            "longer prerelease with equal prefix is greater (semver.org §11)"
        );
        assert!(
            !is_newer("vX.y.z", "0.1.0"),
            "unparseable tags are rejected, never silently upgraded-to"
        );
        assert!(
            !is_newer("0.8", "0.1.0"),
            "two-segment tags are unparseable"
        );
        assert!(
            !is_newer("0.8.1.7", "0.8.1"),
            "four-segment tags are unparseable"
        );
        assert!(!is_newer("v0.1.0", "0.1.1"));
        // 长度不等按缺省 0 处理。
        // FP-11：两段 tag 非法——显式拒绝（旧行为补 0 当 0.2.0 放行）。
        assert!(!is_newer("0.2", "0.1.9"));
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

    /// 0.2.0 → 0.3.0 升级事故的回归测试：`.sha256` 校验文本（hex 开头
    /// 的一行 ASCII）不是可执行文件，必须在替换前被魔数检查拒绝；
    /// 各平台真实魔数必须放行。
    #[test]
    fn executable_magic_rejects_checksum_text_and_accepts_real_binaries() {
        // 事故现场：下载的其实是校验文件内容。
        let checksum_text =
            b"8e16e8c91167e96b673f8165edd1773442429de39ef379ae0446c3bb8b3c2b18  clat.tar.gz\n";
        assert!(!looks_like_executable(checksum_text));
        assert!(!looks_like_executable(b"#!/bin/sh\necho hi\n"));
        assert!(!looks_like_executable(b""));
        assert!(!looks_like_executable(&[0x00, 0x01, 0x02, 0x03]));

        // 真实平台魔数。
        assert!(looks_like_executable(&[0x7f, b'E', b'L', b'F', 0x02]));
        assert!(looks_like_executable(&[0xCF, 0xFA, 0xED, 0xFE])); // macOS arm64
        assert!(looks_like_executable(&[0xFE, 0xED, 0xFA, 0xCF])); // macOS 64 BE
        assert!(looks_like_executable(&[0xCE, 0xFA, 0xED, 0xFE])); // macOS 32 LE
        assert!(looks_like_executable(&[0xCA, 0xFE, 0xBA, 0xBE])); // universal
        assert!(looks_like_executable(&[0x4D, 0x5A, 0x90, 0x00])); // Windows PE
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
    fn release_manifest_requires_a_valid_signature_from_embedded_key() {
        let manifest = include_bytes!("../tests/fixtures/release-manifest.sha256");
        let signature = include_bytes!("../tests/fixtures/release-manifest.sha256.minisig");
        verify_manifest_signature(manifest, signature).expect("fixture signature");

        let mut tampered = manifest.to_vec();
        tampered[0] = if tampered[0] == b'a' { b'b' } else { b'a' };
        assert!(verify_manifest_signature(&tampered, signature).is_err());
        assert!(verify_manifest_signature(manifest, b"not a minisign signature").is_err());
    }

    /// FP-04（2026-08-22 审计）：签名真实性 ≠ 资产身份。真实 fixture
    /// 签名（trusted comment 为测试文案）对任意 release tag 的 publish
    /// 惯例文案失配 → 重放场景（历史合法签名资产挂到新 tag）在替换
    /// 前被拒。删除 `ensure_signature_bound_to_tag` 调用 → 本测试红。
    /// FP-07：流式下载助手——无限源在帽内失败；有限源的增量哈希与
    /// 全量哈希一致（验签路径零行为变化）。
    #[test]
    fn streaming_download_is_bounded_and_hashes_correctly() {
        struct InfiniteBody;
        impl Read for InfiniteBody {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                buffer.fill(b'a');
                Ok(buffer.len())
            }
        }
        let dir = std::env::temp_dir().join(format!(
            "clat-stream-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("dir");
        let mut file = fs::File::create(dir.join("download")).expect("file");
        let error = stream_body_to_file(InfiniteBody, &mut file, 4096, "test-asset")
            .expect_err("infinite sources must hit the cap");
        assert!(
            error.contains("exceeds"),
            "cap error must be explicit: {error}"
        );

        let mut file = fs::File::create(dir.join("finite")).expect("file");
        let payload = b"clat release bytes 0123456789";
        let hash = stream_body_to_file(&payload[..], &mut file, 4096, "test-asset").expect("hash");
        assert_eq!(hash, sha256_hex(payload), "incremental == full-file hash");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn signature_tag_binding_rejects_replayed_assets() {
        let checksum = include_bytes!("../tests/fixtures/release-manifest.sha256");
        let signature = include_bytes!("../tests/fixtures/release-manifest.sha256.minisig");
        // manifest 内容：sha256("abc") + clat-test.tar.gz。
        let error = verify_release_assets(
            "v0.9.0",
            "clat-test.tar.gz.sha256",
            checksum,
            signature,
            &sha256_hex(b"abc"),
            "clat-test.tar.gz",
        )
        .expect_err("a genuine signature for a different tag must be rejected");
        assert!(
            error.contains("replay"),
            "the error must name the replay risk: {error}"
        );
        // 绑定判据的格式纯函数腿（正例无法离线构造——无私钥重签）。
        assert!(
            ensure_signature_bound_to_tag("CLAT v0.9.0 release checksum manifests", "v0.9.0")
                .is_ok()
        );
        assert!(
            ensure_signature_bound_to_tag("CLAT v0.8.0 release checksum manifests", "v0.9.0")
                .is_err()
        );
        assert!(ensure_signature_bound_to_tag("anything else", "v0.9.0").is_err());
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
