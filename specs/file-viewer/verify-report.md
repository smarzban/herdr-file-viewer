# Readiness report: herdr-file-viewer

- Status: gate re-run (readiness-check) — 2026-06-18 (run 2)
- Read: `brief.md`, `acceptance-criteria.md`, `design.md`, `techstack.md`, `plan.md`,
  `constitution.md`, `CONTEXT.md`
- Wrote: this file only. No other artifact modified by the gate.
- **Verdict: ✅ READY TO BUILD** — 0 Critical, 0 High. One accepted Low (L-2) remains;
  non-blocking.

## Run-2 result vs run-1 findings

| ID | Run-1 | Status now | How resolved (owning stage) |
| --- | --- | --- | --- |
| H-1 | High — AC-17 live oracle had no task | **Resolved** | criteria: AC-17 oracle → `manual`; plan: added **T-25** (manual live-launch procedure) + T-1 auto-checks the `[[panes]] placement="split"` declaration |
| H-2 | High — AC-23 verified ≠ criterion | **Resolved** | criteria: AC-23 reworded to a 300 ms **responsiveness / non-blocking** bound; plan: T-12 (in-process bound) + T-19 (non-blocking) now verify it as written |
| M-1 | Medium — herdr CLI surface TBD | **Resolved** | techstack: pinned the manifest (`[[panes]] placement="split"`, `command`=binary) and runtime CLI (`pane split … --no-focus` → parse `result.pane.pane_id` → `pane run`); plan T-1/T-17 updated to match |
| L-1 | Low — AC-16 omitted Presenter | **Resolved** | plan: AC-16 coverage now `T-5, T-14, T-18` |
| L-2 | Low — no single "every action" read-only sweep | **Accepted (open)** | optional; per-component read-only is verified (T-4/T-6/T-9/T-16) + T-22. Not a blocker. |

## Chain coverage (criterion → component → product → task) — full re-walk

| AC | Component(s) | Product(s) | Task(s) | Gap |
| --- | --- | --- | --- | --- |
| AC-1 | Root Resolver | git CLI + std | T-3 | — |
| AC-2 | Root Resolver | git CLI + std | T-3 | — |
| AC-3 | Tree Model, Presenter | ignore, ratatui | T-6, T-14 | — |
| AC-4 | Tree Model | ignore | T-6 | — |
| AC-5 | Tree Model, Input, Controller | ignore, crossterm | T-7, T-18, T-21 | — |
| AC-6 | Tree Model, Git Service | ignore, git CLI | T-7, T-18, T-21 | — |
| AC-7 | Git Service, Presenter | git CLI, ratatui | T-4, T-14 | — |
| AC-8 | View Policy, Content Renderer | glow, ansi-to-tui | T-2, T-11 | — |
| AC-9 | View Policy, Git Service, Content Renderer | delta, git CLI, ansi-to-tui | T-2, T-5, T-11 | — |
| AC-10 | View Policy, Content Renderer | bat, ansi-to-tui | T-2, T-11 | — |
| AC-11 | Input, Controller, View Policy | crossterm, std | T-2, T-13, T-18 | — |
| AC-12 | Content Renderer | in-house guard | T-9 | — |
| AC-13 | Content Renderer, Presenter | in-house guard, ratatui | T-9, T-14 | — |
| AC-14 | Git Service | git CLI | T-5 | — |
| AC-15 | Git Service | git CLI | T-5 | — |
| AC-16 | Git Service, Controller, Presenter | git CLI, ratatui | T-5, T-14, T-18 | — |
| AC-17 | Host Adapter | herdr-plugin.toml + herdr CLI | T-1, T-20, T-24, T-25 | — |
| AC-18 | Input Dispatcher | crossterm | T-13, T-21 | — |
| AC-19 | Editor Launcher, Host Adapter | std::process, herdr CLI | T-16, T-17, T-22 | — |
| AC-20 | Input, Controller, Host Adapter | crossterm, std | T-13, T-20, T-25 | — |
| AC-21 | Presenter | ratatui | T-15, T-21 | — |
| AC-22 | Tree Model | ignore | T-8 | — |
| AC-23 | Content Renderer, Controller | ansi-to-tui, std::thread | T-12, T-19 | — |
| AC-24 | Content Renderer | in-house fallback | T-11, T-24 | — |
| AC-25 | Content Renderer, Presenter | in-house notice, ratatui | T-11, T-14, T-24 | — |
| AC-26 | Root Resolver, Git Service, Controller | git CLI, std | T-3, T-5, T-18, T-23 | — |
| AC-27 | Content Renderer | ansi-to-tui | T-10 | — |
| AC-N1 | Tree Model, Content Renderer, Editor Launcher | read-only | T-6, T-9, T-16, T-22 | L-2 (accepted) |
| AC-N2 | Git Service | git CLI (read-only) | T-4, T-5 | L-2 (accepted) |
| AC-N3 | Input, Controller | reviewer-checked | T-13, T-18 | — |
| AC-N4 | Host Adapter | herdr-plugin.toml | T-1 | — |
| AC-N5 | Tree Model | ignore, std | T-6 | — |

Forward + reverse coverage clean: all 32 criteria map to component → product → task; no orphan
task/component/product (T-25 → AC-17/AC-20).

## Five checks (run 2)

1. **Coverage both ways:** clean (table above). ✓
2. **Consistency:** AC-23's responsiveness wording aligns with the design's off-thread Content
   Renderer and the techstack's std-thread choice; AC-17's `[[panes]] placement="split"` is
   consistent across techstack + plan T-1; terminology matches `CONTEXT.md`. ✓
3. **Constitution:** no MUST violated (read-only, delegated rendering, git-first-class,
   keyboard-first, good plugin citizen, YAGNI). ✓
4. **Verification integrity:** every test-backed criterion has a kind-of-oracle and a performing
   task (AC-17 now `manual` via T-25 + automated manifest check via T-1; AC-23 via T-12/T-19);
   reviewer-checked AC-N3/AC-N4 carry axis + pass/fail question. Both maps complete. ✓
5. **Hygiene:** the herdr CLI/manifest surface is pinned (M-1 closed); no unresolved TBDs. The
   only build-time confirmations left are normal (resolved `ratatui-core` minor + ratatui↔crossterm
   pairing on first `cargo build`) — not spec placeholders. ✓

## Remaining (non-blocking)

- **L-2 (Low, accepted):** AC-N1/AC-N2 are verified per-component plus T-22, but no single task
  exercises *every* action then asserts the whole filesystem/git state is unchanged. Optional
  hardening (e.g., snapshot fs+git around the T-21 keyboard sweep); does not block build.
- **Build-time confirmations (normal):** on first `cargo build`, confirm the resolved
  `ratatui-core` version and the ratatui↔crossterm backend pairing.

## Verdict

✅ **READY TO BUILD.** No Critical or High findings. Proceed to build (execute `plan.md`
task-by-task, test-first). L-2 may be addressed during build or accepted as-is.
