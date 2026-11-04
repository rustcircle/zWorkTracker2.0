# Plan: live-update rules (v1)
Plan id: live-updates | Author: orchestrator | Created: 2026-11-04 06:38 | requires_depth: 1
Spec: .stack/out/delegator/live-updates-spec.md (owner-approved). Base: fd51d79.

## Survey (what exists)
- src/app/store.tsx: one clock object (createClock) with awake/asleep from visibilitychange, window focus,
  backend `window-hidden`/`window-shown`; LiveClock = 1 s tick for useLiveSec (stops while asleep, reads afresh on wake);
  coarse clock NowCtx = useApp().nowSec every 60 s (stops while asleep, catches up on wake); adopt() resets coarse on new snapshot.
- src/app/stepClock.ts useStepNow: per-view setTimeout at the next billing step / minute mark / midnight; null when nothing runs
  (no timer at all). Used by AnalysisTab, TimelineTab, LiveTicker, TaskCardExtras.
- Tabs mount on switch (Shell.tsx render()), so a tab open reads the clock afresh.
- Tray (Rust thread, tray.rs next_wake_secs): h:mm, wakes on each minute of the running stretch and on data change; independent of window.
  Already "stays current" at its display granularity -> no tray change.
- Gap 1: views wake only at steps/minute marks, never while nothing runs; coarse clock is 60 s, spec says 30 s.
- Gap 2: minimize is reported only if the webview flips visibilityState; backend emits window-hidden only on hide-to-tray.

## Approach
One shared 30 s view tick (the existing coarse clock at 30 s) that useStepNow also follows, so every view re-reads
the time at least every 30 s while awake, plus its precise step wakes; 1 s LiveClock unchanged. Backend emits
window-hidden/window-shown on minimize/restore too.

## Nodes
| id | title | owner | deps | writes | verify |
|---|---|---|---|---|---|
| I1 | 30 s shared view tick + unit tests + README/howto | typescript-expert | - | src/app/**, src/features/** (only if a view needs a fix), README.md, site/howto.html | tsc, lint, vitest |
| I2 | emit window-hidden/shown on minimize/restore | rust-expert | - | src-tauri/src/lib.rs | fmt, clippy host+win-gnu, cargo test |
| T1 | Playwright fake-clock spec (a)-(e) | tester | I1 | e2e/live-updates.spec.ts | playwright |
| V1 | all gates | validator | I1,I2,T1 | nodes/V1 | commands |
| RV | review of the diff | reviewer | I1,I2,T1 | nodes/RV | review |

Waves: W0 I1, I2 | W1 T1 | W2 V1, RV. Critical path I1 -> T1 -> V1.

## Re-plan triggers
- node fails its gate twice; spec conflict found (memory note says "nothing refreshes with no timer running", spec says "whether or not a timer runs": spec wins, flagged).
