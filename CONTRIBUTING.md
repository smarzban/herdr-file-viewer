# Contributing

Thanks for your interest in herdr-file-viewer! Bug reports, feature requests, and pull requests are
all welcome.

## Reporting bugs & requesting features

[Open an issue](https://github.com/smarzban/herdr-file-viewer/issues) — there are templates for a
**bug report** and a **feature request**. For a bug, the plugin version (`?` overlay → About) and
your herdr version help a lot.

## Development setup

Requirements: **Rust 1.96+** (edition 2024) and **git** on `PATH`. The renderers (`glow` / `delta` /
`bat`) are optional at runtime and not needed to build or test.

```bash
cargo test                 # unit + integration + e2e (pty) tests
cargo build --release      # what herdr's [[build]] step runs at install time
cargo run                  # run the viewer locally, outside herdr
```

Keep the deterministic tier green — CI enforces it:

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo audit
```

The e2e tests drive the real binary over a pseudo-terminal; they stub the editor via `$EDITOR` and
run in temporary directories, so they need neither glow/delta/bat nor a live herdr.

## Making a change

- **`main` is protected.** Open a PR; CI must be green (rustfmt + clippy, the test matrix on
  Ubuntu/macOS × Rust 1.96/stable, and `cargo audit`). PRs are squash-merged.
- **Commit convention: [Conventional Commits](https://www.conventionalcommits.org/)** — `feat:`,
  `fix:`, `chore:`, `test:`, `docs:`, as in the git log.
- **Docs are part of done.** A user-facing change updates the docs in the *same* PR: a `CHANGELOG.md`
  entry, plus the relevant page(s) — the [keys reference](docs/keys.md), the
  [usage guide](docs/usage.md), or [configuration](docs/configuration.md).
- **Read-only by design.** The viewer never mutates files or git state (see `constitution.md`); any
  write capability is an explicit, opt-in exception, never a default.

## More

`AGENTS.md` is the full cross-agent contributor guide (build/test/verify workflow, conventions, and
the release process). The [`docs/`](docs/README.md) tree holds the user-facing documentation.
