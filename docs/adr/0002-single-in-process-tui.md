# ADR 0002 — A single in-process TUI owns both columns (not herdr-native split panes)

- Status: accepted
- Date: 2026-06-17

## Context

The viewer shows a file tree and a content pane side by side, where moving the selection
in the tree immediately re-renders the content pane. There are two shapes: herdr places
the plugin across native panes (a tree pane + a content pane that coordinate), or the
plugin runs as one process that draws both columns inside a single pane.

## Decision

Run the viewer as a single process that owns and draws both columns within one herdr
split pane. herdr's role is limited to launching that one pane (AC-17) and, separately,
opening hand-off panes for the editor (AC-19).

## Consequences

- Tree↔content selection shares in-process state — no per-keystroke coordination over
  the herdr socket. Keeps interaction responsive (AC-22, AC-23) and the model simple.
- We own the layout, including the narrow-split focus-toggle (AC-21) — more UI code than
  leaning on herdr's pane manager.
- The viewer is a single unit to launch, place, and tear down.
- Reversible only at real cost: splitting into two coordinating panes later would be a
  rewrite of the UI and its state model. Hence this record.
- Independent of ADR-0001: rendering is still delegated to external tools; this decision
  only fixes who owns the layout.
