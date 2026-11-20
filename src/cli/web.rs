use clat::client_ports::{
    host::HostClient,
    presentation::{LocaleContext, UiMessage},
};
use serde_json::json;
use std::{io::Read, process::ExitCode};
pub(super) fn run(args: impl Iterator<Item = String>, locale: LocaleContext) -> ExitCode {
    let result = execute(args.collect(), locale);
    match result {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "{}",
                locale.render(&UiMessage::new("common.error").arg("detail", error))
            );
            ExitCode::FAILURE
        }
    }
}
fn execute(args: Vec<String>, locale: LocaleContext) -> Result<String, String> {
    let client = HostClient::connect_local_presented(2691).map_err(|e| e.render(locale))?;
    let view = client
        .call_with_receipt("web.settings.get", &json!({}))
        .map_err(|e| locale.render(&e.presentation_or_fallback()))?;
    if args.is_empty() || args == ["status"] {
        return Ok(locale
            .text(match view["search_route"].as_str() {
                Some("api") => "webtool.search_deepseek",
                Some("messages") => "webtool.search_messages_active",
                _ if view["search_available"] == true => "webtool.search_shared",
                _ => "webtool.not_configured",
            })
            .into());
    }
    let mut edit = json!({"fetch_provider":view["fetch_provider"],"use_model_key":view["use_model_key"],"search_provider":view["search_provider"],"api_key":null,"clear_key":false});
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["fetch", provider @ ("http" | "deepseek")] => edit["fetch_provider"] = json!(provider),
        ["credential", source @ ("model" | "dedicated")] => {
            edit["use_model_key"] = json!(*source == "model")
        }
        // WEB-D1 三态：smart（默认智能）/ ceramic（强制共享代理）/
        // messages（强制 DeepSeek 原生搜索）。
        ["search", mode @ ("smart" | "ceramic" | "messages")] => {
            edit["search_provider"] = json!(match *mode {
                "smart" => "smart",
                "ceramic" => "ceramic",
                _ => "deepseek_messages",
            })
        }
        ["key", "--stdin"] => {
            let mut key = String::new();
            std::io::stdin()
                .take(4097)
                .read_to_string(&mut key)
                .map_err(|e| e.to_string())?;
            if key.len() > 4096 {
                return Err(locale.text("webtool.invalid_settings").into());
            }
            edit["api_key"] = json!(key.trim_end_matches(['\r', '\n']));
            edit["use_model_key"] = json!(false);
        }
        ["key", "--clear"] => edit["clear_key"] = json!(true),
        _ => return Err(locale.text("webtool.command_help").into()),
    }
    client
        .call_with_receipt("web.settings.set", &edit)
        .map_err(|e| locale.render(&e.presentation_or_fallback()))?;
    Ok(locale.text("webtool.saved").into())
}
