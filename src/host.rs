//! Host Adapter — the herdr boundary: parse the injected launch context (AC-26).
//!
//! `HERDR_PLUGIN_CONTEXT_JSON` is parsed defensively — malformed or missing input degrades
//! to a minimal `{ cwd }` context, never a panic (AC-26).

use crate::context::LaunchContext;
use serde::Deserialize;
use std::path::PathBuf;

/// The shape of `HERDR_PLUGIN_CONTEXT_JSON`. Every field is optional so a partial or absent
/// object degrades gracefully rather than failing to parse; unknown fields are ignored.
#[derive(Deserialize, Default)]
struct RawContext {
    cwd: Option<String>,
    base_branch: Option<String>,
}

/// Build a `LaunchContext` from the process environment: the injected context JSON, falling
/// back to the process working directory. Never panics (AC-26).
pub fn from_env() -> LaunchContext {
    let json = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok();
    let cwd = std::env::current_dir().unwrap_or_default();
    parse_context(json.as_deref(), cwd)
}

/// Pure parser behind [`from_env`] (testable without touching process env). Missing or
/// malformed JSON yields a minimal `{ cwd: fallback_cwd }` context (AC-26).
pub fn parse_context(json: Option<&str>, fallback_cwd: PathBuf) -> LaunchContext {
    let raw: RawContext = json
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    LaunchContext {
        cwd: raw.cwd.map(PathBuf::from).unwrap_or(fallback_cwd),
        base_branch: raw.base_branch,
    }
}
