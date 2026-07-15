# External renderers (optional)

Rendering is **delegated** to best-in-class external CLIs. These are *runtime, install-time*
dependencies (not Cargo dependencies) and each is **optional**:

| View | Renderer | Install |
| --- | --- | --- |
| Rendered markdown | [`glow`](https://github.com/charmbracelet/glow) | `brew install glow` / package manager |
| Diffs | [`delta`](https://github.com/dandavison/delta) | `brew install git-delta` / `cargo install git-delta` |
| Syntax-highlighted content | [`bat`](https://github.com/sharkdp/bat) | `brew install bat` / package manager |
| `.docx` / `.odt` documents | [`pandoc`](https://pandoc.org) | `brew install pandoc` / package manager |
| `.pdf` documents | `pdftotext` ([poppler](https://poppler.freedesktop.org/)) | `brew install poppler` / package manager |
| `.pptx` / `.xlsx` documents | [`libreoffice`](https://www.libreoffice.org/) | `brew install --cask libreoffice` / package manager |

### Rendered documents

Binary office and PDF files that used to show a `[binary file]` placeholder now render: the viewer
converts the file to text with the tool above (`pandoc` emits markdown for Word/OpenDocument;
`pdftotext` and `libreoffice` emit plain text / CSV), then shows that through the **markdown**
renderer (`glow`) — so headings, lists, and tables render and wrap to the pane just like a `.md`
file. Because these converters read a *binary* file, they receive the file **path** (the one file
type that can't be piped on stdin); the path is confined to the tree root and the converter's
output is re-sanitized before display, so the trust boundary is unchanged. As with every renderer,
a missing converter degrades to a short notice naming the tool to install — never a crash.

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

### Bundled markdown palette

The viewer ships a small bundled markdown style palette (`assets/markdown-style.json`) that
`glow` is pointed at when it is present, so rendered markdown uses a consistent set of named
ANSI colors (headings, code blocks, links, etc.) rather than glow's built-in `dark` style.
When the palette file is absent, glow falls back to its built-in `dark` style. Markdown still
renders, just with glow's default colors. The palette is a trusted glow argument (located only
inside the plugin's own dirs), never derived from untrusted input.
