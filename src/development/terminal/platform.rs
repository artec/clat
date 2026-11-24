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
    #[cfg(test)]
    crate::test_support::execution_observation::record("development.pty_start");
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
        if self.terminated {
            return Ok(true);
        }
        // The shell keeps the session identity reserved until cleanup signals finish.
        crate::process::exited_without_reaping(self.pid).map_err(io)
    }
    fn terminate(&mut self) -> Result<(), String> {
        if self.terminated {
            return Ok(());
        }
        self.signal_groups(libc::SIGTERM)?;
        std::thread::sleep(Duration::from_millis(100));
        // Resolve job groups again while the shell still owns this session.
        self.signal_groups(libc::SIGKILL)?;
        crate::process::reap_until(Duration::from_secs(5), || self.child.try_wait()).map_err(io)?;
        self.terminated = true;
        Ok(())
    }
}

#[cfg(unix)]
impl Unix {
    fn signal_groups(&self, signal: i32) -> Result<(), String> {
        let groups = super::processes::groups(self.pid, self.group)?;
        for group in groups {
            let result = unsafe { libc::kill(-group, signal) };
            if result < 0 && signal == libc::SIGKILL {
                let error = std::io::Error::last_os_error();
                // Allow an already-empty group. Darwin also reports EPERM
                // when the exited shell is still the sole group member.
                let absent = error.raw_os_error() == Some(libc::ESRCH);
                let exited = error.raw_os_error() == Some(libc::EPERM)
                    && super::processes::group_has_only_exited_shell(self.pid, group)?;
                if !(absent || exited) {
                    return Err(io(error));
                }
            }
        }
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
    #[cfg(test)]
    crate::test_support::execution_observation::record("development.pty_start");
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn terminal_exit_observation_leaves_the_shell_owned_until_cleanup() {
        let (_, root) = crate::test_support::roots("terminal-exit-before-cleanup");
        std::fs::create_dir_all(&root).unwrap();
        let pid_file = root.join("shell.pid");
        let args = [
            "-c".into(),
            "printf '%s' \"$$\" > \"$1\"; exit 23".into(),
            "fixture".into(),
            pid_file.to_string_lossy().into_owned(),
        ];
        let mut terminal = start("/bin/sh", &args, &root, (24, 80)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !terminal.control.ended().unwrap() {
            assert!(std::time::Instant::now() < deadline, "shell did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid: i32 = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
        // A live PID lookup still finds the uncollected shell at this point.
        let alive_before_cleanup = unsafe { libc::kill(pid, 0) } == 0;
        terminal.control.terminate().unwrap();
        assert!(
            alive_before_cleanup,
            "exit observation must not reap the shell"
        );
        assert!(terminal.control.ended().unwrap());
        terminal.control.terminate().unwrap();
        drop(terminal);
        crate::test_support::cleanup_tree(root.parent().unwrap());
    }
}
