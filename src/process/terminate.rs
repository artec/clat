//! Unix process-group termination that keeps the group's identity until the
//! last signal has been sent.
//!
//! Invariant: a group leader is reaped only after the final signal to its
//! process group. A zombie leader still owns its pid, and with it the
//! process-group id. Once it is reaped and the group is empty, the numeric id
//! can be reused by an unrelated process group, and a late `kill(-pgid)` hits
//! that group instead (2026-11-23 audit A-2). Leader exit is therefore
//! observed with `waitid(.., WNOWAIT)`, which reports the exit and leaves the
//! zombie in place. Callers that used to poll with `try_wait` go through
//! [`exited_without_reaping`] as well, so no path reaps first.

use std::io;
use std::time::{Duration, Instant};

use super::{KILL_GRACE, TerminalStatus};

const POLL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GroupSignal {
    Terminate,
    Kill,
}

/// One process-group leader together with the group it owns.
pub(super) trait GroupLeader {
    /// Sends `signal` to the whole process group (leader and descendants).
    fn signal_group(&mut self, signal: GroupSignal);
    /// Whether the leader has exited, observed without reaping it.
    fn leader_exited(&mut self) -> io::Result<bool>;
    /// Reaps the leader. Valid only after the final group signal.
    fn reap(&mut self) -> io::Result<TerminalStatus>;
}

pub(super) enum GracePeriod {
    UntilLeaderExit,
    Full,
}

/// TERM the group, allow the selected grace period, KILL whatever is left,
/// then reap. `fallback` describes a leader whose status is unreadable.
pub(super) fn terminate(
    leader: &mut dyn GroupLeader,
    grace: GracePeriod,
    fallback: impl FnOnce() -> TerminalStatus,
) -> TerminalStatus {
    leader.signal_group(GroupSignal::Terminate);
    match grace {
        // A terminal job can still be shutting down after its shell has exited.
        GracePeriod::Full => std::thread::sleep(KILL_GRACE),
        GracePeriod::UntilLeaderExit => {
            let deadline = Instant::now() + KILL_GRACE;
            while Instant::now() < deadline && matches!(leader.leader_exited(), Ok(false)) {
                std::thread::sleep(POLL);
            }
        }
    }
    // Leader exit does not prove the process group is empty: TERM-ignoring
    // descendants may still be running. The leader is still unreaped here, so
    // the group id still names this group.
    leader.signal_group(GroupSignal::Kill);
    leader.reap().unwrap_or_else(|_| fallback())
}

/// Whether our child `pid` has exited. `WNOWAIT` leaves it a zombie, so its
/// pid — and, for a group leader, its process-group id — stays reserved.
pub(super) fn exited_without_reaping(pid: libc::pid_t) -> io::Result<bool> {
    // `WNOHANG` keeps the question non-blocking, but the call then succeeds
    // whether or not a child was reported, and POSIX only promises `si_pid`
    // to be 0 for "nothing to report". The `siginfo_t` layout differs per
    // platform, so the answer is read from a zeroed buffer instead: a report
    // always writes non-zero `si_signo`/`si_pid`, an empty one leaves zeros.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid, writable out-parameter for `waitid`.
    let observed = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
        ) == 0
    };
    if !observed {
        let error = io::Error::last_os_error();
        return match error.kind() {
            // A signal hit this thread mid-call: nothing was observed, poll again.
            io::ErrorKind::Interrupted => Ok(false),
            _ => Err(error),
        };
    }
    Ok(written(&info))
}

/// Whether a zeroed `siginfo_t` was filled in by a successful `waitid`.
fn written(info: &libc::siginfo_t) -> bool {
    // SAFETY: `info` is initialized, and reading initialized memory as bytes
    // is sound for any type.
    let bytes = unsafe {
        std::slice::from_raw_parts(
            (info as *const libc::siginfo_t).cast::<u8>(),
            std::mem::size_of::<libc::siginfo_t>(),
        )
    };
    bytes.iter().any(|&byte| byte != 0)
}

/// A non-PTY command spawned in its own process group.
pub(super) struct GroupCommand<'a>(pub &'a mut command_group::GroupChild);

impl GroupLeader for GroupCommand<'_> {
    fn signal_group(&mut self, signal: GroupSignal) {
        use command_group::UnixChildExt as _;
        let _ = self.0.signal(match signal {
            GroupSignal::Terminate => command_group::Signal::SIGTERM,
            GroupSignal::Kill => command_group::Signal::SIGKILL,
        });
    }

    fn leader_exited(&mut self) -> io::Result<bool> {
        exited_without_reaping(self.0.id() as libc::pid_t)
    }

    fn reap(&mut self) -> io::Result<TerminalStatus> {
        self.0.wait().map(super::group_status)
    }
}

/// A PTY session leader; `setsid` made it its own process-group leader.
pub(super) struct PtySession<'a> {
    pub child: &'a mut dyn portable_pty::Child,
    pub group: libc::pid_t,
}

impl GroupLeader for PtySession<'_> {
    fn signal_group(&mut self, signal: GroupSignal) {
        // SAFETY: `kill` only queues a signal for the process group.
        unsafe {
            libc::kill(
                -self.group,
                match signal {
                    GroupSignal::Terminate => libc::SIGTERM,
                    GroupSignal::Kill => libc::SIGKILL,
                },
            );
        }
    }

    fn leader_exited(&mut self) -> io::Result<bool> {
        self.child
            .process_id()
            .map_or(Ok(false), |pid| exited_without_reaping(pid as libc::pid_t))
    }

    fn reap(&mut self) -> io::Result<TerminalStatus> {
        self.child.wait().map(super::pty_status)
    }
}

#[cfg(test)]
mod tests;
