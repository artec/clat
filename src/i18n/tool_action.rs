use super::UiMessage;

pub fn tool_effect(effect: crate::ToolEffect) -> UiMessage {
    let key = match effect {
        crate::ToolEffect::Pure => "remote.effect_pure",
        crate::ToolEffect::Read => "remote.effect_read",
        crate::ToolEffect::Write => "remote.effect_write",
        crate::ToolEffect::Execute => "remote.effect_execute",
        crate::ToolEffect::Network => "remote.effect_network",
        crate::ToolEffect::ExternalRead => "remote.effect_external_read",
        crate::ToolEffect::Destructive => "remote.effect_destructive",
        crate::ToolEffect::SessionWrite => "remote.effect_session_write",
        crate::ToolEffect::UIStructureRead => "remote.effect_ui_structure_read",
        crate::ToolEffect::UIAction => "remote.effect_ui_action",
    };
    UiMessage::new(key)
}

pub fn tool_action(tool: &str) -> UiMessage {
    let host_action = tool
        .rsplit_once(":host:")
        .map(|(_, name)| name)
        .filter(|name| matches!(*name, "ui_apps" | "ui_tree" | "ui_wait" | "ui_act"));
    let key = match host_action.unwrap_or(tool) {
        "web_search" | "web_search_prime" => "web.tool_action.search_web",
        "web_fetch" => "web.tool_action.read_web",
        "read_file" => "web.tool_action.read_file",
        "list_files" => "web.tool_action.list_files",
        "ui_apps" => "web.tool_action.ui_apps",
        "ui_tree" => "web.tool_action.ui_tree",
        "ui_wait" => "web.tool_action.ui_wait",
        "ui_act" => "web.tool_action.ui_act",
        "search" => "web.tool_action.search_files",
        // apply_patch 已于 2026-11-08 退役;该 arm 保留用于历史会话回放
        // 的展示映射(旧 journal 中的 tool/call 仍按名渲染)。
        "edit_file" | "apply_patch" => "web.tool_action.edit_file",
        "write_file" | "acp.write_text_file" => "web.tool_action.write_file",
        "run_command" | "acp.terminal" => "web.tool_action.run_command",
        "view_image" => "web.tool_action.view_image",
        "ask_user" | "ask_user_question" => "web.tool_action.ask_user",
        "update_plan" => "web.tool_action.update_plan",
        "exit_plan_mode" => "web.tool_action.finish_plan",
        "request_plan_mode" => "web.tool_action.request_plan_mode",
        "propose_goal" => "web.tool_action.propose_goal",
        _ => "web.tool_action.other",
    };
    let action = UiMessage::new(key);
    if host_action.is_some() || tool.starts_with("mcp_") || tool.starts_with("plugin_") {
        UiMessage::new("web.tool_action.external").arg_message("action", action)
    } else {
        action
    }
}
