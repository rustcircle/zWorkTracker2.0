//! The task list's groups and the Stopped group's paging (owner, 2026-11-05: the task list stays as in 1.x, "the
//! running on top"). 1.x TaskRail.tsx lists Active (running and paused), then Drafts, then Stopped, and the Stopped
//! group 8 at a time (`STOPPED_PAGE`) until "Show all N stopped tasks", then all of them until "Show fewer". The order
//! inside each group is pinned in tests/order.rs; how the rail draws the groups, in zwt-app's tests/ui_rail_groups.rs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use std::ops::RangeInclusive;

use common::{NOW, ZONE, one, settle, spawn};
use z_work_core::machine::TaskInput;
use z_work_core::model::{LocalFile, TaskState};
use z_work_core::{ManualClock, Ts};
use zwt_bridge::sample::{self, STARTED_TASK};
use zwt_bridge::topics::Topics;
use zwt_bridge::views::{STOPPED_PAGE, StoppedPage, TaskGroup, TaskRow, task_list, task_rows};
use zwt_bridge::worker::{Command, Inbox, Mutation, Update, Worker};

/// The ids of the rows of `group`, in the list's order.
fn in_group(rows: &[TaskRow], group: TaskGroup) -> Vec<&str> {
    rows.iter()
        .filter(|r| r.group == group)
        .map(|r| r.id.as_str())
        .collect()
}

/// 1.x's three groups in 1.x's order: every Active row (running or paused), then every Draft, then every Stopped
/// one, each task in the group of its state; the running task heads the list.
#[test]
fn the_groups_come_in_the_1x_order_each_task_in_the_group_of_its_state() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (_worker, first, _inbox) = spawn(dir.path(), &clock);
    let rows = first.tasks.unwrap();
    assert_eq!(rows.len(), 10);
    let groups: Vec<TaskGroup> = rows.iter().map(|r| r.group).collect();
    let mut sorted = groups.clone();
    sorted.sort();
    assert_eq!(groups, sorted, "Active, then Drafts, then Stopped");
    assert_eq!(
        TaskGroup::ALL.map(TaskGroup::index),
        [0, 1, 2],
        "the UI's models, top first"
    );
    for r in &rows {
        let want = match r.state {
            TaskState::Running | TaskState::Paused => TaskGroup::Active,
            TaskState::Draft => TaskGroup::Drafts,
            TaskState::Stopped => TaskGroup::Stopped,
        };
        assert_eq!(r.group, want, "{}", r.id);
    }
    // The sample once open: one running and four paused, one draft left, four stopped.
    assert_eq!(
        in_group(&rows, TaskGroup::Active),
        [STARTED_TASK, "t-logo", "t-label", "t-blog", "t-guide"],
        "the running task, then by last run: today 10:25 and 07:55, 3 Nov, 2 Nov"
    );
    assert_eq!(in_group(&rows, TaskGroup::Drafts), ["t-a11y"]);
    assert_eq!(
        in_group(&rows, TaskGroup::Stopped),
        ["t-home", "t-box", "t-call", "t-plan"],
        "by last run: today 09:35 and 08:55, 3 Nov, 2 Nov"
    );
    let page = first.stopped_page.unwrap();
    assert_eq!(
        page,
        StoppedPage {
            total: 4,
            all: false
        }
    );
    assert!(!page.has_link(), "4 stopped tasks: one page, no link");
}

/// The sample file with `n` more stopped tasks, "Wrap-up 1" to "Wrap-up n", each run for the minute before it was
/// stopped, a minute after the one before: they last ran later than the sample's own stopped tasks, "Wrap-up n" last.
fn with_stopped(n: usize) -> LocalFile {
    let now = Ts::parse(NOW).unwrap();
    let mut file = sample::build(now, ZONE);
    let task = file.data.tasks["t-home"].clone();
    let segment = file.data.segments["seg-00-1"].clone();
    assert_eq!(segment.task_id, "t-home");
    for i in 1..=n {
        let id = format!("t-wrap-{i:02}");
        let at = now.later(i64::try_from(i).unwrap() * 60);
        let mut t = task.clone();
        t.id.clone_from(&id);
        t.name = format!("Wrap-up {i}");
        t.stopped_at = Some(at);
        file.data.tasks.insert(id.clone(), t);
        let mut s = segment.clone();
        s.id = format!("seg-wrap-{i:02}");
        s.task_id = id;
        s.start = at.later(-60);
        s.end = Some(at);
        s.subtask_id = None;
        file.data.segments.insert(s.id.clone(), s);
    }
    file
}

/// 1.x `STOPPED_PAGE`: the first 8 stopped tasks, the most recently run; the rest wait behind the link, which counts
/// every stopped task. Active and Drafts are never cut, and asking for all lists the whole of `task_rows`.
#[test]
fn the_stopped_group_lists_its_first_8_until_all_are_asked_for() {
    assert_eq!(STOPPED_PAGE, 8, "1.x TaskRail.tsx `STOPPED_PAGE = 8`");
    let file = with_stopped(8);
    let all = task_rows(&file, true);
    let stopped = in_group(&all, TaskGroup::Stopped);
    assert_eq!(
        stopped,
        [
            "t-wrap-08",
            "t-wrap-07",
            "t-wrap-06",
            "t-wrap-05",
            "t-wrap-04",
            "t-wrap-03",
            "t-wrap-02",
            "t-wrap-01",
            "t-home",
            "t-box",
            "t-call",
            "t-plan"
        ],
        "the most recently run first"
    );

    let page = task_list(&file, true, false);
    assert_eq!(
        page.stopped,
        StoppedPage {
            total: 12,
            all: false
        }
    );
    assert!(page.stopped.has_link());
    assert_eq!(in_group(&page.rows, TaskGroup::Stopped), &stopped[..8]);
    for group in [TaskGroup::Active, TaskGroup::Drafts] {
        assert_eq!(in_group(&page.rows, group), in_group(&all, group));
    }
    assert_eq!(page.rows.len(), all.len() - 4);
    assert_eq!(page.rows.as_slice(), &all[..all.len() - 4]);

    let every = task_list(&file, true, true);
    assert_eq!(
        every.stopped,
        StoppedPage {
            total: 12,
            all: true
        }
    );
    assert!(
        every.stopped.has_link(),
        "the link stays, to show fewer again"
    );
    assert_eq!(every.rows, all);
}

/// The link is there only with more stopped tasks than a page (1.x `stopped.length > STOPPED_PAGE`): 8 are a whole
/// page and no link; the 9th brings the link, and the least recently run stopped task goes behind it.
#[test]
fn the_link_comes_with_the_ninth_stopped_task() {
    let eight = task_list(&with_stopped(4), true, false);
    assert_eq!(
        eight.stopped,
        StoppedPage {
            total: 8,
            all: false
        }
    );
    assert!(!eight.stopped.has_link());
    assert_eq!(in_group(&eight.rows, TaskGroup::Stopped).len(), 8);

    let nine = task_list(&with_stopped(5), true, false);
    assert_eq!(nine.stopped.total, 9);
    assert!(nine.stopped.has_link());
    let shown = in_group(&nine.rows, TaskGroup::Stopped);
    assert_eq!(shown.len(), 8);
    assert!(!shown.contains(&"t-plan"), "{shown:?}");
}

/// Creates, runs and stops the tasks "Wrap-up i" for each `i` of `numbers` through the worker, a minute each, the last
/// one last. Returns the update of the last stop.
fn stop_new_tasks(
    worker: &Worker,
    inbox: &Inbox,
    clock: &ManualClock,
    numbers: RangeInclusive<usize>,
) -> Update {
    let mut last = Update::default();
    for i in numbers {
        let name = format!("Wrap-up {i}");
        let create = name.clone();
        worker
            .send(Command::Mutate(Mutation::new(
                "run a new task",
                move |svc| {
                    let id = svc.create_task(&TaskInput {
                        name: create,
                        project_id: "p-brand".to_owned(),
                        description: serde_json::Value::Null,
                        continued_from: None,
                        tags: Vec::new(),
                    })?;
                    svc.start_task(&id).map(|_| ())
                },
            )))
            .unwrap();
        one(worker, inbox);
        clock.advance(60);
        worker
            .send(Command::Mutate(Mutation::new("stop it", move |svc| {
                let id = svc
                    .file()
                    .data
                    .tasks
                    .values()
                    .find(|t| t.name == name)
                    .map(|t| t.id.clone())
                    .unwrap_or_default();
                svc.stop_task(&id)
            })))
            .unwrap();
        last = one(worker, inbox);
    }
    last
}

/// The link's command: "Show all N stopped tasks" lists every stopped task, "Show fewer" the first page again; only
/// the task list moves, and asking for the page already shown sends nothing.
#[test]
fn show_all_and_show_fewer_move_only_the_task_list() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, _first, inbox) = spawn(dir.path(), &clock);
    stop_new_tasks(&worker, &inbox, &clock, 1..=8);
    worker.send(Command::ShowAllStopped(false)).unwrap();
    assert!(
        settle(&worker, &inbox).is_empty(),
        "the first page is shown already"
    );

    worker.send(Command::ShowAllStopped(true)).unwrap();
    let shown = one(&worker, &inbox);
    assert_eq!(shown.topics, Topics::TASKS, "only the task list moves");
    let rows = shown.tasks.unwrap();
    let stopped = in_group(&rows, TaskGroup::Stopped);
    assert_eq!(stopped.len(), 12, "{stopped:?}");
    assert_eq!(&stopped[8..], ["t-home", "t-box", "t-call", "t-plan"]);
    assert_eq!(
        shown.stopped_page,
        Some(StoppedPage {
            total: 12,
            all: true
        })
    );
    worker.send(Command::ShowAllStopped(true)).unwrap();
    assert!(settle(&worker, &inbox).is_empty(), "all are shown already");

    worker.send(Command::ShowAllStopped(false)).unwrap();
    let fewer = one(&worker, &inbox);
    assert_eq!(fewer.topics, Topics::TASKS);
    assert_eq!(
        in_group(&fewer.tasks.unwrap(), TaskGroup::Stopped),
        &stopped[..8]
    );
    assert_eq!(
        fewer.stopped_page,
        Some(StoppedPage {
            total: 12,
            all: false
        })
    );
}

/// The page asked for stays while the stopped tasks drop to one page and grow again (1.x keeps `showAllStopped`): with
/// 8 left every one is listed and no link shows; with a 9th, all 9 are listed and the link reads "Show fewer".
#[test]
fn show_all_stays_while_the_stopped_tasks_drop_to_a_page_and_grow_again() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, _first, inbox) = spawn(dir.path(), &clock);
    stop_new_tasks(&worker, &inbox, &clock, 1..=8);
    worker.send(Command::ShowAllStopped(true)).unwrap();
    assert_eq!(
        one(&worker, &inbox).stopped_page,
        Some(StoppedPage {
            total: 12,
            all: true
        })
    );

    worker
        .send(Command::Mutate(Mutation::new(
            "delete four stopped tasks",
            |svc| {
                for id in ["t-home", "t-box", "t-call", "t-plan"] {
                    svc.delete_task(id)?;
                }
                Ok(())
            },
        )))
        .unwrap();
    let eight = one(&worker, &inbox);
    let page = eight.stopped_page.unwrap();
    assert_eq!(
        page,
        StoppedPage {
            total: 8,
            all: true
        },
        "still every one asked for"
    );
    assert!(!page.has_link(), "one page: no link");
    assert_eq!(in_group(&eight.tasks.unwrap(), TaskGroup::Stopped).len(), 8);

    let nine = stop_new_tasks(&worker, &inbox, &clock, 9..=9);
    let page = nine.stopped_page.unwrap();
    assert_eq!(
        page,
        StoppedPage {
            total: 9,
            all: true
        }
    );
    assert!(page.has_link(), "the link is back, reading \"Show fewer\"");
    assert_eq!(
        in_group(&nine.tasks.unwrap(), TaskGroup::Stopped).len(),
        9,
        "every stopped task listed, the 9th too"
    );
}
