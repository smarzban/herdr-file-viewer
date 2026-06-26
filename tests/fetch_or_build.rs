//! Hermetic test for scripts/fetch-or-build.sh — the install-time [[build]] step.
//! Stubs uname/curl/cargo via PATH and serves a local fixture "release"; uses the real
//! sha256sum/shasum, mktemp, grep, etc. No network, no real cargo build, no new deps.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

fn script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/fetch-or-build.sh")
}

fn tmp(label: &str) -> PathBuf {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("fv-fob-{}-{}-{}", std::process::id(), label, n));
    fs::create_dir_all(&p).unwrap();
    p
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut perm = fs::metadata(path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(path, perm).unwrap();
}

/// Hex sha-256 of a file using whatever tool exists (matches the script's preference order).
fn sha256_of(file: &Path) -> String {
    if let Ok(out) = Command::new("sha256sum").arg(file).output()
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout);
        return s.split_whitespace().next().unwrap().to_string();
    }
    let out = Command::new("shasum")
        .args(["-a", "256"])
        .arg(file)
        .output()
        .expect("need sha256sum or shasum to run this test");
    let s = String::from_utf8_lossy(&out.stdout);
    s.split_whitespace().next().unwrap().to_string()
}

struct Outcome {
    stdout: String,
    stderr: String,
    placed: Option<Vec<u8>>,
    urls: Vec<String>,
}

impl Outcome {
    fn installed_prebuilt(&self) -> bool {
        self.stdout.contains("installed prebuilt")
    }
    fn fell_back(&self) -> bool {
        self.stdout.contains("FAKE_CARGO_BUILD")
    }
}

/// Run the script with stubbed uname/curl/cargo.
/// - `serve_binary == false` → fake curl fails the binary fetch (simulates a 404 / missing asset).
/// - `corrupt_sums == true`  → the published SHA256SUMS holds a wrong hash (checksum mismatch).
fn run_impl(
    os: &str,
    arch: &str,
    version: &str,
    serve_binary: bool,
    corrupt_sums: bool,
    repo_root: Option<&Path>,
    commit_marker: Option<&str>,
) -> Outcome {
    let root = tmp("root");
    let stub = root.join("bin");
    let server = root.join("server");
    let out = root.join("target/release/herdr-file-viewer");
    let urllog = root.join("urls.log");
    fs::create_dir_all(&stub).unwrap();
    fs::create_dir_all(&server).unwrap();

    let cargo_toml = root.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        format!("[package]\nname = \"herdr-file-viewer\"\nversion = \"{version}\"\n"),
    )
    .unwrap();

    // The triple the script will resolve — kept in lockstep with the script's mapping.
    let triple = match (os, arch) {
        ("Darwin", "arm64") | ("Darwin", "aarch64") => "aarch64-apple-darwin",
        ("Darwin", "x86_64") => "x86_64-apple-darwin",
        ("Linux", "x86_64") => "x86_64-unknown-linux-musl",
        _ => "",
    };

    let bin_blob: &[u8] = b"#!/bin/sh\necho prebuilt-viewer\n";
    fs::write(server.join("bin"), bin_blob).unwrap();
    if !triple.is_empty() {
        let real = sha256_of(&server.join("bin"));
        let hash = if corrupt_sums { "0".repeat(64) } else { real };
        fs::write(
            server.join("SHA256SUMS"),
            format!("{hash}  herdr-file-viewer-{triple}\n"),
        )
        .unwrap();
    }

    // The COMMIT marker the release publishes; the gate compares the checkout's HEAD to it.
    if let Some(c) = commit_marker {
        fs::write(server.join("COMMIT"), format!("{c}\n")).unwrap();
    }

    write_exec(
        &stub.join("uname"),
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n  -s) echo {os} ;;\n  -m) echo {arch} ;;\n  *) echo {os} ;;\nesac\n"
        ),
    );

    let bin_rule = if serve_binary {
        format!("cp \"{}/bin\" \"$dest\"", server.display())
    } else {
        "exit 22".to_string() // curl -f exits 22 on HTTP >= 400
    };
    write_exec(
        &stub.join("curl"),
        &format!(
            "#!/bin/sh\ndest=\"\"; url=\"\"\n\
             while [ $# -gt 0 ]; do case \"$1\" in -o) dest=\"$2\"; shift 2 ;; -*) shift ;; *) url=\"$1\"; shift ;; esac; done\n\
             echo \"$url\" >> \"{log}\"\n\
             case \"$url\" in\n  */COMMIT) cp \"{srv}/COMMIT\" \"$dest\" ;;\n  */SHA256SUMS) cp \"{srv}/SHA256SUMS\" \"$dest\" ;;\n  *) {bin_rule} ;;\nesac\n",
            log = urllog.display(),
            srv = server.display(),
        ),
    );

    write_exec(
        &stub.join("cargo"),
        "#!/bin/sh\necho FAKE_CARGO_BUILD \"$@\"\nexit 0\n",
    );

    // The ahead-note logic inspects FV_REPO_ROOT as a git work tree. By default point it at the
    // (non-git) temp root so it is skipped and the platform/download/verify logic is what's under
    // test; the git-checkout tests pass a real git repo here.
    let fv_repo_root: &Path = repo_root.unwrap_or(root.as_path());
    let path = format!("{}:{}", stub.display(), std::env::var("PATH").unwrap());
    let output = Command::new("sh")
        .arg(script_path())
        .env("PATH", path)
        .env("HOME", &root) // no ~/.cargo/env here, so the fallback uses the stubbed cargo
        .env("FV_REPO_ROOT", fv_repo_root)
        .env("FV_CARGO_TOML", &cargo_toml)
        .env("FV_OUT", &out)
        .env("FV_BASE_URL", "https://example.invalid/releases/download")
        .output()
        .expect("run fetch-or-build.sh");

    let urls = fs::read_to_string(&urllog)
        .unwrap_or_default()
        .lines()
        .map(|s| s.to_string())
        .collect();
    let placed = fs::read(&out).ok();
    let o = Outcome {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        placed,
        urls,
    };
    let _ = fs::remove_dir_all(&root);
    o
}

/// Default runner: no git work tree at FV_REPO_ROOT, so the release-tag gate is skipped and the
/// platform/download/verify path is exercised directly.
fn run(os: &str, arch: &str, version: &str, serve_binary: bool, corrupt_sums: bool) -> Outcome {
    run_impl(os, arch, version, serve_binary, corrupt_sums, None, None)
}

/// Throwaway git repo at `dir` with a single commit and NO tag — exactly like herdr's install
/// checkout (a work tree at the cloned commit, without local tags). Returns the HEAD commit SHA,
/// which the ahead-note logic compares against the release's published COMMIT marker.
fn make_git_repo(dir: &Path) -> String {
    fs::create_dir_all(dir).unwrap();
    let g = |args: &[&str]| -> std::process::Output {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        o
    };
    g(&["init", "-q"]);
    fs::write(dir.join("f.txt"), "1").unwrap();
    g(&["add", "-A"]);
    g(&["commit", "-q", "-m", "release"]);
    let head = g(&["rev-parse", "HEAD"]);
    String::from_utf8_lossy(&head.stdout).trim().to_string()
}

#[test]
fn fast_path_installs_verified_binary_on_apple_silicon() {
    let o = run("Darwin", "arm64", "1.2.0", true, false);
    assert!(
        o.installed_prebuilt(),
        "stdout: {}\nstderr: {}",
        o.stdout,
        o.stderr
    );
    assert!(
        !o.fell_back(),
        "must not build from source on the fast path"
    );
    assert_eq!(
        o.placed.as_deref(),
        Some(&b"#!/bin/sh\necho prebuilt-viewer\n"[..])
    );
    assert!(
        o.urls
            .iter()
            .any(|u| u.ends_with("/v1.2.0/herdr-file-viewer-aarch64-apple-darwin")),
        "fetched exactly the version+triple asset, never 'latest'; urls: {:?}",
        o.urls
    );
    assert!(
        o.urls.iter().any(|u| u.ends_with("/v1.2.0/SHA256SUMS")),
        "urls: {:?}",
        o.urls
    );
}

#[test]
fn maps_intel_mac_to_x86_64_apple_darwin() {
    let o = run("Darwin", "x86_64", "1.2.0", true, false);
    assert!(o.installed_prebuilt(), "{}", o.stderr);
    assert!(
        o.urls
            .iter()
            .any(|u| u.ends_with("/v1.2.0/herdr-file-viewer-x86_64-apple-darwin")),
        "urls: {:?}",
        o.urls
    );
}

#[test]
fn maps_linux_to_static_musl_triple() {
    let o = run("Linux", "x86_64", "1.2.0", true, false);
    assert!(o.installed_prebuilt(), "{}", o.stderr);
    assert!(
        o.urls
            .iter()
            .any(|u| u.ends_with("/v1.2.0/herdr-file-viewer-x86_64-unknown-linux-musl")),
        "urls: {:?}",
        o.urls
    );
}

#[test]
fn checksum_mismatch_falls_back_and_installs_nothing() {
    let o = run("Linux", "x86_64", "1.2.0", true, true);
    assert!(o.fell_back(), "stdout: {}", o.stdout);
    assert!(o.placed.is_none(), "must not install an unverified binary");
    assert!(
        o.stderr.contains("checksum mismatch"),
        "stderr: {}",
        o.stderr
    );
}

#[test]
fn missing_release_asset_falls_back_to_source_build() {
    let o = run("Linux", "x86_64", "9.9.9", false, false);
    assert!(o.fell_back(), "stdout: {}", o.stdout);
    assert!(o.placed.is_none());
    assert!(o.stderr.contains("not available"), "stderr: {}", o.stderr);
}

#[test]
fn unmapped_platform_falls_back_without_downloading() {
    let o = run("Linux", "riscv64", "1.2.0", true, false);
    assert!(o.fell_back(), "stdout: {}", o.stdout);
    assert!(
        o.urls.is_empty(),
        "must not download for an unsupported platform; urls: {:?}",
        o.urls
    );
    assert!(
        o.stderr.contains("no prebuilt binary"),
        "stderr: {}",
        o.stderr
    );
}

#[test]
fn script_preserves_the_cargo_source_build_fallback() {
    let s = fs::read_to_string(script_path()).unwrap();
    assert!(
        s.contains("cargo build --release"),
        "fallback must still build from source"
    );
    assert!(
        s.contains(".cargo/env"),
        "fallback must source ~/.cargo/env like the original build step"
    );
}

// Matching commit: when the checkout IS the released commit, the prebuilt installs cleanly and
// (because HEAD == COMMIT) no "ahead" note is emitted.
#[test]
fn uses_prebuilt_when_head_matches_the_release_commit() {
    let gitdir = tmp("gitrepo-match");
    let head = make_git_repo(&gitdir); // tagless work tree, like herdr's install checkout
    let o = run_impl(
        "Linux",
        "x86_64",
        "1.2.0",
        true,
        false,
        Some(&gitdir),
        Some(&head),
    );
    assert!(
        o.installed_prebuilt(),
        "HEAD matches the release COMMIT → prebuilt must be used\nstdout:{}\nstderr:{}",
        o.stdout,
        o.stderr
    );
    assert!(!o.fell_back(), "must not build from source");
    assert!(
        !o.stdout.contains("ahead of"),
        "no ahead-note when the checkout IS the released commit; stdout: {}",
        o.stdout
    );
    let _ = fs::remove_dir_all(&gitdir);
}

// Version-only behavior: when the checkout's HEAD is AHEAD of the release commit (e.g. main has
// merged work that isn't tagged yet), the prebuilt for the declared version is STILL installed —
// landing a PR no longer forces new users to compile — and a transparency note records that the
// working tree carries newer, unreleased source than the binary.
#[test]
fn uses_prebuilt_when_head_is_ahead_of_the_release_commit() {
    let gitdir = tmp("gitrepo-ahead");
    let _head = make_git_repo(&gitdir);
    let other = "0000000000000000000000000000000000000000"; // a commit this checkout is NOT at
    let o = run_impl(
        "Linux",
        "x86_64",
        "1.2.0",
        true,
        false,
        Some(&gitdir),
        Some(other),
    );
    assert!(
        o.installed_prebuilt(),
        "HEAD ahead of the release commit must still use the prebuilt\nstdout:{}\nstderr:{}",
        o.stdout,
        o.stderr
    );
    assert!(
        !o.fell_back(),
        "must not build from source merely because the checkout is ahead of the tag"
    );
    assert_eq!(
        o.placed.as_deref(),
        Some(&b"#!/bin/sh\necho prebuilt-viewer\n"[..]),
        "the verified prebuilt must be installed"
    );
    assert!(
        o.urls
            .iter()
            .any(|u| u.contains("herdr-file-viewer-x86_64-unknown-linux-musl")),
        "the binary download must happen (no commit gate before it); urls: {:?}",
        o.urls
    );
    assert!(
        o.stdout.contains("ahead of"),
        "must note that the checkout is ahead of the release commit; stdout: {}",
        o.stdout
    );
    let _ = fs::remove_dir_all(&gitdir);
}

// A version with NO published release still falls back to source even from a git checkout — the
// asset download 404s, so we never silently install a binary whose version differs from the source.
#[test]
fn version_with_no_release_still_falls_back_from_a_git_checkout() {
    let gitdir = tmp("gitrepo-norelease");
    let head = make_git_repo(&gitdir);
    let o = run_impl(
        "Linux",
        "x86_64",
        "9.9.9",
        false, // serve_binary = false → the asset 404s
        false,
        Some(&gitdir),
        Some(&head),
    );
    assert!(o.fell_back(), "stdout: {}\nstderr: {}", o.stdout, o.stderr);
    assert!(o.placed.is_none(), "must not install anything");
    let _ = fs::remove_dir_all(&gitdir);
}
