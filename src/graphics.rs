//! Graphics Host — inline images in the content pane, via herdr's documented socket API.
//!
//! The viewer never writes graphics escape sequences to its own stdout. Instead it asks the
//! host to place an image, over the same unix socket herdr's own CLI uses
//! (`pane.graphics.set` / `.clear` / `.info`). Two consequences worth stating plainly:
//!
//! - **The AC-27 escape-neutralizer is untouched.** Image bytes travel base64-encoded inside a
//!   JSON request; no `ESC` byte is ever emitted. A hostile file still cannot drive the terminal
//!   (`SECURITY.md`), and [`crate::render::to_text`] keeps sanitizing every byte that becomes text.
//! - **Placement is data, not cursor choreography.** We hand herdr a cell rect, so ratatui's
//!   differential redraw and the image never fight over the cursor.
//!
//! Measured against herdr 0.8.0 (protocol 19) — see [`MAX_IMAGE_BYTES`] and [`GraphicsWorker`]
//! for the two numbers that shape everything here.

use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// The largest image herdr accepts, in **decoded** bytes, for any format.
///
/// Measured by bisection against a live herdr 0.8.0: 511.4 KiB was accepted and 513.5 KiB was
/// rejected with `{"code": "image_too_large"}`, identically for `png` and `rgb`. Beyond roughly
/// 1 MiB of base64 the server stops answering and closes the connection outright, which surfaces
/// here as [`GraphicsError::Transport`] rather than a clean error code — so callers must treat a
/// transport failure as "too big / gone" and degrade, never panic.
pub const MAX_IMAGE_BYTES: usize = 512 * 1024;

/// How long a single socket round-trip may take before we give up on it.
///
/// Generous on purpose: the measured worst case for a legal payload was ~310 ms, and this runs on
/// the graphics worker thread where a stall costs nothing but a late image. Bounding it at all is
/// what matters — an unbounded read would wedge the worker for good if herdr stopped answering.
const CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// The pixel size of one terminal cell, from `pane.graphics.info`.
///
/// Needed to turn a ratatui [`Rect`](ratatui::layout::Rect) (in cells) into a pixel budget for the
/// image. Retina terminals report large values — Ghostty reported 20×41 on the probed machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellMetrics {
    pub cell_width_px: u32,
    pub cell_height_px: u32,
}

/// Where the image sits, in terminal cells, relative to the pane viewport.
///
/// `viewport_col`/`viewport_row` are signed so an image may be scrolled partly off the top or
/// left edge without the caller having to clamp it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub grid_cols: u32,
    pub grid_rows: u32,
    pub viewport_col: i32,
    pub viewport_row: i32,
}

/// The wire formats herdr accepts.
///
/// Only [`Format::Png`] is used. `rgb`/`rgba` are also accepted by the host but measured *slower*
/// at every size (the payload is simply larger, and herdr's decode was never the bottleneck), and
/// they hit the same [`MAX_IMAGE_BYTES`] cap far sooner — a 420×420 raw `rgb` frame is already
/// over it. The variant list mirrors the host's enum so a future need is a one-line change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
}

impl Format {
    fn as_str(self) -> &'static str {
        match self {
            Format::Png => "png",
        }
    }
}

/// One image ready to hand to the host: the encoded bytes plus where to put them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub placement: Placement,
}

/// Why a graphics call could not be completed. Every variant is a "degrade gracefully" signal —
/// the caller shows the text placeholder and a notice, and the viewer carries on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphicsError {
    /// No host to talk to: not running under herdr, or the pane id / socket path is unknown.
    /// Distinguished from the others because it is the *expected* state outside herdr and so
    /// earns a gentler notice than a genuine failure.
    Unavailable,
    /// The host rejected the image for exceeding [`MAX_IMAGE_BYTES`].
    TooLarge,
    /// The host answered with an error object (e.g. `pane_not_found`).
    Host(String),
    /// The socket could not be reached, timed out, or closed mid-request.
    Transport(String),
}

impl std::fmt::Display for GraphicsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GraphicsError::Unavailable => write!(f, "no herdr graphics host"),
            GraphicsError::TooLarge => write!(f, "image too large for the host"),
            GraphicsError::Host(m) => write!(f, "host error: {m}"),
            GraphicsError::Transport(m) => write!(f, "transport error: {m}"),
        }
    }
}

// ---------------------------------------------------------------------------
// The seam the rest of the app depends on
// ---------------------------------------------------------------------------

/// Synchronous access to the host's graphics surface.
///
/// Behind a trait so tests substitute a recorder and never open a socket. Implementations block
/// for the duration of a round-trip (~150 ms for a real image — see [`GraphicsWorker`]), so this
/// is never called from the UI thread directly.
pub trait GraphicsHost: Send {
    fn info(&self) -> Result<CellMetrics, GraphicsError>;
    fn set(&self, frame: &Frame) -> Result<(), GraphicsError>;
    fn clear(&self) -> Result<(), GraphicsError>;
}

/// What the controller wants on screen right now.
///
/// Each command is **absolute state**, never a delta — `Show` means "this image, here", `Hide`
/// means "nothing". That is precisely what makes the worker's last-wins collapse
/// ([`GraphicsWorker`]) correct rather than merely convenient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphicsCommand {
    Show(Box<Frame>),
    Hide,
}

/// A non-blocking outbox for [`GraphicsCommand`]s.
///
/// The controller holds one of these and never waits: sending is fire-and-forget so a 150 ms
/// socket round-trip can't land on the input thread. Tests use a recorder that captures commands
/// synchronously. [`close`](Self::close) exists so teardown can *block* until the desired end
/// state ("nothing") has actually reached the host before the process exits — the worker joins
/// its thread, the null/no-host sink is already done.
pub trait GraphicsSink: Send {
    fn send(&self, command: GraphicsCommand);
    /// Flush everything that has been sent and release the sink. The default is appropriate for a
    /// sink that has nothing to drain; [`GraphicsWorker`] overrides it to join its thread.
    fn close(&mut self) {}
}

// ---------------------------------------------------------------------------
// The worker: keeps slow socket calls off the UI thread
// ---------------------------------------------------------------------------

/// Owns the one long-lived thread that talks to the host.
///
/// **Why a thread at all.** A `pane.graphics.set` round-trip was measured at ~120–155 ms for any
/// non-trivial image and plateaus there (the cost is herdr's full client-frame re-render, not our
/// payload size). Calling that from the event loop would stall input for a sixth of a second per
/// image and make video playback impossible, so every call is dispatched here instead.
///
/// **Why last-wins collapsing.** The worker drains its whole backlog and executes only the final
/// command, exactly as the render worker collapses stale render jobs. This is sound *because*
/// [`GraphicsCommand`] carries absolute state: if three frames queue up while one is in flight,
/// showing only the newest is not an approximation, it is the correct result. Without it a slow
/// host would build an unbounded queue of frames nobody will ever see.
pub struct GraphicsWorker {
    tx: Option<mpsc::Sender<GraphicsCommand>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl GraphicsWorker {
    /// Spawn the worker around `host`. The thread lives until the sender is dropped (see
    /// [`GraphicsSink::close`] — teardown calls it so the final `Hide` is flushed, not lost in a
    /// fire-and-forget race with process exit).
    pub fn spawn(host: Box<dyn GraphicsHost>) -> Self {
        let (tx, rx) = mpsc::channel::<GraphicsCommand>();
        let handle = std::thread::spawn(move || {
            while let Ok(mut command) = rx.recv() {
                // Collapse the backlog: only the newest command describes the desired state.
                while let Ok(newer) = rx.try_recv() {
                    command = newer;
                }
                let _ = match command {
                    GraphicsCommand::Show(frame) => host.set(&frame),
                    GraphicsCommand::Hide => host.clear(),
                };
            }
        });
        Self {
            tx: Some(tx),
            handle: Some(handle),
        }
    }
}

impl GraphicsSink for GraphicsWorker {
    fn send(&self, command: GraphicsCommand) {
        // A dead worker is not worth surfacing: the image simply doesn't appear, and the text
        // placeholder underneath is still correct.
        let _ = self.tx.as_ref().map(|tx| tx.send(command));
    }

    fn close(&mut self) {
        // Teardown must leave the pane clean: the end state is "nothing". Send a final Hide so
        // the desired state is flushed through the same last-wins collapse (a queued Show after
        // us would be wrong anyway — this is the exit path), then drop the sender so the worker's
        // `recv` returns `Err(Disconnected)` and it exits, and join so the Hide has actually been
        // delivered to the host before we return to `run`'s teardown.
        let _ = self.tx.take().map(|tx| tx.send(GraphicsCommand::Hide));
        let _ = self.handle.take().map(|h| h.join());
    }
}

/// A sink that drops everything, for when there is no host (outside herdr, or on Windows).
pub struct NullSink;

impl GraphicsSink for NullSink {
    fn send(&self, _command: GraphicsCommand) {}
}

// ---------------------------------------------------------------------------
// Pure protocol helpers — testable on every platform, no socket involved
// ---------------------------------------------------------------------------

/// Resolve this process's own pane id.
///
/// herdr sets `HERDR_PANE_ID` for every pane process it launches, which is authoritative for
/// *our* pane — unlike `herdr pane current`, which reports the pane the user is looking at and
/// may belong to someone else entirely. Taken as a parameter rather than read here so the
/// resolution logic is testable without touching the real environment.
pub fn pane_id_from_env(var: Option<String>) -> Option<String> {
    var.filter(|id| !id.is_empty())
}

/// Build the JSON request line for `pane.graphics.set`.
///
/// Separate from the transport so the exact wire shape is asserted in a unit test on every
/// platform. Verified against herdr 0.8.0 (protocol 19):
/// `{"id":…,"method":"pane.graphics.set","params":{pane_id, format, image_width, image_height,
/// data_base64, placement:{grid_cols, grid_rows, viewport_col, viewport_row}}}`.
pub fn set_request(pane_id: &str, frame: &Frame) -> String {
    let params = serde_json::json!({
        "pane_id": pane_id,
        "format": frame.format.as_str(),
        "image_width": frame.width,
        "image_height": frame.height,
        "data_base64": encode_base64(&frame.data),
        "placement": {
            "grid_cols": frame.placement.grid_cols,
            "grid_rows": frame.placement.grid_rows,
            "viewport_col": frame.placement.viewport_col,
            "viewport_row": frame.placement.viewport_row,
        },
    });
    request_line("fv:set", "pane.graphics.set", params)
}

/// Build the JSON request line for `pane.graphics.clear`.
pub fn clear_request(pane_id: &str) -> String {
    request_line(
        "fv:clear",
        "pane.graphics.clear",
        serde_json::json!({ "pane_id": pane_id }),
    )
}

/// Build the JSON request line for `pane.graphics.info`.
pub fn info_request(pane_id: &str) -> String {
    request_line(
        "fv:info",
        "pane.graphics.info",
        serde_json::json!({ "pane_id": pane_id }),
    )
}

/// The envelope every request shares: `{"id":…,"method":…,"params":…}` plus the trailing newline
/// that terminates a message on this socket.
fn request_line(id: &str, method: &str, params: serde_json::Value) -> String {
    let request = serde_json::json!({ "id": id, "method": method, "params": params });
    format!("{request}\n")
}

/// Interpret a response line: `{"result":…}` on success, `{"error":{"code","message"}}` otherwise.
///
/// The `image_too_large` code is promoted to [`GraphicsError::TooLarge`] because callers act on
/// it (re-encode smaller) rather than merely reporting it.
pub fn parse_response(line: &str) -> Result<serde_json::Value, GraphicsError> {
    let value: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| GraphicsError::Transport(format!("unparsable response: {e}")))?;
    if let Some(result) = value.get("result") {
        return Ok(result.clone());
    }
    let code = value
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(|c| c.as_str())
        .unwrap_or("unknown");
    if code == "image_too_large" {
        return Err(GraphicsError::TooLarge);
    }
    let message = value
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or(code);
    Err(GraphicsError::Host(message.to_string()))
}

/// Read [`CellMetrics`] out of a `pane_graphics_info` result.
pub fn parse_cell_metrics(result: &serde_json::Value) -> Result<CellMetrics, GraphicsError> {
    let field = |name: &str| {
        result
            .get(name)
            .and_then(|v| v.as_u64())
            .filter(|v| *v > 0)
            .ok_or_else(|| GraphicsError::Host(format!("missing {name}")))
    };
    Ok(CellMetrics {
        cell_width_px: field("cell_width_px")? as u32,
        cell_height_px: field("cell_height_px")? as u32,
    })
}

/// Standard base64 with padding.
///
/// Hand-rolled rather than pulled in as a crate: it is twenty lines, and the house style treats a
/// new dependency as a deliberate decision (see `AGENTS.md`, "Minimal-deps house style") — the
/// test suite rolls its own temp dirs for the same reason.
pub fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

// ---------------------------------------------------------------------------
// LiveGraphics — the real socket client (unix only)
// ---------------------------------------------------------------------------

/// The real [`GraphicsHost`]: one unix-socket round-trip per call.
///
/// **One request per connection.** Verified against herdr 0.8.0: the server writes its response
/// and closes the stream, so a second write on the same connection fails with `EPIPE`. We
/// therefore reconnect for every call rather than holding a session open.
pub struct LiveGraphics {
    socket: PathBuf,
    pane_id: String,
}

impl LiveGraphics {
    /// Build a client from the environment, or `None` when this process has no host to talk to
    /// (not launched by herdr, or on a platform without unix sockets).
    ///
    /// Taking both values as parameters keeps the availability rule testable; [`from_env`] is the
    /// thin wrapper that reads the real environment.
    ///
    /// [`from_env`]: LiveGraphics::from_env
    pub fn new(socket_path: Option<String>, pane_id: Option<String>) -> Option<Self> {
        if !cfg!(unix) {
            return None;
        }
        let socket = socket_path.filter(|p| !p.is_empty())?;
        let pane_id = pane_id_from_env(pane_id)?;
        Some(Self {
            socket: PathBuf::from(socket),
            pane_id,
        })
    }

    /// [`LiveGraphics::new`] against the real process environment.
    pub fn from_env() -> Option<Self> {
        Self::new(
            std::env::var("HERDR_SOCKET_PATH").ok(),
            std::env::var("HERDR_PANE_ID").ok(),
        )
    }

    #[cfg(unix)]
    fn round_trip(&self, request: &str) -> Result<serde_json::Value, GraphicsError> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let transport = |e: io::Error| GraphicsError::Transport(e.to_string());
        let mut stream = UnixStream::connect(&self.socket).map_err(transport)?;
        stream
            .set_read_timeout(Some(CALL_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(CALL_TIMEOUT)))
            .map_err(transport)?;
        stream
            .write_all(request.as_bytes())
            .and_then(|()| stream.flush())
            .map_err(transport)?;

        let mut line = String::new();
        // An empty read means the server closed without answering — what an over-sized payload
        // does past roughly 1 MiB of base64. Report it as the size problem it almost always is.
        match BufReader::new(&stream).read_line(&mut line) {
            Ok(0) => Err(GraphicsError::Transport(
                "host closed the connection".into(),
            )),
            Ok(_) => parse_response(line.trim_end()),
            Err(e) => Err(transport(e)),
        }
    }

    #[cfg(not(unix))]
    fn round_trip(&self, _request: &str) -> Result<serde_json::Value, GraphicsError> {
        Err(GraphicsError::Unavailable)
    }
}

impl GraphicsHost for LiveGraphics {
    fn info(&self) -> Result<CellMetrics, GraphicsError> {
        parse_cell_metrics(&self.round_trip(&info_request(&self.pane_id))?)
    }

    fn set(&self, frame: &Frame) -> Result<(), GraphicsError> {
        // Reject locally what the host would reject anyway: this saves shipping up to a megabyte
        // of base64 across the socket only to be told `image_too_large`.
        if frame.data.len() > MAX_IMAGE_BYTES {
            return Err(GraphicsError::TooLarge);
        }
        self.round_trip(&set_request(&self.pane_id, frame))
            .map(|_| ())
    }

    fn clear(&self) -> Result<(), GraphicsError> {
        self.round_trip(&clear_request(&self.pane_id)).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn frame(data: Vec<u8>) -> Frame {
        Frame {
            format: Format::Png,
            width: 4,
            height: 2,
            data,
            placement: Placement {
                grid_cols: 10,
                grid_rows: 5,
                viewport_col: 2,
                viewport_row: -1,
            },
        }
    }

    // -- base64 -------------------------------------------------------------

    #[test]
    fn base64_matches_the_rfc4648_test_vectors() {
        // The canonical vectors, so a hand-rolled encoder can't drift: padding at every residue.
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode_base64(input.as_bytes()), expected, "input {input:?}");
        }
    }

    #[test]
    fn base64_covers_the_whole_byte_range_including_the_high_indices() {
        // 0xFB..0xFF exercise the '+' and '/' end of the alphabet, which a truncated table
        // would silently get wrong for real (binary) PNG data.
        let all: Vec<u8> = (0u8..=255).collect();
        let encoded = encode_base64(&all);
        assert_eq!(encoded.len(), 344, "4 chars per 3 bytes, padded");
        assert!(encoded.starts_with("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g"));
        // 240..=255 contains 0xF0..=0xFF: the 'w' onwards half of the alphabet plus '+' and '/'.
        assert!(encoded.ends_with("8PHy8/T19vf4+fr7/P3+/w=="));
    }

    // -- request shape ------------------------------------------------------

    #[test]
    fn set_request_matches_the_verified_wire_shape() {
        // Pinned against herdr 0.8.0 (protocol 19), probed live during design. If herdr renames
        // a field this fails here rather than silently painting nothing.
        let line = set_request("wJ:p1", &frame(b"foo".to_vec()));
        assert!(line.ends_with('\n'), "messages are newline-terminated");
        let v: serde_json::Value = serde_json::from_str(line.trim_end()).expect("valid JSON");
        assert_eq!(v["method"], "pane.graphics.set");
        assert_eq!(v["params"]["pane_id"], "wJ:p1");
        assert_eq!(v["params"]["format"], "png");
        assert_eq!(v["params"]["image_width"], 4);
        assert_eq!(v["params"]["image_height"], 2);
        assert_eq!(v["params"]["data_base64"], "Zm9v");
        assert_eq!(v["params"]["placement"]["grid_cols"], 10);
        assert_eq!(v["params"]["placement"]["grid_rows"], 5);
        assert_eq!(v["params"]["placement"]["viewport_col"], 2);
        assert_eq!(
            v["params"]["placement"]["viewport_row"], -1,
            "a partly scrolled-off image sends a negative row rather than being clamped"
        );
        assert!(v["id"].is_string(), "the envelope requires a string id");
    }

    #[test]
    fn clear_and_info_requests_target_the_pane() {
        for (line, method) in [
            (clear_request("wJ:p1"), "pane.graphics.clear"),
            (info_request("wJ:p1"), "pane.graphics.info"),
        ] {
            let v: serde_json::Value = serde_json::from_str(line.trim_end()).expect("valid JSON");
            assert_eq!(v["method"], method);
            assert_eq!(v["params"]["pane_id"], "wJ:p1");
        }
    }

    // -- response parsing ---------------------------------------------------

    #[test]
    fn parse_response_reads_a_result() {
        let ok = parse_response(r#"{"id":"m","result":{"type":"ok"}}"#).expect("a result");
        assert_eq!(ok["type"], "ok");
    }

    #[test]
    fn image_too_large_is_promoted_to_its_own_variant() {
        // Callers re-encode smaller on this specific code, so it must not be lumped in with
        // generic host errors. The code string is herdr's, observed live.
        assert_eq!(
            parse_response(r#"{"id":"m","error":{"code":"image_too_large","message":"…"}}"#),
            Err(GraphicsError::TooLarge)
        );
    }

    #[test]
    fn other_host_errors_keep_their_message() {
        assert_eq!(
            parse_response(r#"{"id":"m","error":{"code":"pane_not_found","message":"pane x"}}"#),
            Err(GraphicsError::Host("pane x".into()))
        );
    }

    #[test]
    fn malformed_json_is_a_transport_error_not_a_panic() {
        assert!(matches!(
            parse_response("not json at all"),
            Err(GraphicsError::Transport(_))
        ));
    }

    #[test]
    fn cell_metrics_reject_missing_or_zero_fields() {
        let good = serde_json::json!({"cell_width_px": 20, "cell_height_px": 41});
        assert_eq!(
            parse_cell_metrics(&good),
            Ok(CellMetrics {
                cell_width_px: 20,
                cell_height_px: 41
            })
        );
        // Zero would make the fit maths divide by zero, so it is rejected at the boundary.
        for bad in [
            serde_json::json!({"cell_width_px": 0, "cell_height_px": 41}),
            serde_json::json!({"cell_height_px": 41}),
        ] {
            assert!(matches!(
                parse_cell_metrics(&bad),
                Err(GraphicsError::Host(_))
            ));
        }
    }

    // -- availability -------------------------------------------------------

    #[test]
    fn a_client_needs_both_a_socket_and_a_pane_id() {
        assert!(LiveGraphics::new(Some("/s".into()), Some("wJ:p1".into())).is_some() == cfg!(unix));
        for (socket, pane) in [
            (None, Some("wJ:p1".to_string())),
            (Some("/s".to_string()), None),
            (Some(String::new()), Some("wJ:p1".to_string())),
            (Some("/s".to_string()), Some(String::new())),
        ] {
            assert!(
                LiveGraphics::new(socket.clone(), pane.clone()).is_none(),
                "empty or missing values must not produce a half-configured client: \
                 socket={socket:?} pane={pane:?}"
            );
        }
    }

    #[test]
    fn an_oversized_frame_is_rejected_without_touching_the_socket() {
        // The socket path is deliberately nonexistent: if the size guard did not short-circuit,
        // this would fail with Transport instead of TooLarge.
        let Some(client) =
            LiveGraphics::new(Some("/nonexistent/herdr.sock".into()), Some("p".into()))
        else {
            return; // non-unix: no client to test
        };
        let big = frame(vec![0u8; MAX_IMAGE_BYTES + 1]);
        assert_eq!(client.set(&big), Err(GraphicsError::TooLarge));
    }

    // -- worker collapsing --------------------------------------------------

    #[derive(Default)]
    struct Recorder {
        calls: Arc<Mutex<Vec<String>>>,
        gate: Option<Arc<Mutex<()>>>,
        /// Signalled just before the worker parks on `gate`, so a test can wait for the first
        /// `set` to have *started* without sleeping (a synchronous, observable tell).
        arrived_tx: Option<mpsc::Sender<()>>,
    }

    impl GraphicsHost for Recorder {
        fn info(&self) -> Result<CellMetrics, GraphicsError> {
            Err(GraphicsError::Unavailable)
        }
        fn set(&self, frame: &Frame) -> Result<(), GraphicsError> {
            self.notify_arrived();
            // Holding the gate models a slow host, so a backlog provably builds up behind the
            // first call rather than us hoping the scheduler produces one.
            let _held = self.gate.as_ref().map(|g| g.lock().unwrap());
            self.calls
                .lock()
                .unwrap()
                .push(format!("set:{}", frame.width));
            Ok(())
        }
        fn clear(&self) -> Result<(), GraphicsError> {
            let _held = self.gate.as_ref().map(|g| g.lock().unwrap());
            self.calls.lock().unwrap().push("clear".into());
            Ok(())
        }
    }

    impl Recorder {
        fn notify_arrived(&self) {
            if let Some(tx) = &self.arrived_tx {
                let _ = tx.send(());
            }
        }
    }

    #[test]
    fn the_worker_collapses_a_backlog_to_the_newest_command() {
        // Force the race rather than hope for it (AGENTS.md): the gate is held here, so the
        // worker blocks inside the FIRST set while the next three commands queue behind it.
        // Releasing it must yield exactly the first and the last — the first (`set:1`) ran
        // because the worker had already started it when the backlog formed, then the queued
        // 2/3/4 collapse to the newest (`set:4`). A timing-based version of this test would
        // pass (or fail) vacuously depending on the scheduler.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(Mutex::new(()));
        let held = gate.lock().unwrap();
        let (arrived_tx, arrived_rx) = mpsc::channel();

        let worker = GraphicsWorker::spawn(Box::new(Recorder {
            calls: Arc::clone(&calls),
            gate: Some(Arc::clone(&gate)),
            arrived_tx: Some(arrived_tx),
        }));
        let mut first = frame(vec![1]);
        first.width = 1;
        worker.send(GraphicsCommand::Show(Box::new(first)));
        // Synchronous tell: this returns only once the worker has STARTED the first set and is
        // parked on the gate, so the commands below are guaranteed to queue rather than race
        // ahead of it. No sleep, no yield loop.
        arrived_rx.recv().expect("worker must start the first set");

        for width in [2u32, 3, 4] {
            let mut f = frame(vec![1]);
            f.width = width;
            worker.send(GraphicsCommand::Show(Box::new(f)));
        }
        drop(held);

        // Bounded, not timed: wait for the terminal state to appear rather than measuring how
        // long it took. A stuck worker fails by timing out the loop, not by a flaky margin.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if calls.lock().unwrap().last().is_some_and(|c| c == "set:4") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker never reached the newest command: {:?}",
                calls.lock().unwrap()
            );
            std::thread::yield_now();
        }
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["set:1".to_string(), "set:4".to_string()],
            "the first ran (already started when the backlog formed), then the queued 2/3/4 \
             collapse to the newest — widths 2 and 3 never appear"
        );
    }

    #[test]
    fn hide_is_absolute_state_so_it_survives_collapsing_after_a_show() {
        // Show-then-Hide must end hidden. This is the property that makes last-wins correct.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let worker = GraphicsWorker::spawn(Box::new(Recorder {
            calls: Arc::clone(&calls),
            gate: None,
            arrived_tx: None,
        }));
        worker.send(GraphicsCommand::Show(Box::new(frame(vec![1]))));
        worker.send(GraphicsCommand::Hide);

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if calls.lock().unwrap().last().is_some_and(|c| c == "clear") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker never cleared: {:?}",
                calls.lock().unwrap()
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn the_null_sink_accepts_and_drops_everything() {
        // Outside herdr this is the whole graphics path; it must never panic.
        NullSink.send(GraphicsCommand::Show(Box::new(frame(vec![1]))));
        NullSink.send(GraphicsCommand::Hide);
    }
}
