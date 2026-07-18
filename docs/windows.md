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
  ids are the Linux/macOS variants). Point your herdr keybinding at the `-windows` id:
  `command = "herdr plugin action invoke open-file-viewer-windows --plugin herdr-file-viewer"`.
- **A `prefix+f` keybinding needs herdr v0.7.2 or newer.** herdr runs custom-command
  (`[[keys.command]]`) keybindings through the platform shell; before v0.7.2 that was `/bin/sh`,
  absent on Windows, so the binding silently did nothing there. herdr **v0.7.2** runs them through
  `cmd.exe /d /c`, so the `prefix+f` binding fires normally. On older herdr, summon the viewer
  by invoking the action **directly** (`herdr plugin action invoke open-file-viewer-windows
  --plugin herdr-file-viewer` from a shell, or via herdr's action menu) rather than through a
  keybinding.
- **Requires herdr's preview channel.** Windows herdr binaries ship only on herdr's pre-release
  update channel, so you need to be on it before installing this plugin on Windows.
- **Non-ASCII paths and pane titles are supported.** The launchers force UTF-8 before parsing
  herdr's JSON under Windows PowerShell 5.1, so names outside the active legacy code page do not
  make the viewer fall back to its plugin install directory.
- **PowerShell and Git Bash/zsh pane shells are both supported.** `pane run` types into whatever
  `terminal.default_shell` is (PowerShell when unset). The launchers read that setting from
  `%APPDATA%\herdr\config.toml` and emit either `& "…\hfv.exe"` (PowerShell) or `"…/hfv.exe"`
  (bash/zsh). They also install a short `%USERPROFILE%\bin\hfv.exe` shim and mirror
  `assets/markdown-style.json` beside it so glow still finds its style when launched from the shim.
- **Git 2.41+ is required for status markers and branch display.** The viewer passes
  `--attr-source=…` on every git call; older Git for Windows (e.g. 2.39) rejects that flag and
  silently degrades to a plain tree.
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
