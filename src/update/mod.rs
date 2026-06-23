//! Update-available check — tell the user when a newer release exists.
//!
//! A bounded, read-only, fail-silent feature: once per 24h it runs `git ls-remote` against
//! our own repo (off the UI thread), compares the highest stable tag to the version compiled
//! into this binary, and — if behind — surfaces a one-line banner. Disabled entirely by the
//! `HERDR_FILE_VIEWER_NO_UPDATE_CHECK` env var. No new dependencies, no telemetry, no mutation.

pub mod cache;
pub mod version;

pub use version::Version;

use cache::{Cache, next_cache, should_check};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use version::{latest_stable, newer_than_current};

/// Setting this env var (to anything) disables the update check and banner entirely.
pub const DISABLE_ENV: &str = "HERDR_FILE_VIEWER_NO_UPDATE_CHECK";

/// The injected probe runner: given the repo URL, returns `git ls-remote`-style stdout. Boxed
/// + `Send` so the background thread owns it; a type alias keeps signatures readable.
pub type ProbeRunner = Box<dyn Fn(&str) -> io::Result<String> + Send>;

/// How long `git ls-remote` may stall mid-transfer before git itself aborts (seconds).
const PROBE_LOW_SPEED_TIME: &str = "5";

/// Hard wall-clock bound on the whole `git ls-remote` invocation. The low-speed settings only
/// cover a stalled HTTP *transfer*, not TCP connect / DNS — so a black-holed network could
/// otherwise hang (and orphan) the `git` child indefinitely. On overrun the child is killed and
/// the probe fails (→ no banner), matching the fail-silent contract.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The repository URL the probe queries (and the source of [`repo_slug`]).
pub fn repo_url() -> &'static str {
    env!("CARGO_PKG_REPOSITORY")
}

/// The `owner/repo` slug for the install command, derived from [`repo_url`].
pub fn repo_slug() -> &'static str {
    repo_url()
        .trim_end_matches('/')
        .trim_start_matches("https://github.com/")
        .trim_start_matches("http://github.com/")
}

/// The one-line footer shown when a newer release exists.
pub fn banner_text(v: &Version) -> String {
    format!(
        "↑ v{v} available · herdr plugin install {} · u to dismiss",
        repo_slug()
    )
}

/// Apply the security boundary for invoking `git` against an untrusted environment.
///
/// The viewed repository is **untrusted**, and `git` reads the repo-local `.git/config` of
/// whatever working directory it is in (URL `insteadOf` rewrites, credential helpers, …) — so an
/// attacker-planted `.git/config` could otherwise redirect or hijack this once-a-day probe. We:
/// - run in `run_dir`, which the caller guarantees is a **freshly-created private empty dir**
///   (so it cannot itself contain a `.git/config`), and ceiling discovery to it
///   (`GIT_CEILING_DIRECTORIES`) so git never walks up to find one — no repo-local config is read,
///   regardless of where herdr launched the pane;
/// - pin the transport to `https` (`GIT_ALLOW_PROTOCOL`), so even a (user-global) URL rewrite
///   can't redirect to a command-capable transport like `ext::` or `file://`;
/// - never prompt (`GIT_TERMINAL_PROMPT=0`).
///
/// The user's own global/system config is intentionally kept — it carries legitimate proxy / CA
/// settings and is in the user's own trust domain (only the *viewed repo* is untrusted).
fn harden_git(cmd: &mut Command, run_dir: &Path) {
    cmd.current_dir(run_dir)
        .env("GIT_CEILING_DIRECTORIES", run_dir)
        .env("GIT_ALLOW_PROTOCOL", "https")
        .env("GIT_TERMINAL_PROMPT", "0");
}

/// Build the hardened `git ls-remote --tags <url>` command, run from `run_dir` (see
/// [`harden_git`]). Constructed separately from [`run_git_ls_remote`] so the security boundary is
/// unit-testable without shelling out.
fn ls_remote_command(repo_url: &str, run_dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(["ls-remote", "--tags", repo_url]);
    harden_git(&mut cmd, run_dir);
    cmd.env("GIT_HTTP_LOW_SPEED_LIMIT", "1000")
        .env("GIT_HTTP_LOW_SPEED_TIME", PROBE_LOW_SPEED_TIME)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    cmd
}

/// Production probe runner. Runs the hardened `git ls-remote` from a **freshly-created private,
/// empty directory** — git reads a `.git/config` in its own cwd, so even the system temp dir
/// could (in principle) carry one; a directory we just made cannot. The directory is removed
/// afterwards. The whole invocation is bounded by [`PROBE_TIMEOUT`] so a connect/DNS hang can't
/// wedge or orphan the `git` child. `Err` on any failure — all of which degrade to "no banner".
pub fn run_git_ls_remote(repo_url: &str) -> io::Result<String> {
    let probe_dir = std::env::temp_dir().join(format!("herdr-fv-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&probe_dir); // clear any stale dir from a prior crashed run
    std::fs::create_dir_all(&probe_dir)?; // no clean dir → abort the probe (no banner)
    let result = run_ls_remote_in(repo_url, &probe_dir);
    let _ = std::fs::remove_dir_all(&probe_dir);
    result
}

/// Spawn the hardened `ls-remote` in `run_dir` and read its stdout, bounded by [`PROBE_TIMEOUT`].
fn run_ls_remote_in(repo_url: &str, run_dir: &Path) -> io::Result<String> {
    let mut child = ls_remote_command(repo_url, run_dir).spawn()?;
    // Read stdout on a worker thread so the wait can be bounded (a hung connect never writes).
    let stdout = child.stdout.take();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut out) = stdout {
            let _ = out.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    match rx.recv_timeout(PROBE_TIMEOUT) {
        Ok(buf) => match wait_bounded(&mut child, PROBE_TIMEOUT) {
            Some(status) if status.success() => Ok(String::from_utf8_lossy(&buf).into_owned()),
            Some(status) => Err(io::Error::other(format!(
                "git ls-remote exited with {status}"
            ))),
            None => Err(io::Error::other("git ls-remote did not exit")),
        },
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(io::Error::other("git ls-remote timed out"))
        }
    }
}

/// Wait for a child to exit within `grace`, killing and reaping it if it overruns.
fn wait_bounded(
    child: &mut std::process::Child,
    grace: Duration,
) -> Option<std::process::ExitStatus> {
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

/// The startup decision: what to show immediately (from cache) and whether to hit the network.
pub struct Decision {
    pub initial: Option<Version>,
    pub should_check: bool,
}

/// Pure startup decision. `initial` is the cached latest-seen version if it is newer than the
/// running build (and the feature is enabled); `should_check` is whether the 24h window has
/// elapsed (and the feature is enabled).
pub fn decide(disabled: bool, now_unix: u64, cache: &Option<Cache>) -> Decision {
    if disabled {
        return Decision {
            initial: None,
            should_check: false,
        };
    }
    let initial = cache
        .as_ref()
        .and_then(|c| c.latest_seen.as_deref())
        .and_then(Version::parse)
        .and_then(newer_than_current);
    let last = cache.as_ref().map(|c| c.last_check_unix).unwrap_or(0);
    Decision {
        initial,
        should_check: should_check(now_unix, last),
    }
}

/// Initial banner state + a one-shot receiver for the background check's result.
pub struct UpdateState {
    pub initial: Option<Version>,
    pub rx: Option<mpsc::Receiver<Option<Version>>>,
}

impl UpdateState {
    pub fn disabled() -> Self {
        UpdateState {
            initial: None,
            rx: None,
        }
    }
}

/// Injected dependencies for [`start_with`] — real values in [`start_default`], fakes in tests.
pub struct StartDeps {
    pub disabled: bool,
    pub now_unix: u64,
    pub cache: Option<Cache>,
    pub cache_dir: Option<PathBuf>,
    pub repo_url: String,
    pub run: ProbeRunner,
}

/// Decide, then (if warranted) spawn the background probe. On a **successful** probe the thread
/// persists the throttle cache (advancing the 24h window + the latest version seen) and sends the
/// "version to show" (`Some` when newer, `None` when nothing newer) over the channel. On a probe
/// **failure** it leaves the cache untouched — so the check simply retries next launch — and sends
/// nothing (the receiver then disconnects, which `Controller::poll` cleans up).
pub fn start_with(deps: StartDeps) -> UpdateState {
    let StartDeps {
        disabled,
        now_unix,
        cache,
        cache_dir,
        repo_url,
        run,
    } = deps;
    let decision = decide(disabled, now_unix, &cache);
    if !decision.should_check {
        return UpdateState {
            initial: decision.initial,
            rx: None,
        };
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // A probe failure leaves the cache as-is (retry next launch) and sends nothing.
        if let Ok(stdout) = run(&repo_url) {
            let latest = latest_stable(&stdout);
            if let Some(dir) = &cache_dir {
                cache::store(dir, &next_cache(now_unix, latest));
            }
            let _ = tx.send(latest.and_then(newer_than_current));
        }
    });
    UpdateState {
        initial: decision.initial,
        rx: Some(rx),
    }
}

/// The real entry point: read the env/clock/cache and use the `git` runner.
pub fn start_default() -> UpdateState {
    let disabled = std::env::var_os(DISABLE_ENV).is_some();
    if disabled {
        return UpdateState::disabled();
    }
    let cache_dir = cache::cache_dir();
    let cache = cache_dir.as_deref().and_then(cache::load);
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    start_with(StartDeps {
        disabled,
        now_unix,
        cache,
        cache_dir,
        repo_url: repo_url().to_string(),
        run: Box::new(run_git_ls_remote),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cache::CHECK_INTERVAL_SECS;
    use version::current;

    #[test]
    fn repo_slug_is_owner_repo() {
        // Derived from CARGO_PKG_REPOSITORY so it stays correct if the repo moves.
        assert_eq!(repo_slug(), "smarzban/herdr-file-viewer");
    }

    #[test]
    fn banner_names_the_version_and_install_command() {
        let v = Version {
            major: 1,
            minor: 1,
            patch: 0,
        };
        let b = banner_text(&v);
        assert!(b.contains("1.1.0"), "names the version: {b}");
        assert!(
            b.contains("herdr plugin install smarzban/herdr-file-viewer"),
            "shows install cmd: {b}"
        );
        assert!(b.contains('u'), "mentions the dismiss key: {b}");
    }

    #[test]
    fn ls_remote_command_is_hardened_against_untrusted_repo_config() {
        // Security regression: the probe must not let the (untrusted) viewed repo's git config
        // influence it. It runs from the given private run-dir with repo discovery ceilinged to
        // it, pins the transport to https, and never prompts — so no repo-local `.git/config` is
        // read (and the run-dir itself is a fresh empty dir, so it can't carry one either).
        use std::ffi::OsStr;
        let run_dir = std::path::Path::new("/some/private/probe-dir");
        let cmd = ls_remote_command(repo_url(), run_dir);
        let env: std::collections::HashMap<_, _> = cmd
            .get_envs()
            .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v.to_owned())))
            .collect();
        assert_eq!(
            cmd.get_current_dir(),
            Some(run_dir),
            "probe runs from its private run-dir, never the viewed repo / process cwd"
        );
        assert_eq!(
            env.get(OsStr::new("GIT_CEILING_DIRECTORIES"))
                .map(|v| v.as_os_str()),
            Some(run_dir.as_os_str()),
            "git must not walk up out of the run-dir to discover (and read) any repo's config"
        );
        assert_eq!(
            env.get(OsStr::new("GIT_ALLOW_PROTOCOL"))
                .map(|v| v.to_str().unwrap_or("")),
            Some("https"),
            "transport pinned to https so a URL rewrite can't reach ext::/file://"
        );
        assert_eq!(
            env.get(OsStr::new("GIT_TERMINAL_PROMPT"))
                .map(|v| v.to_str().unwrap_or("")),
            Some("0"),
            "a credential prompt must never block the probe"
        );
    }

    #[test]
    fn hardened_git_ignores_a_malicious_repo_local_insteadof() {
        // Round-2 regression: a malicious repo-local `url.*.insteadOf` must NOT rewrite the probe
        // URL when git runs under `harden_git` (fresh private dir + ceiling). `git ls-remote
        // --get-url` resolves the URL *without any network*, so this is hermetic.
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "hfv-insteadof-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let evil = base.join("evil-repo");
        let clean = base.join("clean");
        std::fs::create_dir_all(&evil).unwrap();
        std::fs::create_dir_all(&clean).unwrap();

        // Make `evil` a repo whose config rewrites our GitHub URL to an attacker host.
        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&evil)
            .status();
        if init.map(|s| !s.success()).unwrap_or(true) {
            let _ = std::fs::remove_dir_all(&base);
            return; // git unavailable → the construction test still covers the boundary
        }
        let _ = Command::new("git")
            .args([
                "config",
                "url.https://evil.invalid/.insteadOf",
                "https://github.com/",
            ])
            .current_dir(&evil)
            .status();

        let url = repo_url();
        let get_url = |cmd: &mut Command| -> String {
            cmd.args(["ls-remote", "--get-url", url]);
            let out = cmd.output().expect("git --get-url");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        // Precondition: run *inside* the evil repo with no hardening → the rewrite DOES apply.
        let mut unhardened = Command::new("git");
        unhardened.current_dir(&evil);
        assert!(
            get_url(&mut unhardened).contains("evil.invalid"),
            "precondition: the malicious repo-local insteadOf rewrites the URL"
        );
        // Hardened (fresh private dir): the rewrite must NOT apply — the URL is unchanged.
        let mut hardened = Command::new("git");
        harden_git(&mut hardened, &clean);
        assert_eq!(
            get_url(&mut hardened),
            url,
            "harden_git must ignore the repo-local insteadOf and keep the trusted URL"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn fresh_cache_shows_the_banner_without_probing() {
        // AC-U4: a fresh cache (within 24h) shows the cached banner and performs NO network call —
        // the probe runner must never be invoked, and no background check is scheduled.
        let newer = format!("{}.0.0", current().major + 1);
        let cache = Some(Cache {
            last_check_unix: 1_000,
            latest_seen: Some(newer.clone()),
        });
        let state = start_with(StartDeps {
            disabled: false,
            now_unix: 1_000 + 10, // well within the 24h window
            cache,
            cache_dir: None,
            repo_url: "x".into(),
            run: Box::new(|_| panic!("must not probe when the cache is fresh")),
        });
        assert_eq!(
            state.initial,
            Version::parse(&newer),
            "banner shown from cache"
        );
        assert!(
            state.rx.is_none(),
            "fresh cache → no background check scheduled"
        );
    }

    #[test]
    fn decide_uses_cache_for_the_initial_banner_and_gates_the_check() {
        let newer = format!("{}.{}.{}", current().major + 1, 0, 0);
        let cache = Some(Cache {
            last_check_unix: 1_000,
            latest_seen: Some(newer.clone()),
        });

        // Fresh cache (within 24h), behind → show banner from cache, no network.
        let d = decide(false, 1_000 + 10, &cache);
        assert_eq!(d.initial, Version::parse(&newer));
        assert!(!d.should_check, "fresh cache → no check");

        // Stale cache (>24h) → still show cached banner, AND check.
        let d = decide(false, 1_000 + CHECK_INTERVAL_SECS + 1, &cache);
        assert_eq!(d.initial, Version::parse(&newer));
        assert!(d.should_check, "stale → check");

        // Disabled → never a banner, never a check, whatever the cache says.
        let d = decide(true, 10_000_000, &cache);
        assert_eq!(d.initial, None);
        assert!(!d.should_check);

        // No cache → no initial banner, but do check (real clock vs last=0).
        let d = decide(false, 10_000_000, &None);
        assert_eq!(d.initial, None);
        assert!(d.should_check);

        // Cache says we're up-to-date (current version) → no banner.
        let same = current().to_string();
        let upcache = Some(Cache {
            last_check_unix: 0,
            latest_seen: Some(same),
        });
        assert_eq!(decide(false, 0, &upcache).initial, None);
    }

    #[test]
    fn start_with_delivers_a_newer_version_over_the_channel() {
        // A fake probe reporting a newer tag → the receiver yields it; no real network.
        let newer = current().major + 1;
        let stdout = format!("aaa\trefs/tags/v{newer}.0.0\n");
        let dir = std::env::temp_dir().join(format!("hfv-startwith-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = start_with(StartDeps {
            disabled: false,
            now_unix: CHECK_INTERVAL_SECS * 10, // force should_check
            cache: None,
            cache_dir: Some(dir.clone()),
            repo_url: "fake-url".to_string(),
            run: Box::new(move |_url| Ok(stdout.clone())),
        });
        let rx = state.rx.expect("a check was scheduled");
        let got = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("result arrives");
        assert_eq!(got, Version::parse(&format!("{newer}.0.0")));
        // And the cache was written so a re-run wouldn't re-probe.
        assert!(cache::load(&dir).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn start_with_disabled_does_nothing() {
        let state = start_with(StartDeps {
            disabled: true,
            now_unix: 0,
            cache: None,
            cache_dir: None,
            repo_url: "x".into(),
            run: Box::new(|_| panic!("must not probe when disabled")),
        });
        assert!(state.initial.is_none() && state.rx.is_none());
    }
}
