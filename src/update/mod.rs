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
pub mod status;
pub mod version;

pub use gateway::{DiscoveryRunner, ObjectId, ReleaseState, ReleaseTag, RemoteRef, Source};
pub use version::Version;

use cache::{Cache, CacheDelta, RefreshSpotlight};
use std::path::PathBuf;
use std::sync::mpsc;
use version::{current, newer_than_current};

/// One successful remote check suppresses another for this many seconds.
pub(crate) const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// One coordinator refresh shares this absolute deadline across discovery and both documents.
const REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

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

/// The startup decision: the cached notice snapshot and whether a remote refresh is eligible.
pub struct Decision {
    pub initial: NoticeSnapshot,
    pub should_refresh: bool,
}

/// Whether a successful daily check is old enough to permit one new refresh. A timestamp in the
/// future is eligible immediately rather than suppressing checks forever after clock skew.
fn refresh_eligible(now_unix: u64, last_check_unix: u64) -> bool {
    last_check_unix > now_unix || now_unix - last_check_unix >= CHECK_INTERVAL_SECS
}

/// Pure startup projection. It composes release-detail lifetime and spotlight freshness policies
/// over bounded cache facts, performs no I/O, and starts no background work.
pub fn decide(disabled: bool, session_started_at_unix: u64, cache: &Option<Cache>) -> Decision {
    if disabled {
        return Decision {
            initial: NoticeSnapshot::default(),
            should_refresh: false,
        };
    }

    let detected_release = cache
        .as_ref()
        .and_then(|cache| cache.latest_seen.as_deref())
        .and_then(Version::parse)
        .and_then(newer_than_current);
    let release_details = cached_release_details(cache, detected_release);
    let spotlight = cached_spotlight(cache, session_started_at_unix);

    Decision {
        initial: NoticeSnapshot {
            detected_release,
            release_details,
            spotlight,
            cache_writer: None,
        },
        should_refresh: cache
            .as_ref()
            .is_none_or(|cache| refresh_eligible(session_started_at_unix, cache.last_check_unix)),
    }
}

fn cached_release_details(
    cache: &Option<Cache>,
    detected_release: Option<Version>,
) -> Option<release_policy::CachedReleaseDetails> {
    let cached = cache.as_ref().and_then(|cache| {
        let details = cache.release_details.as_ref()?;
        Some(release_policy::CachedReleaseDetails {
            release: Version::parse(&details.release)?,
            details: details.details.clone(),
        })
    });

    match release_policy::cached_release_details(current(), detected_release, cached.as_ref()) {
        release_policy::CachedReleaseDetailsDecision::Cached(details) => Some(details.clone()),
        release_policy::CachedReleaseDetailsDecision::Hidden
        | release_policy::CachedReleaseDetailsDecision::Fetch(_) => None,
    }
}

fn cached_spotlight(
    cache: &Option<Cache>,
    session_started_at_unix: u64,
) -> spotlight_policy::SpotlightCache {
    let Some(cache) = cache else {
        return spotlight_policy::SpotlightCache::default();
    };
    let mut projected = cache
        .dismissed_spotlight_identity
        .clone()
        .map(spotlight_policy::SpotlightCache::with_remembered_dismissal)
        .unwrap_or_default();
    let Some(retrieved_at_unix) = cache.spotlight_retrieved_at_unix else {
        return projected;
    };
    let projection = match (
        cache.spotlight.as_ref(),
        spotlight_policy::freshness_at_session_start(session_started_at_unix, retrieved_at_unix),
    ) {
        (_, spotlight_policy::Freshness::Future) => return projected,
        (Some(spotlight), spotlight_policy::Freshness::Fresh) => spotlight_policy::project(
            spotlight_policy::SpotlightInput::Available(spotlight.clone()),
        ),
        (Some(_), spotlight_policy::Freshness::Stale) | (None, _) => {
            spotlight_policy::SpotlightProjection::Withdrawn
        }
    };
    projected.apply(spotlight_policy::cache_delta(projection, retrieved_at_unix));
    projected
}

/// The complete remote-notice state visible to one controller frame.
///
/// The controller replaces this value only as a whole, so a completed background result cannot
/// apply a new release while leaving an old spotlight behind. The writer is a handle only: it
/// queues bounded cache work on its own worker and never waits during a redraw.
#[derive(Clone, Default)]
pub struct NoticeSnapshot {
    /// The latest stable release detected for the fixed official repository, if it is newer than
    /// the running build.
    pub detected_release: Option<Version>,
    /// Immutable details only when they remain tied to the current detected release.
    pub release_details: Option<release_policy::CachedReleaseDetails>,
    /// The current project spotlight state, projected from the cache or the same completed
    /// refresh that supplied the release state.
    pub spotlight: spotlight_policy::SpotlightCache,
    /// Best-effort cache persistence for future notice intents. `None` when no cache directory
    /// is available or when update checks are disabled.
    pub cache_writer: Option<cache::CacheWriter>,
}

/// Initial notice state + a one-shot receiver for the background check's replacement snapshot.
pub struct UpdateState {
    pub initial: NoticeSnapshot,
    pub rx: Option<mpsc::Receiver<NoticeSnapshot>>,
}

impl UpdateState {
    pub fn disabled() -> Self {
        UpdateState {
            initial: NoticeSnapshot::default(),
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
    pub gateway: Box<dyn gateway::Gateway>,
}

/// Decide, then (if warranted) run one complete coordinator refresh off the UI thread.
///
/// Discovery and both optional document requests receive the same absolute deadline. A determined
/// discovery always advances the daily check through the asynchronous writer, while unavailable
/// documents preserve their independent valid cache state. An unavailable discovery sends no
/// replacement and leaves the successful-check timestamp untouched.
pub fn start_with(deps: StartDeps) -> UpdateState {
    let StartDeps {
        disabled,
        now_unix,
        cache,
        cache_dir,
        run,
        gateway,
    } = deps;
    let decision = decide(disabled, now_unix, &cache);
    let mut initial = decision.initial;
    initial.cache_writer = if disabled {
        None
    } else {
        cache_dir
            .as_ref()
            .map(|dir| cache::CacheWriter::new(dir.clone()))
    };
    if !decision.should_refresh {
        return UpdateState { initial, rx: None };
    }

    let refresh_initial = initial.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + REFRESH_TIMEOUT;
        let Source::Available(state) = run(deadline) else {
            return;
        };

        let latest_release = state.latest_release();
        let latest = latest_release.map(|release| release.version);
        let detected_release = latest.and_then(newer_than_current);
        let (release_details, persisted_release_details) =
            match release_policy::cached_release_details(
                current(),
                detected_release,
                refresh_initial.release_details.as_ref(),
            ) {
                release_policy::CachedReleaseDetailsDecision::Cached(details) => {
                    (Some(details.clone()), None)
                }
                release_policy::CachedReleaseDetailsDecision::Hidden => (None, None),
                release_policy::CachedReleaseDetailsDecision::Fetch(detected) => {
                    let fetched = latest_release.and_then(|release| {
                        let Source::Available(Some(bytes)) = gateway.changelog(release, deadline)
                        else {
                            return None;
                        };
                        let changelog = std::str::from_utf8(&bytes).ok()?;
                        let sections = release_policy::eligible_release_sections(
                            changelog,
                            current(),
                            detected,
                        );
                        (!sections.is_empty()).then(|| release_policy::CachedReleaseDetails {
                            release: detected,
                            details: sections.concat(),
                        })
                    });
                    let persisted =
                        fetched
                            .as_ref()
                            .map(|details| cache::PersistedReleaseDetails {
                                release: details.release.to_string(),
                                details: details.details.clone(),
                            });
                    (fetched, persisted)
                }
            };

        let mut spotlight = refresh_initial.spotlight.clone();
        let mut spotlight_session = spotlight_policy::SpotlightSession::new(now_unix);
        let spotlight_delta = if spotlight_session.should_retrieve(&spotlight) {
            let source = match gateway.spotlight(&state, deadline) {
                Source::Available(Some(bytes)) => {
                    spotlight_policy::SpotlightInput::Available(bytes)
                }
                Source::Available(None) => spotlight_policy::SpotlightInput::Missing,
                Source::Unavailable => spotlight_policy::SpotlightInput::Unavailable,
            };
            let policy_delta =
                spotlight_policy::cache_delta(spotlight_policy::project(source), now_unix);
            let cache_delta = match &policy_delta {
                spotlight_policy::SpotlightCacheDelta::Accepted {
                    spotlight,
                    retrieved_at_unix,
                } => RefreshSpotlight::Store {
                    spotlight: spotlight.identity.clone(),
                    retrieved_at_unix: *retrieved_at_unix,
                },
                spotlight_policy::SpotlightCacheDelta::Withdrawn { retrieved_at_unix } => {
                    RefreshSpotlight::Withdraw {
                        retrieved_at_unix: *retrieved_at_unix,
                    }
                }
                spotlight_policy::SpotlightCacheDelta::Preserve => RefreshSpotlight::Preserve,
            };
            spotlight_session.apply(&mut spotlight, policy_delta);
            cache_delta
        } else {
            RefreshSpotlight::Preserve
        };

        if let Some(writer) = &refresh_initial.cache_writer {
            let _ = writer.enqueue(CacheDelta::Refresh {
                checked_at_unix: now_unix,
                detected_release: latest.map(|version| version.to_string()),
                release_details: persisted_release_details,
                spotlight: spotlight_delta,
            });
        }
        let _ = tx.send(NoticeSnapshot {
            detected_release,
            release_details,
            spotlight,
            cache_writer: refresh_initial.cache_writer,
        });
    });
    UpdateState {
        initial,
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
        gateway: Box::new(gateway::DocumentGateway::new()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use version::current;

    fn future_version(offset: u32) -> Version {
        Version {
            major: current().major + offset,
            minor: 0,
            patch: 0,
        }
    }

    #[test]
    fn startup_decision_disabled_returns_an_empty_snapshot_and_skips_refresh() {
        let release = future_version(1);
        let spotlight = b"# Project\nbody\n".to_vec();
        let cache = Some(Cache {
            last_check_unix: 10,
            latest_seen: Some(release.to_string()),
            release_details: Some(cache::PersistedReleaseDetails {
                release: release.to_string(),
                details: "## [next]\n- cached details\n".into(),
            }),
            spotlight: Some(spotlight.clone()),
            spotlight_retrieved_at_unix: Some(10_000),
            dismissed_spotlight_identity: Some(spotlight),
            ..Cache::default()
        });

        let decision = decide(true, 10_000, &cache);

        assert!(decision.initial.detected_release.is_none());
        assert!(decision.initial.release_details.is_none());
        assert!(decision.initial.spotlight.status_title().is_none());
        assert!(decision.initial.spotlight.whats_new_body().is_none());
        assert!(decision.initial.cache_writer.is_none());
        assert!(!decision.should_refresh);
    }

    #[test]
    fn startup_decision_throttles_a_cache_younger_than_one_day() {
        let last_check_unix = 10_000;
        let decision = decide(
            false,
            last_check_unix + CHECK_INTERVAL_SECS - 1,
            &Some(Cache {
                last_check_unix,
                ..Cache::default()
            }),
        );

        assert!(
            !decision.should_refresh,
            "strictly less than 86,400 elapsed seconds is throttled"
        );
    }

    #[test]
    fn startup_decision_refreshes_at_the_boundary_and_for_a_future_cache_clock() {
        let last_check_unix = 10_000;
        let cache = Some(Cache {
            last_check_unix,
            ..Cache::default()
        });

        assert!(
            decide(false, last_check_unix + CHECK_INTERVAL_SECS, &cache).should_refresh,
            "exactly 86,400 elapsed seconds is eligible"
        );
        assert!(
            decide(false, last_check_unix - 1, &cache).should_refresh,
            "a future cache timestamp is eligible rather than throttling forever"
        );
    }

    #[test]
    fn startup_decision_projects_details_only_for_the_matching_newer_release() {
        let release = future_version(1);
        let details = "## [next]\n- cached details\n";
        let decision = decide(
            false,
            10_000,
            &Some(Cache {
                latest_seen: Some(release.to_string()),
                release_details: Some(cache::PersistedReleaseDetails {
                    release: release.to_string(),
                    details: details.into(),
                }),
                ..Cache::default()
            }),
        );

        assert_eq!(decision.initial.detected_release, Some(release));
        let projected = decision
            .initial
            .release_details
            .as_ref()
            .expect("matching newer release details remain usable");
        assert_eq!(projected.release, release);
        assert_eq!(projected.details, details);
    }

    #[test]
    fn startup_decision_hides_caught_up_and_superseded_release_details() {
        let caught_up = current();
        let older = future_version(1);
        let newer = future_version(2);

        let caught_up_decision = decide(
            false,
            10_000,
            &Some(Cache {
                latest_seen: Some(caught_up.to_string()),
                release_details: Some(cache::PersistedReleaseDetails {
                    release: caught_up.to_string(),
                    details: "caught up details".into(),
                }),
                ..Cache::default()
            }),
        );
        assert!(caught_up_decision.initial.detected_release.is_none());
        assert!(caught_up_decision.initial.release_details.is_none());

        let superseded_decision = decide(
            false,
            10_000,
            &Some(Cache {
                latest_seen: Some(newer.to_string()),
                release_details: Some(cache::PersistedReleaseDetails {
                    release: older.to_string(),
                    details: "superseded details".into(),
                }),
                ..Cache::default()
            }),
        );
        assert_eq!(superseded_decision.initial.detected_release, Some(newer));
        assert!(superseded_decision.initial.release_details.is_none());
    }

    #[test]
    fn startup_decision_projects_fresh_spotlight_and_hides_stale_spotlight() {
        let session_started_at_unix = 1_000_000;
        let spotlight = b"# Project\nbody\n".to_vec();
        let fresh = decide(
            false,
            session_started_at_unix,
            &Some(Cache {
                spotlight: Some(spotlight.clone()),
                spotlight_retrieved_at_unix: Some(
                    session_started_at_unix - CHECK_INTERVAL_SECS + 1,
                ),
                ..Cache::default()
            }),
        )
        .initial;
        assert_eq!(fresh.spotlight.status_title(), Some("Project"));
        assert_eq!(fresh.spotlight.whats_new_body(), Some(b"body\n".as_slice()));

        let stale = decide(
            false,
            session_started_at_unix,
            &Some(Cache {
                spotlight: Some(spotlight),
                spotlight_retrieved_at_unix: Some(session_started_at_unix - CHECK_INTERVAL_SECS),
                ..Cache::default()
            }),
        )
        .initial;
        assert!(stale.spotlight.status_title().is_none());
        assert!(stale.spotlight.whats_new_body().is_none());
    }

    #[test]
    fn startup_decision_preserves_withdrawal_freshness_without_fabricating_content() {
        let session_started_at_unix = 1_000_000;
        let fresh_retrieved_at_unix = session_started_at_unix - CHECK_INTERVAL_SECS + 1;
        let fresh = decide(
            false,
            session_started_at_unix,
            &Some(Cache {
                spotlight_retrieved_at_unix: Some(fresh_retrieved_at_unix),
                ..Cache::default()
            }),
        )
        .initial
        .spotlight;

        assert_eq!(fresh.retrieved_at_unix(), Some(fresh_retrieved_at_unix));
        assert!(fresh.status_title().is_none());
        assert!(fresh.whats_new_body().is_none());
        assert!(
            !spotlight_policy::SpotlightSession::new(session_started_at_unix)
                .should_retrieve(&fresh),
            "a fresh persisted withdrawal remains throttled for this session"
        );

        let stale = decide(
            false,
            session_started_at_unix,
            &Some(Cache {
                spotlight_retrieved_at_unix: Some(session_started_at_unix - CHECK_INTERVAL_SECS),
                ..Cache::default()
            }),
        )
        .initial
        .spotlight;
        assert_eq!(
            stale.retrieved_at_unix(),
            Some(session_started_at_unix - CHECK_INTERVAL_SECS)
        );
        assert!(
            spotlight_policy::SpotlightSession::new(session_started_at_unix)
                .should_retrieve(&stale)
        );

        let unavailable = decide(false, session_started_at_unix, &Some(Cache::default()))
            .initial
            .spotlight;
        assert_eq!(unavailable.retrieved_at_unix(), None);
        assert!(
            spotlight_policy::SpotlightSession::new(session_started_at_unix)
                .should_retrieve(&unavailable)
        );

        let invalid = decide(
            false,
            session_started_at_unix,
            &Some(Cache {
                spotlight: Some(b"not a spotlight document\n".to_vec()),
                spotlight_retrieved_at_unix: Some(fresh_retrieved_at_unix),
                ..Cache::default()
            }),
        )
        .initial
        .spotlight;
        assert_eq!(invalid.retrieved_at_unix(), Some(fresh_retrieved_at_unix));
        assert!(invalid.status_title().is_none());
        assert!(invalid.whats_new_body().is_none());
        assert!(
            !spotlight_policy::SpotlightSession::new(session_started_at_unix)
                .should_retrieve(&invalid),
            "invalid present content is a conclusive withdrawal, not an unavailable result"
        );
    }

    #[test]
    fn stale_spotlight_keeps_its_dismissal_for_an_identical_refresh() {
        let session_started_at_unix = 1_000_000;
        let spotlight = b"# Project\nbody\n".to_vec();
        let mut refreshed = decide(
            false,
            session_started_at_unix,
            &Some(Cache {
                spotlight: Some(spotlight.clone()),
                spotlight_retrieved_at_unix: Some(session_started_at_unix - CHECK_INTERVAL_SECS),
                dismissed_spotlight_identity: Some(spotlight.clone()),
                ..Cache::default()
            }),
        )
        .initial
        .spotlight;

        assert!(refreshed.status_title().is_none());
        assert!(refreshed.whats_new_body().is_none());

        refreshed.apply(spotlight_policy::cache_delta(
            spotlight_policy::project(spotlight_policy::SpotlightInput::Available(spotlight)),
            session_started_at_unix,
        ));

        assert!(
            refreshed.status_title().is_none(),
            "an identical refresh retains the persisted dismissal"
        );
        assert_eq!(
            refreshed.whats_new_body(),
            Some(b"body\n".as_slice()),
            "fresh content restores What's New independently of dismissal"
        );
    }

    #[test]
    fn startup_decision_keeps_a_remembered_dismissal_out_of_status_but_in_whats_new() {
        let session_started_at_unix = 1_000_000;
        let spotlight = b"# Project\nbody\n".to_vec();
        let initial = decide(
            false,
            session_started_at_unix,
            &Some(Cache {
                spotlight: Some(spotlight.clone()),
                spotlight_retrieved_at_unix: Some(session_started_at_unix - 1),
                dismissed_spotlight_identity: Some(spotlight),
                ..Cache::default()
            }),
        )
        .initial;

        assert!(initial.spotlight.status_title().is_none());
        assert_eq!(
            initial.spotlight.whats_new_body(),
            Some(b"body\n".as_slice()),
            "dismissal never suppresses accepted What's New content"
        );
    }

    #[test]
    fn startup_decision_fixes_spotlight_freshness_at_session_start() {
        let session_started_at_unix = 1_000_000;
        let cache = Some(Cache {
            spotlight: Some(b"# Project\nbody\n".to_vec()),
            spotlight_retrieved_at_unix: Some(session_started_at_unix - CHECK_INTERVAL_SECS + 1),
            ..Cache::default()
        });
        let initial = decide(false, session_started_at_unix, &cache).initial;
        let later = decide(false, session_started_at_unix + CHECK_INTERVAL_SECS, &cache).initial;

        assert_eq!(initial.spotlight.status_title(), Some("Project"));
        assert!(
            later.spotlight.status_title().is_none(),
            "a later session independently re-evaluates its own startup freshness"
        );
        assert_eq!(
            initial.spotlight.status_title(),
            Some("Project"),
            "the existing session keeps its startup projection without a live-clock recheck"
        );
    }

    fn available_releases(releases: Vec<ReleaseTag>) -> Source<ReleaseState> {
        Source::Available(
            ReleaseState::new(
                RemoteRef::parse("refs/heads/main").unwrap(),
                ObjectId::parse("0123456789012345678901234567890123456789").unwrap(),
                releases,
            )
            .unwrap(),
        )
    }

    fn available_release(version: Version) -> Source<ReleaseState> {
        available_releases(vec![ReleaseTag::new(
            version,
            ObjectId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        )])
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
            gateway: unavailable_test_gateway(),
        });
        assert_eq!(
            state.initial.detected_release,
            Version::parse(&newer),
            "banner shown from cache"
        );
        assert!(
            state.rx.is_none(),
            "fresh cache → no background check scheduled"
        );
    }

    #[test]
    fn startup_decision_uses_cache_for_the_initial_banner_and_gates_the_check() {
        let newer = format!("{}.{}.{}", current().major + 1, 0, 0);
        let cache = Some(Cache {
            last_check_unix: 1_000,
            latest_seen: Some(newer.clone()),
            ..Cache::default()
        });

        // Fresh cache (within 24h), behind → show banner from cache, no network.
        let d = decide(false, 1_000 + 10, &cache);
        assert_eq!(d.initial.detected_release, Version::parse(&newer));
        assert!(!d.should_refresh, "fresh cache → no check");

        // Stale cache (>24h) → still show cached banner, AND check.
        let d = decide(false, 1_000 + CHECK_INTERVAL_SECS + 1, &cache);
        assert_eq!(d.initial.detected_release, Version::parse(&newer));
        assert!(d.should_refresh, "stale → check");

        // Disabled → never a banner, never a check, whatever the cache says.
        let d = decide(true, 10_000_000, &cache);
        assert!(d.initial.detected_release.is_none());
        assert!(!d.should_refresh);

        // No cache → no initial banner, but do check.
        let d = decide(false, 10_000_000, &None);
        assert!(d.initial.detected_release.is_none());
        assert!(d.should_refresh);

        // Cache says we're up-to-date (current version) → no banner.
        let same = current().to_string();
        let upcache = Some(Cache {
            last_check_unix: 0,
            latest_seen: Some(same),
            ..Cache::default()
        });
        assert!(
            decide(false, 0, &upcache)
                .initial
                .detected_release
                .is_none()
        );
    }

    #[test]
    fn refresh_worker_keeps_details_for_the_same_release() {
        let detected = future_version(1);
        let details = "## [next]\n- cached details\n";
        let dir = std::env::temp_dir().join(format!("hfv-startwith-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let spotlight = b"# Project\nexact spotlight bytes\n".to_vec();
        let cached = Cache {
            latest_seen: Some(detected.to_string()),
            release_details: Some(cache::PersistedReleaseDetails {
                release: detected.to_string(),
                details: details.into(),
            }),
            spotlight: Some(spotlight.clone()),
            spotlight_retrieved_at_unix: Some(CHECK_INTERVAL_SECS * 9 + 1),
            ..Cache::default()
        };
        cache::store(&dir, &cached);
        let state = start_with(StartDeps {
            disabled: false,
            now_unix: CHECK_INTERVAL_SECS * 10,
            cache: Some(cached),
            cache_dir: Some(dir.clone()),
            run: Box::new(move |_| available_release(detected)),
            gateway: unavailable_test_gateway(),
        });
        let UpdateState { initial, rx } = state;
        let got = rx
            .expect("a check was scheduled")
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("result arrives");
        assert_eq!(got.detected_release, Some(detected));
        assert_eq!(
            got.release_details,
            Some(release_policy::CachedReleaseDetails {
                release: detected,
                details: details.into(),
            }),
            "a successful probe for the same release keeps exact cached details"
        );
        assert_eq!(got.spotlight.status_title(), Some("Project"));
        drop(got);
        drop(initial);
        let persisted = cache::load(&dir).expect("successful probe writes the cache");
        assert_eq!(persisted.spotlight.as_deref(), Some(spotlight.as_slice()));
        assert_eq!(
            persisted.spotlight_retrieved_at_unix,
            Some(CHECK_INTERVAL_SECS * 9 + 1)
        );
        assert!(persisted.dismissed_spotlight_identity.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_worker_clears_details_for_a_superseding_release() {
        let cached_release = future_version(1);
        let superseding_release = future_version(2);
        let state = start_with(StartDeps {
            disabled: false,
            now_unix: CHECK_INTERVAL_SECS * 10,
            cache: Some(Cache {
                latest_seen: Some(cached_release.to_string()),
                release_details: Some(cache::PersistedReleaseDetails {
                    release: cached_release.to_string(),
                    details: "## [cached]\n- details\n".into(),
                }),
                ..Cache::default()
            }),
            cache_dir: None,
            run: Box::new(move |_| available_release(superseding_release)),
            gateway: unavailable_test_gateway(),
        });
        assert!(
            state.initial.release_details.is_some(),
            "the valid cached state reaches the compatibility worker"
        );

        let got = state
            .rx
            .expect("a check was scheduled")
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("result arrives");

        assert_eq!(got.detected_release, Some(superseding_release));
        assert!(
            got.release_details.is_none(),
            "details for an older release must not accompany a newer detected release"
        );
    }

    #[test]
    fn refresh_worker_clears_cached_release_when_no_stable_tag_exists() {
        let cached_release = future_version(1);
        let dir = std::env::temp_dir().join(format!("hfv-no-release-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cached = Cache {
            latest_seen: Some(cached_release.to_string()),
            release_details: Some(cache::PersistedReleaseDetails {
                release: cached_release.to_string(),
                details: "## [cached]\n- details\n".into(),
            }),
            ..Cache::default()
        };
        cache::store(&dir, &cached);
        let state = start_with(StartDeps {
            disabled: false,
            now_unix: CHECK_INTERVAL_SECS * 10,
            cache: Some(cached),
            cache_dir: Some(dir.clone()),
            run: Box::new(|_| available_releases(Vec::new())),
            gateway: unavailable_test_gateway(),
        });

        let UpdateState { initial, rx } = state;
        let got = rx
            .expect("a check was scheduled")
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("result arrives");

        assert!(got.detected_release.is_none());
        assert!(got.release_details.is_none());
        drop(got);
        drop(initial);
        let persisted = cache::load(&dir).expect("successful probe writes the cache");
        assert_eq!(persisted.last_check_unix, CHECK_INTERVAL_SECS * 10);
        assert!(persisted.latest_seen.is_none());
        assert!(persisted.release_details.is_none());
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
            gateway: unavailable_test_gateway(),
        });
        assert!(
            state.initial.detected_release.is_none()
                && state.initial.cache_writer.is_none()
                && state.rx.is_none()
        );
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
            state.initial.detected_release.is_none()
                && state.initial.cache_writer.is_none()
                && state.rx.is_none(),
            "disabled=true → the disabled sentinel, regardless of the env"
        );
    }

    #[derive(Clone)]
    struct TestGateway {
        changelog: Source<Option<Vec<u8>>>,
        spotlight: Source<Option<Vec<u8>>>,
    }

    impl gateway::Gateway for TestGateway {
        fn changelog(
            &self,
            _release: &ReleaseTag,
            _deadline: std::time::Instant,
        ) -> Source<Option<Vec<u8>>> {
            self.changelog.clone()
        }

        fn spotlight(
            &self,
            _state: &ReleaseState,
            _deadline: std::time::Instant,
        ) -> Source<Option<Vec<u8>>> {
            self.spotlight.clone()
        }
    }

    fn unavailable_test_gateway() -> Box<dyn gateway::Gateway> {
        Box::new(TestGateway {
            changelog: Source::Unavailable,
            spotlight: Source::Unavailable,
        })
    }

    struct DeadlineGateway {
        deadlines: std::sync::Arc<std::sync::Mutex<Vec<std::time::Instant>>>,
        changelog: Source<Option<Vec<u8>>>,
        spotlight: Source<Option<Vec<u8>>>,
    }

    impl gateway::Gateway for DeadlineGateway {
        fn changelog(
            &self,
            _release: &ReleaseTag,
            deadline: std::time::Instant,
        ) -> Source<Option<Vec<u8>>> {
            self.deadlines.lock().unwrap().push(deadline);
            self.changelog.clone()
        }

        fn spotlight(
            &self,
            _state: &ReleaseState,
            deadline: std::time::Instant,
        ) -> Source<Option<Vec<u8>>> {
            self.deadlines.lock().unwrap().push(deadline);
            self.spotlight.clone()
        }
    }

    fn refresh_with(
        cache: Option<Cache>,
        run: DiscoveryRunner,
        gateway: TestGateway,
    ) -> UpdateState {
        start_with(StartDeps {
            disabled: false,
            now_unix: CHECK_INTERVAL_SECS * 10,
            cache,
            cache_dir: None,
            run,
            gateway: Box::new(gateway),
        })
    }

    fn refreshed(state: UpdateState) -> NoticeSnapshot {
        state
            .rx
            .expect("stale cache schedules one refresh")
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("determined discovery publishes one replacement snapshot")
    }

    #[test]
    fn failed_discovery_preserves_the_successful_check_time() {
        let dir =
            std::env::temp_dir().join(format!("hfv-discovery-failure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cached = Cache {
            last_check_unix: CHECK_INTERVAL_SECS * 8,
            latest_seen: Some(future_version(1).to_string()),
            ..Cache::default()
        };
        cache::store(&dir, &cached);
        let state = start_with(StartDeps {
            disabled: false,
            now_unix: CHECK_INTERVAL_SECS * 10,
            cache: Some(cached),
            cache_dir: Some(dir.clone()),
            run: Box::new(|_| Source::Unavailable),
            gateway: unavailable_test_gateway(),
        });

        assert_eq!(
            state.initial.detected_release,
            Some(future_version(1)),
            "a failed discovery leaves the initial cached state alone"
        );
        let UpdateState { initial, rx } = state;
        assert!(matches!(
            rx.expect("failure still closes the one-shot receiver")
                .recv_timeout(std::time::Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
        drop(initial);
        assert_eq!(
            cache::load(&dir)
                .expect("unavailable discovery preserves the prior cache")
                .last_check_unix,
            CHECK_INTERVAL_SECS * 8,
            "unavailable discovery cannot advance the only successful-check time"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn determined_discovery_advances_its_timestamp_through_every_document_failure() {
        let now = CHECK_INTERVAL_SECS * 10;
        let dir = std::env::temp_dir().join(format!("hfv-document-failure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cached = Cache {
            last_check_unix: CHECK_INTERVAL_SECS * 8,
            ..Cache::default()
        };
        cache::store(&dir, &cached);
        let state = start_with(StartDeps {
            disabled: false,
            now_unix: now,
            cache: Some(cached),
            cache_dir: Some(dir.clone()),
            run: Box::new(move |_| available_release(future_version(1))),
            gateway: Box::new(TestGateway {
                changelog: Source::Unavailable,
                spotlight: Source::Unavailable,
            }),
        });
        let UpdateState { initial, rx } = state;
        let snapshot = rx
            .expect("stale cache schedules one refresh")
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("determined discovery publishes one replacement snapshot");

        assert_eq!(snapshot.detected_release, Some(future_version(1)));
        assert!(snapshot.release_details.is_none());
        assert!(snapshot.spotlight.status_title().is_none());
        drop(snapshot);
        drop(initial);
        let persisted = cache::load(&dir).expect("the async refresh writer persists its one delta");
        assert_eq!(persisted.last_check_unix, now);
        assert_eq!(persisted.latest_seen, Some(future_version(1).to_string()));
        assert!(persisted.release_details.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_passes_one_absolute_deadline_to_discovery_and_both_documents() {
        let release = future_version(1);
        let deadlines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let discovery_deadlines = std::sync::Arc::clone(&deadlines);
        let state = start_with(StartDeps {
            disabled: false,
            now_unix: CHECK_INTERVAL_SECS * 10,
            cache: Some(Cache::default()),
            cache_dir: None,
            run: Box::new(move |deadline| {
                discovery_deadlines.lock().unwrap().push(deadline);
                available_release(release)
            }),
            gateway: Box::new(DeadlineGateway {
                deadlines: std::sync::Arc::clone(&deadlines),
                changelog: Source::Available(Some(
                    format!("## [{release}]\n- details\n").into_bytes(),
                )),
                spotlight: Source::Available(Some(b"# Project\nbody\n".to_vec())),
            }),
        });
        let _ = refreshed(state);
        let deadlines = deadlines.lock().unwrap();
        assert_eq!(deadlines.len(), 3, "discovery plus both documents run once");
        assert!(
            deadlines.windows(2).all(|pair| pair[0] == pair[1]),
            "every source operation receives the coordinator's one absolute deadline"
        );
    }

    #[test]
    fn changelog_and_spotlight_partial_successes_are_independent() {
        let release = future_version(1);
        let changelog = format!("## [{release}]\n- details\n");
        let details = refreshed(refresh_with(
            Some(Cache::default()),
            Box::new(move |_| available_release(release)),
            TestGateway {
                changelog: Source::Available(Some(changelog.into_bytes())),
                spotlight: Source::Unavailable,
            },
        ));
        assert_eq!(
            details.release_details,
            Some(release_policy::CachedReleaseDetails {
                release,
                details: format!("## [{release}]\n- details\n"),
            })
        );
        assert!(details.spotlight.status_title().is_none());

        let spotlight = refreshed(refresh_with(
            Some(Cache::default()),
            Box::new(move |_| available_release(release)),
            TestGateway {
                changelog: Source::Unavailable,
                spotlight: Source::Available(Some(b"# Project\nbody\n".to_vec())),
            },
        ));
        assert!(spotlight.release_details.is_none());
        assert_eq!(spotlight.spotlight.status_title(), Some("Project"));
    }

    #[test]
    fn stale_spotlight_refresh_accepts_or_withdraws_independently() {
        let stale = Cache {
            spotlight: Some(b"# Old\nold body\n".to_vec()),
            spotlight_retrieved_at_unix: Some(CHECK_INTERVAL_SECS * 9),
            ..Cache::default()
        };
        let accepted = refreshed(refresh_with(
            Some(stale.clone()),
            Box::new(|_| available_releases(Vec::new())),
            TestGateway {
                changelog: Source::Unavailable,
                spotlight: Source::Available(Some(b"# New\nnew body\n".to_vec())),
            },
        ));
        assert_eq!(accepted.spotlight.status_title(), Some("New"));
        assert_eq!(
            accepted.spotlight.whats_new_body(),
            Some(b"new body\n".as_slice())
        );

        let withdrawn = refreshed(refresh_with(
            Some(stale),
            Box::new(|_| available_releases(Vec::new())),
            TestGateway {
                changelog: Source::Unavailable,
                spotlight: Source::Available(None),
            },
        ));
        assert!(withdrawn.spotlight.status_title().is_none());
        assert!(withdrawn.spotlight.whats_new_body().is_none());
        assert_eq!(
            withdrawn.spotlight.retrieved_at_unix(),
            Some(CHECK_INTERVAL_SECS * 10)
        );
    }

    #[test]
    fn same_release_details_survive_source_failure_but_catch_up_and_supersede_hide_them() {
        let cached_release = future_version(1);
        let cached_details = release_policy::CachedReleaseDetails {
            release: cached_release,
            details: "## cached\n".into(),
        };
        let cache = Cache {
            latest_seen: Some(cached_release.to_string()),
            release_details: Some(cache::PersistedReleaseDetails {
                release: cached_release.to_string(),
                details: cached_details.details.clone(),
            }),
            ..Cache::default()
        };
        let same_release = refreshed(refresh_with(
            Some(cache.clone()),
            Box::new(move |_| available_release(cached_release)),
            TestGateway {
                changelog: Source::Unavailable,
                spotlight: Source::Unavailable,
            },
        ));
        assert_eq!(same_release.release_details, Some(cached_details));

        let caught_up = refreshed(refresh_with(
            Some(cache.clone()),
            Box::new(|_| available_release(current())),
            TestGateway {
                changelog: Source::Unavailable,
                spotlight: Source::Unavailable,
            },
        ));
        assert!(caught_up.detected_release.is_none());
        assert!(caught_up.release_details.is_none());

        let superseded = refreshed(refresh_with(
            Some(cache),
            Box::new(move |_| available_release(future_version(2))),
            TestGateway {
                changelog: Source::Unavailable,
                spotlight: Source::Unavailable,
            },
        ));
        assert_eq!(superseded.detected_release, Some(future_version(2)));
        assert!(superseded.release_details.is_none());
    }

    #[test]
    fn changelog_without_eligible_sections_has_no_durable_release_details() {
        let release = future_version(1);
        let dir =
            std::env::temp_dir().join(format!("hfv-empty-release-details-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = start_with(StartDeps {
            disabled: false,
            now_unix: CHECK_INTERVAL_SECS * 10,
            cache: Some(Cache::default()),
            cache_dir: Some(dir.clone()),
            run: Box::new(move |_| available_release(release)),
            gateway: Box::new(TestGateway {
                changelog: Source::Available(Some(
                    b"# Changelog\n\n## [Unreleased]\n- pending\n".to_vec(),
                )),
                spotlight: Source::Unavailable,
            }),
        });

        let UpdateState { initial, rx } = state;
        let snapshot = rx
            .expect("stale cache schedules one refresh")
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("determined discovery publishes one replacement snapshot");
        assert_eq!(snapshot.detected_release, Some(release));
        assert!(snapshot.release_details.is_none());
        drop(snapshot);
        drop(initial);

        let persisted = cache::load(&dir).expect("the determined discovery persists its check");
        assert_eq!(persisted.latest_seen, Some(release.to_string()));
        assert!(persisted.release_details.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
