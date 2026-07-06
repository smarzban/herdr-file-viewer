//! Opener — the read-only OS-opener argv builder (AC-9, AC-10).
//!
//! The pure core of the Opener component: given an [`OsKind`], an [`OpenAction`], and a
//! path, it produces the exact argv to hand a file/dir off to the OS default app or file
//! manager. It is pure — no process spawning, no I/O, no trait — and never mutates the file
//! (AC-N1); a later task wires the spawn. The target OS is an explicit parameter (not
//! `cfg!(target_os)`) so all three platforms are unit-testable on any host, and the path is
//! always carried as a single, un-shell-split argv element to keep spaces and metacharacters
//! literal (AC-9).

use std::ffi::OsString;
use std::path::Path;

/// The target operating system whose opener convention to build for. An explicit parameter
/// (rather than compile-time `cfg!`) so every platform's argv is testable on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsKind {
    Mac,
    Linux,
    Windows,
}

/// What to do with the path: open it in the default app, or reveal it in a file manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAction {
    Open,
    Reveal,
}

/// Build the per-OS argv (argv[0] = program, rest = args) to open or reveal `path`.
///
/// The path is always placed as ONE argv element, never shell-split, so spaces and shell
/// metacharacters stay literal (AC-9). `OsString`s are built directly so non-UTF-8 paths are
/// preserved. On Linux, "reveal" opens the containing folder (there is no universal
/// select-in-file-manager); a path with no parent (e.g. `/`) falls back to itself.
pub fn opener_argv(os: OsKind, action: OpenAction, path: &Path) -> Vec<OsString> {
    match (os, action) {
        (OsKind::Mac, OpenAction::Open) => {
            vec![OsString::from("open"), path.as_os_str().to_owned()]
        }
        (OsKind::Mac, OpenAction::Reveal) => vec![
            OsString::from("open"),
            OsString::from("-R"),
            path.as_os_str().to_owned(),
        ],
        (OsKind::Linux, OpenAction::Open) => {
            vec![OsString::from("xdg-open"), path.as_os_str().to_owned()]
        }
        (OsKind::Linux, OpenAction::Reveal) => {
            let target = path.parent().unwrap_or(path);
            vec![OsString::from("xdg-open"), target.as_os_str().to_owned()]
        }
        (OsKind::Windows, OpenAction::Open) => {
            vec![OsString::from("explorer"), path.as_os_str().to_owned()]
        }
        (OsKind::Windows, OpenAction::Reveal) => {
            let mut s = OsString::from("/select,");
            s.push(path);
            vec![OsString::from("explorer"), s]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_open_argv() {
        let path = Path::new("/abs/dir/file.rs");
        assert_eq!(
            opener_argv(OsKind::Mac, OpenAction::Open, path),
            vec![OsString::from("open"), OsString::from("/abs/dir/file.rs")]
        );
    }

    #[test]
    fn mac_reveal_argv() {
        let path = Path::new("/abs/dir/file.rs");
        assert_eq!(
            opener_argv(OsKind::Mac, OpenAction::Reveal, path),
            vec![
                OsString::from("open"),
                OsString::from("-R"),
                OsString::from("/abs/dir/file.rs"),
            ]
        );
    }

    #[test]
    fn linux_open_argv() {
        let path = Path::new("/abs/dir/file.rs");
        assert_eq!(
            opener_argv(OsKind::Linux, OpenAction::Open, path),
            vec![
                OsString::from("xdg-open"),
                OsString::from("/abs/dir/file.rs")
            ]
        );
    }

    #[test]
    fn linux_reveal_argv_is_parent() {
        let path = Path::new("/abs/dir/file.rs");
        assert_eq!(
            opener_argv(OsKind::Linux, OpenAction::Reveal, path),
            vec![OsString::from("xdg-open"), OsString::from("/abs/dir")]
        );
    }

    #[test]
    fn linux_reveal_parent_fallback_is_self() {
        let path = Path::new("/");
        assert_eq!(
            opener_argv(OsKind::Linux, OpenAction::Reveal, path),
            vec![OsString::from("xdg-open"), OsString::from("/")]
        );
    }

    #[test]
    fn windows_open_argv() {
        let path = Path::new("/abs/dir/file.rs");
        assert_eq!(
            opener_argv(OsKind::Windows, OpenAction::Open, path),
            vec![
                OsString::from("explorer"),
                OsString::from("/abs/dir/file.rs")
            ]
        );
    }

    #[test]
    fn windows_reveal_argv_is_select_prefix() {
        let path = Path::new("/abs/dir/file.rs");
        assert_eq!(
            opener_argv(OsKind::Windows, OpenAction::Reveal, path),
            vec![
                OsString::from("explorer"),
                OsString::from("/select,/abs/dir/file.rs"),
            ]
        );
    }

    #[test]
    fn path_is_single_unmodified_element_open() {
        // Spaces, a leading-dash-looking component, and a shell metacharacter must stay
        // literal and un-split (AC-9). The path is absolute.
        let path = Path::new("/tmp/a dir/-weird;name.txt");
        let expected = path.as_os_str().to_owned();

        for os in [OsKind::Mac, OsKind::Linux, OsKind::Windows] {
            let argv = opener_argv(os, OpenAction::Open, path);
            assert_eq!(argv.len(), 2, "{os:?} Open argv should be [prog, path]");
            assert_eq!(
                argv[1], expected,
                "{os:?} path must be one unmodified element"
            );
        }
    }

    #[test]
    fn windows_reveal_path_stays_single_element() {
        let path = Path::new("/tmp/a dir/-weird;name.txt");
        let argv = opener_argv(OsKind::Windows, OpenAction::Reveal, path);
        assert_eq!(argv.len(), 2);
        assert_eq!(
            argv[1],
            OsString::from("/select,/tmp/a dir/-weird;name.txt")
        );
    }
}
