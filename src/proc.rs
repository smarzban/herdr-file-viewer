//! Shared subprocess reaping helpers.
//!
//! The single place that knows how to wait for a child process with a bounded
//! wall-clock budget and kill/reap it if it overruns. Used by the content
//! renderer (`render.rs`) and the update check (`update/mod.rs`) so the
//! timeout-kill semantics are defined once.

use std::process::Child;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const REAP_RESERVE: Duration = Duration::from_millis(50);

/// Wait for `child` to exit within `grace`, polling every 10 ms; if it overruns,
/// kill and reap it within the same wall-clock budget, then return `None`.
///
/// `grace` bounds the **total** wall-clock spent waiting — callers pass a
/// deadline-derived remainder so a double-timeout regression can't happen.
pub fn wait_bounded(child: &mut Child, grace: Duration) -> Option<std::process::ExitStatus> {
    wait_until(child, Instant::now() + grace)
}

/// Wait for `child` through its caller's absolute deadline.
///
/// A small slice of that same deadline is reserved for termination and reaping, so neither path
/// falls through to an unbounded `wait()`. A normally exited child receives its full exit status;
/// an overrun remains `None` after it is killed and reaped.
pub fn wait_until(child: &mut Child, deadline: Instant) -> Option<std::process::ExitStatus> {
    let wait_deadline = deadline.checked_sub(REAP_RESERVE).unwrap_or(deadline);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < wait_deadline => sleep_until(wait_deadline),
            Ok(None) | Err(_) => {
                let _ = terminate_and_reap(child, deadline);
                return None;
            }
        }
    }
}

/// Terminate and reap `child` only until the caller's absolute deadline.
///
/// This is intentionally polling-only after `kill()`: `Child::wait()` has no deadline and could
/// otherwise extend an advisory source call indefinitely.
pub fn terminate_and_reap(
    child: &mut Child,
    deadline: Instant,
) -> Option<std::process::ExitStatus> {
    if let Ok(Some(status)) = child.try_wait() {
        return Some(status);
    }
    let _ = child.kill();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => sleep_until(deadline),
            Ok(None) | Err(_) => return None,
        }
    }
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
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "termination and reaping stay inside the renderer grace period"
        );
        assert!(
            child.try_wait().unwrap().is_some(),
            "the killed renderer child is reaped"
        );
    }
}
