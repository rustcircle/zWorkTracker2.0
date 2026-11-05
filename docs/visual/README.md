# Visual comparison: Tauri 1.x and Slint 2.0 (milestone 1)

PLAN.md section 7.2: each screen and theme as `<screen>-<theme>-tauri.png` (a byte-for-byte copy of a 1.x e2e
screenshot) beside `<screen>-<theme>-slint.png` (rendered by this repository's UI test harness).

## Regenerate the Slint images

From the repository root:

```bash
ZWT_SCREENSHOTS=docs/visual cargo test -p zwt-app --test ui_screens -- --ignored
```

`crates/zwt-app/tests/ui_screens.rs` (ignored by default, inert without `ZWT_SCREENSHOTS`; a relative folder is taken
from the workspace root) renders with Slint's **software renderer** under the testing backend, at the Tauri e2e
viewport **1360 x 900 logical pixels, scale 1** (`playwright.config.ts:13` of 1.x), in Slint's mock time; one state,
`timeline-narrow-light`, is rendered at **960 x 900**, the window's minimum width (app.slint `min-width`).

Sample state, the same on every run:

- A fresh sample store (`zwt_bridge::sample`: 3 projects, 10 tasks, 14 days of segments) in a temp folder, opened by
  the worker on the core's `ManualClock` at **2026-11-04T10:30:00Z** in **Europe/Lisbon** (UTC+0 that day). Opening
  it starts "Social templates", so one task runs.
- Then **4,799 s** of running time, second by second, with the minute ticks the app sends: the running clock reads
  **1:19:59**, as in 1.x's timeline screenshots.
- Timeline: today (Wed 4 Nov 2026) picked, Share of time at **Projects** (the default). Themes `light` and
  `solarizedDark` (1.x ids). The day picker shows the page of 14 days that ends today (Thu 22 Oct to Wed 4 Nov,
  oldest on the left); the sample has time on exactly those 14 days, so both arrows are off.
- Timeline, narrowest window (`timeline-narrow-light-slint.png`, 960 x 900): the `timeline-light` state at the same
  moment (1:19:59, 13h 43m), the window resized to 960 px before the capture and back to 1360 px after it. The rail and
  header are unchanged; the 14 day chips are **compact** (weekday initial over the day number, today's in the accent
  colour with its 2 px border; every chip switches together when the widest label no longer fits, below about
  1,286 px), and the Share of time card wraps its legend below the donut and grows to hold it.
- Share of time crops: Light theme, the card cropped at each level in turn: **Projects**, **Tasks**, **Subtasks**,
  **Tags** (`share-light-<level>-slint.png`); the Subtasks crop is taller (387 px) because of its "Other" note.
- About: Light theme, the third-party notices closed.
- Timeline, time before the page (`timeline-older-light-slint.png`): the same store after one more hand-added hour on
  **15 Oct 2026, 09:00-10:00 UTC** on "Weekly planning" (a stopped task below the rail's fold), so a day before the
  page has time: the **left arrow is on**, the right one stays off (the page ends today). Nothing else on screen
  changes; the Share of time card is recomputed, so it reads 13h 44m where `timeline-light` (captured a minute tick
  earlier in the running time) reads 13h 43m.
- Timeline, empty store (`timeline-empty-light-slint.png`): a store with no project, task or time, opened at the same
  moment (nothing runs): the same page of 14 days, the day card with axis, hour labels, hour lines, sub-grid and now
  line, no block; the rail's empty note; the Share of time card's empty note (owner rule, PARITY.md row 419).
- The day axis draws the sub-grid of PARITY.md row 418: no rounding rule is saved in the sample, so 15 minutes (3
  fainter lines between hour lines).
- Fonts: the 1.x families (Segoe UI Variable Text / Display) are not installed on the Mac that rendered these, so
  Slint used the system fallback; PLAN.md 7.2 judges fonts on Windows.

## Pairs

| Screen | Tauri (copied from W = 1.x worktree `task-deletion-timeline-ad2298`) | Slint |
|---|---|---|
| Timeline, Light | `timeline-light-tauri.png` from `W/test-results/screenshots/timeline-light.png` (sha256 c707fb1c...) | `timeline-light-slint.png` |
| Timeline, Solarized Dark | `timeline-solarized-dark-tauri.png` from `W/test-results/screenshots/timeline-solarized-dark.png` (49c470d1...) | `timeline-solarized-dark-slint.png` |
| Share of time card, Projects, Light | `share-light-projects-tauri.png` from `W/test-results/screenshots/share-light-projects.png` (728ffd47...) | `share-light-projects-slint.png` (crop) |
| Share of time card, Tags, Light | `share-light-tags-tauri.png` from `W/test-results/screenshots/share-light-tags.png` (79a96438...) | `share-light-tags-slint.png` (crop) |
| Share of time card, Tasks, Light | none: the 1.x e2e and docs images hold no Share card at Tasks | `share-light-tasks-slint.png` (crop) |
| Share of time card, Subtasks, Light | none: no 1.x image at Subtasks | `share-light-subtasks-slint.png` (crop) |
| Timeline, time before the page, Light | none: 1.x has no day picker or arrows (owner rule, PARITY.md row 417) | `timeline-older-light-slint.png` |
| Timeline, narrowest window (960 x 900), Light | none: 1.x has no day picker; its e2e runs at 1360 px only | `timeline-narrow-light-slint.png` |
| Timeline, empty store, Light | none: 1.x shows a "Nothing is logged yet" note and no days instead (owner rule, PARITY.md row 419) | `timeline-empty-light-slint.png` |
| About, Light | none: 1.x has no About screenshot (below) | `about-light-slint.png` |

Ten Slint states in all: `timeline-light`, `timeline-solarized-dark`, `timeline-narrow-light`, `timeline-older-light`,
`timeline-empty-light`, `share-light-projects`, `share-light-tasks`, `share-light-subtasks`, `share-light-tags` and
`about-light` (each `<state>-slint.png`). Review and verdicts: REVIEW.md (V2b, 2026-11-05: all 10 pass, no medium or
high defect open). Five were re-taken after V2b, when the task list's groups came back (`timeline-light`,
`timeline-solarized-dark`, `timeline-narrow-light`, `timeline-older-light`, `about-light`: the rail shows its
"ACTIVE 5" heading); REVIEW.md notes it at the top, and they await a review round.

Context only (no Slint counterpart in milestone 1; Analysis is milestone 5):

- `analysis-projects-light-tauri.png` from `W/docs/images/analysis-projects-light.png` (823481f9...): the 1.x
  Analysis Projects section, with an older Share of time donut (labels around the ring, no level switch).
- `analysis-tags-light-tauri.png` from `W/docs/images/analysis-tags.png` (80d9f280...): the **Time by tag** bar
  chart, not the Share of time card at Tags; the Tags pair above uses `share-light-tags.png` instead.

Why `test-results/screenshots` and not `docs/images` for the timeline: the e2e run in `test-results` (4 Nov 2026) is
the newest 1.x UI (W/src equals the 1.x main `src` at a88c1aa); `W/docs/images/timeline-light.png` (sha256
2e89b4d1...) and `timeline-solarized-dark.png` (408be592...) are the 2 Nov documentation images of an older UI
(no "More filters", no entry selection).

**No About image:** `W/test-results/screenshots` (218 files) and `W/docs/images` (32 files) hold no About
screenshot; in 1.x, About is a section of Settings (`settings-*.png` show Audit, Backups, Billing prefix, Reminders,
Restore, Tags and Templates only).

## Differences to review (V1)

Where milestone 1 shows other content than the 1.x screen, or Slint draws something differently:

1. **Header:** only the brand. The zone bar (My time / Client time / UTC / Other...), "Showing ...", Show both,
   Hide earnings and Export arrive with their milestones (4, 5).
2. **Task rail:** no "New task" button, search field or "More filters" (milestone 2). Cards have no billed and
   unbilled line, no Details / edit / Description row; the running card's live box shows the session clock only
   (1.x adds today exact and billable time and Billable now: billing, milestone 5). The tasks are in 1.x's
   Active / Drafts / Stopped groups under their headings ("ACTIVE 5"), the Stopped group 8 at a time; inside each
   group the running task on top, then by last run (owner rules, PARITY.md row 416; groups restored 2026-11-05).
3. **Timeline tab:** 1.x shows period totals, the filter, the kinds legend and every day newest first, each with
   totals per project, shift bands and the entries table. Milestone 1 shows a day picker (new in 2.0: a page of 14
   days, oldest on the left, with "Previous days" / "Next days" arrows, PARITY.md row 417; in windows too narrow for
   the chips' names and dates every chip is compact, weekday initial over day number), the picked day's axis
   with a sub-grid at the rounding increment (row 418), and the Share of time card (an Analysis card in 1.x) for the
   week containing today. With no time the same days, axis and grid show, without blocks (row 419).
4. **Day card:** title only; no zone label (e.g. "Lisbon, WET, UTC+0"), no day totals, no per-project line.
   Hour labels assume a 24-hour day (a 23 or 25-hour day keeps its real length, but its labels after the change
   are off by an hour until milestone 4 brings the zone-aware ticks).
5. **Share of time:** the basis is **Exact** (1.x starts on Billable: milestone 5), so the centre says "Exact". No
   info (i) button, no "View data". Tag combinations carry 1.x's diagonal stripes on the ring and in the swatch
   (stripe and sector polygons computed in Rust, as Slint cannot clip a pattern to an arc). On a wide card the legend
   sits beside the donut from a window of about 1,060 px (the legend at least 280 px, as 1.x's `.a-share`), capped at
   388 px (1.x's card in Analysis is narrow, so its legend is below); narrower, the card wraps the legend below the
   donut and grows to hold it, as 1.x does. A legend row writes the name whole and what follows it (" · task ·
   project") as one muted text, cut once at its end, as 1.x does.
6. **Text:** Slint has no line-height for wrapped text; a `Para` widget sets 1.x's 1.45 by hand, so wrapped notes
   and About paragraphs have 1.x's line spacing. No hover tooltips (`title`): a disabled button's reason is its accessible description.
7. **Block text** on the day axis is the task's name (1.x: the first part of its label); blocks have no tooltip.
8. **Notices:** close after 8 s, as in 1.x, on a one-shot timer of their own, or when dismissed.
9. **AboutSlint** is Slint's own widget (logo, "Version 1.18.1", https://slint.dev); clicking it asks the system to
   open that address.
