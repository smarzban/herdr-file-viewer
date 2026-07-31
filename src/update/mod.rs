//! Update-available check — tell the user when a newer release exists.
//!
//! A bounded, read-only, fail-silent feature: once per 24h it runs `git ls-remote` against
//! our own repo (off the UI thread), compares the highest stable tag to the version compiled
//! into this binary, and — if behind — surfaces a one-line banner. Disabled entirely by the
//! `HERDR_FILE_VIEWER_NO_UPDATE_CHECK` env var. No new dependencies, no telemetry, no mutation.

pub mod cache;
pub mod gateway;
pub mod release_policy;
pub mod spotlight_policy;
pub mod version;

pub use gateway::{DiscoveryRunner, ObjectId, ReleaseState, ReleaseTag, RemoteRef, Source};
pub use version::Version;

use cache::{Cache, next_cache, should_check};
use std::path::PathBuf;
use std::sync::mpsc;
use version::newer_than_current;

/// Setting this env var (to anything) disables the update check and banner entirely.
pub const DISABLE_ENV: &str = "HERDR_FILE_VIEWER_NO_UPDATE_CHECK";

/// The only authority the public-source gateway may query (and the source of [`repo_slug`]).
const OFFICIAL_REPOSITORY_URL: &str = "https://github.com/smarzban/herdr-file-viewer";

/// The fixed official repository HTTPS URL.
pub fn repo_url() -> &'static str {
    OFFICIAL_REPOSITORY_URL
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
    pub run: DiscoveryRunner,
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
        // An unavailable source leaves the cache as-is (retry next launch) and sends nothing.
        let deadline = std::time::Instant::now() + gateway::DISCOVERY_TIMEOUT;
        if let Source::Available(state) = run(deadline) {
            let latest = state.latest_release().map(|release| release.version);
            if let Some(dir) = &cache_dir {
                cache::store(
                    dir,
                    &next_cache(cache.unwrap_or_default(), now_unix, latest),
                );
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
    start_default_with(std::env::var_os(DISABLE_ENV).is_some())
}

/// Like [`start_default`] but with the disable decision **already made by the caller**, so the
/// resolved `config > env > default` precedence is not re-litigated here by re-reading
/// [`DISABLE_ENV`] (AC-3/AC-10). `app::run` passes the effective `update_check` through this: a
/// config `update_check = true` that already won over the env must not be silently vetoed by the
/// env a second time.
pub fn start_default_with(disabled: bool) -> UpdateState {
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
        run: Box::new(gateway::discover_release_state),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cache::CHECK_INTERVAL_SECS;
    use version::current;

    fn available_release(version: Version) -> Source<ReleaseState> {
        Source::Available(
            ReleaseState::new(
                RemoteRef::parse("refs/heads/main").unwrap(),
                ObjectId::parse("0123456789012345678901234567890123456789").unwrap(),
                vec![ReleaseTag::new(
                    version,
                    ObjectId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
                )],
            )
            .unwrap(),
        )
    }

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
    fn fresh_cache_shows_the_banner_without_probing() {
        // AC-U4: a fresh cache (within 24h) shows the cached banner and performs NO network call —
        // the probe runner must never be invoked, and no background check is scheduled.
        let newer = format!("{}.0.0", current().major + 1);
        let cache = Some(Cache {
            last_check_unix: 1_000,
            latest_seen: Some(newer.clone()),
            ..Cache::default()
        });
        let state = start_with(StartDeps {
            disabled: false,
            now_unix: 1_000 + 10, // well within the 24h window
            cache,
            cache_dir: None,
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
            ..Cache::default()
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
            ..Cache::default()
        });
        assert_eq!(decide(false, 0, &upcache).initial, None);
    }

    #[test]
    fn start_with_delivers_a_newer_version_over_the_channel() {
        // A fake probe reporting a newer tag → the receiver yields it; no real network.
        let newer = current().major + 1;
        let detected = Version::parse(&format!("{newer}.0.0")).unwrap();
        let dir = std::env::temp_dir().join(format!("hfv-startwith-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let spotlight = b"# Project\nexact spotlight bytes\n".to_vec();
        let state = start_with(StartDeps {
            disabled: false,
            now_unix: CHECK_INTERVAL_SECS * 10, // force should_check
            cache: Some(Cache {
                spotlight: Some(spotlight.clone()),
                spotlight_retrieved_at_unix: Some(1),
                dismissed_spotlight_identity: Some(spotlight.clone()),
                ..Cache::default()
            }),
            cache_dir: Some(dir.clone()),
            run: Box::new(move |_| available_release(detected)),
        });
        let rx = state.rx.expect("a check was scheduled");
        let got = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("result arrives");
        assert_eq!(got, Version::parse(&format!("{newer}.0.0")));
        let persisted = cache::load(&dir).expect("successful probe writes the cache");
        assert_eq!(
            persisted.spotlight.as_deref(),
            Some(spotlight.as_slice()),
            "the compatibility writer must not discard cached content before T-10 replaces it"
        );
        assert_eq!(persisted.spotlight_retrieved_at_unix, Some(1));
        assert_eq!(persisted.dismissed_spotlight_identity, Some(spotlight));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn start_with_disabled_does_nothing() {
        let state = start_with(StartDeps {
            disabled: true,
            now_unix: 0,
            cache: None,
            cache_dir: None,
            run: Box::new(|_| panic!("must not probe when disabled")),
        });
        assert!(state.initial.is_none() && state.rx.is_none());
    }

    #[test]
    fn start_default_with_honors_the_passed_decision_not_the_env() {
        // AC-3/AC-10 wiring regression: the update start must obey the ALREADY-RESOLVED
        // decision (config > env > default) the caller passes, NOT re-read
        // HERDR_FILE_VIEWER_NO_UPDATE_CHECK. Passing `disabled = true` yields the disabled
        // sentinel (no probe thread, no banner) — proving the arg governs. The enabled path
        // (`disabled = false`) is the env-free `start_with` already covered above; before the
        // fix, `app::run` routed through `start_default()`, letting a set env var silently veto a
        // config `update_check = true`.
        let state = start_default_with(true);
        assert!(
            state.initial.is_none() && state.rx.is_none(),
            "disabled=true → the disabled sentinel, regardless of the env"
        );
    }
}
