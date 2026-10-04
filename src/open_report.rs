//! Host Adapter — the **open-file report**: tell herdr which file this viewer pane shows, so
//! another plugin can read it back with `herdr pane get` (for example to reopen a closed viewer
//! at the same file through the launch **open target**).
//!
//! The value is a herdr pane metadata token, reported through the documented CLI:
//!
//! ```text
//! herdr pane report-metadata <HERDR_PANE_ID> --source herdr-file-viewer --token file_viewer_open=<path>
//! herdr pane report-metadata <HERDR_PANE_ID> --source herdr-file-viewer --clear-token file_viewer_open
//! ```
//!
//! (argv verified against herdr 0.9.1: `herdr pane report-metadata --help`, and `herdr pane get`
//! returning a `tokens` map; token metadata exists since herdr 0.7.4. On an older herdr the call
//! fails and is ignored.)
//!
//! **Read-only w.r.t. files and git.** The report writes nothing to disk and runs no git: it sets
//! one display-only value in herdr's in-memory pane state, which herdr drops when the pane closes
//! and never restores after a server restart. Off by default: config `report_open_file = true`
//! turns it on.
//!
//! **Exact or absent, never truncated.** herdr trims a token value, strips control characters, and
//! caps it at 80 characters. A path that would not survive that unchanged, or that the launch open
//! target parser would read back differently (`name:12` reads as a line suffix), is never sent: the
//! token is cleared instead, so a reader sees either the exact path or nothing.
//!
//! **Cheap.** The run loop calls [`OpenFileReporter::observe`] every tick; it compares one path
//! and does nothing else unless the displayed file changed. A change goes to one background
//! thread that runs the herdr CLI and collapses a backlog to the newest value, so a held `j`
//! never queues one process per row and the UI thread never waits on herdr.

use crate::herdr::HerdrCli;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// The pane metadata token name another plugin reads (`.result.pane.tokens.file_viewer_open`).
/// Mirrors [`crate::open_target::OPEN_ENV`] (`HERDR_FILE_VIEWER_OPEN`), which takes the value back.
pub const TOKEN: &str = "file_viewer_open";

/// The `--source` id the viewer reports under.
pub const SOURCE: &str = "herdr-file-viewer";

/// herdr's cap on a token value, in characters (`MAX_METADATA_TOKEN_VALUE_LEN`, counted with
/// `chars()`). A longer path is not reported (see the module docs).
pub const MAX_VALUE_CHARS: usize = 80;

/// The env var herdr sets to a pane's own id in every split, tab, or overlay pane it starts.
pub const PANE_ENV: &str = "HERDR_PANE_ID";

/// How long quitting waits for the final clear to reach herdr before giving up.
pub const FINISH_WAIT: Duration = Duration::from_secs(1);

/// One change to send to herdr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// Set the token to this path.
    Set(String),
    /// Remove the token.
    Clear,
}

/// The token value for `file` shown under the tree `root`: the path relative to `root`, as the
/// launch open target (`--open` / `HERDR_FILE_VIEWER_OPEN`) accepts it. `None` when no value would
/// reach a reader exactly: `file` is not under `root`, the path is not UTF-8, is longer than
/// [`MAX_VALUE_CHARS`], has surrounding whitespace or a control character (herdr would alter it),
/// or would parse back as a different open target (a trailing `:N` or `:A-B`).
pub fn token_value(root: &Path, file: &Path) -> Option<String> {
    let value = file.strip_prefix(root).ok()?.to_str()?;
    if value.is_empty()
        || value.chars().count() > MAX_VALUE_CHARS
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return None;
    }
    let parsed = crate::open_target::parse_open_target(value)?;
    (parsed.path == value && parsed.line.is_none()).then(|| value.to_string())
}

/// The herdr argv (without the program) for one [`Report`] on `pane_id`.
pub fn report_argv(pane_id: &str, report: &Report) -> Vec<String> {
    let mut argv: Vec<String> = ["pane", "report-metadata", pane_id, "--source", SOURCE]
        .into_iter()
        .map(String::from)
        .collect();
    match report {
        Report::Set(value) => {
            argv.push("--token".into());
            argv.push(format!("{TOKEN}={value}"));
        }
        Report::Clear => {
            argv.push("--clear-token".into());
            argv.push(TOKEN.into());
        }
    }
    argv
}

/// The pane to report on: `HERDR_PANE_ID` when reporting is enabled and the value is non-empty.
/// Outside herdr (no pane id), or with `report_open_file` off, there is nothing to report to.
pub fn report_pane(enabled: bool, pane_env: Option<String>) -> Option<String> {
    enabled
        .then_some(pane_env)
        .flatten()
        .filter(|pane| !pane.trim().is_empty())
}

/// The pure decision behind [`OpenFileReporter`]: which [`Report`], if any, a newly observed
/// displayed file calls for. Holds the launch root, the last observed `(root, file)`, and the
/// value herdr holds.
#[derive(Debug)]
pub struct OpenFileReport {
    home: PathBuf,
    seen: Option<(PathBuf, PathBuf)>,
    reported: Option<String>,
}

impl OpenFileReport {
    /// A report for a viewer that launched rooted at `home`. Only a file shown under `home` is
    /// reported: a viewer started again from this pane roots there, so a path relative to any
    /// other root (a worktree switched to with `W`) would open the wrong file.
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            seen: None,
            reported: None,
        }
    }

    /// Observe the displayed file as `(tree root, absolute path)`, or `None` when no file is shown
    /// (a directory, an empty tree). Returns the report to send, or `None` when herdr already
    /// holds the right value. An unchanged observation returns `None` after one path comparison.
    /// A file shown under a root other than the launch root clears the token.
    pub fn observe(&mut self, open: Option<(&Path, &Path)>) -> Option<Report> {
        let unchanged = match (&self.seen, open) {
            (Some((root, file)), Some((new_root, new_file))) => {
                root == new_root && file == new_file
            }
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return None;
        }
        self.seen = open.map(|(root, file)| (root.to_path_buf(), file.to_path_buf()));
        let value = open
            .filter(|(root, _)| *root == self.home)
            .and_then(|(root, file)| token_value(root, file));
        if value == self.reported {
            return None;
        }
        self.reported = value.clone();
        Some(value.map_or(Report::Clear, Report::Set))
    }

    /// The report that leaves no token behind when the viewer quits: a clear when a value is set.
    pub fn finish(&mut self) -> Option<Report> {
        self.seen = None;
        self.reported.take().map(|_| Report::Clear)
    }
}

/// Sends [`OpenFileReport`] decisions to herdr from one background thread.
pub struct OpenFileReporter {
    state: OpenFileReport,
    tx: mpsc::Sender<Report>,
    done: mpsc::Receiver<()>,
}

impl OpenFileReporter {
    /// Start the reporter for `pane_id` in a viewer launched rooted at `home`, running each report
    /// through `herdr`. If the thread cannot start, every report is dropped silently: the viewer
    /// works the same without it.
    pub fn start(herdr: Box<dyn HerdrCli + Send>, pane_id: String, home: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel::<Report>();
        let (done_tx, done) = mpsc::channel::<()>();
        let _ = std::thread::Builder::new()
            .name("open-file-report".into())
            .spawn(move || {
                while let Ok(mut report) = rx.recv() {
                    // Only the newest value matters: skip any that queued up behind a slow call.
                    while let Ok(newer) = rx.try_recv() {
                        report = newer;
                    }
                    let argv = report_argv(&pane_id, &report);
                    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
                    // Best-effort: an old herdr without token metadata, or a closed pane, fails
                    // here and the viewer carries on.
                    let _ = herdr.run(&args);
                }
                let _ = done_tx.send(());
            });
        Self {
            state: OpenFileReport::new(home),
            tx,
            done,
        }
    }

    /// Start the live reporter, for a viewer launched rooted at `home`, when `enabled` and herdr
    /// gave this pane an id; `None` otherwise.
    pub fn from_env(enabled: bool, home: PathBuf) -> Option<Self> {
        let pane = report_pane(enabled, std::env::var(PANE_ENV).ok())?;
        Some(Self::start(
            Box::new(crate::herdr::LiveHerdr::from_env()),
            pane,
            home,
        ))
    }

    /// Observe the displayed file (see [`OpenFileReport::observe`]) and queue any report.
    pub fn observe(&mut self, open: Option<(&Path, &Path)>) {
        if let Some(report) = self.state.observe(open) {
            let _ = self.tx.send(report);
        }
    }

    /// Clear the token if one is set, then wait up to `wait` for the queue to drain.
    pub fn finish(mut self, wait: Duration) {
        if let Some(report) = self.state.finish() {
            let _ = self.tx.send(report);
        }
        drop(self.tx);
        let _ = self.done.recv_timeout(wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/repo")
    }

    #[test]
    fn token_value_is_the_root_relative_path() {
        assert_eq!(
            token_value(&root(), Path::new("/repo/src/app.rs")).as_deref(),
            Some("src/app.rs")
        );
    }

    #[test]
    fn token_value_keeps_exactly_80_characters_and_drops_81() {
        let at_cap = "a".repeat(MAX_VALUE_CHARS);
        let over = "a".repeat(MAX_VALUE_CHARS + 1);
        assert_eq!(
            token_value(&root(), &root().join(&at_cap)).as_deref(),
            Some(at_cap.as_str())
        );
        assert_eq!(token_value(&root(), &root().join(&over)), None);
    }

    #[test]
    fn token_value_counts_characters_not_bytes() {
        // 80 two-byte characters: 160 bytes, but within herdr's 80-character cap.
        let wide = "é".repeat(MAX_VALUE_CHARS);
        assert_eq!(
            token_value(&root(), &root().join(&wide)).as_deref(),
            Some(wide.as_str())
        );
    }

    #[test]
    fn token_value_refuses_what_herdr_would_alter() {
        for name in ["new\nline.txt", "tab\there.txt", " lead.txt", "trail.txt "] {
            assert_eq!(
                token_value(&root(), &root().join(name)),
                None,
                "{name:?} would not reach a reader unchanged"
            );
        }
    }

    #[test]
    fn token_value_refuses_a_name_the_open_target_reads_as_a_line() {
        assert_eq!(token_value(&root(), &root().join("notes:12")), None);
        assert_eq!(token_value(&root(), &root().join("notes:1-3")), None);
        // A colon that is not a line suffix round-trips.
        assert_eq!(
            token_value(&root(), &root().join("ab:cd.txt")).as_deref(),
            Some("ab:cd.txt")
        );
    }

    #[test]
    fn token_value_refuses_a_file_outside_the_root() {
        assert_eq!(token_value(&root(), Path::new("/elsewhere/x.rs")), None);
        assert_eq!(token_value(&root(), &root()), None);
    }

    #[cfg(unix)]
    #[test]
    fn token_value_refuses_a_non_utf8_path() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let name = OsStr::from_bytes(b"bad\xff.txt");
        assert_eq!(token_value(&root(), &root().join(name)), None);
    }

    /// Pins the argv verified against `herdr pane report-metadata --help` (herdr 0.9.1).
    #[test]
    fn report_argv_matches_the_verified_herdr_cli() {
        assert_eq!(
            report_argv("w1:p2", &Report::Set("src/app.rs".into())),
            [
                "pane",
                "report-metadata",
                "w1:p2",
                "--source",
                "herdr-file-viewer",
                "--token",
                "file_viewer_open=src/app.rs",
            ]
        );
        assert_eq!(
            report_argv("w1:p2", &Report::Clear),
            [
                "pane",
                "report-metadata",
                "w1:p2",
                "--source",
                "herdr-file-viewer",
                "--clear-token",
                "file_viewer_open",
            ]
        );
    }

    #[test]
    fn report_pane_needs_the_switch_and_a_pane_id() {
        assert_eq!(
            report_pane(true, Some("w1:p2".into())),
            Some("w1:p2".into())
        );
        assert_eq!(report_pane(false, Some("w1:p2".into())), None);
        assert_eq!(report_pane(true, None), None);
        assert_eq!(report_pane(true, Some("  ".into())), None);
    }

    #[test]
    fn observe_reports_a_change_once() {
        let mut state = OpenFileReport::new(root());
        let r = root();
        let a = r.join("a.rs");
        let b = r.join("b.rs");
        assert_eq!(
            state.observe(Some((&r, &a))),
            Some(Report::Set("a.rs".into()))
        );
        assert_eq!(state.observe(Some((&r, &a))), None, "unchanged: no report");
        assert_eq!(
            state.observe(Some((&r, &b))),
            Some(Report::Set("b.rs".into()))
        );
    }

    #[test]
    fn observe_sends_nothing_until_a_file_is_shown() {
        let mut state = OpenFileReport::new(root());
        assert_eq!(state.observe(None), None);
        assert_eq!(state.finish(), None, "nothing set, nothing to clear");
    }

    #[test]
    fn observe_clears_when_no_file_or_an_unreportable_file_is_shown() {
        let mut state = OpenFileReport::new(root());
        let r = root();
        let a = r.join("a.rs");
        let long = r.join("x".repeat(MAX_VALUE_CHARS + 1));
        let long2 = r.join("y".repeat(MAX_VALUE_CHARS + 1));
        state.observe(Some((&r, &a)));
        assert_eq!(state.observe(None), Some(Report::Clear));
        assert_eq!(state.observe(None), None);
        state.observe(Some((&r, &a)));
        assert_eq!(
            state.observe(Some((&r, &long))),
            Some(Report::Clear),
            "a stale value must not outlive a path too long to report"
        );
        assert_eq!(
            state.observe(Some((&r, &long2))),
            None,
            "already clear: no second clear"
        );
    }

    #[test]
    fn observe_clears_under_a_switched_root_and_reports_again_back_home() {
        let (home, other) = (PathBuf::from("/wt1"), PathBuf::from("/wt2"));
        let mut state = OpenFileReport::new(home.clone());
        assert_eq!(
            state.observe(Some((&home, &home.join("a.rs")))),
            Some(Report::Set("a.rs".into()))
        );
        assert_eq!(
            state.observe(Some((&other, &other.join("a.rs")))),
            Some(Report::Clear),
            "`a.rs` under /wt2 is not the `a.rs` a viewer restarted at /wt1 would open"
        );
        assert_eq!(
            state.observe(Some((&other, &other.join("b.rs")))),
            None,
            "already clear: no second clear"
        );
        assert_eq!(
            state.observe(Some((&home, &home.join("a.rs")))),
            Some(Report::Set("a.rs".into()))
        );
    }

    #[test]
    fn finish_clears_a_set_token() {
        let mut state = OpenFileReport::new(root());
        let r = root();
        state.observe(Some((&r, &r.join("a.rs"))));
        assert_eq!(state.finish(), Some(Report::Clear));
        assert_eq!(state.finish(), None);
    }
}
