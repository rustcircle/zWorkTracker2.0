# Requests for z-work-core

> **Since 2026-11-04 the core is vendored in this repository** at `crates/z-work-core`, copied from the 1.x repo at
> `a88c1aa` ([DEPENDENCIES.md](DEPENDENCIES.md)). A change listed below can now be made here, in
> `crates/z-work-core`, in its own commit; drop its workaround in the same change. The list stays as the backlog.

Notes for the owner: what zWorkTracker 2.0 would like from the core (written when it was still the 1.x repo's
`src-tauri/crates/z-work-core`, pinned at `a88c1aa`). Until a request lands, the workaround named here lives in this
repository (mostly `crates/zwt-bridge`). Started in milestone 1 (node B1, 2026-11-04).

## Summary

| # | Request | Why | Workaround in 2.0 now |
|---|---|---|---|
| 1 | A public change event after every commit and sync merge | Per-topic updates without diffing the whole file (PLAN.md section 5, live-updates Change 3) | `zwt-bridge` keeps the last file it saw and diffs sections after each revision change |
| 2 | A preference for the Share of time level | The picked level should survive a restart (PARITY 212) | Kept in worker memory: lost at quit |
| 3 | A public sample-data builder | `testkit` is `cfg(test)` only | `zwt_bridge::sample::build`, seeded with `store::seed_store` |
| 4 | Effective subtask tags | PARITY 221 (subtask time under the subtask's own tags) | None: Tags counts a task's whole time under the task's tags (1.3.3 rule) |
| 5 | Billable seconds per piece, without a zone | Share of time on the Billable basis, as 1.x opens Analysis | Share of time uses the Exact basis (`nodes/B2/api.md`) |
| 6 | Document or expose the "tick or it was a sleep" rule | More than 120 s without `observe_activity` turns running time into a pending sleep gap | The worker ticks `observe_activity(None)` with each 30 s heartbeat and each minute tick |
| 7 | Notices without building the whole snapshot | Crash-recovery and other notices are reachable only through `Service::snapshot()` | Milestone 1 shows no core notices |
| 8 | Deterministic ids for seeded or test data | `create_project`/`create_task` always draw a random UUID | The sample builds records directly with fixed ids |

## 1. A change event after every commit

PLAN.md section 5 and live-updates Change 3 ask for typed events naming what changed, after every write and every
sync merge. Today the only signal is `Service::revision()`, bumped by `commit` (both `mutate` and the crate-private
`mutate_system`, which sync merges use) and by `observe_system_zone`.

- Wanted: a public hook or a returned value, e.g. `Service::take_changes() -> Changes` listing the ids touched per
  section (projects, tasks, segments, gaps, pauses, tags, settings, prefs, lease), or a callback registered once.
- Today: `zwt_bridge::topics::diff` compares the previous and current `LocalFile` per section and turns the
  differences into topic bits. It costs one clone of the file per revision change and one pass over each section.
  It also has to reproduce some core rules to be precise (which segments count in a day, that an open segment of
  this device adds no closed time).
- A heartbeat changes `LocalFile::last_heartbeat` without a revision; that is right for views, and worth keeping.

## 2. A preference for the Share of time level

1.4.0 made the picked level sticky (PARITY 212): it stays until the user picks another. At `a88c1aa`,
`model::LocalPrefs` has no field for it (sync targets, theme, hide earnings, backups, setup, lease, epoch and
versions only), and the replicated settings allow-list (`machine::check_setting`: zones, work hours, reminders,
idle minutes, targets) has no key for it either.

- Wanted: `LocalPrefs::share_level: Option<String>` (device-local, like the theme), with a setter on `Service`.
- Today: `zwt_bridge::worker` keeps the level in memory (`Command::SetShareLevel`); it survives data changes and
  is lost at quit.

## 3. A public sample-data builder

`testkit` is `#[cfg(test)]`, so nothing outside the core can build a realistic store with it. 2.0 needs one for
milestone 1 (the app opens only sample data until the owner says otherwise), and later for screenshots,
measurements and UI tests.

- Wanted: a public, deterministic builder (`fn sample(now: Ts, zone: Tz) -> LocalFile`), or `testkit` behind a
  cargo feature.
- Today: `zwt_bridge::sample::build` builds the records field by field (3 projects, the starter tags from
  `Shared::seed_tags`, 10 tasks, 14 days of segments) and `store::seed_store` writes them. Building records by
  hand depends on the model's fields: any new required field in the core is a compile error here, which at least
  makes the drift visible.

## 4. Effective subtask tags (PARITY 221)

The 1.4.0 subtask-tags plan counts subtask time under the subtask's own tags (a subset of the task's, empty = No
tag). At `a88c1aa`, `subtasks::Item` has only `id`, `text` and `checked`, and `model::Task` has `tags`; there are
no per-subtask tags. The Tags level of Share of time counts each task's whole time under the task's own tags (the
1.3.3 rule). Milestone 5 needs the core to store subtask tags and expose effective tags per piece.

## 5. Billable seconds per piece, without a zone

1.x's Analysis opens on the Billable basis. In the core, Billable time is rounded per task and day in a reporting
zone (`billing::task_days`), so there is no billable figure per piece that a period without a zone can sum. The
Share of time port (`zwt-ui-logic`, node B2) uses the Exact basis (work and thinking pieces). Milestone 5 needs a
zone-free billable split per piece, or a `share_of_time` that takes the zone.

## 6. The activity tick and the sleep rule

`IdleTracker::tick` and `settle` take a gap of more than `idle::JUMP_SECS` (120 s) since the previous tick as a
sleep: while a task runs, that time becomes a pending `Sleep` gap and counts in nothing until the user confirms
it. The 1.x shell avoids false sleeps by calling `Service::observe_activity` every 5 s (`ACTIVITY_TICK`). Nothing
in the `Service` API says a caller must tick; a shell that only saves heartbeats loses every stretch over two
minutes.

- Wanted: a doc comment on `Service` (or `observe_activity`) stating the duty, or a `Service::tick(idle: Option<u64>)`
  that does the activity tick and the heartbeat together.
- Today: the worker calls `observe_activity(None)` then `heartbeat()` every 30 s while a task runs, and on each
  `MinuteTick` from the UI. `None` means "the OS can't say", which makes no idle gap; a real sleep still shows as a
  clock jump. Milestone 3 passes the OS idle seconds and lock state, as 1.x does. Test:
  `time_without_ticks_is_a_sleep_and_ticks_keep_it_counted` in `crates/zwt-bridge/tests/worker.rs`.

## 7. Notices without the whole snapshot

`Service` keeps notices (crash recovery at open, sync warnings) in a private field; the only way out is
`Service::snapshot().notices`, which builds the entire UI snapshot. A `Service::notices(&self) -> &[Notice]` would
let 2.0 show them without that cost. Milestone 1 shows none.

## 8. Deterministic ids for seeded data

`Shared::create_project` and `create_task` draw `Uuid::new_v4()`. A builder that wants the same store from the same
inputs (for golden files and screenshots) cannot use them. An id parameter, or a builder that takes ids, would let
sample data go through the core's validation (`clean_project`, `clean_task`) instead of around it.

## Behaviour checked against PARITY rows 81-93 (timer)

Checked through the worker and the core at `a88c1aa`; no difference found for the rows the bridge covers:

| Row | Core behaviour | Test |
|---|---|---|
| 81 | Start only from Draft, Pause only from Running, Resume only from Paused, Stop from Running or Paused (`IllegalTransition` otherwise) | `illegal_and_unknown_commands_come_back_as_errors` |
| 82 | Start and Resume open a segment, Pause and Stop close it | `start_pause_round_trip`, `topics.rs` |
| 86 | Cold-launch Resume is refused (`ColdStartRequired`) | `a_cold_resume_is_refused_with_an_error_and_the_worker_carries_on` |
| 89 | After a start in this launch, Resume works directly | same test |
| 91 | Starting or resuming another task pauses the running one (`Transition::auto_paused_task_*`, `PauseReason::AutoPaused`) | `switching_tasks_pauses_the_running_one_and_says_so` |
| 93 | Only Quit pauses: `Service::on_exit` closes this device's open segments (`PauseReason::AppQuit`) | `shutdown_pauses_what_runs_and_a_sent_command_fails_afterwards` |

Rows 86-88 also need the "continue as a new linked task" form (1.x opens it on a cold Resume); that is UI work.
