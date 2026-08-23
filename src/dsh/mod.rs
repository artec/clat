//! `clat dsh`——DSH web 宿主的终端客户端（D-1 反向桥 → D-2 归一）。
//!
//! 定位（设计档案 `docs/todo/d2-dsh-in-clat-tui.md`）：**替补而非
//! 竞争**——DSH 砍了 TUI，CLAT 补位。D-2 起入口是 CLAT TUI 本体
//!（`clat::tui::run_dsh_mode`，同一 App/事件循环/弹框），本模块只
//! 保留协议与编排层：连接、HTTP client、WS 下行、帧解析、转录装配、
//! backend 任务编排。绝不写 `~/.dsh`（INV-D1）；live 事件渲染借道
//! ReplayEvent 通路（INV-D6 / INV-U4 协议语义零改动）。

pub(crate) mod backend;
pub(crate) mod client;
pub(crate) mod connect;
pub(crate) mod files;
pub(crate) mod frames;
pub(crate) mod transcript;
pub(crate) mod ws;

#[cfg(test)]
mod tests;
