# Windows (preview)

Native Windows (`x86_64-pc-windows-msvc`) is supported as a **preview**, mirroring herdr's own
posture there: the crate builds, the test suite runs (advisory) on `windows-latest` CI, and
install works the same way as Linux/macOS: `herdr plugin install` downloads a SHA-256-verified
prebuilt binary (via `scripts/fetch-or-build.ps1`) or falls back to `cargo build --release`, no
extra tooling required beyond the in-box Windows PowerShell 5.1. The open/toggle actions work via
PowerShell launcher scripts.

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
- **The overlay and popup layouts aren't available on native Windows in this release.** The two
  Windows actions remain `open-file-viewer-windows` and `open-file-viewer-tab-windows`, unchanged;
  there are no `-windows` variants of `open-file-viewer-overlay` / `open-file-viewer-popup`, because
  the Windows launchers spawn the viewer by absolute path from PowerShell rather than through the
  placement-aware `plugin pane open`. Split and tab behave exactly as before. WSL users get all
  four layouts — WSL runs the Linux path.
- **Requires herdr's preview channel.** Windows herdr binaries ship only on herdr's pre-release
  update channel, so you need to be on it before installing this plugin on Windows.
- **Non-ASCII paths and pane titles are supported.** The launchers force UTF-8 before parsing
  herdr's JSON under Windows PowerShell 5.1, so names outside the active legacy code page do not
  make the viewer fall back to its plugin install directory.
- **Preview means best-effort, not a parity guarantee.** There's no Windows host in this
  project's CI gate (the `windows-latest` job is advisory, not required), so a Windows-specific
  regression can land between releases. Full feature parity with Linux/macOS is the goal, not a
  promise. Please [open an issue](https://github.com/smarzban/herdr-file-viewer/issues) if you
  hit a Windows-specific problem.
- **WSL works today, with zero extra setup.** If you'd rather not wait on native-Windows preview
  maturity, the existing Linux (`x86_64-unknown-linux-musl`) binary already runs unmodified
  inside WSL. Install herdr and this plugin from within your WSL distro exactly as you would on
  native Linux.

See also [install & updating](install.md) for the shared install flow and [summoning](summoning.md)
for the open actions and launcher.
