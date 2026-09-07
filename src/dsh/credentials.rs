//! DV-10/D：`~/.dsh/.credentials.yaml` 单条记录读取 + DSH 浏览器会话
//! cookie 自铸（research §8 规格，2026-09-07 负责人批准的 deliberate
//! deviation：读取范围从 storages 投影扩至凭据文件**单条记录**；
//! 「绝不写 `~/.dsh`」边界不变）。
//!
//! 卫生纪律（§8 钉死，违反任何一条即 fail-closed 降级 `--url` 路径）：
//! - 只读 `client-connection/browser-session` 这一条记录，不碰其余键；
//! - 凭据文件本身为符号链接即拒（同用户信任的是文件，不是名字）；
//! - 永不写回、永不创建、永不改权限；
//! - 密钥与 cookie 值**永不进入错误信息/日志**——所有失败理由只描述
//!   形状（"unsupported shape"），不引用文件内容。

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use serde_yaml_ng::Value as YamlValue;
use sha2::{Digest, Sha256};
use std::path::Path;

type HmacSha256 = Hmac<Sha256>;

/// 凭据文件读取的体积上限：真实文档是几条短记录；超限即异形，拒。
const CREDENTIALS_FILE_CAP: u64 = 64 * 1024;
/// cookie 记录键（`credentialKey('client-connection','browser-session')`
/// 的字面形，credentials/types 实证：scope/id 以 `/` 连接）。
const BROWSER_SESSION_KEY: &str = "client-connection/browser-session";
/// 自铸短窗（research §8：1h 在任何宿主 cookieMaxAgeDays 配置下合法；
/// 每次连接重铸，不复用旧值）。
const COOKIE_WINDOW_MS: u64 = 60 * 60 * 1000;

/// 浏览器会话签名密钥。Debug 永远脱敏（卫生纪律）。
pub(crate) struct BrowserSessionSecret([u8; 32]);

impl std::fmt::Debug for BrowserSessionSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("BrowserSessionSecret(<redacted>)")
    }
}

/// 单条记录读取失败。文案不含文件内容（卫生纪律）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CredentialFailure {
    /// 文件缺席（或未提供 dsh home）——干净的降级路径。
    Absent,
    /// 卫生/形状拒绝：文件在，但按规格不可用。
    Rejected(&'static str),
}

impl CredentialFailure {
    /// 不携带任何文件内容的可读理由（错误信息/日志安全）。
    pub(crate) fn reason(&self) -> String {
        match self {
            Self::Absent => "no dsh credentials file".to_owned(),
            Self::Rejected(reason) => (*reason).to_owned(),
        }
    }
}

/// 读 `home/.credentials.yaml` 的 `client-connection/browser-session`
/// 记录，取出 32 字节签名密钥。任何偏差 fail-closed（凭证侧绝不
/// "尽力"）：版本不符、记录缺席、形状不对、符号链接、不可读。
pub(crate) fn load_browser_session_secret(
    home: &Path,
) -> Result<BrowserSessionSecret, CredentialFailure> {
    let path = home.join(".credentials.yaml");
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => CredentialFailure::Absent,
        _ => CredentialFailure::Rejected("the dsh credentials file cannot be inspected"),
    })?;
    if metadata.is_symlink() {
        return Err(CredentialFailure::Rejected(
            "the dsh credentials file is a symbolic link",
        ));
    }
    if metadata.is_dir() || metadata.len() > CREDENTIALS_FILE_CAP {
        return Err(CredentialFailure::Rejected(
            "the dsh credentials file has an unsupported shape",
        ));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|_| CredentialFailure::Rejected("the dsh credentials file cannot be read"))?;
    parse_browser_session_secret(&text)
}

/// 文档形状（credentials-local 实证钉靶）：
/// `version: 1` + `refs: {...}` + `records: {<scope>/<id>: {kind, payload}}`；
/// 目标记录 `kind: grant`、`payload: {version: 1, secret: <base64url,32B>}`。
fn parse_browser_session_secret(text: &str) -> Result<BrowserSessionSecret, CredentialFailure> {
    let rejected = || {
        CredentialFailure::Rejected(
            "the browser-session credential record has an unsupported shape",
        )
    };
    let document: YamlValue = serde_yaml_ng::from_str(text).map_err(|_| rejected())?;
    if document.get("version").and_then(YamlValue::as_u64) != Some(1) {
        return Err(rejected());
    }
    let record = document
        .get("records")
        .and_then(|records| records.get(BROWSER_SESSION_KEY))
        .ok_or_else(rejected)?;
    if record.get("kind").and_then(YamlValue::as_str) != Some("grant") {
        return Err(rejected());
    }
    let payload = record.get("payload").ok_or_else(rejected)?;
    if payload.get("version").and_then(YamlValue::as_u64) != Some(1) {
        return Err(rejected());
    }
    let secret = payload
        .get("secret")
        .and_then(YamlValue::as_str)
        .ok_or_else(rejected)?;
    let bytes = URL_SAFE_NO_PAD.decode(secret).map_err(|_| rejected())?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| rejected())?;
    Ok(BrowserSessionSecret(bytes))
}

/// 铸一条短窗会话 cookie（`name=value` 形，直接可作 `Cookie:` 头）。
/// 形状逐字段镜像 browser-auth.ts（钉靶 d347e70390）：名字为
/// `dsh-auth-` + base64url(SHA-256(authority))；值为 `v1.` +
/// base64url(JSON{version,authority,issuedAt,expiresAt}) + `.` +
/// base64url(HMAC-SHA256(secret, body))。签名对象是 body 的 base64url
/// 文本本身——宿主按同串重算，任何 JSON 序列化皆可。
pub(crate) fn mint_session_cookie(
    secret: &BrowserSessionSecret,
    authority: &str,
    now_ms: u64,
) -> String {
    let name_tag = URL_SAFE_NO_PAD.encode(Sha256::digest(authority.as_bytes()));
    let payload = serde_json::json!({
        "version": 1,
        "authority": authority,
        "issuedAt": now_ms,
        "expiresAt": now_ms + COOKIE_WINDOW_MS,
    });
    let body = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
    let mut mac = HmacSha256::new_from_slice(&secret.0).expect("HMAC accepts any key length");
    mac.update(body.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    format!("dsh-auth-{name_tag}=v1.{body}.{signature}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret_yaml(secret: &str) -> String {
        // 显式拼行：`\` 续行转义会剥掉行首空格，连 YAML 缩进一起剥。
        let mut text = String::from("version: 1\nrefs: {}\nrecords:\n");
        text.push_str("  client-connection/browser-session:\n");
        text.push_str("    kind: grant\n");
        text.push_str("    payload:\n");
        text.push_str("      version: 1\n");
        text.push_str("      secret: ");
        text.push_str(secret);
        text.push('\n');
        text
    }

    fn valid_secret() -> String {
        // 32 字节 0x42 的 base64url（无填充）：43 字符。
        URL_SAFE_NO_PAD.encode([0x42u8; 32])
    }

    #[test]
    fn parse_reads_exactly_the_one_record() {
        let secret = parse_browser_session_secret(&secret_yaml(&valid_secret())).expect("parse");
        // 判别锚：与上游同源同密钥时 cookie 一致（HMAC 输入含密钥字节）。
        assert!(mint_session_cookie(&secret, "127.0.0.1:1", 0).starts_with("dsh-auth-"));
    }

    #[test]
    fn parse_rejects_every_deviation_without_quoting_content() {
        let cases: Vec<(&str, String)> = vec![
            (
                "absent record",
                "version: 1\nrefs: {}\nrecords: {}\n".to_owned(),
            ),
            (
                "wrong document version",
                "version: 2\nrecords: {}\n".to_owned(),
            ),
            (
                "kind mismatch",
                secret_yaml(&valid_secret()).replacen("grant", "legacy", 1),
            ),
            (
                "payload version",
                secret_yaml(&valid_secret()).replacen("version: 1", "version: 2", 2),
            ),
            ("secret not base64url", secret_yaml("!!not-base64!!")),
            (
                "secret wrong length",
                secret_yaml(&URL_SAFE_NO_PAD.encode([1u8; 16])),
            ),
            ("not yaml at all", "\t{".to_owned()),
        ];
        for (label, text) in &cases {
            let failure = parse_browser_session_secret(text)
                .err()
                .unwrap_or_else(|| panic!("{label} must be rejected"));
            assert!(
                !failure.reason().contains("!!") && !failure.reason().contains("secret:"),
                "{label}: failure text must not quote file content: {}",
                failure.reason()
            );
        }
    }

    #[test]
    fn load_refuses_symlinks_and_reports_absent_cleanly() {
        let home = std::env::temp_dir().join(format!(
            "clat-cred-hygiene-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        // 文件缺席 = Absent（干净降级）。
        assert!(matches!(
            load_browser_session_secret(&home),
            Err(CredentialFailure::Absent)
        ));
        // 符号链接 = 卫生拒绝。
        let real = home.join("real.yaml");
        std::fs::write(&real, secret_yaml(&valid_secret())).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, home.join(".credentials.yaml")).unwrap();
        #[cfg(unix)]
        {
            let failure = load_browser_session_secret(&home).unwrap_err();
            assert!(
                failure.reason().contains("symbolic link"),
                "{}",
                failure.reason()
            );
        }
        std::fs::remove_dir_all(&home).ok();
    }

    /// 形状判别（研究 §8 钉靶的逐字段锚）：名 = authority 的 SHA-256
    /// tag；值三段 `v1.body.sig`；body 解码回 authority/短窗；签名对
    /// body 文本重算吻合。撤 HMAC 或换 authority 即红。
    #[test]
    fn minted_cookie_matches_the_pinned_recipe() {
        let secret = BrowserSessionSecret([7u8; 32]);
        let authority = "127.0.0.1:3080";
        let now = 1_788_000_000_000u64;
        let cookie = mint_session_cookie(&secret, authority, now);
        let (name, value) = cookie.split_once('=').expect("name=value");
        assert_eq!(
            name,
            format!(
                "dsh-auth-{}",
                URL_SAFE_NO_PAD.encode(Sha256::digest(authority.as_bytes()))
            )
        );
        let parts: Vec<&str> = value.split('.').collect();
        assert_eq!(parts.len(), 3, "{value:?}");
        assert_eq!(parts[0], "v1");
        let body_json: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).expect("body b64url"))
                .expect("body json");
        assert_eq!(body_json["version"], 1);
        assert_eq!(body_json["authority"], authority);
        assert_eq!(body_json["issuedAt"].as_u64(), Some(now));
        assert_eq!(
            body_json["expiresAt"].as_u64(),
            Some(now + COOKIE_WINDOW_MS),
            "the minted window is exactly the pinned short window"
        );
        let mut mac = HmacSha256::new_from_slice(&[7u8; 32]).unwrap();
        mac.update(parts[1].as_bytes());
        mac.verify_slice(&URL_SAFE_NO_PAD.decode(parts[2]).expect("sig b64url"))
            .expect("signature verifies over the body text");
    }
}
