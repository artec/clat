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
const COMPOSER_UX: &[u8] = include_bytes!("../../web/composer-ux.js");
const COMMAND_PICKER: &[u8] = include_bytes!("../../web/command-picker.js");
const CONVERSATION_FIND: &[u8] = include_bytes!("../../web/conversation-find.js");
const WORKSPACE_REVIEW: &[u8] = include_bytes!("../../web/workspace-review.js");
const TURN_REVIEW: &[u8] = include_bytes!("../../web/turn-review.js");
const FILE_BROWSER: &[u8] = include_bytes!("../../web/file-browser.js");
const SESSION_ORGANIZATION: &[u8] = include_bytes!("../../web/session-organization.js");
const WORKFLOW_DETAILS: &[u8] = include_bytes!("../../web/workflow-details.js");
const NAVIGATION_HELP: &[u8] = include_bytes!("../../web/navigation-help.js");
const TASK_REVIEW: &[u8] = include_bytes!("../../web/task-review.js");
const STYLE_CSS: &[u8] = include_bytes!("../../web/style.css");
const MANIFEST: &[u8] = include_bytes!("../../web/manifest.webmanifest");
const ICON_192: &[u8] = include_bytes!("../../web/icons/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../../web/icons/icon-512.png");

/// GET 资产表：`(字节, content-type)`；未知路径 `None`（404）。
pub(crate) fn asset(path: &str) -> Option<(Cow<'static, [u8]>, &'static str)> {
    match path {
        "/" => Some((Cow::Borrowed(INDEX), "text/html; charset=utf-8")),
        "/app.js" => Some((Cow::Borrowed(APP_JS), "application/javascript")),
        "/composer-ux.js" => Some((Cow::Borrowed(COMPOSER_UX), "application/javascript")),
        "/command-picker.js" => Some((Cow::Borrowed(COMMAND_PICKER), "application/javascript")),
        "/conversation-find.js" => {
            Some((Cow::Borrowed(CONVERSATION_FIND), "application/javascript"))
        }
        "/style.css" => Some((Cow::Borrowed(STYLE_CSS), "text/css; charset=utf-8")),
        "/task-review.js" => Some((Cow::Borrowed(TASK_REVIEW), "application/javascript")),
        "/navigation-help.js" => Some((Cow::Borrowed(NAVIGATION_HELP), "application/javascript")),
        "/workflow-details.js" => Some((Cow::Borrowed(WORKFLOW_DETAILS), "application/javascript")),
        "/workspace-review.js" => Some((Cow::Borrowed(WORKSPACE_REVIEW), "application/javascript")),
        "/turn-review.js" => Some((Cow::Borrowed(TURN_REVIEW), "application/javascript")),
        "/file-browser.js" => Some((Cow::Borrowed(FILE_BROWSER), "application/javascript")),
        "/session-organization.js" => Some((
            Cow::Borrowed(SESSION_ORGANIZATION),
            "application/javascript",
        )),
        "/manifest.webmanifest" => Some((Cow::Borrowed(MANIFEST), "application/manifest+json")),
        "/icons/icon-192.png" => Some((Cow::Borrowed(ICON_192), "image/png")),
        "/icons/icon-512.png" => Some((Cow::Borrowed(ICON_512), "image/png")),
        _ => None,
    }
}
