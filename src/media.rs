//! Media — pure decisions about images and video, with no I/O.
//!
//! Everything here is a pure function over already-in-hand data (a file name, some bytes, the
//! pane geometry), so the whole module is unit-testable on every platform without a socket, a
//! graphics host, or an ffmpeg binary. The enclosing pipeline (see `graphics.rs`, the render
//! worker, and the controller's `media_shown` discipline) does the actual work.

use crate::graphics::{CellMetrics, Placement};
use ratatui::layout::Rect;

/// Video playback: the ffmpeg decoder + bounded drop-oldest queue.
pub mod player;

/// What kind of media a file is, from its extension. One view mode covers all three: only the
/// *payload production* differs (PNG bytes are sent as-is; other images convert; video decodes
/// managed frames).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// A `.png` — sent natively, no conversion required.
    Png,
    /// Any other image that must be converted to PNG to reach the host.
    Image,
    /// A video: a still from frame 0 for the preview, then selectable playback.
    Video,
}

impl MediaKind {
    /// Classify a path by its (case-insensitive) extension. Unknown extensions return `None` —
    /// the file is not media at all. Modelled on `controller::is_markdown`.
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "png" => Some(MediaKind::Png),
            // The images herdr can host after a PNG conversion. jpg/jpeg, gif, webp, bmp, tiff,
            // svg, avif, heic, ico. (A missing converter degrades to the placeholder + notice.)
            "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "svg" | "avif" | "heic"
            | "heif" | "ico" | "jxl" => Some(MediaKind::Image),
            // The container formats ffmpeg can decode for playback. mp4, mkv, webm, mov, avi,
            // m4v, mpg, mpeg, flv, wmv, ogv, ts, 3gp.
            "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "mpg" | "mpeg" | "flv" | "wmv"
            | "ogv" | "ts" | "3gp" | "mts" | "m2ts" => Some(MediaKind::Video),
            _ => None,
        }
    }

    /// A short human label for the text fallback line (`[image: …]` / `[video: …]`).
    pub fn label(self) -> &'static str {
        match self {
            MediaKind::Png | MediaKind::Image => "image",
            MediaKind::Video => "video",
        }
    }
}

/// Parse the pixel dimensions of a PNG from its IHDR chunk.
///
/// Returns `None` for anything that is not a well-formed PNG header: a file too short to hold
/// the 24-byte header, a wrong magic signature, a first chunk that is not `IHDR`, or a
/// non-positive dimension. Malformed input is `None`, never a panic — the caller then falls back
/// to converting via ffmpeg or showing the placeholder.
pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 {
        return None;
    }
    const MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes[..8] != MAGIC {
        return None;
    }
    if bytes[12..16] != *b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

/// A PNG's colour description, read from the same IHDR chunk as its dimensions.
///
/// Byte 24 is the bit depth and byte 25 the colour type, so this costs nothing beyond the header
/// read we already do — no decoder, no subprocess. Returns `None` for a header we cannot parse or
/// a colour type outside the PNG spec's five.
pub fn png_colour(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 26 || png_dimensions(bytes).is_none() {
        return None;
    }
    let depth = bytes[24];
    let kind = match bytes[25] {
        0 => "grey",
        2 => "RGB",
        3 => "indexed",
        4 => "grey+alpha",
        6 => "RGBA",
        _ => return None,
    };
    Some(format!("{depth}-bit {kind}"))
}

/// Human-readable byte size, e.g. `655 KiB` / `1.4 MiB`. Used in the media info line.
pub fn human_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    match bytes {
        b if b >= MIB => format!("{:.1} MiB", b as f64 / MIB as f64),
        b if b >= KIB => format!("{} KiB", b / KIB),
        b => format!("{b} B"),
    }
}

/// A duration in seconds as `m:ss` (or `h:mm:ss` past an hour), for the video info line.
pub fn human_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "?".to_string();
    }
    let total = seconds.round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The pixel size of a cell rectangle, the budget an image (or a video frame) must fit within.
///
/// This is the number handed to ffmpeg's `scale` filter (via `force_original_aspect_ratio`-style
/// fit), and the denominator against which `fit` measures "does this image already fit?".
/// Clamped so the decoded size (4 bytes/px) stays under the host's [`MAX_IMAGE_BYTES`] cap — a
/// pane-sized budget at retina cell metrics would otherwise happily exceed what herdr accepts, and
/// every video frame would be rejected as `image_too_large`. Saturated at the pixel cap too, so an
/// absurd configured cell size still yields a finite budget.
pub fn frame_budget(cell_rect: Rect, cell_px: CellMetrics) -> (u32, u32) {
    let w = cell_rect.width as u32 * cell_px.cell_width_px;
    let h = cell_rect.height as u32 * cell_px.cell_height_px;
    clamp_pixels_to_cap((w, h))
}

/// Shrink a pixel box until it plausibly encodes under the host's byte cap, preserving aspect.
///
/// Factored out of [`frame_budget`] so the still preview and the playback decoder can be handed
/// the *same* number. They used to disagree — the still was hardcoded to 640x360 while playback
/// used the pane-derived budget — so a video visibly changed size the moment you pressed play.
pub fn clamp_pixels_to_cap(box_px: (u32, u32)) -> (u32, u32) {
    let mut w = box_px.0 as u64;
    let mut h = box_px.1 as u64;
    // Pixel cap derived from the host's byte cap, then the box is fitted inside it preserving the
    // pane's aspect (so neither dimension alone exceeds the budget).
    //
    // The divisor is bytes-per-pixel for the ENCODED frame, not a raw RGBA buffer. Measured: a
    // 640x360 PNG video frame is ~84 KiB, i.e. ~0.36 B/px. The original 4 B/px assumed raw RGBA
    // and capped frames at ~362x362 — needlessly soft, since we transmit PNG. 2 B/px keeps a
    // ~5x margin over the measurement while roughly doubling each edge; an unusually busy frame
    // that still overshoots is rejected by the host and skipped, which costs one frame of
    // playback rather than correctness.
    let max_px = crate::graphics::MAX_IMAGE_BYTES as u64 / 2;
    if w.saturating_mul(h) > max_px {
        let scale = (max_px as f64 / (w as f64 * h as f64)).sqrt();
        w = (w as f64 * scale).floor().max(1.0) as u64;
        h = (h as f64 * scale).floor().max(1.0) as u64;
        debug_assert!(
            w.saturating_mul(h) <= max_px,
            "budget clamp must satisfy the decoded cap"
        );
    }
    let max = u32::MAX as u64;
    (w.min(max) as u32, h.min(max) as u32)
}

/// The pixel size to re-encode an over-cap image at, so it lands under the host's byte cap.
///
/// PNG size tracks pixel count closely for the screenshots and diagrams that actually appear in a
/// repo, so scaling both edges by `sqrt(cap / actual)` targets the cap directly. The extra 0.85
/// leaves headroom, because resampling can *hurt* compression (it introduces intermediate colours
/// a flat-region screenshot didn't have). Callers re-measure the result and shrink again if the
/// estimate missed — this is a starting point, not a guarantee.
///
/// Never upscales: an image already under the cap returns its own dimensions.
pub fn downscale_target(dims: (u32, u32), actual_bytes: usize, cap: usize) -> (u32, u32) {
    if actual_bytes <= cap {
        return dims; // already fits — the margin is for shrinking, not for shaving a fitting image
    }
    let ratio = (cap as f64 / actual_bytes.max(1) as f64).sqrt() * 0.85;
    let scale = |v: u32| ((v as f64 * ratio).floor() as u32).max(1);
    (scale(dims.0), scale(dims.1))
}

/// Aspect-preserving fit of an image into the pane's cell grid: as large as the content box allows.
///
/// Returns the cell rectangle the image occupies, anchored at `cell_rect`'s origin (the content
/// pane's inner top-left) and grown until one axis touches the box.
///
/// **Display size is deliberately independent of the byte budget.** [`frame_budget`] bounds how
/// many pixels we may *transmit*; the host then scales whatever it receives into the rect named
/// here, so a byte-limited image still fills the pane. Conflating the two is what made every
/// picture render postage-stamp sized — the ~512x256 transmission clamp was being used as the
/// display box.
///
/// Still never upscales past the image's natural pixel size: a 16x16 favicon stays a favicon
/// rather than being blown up into a blurry wall.
pub fn fit(image_px: (u32, u32), cell_px: CellMetrics, cell_rect: Rect) -> Placement {
    let box_w = cell_rect.width as f64 * cell_px.cell_width_px.max(1) as f64;
    let box_h = cell_rect.height as f64 * cell_px.cell_height_px.max(1) as f64;
    let scale = (box_w / image_px.0.max(1) as f64)
        .min(box_h / image_px.1.max(1) as f64)
        .min(1.0); // never upscale: a small icon stays small
    let fit_w = (image_px.0 as f64 * scale).ceil().max(1.0) as u64;
    let fit_h = (image_px.1 as f64 * scale).ceil().max(1.0) as u64;
    let cols = fit_w.div_ceil(cell_px.cell_width_px.max(1) as u64) as u32;
    let rows = fit_h.div_ceil(cell_px.cell_height_px.max(1) as u64) as u32;
    Placement {
        grid_cols: cols.min(cell_rect.width as u32).max(1),
        grid_rows: rows.min(cell_rect.height as u32).max(1),
        viewport_col: cell_rect.x as i32,
        viewport_row: cell_rect.y as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(w: u32, h: u32) -> CellMetrics {
        CellMetrics {
            cell_width_px: w,
            cell_height_px: h,
        }
    }

    // -- downscale target --------------------------------------------------

    #[test]
    fn downscale_target_leaves_an_already_small_image_alone() {
        // Never upscale: a 400 KiB image under a 512 KiB cap keeps its own dimensions.
        assert_eq!(
            downscale_target((800, 600), 400 * 1024, 512 * 1024),
            (800, 600)
        );
    }

    #[test]
    fn downscale_target_shrinks_both_edges_toward_the_cap() {
        // 2 MiB against a 512 MiB… no: against a 512 KiB cap is a 4x overage, so each edge scales
        // by sqrt(1/4)=0.5, times the 0.85 safety margin → 0.425.
        let (w, h) = downscale_target((4000, 2000), 2048 * 1024, 512 * 1024);
        assert_eq!((w, h), (1700, 850));
        assert!(
            (w as f64 / h as f64 - 2.0).abs() < 0.01,
            "aspect ratio is preserved: {w}x{h}"
        );
    }

    #[test]
    fn downscale_target_never_returns_a_zero_dimension() {
        // A pathological overage must still yield something ffmpeg can scale to, not 0x0.
        let (w, h) = downscale_target((10, 4), usize::MAX, 1);
        assert!(w >= 1 && h >= 1, "got {w}x{h}");
    }

    #[test]
    fn a_large_image_fills_the_content_box_rather_than_the_transmission_budget() {
        // THE REGRESSION: `fit` used to size the placement from `frame_budget`, which is clamped
        // to the host's ~512 KiB transmission cap (~362x362 px). Display and transmission are
        // independent — the host rescales whatever it receives into the rect we name — so that
        // clamp rendered every picture postage-stamp sized inside a big pane.
        let cells = metrics(20, 41); // the probed retina metrics
        let rect = Rect::new(30, 2, 60, 30); // a 1200x1230 px content box
        let p = fit((3008, 1546), cells, rect);

        // 3008x1546 is wider than it is tall relative to the box, so width is the binding axis.
        assert_eq!(
            p.grid_cols, 60,
            "the image spans the full width of the content box"
        );
        assert!(
            p.grid_rows >= 15,
            "and takes a proportional share of the height, not a clamped sliver: {p:?}"
        );
        assert!(
            p.grid_rows <= rect.height as u32,
            "never taller than the box"
        );
        assert_eq!(
            (p.viewport_col, p.viewport_row),
            (30, 2),
            "anchored at the box origin"
        );
    }

    // -- classification ----------------------------------------------------

    #[test]
    fn extensions_classify_case_insensitively() {
        for (name, kind) in [
            ("a.png", MediaKind::Png),
            ("a.PNG", MediaKind::Png),
            ("a.jpg", MediaKind::Image),
            ("a.JPEG", MediaKind::Image),
            ("a.gif", MediaKind::Image),
            ("a.webp", MediaKind::Image),
            ("a.svg", MediaKind::Image),
            ("a.mp4", MediaKind::Video),
            ("a.MKV", MediaKind::Video),
            ("a.mov", MediaKind::Video),
            ("a.webm", MediaKind::Video),
        ] {
            assert_eq!(
                MediaKind::from_path(std::path::Path::new(name)),
                Some(kind),
                "extension {name}"
            );
        }
    }

    #[test]
    fn unknown_extensions_are_not_media() {
        for name in ["a.txt", "a", "a.md", "a.png.bak", "dir/"] {
            assert_eq!(
                MediaKind::from_path(std::path::Path::new(name)),
                None,
                "path {name:?}"
            );
        }
    }

    // -- PNG IHDR parsing ---------------------------------------------------

    /// A minimal, well-formed PNG header (24 bytes): magic + IHDR length + `IHDR` + 8×8.
    fn png_header(w: u32, h: u32) -> Vec<u8> {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        b.extend_from_slice(&13u32.to_be_bytes());
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b
    }

    #[test]
    fn png_dimensions_read_the_ihdr_fields() {
        assert_eq!(png_dimensions(&png_header(1920, 1080)), Some((1920, 1080)));
        assert_eq!(png_dimensions(&png_header(8, 8)), Some((8, 8)));
        // Dimensions are big-endian regardless of host byte order.
        assert_eq!(
            png_dimensions(&png_header(0x0001_0203, 7)),
            Some((0x0001_0203, 7))
        );
    }

    #[test]
    fn png_dimensions_reject_truncated_or_malformed_input() {
        // One byte short of the IHDR fields (23 of 24 bytes).
        let truncated = &png_header(4, 4)[..23];
        assert_eq!(png_dimensions(truncated), None);
        // Same length, wrong magic.
        assert_eq!(png_dimensions(b"not a png at all at all"), None);
        assert_eq!(png_dimensions(&[]), None);
        assert_eq!(png_dimensions(&png_header(0, 4)), None); // zero width
        assert_eq!(png_dimensions(&png_header(4, 0)), None); // zero height
        // A valid header whose first *content* chunk is not IHDR isn't a displayable PNG.
        let mut wrong_type = png_header(4, 4);
        wrong_type[12..16].copy_from_slice(b"PLTE");
        assert_eq!(png_dimensions(&wrong_type), None);
    }

    // -- fit / frame budget ------------------------------------------------

    #[test]
    fn frame_budget_is_the_cell_rect_in_pixels_capped_to_the_decoded_image_budget() {
        // A small pane is the raw cell-rect product (200×410 px = 82 KB decoded, under the cap).
        let budget = frame_budget(Rect::new(0, 0, 10, 10), metrics(20, 41));
        assert_eq!(budget, (200, 410));
        // A retina full-pane budget would exceed 512 KiB encoded (the host cap), so it is
        // clamped: at most MAX_IMAGE_BYTES/2 pixels, aspect preserved and under the cap.
        let huge = frame_budget(Rect::new(0, 0, 400, 200), metrics(20, 41));
        assert!(huge.0 as u64 * huge.1 as u64 <= crate::graphics::MAX_IMAGE_BYTES as u64 / 2);
        assert!(
            (huge.0 as f64 / huge.1 as f64 - 400.0 * 20.0 / (200.0 * 41.0)).abs() < 0.05,
            "aspect preserved under the clamp: {huge:?}"
        );
        // Even a modest pane exceeds the decoded cap at retina cell sizes — the clamp is not
        // only for absurd panes (this is the load-bearing measurement behind 512 KiB).
        let modest = frame_budget(Rect::new(0, 0, 80, 24), metrics(20, 41));
        assert!(modest.0 as u64 * modest.1 as u64 <= crate::graphics::MAX_IMAGE_BYTES as u64 / 2);
        // Pinned to the 2 B/px encoded estimate (was 461x283 under the old, needlessly
        // pessimistic 4 B/px raw-RGBA assumption — see `frame_budget`).
        assert_eq!(modest, (652, 401));
        // Zero cells yield zero pixels, never a divide-by-zero downstream.
        assert_eq!(frame_budget(Rect::new(0, 0, 0, 0), metrics(20, 41)), (0, 0));
    }

    #[test]
    fn fit_preserves_aspect_and_stays_in_the_cell_rect() {
        let cell_px = metrics(20, 41);
        let pane = Rect::new(0, 0, 80, 24);
        // A 16:9 landscape image in a wide pane: width is the binding constraint.
        let landscape = fit((1920, 1080), cell_px, pane);
        assert!(landscape.grid_cols <= pane.width as u32);
        assert!(landscape.grid_rows <= pane.height as u32);
        // grid_cols × cell_w ≈ 1920 and grid_rows × cell_h ≈ 1080 at the same scale.
        let aspect = 1920.0 / 1080.0;
        let placed = (landscape.grid_cols * 20) as f64 / (landscape.grid_rows * 41) as f64;
        assert!(
            (placed - aspect).abs() < 0.15,
            "placed aspect {placed} drifts from {aspect}"
        );
    }

    #[test]
    fn fit_never_upscales_a_small_image() {
        let pane = Rect::new(0, 0, 80, 24);
        let icon = fit((32, 32), metrics(20, 41), pane);
        // 32px in 20px cells → 2 cols; in 41px rows → 1 row. Never blown up to fill the pane.
        assert_eq!(icon.grid_cols, 2);
        assert_eq!(icon.grid_rows, 1);
    }

    #[test]
    fn fit_anchors_at_the_cell_rect_origin() {
        let p = fit((100, 100), metrics(20, 41), Rect::new(3, 5, 80, 24));
        assert_eq!(p.viewport_col, 3);
        assert_eq!(p.viewport_row, 5);
    }

    #[test]
    fn fit_clamps_to_at_least_one_cell() {
        let p = fit((1, 1), metrics(20, 41), Rect::new(0, 0, 10, 10));
        assert_eq!(p.grid_cols, 1);
        assert_eq!(p.grid_rows, 1);
    }
}
