//! Native subtitle presentation, retaining the menu item's action and keyboard behavior.
use super::*;
use objc2::{AnyThread, runtime::NSObjectProtocol, sel};
use objc2_app_kit::{NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSMenu};
use objc2_foundation::{NSMutableAttributedString, NSRange, NSString};

pub(super) fn style(
    app: &tauri::AppHandle,
    menu: &Menu<tauri::Wry>,
    rows: Vec<Recent>,
) -> tauri::Result<()> {
    let menu = menu.clone();
    app.run_on_main_thread(move || {
        // Tauri is pinned. Access through its ContextMenu bound only on the main thread;
        // the cloned Tauri menu owns the native pointer for this entire closure.
        fn native(menu: &impl tauri::menu::ContextMenu) -> &NSMenu {
            unsafe { &*menu.inner_context().ns_menu().cast::<NSMenu>() }
        }
        let native = native(&menu);
        for (index, row) in rows.iter().enumerate() {
            let Some(item) = native.itemAtIndex(index as isize + 1) else {
                continue;
            };
            let project = NSString::from_str(&project_name(row));
            if item.respondsToSelector(sel!(setSubtitle:)) {
                item.setSubtitle(Some(&project));
            } else {
                // macOS 13 predates NSMenuItem.subtitle. Use a two-line attributed title.
                let title = item.title().to_string();
                let value = NSMutableAttributedString::initWithString(
                    NSMutableAttributedString::alloc(),
                    &NSString::from_str(&format!("{title}\n{project}")),
                );
                let range = NSRange::new(title.encode_utf16().count() + 1, project.length());
                unsafe {
                    value.addAttribute_value_range(
                        NSFontAttributeName,
                        &NSFont::systemFontOfSize(11.0),
                        range,
                    );
                    value.addAttribute_value_range(
                        NSForegroundColorAttributeName,
                        &NSColor::secondaryLabelColor(),
                        range,
                    );
                }
                item.setAttributedTitle(Some(&value));
            }
        }
    })
}
