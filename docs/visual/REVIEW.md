# Visual review: Slint 2.0 against Tauri 1.x (milestone 1)

Three rounds, newest first. **V2b (2026-11-05)** re-checks the 10 Slint states of the final tree after fix loops FXV1
and FXV2 (V2-SS1, V2-DP1, the new 960 x 900 state); it supersedes the V2 verdicts and open-defect table. **V2
(2026-11-05)** re-measured the 9 states after the fix round (FXB1, FXB2, FXB3a, FXB3b) and the owner rules of PARITY.md
rows 416-419; the **V1 record (2026-11-04)** keeps the first review's tables, each DEFECT row marked with its V2 status.
Classes and severities as in V1 (DEFECT, SLINT-LIMIT, SCOPE, DATA; high / medium / low). Coordinates are pixels
`x0,y0,x1,y1` in the named PNG. Probes and crops: `.stack/out/orchestrator/20261104-2050-m1-resume/nodes/V2/`
(`probe.py`, `p`, `crops/`) and `nodes/V2b/` (`docs-visual.sha`, `scores.log`, `crops/`).

## V2c: task list groups re-checked (2026-11-05)

Verdict: **PASS**. There is no medium or high defect. The five re-taken images differ from their V2b copies only in
the rail box 14-411,113-900. The main column and header are pixel-identical to V2b, so the axis, arrows, hour
sub-grid, card shadows and legend cap are unchanged. The rail is pixel-identical in the four light images. Rubric
(default profile, threshold 8): 8.85, lowest criterion 8 (artefacts, V2-SH1). Crops and data are in
`.stack/out/visual-reviewer/20261105-0701-v2c/`.

| State | V2c verdict | measurement (against 1.x `timeline-light-tauri.png` / `timeline-solarized-dark-tauri.png`) | open |
|---|---|---|---|
| `timeline-light` | **PASS** | "ACTIVE 5": ink rows 118-127 in #4a5462 on #f3f4f6 (6.98:1). 1.x: #4b5563, 6.87:1. Glyphs at x 18-76 (1.x "ACTIVE 3": x 18-77). Ink top to the card border: 20 px, as in 1.x (138 vs 228). Cards 8 px apart (borders 290/299), as in 1.x | V2-SH1 (low) |
| `timeline-solarized-dark` | **PASS** | heading #839496 on #002b36 (4.75:1), the same as 1.x. Spacing as in light | V2-SH1 (low) |
| `timeline-narrow-light` | **PASS** | rail identical to `timeline-light` | V2-SH1 (low) |
| `timeline-older-light` | **PASS** | rail identical to `timeline-light` | none |
| `about-light` | **PASS** | rail identical to `timeline-light` | V2-A1 (owner) |

The Drafts and Stopped groups and the link lie below the 900 px fold, so they were checked in the 3,200 px rail
crops (`.stack/out/rust-expert/20261105-0602-tasklist-groups/crops/rail-12-stopped-*.png`):
- Gap between groups: 14 px, then the 17 px heading box and 6 px to the card (card 953, DRAFTS ink 971-980, card 991).
- Stopped group: "STOPPED 8" with "Show all 12 stopped tasks" (x 16, 2 px underline offset, weight 600, accent).
  Expanded: "STOPPED 12" with "Show fewer". The count is the number of cards shown, as in 1.x (`tasks.length`).
- **V2c-L1 (low, docs)**: no file in docs/visual shows the Drafts or Stopped group or the link.

## Task list groups restored (2026-11-05, after V2b)

The owner asked for the task list to stay as in 1.x ("activities/tasks is just the same, the running on top"): the rail
lists 1.x's **Active / Drafts / Stopped** groups again (TaskRail.tsx L87-145), each under its heading, the title and how
many cards it lists in capitals ("ACTIVE 5", shell.css `.rail-group-title`); a group with no task is not drawn; the
Stopped group shows 8 cards with "Show all N stopped tasks", then all of them with "Show fewer" (`STOPPED_PAGE`, L15).
Inside each group the order rule of PARITY.md row 416 holds (the running task first, then by last run, the tasks never
run last). **V2-N6 ("one task list") is superseded.**

| Image | Change |
|---|---|
| `timeline-light`, `timeline-solarized-dark`, `timeline-narrow-light`, `timeline-older-light`, `about-light` (the rail is beside both tabs) | re-taken: heading "ACTIVE 5" in box 18-405,115-132, ink rows 118-126 in #4a5462 (text-muted #4b5563; 1.x "ACTIVE 3" in `timeline-light-tauri.png`: rows 209-217, #4b5563); the first card's top border 12 px under the ink's foot (row 138; 1.x: 11 px, row 228: the heading's box is 23 px to the card here, 23.4 px in 1.x, and the fonts differ, PLAN.md 7.2); cards Social templates (running), Logo concepts, Label artwork, Blog redesign, Brand guidelines (cut by the fold); the Drafts and Stopped groups lie below the 900 px fold |
| `timeline-empty-light`, the four `share-light-*` crops | byte-identical (sha256 before and after: `.stack/out/rust-expert/20261105-0602-tasklist-groups/visual-before.sha`, `visual-after.sha`) |

Crops: `.stack/out/rust-expert/20261105-0602-tasklist-groups/crops/` (`slint-heading-3x.png` beside `tauri-heading-3x.png`;
`rail-12-stopped-page.png` and `rail-12-stopped-all.png`, a 3,200 px tall rail with 12 stopped tasks: 8 and the link,
then all 12 and "Show fewer"). These five images have not had a visual-review round since V2b.

## V2b summary (2026-11-05)

Verdict: **PASS** - all 10 states pass; **no medium or high defect is open**. V2-SS1 and V2-DP1 are FIXED (measured
below). Open: 2 low items that need the owner's approval to close as deviations (V2-SH1, V2-E1), 1 low SCOPE item kept
until M2 / M5 (V2-E2), and 2 owner questions (V2-A1 About card title; the Share legend width between 1,060 and
1,167 px). Inputs: the 10 `*-slint.png` in this folder; 8 are byte-identical to V2 (sha256 in
`nodes/V2b/docs-visual.sha` vs `nodes/V2/docs-visual.sha`), `share-light-subtasks` changed, `timeline-narrow-light` is
new. Rubric (default profile, threshold 8, computed in `nodes/V2b/scores.log`): narrow 8.50, Subtasks 8.75, lowest
criterion 8 in both.

| State (`<state>-slint.png`) | 1.x image | V2b verdict | open |
|---|---|---|---|
| `timeline-light` | `timeline-light-tauri.png` | **PASS** (unchanged) | V2-SH1 (low) |
| `timeline-solarized-dark` | `timeline-solarized-dark-tauri.png` | **PASS** (unchanged) | V2-SH1 (low) |
| `timeline-narrow-light` (new, 960 x 900) | none (1.x has no day picker) | **PASS** | V2-SH1 (low) |
| `timeline-older-light` | none | **PASS** (unchanged) | none |
| `timeline-empty-light` | none (owner rule row 419) | **PASS** (unchanged) | V2-E1, V2-E2 (low) |
| `share-light-projects` | `share-light-projects-tauri.png` | **PASS** (unchanged) | none |
| `share-light-tasks` | none | **PASS** (unchanged) | none |
| `share-light-subtasks` | none | **PASS** (was REVISE) | none |
| `share-light-tags` | `share-light-tags-tauri.png` | **PASS** (unchanged) | none |
| `about-light` | none | **PASS** (unchanged) | V2-A1 (owner) |

## V2b: the two medium defects re-measured

| id | V2b measurement | status |
|---|---|---|
| **V2-SS1** | `share-light-subtasks-slint.png` (892 x 387) legend 325-539,85-321, glyph-column runs per row: the first "·" of each row follows the whole name with a 4-6 px gap (one space: rows at y 84, 112, 140, 168, 196, 224, 252, 280 have it at x 511, 483, 468, 402, 500, 501, 484, 500); no "…" before a "·"; 6 of 9 rows end in exactly one "…" at x 526-538 ("Supplier call notes (no subtask) · P…", "Colour study · Logo concepts · Bra…"), 2 rows whole ("Box dieline (no subtask) · Packaging", "Blog redesign (no subtask) · Website"), "Other (3 subtasks)" has no context. Name ink #111827 (17.74:1), context #4b5563 (7.56:1). Note "Other adds up ... every subtask." wraps to 2 lines (341-352, 358-367) inside the card (border y 386). `nodes/V2b/crops/subtasks-legend-v2b-3x.png` | **FIXED** (the legend stays capped at 371 px with 214 px of card free right of the figures, owner decision 8; see owner question 2) |
| **V2-DP1** | `timeline-narrow-light-slint.png` (960 x 900) day picker 492-887,132-179: 14 compact chips, borders 23 px apart (492-514, 521-542 ... 865-887), gap 7 px; line 1 weekday initial (bold, cap height 9 px, rows 139-147), line 2 day number (8 px, rows 154-161), widest "22" 12 px (497-508) with 4-5 px to the borders; every chip shows its initial and number ("T 22" ... "T 3", "W 4"), **no "…"** (prefix image of FXV1 had "…" on all 14). Today: "W" and "4" in accent #0f766e on #e7f1f0 (4.75:1) with the 2 px accent border; others #111827 (17.74:1) and #4f5967 (7.10:1) on white. Arrows off (#a5aab1) as at 1,360 px. At 1,360 px unchanged (`timeline-light` byte-identical). 1,280 px has no capture: compact holds there by the rule (every chip compact while chip width < widest label + 2 x padding, timeline.slint:137, switch near 1,286 px) and `ui_day_chips.rs` (3 tests). `nodes/V2b/crops/narrow-picker-4x.png`, `narrow-picker-today-8x.png` | **FIXED** |

## V2b: `timeline-narrow-light-slint.png` (960 x 900) reviewed

| check | measurement | result |
|---|---|---|
| layout | rail 0-419 and header 0-959 x 0-54 pixel-identical to `timeline-light` (0 px differ); main column cards 445-935, 25 px margins like the wide state | ok |
| day picker | as V2-DP1 above; picker card 120-195, chips inside it, arrows 460-482 and 900-920 (rows 151-160) clear of the chips | ok |
| day axis | axis 485-895, hour labels 00:00 ... 21:00 under their lines, none overlapping; blocks and the hatched running block end at the now line (x 687 = about 11:50, the same moment as `timeline-light` x 884); sub-grid 4.3 px pitch (410 px / 96), dense but as the rule (row 418). `nodes/V2b/crops/narrow-axis-3x.png` | ok |
| Share card | card 445-935 x 334-771 (border #d9dce3 at y 771); the legend wraps below the donut (row ink 682-694, 710-722, 738-750; swatch 468, names 489, figures end 911) and the card grows to hold it: 20 px from the last row to the border (wide state 20 px, donut 655 to border 676). FXV1's prefix image drew the legend at 676-750 below a card ending at y 676 (the FXV2 overflow fix) | ok (FIXED in FXV2) |
| outside any box | below the Share card (421-959, 782-899) and the main column's right gutter (940-959) and left gutter (421-441): 0 pixels off the background #f3f4f6; no scrollbar thumb | ok |
| duplicates (owner request) | same elements as `timeline-light`, nothing new: one picker, one day card, one Share card, one legend (3 rows, each once), 14 chips each day once; "W 4" chip and "Today, Wed 4 Nov 2026" title both kept as in V2 (selection vs card title) | nothing double |

## V2b: low items left by the fix loops

| id | reason given | V2b classification |
|---|---|---|
| V2-SH1 (1 px corner spot, #c9cbcf vs border #d9dce3, invisible at 100 %) | the fix needs per-side shadow bands instead of rings | **low, DEFECT deferred**: keep open; close as an accepted deviation only with the owner's approval. Not visible at target size, so not a gate |
| V2-E1 (empty Share note: solid border, 1.x dashed) | Slint 1.18.1 has no dashed stroke | **low, SLINT-LIMIT** (confirmed: no dash property in the `i-slint-compiler-1.18.1` source). A drawn workaround (a Repeater of 4 x 1 px dash Rectangles per side) stays possible; owner approval to accept the solid border |

## V2b open defects and owner questions

No medium or high row. Every row below is low.

| id | where | sev / class | status | fix or decision |
|---|---|---|---|---|
| V2-SH1 | every card's bottom corners, e.g. day card 1328-1336,312-320 in `timeline-light-slint.png` | low / DEFECT | open, owner approval | per-side shadow bands, or accept |
| V2-E1 | `timeline-empty-light-slint.png` 460-1320,411-487 | low / SLINT-LIMIT | open, owner approval | accept solid border, or the dash Repeater |
| V2-E2 | rail, Share empty and Subtasks notes name M2 / M5 controls | low / SCOPE | keep (1.x-exact) | check again in M2 / M5 |
| V2-A1 | `about-light-slint.png` 462-580,137-155 | low / owner | owner question 1 | suggested: drop the card title |

Owner questions:

1. **About card title** (V2-A1): "zWorkTracker" repeats the header brand 120 px above it; 1.x's About starts with the
   "App version" row. Suggested: remove the card title.
2. **Share legend width between 1,060 and 1,167 px** (an FXV2 change no capture covers): the legend sits beside the
   donut once the card's room minus 32 px holds 252 + 28 + 280 px (share.slint:336, window about 1,060 px) and is
   `min(388 px, what is left)` wide (share.slint:443), so it is 280-387 px wide in that range against 371 px of
   rows at 1,360 px: Projects names fit, Subtasks context is cut sooner. This follows 1.x's `.a-share` (legend at least
   280 px). Accept, or switch to side by side only from about 1,168 px; a 1,080 x 900 Subtasks capture would show it.
   Related (decision 8): above 1,168 px the legend stays at 388 px, so at Subtasks the context is cut ("· P…") while
   214 px of the card stay free.
3. **V2-E1**: accept the solid border of the empty Share note as a Slint 1.18.1 limit?
4. **V2-SH1**: accept the 1 px darker shadow corner (not visible at 100 %) until per-side shadow bands are worth it?

## V2 summary (2026-11-05)

(Superseded by V2b: V2-SS1 and V2-DP1 are FIXED.) Verdict: **REVISE** - every V1 DEFECT is fixed, but 2 medium defects are open: **V2-SS1** (Share of time at
Subtasks cuts each row's own name, unlike 1.x) and **V2-DP1** (day chips elide their labels below about 1,320 px window
width; the window allows 960 px). Both are local UI fixes (section "V2 open defects"). No high defect. 3 low defects
(V2-SH1, V2-E1, V2-E2) and 1 low owner question (V2-A1) are open as well.

| Pair or state | 1.x image | Verdict | V1 defects | open in V2 |
|---|---|---|---|---|
| Timeline, Light | `timeline-light-tauri.png` | **PASS** (image) | V-T1 to V-T5, V-T9, V-T10, V-T13: FIXED | V2-SH1 (low); the view's V2-DP1 (medium) shows only at narrower windows |
| Timeline, Solarized Dark | `timeline-solarized-dark-tauri.png` | **PASS** (image) | V-D1 to V-D5, V-D7 to V-D9: FIXED | as Light |
| Share of time, Projects, Light | `share-light-projects-tauri.png` | **PASS** | V-SP1, V-SP5: FIXED | none |
| Share of time, Tags, Light | `share-light-tags-tauri.png` | **PASS** | V-ST1, V-ST2, V-ST3: FIXED | none |
| Share of time, Tasks, Light (new) | none (1.x e2e has no Tasks capture) | **PASS** | - | none |
| Share of time, Subtasks, Light (new) | none | **REVISE** | - | **V2-SS1 (medium)** |
| Timeline, empty store, Light (new) | none (1.x shows "Nothing is logged yet" instead; owner rule row 419) | **PASS** | - | V2-E1, V2-E2 (low) |
| Timeline, time before the page, Light (new) | none (no day picker in 1.x) | **PASS** | - | none |
| About, Light | none (About is a Settings section in 1.x) | **PASS** | V-A5: FIXED | V2-A1 (low, owner) |
| Day picker below about 1,320 px window width (no image) | none | **REVISE** | - | **V2-DP1 (medium)** |

## V2: the V1 defects re-measured

Same probes as V1 (column and row runs against the card or background colour, Pillow). Slint and Tauri are measured at
the same element in both files; dark-theme values equal the light ones unless given.

| V1 id | V2 measurement (Slint vs Tauri) | status |
|---|---|---|
| V-T1, V-D1 icons | pause bars 4 + 5 px wide, 10 px tall (Slint 142-151,250-259) vs 4 + 5 x 10 (Tauri 141-150,508-517); stop 9 x 10 (330-338,250-259) vs 10 x 10; play on Resume 9 x 12 in both (228-236,410-421 vs 227-235,734-745); dark pause 4 + 5 vs 4 + 4 | **FIXED** |
| V-T2, V-D2 chips and badges | project chip at x 42: 23 px (156-178) vs 22 (271-292), at x 40: 21 vs 20; Running badge 21 px (326-334,127-147) vs 22 (241-262); tag chip 20 px (147,158-177) = 1.x `12px x 1.5 + 2px`; V1 was 13 / 16 / 15 px | **FIXED** (1 px rounding, Mac fallback font; judge on Windows) |
| V-T3, V-D3 pause note | 33 px (36,361-393) vs 32 (36,659-690); V1 23 px | **FIXED** |
| V-T4, V-D4 button height | 28 px: borders at y 241 and 268 vs 499 and 526 | **FIXED** |
| V-T5, V-D5 tab labels | "Timeline" glyph rows 77-86 vs 77-87; underline 101-103 vs 102-104, both 65 px wide (452-516); tab bar border y 103 vs 104 (V1: label 3 px high, underline 2 px lower) | **FIXED** (1 px) |
| V-T9, V-D7 card shadows | below the running card (x 200): border #d9dce3 at y 280, then #dadbdf #e3e4e8 #eaebee #edeef0 #eeeff1 (281-285) vs 1.x #dadbde #e7e8ea #edeef0 #eeeff1 #eff0f2 (578-582); day card y 317-323 down to #f1f2f4 like 1.x's totals card (231-238); dark #173d45 #0d353e #062f39 #032d37 vs #183e47 #0c353e #06303a #052f39. Proof: `crops/shadow-proof-6x.png` (Slint and Tauri side by side, light and dark). V1: no shadow at all | **FIXED** (V2-SH1: 1 px corner spot, low) |
| V-T10, V-D8 rail scrollbar | rail gutter 410-417,60-899: 0 pixels off the background in Light, Dark and About; main column gutter 1338-1359: 0. V1: permanent thumb #858587 at 398-402,128-505. Showing on scroll or hover cannot be seen in a still; FXB1 tests `ui_scrollbar` cover it | **FIXED** (at rest) |
| V-T13, V-A5, V-D9 line height | About paragraph lines start at y 205 and 225 (pitch 20 px = 14 px x 1.45); empty rail note 119 / 139 (20 px); Subtasks note (12 px) 341 / 358 (17 px = 12 x 1.45). V1: 14 px | **FIXED** |
| V-SP1, V-ST3 legend row pitch | swatch tops 28 px apart: Projects 170, 198, 226; Tags 115 ... 255; Tasks 86 ... 310; 1.x Tags 358, 386, 413, 441 ... (28 / 27). V1: 22 px | **FIXED** |
| V-SP5, V-ST6 legend width | legend from swatch (x 304) to the last ")" (674) = 371 px in every crop; on the Timeline 748-1118 = 371 px; "Packaging" ends at x 829, its figures start at 1015 (V1: about 440 px between name and figures) | **FIXED**; side effect at Subtasks: V2-SS1 |
| V-ST1 combination slices | diagonal stripes on the ring; along y 200 through "Concept + Revision" (Slint 26-62): purple / other runs 6-6-5-6-5 px, period 11-12 px; 1.x along y 230: 5-6-6-6-5, period 11-12 px. `crops/tags-ring-stripes-4x.png` vs `tags-ring-stripes-tauri-4x.png`. V1: concentric 4 px rings | **FIXED** |
| V-ST2 combination swatch | 12 px swatch with 2 full and 2 partial diagonal bands of both colours (`crops/tags-swatch-8x.png`); V1: one band | **FIXED** |

## V2: owner decisions (2026-11-04)

| # | item | V2 status |
|---|---|---|
| 1 | V-ST1 combination slices as stripe and sector polygons | **fixed**: diagonal stripes, period 11-12 px as 1.x (row V-ST1 above) |
| 2 | V-T8 M1 menu Timeline + About | accepted (unchanged) |
| 3 | V-SP2 period label next to "Share of time" | accepted (unchanged; PARITY.md row 410) |
| 4 | V-T13 line spacing 1.45 | **fixed**: 20 px pitch at 14 px, 17 px at 12 px |
| 5 | V-T10 overlay scrollbar hidden at rest | **fixed** at rest: no thumb pixel in any capture; shown-while-scrolling is covered by FXB1 tests, not by a still |
| 6 | V-T9 card shadows | **fixed**: shadow profile within 4 levels of 1.x per pixel, proof crop `crops/shadow-proof-6x.png` |
| 7 | V-A4 AboutSlint | accepted (unchanged) |
| 8 | V-SP5 legend width cap | **fixed**: 371 px legend beside the donut; at Subtasks the cap leaves 210 px of the card empty while names are cut (V2-SS1) |

## V2: new states and owner rules

| id | rule or state | where | observation and measurement | status |
|---|---|---|---|---|
| V2-N1 | days run past to future (row 417) | `timeline-light-slint.png` 492-1288,132-180 | Thu 22 Oct ... Tue 3 Nov, "Today 4 Nov" rightmost; 14 chips, no day twice (Sun 25 Oct, the 25-hour day, has one chip); blocks on the axis by time, earliest left | as the rule |
| V2-N2 | arrows (row 417) | 462-480,145-166 and 1300-1318,145-166 | `timeline-light`: both off (ink #a5aab1 = text-muted at 50 %, 2.34:1, disabled so exempt from 1.4.11); `timeline-older-light`: left on #4b5563 (7.56:1), right off; `timeline-empty-light`: both off. Crops `day-picker-older-left-on-2x.png`, `day-picker-light-both-off-2x.png` | as the rule |
| V2-N3 | day chips at 1,360 px | `timeline-light` 836-886,152-165 | chips 51 px wide, gap 6 px; widest date label "28 Oct" 845-877 with 8 px either side; no chip label cut at this width | ok here; see V2-DP1 for narrower windows |
| V2-N4 | sub-grid (row 418) | axis 484-1295,251-285 | 3 lines between hour lines, 8-9 px apart (493, 502, 510 / 527, 536, 544 ...); sub line #f3f4f5 = grid at 25 % (1.10:1 on white; dark #05343f 1.12:1), hour lines #e5e7eb (1.24:1), every third #d1d5db (1.47:1) as 1.x #e6e8eb / #d1d5db; no sub line on an hour line or on the frame (last at 1287, frame 1295). `crops/day-axis-subgrid-4x.png` | as the rule; owner may raise the 25 % if the sub-grid disappears on a low-contrast screen (observation, not a defect) |
| V2-N5 | empty Timeline (row 419) | `timeline-empty-light-slint.png` | same 14 chips (bars empty), same day card, axis, labels, hour lines, sub-grid and now line (838 = 10:30), no block; Share card at Projects with the empty note on surface-alt #e9ebef, text #4b5563 6.33:1; Subtasks segment disabled. `crops/empty-rail-and-picker-1x.png`, `empty-share-note-2x.png` | as the rule; V2-E1, V2-E2 |
| V2-N6 | one task list (row 416) | rail 14-406,60-900 | no Active / Drafts / Stopped heading; Social templates (running) on top, then Logo concepts (axis block ends 10:30), Homepage layout (stopped 09:35), Box dieline (08:55), Label artwork (07:55): most recent first | as the rule; **superseded 2026-11-05**: the groups are back (top of this file) |
| V2-N7 | Share at Tasks | `share-light-tasks-slint.png` | 9 rows, "Task · Project" with the project muted, none cut; pitch 28 px; figures aligned | ok |
| V2-N8 | Share at Subtasks | `share-light-subtasks-slint.png` 300,78-690,372 | 6 of 9 rows cut, each part separately: "Supplier call notes (no su… · Pack…", "Colour … · Logo con… · Brand re…"; a gap before "·" after each cut part; legend ends at x 674 while the card runs to 890. `crops/subtasks-legend-elision-3x.png` | **V2-SS1** |
| V2-N9 | About | `about-light-slint.png` | wording unchanged and exact; paragraph pitch 20 px; card title "zWorkTracker" repeats the header brand | V2-A1 |

## V2 open defects (for the fix round)

V2b status: V2-SS1 **FIXED**, V2-DP1 **FIXED** (FXV1); V2-SH1, V2-E1, V2-E2, V2-A1 open as low (V2b tables above).

| id | where | defect | sev | fix instruction | verify |
|---|---|---|---|---|---|
| **V2-SS1** | Share of time legend, Subtasks (and any row with context): `share-light-subtasks-slint.png` 325-545,85-292 | the row's own name, the task and the project are separate texts that each elide, so the subtask or task name is cut while its context still shows ("Colour … · Logo con… · Brand re…"); 1.x writes name, " · " and context in one line with one ellipsis at its end, so the context is cut first and the name stays whole (`charts.tsx:662-692`, `analysis.css:134,140`) | medium | `share.slint` legend row: put the context parts in **one** muted Text (" · Logo concepts · Brand refresh") with `overflow: elide; horizontal-stretch: 1; min-width: 0;`; give the name Text `horizontal-stretch: 0` and `max-width:` the row's name column width, so it elides only when it alone is wider than the column. Optional (owner, decision 8): size the side-by-side legend to `min(free card width, max(388px, widest row))` so the 210 px left empty on the Timeline card is used before anything is cut | in the new `share-light-subtasks-slint.png` no "…" sits before the first "·" of any row; at most one "…" per row, at its end; no blank gap before a "·" |
| **V2-DP1** | day picker, every window narrower than about 1,320 px (no capture; window `min-width: 960px`, app.slint:182) | the 14 chips share the strip; the rail is fixed at 420 px (app.slint:219), so the strip shrinks 1:1 with the window. At 1,360 px a chip is 51 px (borders 492 and 542), padding 5 px a side (timeline.slint:31-34); widest labels "Today" 36 px (1244-1279), "28 Oct" 33 px (845-877). Chip width = (window - 646) / 14, so "Today" elides below about 1,318 px, the dates below about 1,276 px, and at 960 px a chip is 22.4 px with both lines "…" (timeline.slint:42,49 `overflow: elide`). FXB3a saw about 23 px chips with cut labels at 960 px | medium | `timeline.slint` day chip: when the chip is narrower than its labels + 16 px, show a compact chip: weekday initial on line 1 ("W"), day number on line 2 ("28"), today's chip the day number in accent with its 2 px border; the full date stays the accessible name. Add a 960 x 900 state `timeline-narrow-light-slint.png` to `ui_screens.rs` (owner alternative: chips keep >= 46 px and the strip scrolls sideways, opening at today) | at 960 px and at 1,280 px no chip shows "…" and every chip shows its day number; at 1,360 px unchanged |
| V2-SH1 | every card's bottom corners, e.g. day card 1328-1336,312-320 (`crops/shadow-corner-slint-12x.png`) | the shadow rings stack on the card border's anti-aliased corner: darkest corner pixel #c9cbcf (1331,316; task card #cecfd4 at 401,280), darker than the border #d9dce3; 1.x corners stay at #dadbde. Not visible at 100 % | low | shadow rings (FXB1): start the innermost ring 1 px outside the card border with radius = card radius + 1 (each next ring + 1), so no ring corner overlaps the border | darkest pixel within 8 px of each card corner no darker than #d9dce3 (Light) / #14505e luminance (Dark) |
| V2-E1 | empty Share note box, `timeline-empty-light-slint.png` 460-1320,411-487 | border solid #6b7280 (y 411, x 470-499 all border-strong); 1.x `.a-empty` is `1px dashed var(--border-strong)` (analysis.css:53). Size, fill (#e9ebef) and text match | low | draw the border as dashes (4 px dash, 3 px gap, 1 px, border-strong) with a Repeater of Rectangles per side, or accept as SLINT-LIMIT (no dashed borders in Slint 1.18) | border pixels along y 411 alternate border-strong and surface-alt |
| V2-E2 | wording that names controls M1 does not have | rail "No tasks yet. Use New task to add one, then press Start to begin timing." (New task: M2); Share empty "Try another range or clear the filters." (no range or filter in M1); Subtasks note "View data lists every subtask." (V-SP4) | low (SCOPE) | all three are 1.x-exact (TaskRail.tsx:134, SectionsP2.tsx:11, shareSlices.ts:80) and become true when those controls ship; keep, no change in M1 | check again in M2 / M5 |
| V2-A1 | About card title, `about-light-slint.png` 462-580,137-155 | "zWorkTracker" repeats the header brand (50-155,17-33) 120 px above it; 1.x's About section starts with the "App version" row (OtherSections.tsx:155-162) | low (owner) | owner decision; suggested: drop the card title (the name stays in the header and in "zWorkTracker is MIT licensed") | - |

Residual sizes of 1 px against the Mac 1.x captures (chip 23 / 22, badge 21 / 22, note 33 / 32, tab bar border 103 / 104)
are rounding of the fallback font and are judged on Windows (FXB1); not counted as defects.

## V2: duplicates (owner request: "anything double in the app that is unnecessary")

Every capture was checked for repeated sections or cards, repeated totals, periods, legends, headers and labels, two
buttons for one action, double borders, dividers or shadows, nested or double scrollbars, and items twice in a list or on
the day axis. Found: no section, card, legend, scrollbar, list item, day or segment shown twice.

| item | where | keep / remove / owner | why | crop |
|---|---|---|---|---|
| app name in the header and as the About card title | `about-light-slint.png` 50-155,17-33 and 462-580,137-155 | **owner** (suggest remove the card title) | 1.x About has no app-name heading; the name also stays in the licence line | `crops/dup-about-app-name-2x.png` |
| the shown day in the selected chip ("Today 4 Nov") and in the day card title ("Today, Wed 4 Nov 2026") | `timeline-light-slint.png` 1238-1287,133-179 and 462-637,218-235 | keep | the chip is the picker's selection, the title names the card as 1.x's day card does | `crops/dup-today-chip-and-title-1x.png` |
| two date ranges: picker page 22 Oct - 4 Nov and Share period "2 Nov - 8 Nov 2026" | `timeline-light-slint.png` 492-1288,132-180 and 559-669,346-358 | keep | different ranges, not a repeat; the period label is approved (V-SP2) | `crops/dup-periods-picker-vs-share-1x.png` |
| a task's state three ways: badge, coloured left edge and the note or live box ("Paused" + "Paused by you."; "Stopped" + "Stopped 2026-11-04 09:35 UTC.") | rail cards, e.g. 16-406,116-280 | keep | 1.x shows the same three (TaskCard.tsx:123; `timeline-light-tauri.png` 18-405,590-690) | `crops/dup-card-state-shown-thrice-2x.png` |
| Start / Pause / Resume / Stop on every card, the ones that do not apply disabled | every card, e.g. 34-392,241-268 | keep | 1.x layout; one button per action, none doubled | - |
| a day's project mix in its chip bar and on the axis | chip bars 497-1283,167-173; axis 485-1295,251-285 | keep | the bars summarise 14 days, the axis details one; 2.0 day picker (V-T21) | `crops/day-picker-older-left-on-2x.png` |
| "(no subtask)" on 8 of 9 Subtasks rows | `share-light-subtasks-slint.png` 325-545,85-292 | keep (1.x `NO_SUBTASK_LABEL`) | 1.x wording; with V2-SS1 fixed it no longer pushes the names out | `crops/subtasks-legend-elision-3x.png` |
| "3 subtasks" in the "Other (3 subtasks)" row and in the note under it | same file 325-430,309-321 and 304-680,341-366 | keep | 1.x-exact (`otherLabel`, `otherNote`, shareSlices.ts:75-80) | `crops/subtasks-legend-elision-3x.png` |
| "Tasks" as the rail heading and as a Share level | 14-64,76-92 and 553-589,377-391 | keep | different meanings, both 1.x | - |
| card border plus shadow rings | every card | keep | no second border: border 1 px, then the shadow gradient; only V2-SH1's 1 px corner spot | `crops/shadow-proof-6x.png`, `shadow-corner-slint-12x.png` |
| empty Share note box inside the Share card (border in a border) | `timeline-empty-light-slint.png` 460-1320,411-487 | keep | 1.x `.a-empty` is a bordered box inside the card too (dashed: V2-E1) | `crops/empty-share-note-2x.png` |
| selected chip outline | 1237-1288,132-180 | keep | one 2 px accent border (x 1237-1238), not two | `crops/day-picker-older-left-on-2x.png` |
| scrollbars | rail and main column | nothing to remove | no thumb at rest, no nested scroll area (V-T10) | - |

## V2: colours of the new elements

| element | expected | Slint Light | Slint Dark |
|---|---|---|---|
| sub-grid line | grid at 25 % | #f3f4f5 | #05343f |
| hour line / every third | grid at 55 % / 100 % | #e5e7eb / #d1d5db | #0a3e4b / #14505e |
| arrow on / off | text-muted / at 50 % | #4b5563 / #a5aab1 | - / #415e65 |
| selected chip | accent border 2 px, accent 10 % over surface | #0f766e, #e7f1f0 ("Today" 4.75:1) | #2aa198 |
| chip border | border-strong | #6b7280 | - |
| empty Share note | surface-alt, border-strong, text-muted | #e9ebef, #6b7280, #4b5563 (6.33:1) | - |
| card shadow (first 5 px below a card) | 1.x `--shadow` | #dadbdf ... #eeeff1 | #173d45 ... #032d37 |
| rail scrollbar at rest | none | none | none |

## V2 inspection artefacts

`.stack/out/orchestrator/20261104-2050-m1-resume/nodes/V2/`: `probe.py` and its wrapper `p` (runs, px, med, col, row,
crop), `docs-visual.sha` (sha256 of the 9 Slint PNGs reviewed), `shots.log`, and `crops/`: `shadow-proof-6x.png`,
`shadow-corner-slint-12x.png`, `shadow-corner-tauri-12x.png`, `tags-ring-stripes-4x.png`, `tags-ring-stripes-tauri-4x.png`,
`tags-swatch-8x.png`, `subtasks-legend-elision-3x.png`, `day-axis-subgrid-4x.png`, `day-picker-older-left-on-2x.png`,
`day-picker-light-both-off-2x.png`, `empty-rail-and-picker-1x.png`, `empty-share-note-2x.png`,
`dup-about-app-name-2x.png`, `dup-today-chip-and-title-1x.png`, `dup-periods-picker-vs-share-1x.png`,
`dup-card-state-shown-thrice-2x.png`.

## V1 record (2026-11-04)

Kept as written on 2026-11-04. The **V2** mark at the end of each DEFECT row (and of the rows the owner sent to
the fix round) gives its status on 2026-11-05; the measurements are in "V2: the V1 defects re-measured".

Reviewer: visual-reviewer, 2026-11-04. Inputs: the PNG pairs in this folder (README.md), `crates/zwt-app/ui/theme.slint`
(tokens), `crates/zwt-app/ui/strings.slint` (wording), 1.x `src/` (read-only) for CSS and icons. Rule (PLAN.md 7.2):
same layout, wording, token-exact colours and icons; differences only where Slint cannot render something.

Classes: **SCOPE** (content M1 does not build), **SLINT-LIMIT** (Slint cannot render it as 1.x does), **DEFECT**
(fixable now, fix given), **DATA** (sample data only). Severity: high / medium / low. All coordinates are pixels in the
named PNG (x0,y0,x1,y1). Measurements were made with Pillow on the PNGs; crops and the sampling script are in
`.stack/out/visual-reviewer/20261104-1030-v1-slint-vs-tauri-r1/`.

### Summary

| Pair | Verdict | DEFECT | SLINT-LIMIT | SCOPE | DATA |
|---|---|---|---|---|---|
| Timeline, Light | REVISE | 5 | 5 | 11 | 3 |
| Timeline, Solarized Dark | REVISE (same 5 defects; colours token-exact) | 5 | 5 | 11 | 3 |
| Share of time, Projects, Light | REVISE (1 low defect) | 1 | 1 | 4 | 2 |
| Share of time, Tags, Light | REVISE (2 low defects) + 1 limit to approve | 2 | 1 | 4 | 2 |
| About, Light (no 1.x image) | PASS (wording exact; contrast AA) | 0 | 2 | 2 | 0 |

No high-severity defect. Medium defects: **V-T1** (filled icons drawn without the 1.x stroke, so smaller) and **V-T2**
(chips and badges 5-6 px shorter than 1.x, so every task card is tighter). The defects in the Timeline pairs are
shared components (one fix fixes both themes).

Colour check: every sampled UI colour equals its theme.slint token in both themes (distance 0, or 2-8 on anti-aliased
glyph pixels), and every derived colour (live box, pause note, disabled button) is the same value in both apps, so the
`mix()` rules reproduce 1.x `color-mix`. Only non-token colours: the rail scrollbar (V-T10) and AboutSlint's own text
and badge (V-A4). Section "Colour samples" lists the values.

The author's README "Differences" list was checked item by item: items 1-5 and 7-9 are confirmed (rows below); item 6
(no line-height) is confirmed for wrapped text but it also explains V-T2/V-T3/V-SP1, which are fixable for one-line
boxes and so are classed DEFECT, not limit. Not in the README: V-T1 (icons), V-T4 (button height), V-T5 (tab label
offset), V-T9 (card shadow not rendered), V-T10 (scrollbar), V-ST2 (swatch stripes).

Owner decisions on this round (section below): V-T8 (the M1 menu Timeline + About), V-SP2 (the period label next to
"Share of time", kept as a 2.0 addition) and V-A4 (AboutSlint) are **accepted by the owner (2026-11-04)**. V-ST1,
V-T13, V-T10, V-T9 and V-SP5 are **to fix in FX, re-reviewed in V2**: not fixed yet, so the verdicts above stand
until V2.

### Owner decisions (2026-11-04)

The owner's answers to this review. Each one also covers the rows that point to that item (V-D7 to V-T9, V-D8 to
V-T10, V-D9 and V-A5 to V-T13, V-ST6 to V-SP5).

| # | item | decision | state |
|---|---|---|---|
| 1 | V-ST1 combination slices | Compute the diagonal stripe ∩ sector polygons in Rust (like 1.x, about 6 px period), not rings | fixed (V2, 2026-11-05) |
| 2 | V-T8 M1 menu Timeline + About | Accepted | accepted by the owner (2026-11-04) |
| 3 | V-SP2 period label next to "Share of time" | Keep it (the owner: "it's awesome"); mark it as a 2.0 addition (PARITY.md row 410) | accepted by the owner (2026-11-04) |
| 4 | V-T13 line spacing | Match the 1.x line-height 1.45; lay out the lines by hand if Slint has nothing for it | fixed (V2, 2026-11-05) |
| 5 | V-T10 scrollbar | A custom overlay thumb in Theme tokens: hidden at rest, shown while scrolling or hovering, hidden again when the mouse stops; a one-shot timer only, nothing running when idle | fixed (V2, 2026-11-05) |
| 6 | V-T9 card shadows | Treat as a bug and make them render; prove it with a screenshot | fixed (V2, 2026-11-05) |
| 7 | V-A4 AboutSlint | Accepted | accepted by the owner (2026-11-04) |
| 8 | V-SP5 legend | Cap the legend's width on the wide Timeline card | fixed (V2, 2026-11-05) |

### Timeline, Light (`timeline-light-tauri.png` | `timeline-light-slint.png`)

Verdict: **REVISE** - DEFECT V-T1 to V-T5.

| id | region | Tauri | Slint | class | sev | fix |
|---|---|---|---|---|---|---|
| V-T1 | task action buttons: Start/Pause/Resume/Stop icons | filled glyphs also carry `stroke=currentColor strokeWidth=2` from `Svg` (1.x `components/icons.tsx:5-15`), so play/pause/stop are 2 view-box units larger per side with rounded corners (pause bars 6 units wide) | `Icon` sets `stroke: filled ? transparent : color` (`widgets.slint:18`): filled glyphs are fill-only, visibly smaller and sharper (Slint pause/stop/play at 122-200,255-268 vs Tauri 125-210,505-520) | DEFECT | medium | `widgets.slint` `Icon`: `stroke: color;` for every glyph (keep `fill: filled ? color : transparent`, `stroke-width` as for the outlined glyphs, round cap/join). Verify: pause bar width equals 1.x (6/24 of the icon box) **V2: FIXED** |
| V-T2 | project chip, tag chip, state badge (every card) | `.chip` 12.5 px x line-height 1.5 + 2 px padding: coloured body 18 px (40,273-290); `.badge` 12 px x 1.45 + 1 px padding + border, about 21 px; `.tag-chip` 12 px x 1.5 + 1 px padding, 20 px | ProjectChip body 13 px (38,171-183); Badge about 16 px; TagChip about 15 px: heights come from the glyph height (`preferred-height + 4px/2px`) with no line-height | DEFECT | medium | `widgets.slint`: ProjectChip `height: 12.5px * 1.5 + 4px;` Badge `height: 12px * 1.45 + 4px;` TagChip `height: 12px * 1.5 + 2px;` (text stays `vertical-alignment: center`). Verify: chip body 18 px, badge 21 px **V2: FIXED** |
| V-T3 | boxed pause note ("Paused by you.") | `.task-note-pause` 13 px x 1.45, padding 6/10: body 30 px (300,660-689) | body 23 px (330,358-380) | DEFECT | low | `rail.slint` `Note` Text: `min-height: 13px * 1.45;` (one-line notes match; multi-line spacing stays V-T13) **V2: FIXED** |
| V-T4 | task action buttons | 28 px tall (222,726-753) | 26 px tall (215,391-416) | DEFECT | low | `widgets.slint` Button, `small` variant: `min-height: 28px;` **V2: FIXED** |
| V-T5 | tab bar labels | "Timeline" glyphs y 77-87, underline y 102-104, 61 px wide (454-514) | glyphs y 74-83, underline y 103-105, 65 px wide (452-516): label 3 px higher, 5 px more gap to the underline, underline 4 px wider | DEFECT | low | tab item: label padding-top +3 px (8 px + half of 15 px x 0.45 leading), tab bar 1 px shorter, underline inset 12 px each side (`shell.css:26` `left/right: 12px`) **V2: FIXED** |
| V-T6 | card title weight, clock weight | `.task-name` 600, `.ticker-clock` 650 | rendered bold (700): the code already asks 600 / 650 (`theme.slint:254,257`), the Mac fallback font has no 600 | SLINT-LIMIT (environment) | low | none; judge on Windows with Segoe UI Variable |
| V-T7 | header | brand, zone switch (My time / Client time / UTC / Other...), "Showing Lisbon, WET, UTC+0", Show both, Hide earnings, Export | brand only (same height 54 px, border `border` at y 54) | SCOPE | - | milestones 4-5 |
| V-T8 | tabs | Timeline, Analysis, Billing, Projects, Settings | Timeline, About | SCOPE | - | About is a Settings section in 1.x; the M1 menu (Timeline, About) is **accepted by the owner (2026-11-04)** |
| V-T9 | card shadows (task cards, Timeline cards) | `--shadow` visible below every card (y 578-583: #dadbde -> #eff0f2) | none in the PNG (y 288-295 all `bg` #f3f4f6) although `rail.slint:157-160` and `widgets.slint:347-349` declare a drop shadow | SLINT-LIMIT (renderer, unverified) | low | likely the software renderer used for the screenshots; verify in the Windows build. If the Windows renderer draws it, close; if not, DEFECT **V2: FIXED** |
| V-T10 | rail scrollbar | none visible in the 1.x capture | permanent thumb at 398-402,128-505, colour #858587 (not a token) | SLINT-LIMIT | low | std-widgets ScrollView takes its colours from the style palette, not Theme; options: accept, or replace with a Flickable plus a thumb in `Theme.border-strong` **V2: FIXED** |
| V-T11 | font family | Segoe UI Variable chain; this capture used the system UI font | Slint fallback font (Helvetica-like); text widths within 1-3 % ("Tasks" 46 vs 49 px, brand 105 vs 103 px) | SLINT-LIMIT (environment) | low | judge on Windows (README); Slint takes one family, no CSS fallback list |
| V-T12 | rail: top controls | "New task", search field, "More filters" | none | SCOPE | - | milestone 2 |
| V-T13 | wrapped text line spacing | body line-height 1.45 | lines about 13-14 px apart (note "Paused when the app was closed..." at 44,646-671) | SLINT-LIMIT | low | Slint Text has no line-height; approve **V2: FIXED** |
| V-T14 | running card live box | clock + Today exact, Today billable, Billable now | clock + "this session" only (same box colours #eaf1ed / #8bac9d) | SCOPE | - | milestone 5 (billing) |
| V-T15 | card lower rows | "Billed 0s, unbilled ..." line; edit pencil, Details, Description link | none | SCOPE | - | later milestones |
| V-T16 | Timeline tab body | period totals card, filter, kinds legend, every day newest first | day picker (new), one day card, Share of time card | SCOPE | - | README item 3; day picker is 2.0-only |
| V-T17 | day card header | title + zone label "Lisbon, WET, UTC+0", Billable / Exact totals, per-project line | title only | SCOPE | - | milestone 4 |
| V-T18 | axis track height | 54 px (605-658): a shift-band label lane above the blocks | 36 px (253-288), blocks 24 px as in 1.x | SCOPE | - | shift bands are not in M1; recheck the height in the milestone that adds them |
| V-T19 | shift bands, axis caption, entries table | "Shaded bands are shifts...", table TIME / TASK / PROJECT / DURATION | none | SCOPE | - | later milestones |
| V-T20 | axis blocks | label inside wide blocks (first part of the label), tooltip | task name, no tooltip | SCOPE | - | README item 7 |
| V-T21 | day picker colours | (no 1.x element) | selected pill bg #e7f1f0 = accent 10 % over surface, border/text accent #0f766e; others surface + border-strong #6b7280 + text-muted | SCOPE | - | 2.0-only element; all colours token or token mix |
| V-T22 | sample content | Logo concepts running 1:20:00, ACTIVE 3, Thu 12 Mar 2026 | Social templates running 1:19:59, ACTIVE 5 + DRAFTS 1, Wed 4 Nov 2026 | DATA | - | - |
| V-T23 | tag chips on rail cards | (1.x sample tasks show none) | Concept / Admin / Revision chips after "logged", same order as 1.x `TaskCard.tsx:84-87` | DATA | - | - |
| V-T24 | Share of time numbers | (not on 1.x Timeline) | 13h 43m (46/32/22 %) while the crop shows 13h 44m (45/33/22 %): captures taken at different running seconds | DATA | - | - |

### Timeline, Solarized Dark (`timeline-solarized-dark-tauri.png` | `timeline-solarized-dark-slint.png`)

Verdict: **REVISE** - the same component defects as Light; no dark-only defect.

| id | region | Tauri | Slint | class | sev | fix |
|---|---|---|---|---|---|---|
| V-D1 | icons | as V-T1 | as V-T1 | DEFECT | medium | V-T1 **V2: FIXED** |
| V-D2 | chips, badges | as V-T2 | as V-T2 | DEFECT | medium | V-T2 **V2: FIXED** |
| V-D3 | pause note height | as V-T3 (note bg #163630, both apps) | as V-T3 | DEFECT | low | V-T3 **V2: FIXED** |
| V-D4 | button height | as V-T4 | as V-T4 | DEFECT | low | V-T4 **V2: FIXED** |
| V-D5 | tab labels | as V-T5 | as V-T5 | DEFECT | low | V-T5 **V2: FIXED** |
| V-D6 | title weight | as V-T6 | as V-T6 | SLINT-LIMIT (environment) | low | - |
| V-D7 | card shadow | `--shadow` faint above/below cards (#022d37 next to border #14505e) | none | SLINT-LIMIT (unverified) | low | V-T9 **V2: FIXED** |
| V-D8 | rail scrollbar | none | thumb #143b45 (not a token) | SLINT-LIMIT | low | V-T10 **V2: FIXED** |
| V-D9 | font, line spacing | as V-T11, V-T13 | as V-T11, V-T13 | SLINT-LIMIT | low | - **V2: FIXED** |
| V-D10 | primary button text | Resume text on-accent #002b36 on accent #2aa198 | #002a35 on #2aa198 (token, anti-aliased) | - | - | matches |
| V-D11 | project colours in dark | project chips keep their own colours (Brand refresh #2563eb) | same, and the donut keeps project colours | DATA | - | - |
| V-D12 | scope rows | V-T7, V-T8, V-T12, V-T14 to V-T21 | same | SCOPE | - | - |
| V-D13 | sample content | as V-T22 to V-T24 | as V-T22 to V-T24 | DATA | - | - |

### Share of time, Projects, Light (`share-light-projects-tauri.png` 420x505 | `share-light-projects-slint.png` 892x344)

Verdict: **REVISE** - one low defect (V-SP1); everything else is scope or data. Ring geometry matches: outer
diameter 238 vs 236 px, ring 41 px in both; slice gaps white about 2 px in both; swatch 12 px with 1 px border in both;
segmented control 32 px tall, segment widths within 2 px; selected segment accent #0f766e with on-accent text.

| id | region | Tauri | Slint | class | sev | fix |
|---|---|---|---|---|---|---|
| V-SP1 | legend row pitch | 28 px per row (swatches at y 359, 387, 415, 443): `.a-share-legend li` padding 4 px 8 px, 13 px x 1.45, gap 1 px | 22 px per row (y 178, 200, 222) | DEFECT | low | `share.slint` legend row: `height: 13px * 1.45 + 8px;` (about 27 px) and row spacing 1 px **V2: FIXED** |
| V-SP2 | title row | "Share of time" + info (i) button | "Share of time" + period "2 Nov - 8 Nov 2026" (text-muted), no (i) | SCOPE | low | (i) and its popover come later (README 5). The period is 2.0-only (1.x shows it in the Analysis controls) and comes from the core's period label, not strings.slint: **accepted by the owner (2026-11-04)**: keep it, a 2.0 addition (PARITY.md row 410) |
| V-SP3 | basis | centre "Billable" | centre "Exact" | SCOPE | - | milestone 5 |
| V-SP4 | "View data" disclosure | present | absent | SCOPE | - | later milestone |
| V-SP5 | legend placement | below the donut (narrow Analysis card) | beside the donut on the wide Timeline card; figures at the card's right edge, about 440 px from the names | SCOPE | low | follows the 1.x rule (`analysis.css:124-130`, side by side when wide); owner may cap the legend width if the gap reads badly **V2: FIXED** |
| V-SP6 | Subtasks segment | disabled (muted text) | enabled | DATA | - | 1.x sample has no subtask time in that period; Slint has the same rule (`share-no-subtask-time`) |
| V-SP7 | slices, totals | 4 projects incl. Admin, 24h 15m | 3 projects, 13h 44m | DATA | - | - |
| V-SP8 | font | as V-T11 | as V-T11 | SLINT-LIMIT | low | - |

### Share of time, Tags, Light (`share-light-tags-tauri.png` 420x629 | `share-light-tags-slint.png` 892x344)

Verdict: **REVISE** - two low defects; V-ST1 is a limit the owner must approve explicitly.

| id | region | Tauri | Slint | class | sev | fix |
|---|---|---|---|---|---|---|
| V-ST1 | combination slices on the ring | diagonal 2-colour stripes, about 6 px period, across the slice | concentric 4 px bands round the ring | SLINT-LIMIT | medium | README 5: Slint cannot clip a pattern to an arc. Workaround exists if the owner rejects: compute stripe-and-sector intersection polygons in Rust and pass them as `Path` commands **V2: FIXED** |
| V-ST2 | combination legend swatch | 12 px swatch with 2-3 diagonal stripes of the member colours | one diagonal band ("Concept + Revision", "Admin + Concept" at 304,199-232) | DEFECT | low | `share.slint` striped swatch: repeat the band every 4 px across the 12 px box (3 bands), same angle as 1.x (135 deg) **V2: FIXED** |
| V-ST3 | legend row pitch | 28 px | 22 px | DEFECT | low | V-SP1 (same fix) **V2: FIXED** |
| V-ST4 | combination note | "Tasks with several tags are counted once, under their combination." wrapped, muted | same text, one line, muted | - | - | wording exact |
| V-ST5 | (i), View data, basis, period | as V-SP2 to V-SP4 | as V-SP2 to V-SP4 | SCOPE | - | - |
| V-ST6 | legend placement | below | beside | SCOPE | low | V-SP5 **V2: FIXED** |
| V-ST7 | rows | Concept, Revision, 3 combinations, No tag, Admin; 28h 15m | Concept, Revision, Client call, 2 combinations, Admin; 13h 44m | DATA | - | - |
| V-ST8 | "Tags" selected segment, "Subtasks" enabled | as Slint | accent bg, on-accent text | DATA | - | matches |

### About, Light (`about-light-slint.png`; no 1.x image, compared with 1.x `OtherSections.tsx:155-184`)

Verdict: **PASS** - every 1.x string present matches letter by letter; contrast AA everywhere.

| id | region | 1.x source | Slint | class | sev | fix |
|---|---|---|---|---|---|---|
| V-A1 | wording | "App version"; zone-rules paragraph; privacy paragraph (OtherSections.tsx:161, 178, 180) | identical, same order (zone rules, then privacy) | - | - | matches |
| V-A2 | rows not shown | "Time zone data (bundled in this app)", "Time zone data (backend)", "This device", and the "Map and time zone data" attribution (ZoneDataAttribution.tsx) | absent | SCOPE | low | zones and sync milestones; the map attribution must arrive with the map data |
| V-A3 | 2.0 additions | none in 1.x About | card title "zWorkTracker" (18 px), "zWorkTracker is MIT licensed (see LICENSE).", "Third-party notices" link with chevron (accent #0f766e, 5.47:1), second card with AboutSlint and "zWorkTracker uses Slint for its user interface, under the Slint Royalty-free Desktop, Mobile, and Web Applications License 2.0." | SCOPE | low | sourced in strings.slint (THIRD_PARTY_NOTICES.md, PLAN.md 9); owner approves |
| V-A4 | AboutSlint | - | Slint's own widget: logo badge, "Version 1.18.1", "https://slint.dev/" at about 9 px (glyph rows 487-505), ink #191919 / #1b1b1b (not tokens), 17.6:1 on white | SLINT-LIMIT | low | widget is not styleable; legible but small; **accepted by the owner (2026-11-04)** |
| V-A5 | wrapped paragraph spacing | line-height 1.45 | lines 14 px apart (203, 217) | SLINT-LIMIT | low | V-T13 **V2: FIXED** |
| V-A6 | contrast | - | body #111827 on #ffffff 17.74:1; link 5.47:1; AboutSlint text 17.2-17.6:1 | - | - | AA met |

### Colour samples (median of a box; "ink" = the glyph pixel farthest from the background)

| element | token (Light) | Tauri Light | Slint Light | token (Sol. Dark) | Tauri Dark | Slint Dark |
|---|---|---|---|---|---|---|
| header, cards | surface #ffffff / #002b36 | #ffffff | #ffffff | surface #002b36 | #002b36 | #002b36 |
| rail and main bg | bg #f3f4f6 / #002b36 | #f3f4f6 | #f3f4f6 | bg #002b36 | #002b36 | #002b36 |
| header border, divider, card border | border #d9dce3 / #14505e | #d9dce3 | #d9dce3 | #14505e | #14505e | #14505e |
| title and tab text | text #111827 / #eee8d5 | #111827 | #101726 | #eee8d5 | #eee8d5 | #eee8d5 |
| ACTIVE, this session, inactive tab | text-muted #4b5563 / #839496 | #4b5563 | #4a5462 | #839496 | #839496 | #839496 |
| tab underline, Resume, selected segment | accent #0f766e / #2aa198 | #0f766e | #0f766e | #2aa198 | #2aa198 | #2aa198 |
| running border and badge | success #166534 / #859900 | #166534 | #166534 | #859900 | #859900 | #859900 |
| paused border and badge | warning #92400e / #b58900 | #92400e | #92400e | #b58900 | #b58900 | #b58900 |
| now line | danger #b91c1c / #f0655f | #b91c1c | #b91c1c | #f0655f | #f0655f | #f0655f |
| axis frame | border-strong #6b7280 / #839496 | #6b7280 | #6b7280 | #839496 | #839496 | #839496 |
| axis grid (as drawn) | grid #d1d5db / #14505e, faded | #e6e8eb | #e5e7eb | | #0b3f4c | #0a3e4b |
| axis labels | axis #4b5563 / #839496 | #4b5563 | #505a67 (aa) | | #839496 | #7e9092 (aa) |
| live box bg / border (derived) | success mix | #eaf1ed / #8bac9d | #eaf1ed / #8bac9d | | #0c3531 / #416d38 | #0c3531 / #416d38 |
| pause note bg (derived) | warning 12 % over surface | #f2e8e2 | #f2e8e2 | | #163630 | #163630 |
| disabled button bg (derived) | | #eef0f3 | #eef0f3 | | #05333f | #05323e |
| rail scrollbar | (none) | - | #858587 (not a token) | | - | #143b45 (not a token) **V2: no thumb at rest; the overlay thumb is border-strong** |
