//! The isolated public-source boundary for release discovery.
//!
//! This module is the only place the update feature asks the official repository for release state
//! or documents. It accepts a single absolute deadline, exposes no raw source output, and turns
//! every source or process failure into [`Source::Unavailable`].

use super::{Version, repo_url};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Maximum `git ls-remote` stdout accepted before discovery becomes unavailable.
///
/// The cap is applied while reading the pipe, so the gateway never allocates for an unbounded
/// remote response. It also bounds the number of typed release tags returned to callers.
pub const DISCOVERY_MAX_BYTES: usize = 256 * 1024;

/// Maximum bytes retained from either immutable remote document.
///
/// The reader accepts at most one byte beyond this cap, so an exact 1 MiB document succeeds while
/// an oversized source becomes unavailable before unbounded allocation.
pub const DOCUMENT_MAX_BYTES: usize = 1024 * 1024;

/// The compiled raw-content authority for documents in the official repository.
const DOCUMENT_AUTHORITY: &str = "https://raw.githubusercontent.com";
const CHANGELOG_PATH: &str = "CHANGELOG.md";
const SPOTLIGHT_PATH: &str = "project-spotlight.md";

/// The terminal source outcome exposed by the official-repository gateway.
///
/// Failures intentionally carry no diagnostics: an update check is advisory, and its UI must
/// remain silent when the network, Git, or a hostile response is unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source<T> {
    Available(T),
    Unavailable,
}

/// A caller-injected release discovery seam. The caller chooses one absolute deadline; runners
/// must return only the typed, bounded [`ReleaseState`] or [`Source::Unavailable`].
pub type DiscoveryRunner = Box<dyn Fn(Instant) -> Source<ReleaseState> + Send>;

/// The document half of the official source boundary, injected into the refresh coordinator.
///
/// Each method receives the coordinator's one absolute deadline. Implementations return only
/// bounded typed source facts, never diagnostics, so callers cannot accidentally couple remote
/// failures to the UI.
pub trait Gateway: Send {
    /// Retrieve immutable changelog bytes for the detected release commit.
    fn changelog(&self, release: &ReleaseTag, deadline: Instant) -> Source<Option<Vec<u8>>>;

    /// Retrieve spotlight bytes for the discovered HEAD commit.
    fn spotlight(&self, state: &ReleaseState, deadline: Instant) -> Source<Option<Vec<u8>>>;
}

/// A validated Git object ID (SHA-1 or SHA-256).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObjectId(String);

impl ObjectId {
    /// Parse a canonical-width hexadecimal Git object ID.
    pub fn parse(value: &str) -> Option<Self> {
        matches!(value.len(), 40 | 64)
            .then(|| value.bytes().all(|byte| byte.is_ascii_hexdigit()))?
            .then(|| Self(value.to_owned()))
    }

    /// The validated hexadecimal object ID.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One stable release tag, resolved to the commit it describes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseTag {
    pub version: Version,
    pub object_id: ObjectId,
}

impl ReleaseTag {
    /// Construct a typed release tag for injected test runners.
    pub fn new(version: Version, object_id: ObjectId) -> Self {
        Self { version, object_id }
    }
}

/// The bounded release state discovered from the fixed official repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseState {
    pub head_object_id: ObjectId,
    releases: Vec<ReleaseTag>,
}

impl ReleaseState {
    /// Construct a bounded state for an injected discovery runner.
    pub fn new(head_object_id: ObjectId, releases: Vec<ReleaseTag>) -> Option<Self> {
        (releases.len() <= max_releases()).then_some(Self {
            head_object_id,
            releases,
        })
    }

    /// Stable release tags retained from the bounded discovery response.
    pub fn releases(&self) -> &[ReleaseTag] {
        &self.releases
    }

    /// The highest stable tag, if the public repository has one.
    pub fn latest_release(&self) -> Option<&ReleaseTag> {
        self.releases.iter().max_by_key(|release| release.version)
    }
}

/// Discover the official repository's `HEAD` and stable release tags before `deadline`.
///
/// The source URL is fixed by [`repo_url`], never supplied by a caller. All failures, including a
/// fresh private-directory creation failure, become unavailable so update checks stay silent.
pub fn discover_release_state(deadline: Instant) -> Source<ReleaseState> {
    let Ok(run_dir) = make_private_dir() else {
        return Source::Unavailable;
    };
    let result = discover_with_command(ls_remote_command(&run_dir), deadline);
    let _ = std::fs::remove_dir_all(&run_dir);
    result
}

/// Bounded retrieval of immutable documents from the official repository.
///
/// The public constructor fixes the raw-content authority at compile time. No configuration or
/// environment input can select a host, proxy, redirect destination, or transport security mode.
pub struct DocumentGateway {
    authority: String,
    https_only: bool,
}

impl DocumentGateway {
    /// Construct a gateway for the fixed official raw-content authority.
    pub fn new() -> Self {
        Self {
            authority: DOCUMENT_AUTHORITY.to_owned(),
            https_only: true,
        }
    }

    /// Retrieve the changelog at the immutable commit resolved for `release`.
    pub fn changelog(&self, release: &ReleaseTag, deadline: Instant) -> Source<Option<Vec<u8>>> {
        self.document(release.object_id.as_str(), CHANGELOG_PATH, deadline)
    }

    /// Retrieve the project spotlight at the immutable commit discovered for `HEAD`.
    pub fn spotlight(&self, state: &ReleaseState, deadline: Instant) -> Source<Option<Vec<u8>>> {
        self.document(state.head_object_id.as_str(), SPOTLIGHT_PATH, deadline)
    }

    fn document(&self, object_id: &str, path: &str, deadline: Instant) -> Source<Option<Vec<u8>>> {
        let url = format!(
            "{}/{}/{object_id}/{path}",
            self.authority,
            super::repo_slug()
        );
        let timeout = remaining(deadline);
        if timeout.is_zero() {
            return Source::Unavailable;
        }
        let mut response = match self.agent(timeout).get(&url).call() {
            Ok(response) => response,
            Err(_) => return Source::Unavailable,
        };
        match response.status().as_u16() {
            404 => Source::Available(None),
            200 => response
                .body_mut()
                .with_config()
                .limit(DOCUMENT_MAX_BYTES as u64 + 1)
                .read_to_vec()
                .ok()
                .filter(|bytes| bytes.len() <= DOCUMENT_MAX_BYTES)
                .map(|bytes| Source::Available(Some(bytes)))
                .unwrap_or(Source::Unavailable),
            _ => Source::Unavailable,
        }
    }

    fn agent(&self, timeout: Duration) -> ureq::Agent {
        ureq::Agent::config_builder()
            .https_only(self.https_only)
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(timeout))
            .build()
            .into()
    }

    #[cfg(test)]
    fn with_test_authority(authority: &str) -> Self {
        Self {
            authority: authority.trim_end_matches('/').to_owned(),
            https_only: false,
        }
    }
}

impl Gateway for DocumentGateway {
    fn changelog(&self, release: &ReleaseTag, deadline: Instant) -> Source<Option<Vec<u8>>> {
        Self::changelog(self, release, deadline)
    }

    fn spotlight(&self, state: &ReleaseState, deadline: Instant) -> Source<Option<Vec<u8>>> {
        Self::spotlight(self, state, deadline)
    }
}

impl Default for DocumentGateway {
    fn default() -> Self {
        Self::new()
    }
}

/// Git's HTTPS low-speed timeout complements the single outer deadline by terminating a stalled
/// transfer early. It is not a second wall-clock budget.
const PROBE_LOW_SPEED_TIME: &str = "5";

/// Construct the fixed `ls-remote` query. Its `HEAD` object-ID record and requested
/// `refs/tags/v*` records give the immutable source pins.
fn ls_remote_command(run_dir: &Path) -> Command {
    ls_remote_command_with_program("git", run_dir)
}

fn ls_remote_command_with_program(program: impl AsRef<OsStr>, run_dir: &Path) -> Command {
    let mut command = Command::new(program);
    command.args(["ls-remote", repo_url(), "HEAD", "refs/tags/v*"]);
    harden_git(&mut command, run_dir);
    command
        .env("GIT_HTTP_LOW_SPEED_LIMIT", "1000")
        .env("GIT_HTTP_LOW_SPEED_TIME", PROBE_LOW_SPEED_TIME)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

/// Keep discovery outside the viewed repository while retaining the user's Git transport trust.
///
/// The fresh private directory and ceiling exclude viewed-repository local configuration. Global
/// and system Git configuration, including user proxy, CA, and noninteractive credential-helper
/// settings, remains inherited. HTTPS-only transport, disabled terminal prompts, and null stdin
/// keep the fixed public query noninteractive.
fn harden_git(command: &mut Command, run_dir: &Path) {
    command
        // A ceiling constrains discovery only. Drop explicit repository redirects as well, so a
        // viewer-launched `GIT_DIR` cannot reintroduce its local configuration.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .current_dir(run_dir)
        .env("GIT_CEILING_DIRECTORIES", run_dir)
        .env("GIT_ALLOW_PROTOCOL", "https")
        .env("GIT_TERMINAL_PROMPT", "0");
}

fn discover_with_command(mut command: Command, deadline: Instant) -> Source<ReleaseState> {
    let Ok(child) = command.spawn() else {
        return Source::Unavailable;
    };
    discover_child_bounded(child, DISCOVERY_MAX_BYTES, deadline)
}

fn discover_child_bounded(
    mut child: Child,
    max_bytes: usize,
    deadline: Instant,
) -> Source<ReleaseState> {
    discover_child_bounded_with_spawner(&mut child, max_bytes, deadline, &spawn_reader_thread)
}

fn discover_child_bounded_with_spawner(
    child: &mut Child,
    max_bytes: usize,
    deadline: Instant,
    spawn_reader: &impl Fn(ReaderTask) -> io::Result<std::thread::JoinHandle<()>>,
) -> Source<ReleaseState> {
    let Ok(stdout) = capture_stdout_bounded_with_spawner(child, max_bytes, deadline, spawn_reader)
    else {
        return Source::Unavailable;
    };
    parse_release_bytes(&stdout)
        .map(Source::Available)
        .unwrap_or(Source::Unavailable)
}

/// Read stdout through a cap and use the same deadline to wait for child exit.
///
/// On every error path this kills and reaps the child. The reader asks the pipe for at most one
/// byte beyond the current cap, so an over-cap producer is stopped before the rest is buffered.
#[cfg(all(test, unix))]
fn capture_stdout_bounded(
    child: &mut Child,
    max_bytes: usize,
    deadline: Instant,
) -> Result<Vec<u8>, CaptureFailure> {
    capture_stdout_bounded_with_spawner(child, max_bytes, deadline, &spawn_reader_thread)
}

type ReaderTask = Box<dyn FnOnce() + Send>;

fn spawn_reader_thread(task: ReaderTask) -> io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("release-discovery-reader".to_owned())
        .spawn(task)
}

fn capture_stdout_bounded_with_spawner(
    child: &mut Child,
    max_bytes: usize,
    deadline: Instant,
    spawn_reader: &impl Fn(ReaderTask) -> io::Result<std::thread::JoinHandle<()>>,
) -> Result<Vec<u8>, CaptureFailure> {
    let Some(stdout) = child.stdout.take() else {
        kill_and_reap(child);
        return Err(CaptureFailure::Read);
    };
    let (sender, receiver) = mpsc::channel();
    let reader = match spawn_reader(Box::new(move || {
        let _ = sender.send(read_bounded(stdout, max_bytes));
    })) {
        Ok(reader) => reader,
        Err(_) => {
            kill_and_reap(child);
            return Err(CaptureFailure::Spawn);
        }
    };

    let result = receiver.recv_timeout(remaining(deadline));
    let captured = match result {
        Ok(Ok(stdout)) => stdout,
        Ok(Err(error)) => {
            kill_and_reap(child);
            let _ = reader.join();
            return Err(error);
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            kill_and_reap(child);
            // The direct child is gone, but its descendants can retain the stdout pipe. Detach
            // their blocked reader rather than turning the caller's absolute deadline into join.
            return Err(CaptureFailure::Deadline);
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            kill_and_reap(child);
            return Err(CaptureFailure::Read);
        }
    };
    let _ = reader.join();

    match crate::proc::wait_bounded(child, remaining(deadline)) {
        Some(status) if status.success() => Ok(captured),
        Some(_) => Err(CaptureFailure::Exit),
        None => Err(CaptureFailure::Deadline),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureFailure {
    OverCap,
    Deadline,
    Exit,
    Read,
    Spawn,
}

fn read_bounded(mut stdout: impl Read, max_bytes: usize) -> Result<Vec<u8>, CaptureFailure> {
    const CHUNK: usize = 8 * 1024;
    let mut output = Vec::with_capacity(max_bytes.min(CHUNK));
    let mut chunk = [0_u8; CHUNK];
    loop {
        let remaining = max_bytes - output.len();
        // Request at most one byte beyond the cap. That byte proves overflow without draining a
        // malicious producer's remaining pipe data into memory.
        let wanted = if remaining == usize::MAX {
            CHUNK
        } else {
            (remaining + 1).min(CHUNK)
        };
        let read = stdout
            .read(&mut chunk[..wanted])
            .map_err(|_| CaptureFailure::Read)?;
        if read == 0 {
            return Ok(output);
        }
        if read > remaining {
            return Err(CaptureFailure::OverCap);
        }
        output.extend_from_slice(&chunk[..read]);
    }
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn parse_release_bytes(stdout: &[u8]) -> Option<ReleaseState> {
    parse_release_state(std::str::from_utf8(stdout).ok()?)
}

fn parse_release_state(stdout: &str) -> Option<ReleaseState> {
    let mut head_object_id = None;
    let mut releases = BTreeMap::<Version, TagParts>::new();

    for line in stdout.lines() {
        let Some((object_id, reference)) = line.split_once('\t') else {
            continue;
        };
        let Some(object_id) = ObjectId::parse(object_id) else {
            continue;
        };
        if reference == "HEAD" {
            head_object_id.get_or_insert(object_id);
            continue;
        }
        let Some(tag) = reference.strip_prefix("refs/tags/") else {
            continue;
        };
        let (tag, peeled) = match tag.strip_suffix("^{}") {
            Some(tag) => (tag, true),
            None => (tag, false),
        };
        if !tag.starts_with('v') {
            continue;
        }
        let Some(version) = Version::parse(tag) else {
            // Valid non-stable tags still match `v*`; they are irrelevant to a stable release
            // notice and do not make the otherwise trusted response unavailable.
            continue;
        };
        // Different accepted tag spellings can parse as the same Version, so the first direct
        // and peeled records retain the deterministic facts for each version.
        let part = releases.entry(version).or_default();
        let slot = if peeled {
            &mut part.peeled
        } else {
            &mut part.direct
        };
        slot.get_or_insert(object_id);
    }

    let head_object_id = head_object_id?;
    let mut typed_releases = Vec::with_capacity(releases.len());
    for (version, parts) in releases {
        // A peel line without its tag line cannot be trusted. A normal line without a peel is a
        // lightweight tag, while a normal + peel pair is an annotated tag resolved to its commit.
        let Some(object_id) = parts.direct else {
            continue;
        };
        typed_releases.push(ReleaseTag::new(version, parts.peeled.unwrap_or(object_id)));
    }
    ReleaseState::new(head_object_id, typed_releases)
}

#[derive(Default)]
struct TagParts {
    direct: Option<ObjectId>,
    peeled: Option<ObjectId>,
}

fn max_releases() -> usize {
    // Every accepted tag output line consumes at least 58 bytes: a 40-character object ID, tab,
    // `refs/tags/v0.0.0`, and newline. This cap keeps the returned type bounded if the byte cap
    // changes later.
    DISCOVERY_MAX_BYTES / 58
}

/// Create a fresh, private, empty directory under the system temp directory.
///
/// On Unix, the directory is created with owner-only mode before another process can observe it,
/// then its mode is reasserted. Exclusive creation and a never-reused name prevent an
/// attacker-planted `.git/config` from becoming the probe's working directory. The caller removes
/// the directory when it is finished.
fn make_private_dir() -> io::Result<PathBuf> {
    make_private_dir_in(&std::env::temp_dir(), harden_private_dir)
}

fn make_private_dir_in(
    base: &Path,
    harden: impl Fn(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = PROBE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    for attempt in 0..1024 {
        let path = base.join(format!(
            "herdr-fv-probe-{}-{nanos}-{sequence}-{attempt}",
            std::process::id()
        ));
        match create_private_dir(&path) {
            Ok(()) => match harden(&path) {
                Ok(()) => return Ok(path),
                Err(error) => {
                    let _ = std::fs::remove_dir_all(&path);
                    return Err(error);
                }
            },
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other(
        "could not create a private probe directory",
    ))
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir(path)
}

#[cfg(unix)]
fn harden_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn harden_private_dir(_: &Path) -> io::Result<()> {
    Ok(())
}

static PROBE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    enum LocalResponse {
        Complete {
            status: u16,
            body: Vec<u8>,
            delay: Duration,
        },
        Redirect {
            location: String,
        },
        Malformed,
        Stall,
    }

    struct LocalServer {
        authority: String,
        requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl LocalServer {
        fn start(responses: Vec<LocalResponse>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("local server binds");
            listener
                .set_nonblocking(true)
                .expect("local server becomes nonblocking");
            let authority = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured_requests = Arc::clone(&requests);
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = Arc::clone(&stop);
            let worker = thread::spawn(move || {
                for response in responses {
                    let mut stream = loop {
                        if stopped.load(Ordering::Relaxed) {
                            return;
                        }
                        match listener.accept() {
                            Ok((stream, _)) => {
                                stream
                                    .set_nonblocking(false)
                                    .expect("accepted stream blocks");
                                break stream;
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(1));
                            }
                            Err(_) => return,
                        }
                    };
                    let mut request = Vec::new();
                    let mut byte = [0_u8; 1];
                    while stream.read(&mut byte).unwrap_or(0) == 1 {
                        request.push(byte[0]);
                        if request.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    let path = std::str::from_utf8(&request)
                        .ok()
                        .and_then(|request| request.lines().next())
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("<malformed-request>");
                    captured_requests.lock().unwrap().push(path.to_owned());

                    match response {
                        LocalResponse::Complete {
                            status,
                            body,
                            delay,
                        } => {
                            thread::sleep(delay);
                            let _ = write!(
                                stream,
                                "HTTP/1.1 {status} test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(&body);
                        }
                        LocalResponse::Redirect { location } => {
                            let _ = write!(
                                stream,
                                "HTTP/1.1 302 test\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            );
                        }
                        LocalResponse::Malformed => {
                            let _ = stream.write_all(b"not a valid HTTP response\r\n");
                        }
                        LocalResponse::Stall => {
                            let _ = stream.write_all(
                                b"HTTP/1.1 200 test\r\nContent-Length: 4\r\nConnection: close\r\n\r\n",
                            );
                            let _ = stream.flush();
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(10)));
                            while !stopped.load(Ordering::Relaxed) {
                                let _ = stream.read(&mut byte);
                            }
                        }
                    }
                }
            });
            Self {
                authority,
                requests,
                stop,
                worker: Some(worker),
            }
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for LocalServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn test_release() -> ReleaseTag {
        ReleaseTag::new(
            Version::parse("9.8.7").unwrap(),
            ObjectId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        )
    }

    fn test_state() -> ReleaseState {
        ReleaseState::new(
            ObjectId::parse("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap(),
            vec![],
        )
        .unwrap()
    }

    fn test_gateway(server: &LocalServer) -> DocumentGateway {
        DocumentGateway::with_test_authority(&server.authority)
    }

    #[test]
    fn parser_accepts_a_valid_head_without_tags() {
        let state = parse_release_state("0123456789012345678901234567890123456789\tHEAD\n")
            .expect("a valid HEAD is sufficient when the repository has no stable tags");

        assert_eq!(
            state.head_object_id.as_str(),
            "0123456789012345678901234567890123456789"
        );
        assert!(state.releases().is_empty());
    }

    #[test]
    fn parser_skips_malformed_unrelated_and_symbolic_ref_records() {
        let state = parse_release_state(concat!(
            "ref: refs/heads/.hidden\tHEAD\n",
            "not a remote record\n",
            "not-an-object\tHEAD\n",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\trefs/heads/main\n",
            "0123456789012345678901234567890123456789\tHEAD\n",
            "not-an-object\trefs/tags/v1.2.3\n",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\trefs/tags/v2.0.0-rc1\n",
        ))
        .expect("one valid HEAD remains usable despite unrelated remote records");

        assert_eq!(
            state.head_object_id.as_str(),
            "0123456789012345678901234567890123456789"
        );
        assert!(state.releases().is_empty());
    }

    #[test]
    fn parser_retains_the_first_valid_duplicate_head_and_tag_records() {
        let state = parse_release_state(concat!(
            "ref: refs/heads/main\tHEAD\n",
            "0123456789012345678901234567890123456789\tHEAD\n",
            "1111111111111111111111111111111111111111\tHEAD\n",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\trefs/tags/v1.4.0\n",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\trefs/tags/v1.4.0\n",
            "cccccccccccccccccccccccccccccccccccccccc\trefs/tags/v1.4.0^{}\n",
            "dddddddddddddddddddddddddddddddddddddddd\trefs/tags/v1.4.0^{}\n",
        ))
        .expect("duplicate records retain the first complete facts");

        assert_eq!(
            state.head_object_id.as_str(),
            "0123456789012345678901234567890123456789"
        );
        assert_eq!(
            state.releases(),
            &[ReleaseTag::new(
                Version::parse("1.4.0").unwrap(),
                ObjectId::parse("cccccccccccccccccccccccccccccccccccccccc").unwrap(),
            )],
            "an annotated tag resolves to its first peeled commit"
        );
    }

    #[test]
    fn parser_retains_direct_and_annotated_stable_tags() {
        let state = parse_release_state(concat!(
            "0123456789012345678901234567890123456789\tHEAD\n",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\trefs/tags/v1.3.0\n",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\trefs/tags/v1.4.0\n",
            "cccccccccccccccccccccccccccccccccccccccc\trefs/tags/v1.4.0^{}\n",
        ))
        .expect("direct and annotated stable tags are retained");

        assert_eq!(
            state.releases(),
            &[
                ReleaseTag::new(
                    Version::parse("1.3.0").unwrap(),
                    ObjectId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
                ),
                ReleaseTag::new(
                    Version::parse("1.4.0").unwrap(),
                    ObjectId::parse("cccccccccccccccccccccccccccccccccccccccc").unwrap(),
                ),
            ]
        );
    }

    #[test]
    fn parser_rejects_missing_or_invalid_head() {
        for output in [
            "",
            "not-an-object\tHEAD\n",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\trefs/heads/main\n",
            "ref: refs/heads/main\tHEAD\n",
        ] {
            assert_eq!(
                parse_release_state(output),
                None,
                "no usable HEAD leaves discovery unavailable: {output:?}"
            );
        }
    }

    #[test]
    fn parser_rejects_non_utf8_output() {
        assert_eq!(
            parse_release_bytes(b"0123456789012345678901234567890123456789\tHEAD\n\xff"),
            None,
            "undecodable remote output cannot become a typed release state"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_discovery_maps_exact_cap_to_available_and_cap_plus_one_to_unavailable() {
        let response = concat!(
            "ref: refs/heads/main\tHEAD\n",
            "0123456789012345678901234567890123456789\tHEAD\n",
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        let exact = std::process::Command::new("sh")
            .args(["-c", &format!("printf '%s' '{response}'")])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("exact-cap child starts");
        assert!(matches!(
            discover_child_bounded(exact, response.len(), deadline),
            Source::Available(_)
        ));

        let over_cap = std::process::Command::new("sh")
            .args(["-c", &format!("printf '%sX' '{response}'; exec sleep 60")])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("cap-plus-one child starts");
        assert_eq!(
            discover_child_bounded(
                over_cap,
                response.len(),
                Instant::now() + Duration::from_secs(1)
            ),
            Source::Unavailable,
            "the first byte past the cap makes discovery unavailable"
        );
    }

    #[test]
    fn injected_discovery_runner_exposes_typed_unavailable_or_available_state() {
        let unavailable: DiscoveryRunner = Box::new(|_| Source::Unavailable);
        assert_eq!(unavailable(Instant::now()), Source::Unavailable);

        let available: DiscoveryRunner = Box::new(|_| {
            Source::Available(
                ReleaseState::new(
                    ObjectId::parse("0123456789012345678901234567890123456789").unwrap(),
                    vec![],
                )
                .unwrap(),
            )
        });
        assert!(matches!(available(Instant::now()), Source::Available(_)));
    }

    #[cfg(unix)]
    #[test]
    fn bounded_discovery_accepts_stdout_at_the_exact_cap() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "printf 1234"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("fake child starts with a stdout pipe");

        assert_eq!(
            capture_stdout_bounded(&mut child, 4, Instant::now() + Duration::from_secs(1)),
            Ok(b"1234".to_vec()),
            "a response exactly at the cap remains available"
        );
        assert!(
            child.try_wait().unwrap().is_some(),
            "the exact-cap child is reaped after its pipe closes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_discovery_cap_plus_one_kills_reaps_without_buffering_the_rest() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "printf 12345; exec sleep 60"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("fake child starts with a stdout pipe");

        assert_eq!(
            capture_stdout_bounded(&mut child, 4, Instant::now() + Duration::from_secs(1)),
            Err(CaptureFailure::OverCap),
            "the fifth byte rejects discovery before the sleeping child can stream more"
        );
        assert!(
            child.try_wait().unwrap().is_some(),
            "cap-plus-one must kill and reap a child that continues producing"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_discovery_deadline_kills_reaps_and_does_not_join_descendant_reader() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 1 & exec sleep 1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("fake child starts with a stdout pipe");
        let started = Instant::now();

        assert_eq!(
            capture_stdout_bounded(&mut child, 64, Instant::now() + Duration::from_millis(30)),
            Err(CaptureFailure::Deadline),
            "a child that never closes stdout cannot outlive the one discovery deadline"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "a descendant retaining stdout must not make the deadline path join indefinitely"
        );
        assert!(
            child.try_wait().unwrap().is_some(),
            "deadline expiry kills and reaps the direct child"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reader_thread_spawn_failure_kills_and_reaps_the_child() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "exec sleep 60"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("fake child starts with a stdout pipe");

        let failure = discover_child_bounded_with_spawner(
            &mut child,
            64,
            Instant::now() + Duration::from_secs(1),
            &|_| Err(io::Error::other("injected reader spawn failure")),
        );
        assert_eq!(failure, Source::Unavailable);
        assert!(
            child.try_wait().unwrap().is_some(),
            "reader spawn failure kills and reaps the already-started direct child"
        );
    }

    #[test]
    fn ls_remote_command_keeps_the_fixed_head_only_query_private_and_noninteractive() {
        use std::collections::HashMap;
        use std::ffi::{OsStr, OsString};
        use std::path::Path;

        let run_dir = Path::new("/some/private/probe-dir");
        let cmd = ls_remote_command_with_program("git", run_dir);
        let env: HashMap<OsString, Option<OsString>> = cmd
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
            .collect();

        assert_eq!(cmd.get_current_dir(), Some(run_dir));
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new("ls-remote"),
                OsStr::new(repo_url()),
                OsStr::new("HEAD"),
                OsStr::new("refs/tags/v*"),
            ],
            "the public source and head-only ref patterns are fixed"
        );
        for (key, expected) in [
            ("GIT_CEILING_DIRECTORIES", run_dir.as_os_str()),
            ("GIT_ALLOW_PROTOCOL", OsStr::new("https")),
            ("GIT_TERMINAL_PROMPT", OsStr::new("0")),
            ("GIT_HTTP_LOW_SPEED_LIMIT", OsStr::new("1000")),
            ("GIT_HTTP_LOW_SPEED_TIME", OsStr::new(PROBE_LOW_SPEED_TIME)),
        ] {
            assert_eq!(
                env.get(OsStr::new(key))
                    .and_then(|value| value.as_ref())
                    .map(OsString::as_os_str),
                Some(expected),
                "{key} is explicitly controlled for the private noninteractive probe"
            );
        }
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
        ] {
            assert!(
                env.get(OsStr::new(key)).is_some_and(Option::is_none),
                "{key} is removed so repository-local configuration cannot redirect discovery"
            );
        }
        for key in [
            "HOME",
            "XDG_CONFIG_HOME",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_ASKPASS",
            "SSH_ASKPASS",
            "GIT_CREDENTIAL_HELPER",
            "HTTPS_PROXY",
            "GIT_SSL_CAINFO",
        ] {
            assert!(
                !env.contains_key(OsStr::new(key)),
                "{key} is not overridden by probe hardening"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn ls_remote_command_inherits_proxy_and_ca_config_without_enabling_prompts() {
        use std::os::unix::fs::PermissionsExt;

        let run_dir = make_private_dir().expect("private run dir");
        let fake_git = run_dir.join("fake-git");
        let environment = run_dir.join("environment");
        std::fs::write(
            &fake_git,
            format!(
                "#!/bin/sh\nprintf '%s|%s|%s|%s|%s\\n' \"$HTTPS_PROXY\" \"$GIT_SSL_CAINFO\" \"$GIT_TERMINAL_PROMPT\" \"$GIT_ALLOW_PROTOCOL\" \"$GIT_CEILING_DIRECTORIES\" > {}\n[ \"$HTTPS_PROXY\" = \"https://proxy.invalid:8443\" ] || exit 10\n[ \"$GIT_SSL_CAINFO\" = \"/trusted/ca.pem\" ] || exit 11\n[ \"$GIT_TERMINAL_PROMPT\" = \"0\" ] || exit 12\n[ \"$GIT_ALLOW_PROTOCOL\" = \"https\" ] || exit 13\n[ \"$GIT_CEILING_DIRECTORIES\" = \"{}\" ] || exit 14\nIFS= read -r ignored && exit 15\nprintf '0123456789012345678901234567890123456789\\tHEAD\\n'\n",
                environment.display(),
                run_dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut command = ls_remote_command_with_program(&fake_git, &run_dir);
        command
            .env("HTTPS_PROXY", "https://proxy.invalid:8443")
            .env("GIT_SSL_CAINFO", "/trusted/ca.pem");
        let result = discover_with_command(command, Instant::now() + Duration::from_secs(1));
        assert_eq!(
            result,
            Source::Available(
                ReleaseState::new(
                    ObjectId::parse("0123456789012345678901234567890123456789").unwrap(),
                    Vec::new(),
                )
                .unwrap()
            ),
            "fake Git saw: {:?}",
            std::fs::read_to_string(&environment)
        );
        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[cfg(unix)]
    #[test]
    fn hardened_git_ignores_a_malicious_repo_local_insteadof() {
        // Regression: a malicious repo-local `url.*.insteadOf` must NOT rewrite the fixed public
        // source. `ls-remote --get-url` resolves without network I/O, so this is hermetic.
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "hfv-insteadof-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let evil = base.join("evil-repo");
        let clean = base.join("clean");
        std::fs::create_dir_all(&evil).unwrap();
        std::fs::create_dir_all(&clean).unwrap();

        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&evil)
            .status();
        if init.map(|status| !status.success()).unwrap_or(true) {
            let _ = std::fs::remove_dir_all(&base);
            return;
        }
        let _ = std::process::Command::new("git")
            .args([
                "config",
                "url.https://evil.invalid/.insteadOf",
                "https://github.com/",
            ])
            .current_dir(&evil)
            .status();

        let get_url = |cmd: &mut std::process::Command| -> String {
            cmd.args(["ls-remote", "--get-url", repo_url()]);
            let output = cmd.output().expect("git --get-url");
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        let mut unhardened = std::process::Command::new("git");
        unhardened.current_dir(&evil);
        assert!(
            get_url(&mut unhardened).contains("evil.invalid"),
            "precondition: the malicious repo-local insteadOf rewrites the URL"
        );

        let mut hardened = std::process::Command::new("git");
        harden_git(&mut hardened, &clean);
        assert_eq!(
            get_url(&mut hardened),
            repo_url(),
            "the fresh private directory keeps viewed-repository configuration out of the source query"
        );

        // Set `GIT_DIR` only for child commands: a viewer may inherit this redirect, but the test
        // must not mutate its parallel test process's environment.
        let parent_git_dir = std::env::var_os("GIT_DIR");
        let mut redirected = std::process::Command::new("git");
        redirected
            .current_dir(&clean)
            .env("GIT_DIR", evil.join(".git"));
        assert!(
            get_url(&mut redirected).contains("evil.invalid"),
            "precondition: command-level GIT_DIR makes Git load the evil repository configuration"
        );

        let mut hardened_redirected = std::process::Command::new("git");
        hardened_redirected.env("GIT_DIR", evil.join(".git"));
        harden_git(&mut hardened_redirected, &clean);
        assert_eq!(
            get_url(&mut hardened_redirected),
            repo_url(),
            "hardening drops inherited GIT_DIR before Git can read repository-local configuration"
        );
        assert_eq!(
            std::env::var_os("GIT_DIR"),
            parent_git_dir,
            "the regression uses command-local environment setup only"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn make_private_dir_is_fresh_empty_and_unique() {
        let first = make_private_dir().expect("first private dir");
        let second = make_private_dir().expect("second private dir");
        assert_ne!(first, second, "successive calls use never-reused paths");
        for dir in [&first, &second] {
            assert!(dir.is_dir(), "exists as a directory: {dir:?}");
            assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0, "fresh: {dir:?}");
        }
        assert_eq!(
            std::fs::create_dir(&first).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists,
            "exclusive creation never reuses a planted directory"
        );
        let _ = std::fs::remove_dir_all(first);
        let _ = std::fs::remove_dir_all(second);
    }

    #[cfg(unix)]
    #[test]
    fn make_private_dir_has_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = make_private_dir().expect("private directory");
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o077,
            0,
            "the transient Git execution directory is never group- or world-accessible"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn document_gateway_accepts_an_exact_one_mib_changelog_body() {
        let server = LocalServer::start(vec![LocalResponse::Complete {
            status: 200,
            body: vec![b'c'; DOCUMENT_MAX_BYTES],
            delay: Duration::ZERO,
        }]);

        assert_eq!(
            test_gateway(&server)
                .changelog(&test_release(), Instant::now() + Duration::from_secs(1)),
            Source::Available(Some(vec![b'c'; DOCUMENT_MAX_BYTES]))
        );
    }

    #[test]
    fn document_gateway_rejects_a_one_mib_plus_one_changelog_body() {
        let server = LocalServer::start(vec![LocalResponse::Complete {
            status: 200,
            body: vec![b'c'; DOCUMENT_MAX_BYTES + 1],
            delay: Duration::ZERO,
        }]);

        assert_eq!(
            test_gateway(&server)
                .changelog(&test_release(), Instant::now() + Duration::from_secs(1)),
            Source::Unavailable
        );
    }

    #[test]
    fn document_gateway_accepts_an_exact_one_mib_spotlight_body() {
        let server = LocalServer::start(vec![LocalResponse::Complete {
            status: 200,
            body: vec![b's'; DOCUMENT_MAX_BYTES],
            delay: Duration::ZERO,
        }]);

        assert_eq!(
            test_gateway(&server).spotlight(&test_state(), Instant::now() + Duration::from_secs(1)),
            Source::Available(Some(vec![b's'; DOCUMENT_MAX_BYTES]))
        );
    }

    #[test]
    fn document_gateway_rejects_a_one_mib_plus_one_spotlight_body() {
        let server = LocalServer::start(vec![LocalResponse::Complete {
            status: 200,
            body: vec![b's'; DOCUMENT_MAX_BYTES + 1],
            delay: Duration::ZERO,
        }]);

        assert_eq!(
            test_gateway(&server).spotlight(&test_state(), Instant::now() + Duration::from_secs(1)),
            Source::Unavailable
        );
    }

    #[test]
    fn document_gateway_uses_exact_detected_and_discovered_identities_and_official_spotlight_path()
    {
        let server = LocalServer::start(vec![
            LocalResponse::Complete {
                status: 200,
                body: b"release details".to_vec(),
                delay: Duration::ZERO,
            },
            LocalResponse::Complete {
                status: 200,
                body: b"project spotlight".to_vec(),
                delay: Duration::ZERO,
            },
        ]);
        let gateway = test_gateway(&server);
        let release = test_release();
        let state = test_state();
        let deadline = Instant::now() + Duration::from_secs(1);

        assert_eq!(
            gateway.changelog(&release, deadline),
            Source::Available(Some(b"release details".to_vec()))
        );
        assert_eq!(
            gateway.spotlight(&state, deadline),
            Source::Available(Some(b"project spotlight".to_vec()))
        );
        assert_eq!(
            server.requests(),
            vec![
                "/smarzban/herdr-file-viewer/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/CHANGELOG.md",
                "/smarzban/herdr-file-viewer/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/project-spotlight.md",
            ],
            "document URLs pin object IDs and request the official project-spotlight.md path, never alternate refs or spellings"
        );
    }

    #[test]
    fn document_gateway_treats_only_404_as_absent() {
        let server = LocalServer::start(vec![
            LocalResponse::Complete {
                status: 404,
                body: Vec::new(),
                delay: Duration::ZERO,
            },
            LocalResponse::Complete {
                status: 500,
                body: b"server error".to_vec(),
                delay: Duration::ZERO,
            },
        ]);
        let gateway = test_gateway(&server);
        let deadline = Instant::now() + Duration::from_secs(1);

        assert_eq!(
            gateway.changelog(&test_release(), deadline),
            Source::Available(None),
            "the changelog alone treats a 404 as absent"
        );
        assert_eq!(
            gateway.spotlight(&test_state(), deadline),
            Source::Unavailable,
            "all non-404 statuses are unavailable"
        );
    }

    #[test]
    fn document_gateway_rejects_redirects_without_following_them() {
        let server = LocalServer::start(vec![
            LocalResponse::Redirect {
                location: "/redirect-target".to_owned(),
            },
            LocalResponse::Complete {
                status: 200,
                body: b"must not follow".to_vec(),
                delay: Duration::ZERO,
            },
        ]);

        assert_eq!(
            test_gateway(&server)
                .changelog(&test_release(), Instant::now() + Duration::from_secs(1)),
            Source::Unavailable
        );
        assert_eq!(
            server.requests(),
            vec![
                "/smarzban/herdr-file-viewer/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/CHANGELOG.md"
            ],
            "a redirect response must not cause a second request"
        );
    }

    #[test]
    fn document_gateway_maps_malformed_responses_to_unavailable() {
        let server = LocalServer::start(vec![LocalResponse::Malformed]);

        assert_eq!(
            test_gateway(&server)
                .changelog(&test_release(), Instant::now() + Duration::from_secs(1)),
            Source::Unavailable
        );
    }

    #[test]
    fn document_gateway_stalled_body_respects_its_deadline() {
        let server = LocalServer::start(vec![LocalResponse::Stall]);
        let started = Instant::now();

        assert_eq!(
            test_gateway(&server).changelog(&test_release(), started + Duration::from_millis(50)),
            Source::Unavailable
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "a stalled body must not outlive its absolute deadline"
        );
    }

    #[test]
    fn document_gateway_uses_only_the_shared_deadline_remaining_for_sequential_requests() {
        let server = LocalServer::start(vec![
            LocalResponse::Complete {
                status: 200,
                body: b"first".to_vec(),
                delay: Duration::from_millis(100),
            },
            LocalResponse::Stall,
        ]);
        let gateway = test_gateway(&server);
        let started = Instant::now();
        let deadline = started + Duration::from_millis(250);

        assert_eq!(
            gateway.changelog(&test_release(), deadline),
            Source::Available(Some(b"first".to_vec()))
        );
        assert_eq!(
            gateway.spotlight(&test_state(), deadline),
            Source::Unavailable
        );
        assert!(
            started.elapsed() < Duration::from_millis(320),
            "the second request receives only the first request's remaining deadline"
        );
    }

    #[test]
    fn public_document_gateway_uses_fixed_https_authority_without_environment_configuration() {
        let gateway = DocumentGateway::new();
        let agent = gateway.agent(Duration::from_secs(1));

        assert_eq!(gateway.authority, DOCUMENT_AUTHORITY);
        assert!(gateway.https_only, "production documents permit HTTPS only");
        assert!(agent.config().https_only());
        assert_eq!(agent.config().max_redirects(), 0);
        assert!(
            agent.config().proxy().is_none(),
            "environment proxies cannot override production"
        );
        assert_eq!(
            agent.config().timeouts().global,
            Some(Duration::from_secs(1)),
            "each request gets its supplied remaining deadline"
        );
    }

    #[test]
    fn private_dir_hardening_failure_removes_the_fresh_directory() {
        use std::sync::{Arc, Mutex};

        let base = std::env::temp_dir().join(format!(
            "hfv-private-dir-failure-{}-{}",
            std::process::id(),
            PROBE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&base).expect("fresh test base");
        let attempted = Arc::new(Mutex::new(None));
        let recorded = Arc::clone(&attempted);

        assert!(
            make_private_dir_in(&base, move |path| {
                *recorded.lock().unwrap() = Some(path.to_owned());
                Err(io::Error::other("injected permission hardening failure"))
            })
            .is_err(),
            "a permission hardening failure makes discovery unavailable"
        );
        let created = attempted
            .lock()
            .unwrap()
            .take()
            .expect("hardening attempted");
        assert!(
            !created.exists(),
            "a directory that cannot be hardened is removed rather than left transiently usable"
        );
        let _ = std::fs::remove_dir_all(base);
    }
}
