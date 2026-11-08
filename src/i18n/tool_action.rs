use super::UiMessage;

pub fn tool_action(tool: &str) -> UiMessage {
    let key = match tool {
        "web_search" | "web_search_prime" => "web.tool_action.search_web",
        "web_fetch" => "web.tool_action.read_web",
        "read_file" => "web.tool_action.read_file",
        "list_files" => "web.tool_action.list_files",
        "search" => "web.tool_action.search_files",
        // apply_patch 已于 2026-11-08 退役;该 arm 保留用于历史会话回放
        // 的展示映射(旧 journal 中的 tool/call 仍按名渲染)。
        "edit_file" | "apply_patch" => "web.tool_action.edit_file",
        "write_file" => "web.tool_action.write_file",
        "run_command" => "web.tool_action.run_command",
        "view_image" => "web.tool_action.view_image",
        "ask_user" | "ask_user_question" => "web.tool_action.ask_user",
        "update_plan" => "web.tool_action.update_plan",
        "exit_plan_mode" => "web.tool_action.finish_plan",
        "request_plan_mode" => "web.tool_action.request_plan_mode",
        "propose_goal" => "web.tool_action.propose_goal",
        _ => "web.tool_action.other",
    };
    let action = UiMessage::new(key);
    if tool.starts_with("mcp_") || tool.starts_with("plugin_") {
        UiMessage::new("web.tool_action.external").arg_message("action", action)
    } else {
        action
    }
}
