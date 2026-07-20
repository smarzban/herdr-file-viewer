//! All-modes / smartcase / truncation integration + search e2e.
//!
//! Proves the search subsystem end-to-end through the controller:
//!
//! - **AC-13** — search finds text as displayed in every view mode (RenderedMarkdown, Diff,
//!   SyntaxContent). Because the search reads `content.lines` plain text, it is
//!   view-agnostic — we prove it across modes by steering the view-policy with the right file
//!   extension and git-status metadata.
//!
//! - **AC-12** — smartcase end-to-end: a lowercase query matches mixed-case content; a query
//!   containing an uppercase letter is case-sensitive.
//!
//! - **AC-23** — on a size-cap-truncated file, text beyond the truncation boundary is not
//!   matched. The search literally never sees that text — it only operates on `content.lines`
//!   (the truncated body returned by the ContentProvider).
//!
//! Integration tests live here; they bring in no new Cargo deps and touch no `src/` files.

mod common;

use common::TempDir;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herdr_file_viewer::controller::{
    Components, ContentProvider, Controller, EditorHandoff, EditorOutcome, GitService,
    RenderResult, RootProviders,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::view_policy::ViewMode;
use ratatui::text::Text;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ── stubs ────────────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct StubGit {
    status: BTreeMap<PathBuf, Status>,
    changed: BTreeMap<PathBuf, Status>,
}

impl GitService for StubGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        self.status.clone()
    }
    fn changed_set(&self, _baseline: Baseline) -> BTreeMap<PathBuf, Status> {
        self.changed.clone()
    }
    fn diff(&self, _p: &Path, _b: Baseline, _full: bool) -> String {
        String::new()
    }
    fn diff_directory(&self, _rel_dir: &Path, _baseline: Baseline) -> String {
        String::new()
    }
}

struct NoopEditor;
impl EditorHandoff for NoopEditor {
    fn open(&mut self, _file: &Path) -> EditorOutcome {
        EditorOutcome::NoTakeover
    }
}

/// Build a key event with no modifiers.
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Flatten a `Text` to a plain string for assertions.
fn flatten(t: &Text) -> String {
    t.lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Spin `poll()` until the content pane contains `marker` or the deadline passes.
fn await_marker(ctrl: &mut Controller, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !flatten(ctrl.content()).contains(marker) {
        ctrl.poll();
        assert!(
            Instant::now() < deadline,
            "content '{marker}' never rendered"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ── content provider stubs ───────────────────────────────────────────────────

/// A content provider that returns fixed multi-line text containing the word
/// "needle" in mixed case at known lines, regardless of view mode.  Used to
/// prove AC-13 (view-agnostic search) and AC-12 (smartcase).
///
/// Lines (0-based):
///   0 — "line0 other content"
///   1 — "line1 NEEDLE here"       ← uppercase
///   2 — "line2 other content"
///   3 — "line3 Needle here"       ← mixed case
///   4 — "line4 other content"
///   5 — "line5 needle here"       ← lowercase
///   6 — "line6 other content"
///   7 — "line7 needle here"       ← lowercase
///   8..19 — "lineN other content"
#[derive(Clone, Copy)]
struct SearchContent;

impl ContentProvider for SearchContent {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        let lines: Vec<String> = (0..20)
            .map(|i| match i {
                1 => "line1 NEEDLE here".to_string(),
                3 => "line3 Needle here".to_string(),
                5 => "line5 needle here".to_string(),
                7 => "line7 needle here".to_string(),
                _ => format!("line{i} other content"),
            })
            .collect();
        RenderResult {
            content: Text::raw(lines.join("\n")),
            notices: Vec::new(),
            source: None,
        }
    }
}

/// A content provider that mimics the size-cap truncation: it returns only the
/// first `visible_lines` lines of content plus a truncation notice, simulating
/// what the real Content Renderer does for large files.  The text that would
/// appear "beyond" the truncation is never present in `content.lines`.
///
/// Wrapped in `Arc` so the `Fn` closure can clone it cheaply.
#[derive(Clone)]
struct TruncatedContent {
    full_lines: Arc<Vec<String>>,
    visible: usize,
}

impl ContentProvider for TruncatedContent {
    fn render(&self, _path: &Path, _mode: ViewMode, _raw_diff: Option<&str>) -> RenderResult {
        let mut shown: Vec<String> = self.full_lines[..self.visible].to_vec();
        shown.push("[File truncated - too large to display fully]".to_string());
        RenderResult {
            content: Text::raw(shown.join("\n")),
            notices: Vec::new(),
            source: None,
        }
    }
}

// ── controller builder helpers ───────────────────────────────────────────────

/// Build a controller over `root` (non-git) with `StubGit` and a `Clone` content provider.
/// The closure must be `Fn` (re-rootable), so the provider must be `Clone`.
fn ctrl_with_content<P: ContentProvider + Clone + 'static>(root: &Path, content: P) -> Controller {
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::new(StubGit::default()),
            content: Box::new(content.clone()), // `content` is Clone → cloned each call
        }),
        editor: Box::new(NoopEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    Controller::new(
        common::resolved(root.to_path_buf(), false),
        Baseline::Head,
        components,
    )
}

/// Build a controller over `root` with canned git status/changed maps and a `Clone` content
/// provider.  Used to produce a Diff-mode view (a file that is "changed" in git status).
fn ctrl_with_git_and_content<P: ContentProvider + Clone + 'static>(
    root: &Path,
    git_status: BTreeMap<PathBuf, Status>,
    git_changed: BTreeMap<PathBuf, Status>,
    content: P,
) -> Controller {
    let git: Arc<dyn GitService> = Arc::new(StubGit {
        status: git_status,
        changed: git_changed,
    });
    let components = Components {
        providers: Box::new(move |_resolved| RootProviders {
            git: Arc::clone(&git),
            content: Box::new(content.clone()), // `content` is Clone → cloned each call
        }),
        editor: Box::new(NoopEditor),
        clipboard: Box::new(common::RecordingClipboard::default()),
        renderers: None,
    };
    Controller::new(
        common::resolved(root.to_path_buf(), true), // is_git_repo = true
        Baseline::Head,
        components,
    )
}

// ── AC-13: search in every view mode ─────────────────────────────────────────

/// AC-13 sub-case helper: commit a search for `query` in the given controller and return the
/// number of matches found.  Assumes the content is already rendered (caller waits with
/// `await_marker`).
fn commit_search(ctrl: &mut Controller, query: &str) -> usize {
    ctrl.handle(Intent::OpenSearch);
    for c in query.chars() {
        ctrl.handle_prompt_key(key(KeyCode::Char(c)));
    }
    ctrl.handle_prompt_key(key(KeyCode::Enter));
    ctrl.search().map(|s| s.matches.len()).unwrap_or(0)
}

#[test]
fn search_finds_text_in_rendered_markdown_view() {
    // AC-13: a .md file is displayed in RenderedMarkdown mode. The search still finds
    // the text produced by the content provider — because search reads `content.lines`
    // plain text regardless of how it will be rendered in the pane.
    let dir = TempDir::new();
    // A `.md` extension forces the view-policy to pick RenderedMarkdown as the default.
    std::fs::write(dir.path().join("doc.md"), "placeholder\n").unwrap();

    let mut ctrl = ctrl_with_content(dir.path(), SearchContent);
    await_marker(&mut ctrl, "needle"); // wait for the render worker

    // Confirm the view mode is RenderedMarkdown.
    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::RenderedMarkdown),
        "precondition: .md file is in RenderedMarkdown"
    );

    // All-lowercase "needle" — smartcase → case-insensitive; matches NEEDLE, Needle, needle, needle.
    let count = commit_search(&mut ctrl, "needle");
    assert_eq!(
        count, 4,
        "AC-13: search finds 4 'needle' occurrences in RenderedMarkdown view (case-insensitive)"
    );
}

#[test]
fn search_finds_text_in_diff_view() {
    // AC-13: a changed file defaults to Diff mode.  The search finds the text produced by
    // the ContentProvider (which returns the same needle-bearing body regardless of mode).
    let dir = TempDir::new();
    // A `.rs` extension + git-changed status → Diff view (default_mode for a changed file).
    let rel = PathBuf::from("changed.rs");
    std::fs::write(dir.path().join("changed.rs"), "placeholder\n").unwrap();

    let git_map = BTreeMap::from([(rel.clone(), Status::Modified)]);
    let mut ctrl = ctrl_with_git_and_content(dir.path(), git_map.clone(), git_map, SearchContent);
    await_marker(&mut ctrl, "needle");

    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::Diff),
        "precondition: a changed file starts in Diff view"
    );

    let count = commit_search(&mut ctrl, "needle");
    assert_eq!(
        count, 4,
        "AC-13: search finds 4 'needle' occurrences in Diff view"
    );
}

#[test]
fn search_finds_text_in_syntax_content_view() {
    // AC-13: an unchanged `.rs` file is in SyntaxContent mode.  Same assertion — the search
    // reads `content.lines` from the provider, independent of the rendering pipeline.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("main.rs"), "placeholder\n").unwrap();

    let mut ctrl = ctrl_with_content(dir.path(), SearchContent);
    await_marker(&mut ctrl, "needle");

    assert_eq!(
        ctrl.selected_view_mode(),
        Some(ViewMode::SyntaxContent),
        "precondition: an unchanged .rs file is in SyntaxContent"
    );

    let count = commit_search(&mut ctrl, "needle");
    assert_eq!(
        count, 4,
        "AC-13: search finds 4 'needle' occurrences in SyntaxContent view"
    );
}

// ── AC-12: smartcase end-to-end through the controller ───────────────────────

#[test]
fn smartcase_lowercase_query_matches_mixed_case_content() {
    // AC-12: an all-lowercase query triggers case-insensitive matching.
    // The content has "NEEDLE" (line 1), "Needle" (line 3), "needle" (lines 5, 7) → 4 matches.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "placeholder\n").unwrap();

    let mut ctrl = ctrl_with_content(dir.path(), SearchContent);
    await_marker(&mut ctrl, "needle");
    ctrl.set_content_viewport(40, 5);

    let count = commit_search(&mut ctrl, "needle");
    assert_eq!(
        count, 4,
        "AC-12: all-lowercase 'needle' matches NEEDLE, Needle, needle, needle (4 total)"
    );
}

#[test]
fn smartcase_uppercase_query_is_case_sensitive() {
    // AC-12: a query containing at least one uppercase letter triggers case-sensitive matching.
    // Only the exact "NEEDLE" on line 1 matches; "Needle" (line 3) and lowercase (lines 5, 7)
    // do not.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "placeholder\n").unwrap();

    let mut ctrl = ctrl_with_content(dir.path(), SearchContent);
    await_marker(&mut ctrl, "needle");
    ctrl.set_content_viewport(40, 5);

    let count = commit_search(&mut ctrl, "NEEDLE");
    assert_eq!(
        count, 1,
        "AC-12: uppercase 'NEEDLE' is case-sensitive — matches only the exact 'NEEDLE' on line 1"
    );
}

#[test]
fn smartcase_mixed_case_query_is_case_sensitive() {
    // AC-12: a mixed-case query like "Needle" triggers case-sensitive matching.
    // Only the "Needle" on line 3 matches.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "placeholder\n").unwrap();

    let mut ctrl = ctrl_with_content(dir.path(), SearchContent);
    await_marker(&mut ctrl, "needle");
    ctrl.set_content_viewport(40, 5);

    let count = commit_search(&mut ctrl, "Needle");
    assert_eq!(
        count, 1,
        "AC-12: mixed-case 'Needle' is case-sensitive — matches only the 'Needle' on line 3"
    );
}

// ── AC-23: search covers only the truncated content ──────────────────────────

#[test]
fn search_does_not_find_text_beyond_the_truncation_boundary() {
    // AC-23: on a size-cap-truncated file the ContentProvider returns only the first N lines
    // (mimicking the real truncation path). The search literally never sees the truncated-away
    // text, so a query for a string that exists ONLY beyond the boundary returns zero matches.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.txt"), "placeholder\n").unwrap();

    // Full document: 20 lines. "BEYONDMARKER" is only in line 15 (0-based), past the cap.
    // Lines 0-9 contain "VISIBLETOKEN" at line 3; nothing beyond line 9 is shown.
    let full_lines: Vec<String> = (0..20)
        .map(|i| match i {
            3 => "line3 VISIBLETOKEN present".to_string(),
            15 => "line15 BEYONDMARKER hidden".to_string(),
            _ => format!("line{i} filler"),
        })
        .collect();

    let truncated = TruncatedContent {
        full_lines: Arc::new(full_lines),
        visible: 10, // only lines 0..10 are shown; the truncation notice follows
    };

    let mut ctrl = ctrl_with_content(dir.path(), truncated);
    await_marker(&mut ctrl, "VISIBLETOKEN");
    ctrl.set_content_viewport(40, 5);

    // A query for the visible token finds it.
    let count_visible = commit_search(&mut ctrl, "VISIBLETOKEN");
    assert_eq!(
        count_visible, 1,
        "precondition: VISIBLETOKEN (within the visible region) is found"
    );

    // Re-open a new search for the beyond-boundary token — must return zero.
    let count_beyond = commit_search(&mut ctrl, "BEYONDMARKER");
    assert_eq!(
        count_beyond, 0,
        "AC-23: BEYONDMARKER (past the truncation boundary) is NOT found — search sees only the truncated content"
    );
}
