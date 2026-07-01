//! The throttle cache: the timestamp of the last check + the latest version then seen. Lets
//! the banner show immediately from a prior result while bounding the network to once per 24h.
//! Stores nothing about the user — only a unix time and a version string.

use crate::update::version::Version;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Minimum gap between network checks: 24h.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// The cache file name within the cache dir.
const CACHE_FILE: &str = "update-check.json";

/// The on-disk cache. Both fields tolerate absence (a fresh/upgraded cache).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Cache {
    pub last_check_unix: u64,
    #[serde(default)]
    pub latest_seen: Option<String>,
}

/// Whether enough time has elapsed since `last_check_unix` to hit the network again. A
/// `last_check_unix` in the future (corrupted cache / clock skew) is treated as "check now",
/// consistent with treating any unreadable cache as a reason to re-check.
pub fn should_check(now_unix: u64, last_check_unix: u64) -> bool {
    last_check_unix > now_unix || now_unix - last_check_unix >= CHECK_INTERVAL_SECS
}

/// The cache to persist after a **successful** probe: the check time plus the latest version
/// seen (`None` when the repo has no stable tags — which clears any stale cached banner). A
/// *failed* probe must not call this: the cache is left untouched so the check retries next
/// launch rather than being suppressed for 24h by a transient network blip.
pub fn next_cache(now_unix: u64, latest: Option<Version>) -> Cache {
    Cache {
        last_check_unix: now_unix,
        latest_seen: latest.map(|v| v.to_string()),
    }
}

/// The plugin's cache directory: `$XDG_CACHE_HOME/herdr-file-viewer`, else
/// `$HOME/.cache/herdr-file-viewer` (unix) / `%LOCALAPPDATA%\herdr-file-viewer` (Windows).
/// `None` when no base directory is available (then we check without persisting — a rare
/// headless case).
pub fn cache_dir() -> Option<PathBuf> {
    cache_dir_from(|var| std::env::var_os(var))
}

/// [`cache_dir`]'s logic, factored out so it is testable from a stubbed environment (no real
/// `XDG_CACHE_HOME`/`HOME`/`LOCALAPPDATA` mutation needed). `get_env` mirrors
/// `std::env::var_os`'s signature.
fn cache_dir_from(get_env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let base = cache_base_dir(get_env)?;
    Some(base.join("herdr-file-viewer"))
}

/// The per-user cache base directory, before the `herdr-file-viewer` subdirectory is joined.
/// unix: `$XDG_CACHE_HOME`, else `$HOME/.cache` (today's behaviour, unchanged — AC-3). Windows:
/// `%LOCALAPPDATA%`. `None` when nothing resolves (AC-7).
#[cfg(not(windows))]
fn cache_base_dir(get_env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    get_env("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| get_env("HOME").map(|h| PathBuf::from(h).join(".cache")))
}

#[cfg(windows)]
fn cache_base_dir(get_env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    get_env("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Read and parse the cache; `None` (→ "check now") on any absence/error.
pub fn load(dir: &Path) -> Option<Cache> {
    let raw = std::fs::read_to_string(dir.join(CACHE_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Best-effort persist; creates `dir` if needed. Any error is ignored — a cache we cannot
/// write just means we check again next launch.
///
/// Atomic publish: write a per-process temp file in the same dir, then `rename` it over the
/// target. herdr is multi-pane, so two viewer instances can write concurrently — a plain
/// truncating write could be read torn; with rename each reader sees either the old or the new
/// complete file (last writer wins, never a partial one).
pub fn store(dir: &Path, cache: &Cache) {
    let _ = std::fs::create_dir_all(dir);
    if let Ok(json) = serde_json::to_string(cache) {
        let tmp = dir.join(format!("{CACHE_FILE}.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join(CACHE_FILE));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::version::Version;
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicU64, Ordering};

    // ---- cache_base_dir: platform cache-dir seam (AC-7, T-3) --------------------

    /// A stub environment as a simple lookup, so the resolver is exercised without touching
    /// the real process environment.
    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |var| {
            pairs
                .iter()
                .find(|(k, _)| *k == var)
                .map(|(_, v)| OsString::from(*v))
        }
    }

    /// unix: `XDG_CACHE_HOME` wins when set and non-empty (today's behaviour, unchanged).
    #[cfg(not(windows))]
    #[test]
    fn cache_base_dir_unix_prefers_xdg_cache_home() {
        let got = cache_base_dir(env(&[
            ("XDG_CACHE_HOME", "/xdg/cache"),
            ("HOME", "/home/user"),
        ]));
        assert_eq!(got, Some(PathBuf::from("/xdg/cache")));
    }

    /// unix: falls back to `$HOME/.cache` when `XDG_CACHE_HOME` is unset or empty.
    #[cfg(not(windows))]
    #[test]
    fn cache_base_dir_unix_falls_back_to_home_dot_cache() {
        let got = cache_base_dir(env(&[("HOME", "/home/user")]));
        assert_eq!(got, Some(PathBuf::from("/home/user/.cache")));

        let got_empty_xdg = cache_base_dir(env(&[("XDG_CACHE_HOME", ""), ("HOME", "/home/user")]));
        assert_eq!(got_empty_xdg, Some(PathBuf::from("/home/user/.cache")));
    }

    /// unix: `None` when neither `XDG_CACHE_HOME` nor `HOME` is set (headless case).
    #[cfg(not(windows))]
    #[test]
    fn cache_base_dir_unix_none_when_nothing_set() {
        assert_eq!(cache_base_dir(env(&[])), None);
    }

    /// Windows: resolves to `%LOCALAPPDATA%` when `HOME`/`XDG_CACHE_HOME` are unset (AC-7).
    #[cfg(windows)]
    #[test]
    fn cache_base_dir_windows_uses_local_app_data() {
        let got = cache_base_dir(env(&[("LOCALAPPDATA", r"C:\Users\user\AppData\Local")]));
        assert_eq!(got, Some(PathBuf::from(r"C:\Users\user\AppData\Local")));
    }

    /// Windows: `None` when `%LOCALAPPDATA%` is absent or empty — no base available.
    #[cfg(windows)]
    #[test]
    fn cache_base_dir_windows_none_when_local_app_data_unset() {
        assert_eq!(cache_base_dir(env(&[])), None);
        assert_eq!(cache_base_dir(env(&[("LOCALAPPDATA", "")])), None);
    }

    /// `cache_dir_from` joins the `herdr-file-viewer` subdirectory onto the resolved base, on
    /// every platform.
    #[test]
    fn cache_dir_from_joins_the_plugin_subdir() {
        #[cfg(not(windows))]
        let got = cache_dir_from(env(&[("HOME", "/home/user")]));
        #[cfg(windows)]
        let got = cache_dir_from(env(&[("LOCALAPPDATA", r"C:\Users\user\AppData\Local")]));
        assert!(
            got.unwrap().ends_with("herdr-file-viewer"),
            "joins the plugin subdir onto the resolved base"
        );
    }

    #[test]
    fn should_check_respects_the_24h_window() {
        let day = CHECK_INTERVAL_SECS;
        assert!(
            should_check(1_000 + day, 1_000),
            "exactly 24h later → check"
        );
        assert!(should_check(1_000 + day + 1, 1_000), "past 24h → check");
        assert!(!should_check(1_000 + day - 1, 1_000), "within 24h → skip");
        assert!(
            !should_check(0, 0),
            "zero elapsed → skip (not a check trigger)"
        );
        // First run carries no cache, so `decide` checks against last=0 with the real (large)
        // clock — which is well past the window.
        assert!(
            should_check(1_700_000_000, 0),
            "real clock vs last=0 → check"
        );
        assert!(
            should_check(100, 9_999),
            "clock went backwards → check, never overflow"
        );
    }

    #[test]
    fn next_cache_records_the_check_time_and_version() {
        // A successful probe with a version → record the time and the version.
        let c = next_cache(500, Version::parse("1.2.0"));
        assert_eq!(
            c,
            Cache {
                last_check_unix: 500,
                latest_seen: Some("1.2.0".into())
            }
        );
        // A successful probe that found no stable tag → latest_seen cleared (clears a stale
        // cached banner). (A *failed* probe never reaches here — the caller leaves the cache.)
        let c = next_cache(500, None);
        assert_eq!(
            c,
            Cache {
                last_check_unix: 500,
                latest_seen: None
            }
        );
    }

    static N: AtomicU64 = AtomicU64::new(0);
    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hfv-cache-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn store_then_load_round_trips() {
        let dir = tmp(); // does not exist yet — store must create it
        let c = Cache {
            last_check_unix: 42,
            latest_seen: Some("1.1.0".into()),
        };
        store(&dir, &c);
        assert_eq!(load(&dir), Some(c));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_is_none_for_missing_or_corrupt_cache() {
        let dir = tmp();
        assert_eq!(load(&dir), None, "missing dir → None (check now)");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(CACHE_FILE), "{ not json").unwrap();
        assert_eq!(load(&dir), None, "corrupt → None, never a panic");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
