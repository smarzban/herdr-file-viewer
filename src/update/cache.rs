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

/// The cache to persist after a check attempt. The timestamp always advances (so the 24h
/// throttle holds even when the network is down); `latest_seen` records the probed version on
/// success and otherwise preserves whatever was last seen.
pub fn next_cache(
    now_unix: u64,
    previous: &Option<Cache>,
    probe_succeeded: bool,
    latest: Option<Version>,
) -> Cache {
    let latest_seen = if probe_succeeded {
        latest.map(|v| v.to_string())
    } else {
        previous.as_ref().and_then(|c| c.latest_seen.clone())
    };
    Cache {
        last_check_unix: now_unix,
        latest_seen,
    }
}

/// The plugin's cache directory: `$XDG_CACHE_HOME/herdr-file-viewer`, else
/// `$HOME/.cache/herdr-file-viewer`. `None` when neither is set (then we check without
/// persisting — a rare headless case).
pub fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("herdr-file-viewer"))
}

/// Read and parse the cache; `None` (→ "check now") on any absence/error.
pub fn load(dir: &Path) -> Option<Cache> {
    let raw = std::fs::read_to_string(dir.join(CACHE_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Best-effort persist; creates `dir` if needed. Any error is ignored — a cache we cannot
/// write just means we check again next launch.
pub fn store(dir: &Path, cache: &Cache) {
    let _ = std::fs::create_dir_all(dir);
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = std::fs::write(dir.join(CACHE_FILE), json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::version::Version;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn should_check_respects_the_24h_window() {
        let day = CHECK_INTERVAL_SECS;
        assert!(should_check(1_000 + day, 1_000), "exactly 24h later → check");
        assert!(should_check(1_000 + day + 1, 1_000), "past 24h → check");
        assert!(!should_check(1_000 + day - 1, 1_000), "within 24h → skip");
        assert!(!should_check(0, 0), "zero elapsed → skip (not a check trigger)");
        // First run carries no cache, so `decide` checks against last=0 with the real (large)
        // clock — which is well past the window.
        assert!(should_check(1_700_000_000, 0), "real clock vs last=0 → check");
        assert!(
            should_check(100, 9_999),
            "clock went backwards → check, never overflow"
        );
    }

    #[test]
    fn next_cache_always_advances_the_timestamp() {
        let prev = Some(Cache {
            last_check_unix: 1,
            latest_seen: Some("1.1.0".into()),
        });
        // success with a new version → record it
        let c = next_cache(500, &prev, true, Version::parse("1.2.0"));
        assert_eq!(
            c,
            Cache {
                last_check_unix: 500,
                latest_seen: Some("1.2.0".into())
            }
        );
        // success with no tags → latest_seen cleared
        let c = next_cache(500, &prev, true, None);
        assert_eq!(c.latest_seen, None);
        // probe failure → timestamp advances (throttle holds) but the prior seen value is kept
        let c = next_cache(500, &prev, false, None);
        assert_eq!(
            c,
            Cache {
                last_check_unix: 500,
                latest_seen: Some("1.1.0".into())
            }
        );
        // failure with no prior cache → empty seen
        let c = next_cache(500, &None, false, None);
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
