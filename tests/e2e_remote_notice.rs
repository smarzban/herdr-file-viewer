//! e2e (pty): cached remote notice content is display-only in the real viewer process.
//!
//! Unix-only: see `tests/cli_smoke.rs` for why this `expectrl`-pty e2e suite is not ported to
//! Windows's `conpty` backend in this feature.
#![cfg(unix)]

mod common;

use common::{
    TempDir, git, init_repo_with_commit, viewer_command_with_notices, workspace_fingerprint,
};
use expectrl::process::unix::{PtyStream, UnixProcess, WaitStatus};
use expectrl::process::{NonBlocking, Process};
use expectrl::{Eof, Expect, Session};
use herdr_file_viewer::update::cache::{self, Cache, PersistedReleaseDetails};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const URL_MARKER: &str = "https://example.invalid/hfv.pkg";
const INSTALLER_MARKER: &str = "herdr-file-viewer-9.2.0.pkg";
const RELEASE_DETAILS_MARKER: &str = "RELEASE_DETAILS_VISIBLE";
const SPOTLIGHT_MARKER: &str = "REMOTE_SPOTLIGHT_VISIBLE";
const OSC_52_PREFIX: &[u8] = b"\x1b]52;";
const OSC_52_PAYLOAD: &[u8] = b"c3RvbGVu";

#[derive(Clone)]
struct Transcript(Arc<Mutex<Vec<u8>>>);

/// Records only raw PTY reads, not expectrl's human-readable debug log format.
struct TranscriptStream {
    stream: PtyStream,
    transcript: Transcript,
}

impl TranscriptStream {
    fn new(stream: PtyStream, transcript: Transcript) -> Self {
        Self { stream, transcript }
    }
}

impl Read for TranscriptStream {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = self.stream.read(bytes)?;
        self.transcript
            .0
            .lock()
            .expect("transcript lock")
            .extend_from_slice(&bytes[..count]);
        Ok(count)
    }
}

impl Write for TranscriptStream {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

impl NonBlocking for TranscriptStream {
    fn set_blocking(&mut self, on: bool) -> std::io::Result<()> {
        self.stream.set_blocking(on)
    }
}

fn marker_script(dir: &Path, name: &str, marker_env: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!("#!/bin/sh\nprintf '%s\\n' launched > \"${marker_env}\"\n"),
    )
    .expect("write marker script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make marker script executable");
    path
}

fn seed_fresh_notices(cache_dir: &Path) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after unix epoch")
        .as_secs();
    let spotlight = concat!(
        "# Display-only spotlight\n",
        "REMOTE_SPOTLIGHT_VISIBLE\n",
        "URL=https://example.invalid/hfv.pkg\n",
        "INSTALLER=herdr-file-viewer-9.2.0.pkg\n",
        "OSC8_SAFE_BEFORE\x1b]8;;https://evil.invalid/\x1b\\OSC8_SAFE_AFTER\n",
        "OSC52_SAFE_BEFORE\x1b]52;c;c3RvbGVu\x07OSC52_SAFE_AFTER\n",
        "CSI_SAFE_BEFORE\x1b[2JERASE_SAFE_AFTER\x1b[10;10HCURSOR_SAFE_AFTER\n",
        "C1_SAFE_BEFORE\u{009b}31mC1_SAFE_AFTER\u{0085}C1_SAFE_TAIL\n",
    );
    cache::store(
        cache_dir,
        &Cache {
            last_check_unix: now,
            latest_seen: Some("9.2.0".to_owned()),
            release_details: Some(PersistedReleaseDetails {
                release: "9.2.0".to_owned(),
                details: "## [9.2.0]\n- RELEASE_DETAILS_VISIBLE\n".to_owned(),
            }),
            spotlight: Some(spotlight.as_bytes().to_vec()),
            spotlight_retrieved_at_unix: Some(now),
            ..Cache::default()
        },
    );
}

// macOS CI: ignored for the same pty close/timing limitation as `e2e_help`. The remote rendering
// and display-only behavior remain covered cross-platform by T-19's controller/integration safety
// proofs; Linux retains this real-process smoke.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "help-overlay pty timing is unreliable on macOS CI; display-only behavior is cross-platform integration-tested + verified manually"
)]
fn cached_remote_notices_render_without_external_effects_or_workspace_mutation() {
    let workspace = TempDir::new();
    let support = TempDir::new();
    let root = workspace.path();
    init_repo_with_commit(root);
    std::fs::write(root.join("notice.txt"), "workspace bytes\n").expect("write workspace file");
    git(root, &["add", "notice.txt"]);
    git(root, &["commit", "-q", "-m", "remote-notice fixture"]);

    let cache_base = support.path().join("xdg-cache");
    seed_fresh_notices(&cache_base.join("herdr-file-viewer"));

    let markers = support.path().join("markers");
    std::fs::create_dir_all(&markers).expect("create marker directory");
    let editor_marker = markers.join("editor");
    let open_marker = markers.join("open");
    let reveal_marker = markers.join("reveal");
    let editor = marker_script(support.path(), "record-editor.sh", "EDITOR_MARKER");
    let open = marker_script(support.path(), "record-open.sh", "OPEN_MARKER");
    let reveal = marker_script(support.path(), "record-reveal.sh", "REVEAL_MARKER");

    let config_dir = support.path().join("config");
    std::fs::create_dir_all(&config_dir).expect("create config directory");
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            "markdown = \"sh -c cat\"\nopen = \"{}\"\nreveal = \"{}\"\n",
            open.display(),
            reveal.display(),
        ),
    )
    .expect("write trusted test config");

    let before = workspace_fingerprint(root);
    assert!(before.porcelain.is_empty(), "fixture starts clean");

    let mut cmd = viewer_command_with_notices(root);
    cmd.env("XDG_CACHE_HOME", &cache_base)
        .env("HERDR_PLUGIN_CONFIG_DIR", &config_dir)
        .env("EDITOR", &editor)
        .env("EDITOR_MARKER", &editor_marker)
        .env("OPEN_MARKER", &open_marker)
        .env("REVEAL_MARKER", &reveal_marker);
    let transcript = Transcript(Arc::new(Mutex::new(Vec::new())));
    let mut process = UnixProcess::spawn_command(cmd).expect("spawn viewer in a pty");
    let stream = process.open_stream().expect("open viewer pty stream");
    let mut session = Session::new(process, TranscriptStream::new(stream, transcript.clone()))
        .expect("create viewer pty session");
    session.set_expect_timeout(Some(Duration::from_secs(15)));

    session
        .expect("notice.txt")
        .expect("viewer renders its workspace");
    session.send("?").expect("open What's New");
    session
        .expect(RELEASE_DETAILS_MARKER)
        .expect("detected release details are visibly displayed before effect checks");
    session
        .expect(SPOTLIGHT_MARKER)
        .expect("accepted spotlight body is rendered in What's New");
    session
        .expect(URL_MARKER)
        .expect("remote URL-shaped text is visibly displayed before effect checks");
    session
        .expect(INSTALLER_MARKER)
        .expect("remote installer-shaped text is visibly displayed before effect checks");
    session
        .expect("OSC8_SAFE_BEFOREOSC8_SAFE_AFTER")
        .expect("OSC 8 hyperlink bytes are neutralized while surrounding text remains visible");
    session
        .expect("OSC52_SAFE_BEFOREOSC52_SAFE_AFTER")
        .expect("OSC 52 bytes are neutralized while surrounding text remains visible");
    session
        .expect("CSI_SAFE_BEFOREERASE_SAFE_AFTERCURSOR_SAFE_AFTER")
        .expect("cursor and erase controls are neutralized while surrounding text remains visible");
    session
        .expect("C1_SAFE_BEFORE31mC1_SAFE_AFTERC1_SAFE_TAIL")
        .expect("C1 controls are neutralized while surrounding text remains visible");
    session.send("?").expect("close What's New");
    // This is only an input-boundary gap, matching `e2e_help`: a lone modal close key must be
    // read before `u`, never a freshness or network-timing guess.
    std::thread::sleep(Duration::from_millis(150));
    session
        .send("u")
        .expect("dismiss the visible remote notice from the normal viewer");
    session.send("q").expect("close viewer");
    session.expect(Eof).expect("viewer exits cleanly");
    match session.get_process().wait().expect("reap viewer") {
        WaitStatus::Exited(_, code) => assert_eq!(code, 0, "clean exit after notice flow"),
        other => panic!("expected a clean exit, got {other:?}"),
    }

    let output = transcript.0.lock().expect("transcript lock").clone();
    assert!(
        output.contains(&b'\x1b'),
        "the PTY transcript contains raw terminal bytes, including ordinary TUI CSI output: {:?}",
        String::from_utf8_lossy(&output)
    );
    assert!(
        !output
            .windows(OSC_52_PREFIX.len())
            .any(|bytes| bytes == OSC_52_PREFIX)
            && !output
                .windows(OSC_52_PAYLOAD.len())
                .any(|bytes| bytes == OSC_52_PAYLOAD),
        "the viewer emitted no raw OSC 52 clipboard sequence or source payload: {:?}",
        String::from_utf8_lossy(&output)
    );
    assert!(
        !editor_marker.exists() && !open_marker.exists() && !reveal_marker.exists(),
        "Help display and dismissal must not invoke editor, open, or reveal commands"
    );
    assert_eq!(
        workspace_fingerprint(root),
        before,
        "Help display and dismissal preserve workspace bytes, HEAD, porcelain status, and worktree list"
    );
}
