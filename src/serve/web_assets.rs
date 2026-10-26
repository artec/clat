//! 内嵌 web 资产（PWA-4）：`web/` 纯静态文件经 `include_bytes!` 手排
//! 清单编进单二进制（INV-W1/INV-S8：零新依赖——rust-embed 的目录遍历
//! 便利在资产个位数时不值一个依赖）。
//!
//! 静态 shell 不含凭据，可从固定的干净 URL 冷启动；API 与 SSE 仍在
//! 本地信任/远程凭证闸之后。资产不做运行期替换，因此 token 不可能进入
//! manifest、图标 URL、浏览历史或 referrer。

use std::borrow::Cow;

const ASSETS: &[(&str, &[u8], &str)] = &[
    (
        "/settings-pages.js",
        include_bytes!("../../web/settings-pages.js"),
        "application/javascript",
    ),
    (
        "/resource-library.js",
        include_bytes!("../../web/resource-library.js"),
        "application/javascript",
    ),
    (
        "/task-center.js",
        include_bytes!("../../web/task-center.js"),
        "application/javascript",
    ),
    (
        "/conversation-reader.js",
        include_bytes!("../../web/conversation-reader.js"),
        "application/javascript",
    ),
    (
        "/navigation-pages.js",
        include_bytes!("../../web/navigation-pages.js"),
        "application/javascript",
    ),
    (
        "/icons/clat.svg",
        include_bytes!("../../web/icons/clat.svg"),
        "image/svg+xml",
    ),
    (
        "/project-tree.js",
        include_bytes!("../../web/project-tree.js"),
        "application/javascript",
    ),
    (
        "/application-toolbar.js",
        include_bytes!("../../web/application-toolbar.js"),
        "application/javascript",
    ),
    (
        "/project-context.js",
        include_bytes!("../../web/project-context.js"),
        "application/javascript",
    ),
    (
        "/project-host.js",
        include_bytes!("../../web/project-host.js"),
        "application/javascript",
    ),
    (
        "/project-lifecycle.js",
        include_bytes!("../../web/project-lifecycle.js"),
        "application/javascript",
    ),
    (
        "/workbench-resize.js",
        include_bytes!("../../web/workbench-resize.js"),
        "application/javascript",
    ),
    (
        "/plan-artifact.js",
        include_bytes!("../../web/plan-artifact.js"),
        "application/javascript",
    ),
    (
        "/interaction-dock.js",
        include_bytes!("../../web/interaction-dock.js"),
        "application/javascript",
    ),
    (
        "/",
        include_bytes!("../../web/index.html"),
        "text/html; charset=utf-8",
    ),
    (
        "/app.js",
        include_bytes!("../../web/app.js"),
        "application/javascript",
    ),
    (
        "/workbench-state.js",
        include_bytes!("../../web/workbench-state.js"),
        "application/javascript",
    ),
    (
        "/workbench-shell.js",
        include_bytes!("../../web/workbench-shell.js"),
        "application/javascript",
    ),
    (
        "/plugin-market.js",
        include_bytes!("../../web/plugin-market.js"),
        "application/javascript",
    ),
    (
        "/next-turn.js",
        include_bytes!("../../web/next-turn.js"),
        "application/javascript",
    ),
    (
        "/composer-ux.js",
        include_bytes!("../../web/composer-ux.js"),
        "application/javascript",
    ),
    (
        "/command-picker.js",
        include_bytes!("../../web/command-picker.js"),
        "application/javascript",
    ),
    (
        "/conversation-find.js",
        include_bytes!("../../web/conversation-find.js"),
        "application/javascript",
    ),
    (
        "/workspace-review.js",
        include_bytes!("../../web/workspace-review.js"),
        "application/javascript",
    ),
    (
        "/turn-review.js",
        include_bytes!("../../web/turn-review.js"),
        "application/javascript",
    ),
    (
        "/file-browser.js",
        include_bytes!("../../web/file-browser.js"),
        "application/javascript",
    ),
    (
        "/session-organization.js",
        include_bytes!("../../web/session-organization.js"),
        "application/javascript",
    ),
    (
        "/workflow-details.js",
        include_bytes!("../../web/workflow-details.js"),
        "application/javascript",
    ),
    (
        "/navigation-help.js",
        include_bytes!("../../web/navigation-help.js"),
        "application/javascript",
    ),
    (
        "/task-review.js",
        include_bytes!("../../web/task-review.js"),
        "application/javascript",
    ),
    (
        "/background-jobs.js",
        include_bytes!("../../web/background-jobs.js"),
        "application/javascript",
    ),
    (
        "/scheduled-work.js",
        include_bytes!("../../web/scheduled-work.js"),
        "application/javascript",
    ),
    (
        "/history-query.js",
        include_bytes!("../../web/history-query.js"),
        "application/javascript",
    ),
    (
        "/web-settings.js",
        include_bytes!("../../web/web-settings.js"),
        "application/javascript",
    ),
    (
        "/style.css",
        include_bytes!("../../web/style.css"),
        "text/css; charset=utf-8",
    ),
    (
        "/workbench.css",
        include_bytes!("../../web/workbench.css"),
        "text/css; charset=utf-8",
    ),
    (
        "/manifest.webmanifest",
        include_bytes!("../../web/manifest.webmanifest"),
        "application/manifest+json",
    ),
    (
        "/icons/icon-192.png",
        include_bytes!("../../web/icons/icon-192.png"),
        "image/png",
    ),
    (
        "/icons/icon-512.png",
        include_bytes!("../../web/icons/icon-512.png"),
        "image/png",
    ),
    (
        "/locales/en.json",
        include_bytes!("../../locales/en.json"),
        "application/json",
    ),
    (
        "/locales/zh-Hans.json",
        include_bytes!("../../locales/zh-Hans.json"),
        "application/json",
    ),
    (
        "/locales/zh-Hant.json",
        include_bytes!("../../locales/zh-Hant.json"),
        "application/json",
    ),
];

// Source modules keep their zero-build authoring surface. A normal function
// gives each mounted project lexical ownership without eval, imports or frames.
const PROJECT_MODULES: &[&str] = &[
    "/navigation-pages.js",
    "/settings-pages.js",
    "/resource-library.js",
    "/task-center.js",
    "/conversation-reader.js",
    "/composer-ux.js",
    "/next-turn.js",
    "/command-picker.js",
    "/conversation-find.js",
    "/workspace-review.js",
    "/turn-review.js",
    "/file-browser.js",
    "/session-organization.js",
    "/workflow-details.js",
    "/navigation-help.js",
    "/task-review.js",
    "/plugin-market.js",
    "/background-jobs.js",
    "/scheduled-work.js",
    "/history-query.js",
    "/web-settings.js",
    "/interaction-dock.js",
    "/plan-artifact.js",
    "/workbench-resize.js",
    "/application-toolbar.js",
    "/workbench-shell.js",
    "/project-lifecycle.js",
    "/app.js",
];

fn project_bundle() -> &'static [u8] {
    static BUNDLE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    BUNDLE.get_or_init(|| {
        let mut bytes = b"globalThis.CLATProjectWorkbench = function(context) {\n'use strict';\nconst {document, window} = context;\n".to_vec();
        for route in PROJECT_MODULES {
            let (_, source, _) = ASSETS.iter().find(|(path, _, _)| path == route)
                .expect("project source is an embedded asset");
            bytes.extend_from_slice(source);
            bytes.push(b'\n');
        }
        #[cfg(test)]
        bytes.extend_from_slice(include_bytes!("../../web/e2e/project-test-ports.js"));
        bytes.extend_from_slice(b"\nreturn projectLifecycle();\n};\n");
        bytes
    })
}

/// Public static bytes; authentication remains on the API and event routes.
pub(crate) fn asset(path: &str) -> Option<(Cow<'static, [u8]>, &'static str)> {
    if path == "/project-workbench.js" {
        return Some((Cow::Borrowed(project_bundle()), "application/javascript"));
    }
    if path == "/i18n.js" {
        return Some((
            Cow::Owned(crate::i18n::browser_runtime().into_bytes()),
            "application/javascript",
        ));
    }
    ASSETS
        .iter()
        .find(|(route, _, _)| *route == path)
        .map(|(_, bytes, content_type)| (Cow::Borrowed(*bytes), *content_type))
}
