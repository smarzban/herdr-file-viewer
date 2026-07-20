//! Launch **open target**: parse a `path` / `path:line` string and resolve it under the tree
//! **root**. Pure: no I/O beyond what the caller does when applying the result.
//!
//! Accepts the same shape **line reference** copies (`L`): repo-relative path, optionally with a
//! 1-based line or `start-end` range (range → jump to the start line; a success notice echoes the
//! full reference). Used once at startup from `--open` / `HERDR_FILE_VIEWER_OPEN`; not a sticky
//! config setting.

use std::path::{Component, Path, PathBuf};

/// A parsed launch open target: the path string (as given, path part only) and an optional
/// 1-based source line (or inclusive range) to jump to after the file renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTarget {
    /// Path part only (no `:line` suffix). Relative to the tree **root**, or absolute under it.
    pub path: String,
    /// 1-based source line to land on, when present (range start when [`end_line`] is set).
    pub line: Option<usize>,
    /// Inclusive range end when the target was `path:start-end` and `end != start`.
    /// `None` for path-only or a single line. Jump still uses [`line`].
    pub end_line: Option<usize>,
}

impl OpenTarget {
    /// Display form matching a **line reference**: `path`, `path:N`, or `path:A-B` (ascending).
    pub fn display_ref(&self) -> String {
        match (self.line, self.end_line) {
            (Some(start), Some(end)) if start != end => {
                let (lo, hi) = if start <= end {
                    (start, end)
                } else {
                    (end, start)
                };
                format!("{}:{lo}-{hi}", self.path)
            }
            (Some(n), _) => format!("{}:{n}", self.path),
            _ => self.path.clone(),
        }
    }

    /// Line to scroll to (range start), if any.
    pub fn goto_line(&self) -> Option<usize> {
        self.line
    }
}

/// What the binary should do after parsing argv (pure; no I/O, never exits).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    /// Print a split-launcher decision from stdin JSON, then exit.
    LaunchDecision,
    /// Print a tab-launcher decision from stdin JSON, then exit.
    LaunchDecisionTab,
    /// Start the TUI; `open` is the raw `--open` value when present (env is layered in `app::run`).
    Run { open: Option<String> },
}

/// Parse process argv (excluding argv[0]) into a [`CliAction`].
///
/// Degrades, never fails:
/// - unknown flags are ignored (herdr may append args we do not control)
/// - a bare `--open` with no value is ignored (start with no open target)
/// - `--launch-decision` / `--launch-decision-tab` win over a normal run (and over `--open`)
///
/// `--open` values must not look like flags (`-…`); a following `-x` is left for the next
/// iteration so it can be ignored as unknown rather than treated as a path.
pub fn parse_args<I, S>(args: I) -> CliAction
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut open_flag: Option<String> = None;
    let mut launch_tab = false;
    let mut launch = false;
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        let arg = arg.as_ref();
        match arg {
            "--launch-decision" => {
                launch = true;
                launch_tab = false;
            }
            "--launch-decision-tab" => {
                launch = true;
                launch_tab = true;
            }
            "--open" => {
                let take = args
                    .peek()
                    .map(|s| {
                        let s = s.as_ref();
                        !s.is_empty() && !s.starts_with('-')
                    })
                    .unwrap_or(false);
                if take {
                    open_flag = Some(args.next().unwrap().as_ref().to_string());
                }
                // else: bare `--open` or `--open -something` → no open target
            }
            a if let Some(v) = a.strip_prefix("--open=")
                && !v.is_empty() =>
            {
                open_flag = Some(v.to_string());
            }
            _ => {
                // Unknown: ignore (degrade-don't-die).
            }
        }
    }
    if launch {
        if launch_tab {
            CliAction::LaunchDecisionTab
        } else {
            CliAction::LaunchDecision
        }
    } else {
        CliAction::Run { open: open_flag }
    }
}

/// Parse a raw open-target string into path + optional line/range.
///
/// - Empty / whitespace-only → `None` (caller treats as "no open target").
/// - `path` → path only.
/// - `path:N` (N ≥ 1) → path + line N.
/// - `path:A-B` (A,B ≥ 1) → path + range A..B (jump uses start; notice shows the range).
/// - A trailing `:` whose suffix is not a line/range is left on the path (e.g. odd filenames).
///
/// Windows drive letters (`C:\…`) are fine: only a suffix of digits (or `digits-digits`) after the
/// *last* colon is treated as a line.
pub fn parse_open_target(raw: &str) -> Option<OpenTarget> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some((path, suffix)) = raw.rsplit_once(':')
        && !path.is_empty()
        && let Some((start, end)) = parse_line_suffix(suffix)
    {
        return Some(OpenTarget {
            path: path.to_string(),
            line: Some(start),
            end_line: end,
        });
    }
    Some(OpenTarget {
        path: raw.to_string(),
        line: None,
        end_line: None,
    })
}

/// `N` → `(N, None)`; `A-B` → `(A, Some(B))` when both ≥ 1 and A ≠ B; `A-A` → single line.
/// `None` if not a line/range suffix.
fn parse_line_suffix(suffix: &str) -> Option<(usize, Option<usize>)> {
    if let Ok(n) = suffix.parse::<usize>() {
        return (n >= 1).then_some((n, None));
    }
    let (a, b) = suffix.split_once('-')?;
    let start = a.parse::<usize>().ok()?;
    let end = b.parse::<usize>().ok()?;
    if start >= 1 && end >= 1 {
        if start == end {
            Some((start, None))
        } else {
            Some((start, Some(end)))
        }
    } else {
        None
    }
}

/// Lexically resolve `.` and `..` with no filesystem I/O. Returns `None` if a `..` would climb
/// above the path's root (absolute) or empty base (relative) — i.e. the path is not well-formed
/// under a fixed root after join.
fn lexically_normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                out.push(c.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                // Pop one normal component; refuse to climb above a root / empty base.
                match out.components().next_back() {
                    Some(Component::Normal(_)) => {
                        out.pop();
                    }
                    _ => return None,
                }
            }
        }
    }
    Some(out)
}

/// Resolve the path part under `root`: relative paths join; absolute paths must stay under root.
/// Lexically normalizes `.` / `..` **before** the containment check (AC-N5) so agent-joined path
/// fragments and escape attempts are handled correctly. Returns `None` when the path would escape
/// the tree. Does **not** check that the file exists — that is the caller's job via
/// `TreeModel::reveal`. Pure: no filesystem I/O (do not `canonicalize`).
pub fn resolve_under_root(root: &Path, path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    let abs = lexically_normalize(&joined)?;
    let root_norm = lexically_normalize(root).unwrap_or_else(|| root.to_path_buf());
    abs.starts_with(&root_norm).then_some(abs)
}

/// Precedence for the raw launch string: CLI `--open` value wins over the env var; empty
/// strings on either side are ignored. Pure (no `std::env`).
pub fn pick_raw_open(flag: Option<&str>, env: Option<&str>) -> Option<String> {
    flag.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            env.map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
}

/// Env var name companions / herdr `--env` should set for a launch open target.
pub const OPEN_ENV: &str = "HERDR_FILE_VIEWER_OPEN";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parse_empty_is_none() {
        assert_eq!(parse_open_target(""), None);
        assert_eq!(parse_open_target("   "), None);
    }

    #[test]
    fn parse_path_only() {
        assert_eq!(
            parse_open_target("src/app.rs"),
            Some(OpenTarget {
                path: "src/app.rs".into(),
                line: None,
                end_line: None,
            })
        );
    }

    #[test]
    fn parse_path_with_line() {
        assert_eq!(
            parse_open_target("src/app.rs:42"),
            Some(OpenTarget {
                path: "src/app.rs".into(),
                line: Some(42),
                end_line: None,
            })
        );
    }

    #[test]
    fn parse_path_with_range_keeps_both_ends() {
        assert_eq!(
            parse_open_target("src/app.rs:10-20"),
            Some(OpenTarget {
                path: "src/app.rs".into(),
                line: Some(10),
                end_line: Some(20),
            })
        );
        assert_eq!(
            parse_open_target("src/app.rs:10-20").unwrap().display_ref(),
            "src/app.rs:10-20"
        );
        assert_eq!(
            parse_open_target("src/app.rs:10-20").unwrap().goto_line(),
            Some(10)
        );
    }

    #[test]
    fn parse_line_zero_stays_on_path() {
        assert_eq!(
            parse_open_target("src/app.rs:0"),
            Some(OpenTarget {
                path: "src/app.rs:0".into(),
                line: None,
                end_line: None,
            })
        );
    }

    #[test]
    fn parse_non_numeric_suffix_stays_on_path() {
        assert_eq!(
            parse_open_target("src/foo:bar.rs"),
            Some(OpenTarget {
                path: "src/foo:bar.rs".into(),
                line: None,
                end_line: None,
            })
        );
    }

    #[test]
    fn parse_windows_drive_without_line() {
        assert_eq!(
            parse_open_target(r"C:\work\src\app.rs"),
            Some(OpenTarget {
                path: r"C:\work\src\app.rs".into(),
                line: None,
                end_line: None,
            })
        );
    }

    #[test]
    fn parse_windows_drive_with_line() {
        assert_eq!(
            parse_open_target(r"C:\work\src\app.rs:42"),
            Some(OpenTarget {
                path: r"C:\work\src\app.rs".into(),
                line: Some(42),
                end_line: None,
            })
        );
    }

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(
            parse_open_target("  src/a.rs:3  "),
            Some(OpenTarget {
                path: "src/a.rs".into(),
                line: Some(3),
                end_line: None,
            })
        );
    }

    #[test]
    fn display_ref_normalizes_descending_range() {
        let t = OpenTarget {
            path: "a.rs".into(),
            line: Some(20),
            end_line: Some(10),
        };
        assert_eq!(t.display_ref(), "a.rs:10-20");
        assert_eq!(t.goto_line(), Some(20));
    }

    #[test]
    fn resolve_relative_joins_root() {
        let root = Path::new("/repo");
        assert_eq!(
            resolve_under_root(root, "src/a.rs"),
            Some(PathBuf::from("/repo/src/a.rs"))
        );
    }

    #[test]
    fn resolve_absolute_under_root_ok() {
        let root = Path::new("/repo");
        assert_eq!(
            resolve_under_root(root, "/repo/src/a.rs"),
            Some(PathBuf::from("/repo/src/a.rs"))
        );
    }

    #[test]
    fn resolve_absolute_outside_root_rejected() {
        let root = Path::new("/repo");
        assert_eq!(resolve_under_root(root, "/other/a.rs"), None);
    }

    #[test]
    fn resolve_dotdot_escape_rejected() {
        let root = Path::new("/repo");
        assert_eq!(resolve_under_root(root, "../../../etc/passwd"), None);
        assert_eq!(resolve_under_root(root, "/repo/../../../etc/passwd"), None);
    }

    #[test]
    fn resolve_normalizes_dotdot_under_root() {
        let root = Path::new("/repo");
        assert_eq!(
            resolve_under_root(root, "src/../src/app.rs"),
            Some(PathBuf::from("/repo/src/app.rs"))
        );
        assert_eq!(
            resolve_under_root(root, "src/../src/app.rs"),
            resolve_under_root(root, "src/app.rs")
        );
    }

    #[test]
    fn resolve_sibling_prefix_not_under_root() {
        // Path component starts_with: /repo-evil is not under /repo.
        let root = Path::new("/repo");
        assert_eq!(resolve_under_root(root, "/repo-evil/a.rs"), None);
    }

    #[test]
    fn pick_raw_flag_wins_over_env() {
        assert_eq!(
            pick_raw_open(Some("from-flag"), Some("from-env")),
            Some("from-flag".into())
        );
    }

    #[test]
    fn pick_raw_empty_flag_falls_to_env() {
        assert_eq!(
            pick_raw_open(Some("  "), Some("from-env")),
            Some("from-env".into())
        );
    }

    #[test]
    fn pick_raw_both_empty_is_none() {
        assert_eq!(pick_raw_open(Some(""), Some("")), None);
        assert_eq!(pick_raw_open(None, None), None);
    }

    #[test]
    fn parse_args_open_space_separated() {
        assert_eq!(
            parse_args(["--open", "src/a.rs:1"]),
            CliAction::Run {
                open: Some("src/a.rs:1".into())
            }
        );
    }

    #[test]
    fn parse_args_open_equals() {
        assert_eq!(
            parse_args(["--open=src/a.rs:2"]),
            CliAction::Run {
                open: Some("src/a.rs:2".into())
            }
        );
    }

    #[test]
    fn parse_args_bare_open_is_ignored() {
        assert_eq!(parse_args(["--open"]), CliAction::Run { open: None });
    }

    #[test]
    fn parse_args_unknown_flag_ignored() {
        assert_eq!(
            parse_args(["--herdr-future-flag", "x"]),
            CliAction::Run { open: None }
        );
    }

    #[test]
    fn parse_args_unknown_alongside_open_keeps_target() {
        assert_eq!(
            parse_args(["--weird", "--open", "src/a.rs", "--also-weird"]),
            CliAction::Run {
                open: Some("src/a.rs".into())
            }
        );
    }

    #[test]
    fn parse_args_launch_decision() {
        assert_eq!(parse_args(["--launch-decision"]), CliAction::LaunchDecision);
        assert_eq!(
            parse_args(["--launch-decision-tab"]),
            CliAction::LaunchDecisionTab
        );
    }

    #[test]
    fn parse_args_launch_decision_wins_over_open() {
        assert_eq!(
            parse_args(["--open", "src/a.rs", "--launch-decision"]),
            CliAction::LaunchDecision
        );
    }

    #[test]
    fn parse_args_open_then_flag_not_eaten_as_path() {
        // `--open --nope` must not treat `--nope` as the path.
        assert_eq!(
            parse_args(["--open", "--nope"]),
            CliAction::Run { open: None }
        );
    }
}
