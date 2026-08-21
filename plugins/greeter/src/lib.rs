//! clat greeter 插件（Phase 2c 作者模板 + config 端到端样例）。
//!
//! 用 SDK 的 DSL 声明一个 `greet` 工具，从 `plugins.json` 的
//! `config` 字段读问候语。这是新插件的最小起点——照抄本文件与
//! Cargo.toml 即可开写。

// wit-bindgen 的 proc-macro 展开引用自身 crate 名，需直依赖
// （版本照 SDK 文档钉靶：0.43）。
wit_bindgen::generate!({
    path: "../../wit",
    world: "plugin",
});

// serde 的 derive 展开引用 `::serde::` 路径，需直依赖（生态惯例）；
// wit-bindgen/serde_json 由 SDK 钉靶重导出。
use serde::{Deserialize, Serialize};

const GREET_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "name": { "type": "string", "description": "who to greet" }
  },
  "required": ["name"]
}"#;

#[derive(Deserialize)]
struct GreetArgs {
    name: String,
}

#[derive(Serialize)]
struct GreetOut {
    greeting: String,
}

#[derive(Deserialize)]
struct GreeterConfig {
    greeting: String,
    #[serde(default)]
    upper: bool,
}

fn greet_impl(args: GreetArgs) -> Result<GreetOut, String> {
    let config: GreeterConfig =
        plugin_config().map_err(|error| format!("greeter is not configured: {error}"))?;
    let mut name = args.name;
    if config.upper {
        name = name.to_uppercase();
    }
    Ok(GreetOut {
        greeting: format!("{}, {name}!", config.greeting),
    })
}

clat_plugin::define_plugin! {
    tool "greet" desc("Greets a name using the configured greeting.")
        effect(Pure) schema(GREET_SCHEMA) args(GreetArgs) call(greet_impl);
}
