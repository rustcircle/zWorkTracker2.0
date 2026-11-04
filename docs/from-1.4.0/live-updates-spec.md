# Live updates spec (owner-approved 2026-11-04)

Owner's words: "everything should be dynamic"; "all graphics should be auto updated with the intervals measured, not wait for the user to leave and open the tab again"; "a window updates when it's its open and on every 30s afterwards"; "the timer is of the essence, to the second"; "the timer keeps going even if the app is on the tray".

Rules:
1. Running timer display ticks every 1 s (unchanged).
2. Every other view (Analysis: all charts incl. Share of time and its centre, metric cards, insights, comparison, View data tables; Timeline: period total, day axes, headers, per-project totals, earnings; Billing: unbilled time/amounts; budget/target warnings 80%/100%; task cards and subtask times) recomputes when it opens (tab switch, window shown from tray/minimized) and every 30 s while it stays open and visible, whether or not a timer runs.
3. While the window is hidden (tray) or minimized: no view recomputation; on show, everything catches up at once. Timing itself never stops (only Quit pauses). The tray menu's elapsed time stays current.
4. User actions (start/pause/resume/stop, corrections, added time, tag/subtask changes) and incoming sync changes show immediately everywhere.
5. A refresh never resets chosen chart level, filters, selection, scroll, open View data tables, or form input. Created billing summaries stay frozen.
6. Tests: unit tests per view hook; Playwright with a frozen/fake clock proving (a) views advance at 30 s without reopening a tab, (b) the timer advances per second, (c) hide window -> wait -> show: timer and views show full elapsed time, (d) no recompute while hidden.

## Change (owner, 2026-11-04): reactive frontend
Owner: "I want to make the frontend reactive, meaning a field is updated it reflects immediatly on the ui and when an event is triggered it records and displays new values on the ui".
7. Every committed field change propagates through one reactive store to every view that shows it, immediately, without remount.
8. Every event is recorded first, then its new values show immediately everywhere.
9. No view holds a mount-time snapshot; audit and fix.
10. Time passing for a running task still follows rules 1-3 (pending owner confirmation).

## Change 2 (owner, 2026-11-04): no polling
Owner: "charts should be reactive as well, so no need for pooling, they update as data flows".
Replaces rules 2 and 10: no 30 s interval and no per-view polling. All views derive reactively from the store plus one shared 1 s running-time signal (active only while a task runs). Views re-render only when their rendered output changes (minute-level figures once a minute). Hidden/minimised: derivation may pause, catch up on show; timing never stops.

## Change 3 (owner, 2026-11-04): granular subscriptions
Owner: "every piece that needs a portion of data should register for events with that data and subscribe".
Each component subscribes only to its data slice/topic (entity id, range, tags, settings, running clock). Rust emits typed events naming changed entities after every write and sync merge; the store notifies only matching subscribers. Unsubscribe on unmount; render-count tests prove isolation.
