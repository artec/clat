use super::*;
pub(super) trait Control: Send {
    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String>;
    fn ended(&mut self) -> Result<bool, String>;
    fn terminate(&mut self) -> Result<(), String>;
}
pub(super) struct Session {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub control: Box<dyn Control>,
}
#[cfg(unix)]
struct Unix {
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    pid: i32,
    group: i32,
    terminated: bool,
}
#[cfg(unix)]
pub(super) fn start(
    shell: &str,
    args: &[String],
    cwd: &Path,
    size: (u16, u16),
) -> Result<Session, String> {
    use portable_pty::{PtySize, native_pty_system};
    super::processes::pids()?;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: size.0,
            cols: size.1,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("pty:{e}"))?;
    let cmd = unix_command(shell, args, cwd);
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("pty:{e}"))?;
    let Some(pid) = child.process_id() else {
        let _ = child.kill();
        return Err("terminal_isolation".into());
    };
    let Some(group) = pair.master.process_group_leader() else {
        let _ = child.kill();
        return Err("terminal_isolation".into());
    };
    drop(pair.slave);
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("pty:{e}"))?;
    let writer = pair.master.take_writer().map_err(|e| format!("pty:{e}"))?;
    Ok(Session {
        reader,
        writer,
        control: Box::new(Unix {
            master: pair.master,
            child,
            pid: pid as i32,
            group,
            terminated: false,
        }),
    })
}
#[cfg(unix)]
impl Control for Unix {
    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        self.master
            .resize(portable_pty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("pty:{e}"))
    }
    fn ended(&mut self) -> Result<bool, String> {
        self.child.try_wait().map(|s| s.is_some()).map_err(io)
    }
    fn terminate(&mut self) -> Result<(), String> {
        if self.terminated {
            return Ok(());
        }
        self.terminated = true;
        let groups = super::processes::groups(self.pid, self.group);
        for group in &groups {
            unsafe {
                libc::kill(-*group, libc::SIGTERM);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        let mut remaining = super::processes::groups(self.pid, self.group);
        remaining.extend(groups);
        for group in remaining {
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
        let _ = self.child.kill();
        self.child.wait().map_err(io)?;
        Ok(())
    }
}
#[cfg(windows)]
pub(super) fn start(
    shell: &str,
    args: &[String],
    cwd: &Path,
    size: (u16, u16),
) -> Result<Session, String> {
    super::windows::start(shell, args, cwd, size)
}

#[cfg(unix)]
impl Drop for Unix {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

pub(super) struct Ended;
impl Control for Ended {
    fn resize(&mut self, _: u16, _: u16) -> Result<(), String> {
        Err("terminal_ended".into())
    }
    fn ended(&mut self) -> Result<bool, String> {
        Ok(true)
    }
    fn terminate(&mut self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(unix)]
fn unix_command(shell: &str, args: &[String], cwd: &Path) -> portable_pty::CommandBuilder {
    let mut cmd = portable_pty::CommandBuilder::new(shell);
    cmd.args(args);
    cmd.cwd(cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd
}
