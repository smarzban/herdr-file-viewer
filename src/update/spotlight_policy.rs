//! Pure policy for accepting, caching, and projecting the remote project spotlight.
//!
//! This module owns no I/O or clock. Its caller supplies the remote input, the session-start
//! timestamp, and successful retrieval timestamps, then applies the returned cache deltas.

use super::CHECK_INTERVAL_SECS;
use crate::render::neutralize_plain_text;

/// The result of retrieving the remote spotlight source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpotlightInput {
    /// The remote source existed and supplied these exact bytes.
    Available(Vec<u8>),
    /// The remote source was definitively absent (for example, a missing document).
    Missing,
    /// Retrieval failed, so it is not evidence that any cached spotlight was withdrawn.
    Unavailable,
}

/// An accepted spotlight. `identity` and `body` retain the exact bytes received from the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedSpotlight {
    /// The byte-exact whole document, used as the dismissal identity.
    pub identity: Vec<u8>,
    /// The first nonblank level-one heading, neutralized for a one-line status display.
    pub title: String,
    /// Every byte after the consumed title line, retained without rewriting.
    pub body: Vec<u8>,
}

/// The three meaningful source projections: usable content, an explicit withdrawal, or no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpotlightProjection {
    Accepted(AcceptedSpotlight),
    Withdrawn,
    Unavailable,
}

/// Project remote input without side effects.
///
/// A usable document is valid UTF-8 whose first nonblank line is a `# ` heading with a nonempty
/// visible title. Leading blank lines are only locating metadata; the stored body starts directly
/// after the consumed title line. All bytes in that body and in the complete identity are copied
/// exactly, including terminal-looking bytes which remain for the renderer's normal boundary.
pub fn project(input: SpotlightInput) -> SpotlightProjection {
    match input {
        SpotlightInput::Unavailable => SpotlightProjection::Unavailable,
        SpotlightInput::Missing => SpotlightProjection::Withdrawn,
        SpotlightInput::Available(bytes) => accept(bytes)
            .map(SpotlightProjection::Accepted)
            .unwrap_or(SpotlightProjection::Withdrawn),
    }
}

fn accept(bytes: Vec<u8>) -> Option<AcceptedSpotlight> {
    let source = std::str::from_utf8(&bytes).ok()?;
    let mut offset = 0;

    for line in source.split_inclusive('\n') {
        let line_without_ending = line.trim_end_matches(['\r', '\n']);
        if line_without_ending.trim().is_empty() {
            offset += line.len();
            continue;
        }
        let raw_title = line_without_ending.strip_prefix("# ")?;
        // The shared scanner removes terminal controls. Status presentation also excludes
        // Unicode formatting and line-separator characters that can reorder or split its one line.
        let title = neutralize_plain_text(raw_title)
            .chars()
            .filter(|character| {
                !matches!(
                    character,
                    '\u{61c}'
                        | '\u{200b}'..='\u{200f}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2028}'
                        | '\u{2029}'
                        | '\u{2060}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{feff}'
                )
            })
            .collect::<String>()
            .trim()
            .to_string();
        if title.is_empty() {
            return None;
        }
        let body_start = offset + line.len();
        return Some(AcceptedSpotlight {
            identity: bytes.clone(),
            title,
            body: bytes[body_start..].to_vec(),
        });
    }

    None
}

/// Whether a cache timestamp was fresh at the fixed beginning of this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Fresh,
    Stale,
    /// The persisted retrieval time is later than this session start, so clock skew must recheck.
    Future,
}

/// Measure freshness from the supplied session start, never from a live clock.
pub fn freshness_at_session_start(
    session_started_at_unix: u64,
    retrieved_at_unix: u64,
) -> Freshness {
    if retrieved_at_unix > session_started_at_unix {
        return Freshness::Future;
    }
    if session_started_at_unix - retrieved_at_unix >= CHECK_INTERVAL_SECS {
        Freshness::Stale
    } else {
        Freshness::Fresh
    }
}

/// The in-memory cache shape for a project spotlight.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpotlightCache {
    accepted: Option<AcceptedSpotlight>,
    retrieved_at_unix: Option<u64>,
    dismissed_identity: Option<Vec<u8>>,
}

/// A cache mutation selected by a source projection. Applying `Preserve` is a no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpotlightCacheDelta {
    Accepted {
        spotlight: AcceptedSpotlight,
        retrieved_at_unix: u64,
    },
    Withdrawn {
        retrieved_at_unix: u64,
    },
    Preserve,
}

/// Convert a source projection into a pure cache delta with an explicit retrieval time.
pub fn cache_delta(projection: SpotlightProjection, retrieved_at_unix: u64) -> SpotlightCacheDelta {
    match projection {
        SpotlightProjection::Accepted(spotlight) => SpotlightCacheDelta::Accepted {
            spotlight,
            retrieved_at_unix,
        },
        SpotlightProjection::Withdrawn => SpotlightCacheDelta::Withdrawn { retrieved_at_unix },
        SpotlightProjection::Unavailable => SpotlightCacheDelta::Preserve,
    }
}

impl SpotlightCache {
    /// Rehydrate a persisted dismissal without accepting or displaying cached content. A later
    /// accepted document retains it only when its exact identity matches.
    pub fn with_remembered_dismissal(identity: Vec<u8>) -> Self {
        Self {
            dismissed_identity: Some(identity),
            ..Self::default()
        }
    }

    /// Apply a selected delta. A changed identity clears a remembered dismissal; an identical one
    /// retains it. Withdrawal clears accepted content while recording the successful retrieval
    /// time for future session-start freshness decisions.
    pub fn apply(&mut self, delta: SpotlightCacheDelta) {
        match delta {
            SpotlightCacheDelta::Accepted {
                spotlight,
                retrieved_at_unix,
            } => {
                if self.dismissed_identity.as_deref() != Some(spotlight.identity.as_slice()) {
                    self.dismissed_identity = None;
                }
                self.accepted = Some(spotlight);
                self.retrieved_at_unix = Some(retrieved_at_unix);
            }
            SpotlightCacheDelta::Withdrawn { retrieved_at_unix } => {
                self.accepted = None;
                self.retrieved_at_unix = Some(retrieved_at_unix);
            }
            SpotlightCacheDelta::Preserve => {}
        }
    }

    /// Remember dismissal of the currently accepted exact document. There is no status to dismiss
    /// without accepted content.
    pub fn dismiss(&mut self) {
        if let Some(spotlight) = &self.accepted {
            self.dismissed_identity = Some(spotlight.identity.clone());
        }
    }

    /// The successful retrieval timestamp, if this cache has one.
    pub fn retrieved_at_unix(&self) -> Option<u64> {
        self.retrieved_at_unix
    }

    /// The status title unless this exact accepted document was dismissed.
    pub fn status_title(&self) -> Option<&str> {
        self.accepted
            .as_ref()
            .filter(|spotlight| {
                self.dismissed_identity.as_deref() != Some(spotlight.identity.as_slice())
            })
            .map(|spotlight| spotlight.title.as_str())
    }

    /// The accepted What's New body, independent of the status dismissal state.
    pub fn whats_new_body(&self) -> Option<&[u8]> {
        self.accepted
            .as_ref()
            .map(|spotlight| spotlight.body.as_slice())
    }
}

/// Session-only freshness state. A conclusive retrieval is fresh for the remainder of this run,
/// rather than aging according to a later live-clock read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpotlightSession {
    session_started_at_unix: u64,
    resolved_in_session: bool,
}

impl SpotlightSession {
    pub fn new(session_started_at_unix: u64) -> Self {
        Self {
            session_started_at_unix,
            resolved_in_session: false,
        }
    }

    /// Whether this session should attempt retrieval. Freshness remains fixed at session start.
    pub fn should_retrieve(&self, cache: &SpotlightCache) -> bool {
        !self.resolved_in_session
            && match cache.retrieved_at_unix {
                Some(retrieved_at_unix) => {
                    freshness_at_session_start(self.session_started_at_unix, retrieved_at_unix)
                        != Freshness::Fresh
                }
                None => true,
            }
    }

    /// Apply a result and mark a successful acceptance or withdrawal as resolved for this session.
    /// An unavailable result preserves the cache and remains eligible for a later retry.
    pub fn apply(&mut self, cache: &mut SpotlightCache, delta: SpotlightCacheDelta) {
        self.resolved_in_session |= matches!(
            &delta,
            SpotlightCacheDelta::Accepted { .. } | SpotlightCacheDelta::Withdrawn { .. }
        );
        cache.apply(delta);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::CHECK_INTERVAL_SECS as FRESHNESS_SECS;

    fn accepted(input: &str) -> AcceptedSpotlight {
        match project(SpotlightInput::Available(input.as_bytes().to_vec())) {
            SpotlightProjection::Accepted(spotlight) => spotlight,
            other => panic!("expected an accepted spotlight, got {other:?}"),
        }
    }

    #[test]
    fn accepted_input_consumes_the_first_nonblank_h1_and_preserves_body_and_identity_bytes() {
        // These cases use mixed line endings and terminal-looking bytes in the body. The policy
        // validates only enough to select the title: all accepted source/body bytes remain exact.
        let cases = [
            (
                "leading blanks and CRLF title",
                "\n\r\n# Project Café\r\nbody\x1b[2J\r\n",
                "Project Café",
                b"body\x1b[2J\r\n".as_slice(),
            ),
            (
                "body without a final newline",
                "# Tokyo 東京\nlast body bytes",
                "Tokyo 東京",
                b"last body bytes".as_slice(),
            ),
            (
                "heading-only document",
                "# Only title",
                "Only title",
                b"".as_slice(),
            ),
        ];

        for (name, input, title, body) in cases {
            let spotlight = accepted(input);
            assert_eq!(spotlight.title, title, "{name}: title from the H1");
            assert_eq!(spotlight.body, body, "{name}: all body bytes are exact");
            assert_eq!(
                spotlight.identity,
                input.as_bytes(),
                "{name}: whole document identity is byte-exact"
            );
        }
    }

    #[test]
    fn accepted_titles_are_neutralized_at_the_policy_boundary() {
        let cases = [
            (
                "control and bidi characters",
                "# be\x1b[2Jfore\x07 \u{9b}\u{202e}spoof\u{202c}\u{200d}ed\u{2028}end\nbody\n",
                "before spoofedend",
            ),
            (
                "OSC hyperlink",
                "# before\x1b]8;;https://evil.invalid/\x1b\\after\nbody\n",
                "beforeafter",
            ),
            (
                "OSC clipboard",
                "# before\x1b]52;c;c3RvbGVu\x07after\nbody\n",
                "beforeafter",
            ),
            ("malformed escape", "# before\x1b[31\nbody\n", "before"),
            ("Unicode", "# Café 東京\nbody\n", "Café 東京"),
        ];

        for (name, input, expected_title) in cases {
            let spotlight = accepted(input);
            assert_eq!(
                spotlight.title, expected_title,
                "{name}: title is neutralized"
            );
            assert_eq!(
                spotlight.title.lines().count(),
                1,
                "{name}: status title is one visible line"
            );
            assert_eq!(
                spotlight.identity,
                input.as_bytes(),
                "{name}: identity stays exact"
            );
            assert_eq!(spotlight.body, b"body\n", "{name}: body stays exact");
        }
    }

    #[test]
    fn missing_invalid_or_headingless_input_withdraws_while_unavailable_is_distinct() {
        let withdrawn = [
            SpotlightInput::Missing,
            SpotlightInput::Available(Vec::new()),
            SpotlightInput::Available(b" \r\n\t".to_vec()),
            SpotlightInput::Available(vec![b'#', b' ', 0xff]),
            SpotlightInput::Available(b"ordinary body without a heading\n".to_vec()),
            SpotlightInput::Available(b"# \r\nbody\n".to_vec()),
        ];

        for input in withdrawn {
            assert_eq!(project(input), SpotlightProjection::Withdrawn);
        }
        assert_eq!(
            project(SpotlightInput::Unavailable),
            SpotlightProjection::Unavailable,
            "a failed retrieval is not evidence that the remote source withdrew the spotlight"
        );
    }

    #[test]
    fn freshness_uses_session_start_not_the_live_clock_and_reports_future_cache_time() {
        let start = 1_000_000;
        assert_eq!(
            freshness_at_session_start(start, start - FRESHNESS_SECS + 1),
            Freshness::Fresh,
            "one second below the 24h boundary remains fresh"
        );
        assert_eq!(
            freshness_at_session_start(start, start - FRESHNESS_SECS),
            Freshness::Stale,
            "at exactly 24h the cache is stale"
        );
        assert_eq!(
            freshness_at_session_start(start, start + 1),
            Freshness::Future,
            "a cache timestamp later than session start is an explicit clock-skew state"
        );
    }

    #[test]
    fn an_accepted_retrieval_is_fresh_for_the_rest_of_its_session() {
        let session_start = 100;
        let mut session = SpotlightSession::new(session_start);
        let mut cache = SpotlightCache::default();
        let delta = cache_delta(
            project(SpotlightInput::Available(b"# Current\nbody\n".to_vec())),
            session_start + FRESHNESS_SECS * 2,
        );

        session.apply(&mut cache, delta);
        assert!(
            !session.should_retrieve(&cache),
            "an in-session acceptance is fresh even if its retrieval time is after session start"
        );
        assert!(
            !session.should_retrieve(&cache),
            "freshness does not age into a retrieval during the same session"
        );
    }

    #[test]
    fn unavailable_preserves_cache_and_dismissal_hides_only_matching_status_not_whats_new_body() {
        let mut cache = SpotlightCache::default();
        cache.apply(cache_delta(
            project(SpotlightInput::Available(b"# First\nfirst body\n".to_vec())),
            10,
        ));
        let before_unavailable = cache.clone();
        cache.apply(cache_delta(SpotlightProjection::Unavailable, 20));
        assert_eq!(
            cache, before_unavailable,
            "unavailable input preserves every cache field"
        );

        cache.dismiss();
        assert_eq!(
            cache.status_title(),
            None,
            "dismissal suppresses matching status"
        );
        assert_eq!(
            cache.whats_new_body(),
            Some(b"first body\n".as_slice()),
            "dismissal never hides accepted What's New body"
        );

        // Reaccepting the exact document remembers the dismissal because identity is exact bytes.
        cache.apply(cache_delta(
            project(SpotlightInput::Available(b"# First\nfirst body\n".to_vec())),
            30,
        ));
        assert_eq!(
            cache.status_title(),
            None,
            "same identity remains dismissed"
        );

        // A different byte identity is new content, even when the visible title is unchanged.
        cache.apply(cache_delta(
            project(SpotlightInput::Available(
                b"# First\nchanged body\n".to_vec(),
            )),
            40,
        ));
        assert_eq!(cache.status_title(), Some("First"));
        assert_eq!(cache.whats_new_body(), Some(b"changed body\n".as_slice()));
    }

    #[test]
    fn session_retrieves_without_a_cache_and_rechecks_stale_or_future_cache_times() {
        let session_started_at_unix = 1_000_000;
        let session = SpotlightSession::new(session_started_at_unix);

        assert!(
            session.should_retrieve(&SpotlightCache::default()),
            "an absent retrieval timestamp requires a first retrieval"
        );

        let stale = SpotlightCache {
            retrieved_at_unix: Some(session_started_at_unix - FRESHNESS_SECS),
            ..SpotlightCache::default()
        };
        assert!(
            session.should_retrieve(&stale),
            "the exact freshness boundary requires retrieval"
        );

        let future = SpotlightCache {
            retrieved_at_unix: Some(session_started_at_unix + 1),
            ..SpotlightCache::default()
        };
        assert!(
            session.should_retrieve(&future),
            "a future cache timestamp requires retrieval"
        );
    }

    #[test]
    fn withdrawal_does_not_forget_a_dismissal_of_the_same_document() {
        let input = b"# Title\nbody\n".to_vec();
        let mut cache = SpotlightCache::default();
        cache.apply(cache_delta(
            project(SpotlightInput::Available(input.clone())),
            10,
        ));
        cache.dismiss();
        cache.apply(cache_delta(SpotlightProjection::Withdrawn, 20));
        cache.apply(cache_delta(project(SpotlightInput::Available(input)), 30));

        assert_eq!(
            cache.status_title(),
            None,
            "an identical spotlight stays dismissed after a transient withdrawal"
        );
    }

    #[test]
    fn withdrawn_delta_clears_accepted_content_but_records_the_retrieval_time() {
        let mut cache = SpotlightCache::default();
        cache.apply(cache_delta(
            project(SpotlightInput::Available(b"# Title\nbody\n".to_vec())),
            10,
        ));

        cache.apply(cache_delta(SpotlightProjection::Withdrawn, 20));
        assert_eq!(cache.accepted, None);
        assert_eq!(cache.retrieved_at_unix(), Some(20));
        assert_eq!(cache.status_title(), None);
        assert_eq!(cache.whats_new_body(), None);
    }

    #[test]
    fn conclusive_withdrawals_throttle_fresh_sessions_but_unavailable_state_does_not() {
        let session_started_at_unix = 1_000_000;
        let fresh_retrieved_at_unix = session_started_at_unix - FRESHNESS_SECS + 1;
        let session = SpotlightSession::new(session_started_at_unix);

        let mut withdrawn = SpotlightCache::default();
        withdrawn.apply(cache_delta(
            SpotlightProjection::Withdrawn,
            fresh_retrieved_at_unix,
        ));
        assert_eq!(withdrawn.status_title(), None);
        assert_eq!(withdrawn.whats_new_body(), None);
        assert!(
            !session.should_retrieve(&withdrawn),
            "a fresh withdrawal is a successful retrieval"
        );

        let mut invalid = SpotlightCache::default();
        invalid.apply(cache_delta(
            project(SpotlightInput::Available(b"no heading\n".to_vec())),
            fresh_retrieved_at_unix,
        ));
        assert_eq!(invalid, withdrawn, "invalid present content withdraws");

        let mut stale = SpotlightCache::default();
        stale.apply(cache_delta(
            SpotlightProjection::Withdrawn,
            session_started_at_unix - FRESHNESS_SECS,
        ));
        assert!(
            session.should_retrieve(&stale),
            "a stale withdrawal retries"
        );
        assert!(
            session.should_retrieve(&SpotlightCache::default()),
            "unavailable state has no successful retrieval timestamp"
        );
    }
}
