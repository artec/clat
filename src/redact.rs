//! Secrets redaction for messages that reach display or persistence
//! surfaces (B6 / C3①，2026-08-22).
//!
//! # 不变量（INV-K1）
//!
//! MCP server stderr tails and provider error bodies may carry secrets
//! (a crashing server that prints its own environment; an API error
//! body echoing the offending key). Before such text enters **any**
//! display or persistent surface — the `/mcp` panel, status flashes,
//! `clat exec` stdout, `RunFailed` messages, session-journal
//! `turn/end` payloads — it must pass through [`redact_secrets`].
//! The single-point chokeholds are `format_stderr_tail_public`
//! (mcp/client.rs) and the two `extract_error_message` implementations
//! (providers/openai*.rs); every downstream reader inherits the
//! redaction from there. Adding a *new* producer that embeds raw
//! third-party text in an error must route through this function too.
//!
//! The scanner is deliberately hand-rolled (no regex dependency) and
//! conservative: it only rewrites token-shaped runs introduced by a
//! `bearer` marker, an `sk-` prefix, or a key-ish `name=value`
//! assignment — plain prose mentioning short words like "bearer token"
//! is left untouched. Coverage is hygiene, not a security boundary:
//! exotic spellings (quoted colon forms, split tokens) can still pass
//! through; the goal is that realistic leaks (env dumps, Authorization
//! headers, provider key echoes) do not persist.

/// `name=value` 赋值里视为密钥名的标识符后缀（大小写不敏感）。
/// 带前缀的变量名也命中：`Z_AI_API_KEY`、`OPENAI_API_KEY` 都以
/// `api_key` 结尾。误伤面（如 `monkey=…`）偏向多脱敏，方向安全。
const KEYISH_SUFFIXES: &[&str] = &[
    "api_key",
    "apikey",
    "api-key",
    "access_token",
    "token",
    "secret",
    "password",
    "key",
];

/// 赋值 / bearer 后随值的最小长度——短于它的多半是散文或标志位。
const MIN_VALUE_CHARS: usize = 8;
/// `sk-` 前缀 token 的最小长度（`sk-` + 至少 10 位主体）。
const MIN_SK_CHARS: usize = 12;
const REDACTED: &str = "[REDACTED]";

/// token 值的字节跨度（尾部句点保留不脱——句子末尾的句号不属于密钥）。
fn token_span(text: &str) -> usize {
    let full = text
        .bytes()
        .take_while(|byte| {
            (*byte as char).is_ascii_alphanumeric()
                || matches!(*byte, b'_' | b'-' | b'.' | b'=' | b'/' | b'+')
        })
        .count();
    let mut end = full;
    while end > 0 && text.as_bytes()[end - 1] == b'.' {
        end -= 1;
    }
    end
}

fn starts_with_ignore_case(text: &str, needle: &str) -> bool {
    let mut text_chars = text.chars();
    needle.chars().all(|needle_char| {
        text_chars
            .next()
            .is_some_and(|c| c.eq_ignore_ascii_case(&needle_char))
    })
}

/// 是否 token 起点。阻断字符是"词中"字符（字母数字/`_`/`-`/`.`）；
/// `=` 是赋值分隔符，其后的标记仍是起点（`header=Bearer …`）。
fn at_token_start(output: &str) -> bool {
    output
        .chars()
        .next_back()
        .is_none_or(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
}

/// `=` 前的标识符是否密钥名（按后缀表匹配；回看仅越过的 ASCII 字节，
/// 不会切开多字节字符）。
fn keyish_name_before(output: &str) -> bool {
    let bytes = output.as_bytes();
    let mut start = bytes.len();
    while start > 0
        && (bytes[start - 1].is_ascii_alphanumeric() || matches!(bytes[start - 1], b'_' | b'-'))
    {
        start -= 1;
    }
    if start == bytes.len() {
        return false;
    }
    let name = output[start..].to_ascii_lowercase();
    KEYISH_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

/// INV-K1 的单点脱敏：`Bearer <token>` 的 token、`sk-` 前缀长 token、
/// `…api_key=<value>` 类赋值的值——各自替换为 `[REDACTED]`（标记与
/// 尾随句点保留）。无密文本零变化。
pub(crate) fn redact_secrets(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(offset) = rest.find(|c: char| c.is_ascii_alphabetic() || c == '=') {
        let (head, tail) = rest.split_at(offset);
        output.push_str(head);
        rest = tail;
        let mut characters = rest.chars();
        let first = characters.next().expect("rest starts with the found char");

        if first == '=' {
            let keyish = keyish_name_before(&output);
            output.push('=');
            rest = characters.as_str();
            if keyish {
                let span = token_span(rest);
                if span >= MIN_VALUE_CHARS {
                    output.push_str(REDACTED);
                    rest = &rest[span..];
                }
            }
            continue;
        }

        // sk- 前缀 token（如 sk-proj-…、sk-or-v1-…）。
        if at_token_start(&output)
            && token_span(rest) >= MIN_SK_CHARS
            && starts_with_ignore_case(rest, "sk-")
        {
            let span = token_span(rest);
            output.push_str(REDACTED);
            rest = &rest[span..];
            continue;
        }

        // bearer 标记：`Bearer <token>`（token 前允许空白）。
        if at_token_start(&output) && starts_with_ignore_case(rest, "bearer") {
            output.push_str(&rest[.."bearer".len()]);
            rest = &rest["bearer".len()..];
            let skipped = rest
                .bytes()
                .take_while(|byte| *byte == b' ' || *byte == b'\t')
                .count();
            output.push_str(&rest[..skipped]);
            rest = &rest[skipped..];
            let span = token_span(rest);
            if span >= MIN_VALUE_CHARS {
                output.push_str(REDACTED);
                rest = &rest[span..];
            }
            continue;
        }

        // 无标记命中：跳过这个字符继续扫。
        output.push(first);
        rest = characters.as_str();
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_authorization_headers_are_redacted() {
        assert_eq!(
            redact_secrets("Authorization: Bearer sk-abc123def457"),
            "Authorization: Bearer [REDACTED]"
        );
        assert_eq!(
            redact_secrets("bearer\ttok_1234567890 tail"),
            "bearer\t[REDACTED] tail"
        );
        // JWT 形态（含内部句点）整体脱敏，尾随句点保留。
        assert_eq!(
            redact_secrets("auth ok. header=Bearer aaaa.bbbb.cccc. done"),
            "auth ok. header=Bearer [REDACTED]. done"
        );
        // 散文里的短词不是 token，零误伤。
        assert_eq!(
            redact_secrets("the bearer token was rejected"),
            "the bearer token was rejected"
        );
    }

    #[test]
    fn sk_prefixed_tokens_are_redacted_anywhere() {
        assert_eq!(
            redact_secrets(
                "Incorrect API key provided: sk-proj-abcdefghijklmnopqrs. You can find..."
            ),
            "Incorrect API key provided: [REDACTED]. You can find..."
        );
        assert_eq!(
            redact_secrets("key SK-OR-V1-0123456789abcdef"),
            "key [REDACTED]"
        );
        // 太短的 sk- run（不达 token 形状）不动。
        assert_eq!(redact_secrets("prices sk-1 only"), "prices sk-1 only");
    }

    #[test]
    fn keyish_assignments_are_redacted() {
        // 带前缀的变量名命中后缀表（env dump 的真实形态）。
        assert_eq!(
            redact_secrets("Z_AI_API_KEY=abcdef0123456789 path=/bin"),
            "Z_AI_API_KEY=[REDACTED] path=/bin"
        );
        assert_eq!(
            redact_secrets("OPENAI_API_KEY: set OPENAI_API_KEY=sk-live-0123456789"),
            "OPENAI_API_KEY: set OPENAI_API_KEY=[REDACTED]"
        );
        // URL query 里的 token 同样是密钥。
        assert_eq!(
            redact_secrets("GET https://x.test/v1?token=abcdef01234567&n=1"),
            "GET https://x.test/v1?token=[REDACTED]&n=1"
        );
        // 短值（标志位/散文）不动；非密钥名不动。
        assert_eq!(redact_secrets("token=1 key=abc"), "token=1 key=abc");
        assert_eq!(
            redact_secrets("path=/bin width=1024"),
            "path=/bin width=1024"
        );
        // v1 边界（文档化）：冒号+引号形态不在 '=' 赋值覆盖内。
        assert_eq!(
            redact_secrets("api-key: \"deadbeefcafe1234\""),
            "api-key: \"deadbeefcafe1234\""
        );
    }

    #[test]
    fn ordinary_text_passes_through_unchanged() {
        let samples = [
            "server started on port 8080",
            "TypeError: cannot read property 'id' of undefined",
            "connection reset by peer 127.0.0.1:9",
            "warn: retrying in 2s (attempt 3/5)",
            "中文日志不被改写",
            "",
        ];
        for sample in samples {
            assert_eq!(redact_secrets(sample), sample);
        }
    }
}
