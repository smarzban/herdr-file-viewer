//! Video playback — a long-lived, cancellable ffmpeg decoder feeding a bounded drop-oldest queue.
//!
//! The controller owns one [`Decoder`] while a video is selected. Spawning ffmpeg, reading its
//! PNG frames, and bounding the queue are all this module's job; pacing (`tick_media`) and the
//! graphics placement belong to the controller. No ffmpeg binary is required by tests — the
//! decoder command is injected (the `video` renderer), so a fake can stand in.

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Frames queued ahead of the consumer (the ~125 ms tick). The consumer can only eat ~1/tick, so
/// a tiny window is all that is needed; keeping it bounded (drop-oldest) means a slow consumer
/// can never balloon memory with a backlog nobody will ever see.
const QUEUE_CAPACITY: usize = 4;

/// Substitutes `{start}`, `{fps}`, `{width}`, `{height}` in the configured `video` command
/// template. The file path is substituted separately (`render::with_video_name`), so a hostile
/// name can never reach ffmpeg other than as its own sanitized argv element.
pub fn substitute(
    template: &[String],
    start: &str,
    fps: &str,
    width: &str,
    height: &str,
) -> Vec<String> {
    template
        .iter()
        .map(|arg| {
            arg.replace("{start}", start)
                .replace("{fps}", fps)
                .replace("{width}", width)
                .replace("{height}", height)
        })
        .collect()
}

/// A decoded frame: raw PNG bytes, ready to hand to the graphics host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    pub png: Vec<u8>,
}

/// The bounded drop-oldest queue a decoder thread feeds.
///
/// Unbounded `mpsc` would let ffmpeg outrun the consumer forever (the measured host ceiling is
/// ~8 fps, so the decoder WILL outrun it); a fixed-capacity deque that evicts the OLDEST on
/// overflow is the plan's "bounded, drop-oldest" requirement in concrete form.
#[derive(Debug, Clone)]
pub(crate) struct FrameQueue {
    inner: Arc<Mutex<std::collections::VecDeque<DecodedFrame>>>,
}

impl FrameQueue {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        }
    }

    fn push(&self, frame: DecodedFrame) {
        let mut q = self.inner.lock().expect("frame queue");
        if q.len() >= QUEUE_CAPACITY {
            q.pop_front(); // drop the OLDEST: a backlog nobody will ever see
        }
        q.push_back(frame);
    }

    fn is_empty(&self) -> bool {
        self.inner.lock().expect("frame queue").is_empty()
    }

    /// Test-only: pop the OLDEST queued frame (the framing the fixture asserts). The live reader
    /// (`next_ready`) inverts this to "newest wins" — the fixture exercises the splitter, so it
    /// reads in arrival order.
    #[cfg(test)]
    fn pop_front_(&self) -> Option<DecodedFrame> {
        self.inner.lock().expect("frame queue").pop_front()
    }
}

/// The live frame source: an ffmpeg child process streaming PNG frames on stdout, owned by the
/// decoder thread that reads and splits it, pushing into a [`FrameQueue`].
pub struct Decoder {
    queue: FrameQueue,
    stop_tx: mpsc::Sender<()>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Decoder {
    /// Spawn ffmpeg with `command` (the `video` renderer argv, `{name}` already replaced with
    /// the file path) and start reading its stdout.
    pub fn spawn(command: &[String]) -> Option<Self> {
        let prog = command.first()?;
        let mut cmd = Command::new(prog);
        cmd.args(&command[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let queue = FrameQueue::new();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let queue_reader = queue.clone();
        let handle = std::thread::spawn(move || {
            decode_loop(cmd, stop_rx, queue_reader);
        });
        Some(Self {
            queue,
            stop_tx,
            handle: Some(handle),
        })
    }

    /// The newest ready frame, if any. Drop-oldest at READ time too: if a backlog formed between
    /// ticks, only the newest matters (a `GraphicsCommand` carries absolute state).
    pub fn next_ready(&self) -> Option<DecodedFrame> {
        let mut q = self.queue.inner.lock().expect("frame queue");
        let newest = q.pop_back();
        q.clear(); // discard any older queued frames
        newest
    }

    /// Whether the decoder thread has finished and the queue has drained — the player has nothing
    /// left to show.
    pub fn finished(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| h.is_finished()) && self.queue.is_empty()
    }

    /// Stop the decoder and join its thread (a seek/close/leave). Best-effort: a wedged child is
    /// killed by the loop when the stop signal lands.
    pub fn stop(&mut self) {
        let _ = self.stop_tx.send(());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Read ffmpeg's stdout, split the piped bytes into individual PNG frames, and push them into the
/// queue. Re-synchronizes on the PNG signature (0x89 P N G \r \n 0x1a \n): bytes before a
/// mid-stream signature are garbage (a pipe-read boundary, ffmpeg noise) and dropped. On EOF, a
/// trailing buffer that plausibly ends a PNG is flushed as the final frame.
fn decode_loop(mut cmd: Command, stop_rx: mpsc::Receiver<()>, queue: FrameQueue) {
    use std::io::Read;

    let Ok(mut child) = cmd.spawn() else {
        return; // decoder never started — the player reports "no frames"
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };

    let mut buf: Vec<u8> = Vec::new();
    let mut read = [0u8; 64 * 1024];
    loop {
        if stop_rx.try_recv().is_ok() {
            let _ = child.kill();
            break;
        }
        match stdout.read(&mut read) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&read[..n]);
                // A frame is complete when its IEND trailer lands (see emit_complete_frames), so
                // the last frame is emitted live, not at EOF — nothing to flush here.
                emit_complete_frames(&mut buf, &queue);
            }
            Err(_) => break,
        }
    }
    let _ = crate::proc::terminate_and_reap(&mut child);
}

/// Emit every "complete" PNG in `buf`, leaving any trailing partial in place for the next read.
/// A frame is complete when another signature follows it; the trailing run is flushed at exit.
fn emit_complete_frames(buf: &mut Vec<u8>, queue: &FrameQueue) {
    loop {
        // Garbage before the first signature (a partial pipe read) — drop it.
        if let Some(first) = find_sig(buf)
            && first > 0
        {
            buf.drain(..first);
            continue;
        }
        match find_sig_next(buf) {
            // The first frame ends where the NEXT signature begins.
            Some(next) => {
                queue.push(DecodedFrame {
                    png: buf[..next].to_vec(),
                });
                buf.drain(..next);
            }
            None => {
                // No successor signature YET — but a frame is also complete when it both starts
                // with the signature and ends with its IEND trailer, so the LAST frame does not
                // have to wait for EOF (and a trailing frame is never delayed by one latency).
                if buf.starts_with(&PNG_SIG) && ends_png(buf) {
                    queue.push(DecodedFrame { png: buf.clone() });
                    buf.clear();
                }
                return;
            }
        }
    }
}

const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

fn find_sig(hay: &[u8]) -> Option<usize> {
    hay.windows(8).position(|w| w == PNG_SIG)
}

/// The offset of the NEXT PNG signature after the first.
fn find_sig_next(hay: &[u8]) -> Option<usize> {
    (1..hay.len()).find(|&i| hay[i..].starts_with(&PNG_SIG))
}

/// Cheap "this buffer ends a PNG" check: the IEND chunk is the file's 12-byte trailer (`00 00 00
/// 00 49 45 4E 44` + CRC), so `IEND` sits 8 bytes from the end. Only used to recognize a complete
/// frame mid-stream; a truncated trailer is still "incoming" and the frame waits for more bytes.
fn ends_png(buf: &[u8]) -> bool {
    buf.len() >= 24 && &buf[buf.len() - 8..buf.len() - 4] == b"IEND"
}

/// The pacing interval between frames the controller tick enforces — mirrors the measured herdr
/// display ceiling of ~8 fps for any non-trivial frame (the graphics module's Table 1). Pulling
/// faster would only refill the drop-oldest queue with frames the host cannot paint.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(125);

#[cfg(test)]
mod tests {
    use super::*;

    fn sig() -> Vec<u8> {
        PNG_SIG.to_vec()
    }

    /// A minimal well-formed PNG (signature + IHDR + a fake-but-plausible trailer), enough for the
    /// splitter's plumbing even though only the split boundaries matter here.
    fn png(tag: u8) -> Vec<u8> {
        let mut b = sig();
        b.extend_from_slice(&[tag, tag, tag, tag]);
        b.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D']);
        b.extend_from_slice(&[0, 0, 0, 0]);
        b
    }

    #[test]
    fn substitute_replaces_every_video_field() {
        let template: Vec<String> = [
            "ffmpeg",
            "-ss",
            "{start}",
            "-i",
            "{name}",
            "-vf",
            "scale={width}:{height}",
            "-r",
            "{fps}",
            "-",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let got = substitute(&template, "1.5", "8", "320", "180");
        assert_eq!(got[2], "1.5");
        assert_eq!(got[6], "scale=320:180");
        assert_eq!(got[8], "8");
        assert_eq!(got[4], "{name}", "'name' is substituted later");
    }

    #[test]
    fn bounded_queue_drops_the_oldest_and_read_returns_newest_first() {
        let q = FrameQueue::new();
        for i in 0..(QUEUE_CAPACITY + 3) {
            q.push(DecodedFrame { png: vec![i as u8] });
        }
        // Capacity is kept: frames 0..2 were evicted, 3..6 remain.
        let mut inner = q.inner.lock().unwrap();
        assert_eq!(inner.len(), 4, "capacity holds after overflow");
        // The read side ("oldest first" here is the VecDeque pop_front; the controller's
        // `next_ready` re-shapes this into newest-first drop-oldest).
        assert_eq!(inner.pop_front().map(|f| f.png[0]), Some(3));
    }

    #[test]
    fn frame_splitter_reassembles_a_concatenated_png_stream_from_fragments() {
        let png_a = png(1);
        let png_b = png(2);
        let stream = [&png_a[..], &png_b[..]].concat();

        let queue = FrameQueue::new();
        let mut buf = Vec::new();
        for byte in &stream {
            buf.push(*byte);
            emit_complete_frames(&mut buf, &queue);
        }
        // Sliced delivery still yields both frames once each successor's signature appears.
        let mut frames = Vec::new();
        while let Some(f) = queue.pop_front_() {
            frames.push(f.png);
        }
        assert_eq!(frames, vec![png_a, png_b]);
    }

    #[test]
    fn frame_splitter_drops_garbage_before_a_mid_stream_signature() {
        let stream = [vec![0xff, 0xfe, 0xfd], sig(), vec![7, 7]].concat();
        let queue = FrameQueue::new();
        let mut buf = Vec::new();
        for byte in &stream {
            buf.push(*byte);
            emit_complete_frames(&mut buf, &queue);
        }
        // No second signature ever arrives → nothing is "complete" → the garbage+partial never
        // becomes a frame.
        assert!(
            queue.pop_front_().is_none(),
            "garbage never becomes a frame"
        );
    }
}
