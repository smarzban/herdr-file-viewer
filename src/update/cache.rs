//! The throttle cache: the timestamp of the last check + the latest version then seen. Lets
//! the banner show immediately from a prior result while bounding the network to once per 24h.
//! Stores nothing about the user — only a unix time and a version string.

use crate::update::version::Version;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Minimum gap between network checks: 24h.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// The cache file name within the cache dir.
const CACHE_FILE: &str = "update-check.json";

/// A separate advisory lock, so readers never need to hold the cache-data file open.
const LOCK_FILE: &str = "update-check.lock";

/// Retry a competing writer for a short, bounded interval, then leave the advisory cache alone.
const LOCK_ATTEMPTS: usize = 200;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(5);

/// On Windows an external reader can transiently prevent replacement of the destination.
const REPLACE_ATTEMPTS: usize = 20;
const REPLACE_RETRY_DELAY: Duration = Duration::from_millis(5);

/// Distinguishes staging names from writers in the same process.
static STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const STAGING_ATTEMPTS: usize = 8;

/// The only schema version this build understands. Unversioned files are the legacy update cache
/// and deserialize with this value so they can be extended in place.
const CACHE_SCHEMA_VERSION: u8 = 1;

/// The largest encoded cache accepted from or written to disk: 20 MiB.
pub const CACHE_MAX_BYTES: usize = 20 * 1024 * 1024;

/// Each retained source payload is capped independently. Three fields can hold exact source bytes:
/// release details, the spotlight document, and its dismissed identity.
const MAX_EXACT_FIELD_BYTES: usize = 1024 * 1024;
const MAX_VERSION_BYTES: usize = 64;

/// On-disk representation of immutable details retained for the exact release they describe.
///
/// This is deliberately distinct from the policy's in-memory `CachedReleaseDetails`: T-9 converts
/// persisted strings at the persistence boundary, then the policy owns display eligibility.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PersistedReleaseDetails {
    pub release: String,
    pub details: String,
}

/// The on-disk remote-notice cache. The legacy update fields remain public for the compatibility
/// loop; the newer fields retain advisory notice state only.
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
    #[serde(default)]
    pub dismissed_spotlight_identity: Option<Vec<u8>>,
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
            dismissed_spotlight_identity: None,
        }
    }
}

fn current_schema_version() -> u8 {
    CACHE_SCHEMA_VERSION
}

/// A narrow cache update. Each variant changes only the state implied by its completed intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheDelta {
    /// A successful release check. Changing the detected release invalidates its old details.
    RefreshRelease {
        checked_at_unix: u64,
        detected_release: Option<String>,
    },
    /// Successful immutable release details for the exact currently detected release.
    StoreReleaseDetails { release: String, details: String },
    /// A successful spotlight retrieval with the exact accepted document.
    RefreshSpotlight {
        spotlight: Vec<u8>,
        retrieved_at_unix: u64,
    },
    /// Dismiss the exact currently cached spotlight identity.
    DismissSpotlight { identity: Vec<u8> },
    /// A conclusive withdrawal, which clears content but remains fresh at this retrieval time.
    WithdrawSpotlight { retrieved_at_unix: u64 },
}

/// A cloneable, best-effort writer for completed cache intents.
///
/// [`enqueue`](Self::enqueue) sends only an intent-owned [`CacheDelta`] to the worker, so it does
/// not wait for cache locks or disk. Dropping the final handle closes the channel and joins the
/// worker after it has given every accepted delta one bounded persistence attempt.
#[derive(Clone)]
pub struct CacheWriter {
    // Fields drop in declaration order, so the final sender closes before the final worker
    // reference drops and joins its drained worker.
    sender: mpsc::Sender<CacheDelta>,
    _worker: Arc<CacheWriterWorker>,
}

struct CacheWriterWorker {
    join: Option<JoinHandle<()>>,
}

impl Drop for CacheWriterWorker {
    fn drop(&mut self) {
        let Some(join) = self.join.take() else {
            return;
        };
        // The worker closure owns no CacheWriter. Keep the shutdown path deadlock-free if a
        // future call site changes that ownership and drops the final handle on this worker.
        if join.thread().id() != thread::current().id() {
            let _ = join.join();
        }
    }
}

impl CacheWriter {
    /// Start one cache worker for `dir`. Persistence failures remain advisory and silent.
    pub fn new(dir: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel();
        let join = thread::Builder::new()
            .spawn(move || {
                while let Ok(delta) = receiver.recv() {
                    store_delta(&dir, delta);
                }
            })
            .ok();
        Self {
            sender,
            _worker: Arc::new(CacheWriterWorker { join }),
        }
    }

    /// Queue one completed intent. `true` means the worker accepted the delta for a bounded,
    /// best-effort persistence attempt; it does not promise that the advisory cache was written.
    pub fn enqueue(&self, delta: CacheDelta) -> bool {
        self.sender.send(delta).is_ok()
    }
}

impl Cache {
    /// Apply one intent-owned delta without projecting eligibility, freshness, or UI state.
    pub fn apply(&mut self, delta: CacheDelta) {
        match delta {
            CacheDelta::RefreshRelease {
                checked_at_unix,
                detected_release,
            } => {
                if detected_release
                    .as_ref()
                    .is_some_and(|release| release.len() > MAX_VERSION_BYTES)
                {
                    return;
                }
                if self.latest_seen != detected_release {
                    self.release_details = None;
                }
                self.last_check_unix = checked_at_unix;
                self.latest_seen = detected_release;
            }
            CacheDelta::StoreReleaseDetails { release, details } => {
                if release.len() > MAX_VERSION_BYTES
                    || details.len() > MAX_EXACT_FIELD_BYTES
                    || self.latest_seen.as_deref() != Some(release.as_str())
                {
                    return;
                }
                self.release_details = Some(PersistedReleaseDetails { release, details });
            }
            CacheDelta::RefreshSpotlight {
                spotlight,
                retrieved_at_unix,
            } => {
                if spotlight.len() > MAX_EXACT_FIELD_BYTES {
                    return;
                }
                if self.dismissed_spotlight_identity.as_deref() != Some(spotlight.as_slice()) {
                    self.dismissed_spotlight_identity = None;
                }
                self.spotlight = Some(spotlight);
                self.spotlight_retrieved_at_unix = Some(retrieved_at_unix);
            }
            CacheDelta::DismissSpotlight { identity } => {
                if identity.len() > MAX_EXACT_FIELD_BYTES
                    || self.spotlight.as_deref() != Some(identity.as_slice())
                {
                    return;
                }
                self.dismissed_spotlight_identity = Some(identity);
            }
            CacheDelta::WithdrawSpotlight { retrieved_at_unix } => {
                self.spotlight = None;
                self.spotlight_retrieved_at_unix = Some(retrieved_at_unix);
            }
        }
    }

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
            && self
                .dismissed_spotlight_identity
                .as_ref()
                .is_none_or(|identity| identity.len() <= MAX_EXACT_FIELD_BYTES)
    }
}

/// Whether enough time has elapsed since `last_check_unix` to hit the network again. A
/// `last_check_unix` in the future (corrupted cache / clock skew) is treated as "check now",
/// consistent with treating any unreadable cache as a reason to re-check.
pub fn should_check(now_unix: u64, last_check_unix: u64) -> bool {
    last_check_unix > now_unix || now_unix - last_check_unix >= CHECK_INTERVAL_SECS
}

/// The cache to persist after a **successful** probe: refresh the check time plus the latest
/// version seen (`None` when the repo has no stable tags — which clears any stale cached banner).
/// A *failed* probe must not call this: the cache is left untouched so the check retries next
/// launch rather than being suppressed for 24h by a transient network blip.
///
/// This compatibility writer preserves remote-notice state until T-10 replaces the legacy loop.
pub fn next_cache(mut cache: Cache, now_unix: u64, latest: Option<Version>) -> Cache {
    cache.apply(CacheDelta::RefreshRelease {
        checked_at_unix: now_unix,
        detected_release: latest.map(|v| v.to_string()),
    });
    cache
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

/// Read and parse the cache; `None` (→ "check now") on any absence, invalid state, or error.
///
/// The raw read stops after the encoded cap plus one byte, before `serde_json` can allocate for
/// parsed values. A cache at the cap is valid, while the extra byte proves an oversized file.
pub fn load(dir: &Path) -> Option<Cache> {
    load_bounded(dir, CACHE_MAX_BYTES)
}

fn load_bounded(dir: &Path, max_bytes: usize) -> Option<Cache> {
    let file = std::fs::File::open(dir.join(CACHE_FILE)).ok()?;
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

/// Best-effort persist of a complete snapshot; creates `dir` if needed. Any error is ignored —
/// a cache we cannot write just means we check again next launch. New mutation call sites should
/// prefer [`store_delta`], which rereads the current revision while exclusive instead of replacing
/// it with a stale in-memory snapshot.
///
/// This compatibility path also requires the shared cache lease. Although it does not reread,
/// publishing its caller-supplied snapshot unlocked could collide with a lease-holding writer and
/// replace that writer's complete revision. Rename keeps readers whole, not competing writers
/// serialized, so a lock error leaves the prior revision untouched.
pub fn store(dir: &Path, cache: &Cache) {
    store_bounded(dir, cache, CACHE_MAX_BYTES);
}

/// Best-effort read-modify-write of one completed cache intent across viewer processes.
///
/// The separate lock file is opened read/write because Rust 1.96 documents that Windows file
/// locks require one of those access modes. Once exclusive, this rereads the cache-data file,
/// applies only `delta`, and publishes one complete staged revision. The data handle from
/// `load_bounded` is closed before replacement, which matters when Windows rejects replacing an
/// open destination. Every lock and replacement retry is bounded; failure is advisory and silent.
pub fn store_delta(dir: &Path, delta: CacheDelta) {
    store_delta_bounded(dir, delta, CACHE_MAX_BYTES, LOCK_ATTEMPTS);
}

fn store_bounded(dir: &Path, cache: &Cache, max_bytes: usize) {
    store_with_try_lock(dir, cache, max_bytes, File::try_lock);
}

fn store_with_try_lock(
    dir: &Path,
    cache: &Cache,
    max_bytes: usize,
    try_lock: impl FnMut(&File) -> Result<(), TryLockError>,
) {
    if !cache.is_valid() || std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Some(_lock) = acquire_lock_with(dir, LOCK_ATTEMPTS, try_lock) else {
        return;
    };
    write_complete_revision(dir, cache, max_bytes);
}

fn store_delta_bounded(dir: &Path, delta: CacheDelta, max_bytes: usize, lock_attempts: usize) {
    store_delta_with_try_lock(dir, delta, max_bytes, lock_attempts, File::try_lock);
}

fn store_delta_with_try_lock(
    dir: &Path,
    delta: CacheDelta,
    max_bytes: usize,
    lock_attempts: usize,
    try_lock: impl FnMut(&File) -> Result<(), TryLockError>,
) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    // A failed or unsupported lock is deliberately not an unlocked fallback: atomic rename keeps
    // readers whole, but only this lock makes the read-modify-write merge safe across processes.
    let Some(_lock) = acquire_lock_with(dir, lock_attempts, try_lock) else {
        return;
    };

    // `load_bounded` drops its cache-data `File` before returning, so no destination handle from
    // this writer remains open while the staged revision is renamed over it.
    let mut cache = load_bounded(dir, max_bytes).unwrap_or_default();
    cache.apply(delta);
    write_complete_revision(dir, &cache, max_bytes);
}

fn acquire_lock_with(
    dir: &Path,
    attempts: usize,
    mut try_lock: impl FnMut(&File) -> Result<(), TryLockError>,
) -> Option<File> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(LOCK_FILE))
        .ok()?;

    for attempt in 0..attempts {
        match try_lock(&lock) {
            Ok(()) => return Some(lock),
            Err(TryLockError::WouldBlock) if attempt + 1 < attempts => {
                std::thread::sleep(LOCK_RETRY_DELAY);
            }
            Err(_) => return None,
        }
    }
    None
}

fn write_complete_revision(dir: &Path, cache: &Cache, max_bytes: usize) {
    if !cache.is_valid() {
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
        // Close the staging handle before publishing its complete revision.
        drop(staged);
        replace_with_retry(&dir.join(CACHE_FILE), &staged_path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(staged_path);
    }
}

fn create_staging_file(dir: &Path) -> Option<(PathBuf, File)> {
    for _ in 0..STAGING_ATTEMPTS {
        let path = staging_path(dir);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Some((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
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

fn replace_with_retry(destination: &Path, staged: &Path) -> std::io::Result<()> {
    for attempt in 0..REPLACE_ATTEMPTS {
        match std::fs::rename(staged, destination) {
            Ok(()) => return Ok(()),
            Err(error) if is_transient_replace_error(&error) && attempt + 1 < REPLACE_ATTEMPTS => {
                std::thread::sleep(REPLACE_RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::other("cache replacement retry exhausted"))
}

fn is_transient_replace_error(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        return true;
    }
    // Rust 1.96's `sys/io/error/windows.rs` maps ERROR_SHARING_VIOLATION (32) to
    // `Uncategorized`, so inspect the documented raw Win32 code instead of its error kind.
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(32)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::version::Version;
    use std::cell::Cell;
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn cache_writer_joins_the_worker_on_the_last_handle_drop() {
        let (sender, receiver) = mpsc::channel::<CacheDelta>();
        let (closed_tx, closed_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = Arc::new(CacheWriterWorker {
            join: Some(std::thread::spawn(move || {
                assert!(
                    receiver.recv().is_err(),
                    "last sender closes the worker channel"
                );
                closed_tx.send(()).expect("report closed channel");
                release_rx.recv().expect("release worker");
            })),
        });
        let final_worker = Arc::clone(&worker);
        drop(worker);
        drop(sender);

        let (dropped_tx, dropped_rx) = mpsc::channel();
        let dropper = std::thread::spawn(move || {
            drop(final_worker);
            dropped_tx.send(()).expect("report final drop");
        });
        closed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("last sender must close the worker channel");
        assert!(
            dropped_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "the last handle must wait for its worker to finish"
        );

        release_tx.send(()).expect("release worker");
        dropped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("last handle returns after the worker finishes");
        dropper.join().expect("join final dropper");
    }

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
        let c = next_cache(Cache::default(), 500, Version::parse("1.2.0"));
        assert_eq!(
            c,
            Cache {
                last_check_unix: 500,
                latest_seen: Some("1.2.0".into()),
                ..Cache::default()
            }
        );
        // A successful probe that found no stable tag → latest_seen cleared (clears a stale
        // cached banner). (A *failed* probe never reaches here — the caller leaves the cache.)
        let c = next_cache(Cache::default(), 500, None);
        assert_eq!(
            c,
            Cache {
                last_check_unix: 500,
                latest_seen: None,
                ..Cache::default()
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
            ..Cache::default()
        };
        store(&dir, &c);
        assert_eq!(load(&dir), Some(c));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_cache_migrates_known_fields_with_notice_fields_absent() {
        let dir = tmp();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(CACHE_FILE),
            r#"{"last_check_unix":42,"latest_seen":"1.2.0"}"#,
        )
        .unwrap();

        assert_eq!(
            load(&dir),
            Some(Cache {
                last_check_unix: 42,
                latest_seen: Some("1.2.0".into()),
                ..Cache::default()
            }),
            "the unversioned update cache retains its known fields and starts with no notices"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remote_notice_cache_round_trips_complete_state() {
        let dir = tmp();
        let cache = Cache {
            last_check_unix: 42,
            latest_seen: Some("1.2.0".into()),
            release_details: Some(PersistedReleaseDetails {
                release: "1.2.0".into(),
                details: "## [1.2.0]\n- Exact cached details\n".into(),
            }),
            spotlight: Some(vec![0, b'#', 0xff, b'\n']),
            spotlight_retrieved_at_unix: Some(99),
            dismissed_spotlight_identity: Some(vec![0, 0xff]),
            ..Cache::default()
        };

        store(&dir, &cache);
        assert_eq!(
            load(&dir),
            Some(cache),
            "every persisted remote-notice field retains its exact value"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_deltas_change_only_their_owned_notice_state() {
        let first = b"# First\nold body\n".to_vec();
        let replacement = b"# First\nnew body\n".to_vec();
        let mut cache = Cache {
            last_check_unix: 10,
            latest_seen: Some("1.1.0".into()),
            release_details: Some(PersistedReleaseDetails {
                release: "1.1.0".into(),
                details: "old details".into(),
            }),
            spotlight: Some(first.clone()),
            spotlight_retrieved_at_unix: Some(20),
            dismissed_spotlight_identity: Some(first),
            ..Cache::default()
        };

        cache.apply(CacheDelta::RefreshRelease {
            checked_at_unix: 30,
            detected_release: Some("1.2.0".into()),
        });
        assert_eq!(cache.last_check_unix, 30);
        assert_eq!(cache.latest_seen.as_deref(), Some("1.2.0"));
        assert_eq!(
            cache.release_details, None,
            "old release details are untied"
        );
        assert_eq!(
            cache.spotlight.as_deref(),
            Some(b"# First\nold body\n".as_slice())
        );
        assert_eq!(
            cache.dismissed_spotlight_identity.as_deref(),
            Some(b"# First\nold body\n".as_slice()),
            "a release check does not alter spotlight dismissal"
        );

        cache.apply(CacheDelta::StoreReleaseDetails {
            release: "1.2.0".into(),
            details: "new details".into(),
        });
        assert_eq!(
            cache.release_details,
            Some(PersistedReleaseDetails {
                release: "1.2.0".into(),
                details: "new details".into(),
            })
        );

        cache.apply(CacheDelta::RefreshSpotlight {
            spotlight: replacement.clone(),
            retrieved_at_unix: 40,
        });
        assert_eq!(cache.spotlight, Some(replacement.clone()));
        assert_eq!(cache.spotlight_retrieved_at_unix, Some(40));
        assert_eq!(
            cache.dismissed_spotlight_identity, None,
            "a different exact document clears the old dismissal"
        );

        cache.apply(CacheDelta::DismissSpotlight {
            identity: replacement.clone(),
        });
        cache.apply(CacheDelta::WithdrawSpotlight {
            retrieved_at_unix: 50,
        });
        assert_eq!(cache.spotlight, None);
        assert_eq!(cache.spotlight_retrieved_at_unix, Some(50));
        assert_eq!(cache.dismissed_spotlight_identity, Some(replacement));
    }

    #[test]
    fn cache_delta_rejects_an_overlong_detected_release() {
        let mut cache = Cache {
            last_check_unix: 10,
            latest_seen: Some("1.2.0".into()),
            ..Cache::default()
        };
        let before = cache.clone();

        cache.apply(CacheDelta::RefreshRelease {
            checked_at_unix: 20,
            detected_release: Some("x".repeat(MAX_VERSION_BYTES + 1)),
        });

        assert_eq!(
            cache, before,
            "invalid release input must not advance the throttle"
        );
    }

    #[test]
    fn cache_delta_rejects_invalid_release_details() {
        let mut cache = Cache {
            latest_seen: Some("1.2.0".into()),
            ..Cache::default()
        };
        let before = cache.clone();

        for delta in [
            CacheDelta::StoreReleaseDetails {
                release: "1.3.0".into(),
                details: "wrong detected release".into(),
            },
            CacheDelta::StoreReleaseDetails {
                release: "x".repeat(MAX_VERSION_BYTES + 1),
                details: "overlong release".into(),
            },
            CacheDelta::StoreReleaseDetails {
                release: "1.2.0".into(),
                details: "x".repeat(MAX_EXACT_FIELD_BYTES + 1),
            },
        ] {
            cache.apply(delta);
            assert_eq!(cache, before, "invalid release details must be a no-op");
        }
    }

    #[test]
    fn cache_delta_rejects_an_oversized_spotlight() {
        let original = b"# Original\nbody\n".to_vec();
        let mut cache = Cache {
            spotlight: Some(original.clone()),
            spotlight_retrieved_at_unix: Some(10),
            dismissed_spotlight_identity: Some(original),
            ..Cache::default()
        };
        let before = cache.clone();

        cache.apply(CacheDelta::RefreshSpotlight {
            spotlight: vec![b'x'; MAX_EXACT_FIELD_BYTES + 1],
            retrieved_at_unix: 20,
        });

        assert_eq!(
            cache, before,
            "an oversized spotlight must not clear a dismissal"
        );
    }

    #[test]
    fn cache_delta_rejects_an_invalid_spotlight_dismissal() {
        let spotlight = b"# Current\nbody\n".to_vec();
        let mut cache = Cache {
            spotlight: Some(spotlight),
            ..Cache::default()
        };
        let before = cache.clone();

        for identity in [
            b"# Different\nbody\n".to_vec(),
            vec![b'x'; MAX_EXACT_FIELD_BYTES + 1],
        ] {
            cache.apply(CacheDelta::DismissSpotlight { identity });
            assert_eq!(
                cache, before,
                "only the exact bounded spotlight may be dismissed"
            );
        }
    }

    #[test]
    fn load_rejects_semantically_invalid_persisted_notice_state() {
        let overlong_version = "x".repeat(MAX_VERSION_BYTES + 1);
        let overlong_payload = "x".repeat(MAX_EXACT_FIELD_BYTES + 1);
        let cases = [
            (
                "overlong detected release",
                Cache {
                    latest_seen: Some(overlong_version.clone()),
                    ..Cache::default()
                },
            ),
            (
                "untied release details",
                Cache {
                    latest_seen: Some("1.2.0".into()),
                    release_details: Some(PersistedReleaseDetails {
                        release: "1.3.0".into(),
                        details: "wrong release".into(),
                    }),
                    ..Cache::default()
                },
            ),
            (
                "oversized release details",
                Cache {
                    latest_seen: Some("1.2.0".into()),
                    release_details: Some(PersistedReleaseDetails {
                        release: "1.2.0".into(),
                        details: overlong_payload.clone(),
                    }),
                    ..Cache::default()
                },
            ),
            (
                "oversized spotlight",
                Cache {
                    spotlight: Some(overlong_payload.as_bytes().to_vec()),
                    ..Cache::default()
                },
            ),
            (
                "oversized dismissed identity",
                Cache {
                    dismissed_spotlight_identity: Some(overlong_payload.into_bytes()),
                    ..Cache::default()
                },
            ),
        ];

        for (name, cache) in cases {
            let dir = tmp();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(CACHE_FILE), serde_json::to_vec(&cache).unwrap()).unwrap();
            assert_eq!(load(&dir), None, "{name} must degrade to empty");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn store_rejects_invalid_and_over_cap_cache_state() {
        let invalid_dir = tmp();
        let invalid = Cache {
            latest_seen: Some("x".repeat(MAX_VERSION_BYTES + 1)),
            ..Cache::default()
        };
        store(&invalid_dir, &invalid);
        assert!(
            !invalid_dir.join(CACHE_FILE).exists(),
            "invalid state must never be persisted"
        );

        let cap_dir = tmp();
        let valid = Cache::default();
        let encoded_len = serde_json::to_vec(&valid).unwrap().len();
        store_bounded(&cap_dir, &valid, encoded_len - 1);
        assert!(
            !cap_dir.join(CACHE_FILE).exists(),
            "an encoded cache over the write cap must not be persisted"
        );
    }

    #[test]
    fn load_accepts_a_valid_cache_at_the_exact_encoded_cap() {
        assert_eq!(
            CACHE_MAX_BYTES,
            20 * 1024 * 1024,
            "the production cap is 20 MiB"
        );
        let dir = tmp();
        std::fs::create_dir_all(&dir).unwrap();
        let cache = Cache {
            last_check_unix: 42,
            latest_seen: Some("1.2.0".into()),
            ..Cache::default()
        };
        let encoded = serde_json::to_vec(&cache).unwrap();
        std::fs::write(dir.join(CACHE_FILE), &encoded).unwrap();

        assert_eq!(load_bounded(&dir, encoded.len()), Some(cache));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rejects_a_valid_cache_one_byte_past_the_encoded_cap() {
        let dir = tmp();
        std::fs::create_dir_all(&dir).unwrap();
        let cache = Cache {
            last_check_unix: 42,
            latest_seen: Some("1.2.0".into()),
            ..Cache::default()
        };
        let mut encoded = serde_json::to_vec(&cache).unwrap();
        let cap = encoded.len();
        encoded.push(b' ');
        std::fs::write(dir.join(CACHE_FILE), encoded).unwrap();

        assert_eq!(
            load_bounded(&dir, cap),
            None,
            "oversized cache degrades to empty"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn windows_sharing_violation_is_a_transient_replacement_error() {
        assert!(is_transient_replace_error(
            &std::io::Error::from_raw_os_error(32)
        ));
    }

    #[test]
    fn store_lock_error_preserves_the_prior_complete_revision() {
        let dir = tmp();
        let before = Cache {
            last_check_unix: 10,
            latest_seen: Some("1.2.0".into()),
            ..Cache::default()
        };
        store(&dir, &before);

        store_with_try_lock(
            &dir,
            &Cache {
                last_check_unix: 20,
                latest_seen: Some("1.3.0".into()),
                ..Cache::default()
            },
            CACHE_MAX_BYTES,
            |_| {
                Err(TryLockError::Error(std::io::Error::other(
                    "locking unsupported",
                )))
            },
        );

        assert_eq!(
            load(&dir),
            Some(before),
            "a compatibility snapshot cannot publish without the shared cache lease"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_error_never_publishes_an_unserialized_delta() {
        let dir = tmp();
        let before = Cache {
            last_check_unix: 10,
            latest_seen: Some("1.2.0".into()),
            ..Cache::default()
        };
        store(&dir, &before);

        store_delta_with_try_lock(
            &dir,
            CacheDelta::RefreshRelease {
                checked_at_unix: 20,
                detected_release: Some("1.3.0".into()),
            },
            CACHE_MAX_BYTES,
            1,
            |_| {
                Err(TryLockError::Error(std::io::Error::other(
                    "locking unsupported",
                )))
            },
        );

        assert_eq!(
            load(&dir),
            Some(before),
            "an unavailable lock must leave the cache unchanged rather than write unlocked"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_lock_attempt_exhaustion_drops_the_best_effort_delta() {
        let dir = tmp();
        let attempts = Cell::new(0);

        store_delta_with_try_lock(
            &dir,
            CacheDelta::RefreshRelease {
                checked_at_unix: 20,
                detected_release: Some("1.3.0".into()),
            },
            CACHE_MAX_BYTES,
            1,
            |_| {
                attempts.set(attempts.get() + 1);
                Err(TryLockError::WouldBlock)
            },
        );

        assert_eq!(
            attempts.get(),
            1,
            "the injected one-attempt budget is honored"
        );
        assert_eq!(
            load(&dir),
            None,
            "exhausting the lock budget must not publish an unlocked revision"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_is_none_for_missing_corrupt_unknown_version_or_truncated_cache() {
        let dir = tmp();
        assert_eq!(load(&dir), None, "missing dir → None (check now)");
        std::fs::create_dir_all(&dir).unwrap();

        for (name, json) in [
            ("corrupt", "{ not json"),
            (
                "unknown schema version",
                r#"{"schema_version":99,"last_check_unix":42,"latest_seen":"1.2.0"}"#,
            ),
            ("truncated", r#"{"schema_version":1,"last_check_unix":42"#),
        ] {
            std::fs::write(dir.join(CACHE_FILE), json).unwrap();
            assert_eq!(load(&dir), None, "{name} → None, never a panic");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
