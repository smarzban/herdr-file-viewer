//! Update-available check — tell the user when a newer release exists.
//!
//! A bounded, read-only, fail-silent feature: once per 24h it runs `git ls-remote` against
//! our own repo (off the UI thread), compares the highest stable tag to the version compiled
//! into this binary, and — if behind — surfaces a one-line banner. Disabled entirely by the
//! `HERDR_FILE_VIEWER_NO_UPDATE_CHECK` env var. No new dependencies, no telemetry, no mutation.

pub mod version;
