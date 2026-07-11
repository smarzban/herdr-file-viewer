//! Regression test for the leaked-temp-dir sweep in `common::sweep_stale_in`.
//!
//! `TempDir` cleans up on drop, but a *killed* test run (timeout, SIGKILL) skips `Drop` and
//! leaks its temp dirs; over many killed runs they exhausted the `/tmp` tmpfs. The sweep
//! reclaims those stale orphans on the next run while never touching a live run's dirs.

mod common;

use std::fs;
use std::time::{Duration, SystemTime};

#[test]
fn sweeps_stale_prefixed_dirs_but_spares_fresh_and_foreign() {
    // A private base (its own TempDir) so the test never races the real system temp dir.
    let base = common::TempDir::new();
    let base = base.path();

    let stale = base.join("herdr-fv-test-1-2-3");
    let also_stale = base.join("herdr-fv-test-4-5-6");
    let foreign = base.join("some-other-tool-xyz"); // not our prefix
    for d in [&stale, &also_stale, &foreign] {
        fs::create_dir(d).unwrap();
    }

    // Cutoff in the future: every just-created dir is "older than cutoff", so the prefixed
    // ones are eligible and the foreign one must still be spared (prefix gate).
    common::sweep_stale_in(base, SystemTime::now() + Duration::from_secs(3600));
    assert!(!stale.exists(), "a stale prefixed dir should be swept");
    assert!(!also_stale.exists(), "a stale prefixed dir should be swept");
    assert!(foreign.exists(), "a non-prefixed dir must never be touched");

    // Cutoff in the past: a freshly created prefixed dir is newer than the cutoff, i.e. it
    // looks like a live run's dir, so the age gate must spare it.
    let live = base.join("herdr-fv-test-7-8-9");
    fs::create_dir(&live).unwrap();
    common::sweep_stale_in(base, SystemTime::now() - Duration::from_secs(3600));
    assert!(
        live.exists(),
        "a dir newer than the cutoff (a live run's) must be spared"
    );
}
