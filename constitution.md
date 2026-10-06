# Constitution: herdr-file-viewer

These standing principles outlast any single feature. Change them deliberately.

1. **Read-only by default.** The viewer does not mutate files or git state. Any future write
   capability must be an explicit, opt-in exception.

2. **Delegate rendering; own the experience.** Reuse mature terminal tools for markdown, diff, and
   syntax rendering. Build only the navigation, layout, git awareness, and herdr integration. Do not
   reimplement existing renderers.

3. **Git is first-class, not a mode.** Show git status and diffs throughout the tree and content
   pane instead of in a separate preview feature.

4. **Keyboard-first.** Every action is reachable from the keyboard. Mouse support is additive.

5. **Be a good plugin citizen.** herdr runs plugins unsandboxed as the user. Touch only what the
   task needs. Keep state in the plugin's own state and config directories. Drive herdr through its
   documented CLI or socket.

6. **YAGNI.** Ship the smallest change that delivers the core value. Reject scope that turns the
   viewer into a file manager or git client.
