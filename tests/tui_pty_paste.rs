//! PASTE-1 的真实终端入口验证（AG-DOGFOOD-4，P3）。
//!
//! 在真实 PTY（libc::openpty）里驱动真实 `clat standalone` 二进制：
//! 发送 bracketed-paste 字节序列（终端把粘贴换行发为 `\r`），经
//! 内核 tty 行规程（raw 模式）→ crossterm 事件解析（`Event::Paste`）→
//! TUI 生产管线，断言草稿渲染把段落放在**不同的物理行**。
//!
//! 判别面不解析 ANSI：比较字节流中两段文本之间是否出现行/游标控制
//! 转义。pre-fix（未归一化）：`\r` 以零宽字符进入行缓冲，段落塌进
//! 同一渲染行，两段文本之间没有任何转义；post-fix：段落分行，中间
//! 必有游标定位序列。
//!
//! GUI 桌面终端（Terminal.app/iTerm2 等）的真实 Cmd+V 剪贴板粘贴不在
//! 自动化范围内（需要实体剪贴板与人工按键），见执行报告的 NOT_RUN
//! 与人工验收步骤。

#![cfg(unix)]

use std::os::fd::FromRawFd;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BOOT_WAIT: Duration = Duration::from_secs(30);
const RENDER_WAIT: Duration = Duration::from_secs(8);

struct PtySession {
    master: i32,
    child: Child,
    output: Arc<Mutex<Vec<u8>>>,
    _reader: std::thread::JoinHandle<()>,
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        // 有界回收：try_wait 轮询到退出（kill 后立即结束），绝不阻塞挂起。
        let deadline = Instant::now() + Duration::from_secs(5);
        while self
            .child
            .try_wait()
            .map_or(true, |status| status.is_none())
        {
            if Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        unsafe {
            libc::close(self.master);
        }
    }
}

fn spawn_clat_on_pty(home: &std::path::Path, project: &std::path::Path) -> PtySession {
    let mut master = 0i32;
    let mut slave = 0i32;
    let mut window_size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut window_size,
        )
    };
    assert_eq!(result, 0, "openpty must allocate a real terminal");

    let stdin = unsafe { Stdio::from_raw_fd(libc::dup(slave)) };
    let stdout = unsafe { Stdio::from_raw_fd(libc::dup(slave)) };
    let stderr = unsafe { Stdio::from_raw_fd(libc::dup(slave)) };
    unsafe {
        libc::close(slave);
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_clat"))
        .arg("standalone")
        .current_dir(project)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CI", "1")
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .expect("spawn clat standalone on the pty");

    let _ = child.stderr.take();
    let output = Arc::new(Mutex::new(Vec::new()));
    let reader_output = Arc::clone(&output);
    let reader_master = master;
    let reader = std::thread::Builder::new()
        .name("clat-pty-reader".into())
        .spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                let count = unsafe {
                    libc::read(reader_master, buffer.as_mut_ptr() as *mut _, buffer.len())
                };
                if count <= 0 {
                    break;
                }
                reader_output
                    .lock()
                    .expect("pty output")
                    .extend_from_slice(&buffer[..count as usize]);
            }
        })
        .expect("pty reader thread");

    PtySession {
        master,
        child,
        output,
        _reader: reader,
    }
}

fn pty_send(master: i32, bytes: &[u8]) {
    let written = unsafe { libc::write(master, bytes.as_ptr() as *const _, bytes.len()) };
    assert_eq!(
        written,
        bytes.len() as isize,
        "pty write must deliver all bytes"
    );
}

fn wait_for(session: &PtySession, needle: &str, within: Duration, stage: &str) -> usize {
    let deadline = Instant::now() + within;
    loop {
        let buffer = session.output.lock().expect("pty output").clone();
        if let Some(index) = find(&buffer, needle.as_bytes()) {
            return index;
        }
        if Instant::now() > deadline {
            panic!(
                "timed out at `{stage}` waiting for {needle:?}; output so far ({} bytes): {:?}",
                buffer.len(),
                String::from_utf8_lossy(&buffer[buffer.len().saturating_sub(2000)..])
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 最后一次出现（渲染差分可能多次写同一文本，取最新一次）。
fn find_last(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

/// 真实 PTY + 真实二进制 + bracketed-paste 字节：段落必须落在不同物理行。
#[test]
fn real_pty_bracketed_paste_keeps_paragraphs_on_separate_rows() {
    let base = std::env::temp_dir().join(format!(
        "clat-ag4-pty-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let home = base.join("home");
    let project = base.join("proj");
    std::fs::create_dir_all(&home).expect("home dir");
    std::fs::create_dir_all(&project).expect("project dir");

    let session = spawn_clat_on_pty(&home, &project);

    // 1) 等真实信任门渲染，按真实按键 'y' 授权。
    wait_for(&session, "Trust this project?", BOOT_WAIT, "trust prompt");
    pty_send(session.master, b"y");
    // 2) 等确权挂载完成、Message 输入框出现。
    wait_for(&session, "Message", BOOT_WAIT, "composer ready");

    // 3) 终端粘贴字节（xterm/Terminal.app 在 bracketed paste 中把换行
    //    发为 \r）：三段文本 + 一个空行。
    pty_send(
        session.master,
        b"\x1b[200~AG4 pty first\rsecond paragraph\r\rthird tail\x1b[201~",
    );

    // 4) 等渲染回流，然后按"段落是否分行"判别。ratatui 以 run 为单位
    //    写单元（同行相邻词间也可能有游标移动），因此锚定每个段落的
    //    末词，断言两段之间的字节间隙里出现**行/游标控制转义**：
    //    post-fix 段落落行不同物理行，间隙必有 ESC；pre-fix `\r` 零宽
    //    入行、段落塌进同一渲染行，间隙只有裸 \r、没有 ESC。
    let start = Instant::now();
    let (first_at, second_at, third_at) = loop {
        let buffer = session.output.lock().expect("pty output").clone();
        if let (Some(first), Some(second), Some(third)) = (
            find_last(&buffer, b"first"),
            find_last(&buffer, b"second paragraph"),
            find_last(&buffer, b"third"),
        ) {
            break (first, second, third);
        }
        assert!(
            Instant::now() - start < RENDER_WAIT,
            "pasted paragraphs never rendered: {:?}",
            String::from_utf8_lossy(&buffer[buffer.len().saturating_sub(2000)..])
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    let buffer = session.output.lock().expect("pty output").clone();
    let first_to_second = &buffer[first_at + b"first".len()..second_at];
    assert!(
        first_to_second.contains(&0x1b),
        "paragraph one and two must land on separate physical rows; raw gap: {first_to_second:?}"
    );
    let second_to_third = &buffer[second_at + b"second paragraph".len()..third_at];
    assert!(
        second_to_third.contains(&0x1b),
        "paragraph two and three must land on separate physical rows; raw gap: {second_to_third:?}"
    );
    // 顺带钉住提交安全：粘贴本身绝不能触发发送（无 run 启动迹象）。
    assert!(
        find(&buffer, b"turn 1").is_none(),
        "paste must never submit the draft"
    );
}
