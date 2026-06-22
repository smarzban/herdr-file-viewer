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
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use version::{latest_stable, newer_than_current};

/// Setting this env var (to anything) disables the update check and banner entirely.
pub const DISABLE_ENV: &str = "HERDR_FILE_VIEWER_NO_UPDATE_CHECK";

/// The injected probe runner: given the repo URL, returns `git ls-remote`-style stdout. Boxed
/// + `Send` so the background thread owns it; a type alias keeps signatures readable.
pub type ProbeRunner = Box<dyn Fn(&str) -> io::Result<String> + Send>;

/// How long `git ls-remote` may stall on a dead network before it aborts (seconds), so a
/// background probe can't leave a `git` child hanging indefinitely.
const PROBE_LOW_SPEED_TIME: &str = "5";

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

/// Run the injected probe and return its stdout, or `None` on any error. The runner is a seam
/// so tests never shell out / hit the network.
pub fn probe(run: impl Fn(&str) -> io::Result<String>, repo_url: &str) -> Option<String> {
    run(repo_url).ok()
}

/// Production probe runner: `git ls-remote --tags <url>`, with git's low-speed abort set so a
/// stalled network connection can't hang the background thread (and its child) forever. stderr
/// is discarded; a non-zero exit is an error (→ no banner).
pub fn run_git_ls_remote(repo_url: &str) -> io::Result<String> {
    let out = Command::new("git")
        .args(["ls-remote", "--tags", repo_url])
        .env("GIT_TERMINAL_PROMPT", "0") // never block on a credential prompt
        .env("GIT_HTTP_LOW_SPEED_LIMIT", "1000")
        .env("GIT_HTTP_LOW_SPEED_TIME", PROBE_LOW_SPEED_TIME)
        .stderr(std::process::Stdio::null())
        .output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(io::Error::other(format!(
            "git ls-remote exited with {}",
            out.status
        )))
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

/// Decide, then (if warranted) spawn the background probe. The thread probes, persists the
/// throttle cache, and sends the "version to show" (`Some` when newer, `None` when a successful
/// check found nothing) over the channel. On a probe *failure* it persists the advanced
/// timestamp but sends nothing, leaving any cached banner in place.
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
        let stdout = probe(|url| run(url), &repo_url);
        let succeeded = stdout.is_some();
        let latest = stdout.as_deref().and_then(latest_stable);
        if let Some(dir) = &cache_dir {
            cache::store(dir, &next_cache(now_unix, &cache, succeeded, latest));
        }
        if succeeded {
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
    fn probe_returns_stdout_on_success_and_none_on_error() {
        let ok = probe(|_url| Ok("aaa\trefs/tags/v1.1.0\n".to_string()), "url");
        assert_eq!(
            latest_stable(ok.as_deref().unwrap_or("")),
            Version::parse("1.1.0")
        );
        let err = probe(|_url| Err(io::Error::other("offline")), "url");
        assert_eq!(err, None);
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
