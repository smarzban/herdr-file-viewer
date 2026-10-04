//! Shared subprocess reaping helpers.
//!
//! The single place that knows how to wait for a child process with a bounded
//! wall-clock budget and kill/reap it if it overruns. Used by the content
//! renderer (`render.rs`) and the update check (`update/mod.rs`) so the
//! timeout-kill semantics are defined once.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The directory the viewer was launched from — herdr starts the pane in the plugin root —
/// recorded before the process cwd starts following the tree root (`app::follow_root`).
static LAUNCH_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Record the current directory as the launch directory. Call once, before the first `chdir`.
/// Later calls are ignored, so the first (pre-chdir) directory sticks.
pub fn remember_launch_dir() {
    if let Ok(dir) = std::env::current_dir() {
        let _ = LAUNCH_DIR.set(dir);
    }
}

/// Run `cmd` from the launch directory rather than the viewer's live cwd.
///
/// The viewer's cwd follows the tree root, which may be an untrusted repository. External tools
/// (renderers, the editor, the OS opener, herdr) must not inherit it: a relative configured
/// program (`./tools/render`) would resolve inside that repository, and a tool that reads
/// cwd-local config (delta reads the enclosing repo's `.git/config`) would let the repository
/// steer it. Running them from the launch directory keeps the behaviour from before the cwd
/// followed the root. A no-op until [`remember_launch_dir`] runs (tests, other entry points).
pub fn in_launch_dir(cmd: &mut Command) -> &mut Command {
    if let Some(dir) = LAUNCH_DIR.get() {
        cmd.current_dir(dir);
    }
    cmd
}

const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Wait for `child` to exit within `grace`, polling every 10 ms; if it overruns,
/// kill and reap it, then return `None`.
///
/// `grace` bounds the wait for **useful work** — callers pass a deadline-derived
/// remainder so a double-timeout regression can't happen. See [`wait_until`] for
/// the termination contract on overrun.
pub fn wait_bounded(child: &mut Child, grace: Duration) -> Option<std::process::ExitStatus> {
    wait_until(child, Instant::now() + grace)
}

/// Wait for `child` through its caller's absolute deadline.
///
/// The deadline bounds only the wait for useful work: on overrun (or a wait error) the child is
/// killed and reaped **unconditionally**, so the call may briefly outlive the deadline. That is
/// deliberate — see [`terminate_and_reap`] — and must not be "fixed" by bounding the reap, which
/// reintroduces zombie leaks under load. A normally exited child returns its full exit status; an
/// overrun returns `None` after the kill and reap.
pub fn wait_until(child: &mut Child, deadline: Instant) -> Option<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => sleep_until(deadline),
            Ok(None) | Err(_) => {
                let _ = terminate_and_reap(child);
                return None;
            }
        }
    }
}

/// Kill and reap `child` unconditionally, returning its exit status when the OS reports one.
///
/// SIGKILL cannot be ignored, so the post-kill `wait()` returns promptly except in pathological
/// OS stalls (e.g. uninterruptible sleep), where nothing shorter of leaking the child would help.
/// Accepting that brief overrun is what guarantees no zombie or leaked pipe survives a caller's
/// deadline — a deadline-bounded reap returns early under load and leaks the killed child instead.
pub fn terminate_and_reap(child: &mut Child) -> Option<std::process::ExitStatus> {
    if let Ok(Some(status)) = child.try_wait() {
        return Some(status);
    }
    let _ = child.kill();
    child.wait().ok()
}

fn sleep_until(deadline: Instant) {
    std::thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::Instant;

    const STALL_FIXTURE_ENV: &str = "HERDR_FV_PROC_STALL_FIXTURE";
    const STALL_FIXTURE_NAME: &str = "proc::tests::wait_bounded_stalled_child_fixture";
    const STALL_FIXTURE_MARKER: &str = "herdr-fv-proc-stall";

    fn fixture_command(stall: bool) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("test binary path"));
        command.args(["--exact", STALL_FIXTURE_NAME, "--", STALL_FIXTURE_MARKER]);
        if stall {
            command.env(STALL_FIXTURE_ENV, "1");
        } else {
            command.env_remove(STALL_FIXTURE_ENV);
        }
        command
    }

    #[test]
    fn wait_bounded_stalled_child_fixture() {
        if std::env::var_os(STALL_FIXTURE_ENV).is_some()
            && std::env::args().any(|argument| argument == STALL_FIXTURE_MARKER)
        {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    #[test]
    fn wait_bounded_returns_a_successful_normal_status() {
        let mut child = fixture_command(false)
            .spawn()
            .expect("fixture child starts");

        assert!(
            wait_bounded(&mut child, Duration::from_secs(1)).is_some_and(|status| status.success()),
            "a normally completing renderer child retains its successful status"
        );
    }

    #[test]
    fn wait_bounded_times_out_and_reaps_a_stalled_child() {
        let mut child = fixture_command(true).spawn().expect("stalled child starts");
        let started = Instant::now();

        assert_eq!(wait_bounded(&mut child, Duration::from_millis(100)), None);
        // Bounded vs unbounded: the fixture child stalls for 60s, so this proves termination and
        // reaping returned rather than waiting on it. 2s is 20x the requested timeout — ample for a
        // loaded runner (the flakes this replaced were 314-340ms) while still rejecting a
        // multi-second reap tail, which a 5s bound let through. Not `timeout + small slack` (100ms
        // asserted under 250ms): that is a coin flip. See AGENTS.md, "Tests prove things
        // deterministically".
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "termination and reaping must return rather than wait out the stalled child: {:?}",
            started.elapsed()
        );
        assert!(
            child.try_wait().unwrap().is_some(),
            "the killed renderer child is reaped"
        );
    }
}
