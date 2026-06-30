//! herdr query seam: `HerdrCli` + `LiveHerdr` + `CommandRunner` (AC-3, AC-15).
//! Tests are hermetic — nothing is really spawned.

use herdr_file_viewer::herdr::{CommandRunner, HerdrCli, LiveHerdr};
use std::ffi::OsStr;
use std::io;
use std::process::{ExitStatus, Output};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Helpers for building canned ExitStatus values without spawning.
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn exit_success() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(0)
}

#[cfg(unix)]
fn exit_failure() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(1)
}

fn make_output(status: ExitStatus, stdout: &str) -> Output {
    Output {
        status,
        stdout: stdout.as_bytes().to_vec(),
        stderr: vec![],
    }
}

// ---------------------------------------------------------------------------
// RecordingRunner: records the last call via Arc<Mutex<...>> so the test
// can inspect after run_json consumes the runner inside LiveHerdr.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Recorded {
    program: std::ffi::OsString,
    args: Vec<String>,
}

struct RecordingRunner {
    /// Shared record: test holds a clone of this Arc to read after the call.
    shared: Arc<Mutex<Option<Recorded>>>,
    canned: Output,
}

impl RecordingRunner {
    fn success(stdout: &str, shared: Arc<Mutex<Option<Recorded>>>) -> Self {
        Self {
            shared,
            canned: make_output(exit_success(), stdout),
        }
    }

    fn failure(shared: Arc<Mutex<Option<Recorded>>>) -> Self {
        Self {
            shared,
            canned: make_output(exit_failure(), ""),
        }
    }
}

impl CommandRunner for RecordingRunner {
    fn run(&self, program: &OsStr, args: &[&str]) -> io::Result<Output> {
        *self.shared.lock().unwrap() = Some(Recorded {
            program: program.to_owned(),
            args: args.iter().map(|s| s.to_string()).collect(),
        });
        Ok(make_output(
            self.canned.status,
            &String::from_utf8_lossy(&self.canned.stdout),
        ))
    }
}

// ---------------------------------------------------------------------------
// Test 1: argv correctness + canned stdout returned (AC-3)
// ---------------------------------------------------------------------------

#[test]
fn run_json_passes_program_and_args_and_returns_stdout() {
    let canned = r#"[{"worktree":"/repo","HEAD":"abc","branch":"main"}]"#;
    let record: Arc<Mutex<Option<Recorded>>> = Arc::new(Mutex::new(None));
    let fake = RecordingRunner::success(canned, Arc::clone(&record));
    let cli = LiveHerdr::with_runner("herdr-test-bin", fake);

    let result = cli.run_json(&["worktree", "list", "--json"]).unwrap();

    let rec = record
        .lock()
        .unwrap()
        .clone()
        .expect("runner was never called");

    // (a) program is what was configured
    assert_eq!(rec.program, std::ffi::OsString::from("herdr-test-bin"));
    // (b) args match exactly
    assert_eq!(rec.args, vec!["worktree", "list", "--json"]);
    // (c) run_json returns the canned stdout string
    assert_eq!(result, canned);
}

// ---------------------------------------------------------------------------
// Test 2: non-zero exit → Err (AC-15 — caller can degrade)
// ---------------------------------------------------------------------------

#[test]
fn run_json_returns_err_on_non_zero_exit() {
    let record: Arc<Mutex<Option<Recorded>>> = Arc::new(Mutex::new(None));
    let fake = RecordingRunner::failure(Arc::clone(&record));
    let cli = LiveHerdr::with_runner("herdr-test-bin", fake);

    let result = cli.run_json(&["worktree", "list", "--json"]);

    assert!(result.is_err(), "expected Err on non-zero exit, got Ok");
}

// ---------------------------------------------------------------------------
// Test 3: resolve_program — pure helper, testable without touching the env
// ---------------------------------------------------------------------------

#[test]
fn resolve_program_uses_herdr_bin_path_when_set() {
    use herdr_file_viewer::herdr::resolve_program;
    let result = resolve_program(Some("/custom/herdr".to_string()));
    assert_eq!(result, std::ffi::OsString::from("/custom/herdr"));
}

#[test]
fn resolve_program_falls_back_to_herdr_when_var_is_none() {
    use herdr_file_viewer::herdr::resolve_program;
    let result = resolve_program(None);
    assert_eq!(result, std::ffi::OsString::from("herdr"));
}

#[test]
fn resolve_program_falls_back_to_herdr_when_var_is_empty() {
    use herdr_file_viewer::herdr::resolve_program;
    let result = resolve_program(Some(String::new()));
    assert_eq!(result, std::ffi::OsString::from("herdr"));
}

// ---------------------------------------------------------------------------
// Test 4: HerdrCli is the substitution point — a fake impl compiles and works
// ---------------------------------------------------------------------------

struct FakeCli {
    canned: String,
}

impl HerdrCli for FakeCli {
    fn run_json(&self, _args: &[&str]) -> io::Result<String> {
        Ok(self.canned.clone())
    }
}

#[test]
fn herdr_cli_trait_is_substitutable_with_a_fake() {
    let fake: Box<dyn HerdrCli> = Box::new(FakeCli {
        canned: r#"{"ok":true}"#.to_string(),
    });
    let result = fake.run_json(&["whatever"]).unwrap();
    assert_eq!(result, r#"{"ok":true}"#);
}
