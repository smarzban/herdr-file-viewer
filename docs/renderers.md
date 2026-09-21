# External renderers (optional)

The viewer delegates rendering to external command-line tools. These are optional runtime dependencies, not Cargo dependencies:

| View | Renderer | Install |
| --- | --- | --- |
| Rendered markdown | [`glow`](https://github.com/charmbracelet/glow) | `brew install glow` / package manager |
| Diffs | [`delta`](https://github.com/dandavison/delta) | `brew install git-delta` / `cargo install git-delta` |
| Syntax-highlighted content | [`bat`](https://github.com/sharkdp/bat) | `brew install bat` / package manager |

You can install all three with the bundled helper. Run it from the plugin directory, whose path `herdr plugin list` shows. The helper detects brew, apt, dnf, or pacman. It falls back to `cargo install` for `delta` and `bat`. Because `glow` is written in Go, the helper prints its manual installation link instead of trying Cargo:

```bash
./scripts/install-renderers.sh
```

**If a renderer is not installed, the viewer falls back to plain text.** The content pane names the missing renderer in a short notice, such as *"Markdown renderer unavailable (glow: …); showing plain text."* A missing renderer does not crash the viewer or leave the pane empty. The renderers improve the display but are not required.

The viewer sends untrusted file content to renderers on **stdin**, never as a command argument. It sanitizes renderer output before display. A hostile file name or file content therefore cannot inject a command or control the terminal.

### Bundled markdown palette

The viewer ships a small bundled markdown style palette (`assets/markdown-style.json`) that
`glow` is pointed at when it is present, so rendered markdown uses a consistent set of named
ANSI colors (headings, code blocks, links, etc.) rather than glow's built-in `dark` style.
Prose remains terminal-relative. Within the palette's fixed-color code blocks, comments and
generic subheadings meet the WCAG 4.5:1 contrast minimum against the code background. When the
palette file is absent, glow falls back to its built-in `dark` style. Markdown still renders, just
with glow's default colors. The palette is a trusted glow argument (located only inside the
plugin's own dirs), never derived from untrusted input.
