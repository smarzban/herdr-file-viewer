# External renderers (optional)

Rendering is **delegated** to best-in-class external CLIs. These are *runtime, install-time*
dependencies (not Cargo dependencies) and each is **optional**:

| View | Renderer | Install |
| --- | --- | --- |
| Rendered markdown | [`glow`](https://github.com/charmbracelet/glow) | `brew install glow` / package manager |
| Diffs | [`delta`](https://github.com/dandavison/delta) | `brew install git-delta` / `cargo install git-delta` |
| Syntax-highlighted content | [`bat`](https://github.com/sharkdp/bat) | `brew install bat` / package manager |
| **Images** (non-PNG) → PNG | [`ffmpeg`](https://ffmpeg.org/) | `brew install ffmpeg` / package manager |
| **Video** frames → PNG | [`ffmpeg`](https://ffmpeg.org/) | `brew install ffmpeg` / package manager |
| Media caption details | [`ffprobe`](https://ffmpeg.org/) (ships with ffmpeg) | included with ffmpeg |

Or install all three at once with the bundled helper (best-effort; detects brew/apt/dnf/pacman
and falls back to `cargo install` for `delta` and `bat`; `glow` is written in Go, so the helper
prints its manual install link instead of attempting a cargo install), run from the plugin dir
(`herdr plugin list` shows its path):

```bash
./scripts/install-renderers.sh
```

**If a renderer is not installed, the viewer falls back to plain text** and shows a short
notice in the content pane naming the missing capability (e.g. *“Markdown renderer
unavailable (glow: …); showing plain text.”*). The viewer never crashes or shows an empty
pane when a renderer is absent. It degrades gracefully. So the renderers are recommended for
the best experience but not required to use the viewer.

Untrusted file content is always fed to a renderer on **stdin** (never as a command argument),
and the renderer's output is re-sanitized before display, so a hostile file name or file
content cannot inject a command or drive the terminal.

### Media (images and video)

A media file's still image is shown **inline** in the content pane (via herdr's documented
graphics socket — no escape sequences are ever written, so a hostile file still cannot drive the
terminal). What ffmpeg is needed for:

- **A `.png` is shown natively** — no conversion, ffmpeg not involved — when it is no larger than
  the pane displays and fits the host's **512 KiB** limit. A bigger one is resampled to the pane's
  own pixel size with a high-quality filter (`lanczos`): those pixels could never be shown anyway,
  so nothing visible is lost, and the result usually fits the cap outright. If it still does not,
  a quality ladder re-encodes it — `lanczos`, then `neighbor` at the same size, then smaller —
  stopping at the first rung that fits, so sharpness is given up only as far as the cap forces.
  The caption always reports the file's true pixel size. Without ffmpeg there is nothing to
  resample with, so an over-cap PNG shows its caption plus a notice instead of a picture.
- **Other images** (jpg, gif, webp, svg, …) convert to PNG through ffmpeg (the `image` command,
  defaulting to `ffmpeg -loglevel error -i pipe:0 -sws_flags neighbor -vf scale={width}:{height}:force_original_aspect_ratio=decrease -f image2 -vcodec png pipe:1`). The file bytes
  are fed on **stdin**, keeping the stdin trust boundary intact. `{width}`/`{height}` are
  substituted on every call with the target box, so a replacement command must carry them.
  The default uses nearest-neighbour scaling deliberately: on screenshots and diagrams — what
  actually lives in a code repo — a smoothing filter invents intermediate colours across flat
  regions and can make the re-encoded PNG *larger than the original*, while neighbour keeps text
  crisp and roughly halves the bytes. Set `-sws_flags area` instead if you mostly view photographs.
- **Video** (mp4, mkv, webm, mov, …) decodes frames through the `video` command template
  (default `ffmpeg -loglevel error -re -ss {start} -i {name} -an -vf scale={width}:{height} -r {fps}
  -f image2pipe -vcodec png -`, with the pane's pixel budget substituted for `{width}`/`{height}`).
  `p` plays/pauses, `{`/`}` seek ±5s, `0` restarts. A replacement command should keep `-re`: it
  makes ffmpeg read at the input's native rate, so frames arrive at roughly the speed they can be
  displayed. Without it ffmpeg races to the end of the file (a 7-second clip decodes in under half
  a second), the queue discards almost every frame, and playback appears to stop immediately. The
  poster frame and playback are decoded at the same target size, so pressing `p` never resizes the
  picture.
- **A caption above the picture** reports what you are looking at, e.g.
  `[image: 3008×1546 · PNG · 8-bit RGBA · 655 KiB]` or
  `[video: 854×480 · 0:07 · HEVC · 982 KiB · p to play]`. Colour depth and type come from the PNG
  header directly; a video's resolution, codec, and duration come from `ffprobe`, and are simply
  omitted when it is unavailable. The caption always reports the file's OWN size — if the picture
  had to be re-encoded smaller to fit the host's cap, a `shown at W×H` clause says so. The picture
  is placed *below* the caption, never over it.

When ffmpeg is absent, media files show a placeholder plus a notice naming ffmpeg (use
`config.example.toml` / the `image` / `video` keys to point at an alternative converter). The
`media_max_kib` config key bounds how large a media file may be before the Media view shows a
placeholder instead.

**Playback rate is host-limited.** herdr re-renders its full client frame for every `set`, which
measures ~120 ms of fixed cost regardless of payload size — so practical video tops out around
**8 fps**. This is herdr's ceiling, not the viewer's; frames are capped at ~150 KiB so video
remains comfortably inside herdr's 512 KiB decoded-image limit. See `ARCHITECTURE.md` for the
measured table.

### Bundled markdown palette

The viewer ships a small bundled markdown style palette (`assets/markdown-style.json`) that
`glow` is pointed at when it is present, so rendered markdown uses a consistent set of named
ANSI colors (headings, code blocks, links, etc.) rather than glow's built-in `dark` style.
When the palette file is absent, glow falls back to its built-in `dark` style. Markdown still
renders, just with glow's default colors. The palette is a trusted glow argument (located only
inside the plugin's own dirs), never derived from untrusted input.
