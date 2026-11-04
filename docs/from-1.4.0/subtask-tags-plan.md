# Plan: zWorkTracker 1.4.0, subtask tags, then release (v1)
Plan id: 140-subtask-tags | Author: planner | Created: 2026-11-04 06:38 | requires_depth: 2 | DAG: `dag.json` (source of truth) | check: `dag.dagcheck.txt` | flowchart: `dag.mmd`

## Summary
27 nodes in 14 waves. Wave 1 (W0): H0 owner answers, S1 compatibility spike on real 1.3.3 code, B0 branch sync after live-updates. Critical path S1 -> D1 -> I1 -> A1 -> A2 -> E1 -> G1 -> QC -> PKG1 -> H1 -> H2 -> M1 -> H4 -> SUB1, 197 owner-turns; total work 388, parallelism 1.97. Main risk: data loss when 1.3.x and 1.4 devices share a folder. C1 tests it with fixtures that the 1.3.3 code itself writes.

## Goal, done-when, non-goals
Outcome: in 1.4.0 a subtask can carry a subset of its task's tags. "Same as task" is the default. The tags count in analysis and exports, the data stays safe next to 1.3.x devices, and the release is packaged, checked on the VM and submitted to the Store.

Done when (each maps to nodes):
1. You can set and reset a subtask's tags on its row with the mouse and with the keyboard (UI1, E1).
2. An override written by 1.4 survives a sync round trip through 1.3.3 (C1, whose fixtures come from commit fd51d79).
3. Share of time Tags, the tag filter, tag metrics and the Excel Analytics sheets count subtask time under its effective tags, and Rust and TS agree through the parity fixtures (A1, A2).
4. 1.4.0 appears everywhere and the MSIX is 1.4.0.0. README, howto and Store notice are updated (R1, DOC1, PKG1).
5. GATE-ALL exits 0 on HEAD and quality-control says SHIP (G1, QC).
6. The owner passes the VM checks, approves the 10 screenshots, and the submission shows In certification (H1-H4, SUB1).

Non-goals: subtask tags that are not on the task; tags in billing summaries or quotes; the live-updates work; pushing to the remote; copies and templates carrying overrides (unless the Q5 answer says so).

## Repo facts this plan rests on
- `Task` (model.rs:159-198) has per-field stamps (`tags`/`tags_stamp`) and no `#[serde(flatten)]` catch-all. Serde ignores unknown fields when reading, so a 1.3.x build drops a field it does not know when it writes its own file. S1 measures this.
- merge.rs:150-172 merges each field by stamp (last writer wins). A field that is missing deserialises with the default stamp, so the 1.4 device's newer stamp should still win after 1.3.x rewrites its own file without the field. C1 proves it.
- SCHEMA_VERSION = 2 (model.rs:10). Remote files with a higher version are skipped as newer (sync.rs:434-440), so a bump would cut 1.3.x devices off from the 1.4 device. Keep 2.
- SQLite keeps each record as JSON (store.rs:59-63, `records(kind,id,json)`), so the new field needs no SQL migration.
- Subtasks are `taskItem` nodes with `attrs.subtaskId` (subtasks.rs:139-150, RichText.tsx:15-23). The 1.3.x TipTap schema declares only `checked` and `subtaskId`, so attributes stored in the description would likely be dropped the next time a 1.3.x editor saves it. S1 measures this too.
- Analysis exists twice: Rust analytics.rs:1697-1770 (Excel Analytics, the tags sheet) and TS src/features/analysis/* (engine, tagFilter, shareSlices). fixtures/analytics-parity.json and analyticsParity.test.ts tie them together. CSV and PDF print no tags (no match for `tags` in csv.rs or pdf.rs). report.rs:122 trims tags to the ones in use.
- The commands come from .github/workflows/ci.yml:44-73. There are no git tags; 1.3.3 is HEAD fd51d79.

## Gate sets (used in `verify.how`)
- GATE-TS: `pnpm exec tsc --noEmit; pnpm exec eslint . --max-warnings 0; pnpm exec vitest run; pnpm exec playwright test`
- GATE-RS: `cargo fmt --manifest-path src-tauri/Cargo.toml --all --check`; clippy `--workspace --all-targets --locked -- -D warnings`, the same with `--features smoke`, and clippy `-p z-work-core -p z-work-session --all-targets --locked --target x86_64-pc-windows-gnu -- -D warnings`; `cargo test --manifest-path src-tauri/Cargo.toml --workspace --locked`
- GATE-FIX: the 1.3.1 fixtures are unchanged against fd51d79 (`git diff --exit-code fd51d79 -- .../tests/fixtures/cloud-1.3.1*`). The parity fixtures run inside vitest and cargo test.
- GATE-ALL = GATE-TS + GATE-RS + GATE-FIX + `pnpm check:readme`. B0, R1 and G1 run all of it. The executor's per-node validator gate runs GATE-ALL on every code node, as the brief asks. A node's `verify.how` names its own smallest gate set.
- CI only runs `cargo check --tests` for the GNU target. If clippy for that target fails for reasons that have nothing to do with this work, record it and fall back to CI's `cargo check` (Open doubt).

## Semantics baseline
D1 confirms or amends these rules and writes them into `fixtures/subtask-tags-parity.json`.
- **Stored:** only overrides, keyed by subtaskId, in a new Task field (working name `subtaskTags`). It is skipped when empty, so a file with no overrides stays byte-identical to 1.3.x. Each entry has its own stamp, so two devices editing different subtasks both keep their edits. A reset to "Same as task" is an entry with `tags: null` and a stamp, so the reset reaches other devices.
- **Effective tags** of a segment:
  - The segment is on a subtask with an override: the override ∩ the task's live tags, in the task's order.
  - Otherwise, including time on no subtask and time on a subtask id that no longer exists: the task's live tags.
- **Adding** a tag to the task reaches subtasks on "Same as task" only. **Removing** one prunes it from every override in the same write, and the read-time intersection covers removals made on 1.3.x, which cannot prune.
- **Empty override** (all tags unticked, or the last one pruned) = `No tag` (Q1).
- **Retroactive:** an override applies to all the subtask's logged time, as task tags already do.
- **Tag filter:** none selected = everything. With a selection S, a segment stays when its effective tags meet S, or when they are empty and `No tag` ∈ S.
- **Share of time Tags:** each exact combination of tags is one row, now per piece of time instead of per task, so the rows still add up to the total.
- **Excel tags sheet:** still overlapping, one row per tag, counting segments.
- Subtask tags are always a subset of the task's tags, so the task-list tag filter (TaskFilterBar) does not change.
- **Copies and templates:** `subtasks::reset` forgets subtask ids, so copies and templates get "Same as task" (Q5).

## UI baseline
U1 fixes the details. Recommended for Q3:
- On each subtask row of the task card (TaskCardExtras), next to its time and "Make active", a small tags button. It shows the effective chips, or the text "Same as task".
- The button opens a checklist popover: a "Same as task" choice at the top, then the task's tags as checkboxes.
- Keyboard: Tab reaches the button; Enter or Space opens it; the arrow keys move; Space toggles; Escape closes and returns focus to the button.
- Disabled in the read-only and held states.
- Not inside the description editor.

## Nodes
| id | title | owner | deps | writes (abridged) | key acceptance | verify | risk | est |
|---|---|---|---|---|---|---|---|---|
| H0 | Owner answers Q1-Q5 | caller | - | nodes/H0 | answers recorded | user | 1x2 | 2 |
| S1 | Spike: 1.3.3 vs unknown fields, measured | rust-expert | - | nodes/S1 | per-path survive/lost, with JSON diffs from real fd51d79 runs | validator re-runs one | 2x3 | 20 |
| B0 | Sync with main after live-updates; baseline gates | general-purpose | - | nodes/B0 | main ⊂ HEAD; GATE-ALL 0 | GATE-ALL | 2x2 | 8 |
| D1 | Design note + parity fixture | computer-science-expert | S1,H0 | nodes/D1, fixtures/subtask-tags-parity.json | shape, merge, API, prune, >=12 cases | plan-critic review | 2x3 | 20 |
| U1 | UI/wording spec | typescript-expert | H0 | nodes/U1 | strings, placement, key table | reviewer | 1x2 | 8 |
| R1 | Version 1.4.0 in manifests + lock | general-purpose | B0 | package.json, tauri.conf.json, 3 Cargo.toml, Cargo.lock | no 1.3.3 left there; msixVersion 1.4.0.0 | GATE-ALL | 1x2 | 4 |
| I1 | Core model/merge/HLC/validate/changes/effective fn | rust-expert | D1,B0 | model, merge, hlc, validate, changes, epoch, subtasks .rs | fixture cases; merge proptest; 1.3.1 byte-for-byte | GATE-RS+FIX | 2x3 | 35 |
| T1 | TS types/api/effective mirror/mock | typescript-expert | D1,B0 | src/lib/{types,api,subtasks}.ts, src/mocks/ipcMock.ts | same fixture vs mock | GATE-TS | 2x2 | 20 |
| I2 | Service + Tauri command, CAS, pruning | rust-expert | I1 | machine.rs, service.rs, src-tauri/src/lib.rs | prune in one write; CAS refuses stale | GATE-RS | 2x2 | 25 |
| C1 | Mixed-version proof (1.3.3-made fixtures) | tester | I1,S1 | tests/fixtures/cloud-1.4.0-mixed, cloud_mixed_tests.rs, core lib.rs | 3 named tests + mutation check | cargo test cloud_mixed | 2x3 | 25 |
| A1 | Rust analytics/report/xlsx + parity fixture | rust-expert | I1 | analytics.rs, report.rs, xlsx.rs, fixtures/analytics-parity.json | subset case; no-override outputs unchanged | GATE-RS + verify_xlsx.py | 2x3 | 25 |
| UI1 | Subtask tag control + tests | typescript-expert | T1,U1 | TaskCardExtras.tsx, SubtaskTags.tsx, tasksP2.css | keyboard+mouse tests; read-only off | GATE-TS | 2x2 | 25 |
| A2 | TS analysis by effective tags | typescript-expert | T1,A1 | src/features/analysis/** | parity; filter ANY/No tag; rows sum | GATE-TS | 2x3 | 30 |
| DOC1 | README, Store What's new 1.4.0, howto | general-purpose | R1,U1,UI1,A1 | README.md, STORE_LISTING_NOTICE.md, site/howto.html | 1.4.0 strings; check:readme | check:readme | 1x2 | 12 |
| REV1 | Review core/sync/compat | reviewer | I2,C1 | nodes/REV1 | 0 open blocker/major | adversarial-verifier | 2x3 | 20 |
| E1 | Playwright E2E | tester | UI1,A2 | e2e/subtask-tags.spec.ts | >=4 tests; mutation note; full suite 0 | playwright | 2x2 | 20 |
| REV2 | Review analysis/UI/docs | reviewer | A2,UI1,DOC1 | nodes/REV2 | 0 open blocker/major | adversarial-verifier | 2x2 | 20 |
| SHOT1 | Persona seeds + 10 Store screenshots | tester | UI1,A2 | src/mocks/storeSeed.ts, e2e/store-shots.spec.ts | 10 PNG 1920x1080, 4/3/3 personas | playwright with ZWT_STORE_SHOTS | 1x2 | 20 |
| G1 | Validator: GATE-ALL + DoD 1-5 | validator | all code, docs, reviews | nodes/G1 | all PASS | GATE-ALL | 1x3 | 10 |
| QC | Quality-control before release | quality-control | G1 | nodes/QC | SHIP | validator | 1x3 | 15 |
| PKG1 | Build exe + both MSIX 1.4.0.0 | general-purpose | QC | target/, nodes/PKG1 | 3 files + SHA-256 | ls + shasum | 2x2 | 8 |
| H1 | Owner: first real `pnpm msix:vm` | caller | PKG1 | nodes/H1 | WACK PASS; test install starts | user | 2x3 | 3 |
| H2 | Owner: VM checklist | caller | H1 | nodes/H2 | all lines pass | user | 2x3 | 2 |
| H3 | Owner: approve 10 screenshots | caller | SHOT1,QC | nodes/H3 | approved or retake list | user | 1x2 | 2 |
| M1 | Commit + merge to main locally, no push | general-purpose | H2,H3 | .git | merge on main; no push | git log | 1x2 | 3 |
| H4 | Owner go/no-go for Partner Center | caller | M1 | nodes/H4 | go + SHA-256 | user | 1x3 | 2 |
| SUB1 | Guided submission (irreversible) | caller | H4 | nodes/SUB1 | In certification | user | 1x3 | 4 |

Full acceptance criteria, inputs and rollbacks are in `dag.json`.

## Waves and critical path
W0 H0, S1, B0 | W1 U1, D1, R1 | W2 I1, T1 | W3 I2, C1, A1, UI1 | W4 REV1, A2, DOC1 | W5 E1, SHOT1, REV2 | W6 G1 | W7 QC | W8 PKG1, H3 | W9 H1 | W10 H2 | W11 M1 | W12 H4 | W13 SUB1.
- Critical path: S1 -> D1 -> I1 -> A1 -> A2 -> E1 -> G1 -> QC -> PKG1 -> H1 -> H2 -> M1 -> H4 -> SUB1, L = 197 turns, W = 388, P = 1.97.
- Slack: U1 83, R1 106, B0 32, T1 33, UI1 33, I2 and C1 30. These could take cheaper owners at no cost in time.
- Executor note:
  - I2, C1 and A1 all edit or build z-work-core in W3. Run them with `isolation: "worktree"` and merge in the order I2, A1, C1, or run them one after another.
  - A1 and I2 write different files, but both must build together before REV1 and A2.

## Risks, mitigations, rollbacks
| node | risk | mitigation | rollback |
|---|---|---|---|
| S1/C1 | 1.3.3 drops or corrupts the field in a way the stamp rule does not save (6) | measured on real fd51d79 code first; C1 fixtures come from that code; a mutation check proves the test can fail | C1 touches no product code; re-plan D1 if S1 falsifies the rule |
| D1/I1 | merge loses a concurrent edit to a different subtask (6) | per-entry stamps; merge proptest (commutative, associative, idempotent) | revert the node commit; no-override files are byte-identical |
| A1/A2 | analysis drifts between Rust and TS, or no-override figures change (6) | shared parity fixtures; "outputs unchanged without overrides" is an acceptance criterion | revert |
| B0 | live-updates lands late or touches the same UI and analysis files (4) | B0 blocks code nodes until main holds it; spikes run meanwhile; re-plan trigger | reset to the recorded pre-merge commit |
| H1/H2 | first real msix:vm run fails WACK, or the VM shows a tray or taskbar problem (6) | `pnpm msix:vm --dry-run` before the real run; failure loops back through a fix node, G1, QC, PKG1, H1 | nothing is published before H4 |
| SUB1 | wrong package uploaded (3, irreversible) | H4 records the SHA-256 to upload | cancel the submission before certification, or ship 1.4.1 |
| local data | a user downgrades 1.4 -> 1.3.x and 1.3.x drops the field from local records (2) | none in code; one sentence in the README migration guide (DOC1) | - |

## VM checklist
For H2, on the installed test package in the Windows VM:
1. The taskbar icon and the Start tile show the round-clock logo with no plate.
2. The tray icon is idle when stopped and running when running, and its menu works.
3. Share of time draws the donut at all four levels. The Tags level splits a task whose subtasks carry different tags.
4. Setting, resetting and unticking all tags on a subtask works with the mouse and with the keyboard.
5. Settings > About says 1.4.0.

## Re-plan triggers
- S1 shows 1.3.3 rejects or corrupts a file with an unknown field, or the stamp rule does not hold.
- D1 chooses taskItem attributes in the description instead of a Task field.
- A node runs past 1.5x its est_turns.
- Any gate fails twice.
- Live-updates changes TaskCardExtras.tsx, src/features/analysis/*, api.ts or ipcMock.ts after B0.
- WACK FAIL in H1, or any failed line in H2.
- The owner's H0 answers change scope (for example Q5 = yes, or Q3 adds the editor).

## Open questions for the owner
1. **Q1, empty subset.** When every tag is unticked on a subtask, or the last tag it kept is removed from the task, does the subtask count as `No tag` or go back to "Same as task"? **Default: `No tag`.** This matches "filter out", and "Same as task" stays one click away.
2. **Q2, what "older clients keep unknown fields" can mean.** Shipped 1.3.x can't be changed, and its serde drops fields it does not know when it writes its own file. The plan keeps overrides safe another way: they are stamped per entry and live in the 1.4 device's file, and C1 proves this. Do you accept that? Should 1.4 also keep unknown fields from future versions (a catch-all on Task and Segment) so that 1.5 is safe next to 1.4? **Default: accept, and add the catch-all to I1.** That adds about 10 turns.
3. **Q3, where the subset is chosen.** **Default: a tags button on each subtask row of the task card, with a checklist popover (UI baseline). Not in the description editor or the new-task form.**
4. **Q4, the Store screenshots.** Generate them with Playwright from the mock, using three persona seeds, at 1920x1080 (reproducible and kept in the repo), or capture them from the real app in the VM? **Default: Playwright from the mock**, split 4 web developer, 3 software engineer, 3 data scientist, written to `~/msix-build/store-listing-screenshots-1.4.0/` so the 1.3.x set stays.
5. **Q5, copies and templates.** Should they keep subtask tag overrides, matched by subtask text? **Default: no.** A copy starts at "Same as task", as its subtask ids are reset.

## plan-critic
Not run: the session's spawn budget is spent (128/128, hook denial). This is a self-critique against the planner quality bar:
- Every node has an owner, inputs, a write-set and acceptance criteria that can fail. The caller nodes write their record to nodes/<id>/.
- Irreversible SUB1 has the H4 checkpoint directly upstream and a rollback.
- The plan ends with SUB1 (caller) and not a validator, because the brief puts the release steps after quality-control. G1 and QC are the last mechanical and quality gates.
- Concurrent writers: no same-file pairs (dag.py). W3 builds the same crate in parallel, so see the executor note.
- Gap: SHOT1 also writes outside the repo (~/msix-build/store-listing-screenshots-1.4.0/), which dag.py cannot check. No other node writes there.
- Weakest point: the mixed-version safety argument depends on S1. If S1 contradicts the stamp rule (for example, 1.3.x reconcile restamps the whole task), D1 and C1 must be re-planned before I1.

## dag.py output
`dag.dagcheck.txt` (same folder): `ERRORS: none`, 27 nodes, 49 edges, 14 waves, critical path 197 turns.

## Owner answers (2026-11-04, H0 done)
- Q1 empty subset counts as "No tag": "it's ok" (accepted default).
- Q2 catch-all for unknown fields on Task and Segment in 1.4: "yes".
- Q3 tags button with checklist popover on each subtask row of the task card: "yes and should be a button to select and deselect all tags" -> the popover gets one Select all / Deselect all toggle.
- Q4 screenshots from Playwright mock, three persona seeds, 1920x1080, 4/3/3, into ~/msix-build/store-listing-screenshots-1.4.0/: "correct".
- Q5 copies and templates start at "Same as task": "yes".
