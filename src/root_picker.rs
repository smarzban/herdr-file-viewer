//! Root Picker — the `--pick-root` popup: ask for a directory, then open the viewer there.
//!
//! Runs inside a herdr popup (the manifest's `root-picker` pane, `placement = "popup"`). The user
//! edits a path pre-filled with `~/`; `Tab` completes directory names, `Enter` hands the chosen
//! directory to a fresh viewer tab through `plugin pane open --env HERDR_FILE_VIEWER_ROOT=<dir>`
//! (never `--cwd`: the manifest pane command is relative, #139), and `Esc` / `Ctrl-C` cancel.
//! Exiting closes the popup.
//!
//! Read-only: it lists directories and asks herdr to open a pane; it never writes a file.
//! The state machine ([`RootPicker`]) is pure over an injected [`PickerFs`], so expansion,
//! completion, and validation are unit-tested without touching the real filesystem or herdr.

use crate::herdr::HerdrCli;
use crate::prompt::PromptInput;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use std::io;
use std::path::{Path, PathBuf};

/// What the prompt starts with: the picker resolves from the home directory.
pub const INITIAL_INPUT: &str = "~/";

/// The filesystem the picker reads. Injected so tests stay hermetic.
pub trait PickerFs {
    /// The user's home directory (`$HOME`), if known.
    fn home(&self) -> Option<PathBuf>;
    /// Names of the directories directly inside `dir` (following symlinks). Unreadable → empty.
    fn list_dirs(&self, dir: &Path) -> Vec<String>;
    /// The canonical form of `path` when it is an existing directory, else `None`.
    fn canonical_dir(&self, path: &Path) -> Option<PathBuf>;
}

/// The real filesystem.
pub struct RealFs;

impl PickerFs for RealFs {
    fn home(&self) -> Option<PathBuf> {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
    }

    fn list_dirs(&self, dir: &Path) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            // `Path::is_dir` follows symlinks, so a symlinked directory completes too.
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect()
    }

    fn canonical_dir(&self, path: &Path) -> Option<PathBuf> {
        path.is_dir().then(|| path.canonicalize().ok()).flatten()
    }
}

/// Expand the typed text into a path. `~`, `~/…`, and relative input resolve under `home`;
/// an absolute path is taken as typed. `~user` forms are not supported (`Err`).
pub fn expand(input: &str, home: Option<&Path>) -> Result<PathBuf, String> {
    let input = input.trim();
    if input.starts_with('/') {
        return Ok(PathBuf::from(input));
    }
    let home = home.ok_or_else(|| "cannot find your home directory ($HOME)".to_string())?;
    let rest = if input == "~" {
        ""
    } else if let Some(rest) = input.strip_prefix("~/") {
        rest
    } else if input.starts_with('~') {
        return Err("~user paths are not supported".to_string());
    } else {
        input
    };
    Ok(if rest.is_empty() {
        home.to_path_buf()
    } else {
        home.join(rest)
    })
}

/// The result of one `Tab` press: the new input text and the candidates to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub text: String,
    pub matches: Vec<String>,
}

/// Complete the last path segment of `input` against the directories in its parent.
///
/// - One match → the full name plus a trailing `/`, ready for the next segment.
/// - Several → extended to their longest common prefix, with every match listed.
/// - None → the input is left unchanged and no matches are listed.
///
/// Matching ignores case (`work` completes `Workspace`), and the completed text takes the
/// directory's real casing, so what is opened is the name on disk. Hidden directories are offered
/// only when the typed segment starts with `.`. Matches are sorted case-insensitively so the list
/// (and `Tab` cycling through it) is stable.
pub fn complete(input: &str, fs: &dyn PickerFs) -> Completion {
    let (head, prefix) = match input.rfind('/') {
        Some(i) => input.split_at(i + 1),
        None => ("", input),
    };
    let unchanged = || Completion {
        text: input.to_string(),
        matches: Vec::new(),
    };
    let home = fs.home();
    let Ok(dir) = expand(head, home.as_deref()) else {
        return unchanged();
    };
    let mut matches: Vec<String> = fs
        .list_dirs(&dir)
        .into_iter()
        .filter(|n| starts_with_ignore_case(n, prefix))
        .filter(|n| prefix.starts_with('.') || !n.starts_with('.'))
        .collect();
    matches.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then(a.cmp(b)));
    match matches.as_slice() {
        [] => unchanged(),
        [only] => Completion {
            text: format!("{head}{only}/"),
            matches,
        },
        _ => {
            let lcp = common_prefix(&matches);
            Completion {
                text: format!("{head}{lcp}"),
                matches,
            }
        }
    }
}

/// Two chars equal ignoring case (Unicode lowercase folding, char by char).
fn same_ignoring_case(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// Whether `name` starts with `prefix`, ignoring case.
fn starts_with_ignore_case(name: &str, prefix: &str) -> bool {
    let mut name = name.chars();
    prefix
        .chars()
        .all(|p| name.next().is_some_and(|n| same_ignoring_case(n, p)))
}

/// The longest prefix every string shares ignoring case, on a char boundary, in the first
/// string's casing.
fn common_prefix(items: &[String]) -> &str {
    let Some(first) = items.first() else {
        return "";
    };
    let mut end = first.len();
    for item in &items[1..] {
        let shared = first
            .char_indices()
            .zip(item.chars())
            .take_while(|((_, a), b)| same_ignoring_case(*a, *b))
            .last()
            .map_or(0, |((i, a), _)| i + a.len_utf8());
        end = end.min(shared);
    }
    &first[..end]
}

/// What a key press asks the popup to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Keep prompting (redraw).
    Continue,
    /// Close the popup without opening anything.
    Cancel,
    /// Open a viewer rooted at this canonical directory.
    Open(PathBuf),
}

/// `Tab` cycling state: repeated presses with several matches step through them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cycle {
    /// The input up to and including the last `/` (the directory being completed in).
    head: String,
    /// The candidate list being cycled; `index` is the one currently in the input.
    matches: Vec<String>,
    index: Option<usize>,
}

/// The popup's state: the edited path, the candidates being shown, and any error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootPicker {
    input: PromptInput,
    cycle: Option<Cycle>,
    error: Option<String>,
}

impl Default for RootPicker {
    fn default() -> Self {
        Self::new()
    }
}

impl RootPicker {
    /// A picker pre-filled with `~/`, cursor at the end.
    pub fn new() -> Self {
        Self {
            input: PromptInput::with_text(INITIAL_INPUT),
            cycle: None,
            error: None,
        }
    }

    /// The current input text.
    pub fn input(&self) -> &str {
        self.input.query()
    }

    /// The cursor position in [`Self::input`] (a byte offset on a char boundary).
    pub fn cursor(&self) -> usize {
        self.input.cursor()
    }

    /// The error from the last `Enter`, if it failed.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The completion candidates on show, and which one the input currently holds.
    pub fn matches(&self) -> (&[String], Option<usize>) {
        self.cycle
            .as_ref()
            .map_or((&[][..], None), |c| (c.matches.as_slice(), c.index))
    }

    /// Record a failed hand-off so the user sees it and can retry or cancel.
    pub fn set_error(&mut self, message: impl Into<String>) {
        self.error = Some(message.into());
    }

    /// Apply one key press.
    pub fn handle_key(&mut self, key: KeyEvent, fs: &dyn PickerFs) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Tab {
            self.tab(fs);
            return Outcome::Continue;
        }
        // Any other key ends a Tab cycle and clears a stale error.
        self.cycle = None;
        self.error = None;
        match key.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Char('c') if ctrl => return Outcome::Cancel,
            KeyCode::Char('u') if ctrl => self.input.clear(),
            KeyCode::Enter => return self.submit(fs),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.move_left(),
            KeyCode::Right => self.input.move_right(),
            KeyCode::Home => self.input.move_home(),
            KeyCode::End => self.input.move_end(),
            KeyCode::Char('a') if ctrl => self.input.move_home(),
            KeyCode::Char('e') if ctrl => self.input.move_end(),
            KeyCode::Char(c) if !ctrl && !c.is_control() => self.input.insert(c),
            _ => {}
        }
        Outcome::Continue
    }

    fn set_input(&mut self, text: String) {
        self.input = PromptInput::with_text(text);
    }

    fn tab(&mut self, fs: &dyn PickerFs) {
        self.error = None;
        // A repeated Tab over several matches steps to the next one.
        if let Some(cycle) = &mut self.cycle
            && cycle.matches.len() > 1
        {
            let next = cycle.index.map_or(0, |i| (i + 1) % cycle.matches.len());
            cycle.index = Some(next);
            let text = format!("{}{}/", cycle.head, cycle.matches[next]);
            self.set_input(text);
            return;
        }
        // Complete the whole input (the cursor lands at the end, like a shell completing the
        // last word).
        let typed = self.input.query().to_string();
        let done = complete(&typed, fs);
        let head = match typed.rfind('/') {
            Some(i) => typed[..=i].to_string(),
            None => String::new(),
        };
        self.cycle = (done.matches.len() > 1).then(|| Cycle {
            head,
            matches: done.matches.clone(),
            index: None,
        });
        self.set_input(done.text);
    }

    fn submit(&mut self, fs: &dyn PickerFs) -> Outcome {
        let home = fs.home();
        let path = match expand(self.input.query(), home.as_deref()) {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(e);
                return Outcome::Continue;
            }
        };
        match fs.canonical_dir(&path) {
            Some(dir) => Outcome::Open(dir),
            None => {
                self.error = Some(format!("not a directory: {}", self.input.query().trim()));
                Outcome::Continue
            }
        }
    }
}

/// The herdr argv that opens a viewer tab rooted at `root`.
///
/// Verified against herdr 0.9.1 (`herdr plugin pane open --help`, and a live popup → tab probe):
/// `plugin pane open --plugin herdr-file-viewer --entrypoint file-viewer --placement tab --focus
/// --env HERDR_FILE_VIEWER_ROOT=<dir>`. No `--cwd`: the manifest's pane command is relative, so
/// a foreign cwd would break or redirect the spawn (#139). `Err` when the path is not UTF-8 (the
/// host seam takes `&str` argv).
pub fn open_args(root: &Path) -> Result<Vec<String>, String> {
    let root = root
        .to_str()
        .ok_or_else(|| "path is not valid UTF-8".to_string())?;
    Ok([
        "plugin",
        "pane",
        "open",
        "--plugin",
        "herdr-file-viewer",
        "--entrypoint",
        "file-viewer",
        "--placement",
        "tab",
        "--focus",
        "--env",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([format!("{}={root}", crate::host::ROOT_ENV)])
    .collect())
}

/// Ask herdr to open the viewer at `root`.
pub fn open_viewer(herdr: &dyn HerdrCli, root: &Path) -> Result<(), String> {
    let args = open_args(root)?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    herdr
        .run(&args)
        .map_err(|_| "herdr could not open the file viewer".to_string())
}

/// Drive the picker until the user opens a directory or cancels. The loop is split from
/// [`run`] so a test could drive it with scripted events; `next_key` returns `None` at EOF.
pub fn drive(
    picker: &mut RootPicker,
    fs: &dyn PickerFs,
    herdr: &dyn HerdrCli,
    mut draw: impl FnMut(&RootPicker) -> io::Result<()>,
    mut next_key: impl FnMut() -> io::Result<Option<KeyEvent>>,
) -> io::Result<()> {
    loop {
        draw(picker)?;
        let Some(key) = next_key()? else {
            return Ok(());
        };
        match picker.handle_key(key, fs) {
            Outcome::Continue => {}
            Outcome::Cancel => return Ok(()),
            Outcome::Open(dir) => match open_viewer(herdr, &dir) {
                Ok(()) => return Ok(()),
                Err(e) => picker.set_error(e),
            },
        }
    }
}

/// The `--pick-root` entry point: run the popup on the real terminal.
pub fn run() -> io::Result<()> {
    let mut terminal = ratatui::try_init()?;
    let mut picker = RootPicker::new();
    let herdr = crate::herdr::LiveHerdr::from_env();
    let outcome = drive(
        &mut picker,
        &RealFs,
        &herdr,
        |p| terminal.draw(|f| draw(f, p)).map(|_| ()),
        || loop {
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                return Ok(Some(key));
            }
        },
    );
    ratatui::try_restore()?;
    outcome
}

/// Draw the picker: a bordered box with the prompt, a status line, and the candidates.
pub fn draw(frame: &mut Frame, picker: &RootPicker) {
    let area = frame.area();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Open file viewer at ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // The prompt row, scrolled so the cursor stays visible in a narrow popup.
    let marker = "> ";
    let avail = (inner.width as usize)
        .saturating_sub(marker.len() + 1)
        .max(1);
    let before: Vec<char> = picker.input()[..picker.cursor()].chars().collect();
    let skip = before.len().saturating_sub(avail);
    let shown: String = picker.input().chars().skip(skip).take(avail + 1).collect();
    let prompt = Line::from(vec![
        Span::styled(marker, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(shown),
    ]);
    frame.render_widget(Paragraph::new(prompt), Rect { height: 1, ..inner });
    let cursor_x = inner.x + (marker.len() + before.len() - skip) as u16;
    frame.set_cursor_position((cursor_x.min(inner.right().saturating_sub(1)), inner.y));

    let mut rows: Vec<Line> = Vec::new();
    rows.push(match picker.error() {
        Some(e) => Line::styled(
            e.to_string(),
            Style::default().fg(ratatui::style::Color::Red),
        ),
        None => Line::styled(
            "Tab complete · Enter open · Esc cancel",
            Style::default().add_modifier(Modifier::DIM),
        ),
    });
    let (matches, current) = picker.matches();
    for (i, m) in matches.iter().enumerate() {
        let style = if Some(i) == current {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        rows.push(Line::styled(format!("  {m}/"), style));
    }
    if inner.height > 1 {
        let rest = Rect {
            y: inner.y + 1,
            height: inner.height - 1,
            ..inner
        };
        frame.render_widget(Paragraph::new(rows), rest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// An in-memory tree: each key is a directory, its value the directories inside it.
    struct FakeFs {
        home: Option<PathBuf>,
        dirs: BTreeMap<PathBuf, Vec<&'static str>>,
    }

    impl FakeFs {
        fn new() -> Self {
            let mut dirs = BTreeMap::new();
            dirs.insert(
                PathBuf::from("/home/u"),
                vec!["Workspace", "Work", "Documents", ".config"],
            );
            dirs.insert(
                PathBuf::from("/home/u/Workspace"),
                vec!["herdr-file-viewer", "herdr-tsk", "patch"],
            );
            dirs.insert(PathBuf::from("/home/u/Work"), vec![]);
            dirs.insert(PathBuf::from("/home/u/Documents"), vec![]);
            dirs.insert(PathBuf::from("/home/u/.config"), vec![]);
            dirs.insert(PathBuf::from("/home/u/Workspace/patch"), vec![]);
            dirs.insert(PathBuf::from("/"), vec!["home", "opt"]);
            dirs.insert(PathBuf::from("/opt"), vec!["tools"]);
            Self {
                home: Some(PathBuf::from("/home/u")),
                dirs,
            }
        }
    }

    impl PickerFs for FakeFs {
        fn home(&self) -> Option<PathBuf> {
            self.home.clone()
        }
        fn list_dirs(&self, dir: &Path) -> Vec<String> {
            self.dirs
                .get(dir)
                .map(|v| v.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default()
        }
        fn canonical_dir(&self, path: &Path) -> Option<PathBuf> {
            // Strip a trailing slash the way canonicalize would.
            let p = PathBuf::from(path.to_string_lossy().trim_end_matches('/'));
            let p = if p.as_os_str().is_empty() {
                PathBuf::from("/")
            } else {
                p
            };
            self.dirs.contains_key(&p).then_some(p)
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }
    fn typed(p: &mut RootPicker, fs: &FakeFs, s: &str) {
        for c in s.chars() {
            assert_eq!(p.handle_key(key(KeyCode::Char(c)), fs), Outcome::Continue);
        }
    }

    #[test]
    fn starts_prefilled_with_home() {
        let p = RootPicker::new();
        assert_eq!(p.input(), "~/");
        assert_eq!(p.cursor(), 2);
    }

    #[test]
    fn expand_resolves_home_relative_and_absolute_forms() {
        let home = Some(Path::new("/home/u"));
        assert_eq!(expand("", home), Ok(PathBuf::from("/home/u")));
        assert_eq!(expand("~", home), Ok(PathBuf::from("/home/u")));
        assert_eq!(expand("~/", home), Ok(PathBuf::from("/home/u")));
        assert_eq!(expand("~/a/b", home), Ok(PathBuf::from("/home/u/a/b")));
        assert_eq!(expand("a/b", home), Ok(PathBuf::from("/home/u/a/b")));
        assert_eq!(expand("  /opt/x ", home), Ok(PathBuf::from("/opt/x")));
        assert!(expand("~bob/x", home).is_err());
        // An absolute path needs no home; everything else does.
        assert_eq!(expand("/opt", None), Ok(PathBuf::from("/opt")));
        assert!(expand("~/x", None).is_err());
    }

    #[test]
    fn enter_on_the_prefill_opens_home() {
        let fs = FakeFs::new();
        let mut p = RootPicker::new();
        assert_eq!(
            p.handle_key(key(KeyCode::Enter), &fs),
            Outcome::Open(PathBuf::from("/home/u"))
        );
    }

    #[test]
    fn enter_opens_a_typed_directory_and_rejects_a_missing_one() {
        let fs = FakeFs::new();
        let mut p = RootPicker::new();
        typed(&mut p, &fs, "Workspace/patch");
        assert_eq!(
            p.handle_key(key(KeyCode::Enter), &fs),
            Outcome::Open(PathBuf::from("/home/u/Workspace/patch"))
        );

        let mut p = RootPicker::new();
        typed(&mut p, &fs, "nope");
        assert_eq!(p.handle_key(key(KeyCode::Enter), &fs), Outcome::Continue);
        assert_eq!(p.error(), Some("not a directory: ~/nope"));
        // Editing clears the error.
        p.handle_key(key(KeyCode::Backspace), &fs);
        assert_eq!(p.error(), None);
    }

    #[test]
    fn esc_and_ctrl_c_cancel() {
        let fs = FakeFs::new();
        assert_eq!(
            RootPicker::new().handle_key(key(KeyCode::Esc), &fs),
            Outcome::Cancel
        );
        assert_eq!(
            RootPicker::new().handle_key(ctrl('c'), &fs),
            Outcome::Cancel
        );
    }

    #[test]
    fn ctrl_u_clears_so_an_absolute_path_can_be_typed() {
        let fs = FakeFs::new();
        let mut p = RootPicker::new();
        p.handle_key(ctrl('u'), &fs);
        assert_eq!(p.input(), "");
        typed(&mut p, &fs, "/opt");
        assert_eq!(
            p.handle_key(key(KeyCode::Enter), &fs),
            Outcome::Open(PathBuf::from("/opt"))
        );
    }

    #[test]
    fn tab_completes_a_unique_match_with_a_trailing_slash() {
        let fs = FakeFs::new();
        let mut p = RootPicker::new();
        typed(&mut p, &fs, "Wo");
        p.handle_key(key(KeyCode::Tab), &fs);
        // Work + Workspace share "Work".
        assert_eq!(p.input(), "~/Work");
        typed(&mut p, &fs, "s");
        p.handle_key(key(KeyCode::Tab), &fs);
        assert_eq!(p.input(), "~/Workspace/");
        assert_eq!(p.matches().0, &[] as &[String]);
    }

    #[test]
    fn repeated_tab_cycles_through_several_matches() {
        let fs = FakeFs::new();
        let mut p = RootPicker::new();
        typed(&mut p, &fs, "Workspace/herdr");
        p.handle_key(key(KeyCode::Tab), &fs);
        // Already at the common prefix "herdr-": listed, not yet chosen.
        assert_eq!(p.input(), "~/Workspace/herdr-");
        assert_eq!(
            p.matches(),
            (
                &["herdr-file-viewer".to_string(), "herdr-tsk".to_string()][..],
                None
            )
        );
        p.handle_key(key(KeyCode::Tab), &fs);
        assert_eq!(p.input(), "~/Workspace/herdr-file-viewer/");
        p.handle_key(key(KeyCode::Tab), &fs);
        assert_eq!(p.input(), "~/Workspace/herdr-tsk/");
        p.handle_key(key(KeyCode::Tab), &fs);
        assert_eq!(p.input(), "~/Workspace/herdr-file-viewer/");
        // Typing ends the cycle.
        typed(&mut p, &fs, "x");
        assert_eq!(p.matches().0.len(), 0);
    }

    #[test]
    fn completion_ignores_case_and_keeps_the_real_casing() {
        let fs = FakeFs::new();
        // `wor` matches Work + Workspace; the common prefix takes the on-disk casing.
        let c = complete("~/wor", &fs);
        assert_eq!(c.text, "~/Work");
        assert_eq!(c.matches, ["Work", "Workspace"]);
        // A unique case-insensitive match completes in full.
        assert_eq!(complete("~/Workspace/PAT", &fs).text, "~/Workspace/patch/");
        assert_eq!(complete("~/DOC", &fs).text, "~/Documents/");
        // Hidden directories still need a leading dot.
        assert_eq!(complete("~/.CON", &fs).text, "~/.config/");
    }

    #[test]
    fn hidden_directories_complete_only_when_asked_for() {
        let fs = FakeFs::new();
        let all = complete("~/", &fs);
        assert!(!all.matches.contains(&".config".to_string()), "{all:?}");
        assert_eq!(complete("~/.c", &fs).text, "~/.config/");
    }

    #[test]
    fn tab_with_no_match_leaves_the_input_alone() {
        let fs = FakeFs::new();
        let mut p = RootPicker::new();
        typed(&mut p, &fs, "zzz");
        p.handle_key(key(KeyCode::Tab), &fs);
        assert_eq!(p.input(), "~/zzz");
        // Absolute paths complete too.
        assert_eq!(complete("/op", &fs).text, "/opt/");
        assert_eq!(complete("/opt/t", &fs).text, "/opt/tools/");
    }

    #[test]
    fn common_prefix_is_char_boundary_safe() {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(common_prefix(&v(&["abc", "abd"])), "ab");
        assert_eq!(common_prefix(&v(&["é1", "é2"])), "é");
        assert_eq!(common_prefix(&v(&["a", "b"])), "");
        assert_eq!(common_prefix(&v(&["same", "same"])), "same");
        assert_eq!(common_prefix(&v(&["long", "lo"])), "lo");
        // Case is ignored; the first string's casing is kept.
        assert_eq!(common_prefix(&v(&["Docs", "docs-old"])), "Docs");
        assert_eq!(common_prefix(&v(&["Élan", "éte"])), "É");
    }

    /// Records every herdr argv; optionally fails.
    struct FakeHerdr {
        calls: RefCell<Vec<Vec<String>>>,
        fail: bool,
    }
    impl HerdrCli for FakeHerdr {
        fn run_json(&self, args: &[&str]) -> io::Result<String> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|s| s.to_string()).collect());
            if self.fail {
                Err(io::Error::other("boom"))
            } else {
                Ok(String::new())
            }
        }
    }

    #[test]
    fn open_args_hand_the_root_over_by_env_never_cwd() {
        let args = open_args(Path::new("/home/u/Workspace/patch")).unwrap();
        assert_eq!(
            args,
            [
                "plugin",
                "pane",
                "open",
                "--plugin",
                "herdr-file-viewer",
                "--entrypoint",
                "file-viewer",
                "--placement",
                "tab",
                "--focus",
                "--env",
                "HERDR_FILE_VIEWER_ROOT=/home/u/Workspace/patch",
            ]
        );
        assert!(!args.iter().any(|a| a == "--cwd"));
    }

    #[test]
    fn drive_opens_once_then_exits() {
        let fs = FakeFs::new();
        let herdr = FakeHerdr {
            calls: RefCell::new(Vec::new()),
            fail: false,
        };
        let mut keys = vec![key(KeyCode::Enter)].into_iter();
        let mut p = RootPicker::new();
        drive(&mut p, &fs, &herdr, |_| Ok(()), || Ok(keys.next())).unwrap();
        let calls = herdr.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].last().unwrap(), "HERDR_FILE_VIEWER_ROOT=/home/u");
    }

    #[test]
    fn drive_cancel_never_calls_herdr() {
        let fs = FakeFs::new();
        let herdr = FakeHerdr {
            calls: RefCell::new(Vec::new()),
            fail: false,
        };
        let mut keys = vec![key(KeyCode::Esc), key(KeyCode::Enter)].into_iter();
        let mut p = RootPicker::new();
        drive(&mut p, &fs, &herdr, |_| Ok(()), || Ok(keys.next())).unwrap();
        assert!(herdr.calls.borrow().is_empty());
    }

    #[test]
    fn drive_keeps_the_popup_open_with_an_error_when_herdr_fails() {
        let fs = FakeFs::new();
        let herdr = FakeHerdr {
            calls: RefCell::new(Vec::new()),
            fail: true,
        };
        // Enter fails, then EOF ends the loop.
        let mut keys = vec![key(KeyCode::Enter)].into_iter();
        let mut p = RootPicker::new();
        drive(&mut p, &fs, &herdr, |_| Ok(()), || Ok(keys.next())).unwrap();
        assert_eq!(p.error(), Some("herdr could not open the file viewer"));
    }
}
