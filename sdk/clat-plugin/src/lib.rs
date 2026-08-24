//! clat 插件 SDK（Phase 2c，docs/todo/wasm-plugin-runtime.md §5.2.2）。
//!
//! 在 wit-bindgen 原生路径之上的薄人机工学层：
//!
//! - **依赖钉靶**：插件只依赖本 crate（`clat_plugin::wit_bindgen` /
//!   `clat_plugin::serde` / `clat_plugin::serde_json` 重导出）——宿主
//!   world 与 bindgen 版本升级时在此单点跟进（INV-K3）。
//! - **[`define_plugin!` DSL**：声明工具（名字/描述/effect/schema/参
//!   数类型/处理函数），宏生成 `Guest` 实现与 JSON 管道（INV-K4：与
//!   手写 wit-bindgen 组件行为等价）。
//! - **配置帮手**：宏生成的 [`plugin_config`] 读取 `clat:plugin/config`
//!   导入并反序列化（INV-K2：只有本插件自己的配置）。
//!
//! 作者模板见仓库 `plugins/greeter`（最小心火箭）与 `plugins/digest`
//! （已迁移到本 SDK 的 dogfood 件）。

/// 与宿主一起验证过的依赖组合（钉靶声明，见 crate 文档；proc-macro
/// 的路径限制使 wit-bindgen/serde-derive 须在插件侧直依赖）。
pub use serde_json;

/// 声明式插件定义 DSL。
///
/// # 前置
///
/// 调用方 crate 已执行绑定生成（路径指向仓库 `wit/`）：
///
/// ```ignore
/// clat_plugin::wit_bindgen::generate!({ path: "../../wit", world: "plugin" });
/// ```
///
/// # 用法
///
/// 每行一个工具；`effect` 是 [`Effect`] 的 PascalCase 变体
/// （Pure/Read/Write/Execute/Network/ExternalRead/Destructive/
/// SessionWrite），`schema` 是 `&str` JSON Schema 常量，`args` 是
/// `Deserialize` 参数类型，`call` 是处理函数（签名
/// `fn(Args) -> Result<T, String>`，`T: Serialize`）：
///
/// ```ignore
/// clat_plugin::define_plugin! {
///     tool "greet" desc("Greets using the configured greeting.")
///         effect(Pure) schema(GREET_SCHEMA) args(GreetArgs) call(greet_impl);
/// }
/// ```
///
/// 宏同时生成 [`plugin_config`] / `plugin_config_string`（读取
/// `clat:plugin/config` 导入——宿主未配置时返回错误）。
// 宏体内的 `crate::…` 刻意指**调用方** crate（macro_rules 的文本解
// 析语义——生成的 Guest 实现与 config 导入解析到插件自己的绑定），
// 不是宏定义方，故不用 `$crate`。
#[allow(clippy::crate_in_macro_def)]
#[macro_export]
macro_rules! define_plugin {
    ( $(
        tool $name:literal desc($desc:literal)
        effect($effect:ident) schema($schema:ident)
        args($args:ty) call($handler:path);
    )* ) => {
        /// SDK 生成的插件入口（实现 world 的 `tools` 导出）。
        pub struct SdkPlugin;

        impl crate::exports::clat::plugin::tools::Guest for SdkPlugin {
            fn list_tools() -> Vec<crate::exports::clat::plugin::tools::Definition> {
                vec![
                    $(
                        crate::exports::clat::plugin::tools::Definition {
                            name: $name.to_owned(),
                            description: $desc.to_owned(),
                            input_schema: $schema.to_owned(),
                            effect: crate::exports::clat::plugin::tools::Effect::$effect,
                        },
                    )*
                ]
            }

            fn call(name: String, arguments: String) -> Result<String, String> {
                match name.as_str() {
                    $(
                        $name => {
                            let args: $args = serde_json::from_str(&arguments)
                                .map_err(|error| format!("invalid arguments: {error}"))?;
                            let output = $handler(args)?;
                            serde_json::to_string(&output)
                                .map_err(|error| format!("serialize output: {error}"))
                        }
                    )*
                    other => Err(format!("unknown tool `{other}`")),
                }
            }
        }

        export!(SdkPlugin);

        /// 读取本插件配置（`clat:plugin/config` 导入）并反序列化为 `T`。
        /// 宿主未配置该插件时返回错误（INV-K2：不静默空值）。
        pub fn plugin_config<T: serde::de::DeserializeOwned>() -> Result<T, String> {
            let raw = plugin_config_string()?;
            serde_json::from_str(&raw)
                .map_err(|error| format!("invalid config: {error}"))
        }

        /// 配置的原始 JSON 字符串形态。
        pub fn plugin_config_string() -> Result<String, String> {
            crate::clat::plugin::config::get()
        }

        /// 读取当前 run 的有界、只读宿主上下文。
        pub fn host_context<T: serde::de::DeserializeOwned>() -> Result<T, String> {
            let raw = crate::clat::plugin::host::context()?;
            serde_json::from_str(&raw)
                .map_err(|error| format!("invalid host context: {error}"))
        }

        /// 经 CLAT 权限策略、项目围栏和工具管线调用允许的原生工具。
        pub fn call_host_tool<A, T>(name: &str, arguments: &A) -> Result<T, String>
        where
            A: serde::Serialize,
            T: serde::de::DeserializeOwned,
        {
            let arguments = serde_json::to_string(arguments)
                .map_err(|error| format!("serialize host tool arguments: {error}"))?;
            let output = crate::clat::plugin::host::call_tool(name, &arguments)?;
            serde_json::from_str(&output)
                .map_err(|error| format!("invalid host tool output: {error}"))
        }
    };
}
