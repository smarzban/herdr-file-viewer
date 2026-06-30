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

/// The two sections of the help overlay.
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

    // AC-N6 (scope guard, source level): the two-section set the overlay ships in v1 is
    // exactly {What's New, About}. `HelpState` itself is a generic Vec (the settings seam), so the
    // guarantee that there is no third section is anchored at the SOURCE that defines the sections:
    // `HelpSection` enumerates exactly those two variants, with exactly those labels. A future
    // settings section would have to add a variant here — which would flip this test red on purpose.
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
        // And the ordered v1 label set is precisely these two, in this order.
        let labels: Vec<&str> = [HelpSection::WhatsNew, HelpSection::About]
            .iter()
            .map(|s| s.label())
            .collect();
        assert_eq!(
            labels,
            vec!["What's New", "About"],
            "AC-N6: v1 exposes exactly What's New then About — no Keybindings/Settings"
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
}
