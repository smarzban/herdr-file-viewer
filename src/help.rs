//! Help content source and overlay state for the in-app help overlay.
//!
//! Content source: embedded changelog and about text, no I/O, no side effects.
//! Overlay state: `HelpSectionState` and `HelpState` — pure in-memory navigation.

/// The full `CHANGELOG.md`, embedded at compile time (AC-12, AC-13).
pub const CHANGELOG_MD: &str = include_str!("../CHANGELOG.md");

/// The What's New body source: `CHANGELOG_MD` with the file-meta preamble stripped.
///
/// The raw `CHANGELOG.md` opens with the `# Changelog` title, a "Keep a Changelog / Semantic
/// Versioning" paragraph, and link references — file metadata an in-app "What's New" doesn't want.
/// This returns the slice starting at the first `## [` version heading, so only the version
/// sections (`## [..]` + their `### Added`/`### Fixed` entries) render. Falls back to the whole
/// string if no version heading is found (the const stays whole; the newest-first test reads it).
pub fn changelog_display() -> &'static str {
    match CHANGELOG_MD.find("## [") {
        Some(idx) => &CHANGELOG_MD[idx..],
        None => CHANGELOG_MD,
    }
}

/// The built-in, fixed sections of the help overlay: What's New and About.
///
/// This enum is intentionally closed at these two variants — it is NOT the full inventory of
/// sections the overlay can show. `HelpState.sections` (below) is a generic `Vec<HelpSectionState>`
/// precisely so later features can append further sections without touching this enum; the
/// settings-config feature (SMA-49) does exactly that, appending a display-only "Settings" section
/// at runtime as a raw `HelpSectionState` (see `open_help` / `set_settings_display`) rather than
/// adding a `Settings` variant here. Do not "fix" a 3-tab runtime overlay by adding a variant to
/// this enum — the enum staying two-variant is the intended seam, not a bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpSection {
    WhatsNew,
    About,
}

impl HelpSection {
    /// Human-readable tab label for the section.
    pub fn label(self) -> &'static str {
        match self {
            HelpSection::WhatsNew => "What's New",
            HelpSection::About => "About",
        }
    }
}

// ---------------------------------------------------------------------------
// Help Overlay State
// ---------------------------------------------------------------------------

/// State for a single tab in the help overlay: its label, body text, and scroll offset.
///
/// Body is `ratatui::text::Text<'static>` so it can be produced once and held cheaply
/// without re-allocating across frames. `scroll` is the top-visible line (0-based).
pub struct HelpSectionState {
    pub label: &'static str,
    pub body: ratatui::text::Text<'static>,
    pub scroll: u16,
}

/// Active state for the help overlay: an ordered list of sections and the active index.
///
/// Sections are a generic `Vec` (the seam for future settings additions) — no hard-coded pair.
/// SHORTCUT: linear scan over sections — fine for ≤20 sections; index if the list can grow large.
pub struct HelpState {
    pub sections: Vec<HelpSectionState>,
    active: usize,
}

impl HelpState {
    /// Create a new `HelpState` with the given sections; `active` starts at 0.
    pub fn new(sections: Vec<HelpSectionState>) -> Self {
        Self {
            sections,
            active: 0,
        }
    }

    /// The index of the currently-active section.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The label of each section, in order.
    pub fn section_labels(&self) -> Vec<&'static str> {
        self.sections.iter().map(|s| s.label).collect()
    }

    /// Advance to the next section, wrapping from the last to the first (AC-7).
    pub fn next(&mut self) {
        if self.sections.is_empty() {
            return;
        }
        self.active = (self.active + 1) % self.sections.len();
    }

    /// Move to the previous section, wrapping from the first to the last (AC-7).
    pub fn prev(&mut self) {
        if self.sections.is_empty() {
            return;
        }
        self.active = self
            .active
            .checked_sub(1)
            .unwrap_or(self.sections.len() - 1);
    }

    /// Select a section by index. Out-of-range indices are silently ignored (no panic).
    pub fn select(&mut self, idx: usize) {
        if idx < self.sections.len() {
            self.active = idx;
        }
    }

    /// The body text of the currently-active section.
    pub fn active_body(&self) -> &ratatui::text::Text<'static> {
        &self.sections[self.active].body
    }

    /// Scroll the active section by `delta` lines (positive = down, negative = up).
    ///
    /// Saturates at 0 (no underflow). The upper bound is enforced separately by
    /// `clamp_scroll` once the presenter knows the viewport height (AC-8, AC-9).
    pub fn scroll_by(&mut self, delta: i32) {
        if self.sections.is_empty() {
            return;
        }
        let s = &mut self.sections[self.active];
        if delta >= 0 {
            s.scroll = s.scroll.saturating_add(delta as u16);
        } else {
            s.scroll = s.scroll.saturating_sub((-delta) as u16);
        }
    }

    /// Pin the active section's scroll to `[0, total_rows − viewport_height]`.
    ///
    /// Called each frame after the presenter has measured the visible viewport height *and* the
    /// body's total row count, so over-shoots from `scroll_by` are resolved against the real
    /// content size (AC-9). The caller decides what `total_rows` means: the body is drawn with
    /// `Paragraph::wrap`, so the scroll offset is in **wrapped (rendered) rows**, not raw lines —
    /// the Presenter therefore passes the body's WRAPPED row count at the draw width (mirroring how
    /// the content pane clamps against `rendered_line_count_for`). Tests over non-wrapping bodies
    /// pass the raw `body.lines.len()`, which is correct because wrapped == raw there.
    pub fn clamp_scroll(&mut self, total_rows: u16, viewport_height: u16) {
        if self.sections.is_empty() {
            return;
        }
        let s = &mut self.sections[self.active];
        let max_scroll = total_rows.saturating_sub(viewport_height);
        s.scroll = s.scroll.min(max_scroll);
    }
}

// ---------------------------------------------------------------------------

/// Assemble the "About" pane text. The Presenter center-aligns this section (AC-17).
///
/// Lines, in order:
/// 1. `Herdr File Viewer` (the display title, alone — the nice form, not the raw package name)
/// 2. package description
/// 3. *(blank)*
/// 4. bare repo host+path (the `https://` scheme + any `Repository:` label stripped)
/// 5. *(blank)*
/// 6. `vX.Y.Z · <status>` (version + update status: `Up to date` or `Update available: vX.Y.Z`)
/// 7. `<SPDX> License`
/// 8. *(blank)*
/// 9. GitHub-star call-to-action — the closing line (a plain `★`, U+2605, not the `⭐️` emoji,
///    whose double-width mis-renders in the TUI)
///
/// (AC-16, AC-17, AC-18, AC-19)
pub fn about_text(update: Option<crate::update::version::Version>) -> String {
    let status = match update {
        Some(v) => format!("Update available: v{v}"),
        None => "Up to date".to_owned(),
    };
    // Bare host+path: strip the URL scheme so it reads as a plain repo handle, no label.
    let repository = env!("CARGO_PKG_REPOSITORY")
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    format!(
        "{title}\n\
         {description}\n\
         \n\
         {repository}\n\
         \n\
         v{version} · {status}\n\
         {license} License\n\
         \n\
         {star_cta}",
        // The display title (the nice form) — NOT the raw `CARGO_PKG_NAME` (`herdr-file-viewer`),
        // which still appears verbatim in the bare repo URL below.
        title = "Herdr File Viewer",
        version = env!("CARGO_PKG_VERSION"),
        description = env!("CARGO_PKG_DESCRIPTION"),
        repository = repository,
        status = status,
        license = env!("CARGO_PKG_LICENSE"),
        star_cta = "If you enjoy the file viewer, don't forget to give it a ★ on GitHub!",
    )
}

/// Assemble the "Settings" pane text (AC-15, AC-18): a first line reflecting the config
/// [`crate::config::LoadOutcome`], then one row per effective setting. Pure — no env/FS; both
/// arguments are already-resolved values the caller computed once at startup (`app::run`).
///
/// `None` renderer/editor fields print as `(default)`; `hide_dotfiles`/`update_check` print as
/// `true`/`false` and `on`/`off` respectively. Formatting is kept simple and stable since this
/// feeds a presenter snapshot.
pub fn settings_text(
    eff: &crate::config::EffectiveSettings,
    outcome: &crate::config::LoadOutcome,
    config_path: &std::path::Path,
) -> String {
    let status_line = match outcome {
        crate::config::LoadOutcome::Loaded => "Config: loaded.".to_owned(),
        crate::config::LoadOutcome::Absent => "Config: no file found, using defaults.".to_owned(),
        crate::config::LoadOutcome::Malformed(reason) => {
            // `reason` is the toml crate's error, which is MULTI-LINE (it appends a source
            // snippet and a `^` caret pointer). Keep only its first line so the status stays a
            // single readable line instead of spilling the caret art across the overlay rows.
            let summary = reason.lines().next().unwrap_or("parse error").trim();
            format!("Config: invalid, using defaults ({summary}).")
        }
    };
    // Always show where the file is (or would be) so the user knows what to fix or create.
    let location_line = format!("Location: {}", config_path.display());

    let opt_os = |v: &Option<std::ffi::OsString>| -> String {
        v.as_ref()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "(default)".to_owned())
    };
    let opt_argv = |v: &Option<Vec<String>>| -> String {
        v.as_ref()
            .map(|argv| argv.join(" "))
            .unwrap_or_else(|| "(default)".to_owned())
    };

    format!(
        "{status_line}\n\
         {location_line}\n\
         editor          = {editor}\n\
         markdown        = {markdown}\n\
         diff            = {diff}\n\
         syntax          = {syntax}\n\
         open            = {open}\n\
         reveal          = {reveal}\n\
         hide_dotfiles   = {hide_dotfiles}\n\
         update_check    = {update_check}\n\
         confirm_discard = {confirm_discard}\n\
         scroll_lines    = {scroll_lines}\n\
         tree_width      = {tree_width}\n\
         tree_position   = {tree_position}\n\
         tree_max_cols   = {tree_max_cols}",
        editor = opt_os(&eff.editor),
        markdown = opt_argv(&eff.markdown),
        diff = opt_argv(&eff.diff),
        syntax = opt_argv(&eff.syntax),
        open = opt_argv(&eff.open),
        reveal = opt_argv(&eff.reveal),
        hide_dotfiles = eff.hide_dotfiles,
        update_check = if eff.update_check { "on" } else { "off" },
        scroll_lines = eff.scroll_lines,
        tree_width = eff.tree_width,
        tree_position = eff.tree_position.label(),
        tree_max_cols = eff.tree_max_cols,
        confirm_discard = if eff.confirm_discard { "on" } else { "off" },
    )
}

/// Assemble the "Keybindings" pane text (AC-16, AC-19, AC-20): a display-only, grouped listing of
/// every global action with its config var, effective key(s), and description. Pure — no env/FS; all
/// three inputs are already-resolved values the caller computed once at startup (`app::run`).
///
/// - When `outcome` is non-empty (a `[keys]` entry was ignored), a FIRST status line names the
///   rejected entries and their reasons, so the user knows some bindings were dropped and the
///   defaults kept (AC-16, the surfacing path). It is a single stable line, followed by a blank line.
/// - Then the actions, GROUPED under their [`category`](crate::input::Binding::category) header and
///   rendered in [`CATEGORY_ORDER`](crate::input::CATEGORY_ORDER); within a group they keep registry
///   (Intent::ALL) order, and groups are separated by a blank line. Each action is TWO lines:
///   - line 1: the config var (the [`name`](crate::input::Binding::name), left-aligned to the widest
///     name so the key column lines up), then its effective key(s) (from
///     [`keys_for`](crate::input::EffectiveBindings::keys_for), rendered via
///     [`key_label`](crate::input::key_label) and joined by " / "), then a trailing "  (custom)"
///     marker when the key set came from config (AC-20). An action with no effective key at all (its
///     only key was `Esc`, taken by the no-lockout floor) renders its key column as "(unbound)".
///   - line 2: the action's description, indented under it.
///
/// Formatting is kept simple and stable since this feeds a presenter snapshot (like `settings_text`).
///
/// Visibility is `pub(crate)` (not `pub`) because its argument types (`Binding`, `EffectiveBindings`,
/// `KeyLoadOutcome`) are `pub(crate)`; a `pub` signature would leak them and trip `private_interfaces`.
pub(crate) fn keybindings_text(
    registry: &[crate::input::Binding],
    bindings: &crate::input::EffectiveBindings,
    outcome: &crate::input::KeyLoadOutcome,
) -> String {
    let mut lines: Vec<String> = Vec::new();

    // AC-16: surface any ignored `[keys]` entries on a leading status line (name + reason), so the
    // failure is never silent. One line; the per-entry reasons come from `RejectReason`'s Display.
    if !outcome.is_empty() {
        let names: Vec<String> = outcome
            .rejected
            .iter()
            .map(|r| format!("{} ({})", r.name, r.reason))
            .collect();
        lines.push(format!(
            "{n} custom binding(s) ignored (using defaults): {names}",
            n = outcome.rejected.len(),
            names = names.join(", "),
        ));
        lines.push(String::new());
    }

    // Config-var (intent name) column width: the longest name, so the key column aligns down the
    // whole overlay. Line 1 leads with the `[keys]` config var (the thing you type to remap) and the
    // effective key(s); line 2 carries the human description under it.
    let name_width = registry.iter().map(|b| b.name.len()).max().unwrap_or(0);

    // Group the flat registry into the overlay's display sections (AC-19). Walk CATEGORY_ORDER for a
    // stable section order; within a section keep registry (Intent::ALL) order. A category with no
    // rows renders nothing (the registry invariant test forbids an empty category anyway).
    let mut first_group = true;
    for category in crate::input::CATEGORY_ORDER {
        let mut rows = registry
            .iter()
            .filter(|b| b.category == *category)
            .peekable();
        if rows.peek().is_none() {
            continue;
        }
        if !first_group {
            lines.push(String::new());
        }
        first_group = false;
        lines.push((*category).to_string());

        for binding in rows {
            let keys = bindings.keys_for(binding.intent);
            let rendered = if keys.is_empty() {
                "(unbound)".to_owned()
            } else {
                keys.iter()
                    .map(|k| crate::input::key_label(*k))
                    .collect::<Vec<_>>()
                    .join(" / ")
            };
            let marker = if bindings.is_customized(binding.intent) {
                "  (custom)"
            } else {
                ""
            };
            // Line 1: the config var (left, padded) then the effective key(s) and any custom marker.
            lines.push(format!(
                "  {name:<name_width$}  {rendered}{marker}",
                name = binding.name,
            ));
            // Line 2: the human description, indented under its action.
            lines.push(format!("    {}", binding.description));
        }
    }

    // Footer: how to actually create a config (the overlay is display-only, NC-3). Points at the
    // bundled template and the Settings tab, which shows the exact resolved config path + load
    // status, so this note stays pure (no path plumbing needed here).
    lines.push(String::new());
    lines.push(
        "To customize keys, create a config file: copy the bundled config.example.toml to your"
            .to_string(),
    );
    lines.push(
        "config dir and rename it to config.toml, then edit and relaunch. The Settings tab shows"
            .to_string(),
    );
    lines.push("your exact config path and whether a file is loaded.".to_string());

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::version::Version;

    // (a) CHANGELOG_MD is non-empty and version headings are newest-first.
    #[test]
    fn changelog_is_embedded_and_newest_first() {
        assert!(!CHANGELOG_MD.is_empty(), "CHANGELOG_MD must not be empty");

        let idx_150 = CHANGELOG_MD
            .find("## [1.5.0]")
            .expect("CHANGELOG_MD must contain the 1.5.0 heading");
        let idx_140 = CHANGELOG_MD
            .find("## [1.4.0]")
            .expect("CHANGELOG_MD must contain the 1.4.0 heading");

        assert!(
            idx_150 < idx_140,
            "1.5.0 heading (byte {idx_150}) must appear before 1.4.0 heading (byte {idx_140})"
        );
    }

    // ①: changelog_display() drops the file-meta preamble (title + Keep-a-Changelog/SemVer
    // paragraph + link refs) and starts at the first version heading — but keeps the entries.
    #[test]
    fn changelog_display_strips_file_preamble() {
        let shown = changelog_display();
        assert!(
            !shown.contains("Keep a Changelog"),
            "changelog_display() must not contain the 'Keep a Changelog' preamble line"
        );
        assert!(
            !shown.contains("Semantic Versioning"),
            "changelog_display() must not contain the 'Semantic Versioning' preamble line"
        );
        assert!(
            shown.starts_with("## ["),
            "changelog_display() must start at the first '## [' version heading"
        );
        // The const stays whole — the preamble is only sliced off for display.
        assert!(
            CHANGELOG_MD.contains("Keep a Changelog"),
            "CHANGELOG_MD const must remain whole (preamble intact for the newest-first test)"
        );
    }

    // (b) about_text(None) contains the required identity fields (AC-16, AC-17). The version now
    // lives on the status line; the repo URL is BARE (scheme stripped, no "Repository:" label);
    // the license reads "<SPDX> License".
    #[test]
    fn about_text_contains_identity_fields() {
        let text = about_text(None);
        assert!(
            text.contains(env!("CARGO_PKG_VERSION")),
            "about_text must contain the package version (AC-16)"
        );
        // The About top line is the DISPLAY title (the nice form), not the raw package name.
        assert!(
            text.contains("Herdr File Viewer"),
            "about_text must contain the display title (AC-17)"
        );
        // The raw package name still appears verbatim — in the bare repo URL.
        assert!(
            text.contains("herdr-file-viewer"),
            "about_text must still contain the package name in the repo URL (AC-17)"
        );
        assert!(
            text.contains(env!("CARGO_PKG_DESCRIPTION")),
            "about_text must contain the package description (AC-17)"
        );
        // The repo URL is rendered BARE — host+path only, the https:// scheme stripped.
        assert!(
            text.contains("github.com/smarzban/herdr-file-viewer"),
            "about_text must contain the bare repository URL (AC-17)"
        );
        assert!(
            !text.contains("https://") && !text.contains("Repository:"),
            "about_text must strip the URL scheme and the 'Repository:' label (AC-17)"
        );
        // The license reads "<SPDX> License" (e.g. "MIT License").
        assert!(
            text.contains("MIT License"),
            "about_text must contain the license as '<SPDX> License' (AC-17)"
        );
    }

    // (c) AC-18: the GitHub-star CTA uses a plain ★ (U+2605, not the ⭐️ emoji) and is the CLOSING
    // line of About — the last non-empty line, below "<SPDX> License".
    #[test]
    fn star_cta_is_the_closing_line() {
        let text = about_text(None);
        let lines: Vec<&str> = text.split('\n').collect();

        let cta_pos = lines
            .iter()
            .position(|l| l.contains('★') && l.contains("GitHub"))
            .expect("about_text must contain the ★ GitHub CTA line (AC-18)");
        // Plain star only — never the ⭐️ emoji (its double-width mis-renders in the TUI).
        assert!(
            !text.contains('⭐'),
            "about_text must use a plain ★ (U+2605), not the ⭐️ emoji (AC-18)"
        );
        let license_pos = lines
            .iter()
            .position(|l| l.contains("License"))
            .expect("about_text must contain the License line");
        assert!(
            license_pos < cta_pos,
            "the CTA (line {cta_pos}) must come BELOW the License line (line {license_pos}) — AC-18"
        );
        // It is the LAST non-empty line of About.
        let last_non_empty = lines
            .iter()
            .rposition(|l| !l.trim().is_empty())
            .expect("about_text has a non-empty line");
        assert_eq!(
            cta_pos, last_non_empty,
            "the CTA must be the closing (last non-empty) line of About (AC-18)"
        );
    }

    // --- HelpState / HelpSectionState tests ---

    fn make_section(label: &'static str, lines: usize) -> HelpSectionState {
        use ratatui::text::{Line, Text};
        let body = Text::from(
            (0..lines)
                .map(|i| Line::from(format!("line {i}")))
                .collect::<Vec<_>>(),
        );
        HelpSectionState {
            label,
            body,
            scroll: 0,
        }
    }

    // AC-7: next() wraps from last to first; prev() wraps from first to last.
    #[test]
    fn next_wraps_from_last_to_first() {
        let mut state = HelpState::new(vec![
            make_section("A", 5),
            make_section("B", 5),
            make_section("C", 5),
        ]);
        state.select(2); // jump to last
        state.next();
        assert_eq!(
            state.active_index(),
            0,
            "next() from last must wrap to first"
        );
    }

    #[test]
    fn prev_wraps_from_first_to_last() {
        let mut state = HelpState::new(vec![
            make_section("A", 5),
            make_section("B", 5),
            make_section("C", 5),
        ]);
        // active starts at 0
        state.prev();
        assert_eq!(
            state.active_index(),
            2,
            "prev() from first must wrap to last"
        );
    }

    // AC-8 / AC-9: scroll_by moves offset; clamp_scroll pins to [0, lines - height].
    #[test]
    fn scroll_by_moves_active_section_offset() {
        let mut state = HelpState::new(vec![make_section("A", 20)]);
        state.scroll_by(5);
        assert_eq!(
            state.sections[0].scroll, 5,
            "scroll_by(5) must set offset to 5"
        );
    }

    #[test]
    fn scroll_by_saturates_at_zero_on_negative() {
        let mut state = HelpState::new(vec![make_section("A", 20)]);
        state.scroll_by(-10); // no underflow
        assert_eq!(
            state.sections[0].scroll, 0,
            "scroll_by negative must not go below 0"
        );
    }

    #[test]
    fn clamp_scroll_pins_to_max() {
        let mut state = HelpState::new(vec![make_section("A", 10)]);
        state.scroll_by(100); // over-shoot: saturates at u16::MAX in scroll_by, clamped by clamp
        // Non-wrapping fixture → total_rows == raw lines (10). viewport_height = 4 → max = 10 - 4 = 6.
        state.clamp_scroll(10, 4);
        assert_eq!(
            state.sections[0].scroll, 6,
            "clamp_scroll must pin offset to total_rows - viewport_height"
        );
    }

    #[test]
    fn clamp_scroll_zero_when_body_fits() {
        let mut state = HelpState::new(vec![make_section("A", 3)]);
        state.scroll_by(10);
        // total_rows (3) <= viewport (10) → max = 0.
        state.clamp_scroll(3, 10);
        assert_eq!(
            state.sections[0].scroll, 0,
            "clamp_scroll must zero offset when body fits in the viewport"
        );
    }

    // AC-8: switching sections preserves each section's independent scroll.
    #[test]
    fn switching_sections_preserves_scroll_offset() {
        let mut state = HelpState::new(vec![make_section("A", 20), make_section("B", 20)]);
        // Set a scroll on section 0
        state.scroll_by(7);
        assert_eq!(state.sections[0].scroll, 7);
        // Switch to section 1
        state.next();
        assert_eq!(state.active_index(), 1);
        // Scroll section 1 independently
        state.scroll_by(3);
        assert_eq!(state.sections[1].scroll, 3);
        // Switch back to section 0 — its scroll is preserved
        state.prev();
        assert_eq!(state.active_index(), 0);
        assert_eq!(
            state.sections[0].scroll, 7,
            "section 0's scroll must be preserved after switching away and back"
        );
    }

    // select(idx) ignores out-of-range without panic.
    #[test]
    fn select_out_of_range_is_safe() {
        let mut state = HelpState::new(vec![make_section("A", 5), make_section("B", 5)]);
        state.select(99); // out of range — must not panic, must not change active
        assert_eq!(state.active_index(), 0);
    }

    // section_labels returns labels in order.
    #[test]
    fn section_labels_in_order() {
        let state = HelpState::new(vec![make_section("Foo", 1), make_section("Bar", 1)]);
        assert_eq!(state.section_labels(), vec!["Foo", "Bar"]);
    }

    // active_body returns the body of the active section.
    #[test]
    fn active_body_matches_active_section() {
        let state = HelpState::new(vec![make_section("A", 3), make_section("B", 7)]);
        assert_eq!(state.active_body().lines.len(), 3);
    }

    // (d) Update status line reflects the argument.
    #[test]
    fn about_text_update_status() {
        let v = Version {
            major: 2,
            minor: 0,
            patch: 0,
        };
        let with_update = about_text(Some(v));
        assert!(
            with_update.contains("Update available"),
            "about_text(Some(_)) must contain 'Update available'"
        );
        assert!(
            with_update.contains("2.0.0"),
            "about_text(Some(v)) must contain the version string"
        );

        let up_to_date = about_text(None);
        assert!(
            up_to_date.contains("Up to date"),
            "about_text(None) must contain 'Up to date'"
        );
    }

    // --- negative-criteria conformance (AC-N5, AC-N6) ---

    // AC-N6 (in-app-help-overlay's v1 scope guard) is INTENTIONALLY SUPERSEDED by
    // settings-config's AC-18 (SMA-49): a real `app::run` launch now wires `set_settings_display`,
    // so the live overlay ships a third "Settings" tab. This test's actual, narrower job is proving
    // the *built-in* `HelpSection` enum stays closed at exactly {WhatsNew, About} — no Keybindings
    // variant, no Settings variant. That enum-level closure still holds and is still worth guarding:
    // the Settings section deliberately bypasses this enum via the generic `HelpSectionState` seam
    // (`HelpState.sections: Vec<..>`) that the in-app-help-overlay design itself reserved for it, so
    // a 3-tab runtime overlay does NOT contradict this test passing. Do not read this as "the overlay
    // must only ever show two tabs," and do not "fix" it by adding a `Settings` variant here.
    #[test]
    fn help_section_set_is_exactly_whats_new_and_about() {
        // Exhaustively enumerate the variants by matching every one: adding a variant makes this
        // match non-exhaustive (a compile error), forcing the author to revisit the scope guard.
        for s in [HelpSection::WhatsNew, HelpSection::About] {
            match s {
                HelpSection::WhatsNew => assert_eq!(s.label(), "What's New"),
                HelpSection::About => assert_eq!(s.label(), "About"),
            }
        }
        // And the built-in enum's label set is precisely these two, in this order — the runtime
        // overlay may append further HelpSectionState entries (e.g. Settings, SMA-49) beyond this.
        let labels: Vec<&str> = [HelpSection::WhatsNew, HelpSection::About]
            .iter()
            .map(|s| s.label())
            .collect();
        assert_eq!(
            labels,
            vec!["What's New", "About"],
            "AC-N6 (enum-level, superseded at the overlay level by SMA-49): the built-in \
             HelpSection enum is exactly What's New then About — no Keybindings/Settings variant. \
             The runtime overlay's separate Settings HelpSectionState is expected and does not \
             violate this."
        );
    }

    // AC-N5 (no network, by construction): `about_text` is a pure function of its single argument
    // — the ALREADY-cached update status. It reads no global, performs no I/O, and issues no probe;
    // the only thing that varies its output is the value passed in. We prove this by determinism:
    // for a fixed argument the output is byte-identical across calls (no hidden time/network/random
    // input), and the ONLY observable difference between two calls is driven by the argument.
    #[test]
    fn about_text_is_a_pure_function_of_its_cached_argument() {
        // Same argument → byte-identical output across repeated calls (no hidden varying input such
        // as a network/update probe would introduce).
        let a1 = about_text(None);
        let a2 = about_text(None);
        assert_eq!(
            a1, a2,
            "AC-N5: about_text(None) must be deterministic — no network/probe varies its output"
        );

        let v = Version {
            major: 9,
            minor: 9,
            patch: 9,
        };
        let b1 = about_text(Some(v));
        let b2 = about_text(Some(v));
        assert_eq!(
            b1, b2,
            "AC-N5: about_text(Some(_)) must be deterministic for a fixed cached value"
        );

        // The ONLY observable difference between the two outputs is the update-status line, i.e. it
        // reflects exactly the passed cached value — never a freshly-probed one. `None` ⇒ "Up to
        // date"; `Some(9.9.9)` ⇒ "Update available: v9.9.9". Identity lines (name/version/repo/
        // license/CTA) are identical between the two.
        assert!(a1.contains("Up to date"));
        assert!(!a1.contains("Update available"));
        assert!(b1.contains("Update available: v9.9.9"));
        assert!(!b1.contains("Up to date"));
    }

    // --- settings_text tests (AC-15, AC-18) ---

    use crate::config::{EffectiveSettings, LoadOutcome};

    fn sample_eff() -> EffectiveSettings {
        EffectiveSettings {
            editor: Some(std::ffi::OsString::from("nano")),
            markdown: Some(vec!["glow".to_string(), "-w".to_string(), "80".to_string()]),
            diff: None,
            syntax: None,
            open: None,
            reveal: None,
            hide_dotfiles: true,
            update_check: false,
            confirm_discard: false,
            scroll_lines: 7,
            tree_width: 25,
            tree_position: crate::config::TreePosition::Right,
            tree_max_cols: 50,
        }
    }

    // AC-18: every setting key appears with its effective value; unset fields show "(default)".
    #[test]
    fn settings_text_lists_every_setting_row() {
        let eff = sample_eff();
        let text = settings_text(
            &eff,
            &LoadOutcome::Loaded,
            std::path::Path::new("/cfg/config.toml"),
        );

        for key in [
            "editor",
            "markdown",
            "diff",
            "syntax",
            "open",
            "reveal",
            "hide_dotfiles",
            "update_check",
            "scroll_lines",
            "tree_width",
            "tree_position",
            "tree_max_cols",
        ] {
            assert!(
                text.contains(key),
                "settings_text must contain a row for '{key}':\n{text}"
            );
        }
        // AC-9: the effective scroll step is shown as its own row with its value (7 in the fixture).
        assert!(
            text.lines()
                .any(|l| l.trim_start().starts_with("scroll_lines") && l.contains('7')),
            "settings_text must show the effective scroll_lines value (7):\n{text}"
        );
        // AC-12: the effective tree width (25) and tree position (right) each appear as their own row.
        assert!(
            text.lines()
                .any(|l| l.trim_start().starts_with("tree_width") && l.contains("25")),
            "settings_text must show the effective tree_width value (25):\n{text}"
        );
        assert!(
            text.lines()
                .any(|l| l.trim_start().starts_with("tree_position") && l.contains("right")),
            "settings_text must show the effective tree_position (right):\n{text}"
        );
        assert!(
            text.lines()
                .any(|l| l.trim_start().starts_with("tree_max_cols") && l.contains("50")),
            "settings_text must show the effective tree_max_cols value (50):\n{text}"
        );
        assert!(text.contains("nano"), "editor value must appear:\n{text}");
        assert!(
            text.contains("glow -w 80"),
            "the markdown argv must appear space-joined:\n{text}"
        );
        assert!(
            text.contains("true"),
            "hide_dotfiles=true must appear:\n{text}"
        );
        assert!(
            text.contains("off"),
            "update_check=false must render as 'off':\n{text}"
        );
        // None fields show "(default)" — diff/syntax/open/reveal are all None in the fixture.
        let default_count = text.matches("(default)").count();
        assert_eq!(
            default_count, 4,
            "diff/syntax/open/reveal are None and must each show '(default)':\n{text}"
        );
    }

    #[test]
    fn settings_text_loaded_outcome_reflects_success() {
        let eff = sample_eff();
        let text = settings_text(
            &eff,
            &LoadOutcome::Loaded,
            std::path::Path::new("/cfg/config.toml"),
        );
        assert!(
            text.starts_with("Config: loaded."),
            "the Loaded outcome must be reflected on the first line:\n{text}"
        );
    }

    // AC-15: a Malformed outcome surfaces a "using defaults" indicator plus the reason.
    #[test]
    fn settings_text_malformed_outcome_shows_using_defaults_and_reason() {
        let eff = sample_eff();
        let text = settings_text(
            &eff,
            &LoadOutcome::Malformed("bad toml".to_string()),
            std::path::Path::new("/cfg/config.toml"),
        );
        assert!(
            text.contains("using defaults"),
            "AC-15: a Malformed outcome must contain a 'using defaults' indicator:\n{text}"
        );
        assert!(
            text.contains("bad toml"),
            "AC-15: the malformed reason must be surfaced:\n{text}"
        );
    }

    #[test]
    fn settings_text_malformed_reason_is_collapsed_to_a_single_status_line() {
        // Regression: the real toml error is MULTI-LINE (a source snippet + a `^` caret). The
        // status line must show only its first line, not spill the caret art across the overlay.
        let eff = sample_eff();
        let multiline =
            "TOML parse error at line 1, column 5\n  |\n1 | x = = [\n  |     ^\nexpected value";
        let text = settings_text(
            &eff,
            &LoadOutcome::Malformed(multiline.to_string()),
            std::path::Path::new("/cfg/config.toml"),
        );
        let status = text.lines().next().unwrap();
        assert!(
            status.contains("using defaults") && status.contains("line 1, column 5"),
            "status keeps a one-line locator:\n{status}"
        );
        assert!(
            !text.contains('^') && !text.contains("expected value"),
            "the multi-line caret/snippet must not leak into the overlay:\n{text}"
        );
    }

    #[test]
    fn settings_text_shows_the_config_location() {
        // The resolved config path is surfaced so the user knows what to fix/create.
        let eff = sample_eff();
        let text = settings_text(
            &eff,
            &LoadOutcome::Absent,
            std::path::Path::new("/home/u/.config/herdr-file-viewer/config.toml"),
        );
        assert!(
            text.contains("/home/u/.config/herdr-file-viewer/config.toml"),
            "the config-file location must be shown:\n{text}"
        );
    }

    #[test]
    fn settings_text_absent_outcome_shows_using_defaults() {
        let eff = sample_eff();
        let text = settings_text(
            &eff,
            &LoadOutcome::Absent,
            std::path::Path::new("/cfg/config.toml"),
        );
        assert!(
            text.contains("using defaults"),
            "an Absent outcome must also indicate defaults are in use:\n{text}"
        );
    }

    // --- keybindings_text tests (AC-16, AC-19, AC-20) ---

    use crate::config::KeySpec;
    use crate::input;
    use std::collections::BTreeMap;

    /// Resolve the registry against a one-entry `[keys]` table `name = <single key>`.
    fn resolve_one(name: &str, key: &str) -> (input::EffectiveBindings, input::KeyLoadOutcome) {
        let mut keys: BTreeMap<String, KeySpec> = BTreeMap::new();
        keys.insert(name.to_string(), KeySpec::One(key.to_string()));
        input::resolve_bindings(input::registry(), Some(&keys))
    }

    // AC-19: the Keybindings section lists EVERY registry action — one row per action showing its
    // description and effective key(s). We assert the output contains each binding's description
    // (loop over registry()) and representative effective key labels ('r' for refresh, 'Tab' for
    // toggle_focus). With no rejected entries there is no ignored-bindings status line.
    #[test]
    fn keybindings_text_lists_every_registry_action_ac19() {
        let text = keybindings_text(
            input::registry(),
            &input::default_bindings(),
            &input::KeyLoadOutcome::default(),
        );
        for binding in input::registry() {
            // Every action shows its config var (the `[keys]` intent name) AND its description.
            assert!(
                text.contains(binding.name),
                "keybindings_text must show the config var '{}':\n{text}",
                binding.name
            );
            assert!(
                text.contains(binding.description),
                "keybindings_text must contain a row for '{}' (its description):\n{text}",
                binding.name
            );
        }
        // Every display group header renders (grouping, AC-19).
        for cat in input::CATEGORY_ORDER {
            assert!(
                text.lines().any(|l| l == *cat),
                "the '{cat}' group header must render:\n{text}"
            );
        }
        // Representative effective key appears on the action's line: 'r' → refresh, 'Tab' → toggle_focus.
        let refresh_row = text
            .lines()
            .find(|l| l.split_whitespace().next() == Some("refresh"))
            .expect("keybindings_text must contain the refresh row");
        assert!(
            refresh_row.split_whitespace().any(|t| t == "r"),
            "refresh's default key 'r' must render on its row:\n{refresh_row}"
        );
        let focus_row = text
            .lines()
            .find(|l| l.split_whitespace().next() == Some("toggle_focus"))
            .expect("keybindings_text must contain the toggle_focus row");
        assert!(
            focus_row.split_whitespace().any(|t| t == "Tab"),
            "toggle_focus's default key 'Tab' must render on its row:\n{focus_row}"
        );
        // A clean default outcome carries no ignored-bindings status line.
        assert!(
            !text.contains("ignored (using defaults)"),
            "a clean default outcome must not show an ignored-bindings line:\n{text}"
        );
    }

    #[test]
    fn keybindings_text_groups_actions_under_their_category_header() {
        // AC-19 (grouping): each action renders under its category header, and the headers appear in
        // CATEGORY_ORDER (like herdr's grouped keybinds list).
        let text = keybindings_text(
            input::registry(),
            &input::default_bindings(),
            &input::KeyLoadOutcome::default(),
        );
        let lines: Vec<&str> = text.lines().collect();
        let header = |h: &str| lines.iter().position(|l| *l == h);
        let action = |a: &str| {
            lines
                .iter()
                .position(|l| l.split_whitespace().next() == Some(a))
        };

        // Headers appear in CATEGORY_ORDER.
        let positions: Vec<usize> = input::CATEGORY_ORDER
            .iter()
            .map(|c| header(c).unwrap_or_else(|| panic!("missing header '{c}'")))
            .collect();
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        assert_eq!(
            positions, sorted,
            "category headers must render in CATEGORY_ORDER:\n{text}"
        );

        // Pin the grouping CONTRACT, not just that grouping happens: a representative action from
        // every category must render within THAT category's span (its header .. the next header),
        // so miscategorising an action, or reordering/renaming CATEGORY_ORDER out of sync with the
        // registry's `category` values, fails here instead of passing silently.
        let span = |cat: &str| {
            let start = header(cat).unwrap_or_else(|| panic!("missing header '{cat}'"));
            let next = positions.iter().filter(|&&p| p > start).min().copied();
            (start, next.unwrap_or(lines.len()))
        };
        for (name, cat) in [
            ("nav_up", "Navigation"),
            ("activate", "Navigation"),
            ("toggle_wrap", "View & layout"),
            ("refresh", "Git & filters"),
            ("open_with_app", "Open & copy"),
            ("add_annotation", "Annotations"),
            ("open_finder", "Search & jump"),
            ("close", "Session"),
        ] {
            let at = action(name).unwrap_or_else(|| panic!("missing action '{name}'"));
            let (start, end) = span(cat);
            assert!(
                start < at && at < end,
                "action '{name}' must render under the '{cat}' header (rows {start}..{end}), got row {at}:\n{text}"
            );
        }
    }

    // AC-20: a custom binding (an action remapped via `[keys]`) is visually marked with "(custom)";
    // an unremapped action's row is not.
    #[test]
    fn keybindings_text_footer_points_to_the_config_template() {
        // The Keybindings tab ends with a note telling users how to create a config: copy the
        // bundled config.example.toml, rename to config.toml. Keeps the display-only overlay (NC-3)
        // discoverable without an in-app editor.
        let text = keybindings_text(
            input::registry(),
            &input::default_bindings(),
            &input::KeyLoadOutcome::default(),
        );
        assert!(
            text.contains("config.example.toml"),
            "the Keybindings tab must point users at the bundled config.example.toml:\n{text}"
        );
        assert!(
            text.contains("config.toml"),
            "the note must tell users to rename it to config.toml:\n{text}"
        );
        assert!(
            text.contains("Settings"),
            "the note must send users to the Settings tab for their exact config path:\n{text}"
        );
    }

    #[test]
    fn keybindings_text_marks_custom_binding_ac20() {
        let (bindings, outcome) = resolve_one("refresh", "g");
        let text = keybindings_text(input::registry(), &bindings, &outcome);

        // The refresh action line (led by its config var) now shows 'g' and the "(custom)" marker.
        let refresh_row = text
            .lines()
            .find(|l| l.split_whitespace().next() == Some("refresh"))
            .expect("keybindings_text must contain the refresh row");
        assert!(
            refresh_row.split_whitespace().any(|t| t == "g"),
            "the refresh row must show its new key 'g':\n{refresh_row}"
        );
        assert!(
            refresh_row.contains("(custom)"),
            "the remapped refresh row must carry the '(custom)' marker (AC-20):\n{refresh_row}"
        );

        // An unremapped row (nav_up) must NOT be marked custom.
        let nav_up_row = text
            .lines()
            .find(|l| l.split_whitespace().next() == Some("nav_up"))
            .expect("keybindings_text must contain the nav_up row");
        assert!(
            !nav_up_row.contains("(custom)"),
            "an unremapped nav_up row must not carry the '(custom)' marker (AC-20):\n{nav_up_row}"
        );
    }

    // AC-20 edge (reviewer note): an intent can be customized yet have ZERO effective keys — its
    // only key was `Esc`, which the no-lockout floor reassigns to Close. Its row still reads clearly
    // via "(unbound)" and stays marked "(custom)".
    #[test]
    fn keybindings_text_unbound_row_for_esc_only_custom_binding() {
        // Rebind `refresh` to Esc only: the Esc floor takes Esc for Close, leaving refresh keyless.
        let (bindings, outcome) = resolve_one("refresh", "Esc");
        assert!(outcome.is_empty(), "naming Esc is valid, not rejected");
        assert!(
            bindings.keys_for(crate::intent::Intent::Refresh).is_empty(),
            "refresh has no effective key once Esc is claimed by the floor"
        );
        let text = keybindings_text(input::registry(), &bindings, &outcome);
        let refresh_row = text
            .lines()
            .find(|l| l.split_whitespace().next() == Some("refresh"))
            .expect("keybindings_text must contain the refresh row");
        assert!(
            refresh_row.contains("(unbound)"),
            "a keyless customized row must render its key column as '(unbound)':\n{refresh_row}"
        );
        assert!(
            refresh_row.contains("(custom)"),
            "the row is still a custom binding:\n{refresh_row}"
        );
    }

    #[test]
    fn keybindings_text_shows_displaced_annotation_actions_as_unbound() {
        let mut keys: BTreeMap<String, KeySpec> = BTreeMap::new();
        keys.insert("refresh".to_string(), KeySpec::One("a".to_string()));
        keys.insert("show_help".to_string(), KeySpec::One("A".to_string()));
        let (bindings, outcome) = input::resolve_bindings(input::registry(), Some(&keys));
        assert!(outcome.is_empty());

        let text = keybindings_text(input::registry(), &bindings, &outcome);
        for name in ["add_annotation", "show_annotations"] {
            let row = text
                .lines()
                .find(|line| line.split_whitespace().next() == Some(name))
                .unwrap_or_else(|| panic!("missing {name} row:\n{text}"));
            assert!(
                row.contains("(unbound)"),
                "a config-owned default must leave {name} visibly unbound:\n{row}"
            );
            assert!(
                !row.contains("(custom)"),
                "the displaced annotation action itself was not customized:\n{row}"
            );
        }
    }

    // AC-16: an outcome carrying a rejected entry surfaces a leading ignored-bindings status line
    // naming the dropped entry (and its reason), so the failure is not silent.
    #[test]
    fn keybindings_text_surfaces_ignored_bindings_ac16() {
        // An unknown intent name is rejected by the resolver (defaults kept for everything valid).
        let (bindings, outcome) = resolve_one("bogus_intent", "g");
        assert!(!outcome.is_empty(), "an unknown intent name is rejected");

        let text = keybindings_text(input::registry(), &bindings, &outcome);
        let status = text.lines().next().unwrap();
        assert!(
            status.contains("ignored (using defaults)"),
            "AC-16: a rejected entry must surface an ignored-bindings status line:\n{status}"
        );
        assert!(
            status.contains("bogus_intent"),
            "the ignored entry's name must be surfaced:\n{status}"
        );
    }
}
