---
name: herdr-file-viewer
description: Open a known repository file, source line, or source range in a Herdr Files pane for the user to inspect.
---

# Herdr File Viewer

Use this skill when the user asks to open, show, or reveal a file, source line, range, or function
in the Herdr file viewer (also called "Files"). Use it when you have identified a likely change
and offer to show the user where it is.

## Resolve the location

Resolve the request to a repository-relative target before launching the viewer:

- File: `src/app.rs`
- One source line: `src/app.rs:42`
- Inclusive source range: `src/app.rs:42-58`

For a function or helper, locate its definition and use its definition line, not a call site. If
there are materially different matches and context does not identify one, ask a short question
instead of guessing. For a suspected bug line, distinguish an observed failure location from a
hypothesis; do not present a guessed location as certain.

## Open it in Herdr (Linux, macOS, or WSL)

From the target repository or worktree, launch a fresh Files pane. Set `--cwd` to the agent's
current working directory so the viewer resolves the correct worktree root.

```bash
herdr plugin pane open --plugin herdr-file-viewer --entrypoint file-viewer --placement split --direction right --cwd "$PWD" --focus --env "HERDR_FILE_VIEWER_OPEN=src/app.rs:42"
```

Replace the example target with the resolved file, line, or range. The viewer loads the file and
scrolls to the requested location. A range receives a brief highlight.

Treat the target as data, never shell source. Prefer a structured argv or process API that passes
`HERDR_FILE_VIEWER_OPEN=<target>` as one argument. In a shell, do not interpolate a raw path into
command text: shell-escape it when assigning it, then expand the variable only inside double quotes.

Do not key-script the TUI. Do not close, focus, or try to retarget an existing Files pane: launch
open targets are applied only when a new viewer starts, and an existing pane may contain the user's
annotations or navigation state.

## Native Windows preview

The Windows launcher can open Files, but it does not accept an open target, and `herdr plugin pane
open` cannot start the manifest entrypoint on native Windows. For a targeted request, use WSL with
the command above. If the binary is already on `PATH`, run
`herdr-file-viewer.exe --open "<target>"` in a terminal you intend to devote to the viewer. Do not
say a generic Windows Files action opened the requested location.

Outside Herdr, if the binary is on `PATH`, run it directly:

```bash
herdr-file-viewer --open src/app.rs:42
```

## Conversation behavior

When the user directly asks to see a location, open it after resolving the target. When you have
identified a likely location while explaining a diagnosis or change, offer a short question such as
"Want me to show you where to change that setting?" Open it after the user confirms.

After launching, state the exact target you opened. If the target cannot be resolved to a real file
under the viewer root, explain that rather than opening an arbitrary or outside-root path.
