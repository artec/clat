use super::*;
use command_group::CommandGroup as _;
use std::process::{Command, Stdio};

fn spawn(script: &str) -> command_group::GroupChild {
    Command::new("/bin/sh")
        .args(["-c", script])
        .group_spawn()
        .unwrap()
}

/// Polls `condition` for a while; fails loudly instead of hanging.
fn soon(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

/// Whether a process (or a whole process group, given `-pid`) still exists.
fn alive(id: libc::pid_t) -> bool {
    // SAFETY: signal 0 probes for the process without delivering anything.
    unsafe { libc::kill(id, 0) == 0 }
}

/// The zombie is what keeps the group id reserved for a KILL that may still
/// follow, so observing a leader's exit must never reap it.
#[test]
fn observing_the_leader_exit_leaves_it_unreaped() {
    let mut child = spawn("exit 7");
    let pid = child.id() as libc::pid_t;
    let mut leader = GroupCommand(&mut child);
    assert!(
        soon(|| leader.leader_exited().unwrap()),
        "leader never exited"
    );
    assert!(alive(pid), "observing the exit must not reap the leader");
    assert_eq!(leader.reap().unwrap().exit_code, Some(7));
    assert!(
        exited_without_reaping(pid).is_err(),
        "the leader is reaped once `reap` returns"
    );
}

/// A PTY session leader is observed the same way.
#[test]
fn observing_a_pty_leader_exit_leaves_it_unreaped() {
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize::default())
        .unwrap();
    let mut command = portable_pty::CommandBuilder::new("/bin/sh");
    command.args(["-c", "exit 7"]);
    let mut child = pair.slave.spawn_command(command).unwrap();
    let pid = child.process_id().unwrap() as libc::pid_t;
    let mut session = PtySession {
        child: child.as_mut(),
        group: pid,
    };
    assert!(soon(|| session.leader_exited().unwrap()));
    assert!(alive(pid), "observing the exit must not reap the leader");
    assert_eq!(session.reap().unwrap().exit_code, Some(7));
}

#[test]
fn pty_termination_preserves_group_grace_after_leader_exit() {
    use super::super::ChildHandle;

    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize::default())
        .unwrap();
    let mut command = portable_pty::CommandBuilder::new("/bin/sh");
    command.args(["-c", "exit 7"]);
    let child = pair.slave.spawn_command(command).unwrap();
    let pid = child.process_id().unwrap() as libc::pid_t;
    let mut handle = ChildHandle::Pty {
        child,
        process_group: Some(pid),
    };
    assert!(soon(|| handle.leader_exited().unwrap()));

    let started = Instant::now();
    let status = handle.terminate_tree();
    assert!(
        started.elapsed() >= KILL_GRACE,
        "the group still needs its full TERM grace when the leader has already exited"
    );
    assert_eq!(status.exit_code, Some(7));
    assert!(exited_without_reaping(pid).is_err());
}

/// Records, at the final group KILL, which watched processes still existed:
/// the leader (which must still be unreaped) and, optionally, a descendant.
struct Observed<'a> {
    leader: GroupCommand<'a>,
    watched: Vec<libc::pid_t>,
    alive_at_kill: Option<Vec<bool>>,
}

impl GroupLeader for Observed<'_> {
    fn signal_group(&mut self, signal: GroupSignal) -> std::io::Result<()> {
        if signal == GroupSignal::Kill {
            self.alive_at_kill = Some(self.watched.iter().copied().map(alive).collect());
        }
        self.leader.signal_group(signal)
    }

    fn leader_exited(&mut self) -> std::io::Result<bool> {
        self.leader.leader_exited()
    }

    fn reap(&mut self) -> std::io::Result<TerminalStatus> {
        self.leader.reap()
    }
}

#[test]
fn supervised_early_exit_keeps_leader_until_shutdown() {
    let mut child = crate::process::SupervisedChild::spawn(
        std::process::Command::new("sh").args(["-c", "exit 23"]),
        Duration::from_millis(100),
        Duration::from_secs(2),
    )
    .unwrap();
    let pid = child.id().unwrap() as libc::pid_t;
    assert!(soon(|| child.observed_exit().unwrap()));
    assert!(exited_without_reaping(pid).unwrap());
    assert_eq!(child.shutdown().unwrap().unwrap().code(), Some(23));
    assert!(exited_without_reaping(pid).is_err());
    assert!(child.shutdown().unwrap().is_none());
}

/// Spawns `script` with a piped stdout and returns that pipe. A script that
/// relies on a `trap` prints one line once the trap is installed; tests read
/// it before terminating, so TERM can never overtake the `trap` and hit a
/// shell that still has the default disposition.
fn spawn_reporting(
    script: &str,
) -> (
    command_group::GroupChild,
    std::io::BufReader<std::process::ChildStdout>,
) {
    let mut child = Command::new("/bin/sh")
        .args(["-c", script])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .group_spawn()
        .unwrap();
    let stdout = std::io::BufReader::new(child.inner().stdout.take().unwrap());
    (child, stdout)
}

/// The one line a script prints once its signal disposition is in place.
fn readiness(stdout: &mut std::io::BufReader<std::process::ChildStdout>) -> String {
    use std::io::BufRead as _;
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert!(
        !line.is_empty(),
        "the script exited before reporting readiness"
    );
    line.trim().to_owned()
}

/// A leader that dies on TERM is a zombie when the group is KILLed: the
/// common case in which a reaped leader would have freed the group id first.
#[test]
fn final_group_kill_is_sent_while_the_leader_is_still_unreaped() {
    let mut child = spawn("sleep 30");
    let pid = child.id() as libc::pid_t;
    let mut observed = Observed {
        leader: GroupCommand(&mut child),
        watched: vec![pid],
        alive_at_kill: None,
    };
    let status = terminate(&mut observed, GracePeriod::UntilLeaderExit, || {
        unreachable!("the leader is ours to reap")
    });
    assert_eq!(
        observed.alive_at_kill,
        Some(vec![true]),
        "the KILL must find the leader still holding the group id"
    );
    assert!(
        status.exit_code.is_none() && status.signal.is_some(),
        "TERM ended the leader: {status:?}"
    );
    assert!(
        exited_without_reaping(pid).is_err(),
        "the leader is reaped once `terminate` returns"
    );
}

/// A leader that ignores TERM is KILLed, and only after the grace period.
#[test]
fn final_group_kill_still_reaches_a_term_ignoring_leader() {
    let (mut child, stdout) =
        spawn_reporting("trap '' TERM; echo ready; while :; do sleep 1; done");
    let mut stdout = stdout;
    assert_eq!(readiness(&mut stdout), "ready");
    let started = Instant::now();
    let mut leader = GroupCommand(&mut child);
    let status = terminate(&mut leader, GracePeriod::UntilLeaderExit, || {
        unreachable!("the leader is ours to reap")
    });
    assert!(started.elapsed() >= KILL_GRACE);
    assert!(
        status.exit_code.is_none() && status.signal.is_some(),
        "KILL ended the leader: {status:?}"
    );
}

/// A descendant that ignores TERM survives the grace period and is removed by
/// the final group KILL, while the leader keeps reporting its own real exit.
#[test]
fn term_ignoring_descendants_are_killed_and_the_leader_keeps_its_real_status() {
    let (mut child, stdout) =
        spawn_reporting("sh -c 'trap \"\" TERM; echo $$; exec sleep 30' & wait");
    let mut stdout = stdout;
    let descendant: libc::pid_t = readiness(&mut stdout).parse().unwrap();
    let leader_pid = child.id() as libc::pid_t;
    let mut observed = Observed {
        leader: GroupCommand(&mut child),
        watched: vec![leader_pid, descendant],
        alive_at_kill: None,
    };
    let status = terminate(&mut observed, GracePeriod::UntilLeaderExit, || {
        unreachable!("the leader is ours to reap")
    });
    assert_eq!(status.signal.as_deref(), Some("15"), "{status:?}");
    assert_eq!(
        observed.alive_at_kill,
        Some(vec![true, true]),
        "the descendant must still be alive when the group is KILLed"
    );
    assert!(
        soon(|| !alive(descendant)),
        "the TERM-ignoring descendant must not outlive the group KILL"
    );
}

/// The monitor polls for exit and may still call `terminate_tree` afterwards
/// (its drain-grace path): polling must leave the leader unreaped, and the
/// termination must report the leader's real exit.
#[test]
fn polling_a_child_handle_does_not_reap_before_termination() {
    use super::super::ChildHandle;

    let mut handle = ChildHandle::Group(spawn("exit 3"));
    let ChildHandle::Group(child) = &handle else {
        unreachable!("constructed as a group child");
    };
    let pid = child.id() as libc::pid_t;
    assert!(soon(|| handle.leader_exited().unwrap()));
    assert!(
        alive(pid),
        "a group signal may still follow the poll: the leader must be unreaped"
    );
    assert_eq!(handle.terminate_tree().exit_code, Some(3));
    assert!(exited_without_reaping(pid).is_err());
}
