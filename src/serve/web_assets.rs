//! 内嵌 web 资产（PWA-4）：`web/` 纯静态文件经 `include_bytes!` 手排
//! 清单编进单二进制（INV-W1/INV-S8：零新依赖——rust-embed 的目录遍历
//! 便利在资产个位数时不值一个依赖）。
//!
//! manifest 的 `{TOKEN}` 占位符在 **token 闸验证通过后**按已验证的
//! query token 替换（`start_url`/`icons[].src`）——浏览器安装 PWA 时
//! 自行拉取 manifest 与图标且不能带 Authorization 头，占位符方案让
//! 安装链路走 query token 过闸，INV-S1 对全部资产原样生效。

use std::borrow::Cow;

/// index 的子资源引用同样带 `{TOKEN}` 占位符：浏览器按字面 URL 拉取
/// css/js/图标（不带 query 就过不了闸），与 manifest 同一替换路径。
const INDEX_TEMPLATE: &str = include_str!("../../web/index.html");
const APP_JS: &[u8] = include_bytes!("../../web/app.js");
const STYLE_CSS: &[u8] = include_bytes!("../../web/style.css");
const MANIFEST_TEMPLATE: &str = include_str!("../../web/manifest.webmanifest");
const ICON_192: &[u8] = include_bytes!("../../web/icons/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../../web/icons/icon-512.png");

/// 已验证 token 的 manifest 占位符。
const TOKEN_PLACEHOLDER: &str = "{TOKEN}";

/// GET 资产表：`(字节, content-type)`；未知路径 `None`（404）。
/// 调用方必须在三闸全部通过后才可调用（manifest 替换依赖已验证
/// token——这不是鉴权，是让安装链路的图标 URL 携带同一 token）。
pub(crate) fn asset(
    path: &str,
    verified_token: &str,
) -> Option<(Cow<'static, [u8]>, &'static str)> {
    match path {
        "/" => Some((
            Cow::Owned(
                INDEX_TEMPLATE
                    .replace(TOKEN_PLACEHOLDER, verified_token)
                    .into_bytes(),
            ),
            "text/html; charset=utf-8",
        )),
        "/app.js" => Some((Cow::Borrowed(APP_JS), "application/javascript")),
        "/style.css" => Some((Cow::Borrowed(STYLE_CSS), "text/css; charset=utf-8")),
        "/manifest.webmanifest" => Some((
            Cow::Owned(
                MANIFEST_TEMPLATE
                    .replace(TOKEN_PLACEHOLDER, verified_token)
                    .into_bytes(),
            ),
            "application/manifest+json",
        )),
        "/icons/icon-192.png" => Some((Cow::Borrowed(ICON_192), "image/png")),
        "/icons/icon-512.png" => Some((Cow::Borrowed(ICON_512), "image/png")),
        _ => None,
    }
}
