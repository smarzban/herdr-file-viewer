# Constitution — herdr-file-viewer

Standing principles for this project. These outlast any single feature; change them
deliberately.

1. **Read-only by default.** The viewer observes; it does not mutate files or git
   state. Any future write capability is an explicit, opt-in exception, never a
   surprise.

2. **Delegate rendering; own the experience.** Reuse mature terminal tools for
   markdown, diff, and syntax rendering. Build only the differentiated shell
   (navigation, layout, git-awareness, herdr integration). Don't reinvent solved
   problems.

3. **Git is first-class, not a mode.** Git status and diffs are woven through the tree
   and content pane — not a separate preview feature.

4. **Keyboard-first.** Every action is reachable from the keyboard. Mouse support, if
   any, is additive.

5. **Be a good plugin citizen.** herdr runs plugins unsandboxed, as the user. Touch
   only what the task needs, keep state in the plugin's own state/config dirs, and
   drive herdr through its documented CLI/socket — never around it.

6. **YAGNI.** Ship the smallest thing that delivers the core value. Resist scope that
   turns a viewer into a file manager or a git client.
