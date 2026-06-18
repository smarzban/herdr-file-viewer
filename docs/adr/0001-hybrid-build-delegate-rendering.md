# ADR 0001 — Hybrid build: own the shell, delegate rendering

- Status: accepted
- Date: 2026-06-16

## Context

The viewer needs three kinds of rendering — markdown, git diffs, and
syntax-highlighted source — plus a git-aware navigation shell (recursive tree,
two-column layout, status markers, diff baselines) integrated with herdr's worktree
context. Mature terminal CLIs already render markdown, diffs, and syntax well. No
off-the-shelf tool delivers the git-first-class, herdr-context-aware navigation we
want.

## Decision

Build the navigation/layout/git shell ourselves; delegate all content rendering to
existing terminal CLIs invoked by the shell. Compose-an-existing-file-manager and
build-everything-custom were both considered and rejected.

## Consequences

- We own the differentiated, hard part and reuse the commodity, solved part — the best
  control-to-effort trade.
- We take a dependency on external render tools: their presence, versions, and output
  formats become part of our contract (mitigated at the techstack stage by pinning and
  detecting them).
- Rendering fidelity is bounded by those tools; deep customization of, say, diff
  styling means configuring them, not rewriting them.
- Reversible only at real cost: replacing delegated rendering with in-process rendering
  later is a significant rewrite. Hence this record.
