//! An owned non-interactive process group with bounded teardown.
use command_group::{CommandGroup as _, GroupChild};
use std::io;
use std::process::{ChildStdout, Command, ExitStatus};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct SupervisedChild {
    child: Option<GroupChild>,
    #[cfg(unix)]
    grace: Duration,
    reap_limit: Duration,
}

impl SupervisedChild {
    pub fn spawn(command: &mut Command, grace: Duration, reap_limit: Duration) -> io::Result<Self> {
        #[cfg(not(unix))]
        let _ = grace;
        Ok(Self {
            child: Some(command.group_spawn()?),
            #[cfg(unix)]
            grace,
            reap_limit,
        })
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.as_mut()?.inner().stdout.take()
    }

    pub fn id(&self) -> Option<u32> {
        self.child.as_ref().map(GroupChild::id)
    }

    pub fn observed_exit(&mut self) -> io::Result<bool> {
        let Some(child) = self.child.as_mut() else {
            return Ok(true);
        };
        #[cfg(unix)]
        {
            super::terminate::exited_without_reaping(child.id() as libc::pid_t)
        }
        #[cfg(not(unix))]
        {
            child.try_wait().map(|status| status.is_some())
        }
    }

    /// Retire ownership even when cleanup cannot be confirmed. A second call
    /// never sends signals to the numeric identity of an already reaped child.
    pub fn shutdown(&mut self) -> io::Result<Option<ExitStatus>> {
        let Some(mut child) = self.child.take() else {
            return Ok(None);
        };
        #[cfg(target_os = "macos")]
        let group_id = child.id() as libc::pid_t;
        #[cfg(unix)]
        let signal = super::terminate::signal_termination(
            &mut super::terminate::GroupCommand(&mut child),
            super::terminate::GracePeriod::UntilLeaderExit,
            self.grace,
        )
        .or_else(|error| {
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(error)
            }
        });
        #[cfg(not(unix))]
        let signal = child.kill();

        let status = reap_until(self.reap_limit, || child.try_wait())?;
        #[cfg(target_os = "macos")]
        let signal = signal.or_else(|error| {
            // Darwin can report EPERM if the only member was a zombie.
            // Probe without delivering a signal after the leader was reaped.
            if error.raw_os_error() == Some(libc::EPERM)
                && unsafe { libc::kill(-group_id, 0) } == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                Ok(())
            } else {
                Err(error)
            }
        });
        signal?;
        Ok(Some(status))
    }
}

pub(crate) fn reap_until<T>(
    limit: Duration,
    mut poll: impl FnMut() -> io::Result<Option<T>>,
) -> io::Result<T> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = poll()? {
            return Ok(status);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

impl Drop for SupervisedChild {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            eprintln!("clat: owned process cleanup incomplete: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn supervised_windows_job_terminates_descendant() {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::{
            Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
        };

        const FIXTURE: &str = "CLAT_SUPERVISED_CHILD_TEST_PID";
        if let Some(path) = std::env::var_os(FIXTURE) {
            let mut descendant = Command::new("ping.exe")
                .args(["-n", "60", "127.0.0.1"])
                .spawn()
                .unwrap();
            std::fs::write(path, descendant.id().to_string()).unwrap();
            reap_until(Duration::from_secs(70), || descendant.try_wait()).unwrap();
            return;
        }

        let (storage, _) = crate::test_support::roots("supervised-child-job");
        std::fs::create_dir_all(&storage).unwrap();
        let pid_file = storage.join("descendant");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "process::supervised_child::tests::supervised_windows_job_terminates_descendant",
            ])
            .env(FIXTURE, &pid_file)
            .stdout(std::process::Stdio::null());
        let mut owned =
            SupervisedChild::spawn(&mut command, Duration::ZERO, Duration::from_secs(5)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let pid = loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_file)
                .unwrap_or_default()
                .parse()
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "missing descendant pid");
            std::thread::sleep(Duration::from_millis(20));
        };
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(!raw.is_null(), "descendant must be running");
        let descendant = unsafe { OwnedHandle::from_raw_handle(raw) };
        assert_eq!(
            unsafe { WaitForSingleObject(descendant.as_raw_handle(), 0) },
            WAIT_TIMEOUT
        );
        assert!(!owned.observed_exit().unwrap());
        assert!(owned.shutdown().unwrap().is_some());
        assert_eq!(
            unsafe { WaitForSingleObject(descendant.as_raw_handle(), 5000) },
            WAIT_OBJECT_0
        );
        assert!(owned.shutdown().unwrap().is_none());
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }

    #[test]
    fn bounded_reaping_is_bounded_and_prompt() {
        let started = Instant::now();
        assert_eq!(
            reap_until::<ExitStatus>(Duration::from_millis(120), || Ok(None))
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut,
        );
        assert!(started.elapsed() < Duration::from_secs(2));

        let mut command = Command::new(if cfg!(windows) { "cmd" } else { "true" });
        if cfg!(windows) {
            command.args(["/C", "exit", "0"]);
        }
        let mut child = command.spawn().unwrap();
        assert!(
            reap_until(Duration::from_secs(5), || child.try_wait())
                .unwrap()
                .success()
        );

        let failure = reap_until::<ExitStatus>(Duration::from_secs(2), || {
            Err(io::Error::other("watch broken"))
        });
        assert_eq!(failure.unwrap_err().to_string(), "watch broken");
    }
}
