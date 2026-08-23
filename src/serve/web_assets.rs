//! 内嵌 web 资产（PWA-4）：`web/` 纯静态文件经 `include_bytes!` 手排
//! 清单编进单二进制（INV-W1/INV-S8：零新依赖——rust-embed 的目录遍历
//! 便利在资产个位数时不值一个依赖）。
//!
//! 静态 shell 不含凭据，可从固定的干净 URL 冷启动；API 与 SSE 仍在
//! Bearer token 闸之后。资产不做运行期替换，因此 token 不可能进入
//! manifest、图标 URL、浏览历史或 referrer。

use std::borrow::Cow;

const INDEX: &[u8] = include_bytes!("../../web/index.html");
const APP_JS: &[u8] = include_bytes!("../../web/app.js");
const STYLE_CSS: &[u8] = include_bytes!("../../web/style.css");
const MANIFEST: &[u8] = include_bytes!("../../web/manifest.webmanifest");
const ICON_192: &[u8] = include_bytes!("../../web/icons/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../../web/icons/icon-512.png");

/// GET 资产表：`(字节, content-type)`；未知路径 `None`（404）。
pub(crate) fn asset(path: &str) -> Option<(Cow<'static, [u8]>, &'static str)> {
    match path {
        "/" => Some((Cow::Borrowed(INDEX), "text/html; charset=utf-8")),
        "/app.js" => Some((Cow::Borrowed(APP_JS), "application/javascript")),
        "/style.css" => Some((Cow::Borrowed(STYLE_CSS), "text/css; charset=utf-8")),
        "/manifest.webmanifest" => Some((Cow::Borrowed(MANIFEST), "application/manifest+json")),
        "/icons/icon-192.png" => Some((Cow::Borrowed(ICON_192), "image/png")),
        "/icons/icon-512.png" => Some((Cow::Borrowed(ICON_512), "image/png")),
        _ => None,
    }
}
