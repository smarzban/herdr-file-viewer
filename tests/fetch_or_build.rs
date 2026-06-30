//! Hermetic tests for the install-time `[[build]]` step: `scripts/fetch-or-build.sh` (unix) and
//! `scripts/fetch-or-build.ps1` (Windows, T-7 — AC-11, AC-12, AC-13). Each platform's tests stub
//! its own platform's download/build tools and serve a local fixture "release"; no network, no
//! real cargo build, no new deps.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

/// A fresh empty temp dir, distinct per `label` so parallel tests don't collide. Shared by both
/// platforms' test modules below.
fn tmp(label: &str) -> PathBuf {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("fv-fob-{}-{}-{}", std::process::id(), label, n));
    fs::create_dir_all(&p).unwrap();
    p
}

// ===============================================================================================
// unix: scripts/fetch-or-build.sh
// ===============================================================================================

#[cfg(unix)]
mod unix_sh {
    use super::tmp;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn script_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/fetch-or-build.sh")
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
} // mod unix_sh

// ===============================================================================================
// Windows: scripts/fetch-or-build.ps1 (T-7 — AC-11, AC-12, AC-13)
// ===============================================================================================
//
// PowerShell functions take precedence over cmdlets of the same name in the calling scope (the
// same mechanism Pester's mocking uses), so the hermetic test shadows `Invoke-WebRequest` with a
// mock that serves a local fixture, then dot-sources the real script — no production-code seam
// beyond the FV_* env vars is needed. `cargo` is stubbed via a `.cmd` shim prepended onto PATH
// (no chmod/exec-bit ceremony needed on Windows). Real `Get-FileHash`/`certutil` run unmocked —
// both are purely local/file-based, so they need no network stubbing to stay hermetic.
#[cfg(windows)]
mod windows_ps1 {
    use super::tmp;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn script_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/fetch-or-build.ps1")
    }

    /// Hex SHA-256 of a file via the built-in `certutil` (no new Cargo dep for the test either).
    fn sha256_of(file: &Path) -> String {
        let out = Command::new("certutil")
            .args(["-hashfile"])
            .arg(file)
            .arg("SHA256")
            .output()
            .expect("certutil is available on every Windows host");
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines()
            .map(str::trim)
            .find(|l| l.len() == 64 && l.bytes().all(|b| b.is_ascii_hexdigit()))
            .expect("certutil prints the 64-hex-char digest line")
            .to_lowercase()
    }

    /// Build the mock `Invoke-WebRequest` + dot-source driver script. The mock serves
    /// `server\bin`/`SHA256SUMS`/`COMMIT` by URL suffix, mirroring the sh test's curl stub
    /// `case`/`esac`, and logs every requested URL to `urllog` for the "fetched exactly the
    /// expected asset" assertions.
    fn write_driver(
        driver_path: &Path,
        real_script: &Path,
        server: &Path,
        urllog: &Path,
        serve_binary: bool,
    ) {
        let bin_rule = if serve_binary {
            format!(
                r#"Copy-Item -Path '{}' -Destination $OutFile -Force"#,
                server.join("bin").display()
            )
        } else {
            "throw 'simulated 404'".to_string()
        };
        let driver = format!(
            r#"
function Invoke-WebRequest {{
    param($Uri, $OutFile, [switch]$UseBasicParsing, $ErrorAction)
    Add-Content -Path '{urllog}' -Value $Uri
    if ($Uri -like '*/COMMIT') {{
        Copy-Item -Path '{commit_src}' -Destination $OutFile -Force
    }} elseif ($Uri -like '*/SHA256SUMS') {{
        Copy-Item -Path '{sums_src}' -Destination $OutFile -Force
    }} else {{
        {bin_rule}
    }}
}}
. '{script}'
"#,
            urllog = urllog.display(),
            commit_src = server.join("COMMIT").display(),
            sums_src = server.join("SHA256SUMS").display(),
            script = real_script.display(),
        );
        fs::write(driver_path, driver).unwrap();
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

    /// Run the (mocked) script for a fixture release at `version`.
    /// - `serve_binary == false` → the mocked download throws (simulates a 404 / missing asset).
    /// - `corrupt_sums == true`  → the published SHA256SUMS holds a wrong hash (checksum mismatch).
    fn run(version: &str, serve_binary: bool, corrupt_sums: bool) -> Outcome {
        let root = tmp("root-ps1");
        let stub = root.join("bin");
        let server = root.join("server");
        let out = root.join("target/release/herdr-file-viewer.exe");
        let urllog = root.join("urls.log");
        fs::create_dir_all(&stub).unwrap();
        fs::create_dir_all(&server).unwrap();

        let cargo_toml = root.join("Cargo.toml");
        fs::write(
            &cargo_toml,
            format!("[package]\nname = \"herdr-file-viewer\"\nversion = \"{version}\"\n"),
        )
        .unwrap();

        let triple = "x86_64-pc-windows-msvc";
        let asset = format!("herdr-file-viewer-{triple}.exe");
        let bin_blob: &[u8] = b"fake windows prebuilt binary\r\n";
        fs::write(server.join("bin"), bin_blob).unwrap();
        let real_hash = sha256_of(&server.join("bin"));
        let hash = if corrupt_sums {
            "0".repeat(64)
        } else {
            real_hash
        };
        fs::write(server.join("SHA256SUMS"), format!("{hash}  {asset}\n")).unwrap();
        // No COMMIT fixture: FV_REPO_ROOT below is not a git work tree, so the ahead-note path
        // is skipped and the platform/download/verify logic is what's under test (mirrors the
        // sh test's default `run`).
        fs::write(server.join("COMMIT"), "unused\n").unwrap();

        // Stub `cargo` via a `.cmd` shim on PATH — no chmod/exec-bit needed on Windows.
        fs::write(
            stub.join("cargo.cmd"),
            "@echo off\r\necho FAKE_CARGO_BUILD %*\r\nexit /b 0\r\n",
        )
        .unwrap();

        let driver = root.join("driver.ps1");
        write_driver(&driver, &script_path(), &server, &urllog, serve_binary);

        let path = format!(
            "{};{}",
            stub.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&driver)
            .env("PATH", path)
            .env("PROCESSOR_ARCHITECTURE", "AMD64")
            .env("FV_REPO_ROOT", &root) // not a git work tree → ahead-note path is skipped
            .env("FV_CARGO_TOML", &cargo_toml)
            .env("FV_OUT", &out)
            .env("FV_BASE_URL", "https://example.invalid/releases/download")
            .output()
            .expect("run fetch-or-build.ps1 via the mock driver");

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

    /// (a) matching prebuilt + correct SHA → installs to FV_OUT, exits 0 without invoking cargo.
    #[test]
    fn fast_path_installs_verified_binary() {
        let o = run("1.2.0", true, false);
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
            Some(&b"fake windows prebuilt binary\r\n"[..])
        );
        assert!(
            o.urls
                .iter()
                .any(|u| u.ends_with("/v1.2.0/herdr-file-viewer-x86_64-pc-windows-msvc.exe")),
            "fetched exactly the version+triple .exe asset, never 'latest'; urls: {:?}",
            o.urls
        );
        assert!(
            o.urls.iter().any(|u| u.ends_with("/v1.2.0/SHA256SUMS")),
            "urls: {:?}",
            o.urls
        );
    }

    /// (b) missing asset (404) → source-build fallback.
    #[test]
    fn missing_release_asset_falls_back_to_source_build() {
        let o = run("9.9.9", false, false);
        assert!(o.fell_back(), "stdout: {}\nstderr: {}", o.stdout, o.stderr);
        assert!(o.placed.is_none());
    }

    /// (c) checksum mismatch → fallback (never installs the unverified binary).
    #[test]
    fn checksum_mismatch_falls_back_and_installs_nothing() {
        let o = run("1.2.0", true, true);
        assert!(o.fell_back(), "stdout: {}\nstderr: {}", o.stdout, o.stderr);
        assert!(o.placed.is_none(), "must not install an unverified binary");
        assert!(
            o.stderr.contains("checksum mismatch"),
            "stderr: {}",
            o.stderr
        );
    }

    /// An unmapped architecture (no x86_64 Windows match — AC-N4 means no aarch64 support)
    /// falls back without ever attempting a download.
    #[test]
    fn unmapped_architecture_falls_back_without_downloading() {
        let root = tmp("root-ps1-unmapped");
        let stub = root.join("bin");
        let server = root.join("server");
        let out = root.join("target/release/herdr-file-viewer.exe");
        let urllog = root.join("urls.log");
        fs::create_dir_all(&stub).unwrap();
        fs::create_dir_all(&server).unwrap();
        let cargo_toml = root.join("Cargo.toml");
        fs::write(
            &cargo_toml,
            "[package]\nname = \"herdr-file-viewer\"\nversion = \"1.2.0\"\n",
        )
        .unwrap();
        fs::write(
            stub.join("cargo.cmd"),
            "@echo off\r\necho FAKE_CARGO_BUILD %*\r\nexit /b 0\r\n",
        )
        .unwrap();
        let driver = root.join("driver.ps1");
        write_driver(&driver, &script_path(), &server, &urllog, true);

        let path = format!(
            "{};{}",
            stub.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&driver)
            .env("PATH", path)
            .env("PROCESSOR_ARCHITECTURE", "ARM64") // unsupported in v1 (AC-N4)
            .env("FV_REPO_ROOT", &root)
            .env("FV_CARGO_TOML", &cargo_toml)
            .env("FV_OUT", &out)
            .env("FV_BASE_URL", "https://example.invalid/releases/download")
            .output()
            .expect("run fetch-or-build.ps1 via the mock driver");

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let urls = fs::read_to_string(&urllog).unwrap_or_default();
        assert!(
            stdout.contains("FAKE_CARGO_BUILD"),
            "stdout: {stdout}\nstderr: {stderr}"
        );
        assert!(
            urls.is_empty(),
            "must not download for an unsupported architecture; urls log: {urls:?}"
        );
        assert!(stderr.contains("no prebuilt binary"), "stderr: {stderr}");
        let _ = fs::remove_dir_all(&root);
    }

    /// The fallback preserves the cargo source-build path, like the sh script.
    #[test]
    fn script_preserves_the_cargo_source_build_fallback() {
        let s = fs::read_to_string(script_path()).unwrap();
        assert!(
            s.contains("cargo build --release"),
            "fallback must still build from source"
        );
        assert!(
            s.contains("rustup.rs"),
            "the no-cargo message must point at how to get Rust, like the sh script"
        );
    }
}
