//! `clat dsh`——DSH web 宿主的终端客户端（D-1 反向桥）。
//!
//! 定位（设计档案 `docs/todo/dsh-reverse-bridge.md`）：**替补而非
//! 竞争**——DSH 砍了 TUI，CLAT 补位。纯在线极简流程：探测/spawn
//! `dsh web` → HTTP 动作面 + WS 下行事件流。绝不写 `~/.dsh`
//! （INV-D1）；live 事件渲染借道 ReplayEvent 通路（INV-D6）。

pub(crate) mod app;
pub(crate) mod client;

pub use app::run_dsh;
pub(crate) mod connect;
pub(crate) mod files;
pub(crate) mod frames;
pub(crate) mod transcript;
pub(crate) mod ws;

#[cfg(test)]
mod tests;
