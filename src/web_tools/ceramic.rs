//! 共享 Ceramic 搜索兜底(2026-11-08 负责人令):未配置 DeepSeek web key
//! 时,原生 `web_search` 改走项目方运营的代理 `search.dengcb.net`,
//! 原匿名 DuckDuckGo Lite 抓取同批移除。代理要求 Bearer 口令,而口令
//! 明文绝不进开源仓库:CLAT 运行时从同源 `GET /token` 取 AES-256-GCM
//! 加密 blob,用嵌在本模块的两段 16 字节密钥拼合出的 32 字节密钥解密
//! (密钥随开源可见——这是防脚本小子的混淆,不是安全边界;真正的防线
//! 是代理侧限流与固定额度),仅在内存缓存。轮换无需发版:更新 blob
//! 后,客户端在下一次 401 或 TTL 到期时自动重取。
use super::provider::validate_url;
use super::*;
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

const CERAMIC_BASE: &str = "https://search.dengcb.net";
const TOKEN_PATH: &str = "/token";
const SEARCH_PATH: &str = "/search";
/// Workers websearch 单请求 `limit` 上限 10;工具入参 1–50 在此收口。
const CERAMIC_MAX_RESULTS: u64 = 10;
/// 口令缓存 TTL:过期或 401 作废后重取,支持服务端轮换。
const TOKEN_TTL: Duration = Duration::from_secs(60 * 60);
/// 加密 blob 上限(nonce + 短口令密文 + tag 远小于此;防异常端点撑内存)。
const TOKEN_BLOB_MAX: usize = 64 * 1024;
/// AES-256 密钥的两半,运行时拼接——二进制中不存在连续 32 字节密钥
/// 字面量。拼接结果只记录在 local-only 的 docs/research 文档里。
const KEY_A: [u8; 16] = [
    0x1d, 0xd7, 0xf0, 0x55, 0x68, 0xef, 0x9c, 0xc8, 0x57, 0x47, 0x72, 0x76, 0x6f, 0x16, 0x23, 0x26,
];
const KEY_B: [u8; 16] = [
    0xf0, 0x0f, 0xa2, 0x37, 0xf0, 0x0f, 0x06, 0x3e, 0x76, 0x28, 0x85, 0x0d, 0x68, 0x7f, 0x35, 0x1c,
];

/// 进程内共享口令缓存。不落盘、不进 journal;解密失败即显式报错。
#[derive(Default)]
pub(super) struct SharedToken(Mutex<Option<(Instant, String)>>);
impl SharedToken {
    fn cached(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(at, _)| at.elapsed() < TOKEN_TTL)
            .map(|(_, token)| token.clone())
    }
    fn store(&self, token: String) {
        *self.0.lock().unwrap() = Some((Instant::now(), token));
    }
    fn invalidate(&self) {
        *self.0.lock().unwrap() = None;
    }
}

fn assembled_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(&KEY_A);
    key[16..].copy_from_slice(&KEY_B);
    key
}

/// blob 布局:base64(nonce[12] ‖ 密文 ‖ tag[16])。GCM 自带完整性——
/// 篡改/截断/非 UTF-8/空口令一律 `Err(())`,由调用方映射为显式错误。
fn decrypt_blob(body: &[u8]) -> Result<String, ()> {
    use base64::Engine as _;
    use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey};
    let text = std::str::from_utf8(body).map_err(|_| ())?.trim();
    let blob = base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| ())?;
    if blob.len() < 12 + 16 + 1 {
        return Err(());
    }
    let (nonce, ciphertext) = blob.split_at(12);
    let mut buffer = ciphertext.to_vec();
    let key = LessSafeKey::new(
        UnboundKey::new(&ring::aead::AES_256_GCM, &assembled_key()).map_err(|_| ())?,
    );
    let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| ())?;
    let plain = key
        .open_in_place(nonce, Aad::empty(), &mut buffer)
        .map_err(|_| ())?;
    let token = String::from_utf8(plain.to_vec()).map_err(|_| ())?;
    (!token.is_empty()).then_some(token).ok_or(())
}

/// 测试专用加密助手(与生产同一密钥),供 Wire 夹具构造合法 blob。
/// 固定 nonce 仅在测试可接受——生产 blob 的 nonce 由负责人端脚本随机。
#[cfg(test)]
pub(super) fn test_blob(token: &str) -> String {
    use base64::Engine as _;
    use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey};
    let key =
        LessSafeKey::new(UnboundKey::new(&ring::aead::AES_256_GCM, &assembled_key()).unwrap());
    let mut buffer = token.as_bytes().to_vec();
    let nonce = Nonce::assume_unique_for_key([7u8; 12]);
    key.seal_in_place_append_tag(nonce, Aad::empty(), &mut buffer)
        .unwrap();
    let mut blob = [7u8; 12].to_vec();
    blob.extend_from_slice(&buffer);
    base64::engine::general_purpose::STANDARD.encode(blob)
}

impl WebService {
    /// 取共享口令:缓存新鲜直接用;否则 GET /token(走同一 WebTransport,
    /// 自动继承公网地址校验、DNS pin、超时与字节上限)→ 解密 → 缓存。
    fn shared_token(
        &self,
        req_cancel: &CancelToken,
        run_cancel: &CancelToken,
    ) -> Result<String, WebError> {
        if let Some(token) = self.shared.cached() {
            return Ok(token);
        }
        check(req_cancel)?;
        let response = self.consent_execute(
            HttpRequest {
                url: format!("{CERAMIC_BASE}{TOKEN_PATH}"),
                method: "GET",
                body: Vec::new(),
                timeout: TIMEOUT,
                consented: Vec::new(),
                headers: vec![("accept".into(), "text/plain".into())],
            },
            req_cancel,
            run_cancel,
        )?;
        check(req_cancel)?;
        if response.status != 200 || response.body.len() > TOKEN_BLOB_MAX {
            return Err(
                WebError::new("shared_token_failed").detail(format!("HTTP {}", response.status))
            );
        }
        let token =
            decrypt_blob(&response.body).map_err(|_| WebError::new("shared_token_failed"))?;
        self.shared.store(token.clone());
        Ok(token)
    }

    /// 无 key 兜底搜索。401 → 作废缓存 → 重取一次 → 重试一次;二次 401
    /// 视为口令失效(服务端轮换后 blob 未更新的窗口),显式报错不循环。
    pub(super) fn search_shared(
        &self,
        query: &str,
        count: u64,
        req_cancel: &CancelToken,
        run_cancel: &CancelToken,
    ) -> Result<Value, WebError> {
        let limit = count.min(CERAMIC_MAX_RESULTS);
        let body = json!({"query": query, "limit": limit});
        let token = self.shared_token(req_cancel, run_cancel)?;
        let response = self.ceramic_post_consented(&token, &body, req_cancel, run_cancel)?;
        let response = if response.status == 401 {
            self.shared.invalidate();
            let refreshed = self.shared_token(req_cancel, run_cancel)?;
            let retry = self.ceramic_post_consented(&refreshed, &body, req_cancel, run_cancel)?;
            if retry.status == 401 {
                return Err(WebError::new("credential_invalid").detail("HTTP 401"));
            }
            retry
        } else {
            response
        };
        shared_search_response(response, limit)
    }

    /// POST /search 同样过同意通道(域名被 TUN fake-IP 解析时,这里才是
    /// 第一个被拦的调用)。401 重试由 search_shared 编排,本方法单跳。
    fn ceramic_post_consented(
        &self,
        token: &str,
        body: &Value,
        req_cancel: &CancelToken,
        run_cancel: &CancelToken,
    ) -> Result<HttpResponse, WebError> {
        check(req_cancel)?;
        self.consent_execute(
            HttpRequest {
                url: format!("{CERAMIC_BASE}{SEARCH_PATH}"),
                method: "POST",
                body: serde_json::to_vec(body).unwrap(),
                timeout: TIMEOUT,
                consented: Vec::new(),
                headers: vec![
                    ("authorization".into(), format!("Bearer {token}")),
                    ("content-type".into(), "application/json".into()),
                ],
            },
            req_cancel,
            run_cancel,
        )
    }
}

/// 响应解析与状态映射,独立成自由函数保持每段 <80 行。
fn shared_search_response(response: HttpResponse, limit: u64) -> Result<Value, WebError> {
    if response.body.len() > MAX_BYTES {
        return Err(WebError::new("too_large"));
    }
    if response.status != 200 {
        return Err(WebError::new(match response.status {
            429 => "rate_limit",
            _ => "provider_failed",
        })
        .detail(format!("HTTP {}", response.status)));
    }
    let value: Value =
        serde_json::from_slice(&response.body).map_err(|_| WebError::new("invalid_response"))?;
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .filter(|v| v.len() <= 50)
        .ok_or_else(|| WebError::new("invalid_response"))?;
    let mut sources = Vec::new();
    for item in items.iter().take(limit as usize) {
        let url = item
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| WebError::new("invalid_response"))?;
        validate_url(url)?;
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(256)
            .collect::<String>();
        let description = item
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        let (text, truncated) = bounded(description);
        sources.push(json!({"source": source(url, &title), "text": text, "truncated": truncated}));
    }
    Ok(json!({"provider":"ceramic","sources":sources,"external_untrusted":true}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_roundtrip_and_rejects_tampering() {
        let blob = test_blob("secret-token");
        assert_eq!(decrypt_blob(blob.as_bytes()).unwrap(), "secret-token");
        // 篡改一个密文字节 → GCM tag 校验失败。
        let mut tampered = blob.clone();
        let mid = tampered.len() / 2;
        tampered.replace_range(
            mid..mid + 1,
            if &tampered[mid..mid + 1] == "A" {
                "B"
            } else {
                "A"
            },
        );
        assert!(decrypt_blob(tampered.as_bytes()).is_err());
        // 非 base64 / 过短 / 空 token 一律拒绝。
        assert!(decrypt_blob(b"not base64 !!!").is_err());
        assert!(decrypt_blob(b"AAAA").is_err());
    }
}
