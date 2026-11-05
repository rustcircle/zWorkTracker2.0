# Milestone 1: status when paused (2026-11-05)

The owner paused 2.0 to finish the Tauri 1.4 version first (1.x repo, branch `claude/task-deletion-timeline-ad2298`).
This commit saves the M1 work so a future version can continue from it. It is **not merged into `main`** and M1 is
**not signed off**.

## Done and gated
- Full gate on the tree before the last two items below: fmt, clippy (host and `x86_64-pc-windows-gnu`, `-D warnings`),
  `cargo test --workspace` 1,259 passed / 1 ignored (with `ZWT_1X_DIR` set), both real-window tests passed, Windows
  release exe PE32+ GUI, 24.1 MB.
- Review findings R1-R4, N1, N3 fixed, each with a test that was red before and green after. N2 (tie order) is still open in PLAN §12.
- Owner visual decisions 1-8 applied (code or docs). Visual review V2b and V2c: PASS, no defect of medium or higher.
- Pause-frame delay: macOS pauses CADisplayLink while the display sleeps (a measurement artefact); the frame hook now has a 1 s deadline.
- CPU: 1 frame per tick instead of 2 (-45 %). Matching Tauri still needs partial rendering.
- Owner additions: Timeline days past to future with paging arrows (14 days), hour sub-grid at the rounding increment
  (15 min when rounding is off), the empty Timeline keeps its days, renderer femtovg-wgpu (DX12 with FXC, then Vulkan, then software).
- Task list: the 1.x Active / Drafts / Stopped groups restored (Stopped shows 8, "Show all / Show fewer"), with the owner's order rule inside each group:
  running first, then by last run, newest first; never-run tasks last.
- Duplicate audit done; SIMP removed 425 lines (screenshots byte-identical).
- Docs: PLAN/PARITY rows 403-420 (M3 window without a native title bar, M6 sync providers, V-SP2), MEASUREMENTS, DEPENDENCIES, notices.

## Not done (resume here)
1. **Memory.** The footprint is about 1.0-1.4 GB on femtovg-wgpu and 0.66 GB on OpenGL, against PLAN §10.2's target of 60 MB or less.
   The root cause is Apple's Metal driver plus femtovg's pipeline, not a leak: see `docs/notes/renderer-memory-macos.md`.
   Vello smoke runs used slightly more (about 1.14 GB). Those runs logged `renderer=software`, so the renderer label in
   `crates/zwt-app/src/renderer.rs` must learn Vello before any Vello number counts. The software renderer measured 59 MB.
   Decide the renderer with real Windows numbers.
2. **Windows numbers** (owner's UTM VM; note that Windows ARM runs this x64 exe under emulation, and the GPU may be WARP): every "pending owner" cell in `docs/MEASUREMENTS.md`.
3. **M1b re-measure** on the chosen renderer, then a quality-control pass, `docs/MILESTONES.md`, merge `--no-ff` into `main`.
4. **Owner choices still open:** the minute clock wakes the app while it is minimised (row 420); both arrows are off at launch with the 14-day sample;
   the About title repeats the app name; one dashed border is solid (a Slint limit); next/previous paging vs 1.x's "Show all"; the optional `rv2-f2` patch.
5. **Roadmap:** `docs/ROADMAP-TASKS.md` with `docs/roadmap/M2.md` and `M3.md` is an **incomplete draft**. The planner was stopped,
   so M4-M7 are missing. Finish it with tasks, subtasks and YOU/CLAUDE pairing for an owner who is learning Rust.

Detailed logs from this run lived in `.stack/out/` (gitignored, local only).
