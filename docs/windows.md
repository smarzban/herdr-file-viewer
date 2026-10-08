# Windows (preview)

Native Windows (`x86_64-pc-windows-msvc`) support is a **preview**, matching herdr's Windows
support. The crate builds and the test suite runs as an advisory `windows-latest` CI job.
Installation follows the Linux and macOS flow. `herdr plugin install` downloads a SHA-256-verified
prebuilt binary through `scripts/fetch-or-build.ps1` or falls back to `cargo build --release`. It
requires no extra tooling beyond Windows PowerShell 5.1. PowerShell launcher scripts implement the
open and toggle actions.

- **On Windows, bind the `-windows` action ids.** herdr requires every action id to be unique, so
  the Windows launchers register as **`open-file-viewer-windows`** and
  **`open-file-viewer-tab-windows`** (the unqualified `open-file-viewer` / `open-file-viewer-tab`
  ids are the Linux/macOS variants). Point `plugin_action` bindings at the qualified Windows ids:

  ```toml
  [[keys.command]]
  key = "prefix+f"
  type = "plugin_action"
  command = "herdr-file-viewer.open-file-viewer-windows"
  description = "open file viewer in split"

  [[keys.command]]
  key = "prefix+shift+f"
  type = "plugin_action"
  command = "herdr-file-viewer.open-file-viewer-tab-windows"
  description = "open file viewer in tab"
  ```
- **Requires herdr's preview channel.** Windows herdr binaries ship only on herdr's pre-release
  update channel, so you need to be on it before installing this plugin on Windows.
- **Non-ASCII paths and pane titles are supported.** The launchers force UTF-8 before parsing
  herdr's JSON under Windows PowerShell 5.1, so names outside the active legacy code page do not
  make the viewer fall back to its plugin install directory.
- **Preview support does not guarantee parity.** The project's required CI gate has no Windows host;
  the `windows-latest` job is advisory. A Windows-specific regression can therefore land between
  releases. Full feature parity with Linux and macOS is the goal. [Open an
  issue](https://github.com/smarzban/herdr-file-viewer/issues) for a Windows-specific problem.
- **Open at another directory is Linux/macOS only for now.** `open-file-viewer-at` and
  `open-file-viewer-at-tab` (and their root-picker popup) are not declared for Windows; use WSL.
- **`e` falls back to Notepad.** With neither `editor` nor `$EDITOR` set, the editor hand-off opens
  `%SystemRoot%\System32\notepad.exe`.
- **WSL needs no extra setup.** The Linux (`x86_64-unknown-linux-musl`) binary runs unmodified
  inside WSL. Install herdr and this plugin from the WSL distribution as you would on native Linux.

See also [install & updating](install.md) for the shared install flow and [summoning](summoning.md)
for the open actions and launcher.
