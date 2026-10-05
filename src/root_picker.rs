//! Root Picker — the `--pick-root` popup: ask for a directory, then open the viewer there.
//!
//! Runs inside a herdr popup (the manifest's `root-picker` pane, `placement = "popup"`). The user
//! edits a path pre-filled with `~/`; `Tab` completes directory and file names, `Enter` hands the
//! chosen directory (or a file: its directory, with the file opened) to a fresh viewer — a split beside the invoking pane, or a new tab, per
//! [`PLACEMENT_ENV`] — through `plugin pane open --env HERDR_FILE_VIEWER_ROOT=<dir>` (never
//! `--cwd`: the manifest pane command is relative, #139), and `Esc` / `Ctrl-C` cancel.
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

/// Env var the launcher (`scripts/open-file-viewer-at.sh`) sets on the popup to say where the
/// viewer opens: `tab` for a new tab; anything else (or unset) for a split.
pub const PLACEMENT_ENV: &str = "HERDR_FILE_VIEWER_PICK_PLACEMENT";

/// Where the picked viewer opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// A split beside the pane the popup was summoned from, in the configured `open_direction`
    /// (the same place `prefix+f`'s split goes).
    Split(crate::config::OpenDirection),
    /// A new tab.
    Tab,
}

impl Placement {
    /// Read [`PLACEMENT_ENV`]'s value: `tab` (trimmed, any case) → [`Placement::Tab`], else a
    /// split in `direction`. Defaulting to a split keeps a missing or garbled value harmless.
    pub fn from_env_value(raw: Option<&str>, direction: crate::config::OpenDirection) -> Self {
        match raw.map(str::trim) {
            Some(v) if v.eq_ignore_ascii_case("tab") => Placement::Tab,
            _ => Placement::Split(direction),
        }
    }

    fn title(self) -> &'static str {
        match self {
            Placement::Split(_) => " Open file viewer at (split) ",
            Placement::Tab => " Open file viewer at (new tab) ",
        }
    }
}

/// The filesystem the picker reads. Injected so tests stay hermetic.
pub trait PickerFs {
    /// The user's home directory (`$HOME`), if known.
    fn home(&self) -> Option<PathBuf>;
    /// The entries directly inside `dir` (following symlinks): directories with a trailing `/`,
    /// regular files without. Anything else, and an unreadable `dir`, yields nothing.
    fn list_entries(&self, dir: &Path) -> Vec<String>;
    /// The canonical form of an existing directory (`true`) or regular file (`false`) at `path`.
    fn resolve(&self, path: &Path) -> Option<(PathBuf, bool)>;
}

/// The real filesystem.
pub struct RealFs;

impl PickerFs for RealFs {
    fn home(&self) -> Option<PathBuf> {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
    }

    fn list_entries(&self, dir: &Path) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                // `Path::is_dir` / `is_file` follow symlinks, so symlinked entries complete too.
                let path = e.path();
                if path.is_dir() {
                    Some(format!("{name}/"))
                } else {
                    path.is_file().then_some(name)
                }
            })
            .collect()
    }

    fn resolve(&self, path: &Path) -> Option<(PathBuf, bool)> {
        let canonical = path.canonicalize().ok()?;
        if canonical.is_dir() {
            Some((canonical, true))
        } else {
            canonical.is_file().then_some((canonical, false))
        }
    }
}

/// The typed text with a pasted absolute path honoured: everything up to the last `//` is
/// dropped, so `~//private/tmp` (an absolute path typed or pasted after the `~/` prefill) means
/// `/private/tmp`, as in Emacs's minibuffer. Surrounding whitespace is trimmed.
pub fn normalize(input: &str) -> &str {
    let input = input.trim();
    match input.rfind("//") {
        Some(i) => &input[i + 1..],
        None => input,
    }
}

/// The same path read from `/` instead of `~`: `~/private/tmp`, `private/tmp` → `/private/tmp`.
/// `None` when the input is already absolute, or names home itself (`~`, `~/`, empty).
pub fn from_root(input: &str) -> Option<String> {
    let input = normalize(input);
    if input.starts_with('/') {
        return None;
    }
    let rest = input
        .strip_prefix("~/")
        .or_else(|| (input == "~").then_some(""))
        .unwrap_or(input);
    // `~user` forms are not paths under home; leave them to `expand`'s error.
    if rest.is_empty() || (input.starts_with('~') && !input.starts_with("~/") && input != "~") {
        return None;
    }
    Some(format!("/{rest}"))
}

/// Expand the typed text into a path. `~`, `~/…`, and relative input resolve under `home`;
/// an absolute path (or one pasted after the prefill, see [`normalize`]) is taken as typed.
/// `~user` forms are not supported (`Err`).
pub fn expand(input: &str, home: Option<&Path>) -> Result<PathBuf, String> {
    let input = normalize(input);
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
    /// The part of `text` before the completed segment (up to and including its last `/`), which
    /// `Tab` cycling and the arrow keys append each match to.
    pub head: String,
}

/// Complete the last path segment of `input` against the entries in its parent. Candidates are
/// [`PickerFs::list_entries`] names, so a directory carries its trailing `/` and a file does not.
///
/// - One match → the full name (a directory's trailing `/` included, ready for the next segment).
/// - Several → extended to their longest common prefix, with every match listed.
/// - None → the input is left unchanged and no matches are listed.
///
/// Matching ignores case (`work` completes `Workspace`), and the completed text takes the
/// entry's real casing, so what is opened is the name on disk. Hidden entries are offered
/// only when the typed segment starts with `.`. Matches are sorted case-insensitively so the list
/// (and `Tab` cycling through it) is stable.
///
/// When nothing matches under home, the same path is tried from `/` ([`from_root`]): `~/priv` +
/// `Tab` becomes `/private/`, so a system path does not need a leading `/`.
pub fn complete(input: &str, fs: &dyn PickerFs) -> Completion {
    let done = complete_in(normalize(input), fs);
    if done.matches.is_empty()
        && let Some(rooted) = from_root(input)
    {
        let alt = complete_in(&rooted, fs);
        if !alt.matches.is_empty() {
            return alt;
        }
    }
    done
}

fn complete_in(input: &str, fs: &dyn PickerFs) -> Completion {
    let (head, prefix) = match input.rfind('/') {
        Some(i) => input.split_at(i + 1),
        None => ("", input),
    };
    let unchanged = || Completion {
        text: input.to_string(),
        matches: Vec::new(),
        head: head.to_string(),
    };
    let home = fs.home();
    let Ok(dir) = expand(head, home.as_deref()) else {
        return unchanged();
    };
    let mut matches: Vec<String> = fs
        .list_entries(&dir)
        .into_iter()
        .filter(|n| starts_with_ignore_case(n, prefix))
        .filter(|n| prefix.starts_with('.') || !n.starts_with('.'))
        .collect();
    matches.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then(a.cmp(b)));
    match matches.as_slice() {
        [] => unchanged(),
        [only] => Completion {
            text: format!("{head}{only}"),
            matches,
            head: head.to_string(),
        },
        _ => {
            let lcp = common_prefix(&matches);
            Completion {
                text: format!("{head}{lcp}"),
                matches,
                head: head.to_string(),
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
    /// Open a viewer on this target.
    Open(Target),
}

/// What `Enter` resolved to: the directory the viewer roots at (then the usual worktree
/// resolution), and the file to open in it when a file was picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Canonical directory: the picked directory, or a picked file's parent.
    pub root: PathBuf,
    /// Canonical path of the picked file, when one was picked.
    pub file: Option<PathBuf>,
}

/// `Tab` cycling state: repeated presses with several matches step through them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cycle {
    /// The input up to and including the last `/` (the directory being completed in).
    head: String,
    /// The candidate list being cycled; `index` is the one currently in the input.
    matches: Vec<String>,
    index: Option<usize>,
    /// The highlight was last moved with an arrow key: the next `Tab` completes inside the
    /// highlighted match instead of stepping to the next one.
    arrowed: bool,
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
        // With a match list on show, Up/Down (and Shift-Tab) move through it; the input follows
        // the highlighted match, so Enter opens it and Tab keeps completing inside it. With no
        // list they do nothing (a single-line prompt has nowhere to move).
        match key.code {
            KeyCode::Down => {
                self.step(1, true);
                return Outcome::Continue;
            }
            KeyCode::Up => {
                self.step(-1, true);
                return Outcome::Continue;
            }
            KeyCode::BackTab => {
                self.step(-1, false);
                return Outcome::Continue;
            }
            _ => {}
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

    /// Move the highlight through the match list by `delta` (wrapping) and put that match in the
    /// input. From no highlight, forward lands on the first match and backward on the last.
    /// Returns `false` when there is no list to move through.
    fn step(&mut self, delta: isize, arrowed: bool) -> bool {
        let Some(cycle) = &mut self.cycle else {
            return false;
        };
        let n = cycle.matches.len();
        if n < 2 {
            return false;
        }
        let next = match cycle.index {
            None if delta >= 0 => 0,
            None => n - 1,
            Some(i) => (i as isize + delta).rem_euclid(n as isize) as usize,
        };
        cycle.index = Some(next);
        cycle.arrowed = arrowed;
        let text = format!("{}{}", cycle.head, cycle.matches[next]);
        self.error = None;
        self.set_input(text);
        true
    }

    fn tab(&mut self, fs: &dyn PickerFs) {
        self.error = None;
        // After choosing with an arrow key, Tab completes inside the choice (descending into a
        // directory); otherwise a repeated Tab over several matches steps to the next one.
        if self.cycle.as_ref().is_some_and(|c| c.arrowed) {
            self.cycle = None;
        } else if self.step(1, false) {
            return;
        }
        // Complete the whole input (the cursor lands at the end, like a shell completing the
        // last word).
        let typed = self.input.query().to_string();
        let done = complete(&typed, fs);
        self.cycle = (done.matches.len() > 1).then(|| Cycle {
            head: done.head.clone(),
            matches: done.matches.clone(),
            index: None,
            arrowed: false,
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
        // Home first; if that does not exist, the same path from `/` (no leading `/` needed).
        let found = fs.resolve(&path).or_else(|| {
            from_root(self.input.query()).and_then(|rooted| fs.resolve(Path::new(&rooted)))
        });
        match found {
            Some((dir, true)) => Outcome::Open(Target {
                root: dir,
                file: None,
            }),
            Some((file, false)) => match file.parent() {
                Some(dir) => Outcome::Open(Target {
                    root: dir.to_path_buf(),
                    file: Some(file),
                }),
                None => Outcome::Continue,
            },
            None => {
                self.error = Some(format!("not found: {}", self.input.query().trim()));
                Outcome::Continue
            }
        }
    }
}

/// The herdr argv that opens a viewer on `target`, placed per `placement`.
///
/// Verified against herdr 0.9.1 (`herdr plugin pane open --help`, and live popup → tab and popup →
/// split probes: from a popup, a split with no `--target-pane` lands beside the tiled pane the popup
/// was opened over, and `--env` reaches the popup's own process):
/// `plugin pane open --plugin herdr-file-viewer --entrypoint file-viewer --placement split
/// --direction <right|down> --focus --env HERDR_FILE_VIEWER_ROOT=<dir>`, or `--placement tab` with
/// no `--direction`; a picked file adds `--env HERDR_FILE_VIEWER_OPEN=<file>` (absolute, under the
/// root, which the launch open target accepts). No `--cwd`: the manifest's pane command is relative, so a foreign cwd would
/// break or redirect the spawn (#139). `Err` when the path is not UTF-8 (the host seam takes
/// `&str` argv).
pub fn open_args(target: &Target, placement: Placement) -> Result<Vec<String>, String> {
    let utf8 = |p: &Path| {
        p.to_str()
            .map(str::to_string)
            .ok_or_else(|| "path is not valid UTF-8".to_string())
    };
    let root = utf8(&target.root)?;
    let file = target.file.as_deref().map(utf8).transpose()?;
    let mut args: Vec<String> = [
        "plugin",
        "pane",
        "open",
        "--plugin",
        "herdr-file-viewer",
        "--entrypoint",
        "file-viewer",
        "--placement",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    match placement {
        Placement::Split(direction) => {
            args.extend(["split", "--direction", direction.label()].map(String::from));
        }
        Placement::Tab => args.push("tab".to_string()),
    }
    args.extend(["--focus", "--env"].map(String::from));
    args.push(format!("{}={root}", crate::host::ROOT_ENV));
    if let Some(file) = file {
        args.push("--env".to_string());
        args.push(format!("{}={file}", crate::open_target::OPEN_ENV));
    }
    Ok(args)
}

/// The tab label a picker-opened viewer tab gets, matching the viewer pane's own `Files` title.
pub const TAB_LABEL: &str = "Files";

/// Ask herdr to open the viewer on `target`. A new tab is then renamed [`TAB_LABEL`]
/// (`herdr tab rename <tab_id> Files`, verified on herdr 0.9.1), best-effort: the viewer is already
/// open, so a failed or unparseable rename never turns the hand-off into an error.
pub fn open_viewer(
    herdr: &dyn HerdrCli,
    target: &Target,
    placement: Placement,
) -> Result<(), String> {
    let args = open_args(target, placement)?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let opened = herdr
        .run_json(&args)
        .map_err(|_| "herdr could not open the file viewer".to_string())?;
    if placement == Placement::Tab
        && let Some(tab) = opened_tab_id(&opened)
    {
        let _ = herdr.run(&["tab", "rename", &tab, TAB_LABEL]);
    }
    Ok(())
}

/// The tab id from `plugin pane open`'s reply (`result.plugin_pane.pane.tab_id`), if it is a
/// flag-safe token (it goes straight into an argv).
fn opened_tab_id(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let tab = v
        .pointer("/result/plugin_pane/pane/tab_id")?
        .as_str()?
        .to_string();
    crate::launch::is_flag_safe(&tab).then_some(tab)
}

/// Drive the picker until the user opens a directory or cancels. The loop is split from
/// [`run`] so a test could drive it with scripted events; `next_key` returns `None` at EOF.
pub fn drive(
    picker: &mut RootPicker,
    fs: &dyn PickerFs,
    herdr: &dyn HerdrCli,
    placement: Placement,
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
            Outcome::Open(target) => match open_viewer(herdr, &target, placement) {
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
    // The split direction is the same `open_direction` the split launcher honours; the popup gets
    // the plugin's HERDR_PLUGIN_CONFIG_DIR, so the config resolves exactly as it does there.
    let (config, _) = crate::config::load_config_from_env();
    let eff = crate::config::resolve(&config, |k| std::env::var(k).ok());
    let placement = Placement::from_env_value(
        std::env::var(PLACEMENT_ENV).ok().as_deref(),
        eff.open_direction,
    );
    let outcome = drive(
        &mut picker,
        &RealFs,
        &herdr,
        placement,
        |p| terminal.draw(|f| draw(f, p, placement)).map(|_| ()),
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
pub fn draw(frame: &mut Frame, picker: &RootPicker, placement: Placement) {
    let area = frame.area();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(placement.title());
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
            "Tab complete · ↑↓ choose · Enter open · Esc cancel",
            Style::default().add_modifier(Modifier::DIM),
        ),
    });
    let (matches, current) = picker.matches();
    // Scroll the list so the highlighted match stays visible (row 0 is the status line).
    let list_rows = (inner.height as usize).saturating_sub(2).max(1);
    let offset = current.map_or(0, |c| (c + 1).saturating_sub(list_rows));
    for (i, m) in matches.iter().enumerate().skip(offset) {
        let style = if Some(i) == current {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        rows.push(Line::styled(format!("  {m}"), style));
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

    /// An in-memory tree: `dirs` maps a directory to the directories inside it, `files` to the
    /// regular files inside it.
    struct FakeFs {
        home: Option<PathBuf>,
        dirs: BTreeMap<PathBuf, Vec<&'static str>>,
        files: BTreeMap<PathBuf, Vec<&'static str>>,
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
            dirs.insert(
                PathBuf::from("/many"),
                vec![
                    "d00", "d01", "d02", "d03", "d04", "d05", "d06", "d07", "d08", "d09",
                ],
            );
            let mut files = BTreeMap::new();
            files.insert(
                PathBuf::from("/home/u/Workspace/patch"),
                vec!["starter-config.toml", "notes.md", "notes-old.md"],
            );
            files.insert(PathBuf::from("/home/u"), vec![".zshrc"]);
            Self {
                home: Some(PathBuf::from("/home/u")),
                dirs,
                files,
            }
        }
    }

    impl PickerFs for FakeFs {
        fn home(&self) -> Option<PathBuf> {
            self.home.clone()
        }
        fn list_entries(&self, dir: &Path) -> Vec<String> {
            let dirs = self
                .dirs
                .get(dir)
                .into_iter()
                .flatten()
                .map(|d| format!("{d}/"));
            let files = self
                .files
                .get(dir)
                .into_iter()
                .flatten()
                .map(|f| f.to_string());
            dirs.chain(files).collect()
        }
        fn resolve(&self, path: &Path) -> Option<(PathBuf, bool)> {
            // Strip a trailing slash the way canonicalize would.
            let p = PathBuf::from(path.to_string_lossy().trim_end_matches('/'));
            let p = if p.as_os_str().is_empty() {
                PathBuf::from("/")
            } else {
                p
            };
            if self.dirs.contains_key(&p) {
                return Some((p, true));
            }
            let name = p.file_name()?.to_str()?;
            let in_parent = self.files.get(p.parent()?)?.contains(&name);
            in_parent.then_some((p, false))
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
            Outcome::Open(Target {
                root: PathBuf::from("/home/u"),
                file: None
            })
        );
    }

    #[test]
    fn enter_opens_a_typed_directory_and_rejects_a_missing_one() {
        let fs = FakeFs::new();
        let mut p = RootPicker::new();
        typed(&mut p, &fs, "Workspace/patch");
        assert_eq!(
            p.handle_key(key(KeyCode::Enter), &fs),
            Outcome::Open(Target {
                root: PathBuf::from("/home/u/Workspace/patch"),
                file: None
            })
        );

        let mut p = RootPicker::new();
        typed(&mut p, &fs, "nope");
        assert_eq!(p.handle_key(key(KeyCode::Enter), &fs), Outcome::Continue);
        assert_eq!(p.error(), Some("not found: ~/nope"));
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
            Outcome::Open(Target {
                root: PathBuf::from("/opt"),
                file: None
            })
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
                &["herdr-file-viewer/".to_string(), "herdr-tsk/".to_string()][..],
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
    fn arrows_move_through_the_match_list_and_enter_opens_the_highlight() {
        let fs = FakeFs::new();
        let mut p = RootPicker::new();
        typed(&mut p, &fs, "Workspace/");
        p.handle_key(key(KeyCode::Tab), &fs);
        // Three matches, none highlighted yet.
        assert_eq!(p.matches().0.len(), 3);
        assert_eq!(p.matches().1, None);
        // Up from no highlight wraps to the last; Down from there wraps to the first.
        p.handle_key(key(KeyCode::Up), &fs);
        assert_eq!(p.matches().1, Some(2));
        assert_eq!(p.input(), "~/Workspace/patch/");
        p.handle_key(key(KeyCode::Down), &fs);
        assert_eq!(p.matches().1, Some(0));
        p.handle_key(key(KeyCode::Down), &fs);
        assert_eq!(p.input(), "~/Workspace/herdr-tsk/");
        // Shift-Tab goes back.
        p.handle_key(key(KeyCode::BackTab), &fs);
        assert_eq!(p.input(), "~/Workspace/herdr-file-viewer/");
        // Enter opens what is highlighted (herdr-file-viewer has no fake dir entry, so pick patch).
        p.handle_key(key(KeyCode::Up), &fs);
        assert_eq!(
            p.handle_key(key(KeyCode::Enter), &fs),
            Outcome::Open(Target {
                root: PathBuf::from("/home/u/Workspace/patch"),
                file: None
            })
        );
    }

    #[test]
    fn the_list_scrolls_to_keep_the_highlight_visible() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let fs = FakeFs::new();
        let mut p = RootPicker::new();
        p.handle_key(ctrl('u'), &fs);
        typed(&mut p, &fs, "/many/");
        p.handle_key(key(KeyCode::Tab), &fs);
        // Highlight the last of ten matches.
        p.handle_key(key(KeyCode::Up), &fs);
        // 8 rows: border, prompt, status, 4 list rows, border.
        let mut t = Terminal::new(TestBackend::new(30, 8)).unwrap();
        t.draw(|f| draw(f, &p, Placement::Tab)).unwrap();
        let screen: String = t
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            screen.contains("d09/"),
            "highlight scrolled into view: {screen}"
        );
        assert!(!screen.contains("d00/"), "the top scrolled away: {screen}");
    }

    #[test]
    fn a_pasted_absolute_path_after_the_prefill_is_absolute() {
        assert_eq!(normalize("~//private/tmp"), "/private/tmp");
        assert_eq!(normalize("~/a//b/c"), "/b/c");
        assert_eq!(normalize("  /opt "), "/opt");
        assert_eq!(normalize("~/Work"), "~/Work");
        let fs = FakeFs::new();
        let mut p = RootPicker::new();
        typed(&mut p, &fs, "/opt");
        assert_eq!(p.input(), "~//opt");
        assert_eq!(
            p.handle_key(key(KeyCode::Enter), &fs),
            Outcome::Open(Target {
                root: PathBuf::from("/opt"),
                file: None
            })
        );
    }

    #[test]
    fn from_root_reads_a_home_relative_path_from_slash() {
        assert_eq!(from_root("~/private/tmp").as_deref(), Some("/private/tmp"));
        assert_eq!(from_root("private/tmp").as_deref(), Some("/private/tmp"));
        assert_eq!(from_root("~//opt"), None, "already absolute");
        assert_eq!(from_root("/opt"), None);
        assert_eq!(from_root("~/"), None, "home itself");
        assert_eq!(from_root("~"), None);
        assert_eq!(from_root(""), None);
        assert_eq!(from_root("~bob/x"), None);
    }

    #[test]
    fn a_path_missing_under_home_falls_back_to_slash() {
        let fs = FakeFs::new();
        // ~/opt does not exist, /opt does: Enter opens /opt without a leading slash.
        let mut p = RootPicker::new();
        typed(&mut p, &fs, "opt");
        assert_eq!(
            p.handle_key(key(KeyCode::Enter), &fs),
            Outcome::Open(Target {
                root: PathBuf::from("/opt"),
                file: None
            })
        );
        // Tab does the same and rewrites the line to the absolute path it found.
        let mut p = RootPicker::new();
        typed(&mut p, &fs, "op");
        p.handle_key(key(KeyCode::Tab), &fs);
        assert_eq!(p.input(), "/opt/");
        // Home still wins when it matches: ~/Wo completes under home, not /.
        assert_eq!(complete("~/Wo", &fs).text, "~/Work");
        // Nowhere: unchanged, and Enter reports it.
        let mut p = RootPicker::new();
        typed(&mut p, &fs, "nope");
        p.handle_key(key(KeyCode::Tab), &fs);
        assert_eq!(p.input(), "~/nope");
    }

    #[test]
    fn cycling_after_a_slash_fallback_stays_under_slash() {
        let fs = FakeFs::new();
        let mut p = RootPicker::new();
        typed(&mut p, &fs, "many/d0");
        p.handle_key(key(KeyCode::Tab), &fs);
        assert_eq!(p.input(), "/many/d0");
        p.handle_key(key(KeyCode::Down), &fs);
        assert_eq!(p.input(), "/many/d00/");
    }

    #[test]
    fn tab_after_an_arrow_choice_completes_inside_it() {
        let fs = FakeFs::new();
        let mut p = RootPicker::new();
        typed(&mut p, &fs, "Workspace/");
        p.handle_key(key(KeyCode::Tab), &fs);
        // Arrow up to patch/, then Tab descends into it (its files) instead of cycling.
        p.handle_key(key(KeyCode::Up), &fs);
        assert_eq!(p.input(), "~/Workspace/patch/");
        p.handle_key(key(KeyCode::Tab), &fs);
        assert_eq!(p.input(), "~/Workspace/patch/");
        assert_eq!(
            p.matches().0,
            ["notes-old.md", "notes.md", "starter-config.toml"]
        );
        // Plain repeated Tab (no arrow) still cycles, as in a shell.
        p.handle_key(key(KeyCode::Tab), &fs);
        assert_eq!(p.input(), "~/Workspace/patch/notes-old.md");
    }

    #[test]
    fn arrows_without_a_list_do_nothing() {
        let fs = FakeFs::new();
        let mut p = RootPicker::new();
        typed(&mut p, &fs, "Work");
        p.handle_key(key(KeyCode::Down), &fs);
        p.handle_key(key(KeyCode::Up), &fs);
        assert_eq!(p.input(), "~/Work");
    }

    #[test]
    fn completion_ignores_case_and_keeps_the_real_casing() {
        let fs = FakeFs::new();
        // `wor` matches Work + Workspace; the common prefix takes the on-disk casing.
        let c = complete("~/wor", &fs);
        assert_eq!(c.text, "~/Work");
        assert_eq!(c.matches, ["Work/", "Workspace/"]);
        // A unique case-insensitive match completes in full.
        assert_eq!(complete("~/Workspace/PAT", &fs).text, "~/Workspace/patch/");
        assert_eq!(complete("~/DOC", &fs).text, "~/Documents/");
        // Hidden directories still need a leading dot.
        assert_eq!(complete("~/.CON", &fs).text, "~/.config/");
    }

    #[test]
    fn files_complete_without_a_trailing_slash() {
        let fs = FakeFs::new();
        assert_eq!(
            complete("~/Workspace/patch/sta", &fs).text,
            "~/Workspace/patch/starter-config.toml"
        );
        // Files and directories mix in one listing; directories keep their `/`.
        let c = complete("~/Workspace/patch/notes", &fs);
        assert_eq!(c.text, "~/Workspace/patch/notes");
        assert_eq!(c.matches, ["notes-old.md", "notes.md"]);
        assert_eq!(complete("~/.z", &fs).text, "~/.zshrc");
    }

    #[test]
    fn enter_on_a_file_roots_at_its_directory_and_opens_it() {
        let fs = FakeFs::new();
        let mut p = RootPicker::new();
        typed(&mut p, &fs, "Workspace/patch/starter-config.toml");
        assert_eq!(
            p.handle_key(key(KeyCode::Enter), &fs),
            Outcome::Open(Target {
                root: PathBuf::from("/home/u/Workspace/patch"),
                file: Some(PathBuf::from("/home/u/Workspace/patch/starter-config.toml")),
            })
        );
    }

    #[test]
    fn open_args_add_the_open_target_for_a_picked_file() {
        let target = Target {
            root: PathBuf::from("/tmp/x"),
            file: Some(PathBuf::from("/tmp/x/starter-config.toml")),
        };
        let args = open_args(&target, Placement::Tab).unwrap();
        assert_eq!(
            &args[args.len() - 4..],
            [
                "--env",
                "HERDR_FILE_VIEWER_ROOT=/tmp/x",
                "--env",
                "HERDR_FILE_VIEWER_OPEN=/tmp/x/starter-config.toml",
            ]
        );
    }

    #[test]
    fn hidden_directories_complete_only_when_asked_for() {
        let fs = FakeFs::new();
        let all = complete("~/", &fs);
        assert!(!all.matches.contains(&".config/".to_string()), "{all:?}");
        assert!(!all.matches.contains(&".zshrc".to_string()), "{all:?}");
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
                Ok(OPENED_REPLY.to_string())
            }
        }
    }

    #[test]
    fn open_args_hand_the_root_over_by_env_never_cwd() {
        let target = Target {
            root: PathBuf::from("/home/u/Workspace/patch"),
            file: None,
        };
        let args = open_args(&target, Placement::Tab).unwrap();
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
    fn open_args_split_follows_the_configured_direction() {
        use crate::config::OpenDirection;
        for (dir, label) in [
            (OpenDirection::Right, "right"),
            (OpenDirection::Down, "down"),
        ] {
            let target = Target {
                root: PathBuf::from("/r"),
                file: None,
            };
            let args = open_args(&target, Placement::Split(dir)).unwrap();
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
                    "split",
                    "--direction",
                    label,
                    "--focus",
                    "--env",
                    "HERDR_FILE_VIEWER_ROOT=/r",
                ]
            );
        }
    }

    #[test]
    fn placement_env_selects_tab_else_defaults_to_split() {
        use crate::config::OpenDirection::{Down, Right};
        assert_eq!(PLACEMENT_ENV, "HERDR_FILE_VIEWER_PICK_PLACEMENT");
        assert_eq!(
            Placement::from_env_value(Some("tab"), Right),
            Placement::Tab
        );
        assert_eq!(
            Placement::from_env_value(Some(" TAB "), Right),
            Placement::Tab
        );
        assert_eq!(
            Placement::from_env_value(Some("split"), Down),
            Placement::Split(Down)
        );
        assert_eq!(
            Placement::from_env_value(None, Right),
            Placement::Split(Right)
        );
        assert_eq!(
            Placement::from_env_value(Some("bogus"), Right),
            Placement::Split(Right)
        );
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
        drive(
            &mut p,
            &fs,
            &herdr,
            Placement::Tab,
            |_| Ok(()),
            || Ok(keys.next()),
        )
        .unwrap();
        let calls = herdr.calls.borrow();
        // One open (then the tab rename, covered by its own test).
        assert_eq!(calls[0].last().unwrap(), "HERDR_FILE_VIEWER_ROOT=/home/u");
        assert_eq!(
            calls
                .iter()
                .filter(|c| c[..3] == ["plugin", "pane", "open"])
                .count(),
            1
        );
    }

    /// A trimmed `plugin pane open` reply, shaped like herdr 0.9.1's (`plugin_pane_opened`).
    const OPENED_REPLY: &str = r#"{"id":"cli:plugin","result":{"plugin_pane":{"entrypoint":"file-viewer","pane":{"label":"Files","pane_id":"w39:pX","tab_id":"w39:tH"},"plugin_id":"herdr-file-viewer"},"type":"plugin_pane_opened"}}"#;

    #[test]
    fn a_new_tab_is_renamed_files_and_a_split_is_not() {
        let fs = FakeFs::new();
        for (placement, renamed) in [
            (Placement::Tab, true),
            (Placement::Split(crate::config::OpenDirection::Right), false),
        ] {
            let herdr = FakeHerdr {
                calls: RefCell::new(Vec::new()),
                fail: false,
            };
            let mut keys = vec![key(KeyCode::Enter)].into_iter();
            let mut p = RootPicker::new();
            drive(
                &mut p,
                &fs,
                &herdr,
                placement,
                |_| Ok(()),
                || Ok(keys.next()),
            )
            .unwrap();
            let calls = herdr.calls.borrow();
            if renamed {
                assert_eq!(calls.len(), 2, "{calls:?}");
                assert_eq!(calls[1], ["tab", "rename", "w39:tH", "Files"]);
            } else {
                assert_eq!(calls.len(), 1, "{calls:?}");
            }
        }
    }

    #[test]
    fn opened_tab_id_rejects_missing_or_unsafe_ids() {
        assert_eq!(opened_tab_id(OPENED_REPLY), Some("w39:tH".to_string()));
        assert_eq!(opened_tab_id(""), None);
        assert_eq!(opened_tab_id(r#"{"result":{}}"#), None);
        let hostile = OPENED_REPLY.replace("w39:tH", "--evil");
        assert_eq!(opened_tab_id(&hostile), None);
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
        drive(
            &mut p,
            &fs,
            &herdr,
            Placement::Tab,
            |_| Ok(()),
            || Ok(keys.next()),
        )
        .unwrap();
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
        drive(
            &mut p,
            &fs,
            &herdr,
            Placement::Tab,
            |_| Ok(()),
            || Ok(keys.next()),
        )
        .unwrap();
        assert_eq!(p.error(), Some("herdr could not open the file viewer"));
    }
}
