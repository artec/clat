//! 纯在线连接流程（D-1 §2）：探测 → describe 指纹 → 是 DSH 直接用
//! （不问出处）；否 → spawn `dsh web`（先默认端口，bind 失败重试
//! `--port 0` + 解析 stdout 就绪行）；无 dsh 且无 `~/.dsh` → 「dsh
//! 未安装」。掉线由前端横幅 + `/reconnect` 手动重试（INV-D2/D4）。

use crate::dsh::client::{DshClient, looks_like_dsh};
use serde_json::Value;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub(crate) const DEFAULT_PORT: u16 = 3080;
const SPAWN_READY_TIMEOUT: Duration = Duration::from_secs(20);
const PROBE_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub(crate) struct Online {
    pub(crate) port: u16,
    pub(crate) describe: Value,
    /// 本进程 spawn 的宿主句柄（None = 探测直连了别人起的宿主——
    /// 归属权不明，永不触碰）。D-2 退出清理：调用方持有至退出。
    pub(crate) child: Option<std::process::Child>,
}

#[derive(Debug)]
pub(crate) enum ConnectFailure {
    /// 无 dsh 可执行且无 `~/.dsh`（决策链 5：一句报错，不装不指引）。
    NotInstalled,
    Failed(String),
}

/// 就绪探测的轮内上限：describe 指纹通过 = 在线。
fn probe(port: u16) -> Option<Value> {
    let client = DshClient::new(port);
    let describe = client.probe_describe(port).ok()?;
    looks_like_dsh(&describe).then_some(describe)
}

/// 连接流程（探测 → spawn → 就绪）。`dsh_binary`/`home` 由调用方注入
/// （生产从 PATH 与 `~/.dsh` 解析，测试直接传参）。
pub(crate) fn ensure_online(
    preferred_port: u16,
    dsh_binary: &str,
    home: Option<&Path>,
) -> Result<Online, ConnectFailure> {
    if let Some(describe) = probe(preferred_port) {
        return Ok(Online {
            port: preferred_port,
            describe,
            child: None,
        });
    }
    // spawn 路径。可执行缺席时按有无 ~/.dsh 区分两种失败形态。
    let spawned = spawn_web(dsh_binary, preferred_port);
    match spawned {
        Ok((port, mut child)) => {
            // 就绪轮询：describe 通过才算在线（INV：指纹是唯一闸门）。
            let deadline = Instant::now() + SPAWN_READY_TIMEOUT;
            while Instant::now() < deadline {
                if let Some(describe) = probe(port) {
                    return Ok(Online {
                        port,
                        describe,
                        child: Some(child),
                    });
                }
                std::thread::sleep(PROBE_INTERVAL);
            }
            // 就绪行之后仍不就绪 = 宿主坏了：自己 spawn 的孤儿不外泄，
            // 带走它再报错。
            let _ = child.kill();
            let _ = child.wait();
            Err(ConnectFailure::Failed(format!(
                "dsh web did not become ready within {}s on port {port}",
                SPAWN_READY_TIMEOUT.as_secs()
            )))
        }
        Err(error) => {
            if home.is_none() {
                Err(ConnectFailure::NotInstalled)
            } else {
                Err(ConnectFailure::Failed(error))
            }
        }
    }
}

/// spawn `dsh web`：先 `--port <preferred>`；进程即刻退出（典型：端口
/// 被非 DSH 占用）则重试 `--port 0` 并解析 stdout 的
/// `dsh web: http://127.0.0.1:<port>` 就绪行拿实际端口。返回就绪
/// 探测应使用的端口 + **存活子进程句柄**（调用方持有至退出——D-2
/// 退出清理：clat 只 kill 自己 spawn 的宿主）。
fn spawn_web(dsh_binary: &str, preferred_port: u16) -> Result<(u16, std::process::Child), String> {
    let mut last_error = String::new();
    for attempt in [Some(preferred_port), None] {
        let mut command = std::process::Command::new(dsh_binary);
        command
            .arg("web")
            .arg("--no-open")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        if let Some(port) = attempt {
            command.arg("--port").arg(port.to_string());
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return Err(format!("cannot start `{dsh_binary} web`: {error}"));
            }
        };
        let stdout = child.stdout.take();
        let (line_tx, line_rx) = mpsc::channel::<String>();
        if let Some(stdout) = stdout {
            std::thread::spawn(move || {
                use std::io::BufRead as _;
                let reader = std::io::BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    if line_tx.send(line).is_err() {
                        // 就绪行已被消费、接收端走了：继续读完管道——
                        // 没人读的话子进程写满 stdout 缓冲（64KiB）会
                        // 卡死宿主（D-2 退出清理发现的连带隐患）。
                        continue;
                    }
                }
            });
        }
        let deadline = Instant::now() + SPAWN_READY_TIMEOUT;
        loop {
            // 就绪行（research §10.4-4）：`dsh web: http://127.0.0.1:<port>`。
            while let Ok(line) = line_rx.try_recv() {
                if let Some(port) = parse_ready_port(&line) {
                    return Ok((port, child));
                }
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    last_error = format!("`{dsh_binary} web` exited early: {status}");
                    break; // 尝试下一档（--port 0）
                }
                Ok(None) => {}
                Err(error) => return Err(format!("cannot watch `{dsh_binary} web`: {error}")),
            }
            if Instant::now() >= deadline {
                // 自身失败路径不留孤儿。
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`{dsh_binary} web` produced no readiness line within {}s",
                    SPAWN_READY_TIMEOUT.as_secs()
                ));
            }
            // 有明确端口档位时也可以直接探测就绪（就绪行可能尚未刷出）。
            if let Some(port) = attempt
                && probe(port).is_some()
            {
                return Ok((port, child));
            }
            std::thread::sleep(PROBE_INTERVAL);
        }
    }
    Err(last_error)
}

fn parse_ready_port(line: &str) -> Option<u16> {
    let marker = "dsh web: http://127.0.0.1:";
    let start = line.find(marker)? + marker.len();
    let tail = &line[start..];
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_line_port_parsing() {
        assert_eq!(
            parse_ready_port("dsh web: http://127.0.0.1:41234"),
            Some(41234)
        );
        assert_eq!(
            parse_ready_port("  dsh web: http://127.0.0.1:3080  "),
            Some(3080)
        );
        assert_eq!(parse_ready_port("dsh web: http://localhost:3080"), None);
        assert_eq!(parse_ready_port("irrelevant"), None);
    }
}
