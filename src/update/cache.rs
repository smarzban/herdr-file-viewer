//! Bounded advisory facts for remote notices: successful-check time, detected release, and the
//! exact release-detail and spotlight bytes that a refresh may project. It stores no startup
//! eligibility or display policy, and nothing about user actions.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The cache file name within the cache dir.
const CACHE_FILE: &str = "update-check.json";

/// Distinguishes staging names from publishers in the same process.
static STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The only schema version this build understands. Unversioned files are the legacy update cache
/// and deserialize with this value so they can be extended in place.
const CACHE_SCHEMA_VERSION: u8 = 1;

/// The largest encoded cache accepted from or written to disk: 20 MiB.
pub const CACHE_MAX_BYTES: usize = 20 * 1024 * 1024;

/// Each retained source payload is capped independently: release details and the spotlight document.
const MAX_EXACT_FIELD_BYTES: usize = 1024 * 1024;
const MAX_VERSION_BYTES: usize = 64;

/// On-disk representation of immutable details retained for the exact release they describe.
///
/// This is deliberately distinct from the policy's in-memory `CachedReleaseDetails`: the
/// persistence boundary retains strings, then policy owns display eligibility.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PersistedReleaseDetails {
    pub release: String,
    pub details: String,
}

/// The on-disk remote-notice cache. A refresh publishes this complete record as one advisory
/// snapshot, so unrelated publishers never merge their independently computed fields.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Cache {
    #[serde(default = "current_schema_version")]
    pub schema_version: u8,
    pub last_check_unix: u64,
    #[serde(default)]
    pub latest_seen: Option<String>,
    #[serde(default)]
    pub release_details: Option<PersistedReleaseDetails>,
    #[serde(default)]
    pub spotlight: Option<Vec<u8>>,
    #[serde(default)]
    pub spotlight_retrieved_at_unix: Option<u64>,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            last_check_unix: 0,
            latest_seen: None,
            release_details: None,
            spotlight: None,
            spotlight_retrieved_at_unix: None,
        }
    }
}

fn current_schema_version() -> u8 {
    CACHE_SCHEMA_VERSION
}

impl Cache {
    /// Validate untrusted persisted state before it can enter the advisory cache.
    fn is_valid(&self) -> bool {
        self.schema_version == CACHE_SCHEMA_VERSION
            && self
                .latest_seen
                .as_ref()
                .is_none_or(|release| release.len() <= MAX_VERSION_BYTES)
            && self.release_details.as_ref().is_none_or(|details| {
                details.release.len() <= MAX_VERSION_BYTES
                    && details.details.len() <= MAX_EXACT_FIELD_BYTES
                    && self.latest_seen.as_deref() == Some(details.release.as_str())
            })
            && self
                .spotlight
                .as_ref()
                .is_none_or(|spotlight| spotlight.len() <= MAX_EXACT_FIELD_BYTES)
    }
}

/// The plugin's cache directory: `$XDG_CACHE_HOME/herdr-file-viewer`, else
/// `$HOME/.cache/herdr-file-viewer` (unix) / `%LOCALAPPDATA%\herdr-file-viewer` (Windows).
/// `None` when no base directory is available (then we check without persisting).
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
/// unix: `$XDG_CACHE_HOME`, else `$HOME/.cache`. Windows: `%LOCALAPPDATA%`.
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

/// Read and parse the cache; `None` (→ "check now") on any absence, invalid state, or error.
///
/// The raw read stops after the encoded cap plus one byte, before `serde_json` can allocate for
/// parsed values. A cache at the cap is valid, while the extra byte proves an oversized file.
pub fn load(dir: &Path) -> Option<Cache> {
    load_bounded(dir, CACHE_MAX_BYTES)
}

fn load_bounded(dir: &Path, max_bytes: usize) -> Option<Cache> {
    let file = File::open(dir.join(CACHE_FILE)).ok()?;
    let mut raw = Vec::new();
    file.take((max_bytes + 1) as u64)
        .read_to_end(&mut raw)
        .ok()?;
    if raw.len() > max_bytes {
        return None;
    }
    let cache: Cache = serde_json::from_slice(&raw).ok()?;
    cache.is_valid().then_some(cache)
}

/// Best-effort persist of one complete cache snapshot. It writes a unique sibling staging file,
/// closes it, then rename-publishes it. Readers observe either complete snapshot; concurrent
/// publishers are last-complete-writer-wins. Any failure is silent because this cache is advisory.
pub fn store(dir: &Path, cache: &Cache) {
    store_bounded(dir, cache, CACHE_MAX_BYTES);
}

fn store_bounded(dir: &Path, cache: &Cache, max_bytes: usize) {
    if !cache.is_valid() || std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_vec(cache) else {
        return;
    };
    if json.len() > max_bytes {
        return;
    }

    let Some((staged_path, mut staged)) = create_staging_file(dir) else {
        return;
    };
    let result = (|| -> std::io::Result<()> {
        staged.write_all(&json)?;
        drop(staged);
        std::fs::rename(staged_path.as_path(), dir.join(CACHE_FILE))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(staged_path);
    }
}

fn create_staging_file(dir: &Path) -> Option<(PathBuf, File)> {
    let path = staging_path(dir);
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .ok()
        .map(|file| (path, file))
}

fn staging_path(dir: &Path) -> PathBuf {
    let sequence = STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    dir.join(format!(
        "{CACHE_FILE}.{}-{nanos}-{sequence}.tmp",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};

    // ---- cache_base_dir: platform cache-dir seam (AC-7) --------------------------

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |var| {
            pairs
                .iter()
                .find(|(key, _)| *key == var)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn cache_base_dir_unix_prefers_xdg_then_home_dot_cache() {
        assert_eq!(
            cache_base_dir(env(&[
                ("XDG_CACHE_HOME", "/xdg/cache"),
                ("HOME", "/home/user")
            ])),
            Some(PathBuf::from("/xdg/cache"))
        );
        assert_eq!(
            cache_base_dir(env(&[("XDG_CACHE_HOME", ""), ("HOME", "/home/user")])),
            Some(PathBuf::from("/home/user/.cache"))
        );
        assert_eq!(cache_base_dir(env(&[])), None);
    }

    #[cfg(windows)]
    #[test]
    fn cache_base_dir_windows_uses_local_app_data() {
        assert_eq!(
            cache_base_dir(env(&[("LOCALAPPDATA", r"C:\Users\user\AppData\Local")])),
            Some(PathBuf::from(r"C:\Users\user\AppData\Local"))
        );
        assert_eq!(cache_base_dir(env(&[])), None);
    }

    #[test]
    fn cache_dir_from_joins_the_plugin_subdir() {
        #[cfg(not(windows))]
        let got = cache_dir_from(env(&[("HOME", "/home/user")]));
        #[cfg(windows)]
        let got = cache_dir_from(env(&[("LOCALAPPDATA", r"C:\Users\user\AppData\Local")]));
        assert!(got.unwrap().ends_with("herdr-file-viewer"));
    }

    static N: AtomicU64 = AtomicU64::new(0);

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hfv-cache-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn complete_cache(id: u64) -> Cache {
        Cache {
            last_check_unix: id,
            latest_seen: Some(format!("{id}.0.0")),
            release_details: Some(PersistedReleaseDetails {
                release: format!("{id}.0.0"),
                details: format!("## [{id}.0.0]\n- exact details {id}\n"),
            }),
            spotlight: Some(format!("# Project {id}\nbody {id}\n").into_bytes()),
            spotlight_retrieved_at_unix: Some(id),
            ..Cache::default()
        }
    }

    #[test]
    fn legacy_and_current_caches_round_trip() {
        let legacy_dir = tmp();
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(
            legacy_dir.join(CACHE_FILE),
            r#"{"last_check_unix":42,"latest_seen":"1.2.0"}"#,
        )
        .unwrap();
        assert_eq!(
            load(&legacy_dir),
            Some(Cache {
                last_check_unix: 42,
                latest_seen: Some("1.2.0".into()),
                ..Cache::default()
            })
        );

        let current_dir = tmp();
        let current = complete_cache(42);
        store(&current_dir, &current);
        assert_eq!(load(&current_dir), Some(current));

        let _ = std::fs::remove_dir_all(&legacy_dir);
        let _ = std::fs::remove_dir_all(&current_dir);
    }

    #[test]
    fn load_accepts_the_exact_cap_and_rejects_cap_plus_one() {
        let dir = tmp();
        std::fs::create_dir_all(&dir).unwrap();
        let cache = Cache {
            last_check_unix: 42,
            latest_seen: Some("1.2.0".into()),
            ..Cache::default()
        };
        let encoded = serde_json::to_vec(&cache).unwrap();
        std::fs::write(dir.join(CACHE_FILE), &encoded).unwrap();
        assert_eq!(load_bounded(&dir, encoded.len()), Some(cache.clone()));

        let mut oversized = encoded;
        oversized.push(b' ');
        std::fs::write(dir.join(CACHE_FILE), oversized).unwrap();
        assert_eq!(
            load_bounded(&dir, serde_json::to_vec(&cache).unwrap().len()),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_observes_only_complete_old_or_new_snapshots() {
        let dir = tmp();
        let old = complete_cache(1);
        let new = complete_cache(2);
        store(&dir, &old);

        let writing = Arc::new(AtomicBool::new(true));
        let writer_dir = dir.clone();
        let writer_done = Arc::clone(&writing);
        let writer_new = new.clone();
        let writer = std::thread::spawn(move || {
            store(&writer_dir, &writer_new);
            writer_done.store(false, Ordering::Release);
        });
        while writing.load(Ordering::Acquire) {
            assert!(matches!(load(&dir), Some(snapshot) if snapshot == old || snapshot == new));
        }
        writer.join().unwrap();
        assert_eq!(load(&dir), Some(new));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sequential_and_concurrent_publishers_leave_one_complete_writer_snapshot() {
        let dir = tmp();
        let first = complete_cache(1);
        let second = complete_cache(2);
        store(&dir, &first);
        store(&dir, &second);
        assert_eq!(load(&dir), Some(second.clone()));

        let barrier = Arc::new(Barrier::new(3));
        let first_dir = dir.clone();
        let first_barrier = Arc::clone(&barrier);
        let first_snapshot = first.clone();
        let first_publisher = std::thread::spawn(move || {
            first_barrier.wait();
            store(&first_dir, &first_snapshot);
        });
        let second_dir = dir.clone();
        let second_barrier = Arc::clone(&barrier);
        let second_snapshot = second.clone();
        let second_publisher = std::thread::spawn(move || {
            second_barrier.wait();
            store(&second_dir, &second_snapshot);
        });
        barrier.wait();
        first_publisher.join().unwrap();
        second_publisher.join().unwrap();

        assert!(matches!(load(&dir), Some(snapshot) if snapshot == first || snapshot == second));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_or_over_cap_snapshots_are_not_persisted() {
        let invalid_dir = tmp();
        store(
            &invalid_dir,
            &Cache {
                latest_seen: Some("x".repeat(MAX_VERSION_BYTES + 1)),
                ..Cache::default()
            },
        );
        assert!(!invalid_dir.join(CACHE_FILE).exists());

        let cap_dir = tmp();
        let cache = Cache::default();
        store_bounded(
            &cap_dir,
            &cache,
            serde_json::to_vec(&cache).unwrap().len() - 1,
        );
        assert!(!cap_dir.join(CACHE_FILE).exists());
        let _ = std::fs::remove_dir_all(&invalid_dir);
        let _ = std::fs::remove_dir_all(&cap_dir);
    }

    #[test]
    fn store_is_silent_when_publication_fails_and_needs_no_lock_file() {
        let dir = tmp();
        std::fs::create_dir_all(&dir).unwrap();
        let destination = dir.join(CACHE_FILE);
        std::fs::create_dir(&destination).unwrap();

        store(&dir, &complete_cache(1));

        assert!(
            destination.is_dir(),
            "a failed publish preserves the prior destination"
        );
        assert!(
            !dir.join("update-check.lock").exists(),
            "a complete snapshot needs no lock or writer"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rejects_corrupt_unknown_or_semantically_invalid_cache() {
        let dir = tmp();
        std::fs::create_dir_all(&dir).unwrap();
        for raw in [
            b"{ not json".as_slice(),
            br#"{"schema_version":99,"last_check_unix":42,"latest_seen":"1.2.0"}"#,
        ] {
            std::fs::write(dir.join(CACHE_FILE), raw).unwrap();
            assert_eq!(load(&dir), None);
        }
        let invalid = Cache {
            latest_seen: Some("1.2.0".into()),
            release_details: Some(PersistedReleaseDetails {
                release: "1.3.0".into(),
                details: "wrong release".into(),
            }),
            ..Cache::default()
        };
        std::fs::write(dir.join(CACHE_FILE), serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert_eq!(load(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
