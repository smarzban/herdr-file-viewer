//! Shared subprocess reaping helpers.
//!
//! The single place that knows how to wait for a child process with a bounded
//! wall-clock budget and kill/reap it if it overruns. Used by the content
//! renderer (`render.rs`) and the update check (`update/mod.rs`) so the
//! timeout-kill semantics are defined once.

use std::process::Child;
use std::time::Duration;

/// Wait for `child` to exit within `grace`, polling every 10 ms; if it overruns,
/// kill and reap it, then return `None`.
///
/// `grace` bounds the **total** wall-clock spent waiting — callers pass a
/// deadline-derived remainder so a double-timeout regression can't happen.
pub fn wait_bounded(child: &mut Child, grace: Duration) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}
