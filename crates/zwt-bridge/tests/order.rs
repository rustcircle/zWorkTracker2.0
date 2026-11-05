//! The order inside each group of the task list. The list keeps 1.x's groups, Active (running and paused), Drafts,
//! then Stopped (TaskRail.tsx; owner, 2026-11-05: "activities/tasks is just the same, the running on top"; the
//! groups themselves are pinned in tests/groups.rs). Inside each group the owner's rule of 2026-11-04:
//! 1. the task running here is on top;
//! 2. then the tasks that have run, by when they last ran (the end of their latest segment, or now while one runs
//!    on another device), the most recent first and the least recent at the bottom of them;
//! 3. then the tasks that never ran, the newest created first;
//! 4. ties by creation time, the newest first, then by id.
//!
//! 1.x orders Active running first, then paused, each by its latest activity; Drafts newest created first; Stopped by
//! latest activity, a task with no time by its creation time among the others; ties in the snapshot's order
//! (TaskRail.tsx). These tests pin the places where the rule differs, each inside one group, and one order both give
//! that the flat list of 2026-11-04 broke (rule 2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use common::{NOW, ZONE, one, spawn};
use z_work_core::model::{LocalFile, RemoteDevice, Segment, SegmentEdit, Stamp, TaskState};
use z_work_core::{ManualClock, Ts};
use zwt_bridge::sample::{self, DEVICE_ID, STARTED_TASK};
use zwt_bridge::views::{TaskGroup, TaskRow, task_rows};
use zwt_bridge::worker::Command;

const OTHER_DEVICE: &str = "5a3b1e00-0000-4000-8000-0000000000ff";

fn now() -> Ts {
    Ts::parse(NOW).unwrap()
}

fn ids(rows: &[TaskRow]) -> Vec<&str> {
    rows.iter().map(|r| r.id.as_str()).collect()
}

/// The ids of the rows of `group`, in the list's order.
fn in_group(rows: &[TaskRow], group: TaskGroup) -> Vec<&str> {
    rows.iter()
        .filter(|r| r.group == group)
        .map(|r| r.id.as_str())
        .collect()
}

/// The sample as seeded: nothing open yet, `t-social` and `t-a11y` drafts that never ran.
fn sample_file() -> LocalFile {
    sample::build(now(), ZONE)
}

fn add_segment(
    file: &mut LocalFile,
    id: &str,
    task: &str,
    device: &str,
    start: Ts,
    end: Option<Ts>,
) {
    file.data.segments.insert(
        id.to_owned(),
        Segment {
            id: id.to_owned(),
            task_id: task.to_owned(),
            device_id: device.to_owned(),
            start,
            end,
            manual: false,
            subtask_id: None,
            edit: None,
            edit_stamp: Stamp {
                at: start,
                by: device.to_owned(),
            },
        },
    );
}

fn set_state(file: &mut LocalFile, task: &str, state: TaskState) {
    let t = file.data.tasks.get_mut(task).unwrap();
    t.state = state;
    t.pause_reason = None;
}

/// Voids every segment of `task`: none counts, so the task never ran.
fn void_all(file: &mut LocalFile, task: &str) {
    for seg in file
        .data
        .segments
        .values_mut()
        .filter(|s| s.task_id == task)
    {
        seg.edit = Some(SegmentEdit {
            start: seg.start,
            end: seg.end.unwrap(),
            voided: true,
            reason: "not this task".to_owned(),
            settled: false,
        });
    }
}

/// Rule 1: the task running here is on top of Active, and so of the whole list, even when a task running on another
/// computer was heard from later than this one started (1.x sorts every running task by its latest activity, so that
/// one came first).
#[test]
fn the_running_task_is_first() {
    let mut file = sample_file();
    // `t-social` runs here since 10:00; `t-blog` runs on another computer since 10:20, last heard from at 10:29.
    add_segment(
        &mut file,
        "seg-here",
        STARTED_TASK,
        DEVICE_ID,
        now().later(-1800),
        None,
    );
    set_state(&mut file, STARTED_TASK, TaskState::Running);
    add_segment(
        &mut file,
        "seg-there",
        "t-blog",
        OTHER_DEVICE,
        now().later(-600),
        None,
    );
    set_state(&mut file, "t-blog", TaskState::Running);
    file.remote_devices.insert(
        OTHER_DEVICE.to_owned(),
        RemoteDevice {
            name: "Other computer".to_owned(),
            last_heartbeat: Some(now().later(-60)),
            written_at: now().later(-60),
        },
    );

    let rows = task_rows(&file, true);
    let order = ids(&rows);
    assert_eq!(
        order[0], STARTED_TASK,
        "the task running here heads the list: {order:?}"
    );
    let active = in_group(&rows, TaskGroup::Active);
    assert_eq!(
        active[0], STARTED_TASK,
        "the task running here first: {active:?}"
    );
    assert_eq!(
        active[1], "t-blog",
        "a task running elsewhere runs now: it ran most recently of the rest: {active:?}"
    );
    assert_eq!(
        &active[2..],
        ["t-logo", "t-label", "t-guide"],
        "then the paused tasks by when they last ran: {active:?}"
    );
}

/// Rule 2, a regression test rather than a difference (1.x orders these the same): a task that ran and was stopped
/// moves to the top of the Stopped group, above the stopped tasks that ran before it, and not above the paused ones as
/// in the flat list of 2026-11-04; the task it had paused heads Active while nothing runs, and goes under the next task
/// that runs.
#[test]
fn a_task_run_and_stopped_moves_to_the_top_of_its_group() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, first, inbox) = spawn(dir.path(), &clock);
    let rows = first.tasks.unwrap();
    let active = in_group(&rows, TaskGroup::Active);
    let guide = active.iter().position(|id| *id == "t-guide").unwrap();
    assert!(guide > 1, "t-guide last ran days ago: {active:?}");

    // Run the older task (pausing the one that runs), then stop it.
    clock.advance(60);
    worker.send(Command::Resume("t-guide".to_owned())).unwrap();
    one(&worker, &inbox);
    clock.advance(300);
    worker.send(Command::Stop("t-guide".to_owned())).unwrap();
    let stopped = one(&worker, &inbox).tasks.unwrap();
    assert!(
        stopped.iter().all(|r| r.state != TaskState::Running),
        "nothing runs"
    );
    let group = in_group(&stopped, TaskGroup::Stopped);
    assert_eq!(
        group,
        ["t-guide", "t-home", "t-box", "t-call", "t-plan"],
        "stopped last, so it ran most recently of the stopped tasks: {group:?}"
    );
    let active = in_group(&stopped, TaskGroup::Active);
    assert_eq!(
        active[0], STARTED_TASK,
        "then the task it paused, the most recent of the rest: {active:?}"
    );

    // Something runs again: it goes on top, the task paused before right under it; the stopped task stays first of
    // its group.
    clock.advance(60);
    worker.send(Command::Resume("t-logo".to_owned())).unwrap();
    let resumed = one(&worker, &inbox).tasks.unwrap();
    let active = in_group(&resumed, TaskGroup::Active);
    assert_eq!(&active[..2], ["t-logo", STARTED_TASK], "{active:?}");
    assert_eq!(
        ids(&resumed)[0],
        "t-logo",
        "the running task heads the list"
    );
    assert_eq!(
        in_group(&resumed, TaskGroup::Stopped)[0],
        "t-guide",
        "position 0 of the stopped tasks: {:?}",
        ids(&resumed)
    );
}

/// Rule 3: inside a group, the tasks that never ran are below every task that ran, the newest created first. In
/// Stopped, 1.x sorts a task with no time by when it was made among the others' latest activity, so two stopped tasks
/// made at 10:28 and 10:29 whose time was all voided came above every stopped task that ran.
#[test]
fn tasks_never_run_are_at_the_bottom_of_their_group_newest_first() {
    let mut file = sample_file();
    // Both drafts never ran; `t-social` is made the newer of the two.
    file.data.tasks.get_mut(STARTED_TASK).unwrap().created_at = now().later(-3600);
    // `t-plan` and `t-call` (stopped): every segment voided; made at 10:29 and 10:28, later than any task last ran.
    void_all(&mut file, "t-plan");
    void_all(&mut file, "t-call");
    file.data.tasks.get_mut("t-plan").unwrap().created_at = now().later(-60);
    file.data.tasks.get_mut("t-call").unwrap().created_at = now().later(-120);
    let rows = task_rows(&file, true);
    assert_eq!(rows.len(), 10);
    assert_eq!(
        in_group(&rows, TaskGroup::Drafts),
        [STARTED_TASK, "t-a11y"],
        "never run, the newest first: {:?}",
        ids(&rows)
    );
    assert_eq!(
        in_group(&rows, TaskGroup::Stopped),
        ["t-home", "t-box", "t-plan", "t-call"],
        "the stopped tasks that ran (today 09:35 and 08:55), then the two that never ran, the newest made first: {:?}",
        ids(&rows)
    );

    // A draft with time logged by hand has run: it leaves the bottom of the Drafts for its place by that time, the
    // top; it stays a draft, under Drafts.
    add_segment(
        &mut file,
        "seg-manual",
        "t-a11y",
        DEVICE_ID,
        now().later(-120),
        Some(now().later(-60)),
    );
    file.data.segments.get_mut("seg-manual").unwrap().manual = true;
    let rows = task_rows(&file, true);
    assert_eq!(
        in_group(&rows, TaskGroup::Drafts),
        ["t-a11y", STARTED_TASK],
        "the draft that ran first, the only one that never ran last: {:?}",
        ids(&rows)
    );
    assert!(
        !in_group(&rows, TaskGroup::Active).contains(&"t-a11y"),
        "the latest time of all does not take a draft out of its group"
    );
}

/// Rules 2 and 3 read a segment as it counts: a correction's end, not the raw one; a voided segment not at all. A
/// stopped task corrected to end later than another stopped task's last run comes above it, a voided later segment
/// does not lift a paused task above another, and a paused task whose only time was voided never ran: the bottom of
/// Active.
#[test]
fn corrections_count_and_voided_segments_do_not() {
    let mut file = sample_file();
    // The sample's latest end is 10:25. `t-home` (stopped): a segment that ended at 10:26, corrected to end at 10:28;
    // `t-box` (stopped) last ran until 10:27.
    add_segment(
        &mut file,
        "seg-fixed",
        "t-home",
        DEVICE_ID,
        now().later(-600),
        Some(now().later(-240)),
    );
    file.data.segments.get_mut("seg-fixed").unwrap().edit = Some(SegmentEdit {
        start: now().later(-600),
        end: now().later(-120),
        voided: false,
        reason: "forgot to stop".to_owned(),
        settled: false,
    });
    add_segment(
        &mut file,
        "seg-box",
        "t-box",
        DEVICE_ID,
        now().later(-500),
        Some(now().later(-180)),
    );
    // `t-logo` (paused): its own 10:27 segment, and a voided one that ended at 10:29; `t-blog` (paused) last ran until
    // 10:28.
    add_segment(
        &mut file,
        "seg-logo",
        "t-logo",
        DEVICE_ID,
        now().later(-500),
        Some(now().later(-180)),
    );
    add_segment(
        &mut file,
        "seg-gone",
        "t-logo",
        DEVICE_ID,
        now().later(-100),
        Some(now().later(-60)),
    );
    file.data.segments.get_mut("seg-gone").unwrap().edit = Some(SegmentEdit {
        start: now().later(-100),
        end: now().later(-60),
        voided: true,
        reason: "not worked".to_owned(),
        settled: false,
    });
    add_segment(
        &mut file,
        "seg-blog",
        "t-blog",
        DEVICE_ID,
        now().later(-400),
        Some(now().later(-120)),
    );
    // `t-label` (paused): every segment voided.
    void_all(&mut file, "t-label");

    let rows = task_rows(&file, true);
    let stopped = in_group(&rows, TaskGroup::Stopped);
    assert_eq!(
        &stopped[..2],
        ["t-home", "t-box"],
        "the corrected end (10:28) above 10:27, though the raw end (10:26) is below it: {stopped:?}"
    );
    let active = in_group(&rows, TaskGroup::Active);
    assert_eq!(
        &active[..2],
        ["t-blog", "t-logo"],
        "10:28 above 10:27; the voided 10:29 does not count: {active:?}"
    );
    assert_eq!(
        active.last(),
        Some(&"t-label"),
        "only voided time: never ran, under every task of its group that ran: {active:?}"
    );
}

/// Where `id` is in `order`.
fn place(order: &[&str], id: &str) -> usize {
    order.iter().position(|o| *o == id).unwrap()
}

/// Rule 4: tasks of one group that last ran at the same second are ordered by creation time, the newest first, then
/// by id; the order does not depend on where their segments sit in the store (1.x keeps such ties in the snapshot's
/// order, which is the order of the ids).
#[test]
fn ties_go_by_creation_time_then_id_and_stay_put() {
    let mut file = sample_file();
    // `t-home` and `t-box` (both stopped) both last ran until 10:29, later than any other task: `t-home` is the newer
    // task, and its id is the later one.
    let end = now().later(-60);
    add_segment(
        &mut file,
        "seg-tie-a",
        "t-home",
        DEVICE_ID,
        now().later(-900),
        Some(end),
    );
    add_segment(
        &mut file,
        "seg-tie-b",
        "t-box",
        DEVICE_ID,
        now().later(-500),
        Some(end),
    );
    file.data.tasks.get_mut("t-home").unwrap().created_at = now().later(-10 * 86_400);
    let rows = task_rows(&file, true);
    let stopped = in_group(&rows, TaskGroup::Stopped);
    assert_eq!(
        (place(&stopped, "t-home"), place(&stopped, "t-box")),
        (0, 1),
        "same last run: the newer task first, side by side, on top of their group: {stopped:?}"
    );

    // Same last run and the same creation time: by id.
    file.data.tasks.get_mut("t-home").unwrap().created_at = file.data.tasks["t-box"].created_at;
    let rows = task_rows(&file, true);
    let stopped = in_group(&rows, TaskGroup::Stopped);
    assert_eq!(
        (place(&stopped, "t-box"), place(&stopped, "t-home")),
        (0, 1),
        "then by id: {stopped:?}"
    );
    let order: Vec<String> = ids(&rows).into_iter().map(str::to_owned).collect();

    // The same data with the two segments stored under each other's ids: the same order.
    let mut swapped = file.clone();
    let a = swapped.data.segments.remove("seg-tie-a").unwrap();
    let b = swapped.data.segments.remove("seg-tie-b").unwrap();
    swapped.data.segments.insert(
        "seg-tie-a".to_owned(),
        Segment {
            id: "seg-tie-a".to_owned(),
            ..b
        },
    );
    swapped.data.segments.insert(
        "seg-tie-b".to_owned(),
        Segment {
            id: "seg-tie-b".to_owned(),
            ..a
        },
    );
    assert_eq!(
        ids(&task_rows(&swapped, true)),
        order,
        "stable whatever the store's order"
    );
}
