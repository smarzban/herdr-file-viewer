//! Input Dispatcher — map raw key events to [`Intent`]s (AC-18).
//!
//! Keyboard-complete: every viewer function has at least one key. Unbound keys are a no-op
//! (`None`). Char bindings fire only with no active modifier, so a chord like Ctrl+C (the
//! terminal interrupt) never trips an intent. No key yields an editing action — the closed
//! [`Intent`] set has none (AC-N3).

use crate::config::KeySpec;
use crate::intent::Intent;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// The resolved key-to-[`Intent`] decode map plus the set of intents whose key set came from
/// user config (a **custom binding**).
///
/// Built once from the [`REGISTRY`] (defaults) or, later, the registry layered with a user's
/// `[keys]` config. The [dispatcher](decode) only ever reads it — it never sees the raw registry
/// or config. Fields are private; callers use the accessors. `customized` is empty for the default
/// build; the T-5 bindings resolver populates it from a user's config.
pub(crate) struct EffectiveBindings {
    /// Which [`Intent`] each logical key decodes to.
    map: HashMap<KeyCode, Intent>,
    /// Intents whose effective key set came from a user `[keys]` entry; empty under the defaults.
    /// Populated by [`resolve_bindings`]; read via [`EffectiveBindings::is_customized`] (wired into
    /// the T-7 Keybindings overlay, exercised by this module's resolver tests).
    #[allow(dead_code)]
    // read only in is_customized (test + T-7 overlay), not yet on the hot path.
    customized: HashSet<Intent>,
}

impl EffectiveBindings {
    /// The [`Intent`] this logical key decodes to, or `None` if the key is unbound.
    pub(crate) fn intent_for(&self, code: KeyCode) -> Option<Intent> {
        self.map.get(&code).copied()
    }

    /// Whether `intent`'s effective key set came from user config (a custom binding).
    pub(crate) fn is_customized(&self, intent: Intent) -> bool {
        self.customized.contains(&intent)
    }

    /// The effective key(s) that decode to `intent`, in a deterministic order (sorted by their
    /// rendered [`key_label`]), so the Keybindings view-model and its tests are stable. Empty when
    /// `intent` has no effective key at all: e.g. an intent whose only key was `Esc`, which the
    /// no-lockout floor reassigns to `Close`. Consumed by the T-7 Keybindings view-model.
    pub(crate) fn keys_for(&self, intent: Intent) -> Vec<KeyCode> {
        let mut codes: Vec<KeyCode> = self
            .map
            .iter()
            .filter(|&(_, &i)| i == intent)
            .map(|(&code, _)| code)
            .collect();
        codes.sort_by_key(|&c| key_label(c));
        codes
    }
}

/// Fold the [`REGISTRY`] into the default [`EffectiveBindings`]: every default key of every row
/// decodes to that row's intent, with no key customized.
///
/// Pure — no config, env, or I/O (AC-24). Delegates to [`resolve_bindings`] with **no** `[keys]`
/// config so the default and configured paths share one code path (the resolver with an empty
/// override is the identity over the registry). Because the registry's default keys are
/// collision-free (test-enforced, AC-3), the resulting map reproduces today's `map_key` bindings
/// verbatim (AC-1).
pub(crate) fn default_bindings() -> EffectiveBindings {
    resolve_bindings(registry(), None).0
}

/// Decode a key event into an [`Intent`] against a set of effective bindings, or `None` if the
/// key is unbound.
///
/// Pure (AC-24). Control-style chords (Ctrl/Alt/Super/…) never fire an intent so reserved combos
/// (e.g. Ctrl+C) stay clear; Shift is allowed, because shifted characters (`<` / `>`) are ordinary
/// typing — not a chord — and some terminals report them with the Shift bit set. `key.code` is the
/// already-normalized lookup key: a shifted char carries its case in `key.code`, and a named key
/// reports its base [`KeyCode`] with Shift in the modifiers (stripped by the guard below).
pub(crate) fn decode(key: KeyEvent, bindings: &EffectiveBindings) -> Option<Intent> {
    if key.modifiers.difference(KeyModifiers::SHIFT) != KeyModifiers::NONE {
        return None;
    }
    bindings.intent_for(key.code)
}

/// Borrow the process-lifetime default [`EffectiveBindings`], built once from the [`REGISTRY`].
fn default_bindings_static() -> &'static EffectiveBindings {
    static DEFAULT_BINDINGS: OnceLock<EffectiveBindings> = OnceLock::new();
    DEFAULT_BINDINGS.get_or_init(default_bindings)
}

/// Decode a key event into an [`Intent`] under the **default** bindings, or `None` if unbound.
///
/// Thin wrapper over [`decode`] against the registry-derived [`default_bindings`]; the single call
/// site and the existing dispatcher tests exercise this to prove the default map is unchanged
/// (AC-1). The run loop switches to the effective bindings in T-6.
pub fn map_key(key: KeyEvent) -> Option<Intent> {
    decode(key, default_bindings_static())
}

/// One row of the [keybinding registry](REGISTRY): a single [`Intent`] paired with its stable
/// snake_case name, its default key(s), and a human description.
///
/// This is the single source of truth for each global action's default binding. The dispatcher,
/// the help overlay, and the README docs-consistency check all derive from it, so a key, its
/// config name, and its description live in exactly one place. `default_keys` reproduces the
/// current [`map_key`] bindings verbatim; `name` is the public `[keys]` config key.
pub(crate) struct Binding {
    /// The global action this row binds. Consumed by [`default_bindings`].
    pub intent: Intent,
    /// The stable snake_case identifier, used as the `[keys]` config key (e.g. `nav_up`) and the
    /// name [`resolve_bindings`] matches a config entry against.
    pub name: &'static str,
    /// The default key(s) that decode to `intent` absent any user config. Non-empty. Consumed by
    /// [`default_bindings`].
    pub default_keys: &'static [KeyCode],
    /// A concise, human-readable one-liner describing the action.
    pub description: &'static str,
    /// The display group this action belongs to in the Keybindings overlay (one of
    /// [`CATEGORY_ORDER`]). Purely presentational: it groups the flat registry into sections and
    /// never affects decoding. Every row's category must be a member of [`CATEGORY_ORDER`]
    /// (test-enforced), so the overlay can iterate categories in order and lose no action.
    pub category: &'static str,
}

/// The Keybindings-overlay display groups, in render order. Each [`Binding::category`] is one of
/// these; the view-model walks them in this order and lists every action whose category matches.
/// Presentational only (grouping does not touch the dispatcher).
pub(crate) const CATEGORY_ORDER: &[&str] = &[
    "Navigation",
    "View & layout",
    "Git & filters",
    "Open & copy",
    "Search & jump",
    "Session",
];

/// The keybinding registry: one [`Binding`] per [`Intent::ALL`] member, in that order.
///
/// The `default_keys` mirror [`map_key`] exactly (behavior-preserving foundation). Invariants,
/// all test-enforced rather than at runtime: every `Intent::ALL` member appears exactly once
/// (AC-2), no two rows share a `name` (AC-5), and no [`KeyCode`] is a default of two rows (AC-3).
pub(crate) const REGISTRY: &[Binding] = &[
    Binding {
        intent: Intent::NavUp,
        name: "nav_up",
        default_keys: &[KeyCode::Up, KeyCode::Char('k')],
        description: "Move the tree cursor up one row.",
        category: "Navigation",
    },
    Binding {
        intent: Intent::NavDown,
        name: "nav_down",
        default_keys: &[KeyCode::Down, KeyCode::Char('j')],
        description: "Move the tree cursor down one row.",
        category: "Navigation",
    },
    Binding {
        intent: Intent::Expand,
        name: "expand",
        default_keys: &[KeyCode::Right, KeyCode::Char('l')],
        description: "Expand the selected directory.",
        category: "Navigation",
    },
    Binding {
        intent: Intent::Collapse,
        name: "collapse",
        default_keys: &[KeyCode::Left, KeyCode::Char('h')],
        description: "Collapse the selected directory.",
        category: "Navigation",
    },
    Binding {
        intent: Intent::Activate,
        name: "activate",
        default_keys: &[KeyCode::Enter],
        description: "Activate the selected node: expand/collapse a directory, or open a file.",
        category: "Navigation",
    },
    Binding {
        intent: Intent::OpenFullscreen,
        name: "open_fullscreen",
        default_keys: &[KeyCode::Char('Z')],
        description: "Toggle full-screen reading of the selected file (in-pane plus herdr pane zoom).",
        category: "View & layout",
    },
    Binding {
        intent: Intent::ToggleIgnore,
        name: "toggle_ignore",
        default_keys: &[KeyCode::Char('i')],
        description: "Reveal or hide gitignored files.",
        category: "Git & filters",
    },
    Binding {
        intent: Intent::ToggleHidden,
        name: "toggle_hidden",
        default_keys: &[KeyCode::Char('.')],
        description: "Hide or reveal dot-prefixed (hidden) files and folders.",
        category: "Git & filters",
    },
    Binding {
        intent: Intent::ToggleChangedOnly,
        name: "toggle_changed_only",
        default_keys: &[KeyCode::Char('c')],
        description: "Restrict the tree to changed files, or restore the full tree.",
        category: "Git & filters",
    },
    Binding {
        intent: Intent::ToggleBaseline,
        name: "toggle_baseline",
        default_keys: &[KeyCode::Char('b')],
        description: "Switch the diff baseline between base-branch and HEAD.",
        category: "Git & filters",
    },
    Binding {
        intent: Intent::ToggleDeltaRaw,
        name: "toggle_delta_raw",
        default_keys: &[KeyCode::Char('D')],
        description: "Toggle the diff view between delta rendering and plain git diff text.",
        category: "Git & filters",
    },
    Binding {
        intent: Intent::CycleView,
        name: "cycle_view",
        default_keys: &[KeyCode::Char('v')],
        description: "Cycle the content pane's view mode.",
        category: "View & layout",
    },
    Binding {
        intent: Intent::OpenInEditor,
        name: "open_in_editor",
        default_keys: &[KeyCode::Char('e')],
        description: "Hand the selected file off to an external editor.",
        category: "Open & copy",
    },
    Binding {
        intent: Intent::OpenWithApp,
        name: "open_with_app",
        default_keys: &[KeyCode::Char('O')],
        description: "Open the selected entry with the OS default application.",
        category: "Open & copy",
    },
    Binding {
        intent: Intent::RevealInFileManager,
        name: "reveal_in_file_manager",
        default_keys: &[KeyCode::Char('R')],
        description: "Reveal the selected entry in the OS file manager.",
        category: "Open & copy",
    },
    Binding {
        intent: Intent::CopyRepoPath,
        name: "copy_repo_path",
        default_keys: &[KeyCode::Char('y')],
        description: "Copy the selected node's repo-relative path to the clipboard.",
        category: "Open & copy",
    },
    Binding {
        intent: Intent::CopyAbsPath,
        name: "copy_abs_path",
        default_keys: &[KeyCode::Char('Y')],
        description: "Copy the selected node's absolute path to the clipboard.",
        category: "Open & copy",
    },
    Binding {
        intent: Intent::ToggleFocus,
        name: "toggle_focus",
        default_keys: &[KeyCode::Tab],
        description: "Move focus between the tree and content columns.",
        category: "View & layout",
    },
    Binding {
        intent: Intent::ShrinkTree,
        name: "shrink_tree",
        default_keys: &[KeyCode::Char('<')],
        description: "Narrow the tree column.",
        category: "View & layout",
    },
    Binding {
        intent: Intent::GrowTree,
        name: "grow_tree",
        default_keys: &[KeyCode::Char('>')],
        description: "Widen the tree column.",
        category: "View & layout",
    },
    Binding {
        intent: Intent::ToggleWrap,
        name: "toggle_wrap",
        default_keys: &[KeyCode::Char('w')],
        description: "Force content-line wrapping on or off.",
        category: "View & layout",
    },
    Binding {
        intent: Intent::ToggleZoom,
        name: "toggle_zoom",
        default_keys: &[KeyCode::Char('z')],
        description: "Hide the tree so the content pane fills the frame, or restore the split.",
        category: "View & layout",
    },
    Binding {
        intent: Intent::Refresh,
        name: "refresh",
        default_keys: &[KeyCode::Char('r')],
        description: "Re-read git state (status and changed-set) and re-render.",
        category: "Git & filters",
    },
    Binding {
        intent: Intent::DismissUpdate,
        name: "dismiss_update",
        default_keys: &[KeyCode::Char('u')],
        description: "Dismiss the update-available banner for this session.",
        category: "Session",
    },
    Binding {
        intent: Intent::SwitchWorktree,
        name: "switch_worktree",
        default_keys: &[KeyCode::Char('W')],
        description: "Open the worktree picker to re-root at another git worktree.",
        category: "Session",
    },
    Binding {
        intent: Intent::OpenFinder,
        name: "open_finder",
        default_keys: &[KeyCode::Char('f')],
        description: "Open the go-to-file finder to navigate to any file by fuzzy query.",
        category: "Search & jump",
    },
    Binding {
        intent: Intent::OpenGoToLine,
        name: "open_go_to_line",
        default_keys: &[KeyCode::Char(':')],
        description: "Open the go-to-line prompt to scroll the content pane to a line number.",
        category: "Search & jump",
    },
    Binding {
        intent: Intent::OpenSearch,
        name: "open_search",
        default_keys: &[KeyCode::Char('/')],
        description: "Open the search prompt at the bottom of the content pane.",
        category: "Search & jump",
    },
    Binding {
        intent: Intent::NextMatch,
        name: "next_match",
        default_keys: &[KeyCode::Char('n')],
        description: "Advance to the next search match (wraps at the end).",
        category: "Search & jump",
    },
    Binding {
        intent: Intent::PrevMatch,
        name: "prev_match",
        default_keys: &[KeyCode::Char('N')],
        description: "Retreat to the previous search match (wraps at the start).",
        category: "Search & jump",
    },
    Binding {
        intent: Intent::TreeScrollLeft,
        name: "tree_scroll_left",
        default_keys: &[KeyCode::Char('H')],
        description: "Scroll the tree pane left.",
        category: "View & layout",
    },
    Binding {
        intent: Intent::TreeScrollRight,
        name: "tree_scroll_right",
        default_keys: &[KeyCode::Char('L')],
        description: "Scroll the tree pane right.",
        category: "View & layout",
    },
    Binding {
        intent: Intent::ShowHelp,
        name: "show_help",
        default_keys: &[KeyCode::Char('?')],
        description: "Open the in-app help overlay (What's New and About).",
        category: "Session",
    },
    Binding {
        intent: Intent::Close,
        name: "close",
        default_keys: &[KeyCode::Char('q'), KeyCode::Esc],
        description: "Close the viewer and return to the prior pane.",
        category: "Session",
    },
];

/// Borrow the [keybinding registry](REGISTRY) rows: the single source of truth for each global
/// action's default key(s), snake_case name, and description.
pub(crate) fn registry() -> &'static [Binding] {
    REGISTRY
}

/// Translate one user **key spec** token into the logical [`KeyCode`] it names, or `None` for any
/// token outside the modifier-free **bindable key** surface (AC-11, AC-12; enforces NC-1/NC-5).
///
/// Rules:
/// - A token that is exactly one Unicode character is that character key, **case-sensitive** — so
///   `"V"` and `"v"` differ, and shifted punctuation (`"<"`, `"?"`) is its own key.
/// - Otherwise the token is matched **case-insensitively** against the pinned named-key table:
///   `Tab`, `Enter`, `Esc`, the four arrows, `Home`, `End`, `PageUp`, `PageDown`, `Space` (the
///   space character), `Backspace`, `Delete`, `Insert`, and `F1`..`F12`.
/// - Everything else returns `None`: the empty string, any other multi-character token (`"abc"`,
///   `"PgDn"`), an F-key out of the `1..=12` range (`"F0"`, `"F13"`), and — by construction, since
///   the whitelist admits no `+` chord — any `Ctrl+`/`Alt+` token. No control-chord binding is even
///   expressible (NC-1).
///
/// Pure, total, and never panics (AC-24).
pub(crate) fn parse_key_spec(s: &str) -> Option<KeyCode> {
    // A token of exactly one Unicode character is that character key, case-sensitive.
    if s.chars().count() == 1 {
        return Some(KeyCode::Char(s.chars().next().unwrap()));
    }
    // Named keys match case-insensitively (all ASCII).
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "tab" => Some(KeyCode::Tab),
        "enter" => Some(KeyCode::Enter),
        "esc" => Some(KeyCode::Esc),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pageup" => Some(KeyCode::PageUp),
        "pagedown" => Some(KeyCode::PageDown),
        "space" => Some(KeyCode::Char(' ')),
        "backspace" => Some(KeyCode::Backspace),
        "delete" => Some(KeyCode::Delete),
        "insert" => Some(KeyCode::Insert),
        // "f1".."f12": parse the number after "f", accepting only 1..=12 (rejects "f0", "f13",
        // "f1x", and — as a two-plus-char token that never reaches here — a bare "f").
        _ => {
            let n: u8 = lower.strip_prefix('f')?.parse().ok()?;
            (1..=12).contains(&n).then_some(KeyCode::F(n))
        }
    }
}

/// Why a `[keys]` entry was rejected during binding resolution. Renders to a short human string
/// (via [`std::fmt::Display`]) so the T-7 Keybindings help section can surface which bindings were
/// ignored rather than dropping them silently (AC-16).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // variants/payloads read by the T-7 overlay; exercised in this module's tests.
pub(crate) enum RejectReason {
    /// The entry's key is not a recognized registry [intent name](Binding::name) (AC-14).
    UnknownIntent,
    /// The spec named a non-[bindable key](parse_key_spec), or named no key at all (AC-12); carries
    /// the offending token, or the sentinel `"empty"` when the spec listed nothing.
    BadKeySpec(String),
    /// The entry shares a key with another valid entry — a duplicate-key clash (AC-15); carries the
    /// rendered key label.
    DuplicateKey(String),
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RejectReason::UnknownIntent => write!(f, "unknown intent name"),
            RejectReason::BadKeySpec(token) => write!(f, "unbindable key \"{token}\""),
            RejectReason::DuplicateKey(key) => write!(f, "duplicate key \"{key}\""),
        }
    }
}

/// One rejected `[keys]` entry: the intent name the user wrote and why it was dropped. Recorded so
/// T-7 can tell the user which bindings were ignored (the surfacing path, AC-16).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // fields read by the T-6 wiring / T-7 overlay; exercised in this module's tests.
pub(crate) struct RejectedBinding {
    /// The `[keys]` config key (intent name) as the user wrote it.
    pub name: String,
    /// Why this entry was rejected and reverted to its default key set.
    pub reason: RejectReason,
}

/// The outcome of resolving a user's `[keys]` config: the entries that were rejected (each reverting
/// to its default key set). Empty when every entry was valid, or when the config had no `[keys]`
/// table. The resolved bindings themselves ride the sibling [`EffectiveBindings`]; this records only
/// what was dropped, for the surfacing path (AC-16).
// `Default` (an empty outcome: no rejected entries) is the controller's initial value before the
// T-6 wiring resolves the real bindings, so a controller always holds a valid outcome in tests.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(dead_code)] // consumed by the T-6 wiring / T-7 overlay; exercised in this module's tests.
pub(crate) struct KeyLoadOutcome {
    pub rejected: Vec<RejectedBinding>,
}

impl KeyLoadOutcome {
    /// Whether every `[keys]` entry resolved cleanly (no rejected entries).
    #[allow(dead_code)] // consumed by the T-6 wiring / T-7 overlay; used in this module's tests.
    pub(crate) fn is_empty(&self) -> bool {
        self.rejected.is_empty()
    }
}

/// Render a logical [`KeyCode`] to a short human label (for a rejection reason, and the
/// Keybindings section). Mirrors the [`parse_key_spec`] surface in reverse.
pub(crate) fn key_label(code: KeyCode) -> String {
    match code {
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Up => "Up".to_string(),
        KeyCode::Down => "Down".to_string(),
        KeyCode::Left => "Left".to_string(),
        KeyCode::Right => "Right".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PageUp => "PageUp".to_string(),
        KeyCode::PageDown => "PageDown".to_string(),
        KeyCode::Backspace => "Backspace".to_string(),
        KeyCode::Delete => "Delete".to_string(),
        KeyCode::Insert => "Insert".to_string(),
        KeyCode::F(n) => format!("F{n}"),
        other => format!("{other:?}"),
    }
}

/// Resolve a user's `[keys]` config against the [registry](REGISTRY) into the effective
/// key -> intent map plus a [`KeyLoadOutcome`] recording every rejected entry.
///
/// Pure, total, never panics (AC-24): a function of (registry + parsed `[keys]`) only, reading no
/// env, filesystem, or global state and spawning no process. Resolution is deterministic and
/// defensive, in exactly this order:
///
/// 1. An entry whose key is not a registry [name](Binding::name) is rejected
///    ([`RejectReason::UnknownIntent`], AC-14).
/// 2. An entry whose spec names a non-[bindable key](parse_key_spec) is rejected **whole**
///    ([`RejectReason::BadKeySpec`], AC-12); a key repeated within one spec collapses to one, and a
///    spec that lists no key at all is likewise rejected so no listed intent ends up keyless.
/// 3. Two surviving entries that claim the same key are a duplicate-key clash: **both** are rejected
///    ([`RejectReason::DuplicateKey`], AC-15) and revert to their defaults.
/// 4. Surviving valid entries' keys are assigned first (replace-semantics: an un-relisted default
///    stops decoding, AC-7; an explicit key beats another intent's default, AC-10). Each intent with
///    no valid entry then keeps its default keys except those an explicit binding already claimed
///    (AC-8). Finally `Esc -> Close` is forced unconditionally — the no-lockout floor, which wins
///    over any explicit `Esc` claim (AC-18).
///
/// An intent set by a valid entry is marked customized (AC-20). With `keys = None`/empty the result
/// is exactly [`default_bindings`] (AC-1, AC-6, AC-8).
pub(crate) fn resolve_bindings(
    registry: &[Binding],
    keys: Option<&std::collections::BTreeMap<String, KeySpec>>,
) -> (EffectiveBindings, KeyLoadOutcome) {
    // 1. name -> &Binding lookup.
    let lookup: HashMap<&str, &Binding> = registry.iter().map(|b| (b.name, b)).collect();

    let mut rejected: Vec<RejectedBinding> = Vec::new();
    // Candidate valid entries: (intent, config name, deduped key set), in the config's order.
    let mut candidates: Vec<(Intent, String, Vec<KeyCode>)> = Vec::new();

    // 2. Parse & validate each config entry, in the map's deterministic (BTreeMap) order.
    if let Some(keys) = keys {
        for (name, spec) in keys {
            let Some(binding) = lookup.get(name.as_str()).copied() else {
                rejected.push(RejectedBinding {
                    name: name.clone(),
                    reason: RejectReason::UnknownIntent,
                });
                continue;
            };
            let raw: &[String] = match spec {
                KeySpec::One(s) => std::slice::from_ref(s),
                KeySpec::Many(v) => v.as_slice(),
            };
            // Parse every token; any single failure rejects the whole entry. Dedupe within the spec.
            let mut codes: Vec<KeyCode> = Vec::new();
            let mut bad: Option<String> = None;
            for token in raw {
                match parse_key_spec(token) {
                    Some(code) if !codes.contains(&code) => codes.push(code),
                    Some(_) => {} // key listed twice in one spec — harmless, keep it once.
                    None => {
                        bad = Some(token.clone());
                        break;
                    }
                }
            }
            if let Some(token) = bad {
                rejected.push(RejectedBinding {
                    name: name.clone(),
                    reason: RejectReason::BadKeySpec(token),
                });
                continue;
            }
            if codes.is_empty() {
                // e.g. `nav_up = []`: an entry that lists no key would strand its intent keyless.
                rejected.push(RejectedBinding {
                    name: name.clone(),
                    reason: RejectReason::BadKeySpec("empty".to_string()),
                });
                continue;
            }
            candidates.push((binding.intent, name.clone(), codes));
        }
    }

    // 3. Duplicate-key clash: any key claimed by two DIFFERENT candidate entries rejects all of them.
    let mut claims: HashMap<KeyCode, Vec<usize>> = HashMap::new();
    for (i, (_, _, codes)) in candidates.iter().enumerate() {
        for &code in codes {
            claims.entry(code).or_default().push(i);
        }
    }
    let clashed: HashSet<usize> = claims
        .values()
        .filter(|owners| owners.len() > 1)
        .flatten()
        .copied()
        .collect();

    let mut valid: Vec<(Intent, Vec<KeyCode>)> = Vec::new();
    for (i, (intent, name, codes)) in candidates.into_iter().enumerate() {
        if clashed.contains(&i) {
            // Report the first key of this entry that participates in a clash (deterministic).
            let key = codes
                .iter()
                .copied()
                .find(|c| claims.get(c).is_some_and(|owners| owners.len() > 1))
                .unwrap_or(codes[0]);
            rejected.push(RejectedBinding {
                name,
                reason: RejectReason::DuplicateKey(key_label(key)),
            });
        } else {
            valid.push((intent, codes));
        }
    }

    // 4. Build the effective map + customized set.
    let mut map: HashMap<KeyCode, Intent> = HashMap::new();
    let mut customized: HashSet<Intent> = HashSet::new();
    // 4a. Explicit (valid) entries first, so an explicit key beats a colliding default (AC-10).
    for (intent, codes) in &valid {
        for &code in codes {
            map.insert(code, *intent);
        }
        customized.insert(*intent);
    }
    // 4b. Defaults for every intent NOT set by a valid entry, skipping keys already claimed (AC-8);
    //     an overridden intent's defaults are dropped entirely (replace-semantics, AC-7).
    for binding in registry {
        if customized.contains(&binding.intent) {
            continue;
        }
        for &code in binding.default_keys {
            map.entry(code).or_insert(binding.intent);
        }
    }
    // 4c. Esc floor — last and unconditional: Esc always closes, even if `close` was remapped away
    //     or another entry claimed Esc (AC-18).
    map.insert(KeyCode::Esc, Intent::Close);

    (
        EffectiveBindings { map, customized },
        KeyLoadOutcome { rejected },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The canonical key bindings, also used to prove keyboard-completeness.
    const BINDINGS: &[(KeyCode, Intent)] = &[
        (KeyCode::Up, Intent::NavUp),
        (KeyCode::Char('k'), Intent::NavUp),
        (KeyCode::Down, Intent::NavDown),
        (KeyCode::Char('j'), Intent::NavDown),
        (KeyCode::Right, Intent::Expand),
        (KeyCode::Char('l'), Intent::Expand),
        (KeyCode::Left, Intent::Collapse),
        (KeyCode::Char('h'), Intent::Collapse),
        (KeyCode::Enter, Intent::Activate),
        (KeyCode::Char('Z'), Intent::OpenFullscreen),
        (KeyCode::Char('i'), Intent::ToggleIgnore),
        (KeyCode::Char('.'), Intent::ToggleHidden),
        (KeyCode::Char('c'), Intent::ToggleChangedOnly),
        (KeyCode::Char('b'), Intent::ToggleBaseline),
        (KeyCode::Char('D'), Intent::ToggleDeltaRaw),
        (KeyCode::Char('v'), Intent::CycleView),
        (KeyCode::Char('e'), Intent::OpenInEditor),
        (KeyCode::Char('f'), Intent::OpenFinder),
        (KeyCode::Char(':'), Intent::OpenGoToLine),
        (KeyCode::Char('/'), Intent::OpenSearch),
        (KeyCode::Char('n'), Intent::NextMatch),
        (KeyCode::Char('N'), Intent::PrevMatch),
        (KeyCode::Char('H'), Intent::TreeScrollLeft),
        (KeyCode::Char('L'), Intent::TreeScrollRight),
        (KeyCode::Char('O'), Intent::OpenWithApp),
        (KeyCode::Char('R'), Intent::RevealInFileManager),
        (KeyCode::Char('y'), Intent::CopyRepoPath),
        (KeyCode::Char('Y'), Intent::CopyAbsPath),
        (KeyCode::Char('W'), Intent::SwitchWorktree),
        (KeyCode::Tab, Intent::ToggleFocus),
        (KeyCode::Char('<'), Intent::ShrinkTree),
        (KeyCode::Char('>'), Intent::GrowTree),
        (KeyCode::Char('w'), Intent::ToggleWrap),
        (KeyCode::Char('z'), Intent::ToggleZoom),
        (KeyCode::Char('r'), Intent::Refresh),
        (KeyCode::Char('u'), Intent::DismissUpdate),
        (KeyCode::Char('?'), Intent::ShowHelp),
        (KeyCode::Char('q'), Intent::Close),
        (KeyCode::Esc, Intent::Close),
    ];

    #[test]
    fn registry_covers_every_intent_exactly_once_with_nonempty_keys() {
        // AC-2: every global action (Intent::ALL member) appears in the registry exactly once
        // with a non-empty default key set, so nothing is unreachable by default.
        let intents: HashSet<Intent> = registry().iter().map(|b| b.intent).collect();
        let all: HashSet<Intent> = Intent::ALL.iter().copied().collect();
        assert_eq!(intents, all, "REGISTRY must cover exactly Intent::ALL");
        assert_eq!(
            registry().len(),
            Intent::ALL.len(),
            "REGISTRY must have one row per Intent::ALL member (no duplicates)"
        );
        for b in registry() {
            assert!(
                !b.default_keys.is_empty(),
                "{:?} ({}) must have >=1 default key",
                b.intent,
                b.name
            );
        }
    }

    #[test]
    fn registry_names_are_unique() {
        // AC-5: each global action carries a unique snake_case intent name (these become the
        // public `[keys]` config keys, so a clash would make one intent unaddressable).
        let names: HashSet<&str> = registry().iter().map(|b| b.name).collect();
        assert_eq!(
            names.len(),
            registry().len(),
            "no two REGISTRY rows may share a name"
        );
    }

    #[test]
    fn registry_every_row_has_a_known_category_and_every_category_is_used() {
        // AC-19 (grouping): every registry row's category is a member of CATEGORY_ORDER, so the
        // Keybindings overlay can iterate categories in order and lose no action; and every declared
        // category carries at least one action, so no empty group header renders.
        let known: HashSet<&str> = CATEGORY_ORDER.iter().copied().collect();
        for b in registry() {
            assert!(
                known.contains(b.category),
                "'{}' has category '{}' which is not in CATEGORY_ORDER",
                b.name,
                b.category
            );
        }
        for cat in CATEGORY_ORDER {
            assert!(
                registry().iter().any(|b| b.category == *cat),
                "category '{cat}' has no actions (would render an empty group header)"
            );
        }
    }

    #[test]
    fn registry_default_keys_are_collision_free() {
        // AC-3: no bindable key is in the default key set of two different actions.
        let all_keys: Vec<KeyCode> = registry()
            .iter()
            .flat_map(|b| b.default_keys.iter().copied())
            .collect();
        let unique: HashSet<KeyCode> = all_keys.iter().copied().collect();
        assert_eq!(
            unique.len(),
            all_keys.len(),
            "REGISTRY default keys must be collision-free (no key in two rows)"
        );
    }

    #[test]
    fn bound_keys_map_to_their_intents() {
        for &(code, want) in BINDINGS {
            assert_eq!(
                map_key(k(code)),
                Some(want),
                "{code:?} should map to {want:?}"
            );
        }
    }

    #[test]
    fn every_intent_has_at_least_one_key() {
        // AC-18: keyboard-complete — no viewer function is unreachable from the keyboard.
        let reachable: HashSet<Intent> = BINDINGS
            .iter()
            .filter_map(|&(c, _)| map_key(k(c)))
            .collect();
        for intent in Intent::ALL {
            assert!(
                reachable.contains(&intent),
                "{intent:?} has no bound key (AC-18)"
            );
        }
    }

    #[test]
    fn unmapped_keys_are_a_noop() {
        assert_eq!(map_key(k(KeyCode::Char('g'))), None);
        assert_eq!(map_key(k(KeyCode::Char('x'))), None);
        assert_eq!(map_key(k(KeyCode::F(1))), None);
        assert_eq!(map_key(k(KeyCode::Backspace)), None);
    }

    #[test]
    fn parse_key_spec_accepts_every_bindable_class() {
        // AC-11: a representative of every bindable-key class parses to its KeyCode.
        // Printable single chars are case-SENSITIVE (a shifted char is its own key).
        assert_eq!(parse_key_spec("g"), Some(KeyCode::Char('g')));
        assert_eq!(parse_key_spec("V"), Some(KeyCode::Char('V')));
        assert_eq!(parse_key_spec("<"), Some(KeyCode::Char('<')));
        assert_eq!(parse_key_spec("?"), Some(KeyCode::Char('?')));
        // Named keys are matched case-INSENSITIVELY ("Tab" and "tab" both parse).
        assert_eq!(parse_key_spec("Tab"), Some(KeyCode::Tab));
        assert_eq!(parse_key_spec("tab"), Some(KeyCode::Tab));
        assert_eq!(parse_key_spec("Enter"), Some(KeyCode::Enter));
        assert_eq!(parse_key_spec("Esc"), Some(KeyCode::Esc));
        assert_eq!(parse_key_spec("Up"), Some(KeyCode::Up));
        assert_eq!(parse_key_spec("Down"), Some(KeyCode::Down));
        assert_eq!(parse_key_spec("Left"), Some(KeyCode::Left));
        assert_eq!(parse_key_spec("Right"), Some(KeyCode::Right));
        assert_eq!(parse_key_spec("Home"), Some(KeyCode::Home));
        assert_eq!(parse_key_spec("End"), Some(KeyCode::End));
        assert_eq!(parse_key_spec("PageUp"), Some(KeyCode::PageUp));
        assert_eq!(parse_key_spec("PageDown"), Some(KeyCode::PageDown));
        assert_eq!(parse_key_spec("Space"), Some(KeyCode::Char(' ')));
        assert_eq!(parse_key_spec("Backspace"), Some(KeyCode::Backspace));
        assert_eq!(parse_key_spec("Delete"), Some(KeyCode::Delete));
        assert_eq!(parse_key_spec("Insert"), Some(KeyCode::Insert));
        assert_eq!(parse_key_spec("F1"), Some(KeyCode::F(1)));
        assert_eq!(parse_key_spec("F5"), Some(KeyCode::F(5)));
        assert_eq!(parse_key_spec("F12"), Some(KeyCode::F(12)));
    }

    #[test]
    fn parse_key_spec_rejects_chords_and_garbage() {
        // AC-12 / NC-1: a modifier chord or an otherwise-unparseable token is rejected, so no
        // Ctrl/Alt binding is ever produced and garbage falls through to the default key set.
        assert_eq!(parse_key_spec("Ctrl+r"), None, "Ctrl chord is not bindable");
        assert_eq!(parse_key_spec("Alt+x"), None, "Alt chord is not bindable");
        assert_eq!(parse_key_spec(""), None, "empty token");
        assert_eq!(parse_key_spec("abc"), None, "multi-char garbage");
        assert_eq!(parse_key_spec("F13"), None, "F-key above range");
        assert_eq!(parse_key_spec("F0"), None, "F-key below range");
        assert_eq!(parse_key_spec("PgDn"), None, "unknown named key");
    }

    #[test]
    fn modified_char_keys_do_not_trigger_intents() {
        // Ctrl+C is the terminal interrupt, not "changed-only"; Alt-chords are unbound too.
        // Accidental or reserved chords must not fire a viewer action.
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn shift_is_allowed_for_shifted_characters() {
        // '<' / '>' are typed with Shift; the resize keys must fire whether or not the
        // terminal reports the Shift bit — but a Ctrl chord on the same key must not.
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('<'), KeyModifiers::SHIFT)),
            Some(Intent::ShrinkTree)
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('>'), KeyModifiers::NONE)),
            Some(Intent::GrowTree)
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('<'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn shift_w_maps_to_switch_worktree_and_lowercase_w_stays_toggle_wrap() {
        // `W` (Shift+w, Char('W')) summons the worktree picker (AC-5/AC-N5).
        // `w` (ToggleWrap) must be unaffected — no collision.
        // A Ctrl chord on `W` must fire nothing.
        assert_eq!(map_key(k(KeyCode::Char('W'))), Some(Intent::SwitchWorktree));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('W'), KeyModifiers::SHIFT)),
            Some(Intent::SwitchWorktree)
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('W'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(map_key(k(KeyCode::Char('w'))), Some(Intent::ToggleWrap));
    }

    #[test]
    fn f_maps_to_open_finder_and_modifier_chords_are_inert() {
        // `f` opens the go-to-file finder (AC-1, AC-N6). Ctrl-f / Alt-f must not fire an intent
        // (reserved terminal chords / alt-text-entry paths must stay clear).
        assert_eq!(map_key(k(KeyCode::Char('f'))), Some(Intent::OpenFinder));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT)),
            None
        );
    }

    #[test]
    fn question_mark_maps_to_show_help_and_modifier_chords_are_inert() {
        // `?` (Shift+/) opens the help overlay (AC-1, AC-N4). Ctrl-? / Alt-? must not fire an
        // intent. `?` is a shifted character — SHIFT bit set must still map.
        assert_eq!(
            map_key(k(KeyCode::Char('?'))),
            Some(Intent::ShowHelp),
            "'?' must map to ShowHelp"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT)),
            Some(Intent::ShowHelp),
            "'?' with SHIFT must still map to ShowHelp"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-? must not fire an intent"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::ALT)),
            None,
            "Alt-? must not fire an intent"
        );
    }

    #[test]
    fn colon_maps_to_open_go_to_line_and_modifier_chords_are_inert() {
        // `:` opens the go-to-line prompt (AC-1, AC-N6). Ctrl-: / Alt-: must not fire an intent.
        // (Shift is allowed — `:` is a shifted char on many layouts — so do not assert Shift is None.)
        assert_eq!(map_key(k(KeyCode::Char(':'))), Some(Intent::OpenGoToLine));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::ALT)),
            None
        );
    }

    #[test]
    fn search_keys_map_correctly_and_modifier_chords_are_inert() {
        // AC-8, AC-N6: `/` → OpenSearch, `n` → NextMatch, `N` → PrevMatch.
        // Ctrl/Alt chords on these keys must NOT fire an intent (AC-N6).
        // `N` is a shifted character (Char('N') with SHIFT) — must still map.
        assert_eq!(map_key(k(KeyCode::Char('/'))), Some(Intent::OpenSearch));
        assert_eq!(map_key(k(KeyCode::Char('n'))), Some(Intent::NextMatch));
        assert_eq!(map_key(k(KeyCode::Char('N'))), Some(Intent::PrevMatch));

        // `N` with SHIFT bit set (as some terminals report it) still maps:
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT)),
            Some(Intent::PrevMatch)
        );

        // Ctrl/Alt chords are inert (AC-N6):
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::ALT)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT)),
            None
        );
    }

    #[test]
    fn copy_path_keys_are_distinct_and_shift_capital_y_is_the_absolute_path() {
        // `y` copies the repo-relative path; `Y` (Shift+y, reported as `Char('Y')` with the
        // Shift bit set) copies the absolute path. A Ctrl chord on the same key fires neither.
        assert_eq!(map_key(k(KeyCode::Char('y'))), Some(Intent::CopyRepoPath));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT)),
            Some(Intent::CopyAbsPath)
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn shift_h_l_map_to_tree_horizontal_scroll_and_ctrl_chords_are_inert() {
        // `H` (Shift+h) and `L` (Shift+l) scroll the tree pane left/right (AC-18 — the tree's
        // h-scroll was mouse-only). The lowercase `h`/`l` stay Collapse/Expand; no collision.
        // A Ctrl chord on the same key fires nothing.
        assert_eq!(map_key(k(KeyCode::Char('H'))), Some(Intent::TreeScrollLeft));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT)),
            Some(Intent::TreeScrollLeft),
            "H with SHIFT bit set still maps to TreeScrollLeft"
        );
        assert_eq!(
            map_key(k(KeyCode::Char('L'))),
            Some(Intent::TreeScrollRight)
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT)),
            Some(Intent::TreeScrollRight),
            "L with SHIFT bit set still maps to TreeScrollRight"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-H must not fire an intent"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-L must not fire an intent"
        );
        // Lowercase h/l stay Collapse/Expand (no collision).
        assert_eq!(map_key(k(KeyCode::Char('h'))), Some(Intent::Collapse));
        assert_eq!(map_key(k(KeyCode::Char('l'))), Some(Intent::Expand));
    }

    #[test]
    fn shift_z_maps_to_open_fullscreen_and_lowercase_z_stays_toggle_zoom() {
        // `Z` (Shift+`z`, reported as `Char('Z')` with or without the Shift bit) opens the
        // selected file full-screen (in-plugin zoom + herdr pane zoom). Lowercase `z` stays the
        // in-plugin-only zoom toggle — no collision. A Ctrl chord on `Z` must fire nothing.
        assert_eq!(map_key(k(KeyCode::Char('Z'))), Some(Intent::OpenFullscreen));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('Z'), KeyModifiers::SHIFT)),
            Some(Intent::OpenFullscreen),
            "Z with the SHIFT bit set still maps to OpenFullscreen"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('Z'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-Z must not fire an intent"
        );
        assert_eq!(map_key(k(KeyCode::Char('z'))), Some(Intent::ToggleZoom));
    }

    #[test]
    fn shift_o_r_map_to_open_and_reveal_and_lowercase_o_is_unbound() {
        // `O` (Shift+o) opens the selected entry with the OS default app; `R` (Shift+r) reveals
        // it in the OS file manager. Lowercase `o` stays unbound (`r` is Refresh — untouched).
        // A Ctrl chord on either capital key must fire nothing.
        assert_eq!(map_key(k(KeyCode::Char('O'))), Some(Intent::OpenWithApp));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('O'), KeyModifiers::SHIFT)),
            Some(Intent::OpenWithApp),
            "O with SHIFT bit set still maps to OpenWithApp"
        );
        assert_eq!(
            map_key(k(KeyCode::Char('R'))),
            Some(Intent::RevealInFileManager)
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT)),
            Some(Intent::RevealInFileManager),
            "R with SHIFT bit set still maps to RevealInFileManager"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('O'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-O must not fire an intent"
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-R must not fire an intent"
        );
        // Lowercase `o` stays unbound; `r` stays Refresh (no collision).
        assert_eq!(map_key(k(KeyCode::Char('o'))), None);
        assert_eq!(map_key(k(KeyCode::Char('r'))), Some(Intent::Refresh));
    }

    #[test]
    fn decode_rejects_control_chords_but_shift_and_none_still_decode() {
        // AC-4: the pure decode path yields no intent for a control chord (Ctrl/Alt) on a bound
        // key, while the same key with Shift-only or no modifier still decodes to its intent.
        let bindings = default_bindings();

        // Bound keys carrying a control chord decode to nothing.
        assert_eq!(
            decode(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
                &bindings
            ),
            None,
            "Ctrl+r must not decode"
        );
        assert_eq!(
            decode(
                KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT),
                &bindings
            ),
            None,
            "Alt+e must not decode"
        );
        assert_eq!(
            decode(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                &bindings
            ),
            None,
            "Ctrl+q must not decode"
        );

        // The same keys with no modifier still decode to their intents.
        assert_eq!(
            decode(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
                &bindings
            ),
            Some(Intent::Refresh)
        );
        assert_eq!(
            decode(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                &bindings
            ),
            Some(Intent::Close)
        );
        // A shifted character (Shift-only) is ordinary typing, not a control chord — it decodes.
        assert_eq!(
            decode(
                KeyEvent::new(KeyCode::Char('<'), KeyModifiers::SHIFT),
                &bindings
            ),
            Some(Intent::ShrinkTree)
        );
    }

    #[test]
    fn decode_rejects_control_chords_on_a_customized_binding_ac4() {
        // AC-4 names default AND custom: a control chord must not fire an intent even when the key
        // came from a `[keys]` remap. The earlier test only exercised `default_bindings()`; this
        // pins the custom clause by resolving `refresh = "g"` and asserting Ctrl/Alt + g decode to
        // nothing while the plain custom key still decodes to its remapped intent.
        let (bindings, out) = resolve_with(&[("refresh", one("g"))]);
        assert!(out.is_empty(), "refresh = \"g\" is a valid remap");

        assert_eq!(
            decode(
                KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
                &bindings
            ),
            None,
            "Ctrl + a custom-bound key must not decode"
        );
        assert_eq!(
            decode(
                KeyEvent::new(KeyCode::Char('g'), KeyModifiers::ALT),
                &bindings
            ),
            None,
            "Alt + a custom-bound key must not decode"
        );
        assert_eq!(
            decode(
                KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
                &bindings
            ),
            Some(Intent::Refresh),
            "the custom key with no modifier decodes to its remapped intent"
        );
    }

    #[test]
    fn reject_reason_display_carries_key_label_and_token() {
        // The `RejectReason` Display strings feed the help overlay's ignored-bindings status line
        // (AC-16). `UnknownIntent` is exercised via `keybindings_text_surfaces_ignored_bindings_ac16`;
        // the two data-carrying variants embed a key label / token that must not silently blank out.
        assert_eq!(
            RejectReason::UnknownIntent.to_string(),
            "unknown intent name"
        );
        assert_eq!(
            RejectReason::BadKeySpec("Ctrl+x".to_string()).to_string(),
            "unbindable key \"Ctrl+x\""
        );
        assert_eq!(
            RejectReason::DuplicateKey("g".to_string()).to_string(),
            "duplicate key \"g\""
        );
    }

    #[test]
    fn decode_over_default_bindings_agrees_with_map_key() {
        // Belt-and-suspenders AC-1: the pure decode path over the default bindings returns exactly
        // what map_key returns, across bound, arrow, shifted, chorded, and unbound representatives.
        let bindings = default_bindings();
        let cases = [
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('W'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
        ];
        for key in cases {
            assert_eq!(decode(key, &bindings), map_key(key), "{key:?}");
        }
    }

    // --- T-5 Bindings Resolver (AC-6, AC-7, AC-8, AC-10, AC-14, AC-15, AC-18, AC-20) ---

    use crate::config::KeySpec;
    use std::collections::BTreeMap;

    /// A single-key `[keys]` spec.
    fn one(s: &str) -> KeySpec {
        KeySpec::One(s.to_string())
    }

    /// A multi-key `[keys]` spec.
    fn many(v: &[&str]) -> KeySpec {
        KeySpec::Many(v.iter().map(|s| s.to_string()).collect())
    }

    /// Resolve the registry against a `[keys]` table built from `(name, spec)` pairs.
    fn resolve_with(pairs: &[(&str, KeySpec)]) -> (EffectiveBindings, KeyLoadOutcome) {
        let map: BTreeMap<String, KeySpec> = pairs
            .iter()
            .map(|(n, s)| (n.to_string(), s.clone()))
            .collect();
        resolve_bindings(registry(), Some(&map))
    }

    /// Decode a bare key (no modifier) against resolved bindings.
    fn dec(b: &EffectiveBindings, code: KeyCode) -> Option<Intent> {
        decode(KeyEvent::new(code, KeyModifiers::NONE), b)
    }

    #[test]
    fn resolve_valid_entry_rebinds_key_ac6() {
        // AC-6: a valid entry makes exactly the key its spec names decode to that action.
        let (b, out) = resolve_with(&[("refresh", one("g"))]);
        assert!(out.is_empty(), "a valid entry produces no rejects");
        assert_eq!(dec(&b, KeyCode::Char('g')), Some(Intent::Refresh));
    }

    #[test]
    fn resolve_replaces_default_key_ac7() {
        // AC-7: replace-semantics — a default the spec does not re-list stops decoding.
        let (b, _) = resolve_with(&[("refresh", one("g"))]);
        assert_eq!(dec(&b, KeyCode::Char('g')), Some(Intent::Refresh));
        assert_eq!(
            dec(&b, KeyCode::Char('r')),
            None,
            "the displaced default 'r' no longer decodes to Refresh"
        );
    }

    #[test]
    fn resolve_unlisted_intent_keeps_defaults_ac8() {
        // AC-8: an intent with no `[keys]` entry keeps its full default key set.
        let (b, _) = resolve_with(&[("refresh", one("g"))]);
        assert_eq!(dec(&b, KeyCode::Char('j')), Some(Intent::NavDown));
        assert_eq!(dec(&b, KeyCode::Down), Some(Intent::NavDown));
    }

    #[test]
    fn resolve_array_spec_binds_all_and_replaces_ac9_ac7() {
        // AC-9 + replace-semantics: an array binds every listed key; the un-relisted default drops.
        let (b, out) = resolve_with(&[("nav_up", many(&["w", "Up"]))]);
        assert!(out.is_empty());
        assert_eq!(dec(&b, KeyCode::Char('w')), Some(Intent::NavUp));
        assert_eq!(dec(&b, KeyCode::Up), Some(Intent::NavUp));
        assert_eq!(
            dec(&b, KeyCode::Char('k')),
            None,
            "nav_up's default 'k' is not re-listed, so it drops"
        );
    }

    #[test]
    fn resolve_explicit_beats_default_ac10() {
        // AC-10: binding refresh to 'j' (nav_down's default) routes 'j' to Refresh; nav_down keeps
        // its other default (Down) but loses 'j'.
        let (b, _) = resolve_with(&[("refresh", one("j"))]);
        assert_eq!(dec(&b, KeyCode::Char('j')), Some(Intent::Refresh));
        assert_ne!(dec(&b, KeyCode::Char('j')), Some(Intent::NavDown));
        assert_eq!(dec(&b, KeyCode::Down), Some(Intent::NavDown));
    }

    #[test]
    fn resolve_unknown_intent_rejected_siblings_apply_ac14() {
        // AC-14: an unknown name is rejected while a sibling valid entry still applies; no panic.
        let (b, out) = resolve_with(&[("bogus", one("g")), ("refresh", one("p"))]);
        assert!(
            out.rejected
                .iter()
                .any(|r| r.name == "bogus" && r.reason == RejectReason::UnknownIntent),
            "the unknown name is rejected as UnknownIntent"
        );
        assert_eq!(dec(&b, KeyCode::Char('p')), Some(Intent::Refresh));
    }

    #[test]
    fn resolve_duplicate_key_clash_rejects_both_ac15() {
        // AC-15: two entries claiming one key both revert to their defaults.
        let (b, out) = resolve_with(&[("refresh", one("g")), ("open_finder", one("g"))]);
        assert_eq!(out.rejected.len(), 2, "both clashing entries are rejected");
        assert_eq!(dec(&b, KeyCode::Char('r')), Some(Intent::Refresh));
        assert_eq!(dec(&b, KeyCode::Char('f')), Some(Intent::OpenFinder));
        assert_eq!(
            dec(&b, KeyCode::Char('g')),
            None,
            "the clashed key 'g' decodes to nothing"
        );
    }

    #[test]
    fn resolve_bad_key_token_rejects_whole_entry_ac12() {
        // AC-12: any unparseable token in a spec rejects the whole entry (the intent keeps defaults).
        let (b, out) = resolve_with(&[("refresh", many(&["g", "Ctrl+x"]))]);
        assert!(
            out.rejected
                .iter()
                .any(|r| r.name == "refresh" && matches!(r.reason, RejectReason::BadKeySpec(_))),
        );
        assert_eq!(
            dec(&b, KeyCode::Char('g')),
            None,
            "the whole entry is rejected, so its parseable key 'g' is not bound"
        );
        assert_eq!(dec(&b, KeyCode::Char('r')), Some(Intent::Refresh));
    }

    #[test]
    fn resolve_empty_array_spec_rejected_ac12() {
        // AC-12: a spec that lists no key is rejected so the intent is not stranded keyless.
        let (b, out) = resolve_with(&[("refresh", many(&[]))]);
        assert!(
            out.rejected
                .iter()
                .any(|r| r.name == "refresh" && matches!(r.reason, RejectReason::BadKeySpec(_))),
        );
        assert_eq!(
            dec(&b, KeyCode::Char('r')),
            Some(Intent::Refresh),
            "refresh keeps its default"
        );
    }

    #[test]
    fn resolve_esc_floor_holds_across_configs_ac18() {
        // AC-18(a): with no `[keys]`, or omitting `close`, Esc still closes.
        let (b_none, _) = resolve_bindings(registry(), None);
        assert_eq!(dec(&b_none, KeyCode::Esc), Some(Intent::Close));
        let (b_omit, _) = resolve_with(&[("refresh", one("g"))]);
        assert_eq!(dec(&b_omit, KeyCode::Esc), Some(Intent::Close));

        // AC-18(b): rebinding `close` to 'x' keeps 'x' AND Esc closing; the dropped 'q' decodes to
        // nothing.
        let (b_rebound, _) = resolve_with(&[("close", one("x"))]);
        assert_eq!(dec(&b_rebound, KeyCode::Char('x')), Some(Intent::Close));
        assert_eq!(dec(&b_rebound, KeyCode::Esc), Some(Intent::Close));
        assert_eq!(dec(&b_rebound, KeyCode::Char('q')), None);

        // AC-18(c): the Esc floor beats an explicit Esc claim by another intent (not a rejection).
        let (b_claim, out) = resolve_with(&[("refresh", one("Esc"))]);
        assert!(out.is_empty(), "naming Esc is valid, not rejected");
        assert_eq!(dec(&b_claim, KeyCode::Esc), Some(Intent::Close));
    }

    #[test]
    fn resolve_marks_customized_ac20() {
        // AC-20: an intent set by a valid entry is marked customized; an unlisted one is not.
        let (b, _) = resolve_with(&[("refresh", one("g"))]);
        assert!(b.is_customized(Intent::Refresh));
        assert!(!b.is_customized(Intent::NavDown));
    }

    // --- T-8 docs consistency: the keys reference `## Keys` table stays in sync with the registry (AC-21) ---

    /// The keys-reference source (`docs/keys.md`), compiled in so a drift between the registry and
    /// the `## Keys` table fails the build. This assertion lives here, not in
    /// `tests/docs_consistency.rs`, because it reads the `pub(crate)` [`registry`] / [`key_label`],
    /// which an integration test cannot see.
    const KEYS_DOC: &str = include_str!("../docs/keys.md");

    #[test]
    fn keys_doc_table_documents_every_registry_action_ac21() {
        // AC-21: the `docs/keys.md` `## Keys` table must document every global action in the
        // registry so the table can never silently drift from the bindings. Scope the check to the
        // `## Keys` section (its heading to the next `## ` heading) so a stray backtick elsewhere in
        // the doc cannot satisfy it.
        let start = KEYS_DOC
            .find("## Keys")
            .expect("docs/keys.md must carry a `## Keys` section");
        let rest = &KEYS_DOC[start + "## Keys".len()..];
        let end = rest.find("\n## ").unwrap_or(rest.len());
        let keys_section = &rest[..end];

        // Per action, require AT LEAST ONE of its default keys to appear as a backtick-wrapped
        // label (e.g. `` `k` ``). Rationale: an action with an arrow default (nav_up = [Up, k]) is
        // written in the table with the glyph `↑`, not the word `Up`, so a literal `Up` check
        // would fail spuriously; but every such action ALSO carries a letter key (`k`) whose
        // backtick form IS in the table. A per-action "at least one key documented" check is
        // therefore robust to the glyph-vs-name spelling AND still catches a NEW registry action
        // that has no README row at all (none of its keys would appear).
        for binding in registry() {
            let documented = binding
                .default_keys
                .iter()
                .any(|&code| keys_section.contains(&format!("`{}`", key_label(code))));
            assert!(
                documented,
                "the docs/keys.md `## Keys` table documents no key for `{}` (intent {:?}); expected a \
                 backtick-wrapped label for one of {:?}",
                binding.name,
                binding.intent,
                binding
                    .default_keys
                    .iter()
                    .map(|&c| key_label(c))
                    .collect::<Vec<_>>(),
            );
        }
    }

    /// The configuration-reference source (`docs/configuration.md`), compiled in so the
    /// remappable-actions table can't drift from the registry. Every `[keys]` intent name is a
    /// stable config identifier, so the doc must list them all.
    const CONFIG_DOC: &str = include_str!("../docs/configuration.md");

    #[test]
    fn configuration_doc_lists_every_remappable_intent() {
        // Every registry intent name must appear (backtick-wrapped) in docs/configuration.md's
        // "Every remappable action" table, so the full `[keys]` surface is documented and can't
        // drift: adding a new Intent to the registry without listing its name fails the build.
        // Backtick-wrapping avoids matching a bare word that also appears in prose.
        for binding in registry() {
            let needle = format!("`{}`", binding.name);
            assert!(
                CONFIG_DOC.contains(&needle),
                "docs/configuration.md must list the remappable intent `{}` (backtick-wrapped) in \
                 the keybindings table",
                binding.name,
            );
        }
    }
}
