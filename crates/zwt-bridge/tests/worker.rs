//! The worker end to end on a seeded temp store, and the hub applying its updates.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::{NOW, date, minutes, one, settle, spawn, today};
use z_work_core::machine::ProjectInput;
use z_work_core::model::TaskState;
use z_work_core::{Clock, ManualClock};
use zwt_bridge::hub::{Hub, Recorder};
use zwt_bridge::sample::STARTED_TASK;
use zwt_bridge::share::ShareLevel;
use zwt_bridge::topics::Topics;
use zwt_bridge::views::{CHIP_DAYS, TaskGroup, TaskRow};
use zwt_bridge::worker::{
    Command, CommandKind, ErrorKind, Inbox, Mutation, Notice, Update, Worker, WorkerConfig,
};

const T: Topics = Topics::TASKS;
const R: Topics = Topics::RUNNING;
const D: Topics = Topics::DAY;
const C: Topics = Topics::CHIPS;
const S: Topics = Topics::SHARE;

fn at(iso: &str) -> i64 {
    z_work_core::Ts::parse(iso).unwrap().secs()
}

fn row<'a>(rows: &'a [TaskRow], id: &str) -> &'a TaskRow {
    rows.iter().find(|r| r.id == id).unwrap()
}

#[test]
fn the_first_update_carries_every_view() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (_worker, first, _inbox) = spawn(dir.path(), &clock);
    assert_eq!(first.topics, Topics::VIEWS);
    let rows = first.tasks.unwrap();
    assert_eq!(rows.len(), 10);
    assert_eq!(rows[0].id, STARTED_TASK, "running first");
    // 1.x's Active group (running or paused), the first group of the list.
    assert!(matches!(
        rows[0].state,
        TaskState::Running | TaskState::Paused
    ));
    assert_eq!(rows[0].group, TaskGroup::Active);
    assert!(
        first.stopped_page.is_some(),
        "TASKS: rows and paging together"
    );
    assert_eq!(rows[0].open_since, Some(at(NOW)));
    assert_eq!(
        rows.iter()
            .filter(|r| r.state == TaskState::Running)
            .count(),
        1
    );
    let running = first.running.unwrap();
    assert_eq!(running.task_id.as_deref(), Some(STARTED_TASK));
    assert_eq!(running.task_name, "Social templates");
    assert_eq!(running.started_at, Some(at(NOW)));
    let day = first.day.unwrap();
    assert_eq!(day.head.date, today());
    assert_eq!(day.head.label, "Today, Wed 4 Nov 2026");
    assert!(
        day.pieces.iter().any(|p| p.end.is_none()),
        "the running piece is open"
    );
    assert!(day.pieces.len() >= 4);
    let chips = first.chips.unwrap();
    assert_eq!(chips.len(), usize::try_from(CHIP_DAYS).unwrap());
    // The past to the future (owner, 2026-11-04): the oldest day first, today last.
    assert_eq!(chips[0].date, date(2026, 10, 22));
    assert_eq!(chips[chips.len() - 1].date, today());
    let share = first.share.unwrap();
    assert_eq!(share.head.picked, None);
    assert_eq!(share.head.shown, ShareLevel::Projects);
    assert_eq!(share.head.period.label, "2 Nov – 8 Nov 2026");
    assert_eq!(share.rows.len(), 3, "one row per project");
}

#[test]
fn start_pause_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, _, inbox) = spawn(dir.path(), &clock);
    let ticks = minutes(&worker, &inbox, &clock, 10);
    assert_eq!(ticks.len(), 10, "each minute moved the share");
    assert!(ticks.iter().all(|u| u.topics == S));
    clock.advance(30);
    worker
        .send(Command::Pause(STARTED_TASK.to_owned()))
        .unwrap();
    let paused = one(&worker, &inbox);
    assert_eq!(paused.topics, T | R | D | C | S);
    assert_eq!(paused.error, None);
    let r = paused.tasks.as_ref().unwrap();
    let social = row(r, STARTED_TASK);
    assert_eq!(
        (social.state, social.closed_secs, social.open_since),
        (TaskState::Paused, 630, None)
    );
    assert_eq!(social.note, "Paused by you.");
    assert_eq!(social.logged_text, "10m 30s logged");
    assert!(!paused.running.unwrap().is_running());
    clock.advance(60);
    worker
        .send(Command::Resume(STARTED_TASK.to_owned()))
        .unwrap();
    let resumed = one(&worker, &inbox);
    // A segment opened this second adds no time yet: chips and share stay as they are.
    assert_eq!(resumed.topics, T | R | D);
    let social = resumed
        .tasks
        .unwrap()
        .into_iter()
        .find(|r| r.id == STARTED_TASK)
        .unwrap();
    assert_eq!(social.state, TaskState::Running);
    assert_eq!(social.closed_secs, 630);
    assert_eq!(social.open_since, Some(at(NOW) + 690));
    assert_eq!(resumed.running.unwrap().started_at, Some(at(NOW) + 690));
}

#[test]
fn switching_tasks_pauses_the_running_one_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, _, inbox) = spawn(dir.path(), &clock);
    minutes(&worker, &inbox, &clock, 5);
    clock.advance(15);
    worker.send(Command::Start("t-a11y".to_owned())).unwrap();
    let u = one(&worker, &inbox);
    assert_eq!(u.topics, T | R | D | C | S);
    let notice = u.notice.unwrap();
    assert_eq!(
        notice,
        Notice::AutoPaused {
            task_id: STARTED_TASK.to_owned(),
            task_name: "Social templates".to_owned(),
            by_task_id: "t-a11y".to_owned(),
            by_task_name: "Accessibility fixes".to_owned(),
        }
    );
    assert_eq!(
        notice.text(),
        "Paused \"Social templates\" because you started \"Accessibility fixes\". Only one task is timed at a time."
    );
    let rows = u.tasks.unwrap();
    assert_eq!(
        row(&rows, STARTED_TASK).note,
        "Auto-paused because \"Accessibility fixes\" was started."
    );
    assert_eq!(row(&rows, "t-a11y").state, TaskState::Running);
    assert_eq!(
        rows.iter()
            .filter(|r| r.state == TaskState::Running)
            .count(),
        1
    );
    assert_eq!(u.running.unwrap().task_id.as_deref(), Some("t-a11y"));
}

#[test]
fn a_cold_resume_is_refused_with_an_error_and_the_worker_carries_on() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, _, inbox) = spawn(dir.path(), &clock);
    minutes(&worker, &inbox, &clock, 15);
    worker.shutdown().unwrap();
    clock.advance(3600);
    // The next launch opens the store as it is: nothing runs, and it is cold.
    let (worker, first, inbox) = spawn(dir.path(), &clock);
    assert!(!first.running.unwrap().is_running());
    let rows = first.tasks.unwrap();
    let social = row(&rows, STARTED_TASK);
    assert_eq!(social.state, TaskState::Paused);
    assert_eq!(social.closed_secs, 900, "Quit paused it then");
    assert_eq!(
        social.note,
        "Paused when the app was closed. Time stopped at that moment."
    );
    assert!(
        social
            .cold_note
            .starts_with("After a restart, Resume asks for every field again")
    );
    worker
        .send(Command::Resume(STARTED_TASK.to_owned()))
        .unwrap();
    let refused = one(&worker, &inbox);
    let e = refused.error.unwrap();
    assert_eq!(e.kind, ErrorKind::ColdStartRequired);
    assert_eq!(e.command, CommandKind::Resume);
    assert_eq!(e.task_id.as_deref(), Some(STARTED_TASK));
    assert_eq!(refused.topics, Topics::NONE, "nothing changed");
    // A start with every field filled works, warms the launch, and then Resume works directly.
    worker.send(Command::Start("t-a11y".to_owned())).unwrap();
    let started = one(&worker, &inbox);
    assert_eq!(started.error, None);
    let rows = started.tasks.unwrap();
    assert!(row(&rows, STARTED_TASK).cold_note.is_empty(), "warm now");
    worker
        .send(Command::Resume(STARTED_TASK.to_owned()))
        .unwrap();
    let resumed = one(&worker, &inbox);
    assert_eq!(resumed.error, None);
    assert!(matches!(resumed.notice, Some(Notice::AutoPaused { .. })));
}

#[test]
fn illegal_and_unknown_commands_come_back_as_errors() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, _, inbox) = spawn(dir.path(), &clock);
    worker.send(Command::Pause("t-a11y".to_owned())).unwrap();
    worker
        .send(Command::Start("no-such-task".to_owned()))
        .unwrap();
    worker.send(Command::Stop("t-home".to_owned())).unwrap();
    let errors: Vec<_> = settle(&worker, &inbox)
        .into_iter()
        .map(|u| {
            assert_eq!(u.topics, Topics::NONE);
            let e = u.error.unwrap();
            (e.command, e.kind)
        })
        .collect();
    assert_eq!(
        errors,
        vec![
            (CommandKind::Pause, ErrorKind::IllegalTransition),
            (CommandKind::Start, ErrorKind::NotFound),
            (CommandKind::Stop, ErrorKind::IllegalTransition),
        ]
    );
    assert_eq!(worker.stats().commands, 3);
}

#[test]
fn select_day_moves_only_the_day() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, _, inbox) = spawn(dir.path(), &clock);
    worker.send(Command::SelectDay(date(2026, 11, 3))).unwrap();
    let u = one(&worker, &inbox);
    assert_eq!(u.topics, D);
    let day = u.day.unwrap();
    assert_eq!(day.head.date, date(2026, 11, 3));
    assert_eq!(day.head.label, "Yesterday, Tue 3 Nov 2026");
    assert!(day.pieces.iter().all(|p| p.end.is_some()));
    worker.send(Command::SelectDay(date(2026, 11, 3))).unwrap();
    assert!(
        settle(&worker, &inbox).is_empty(),
        "the same day again changes nothing"
    );
    worker.send(Command::SelectDay(today())).unwrap();
    assert_eq!(one(&worker, &inbox).day.unwrap().head.date, today());
}

#[test]
fn the_share_level_moves_only_the_share_and_tags_survive_data_changes() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, _, inbox) = spawn(dir.path(), &clock);
    worker
        .send(Command::SetShareLevel(ShareLevel::Tags))
        .unwrap();
    let u = one(&worker, &inbox);
    assert_eq!(u.topics, S);
    let share = u.share.unwrap();
    assert_eq!(
        (share.head.picked, share.head.shown),
        (Some(ShareLevel::Tags), ShareLevel::Tags)
    );
    assert!(
        share.rows.iter().all(|r| r.key.starts_with("tags:")),
        "{:?}",
        share.rows
    );
    minutes(&worker, &inbox, &clock, 10);
    clock.advance(20);
    worker
        .send(Command::Pause(STARTED_TASK.to_owned()))
        .unwrap();
    let paused = one(&worker, &inbox);
    let head = paused.share.unwrap().head;
    assert_eq!(
        (head.picked, head.shown),
        (Some(ShareLevel::Tags), ShareLevel::Tags)
    );
    clock.advance(60);
    worker.send(Command::Start("t-a11y".to_owned())).unwrap();
    let ticks = minutes(&worker, &inbox, &clock, 10);
    assert!(
        ticks
            .iter()
            .filter_map(|u| u.share.as_ref())
            .all(|s| s.head.shown == ShareLevel::Tags)
    );
    clock.advance(20);
    worker.send(Command::Stop("t-a11y".to_owned())).unwrap();
    let stopped = one(&worker, &inbox);
    let head = stopped.share.unwrap().head;
    assert_eq!(
        (head.picked, head.shown),
        (Some(ShareLevel::Tags), ShareLevel::Tags)
    );
}

#[test]
fn a_minute_tick_recomputes_the_share_only_and_only_when_it_shows_something_new() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, first, inbox) = spawn(dir.path(), &clock);
    let mut hub = Hub::new();
    let mut ui = Recorder::default();
    hub.apply(first, &mut ui.sinks());
    let before = ui.clone();
    // Running: a minute later the running task's share grew.
    clock.advance(60);
    worker.send(Command::MinuteTick).unwrap();
    let u = one(&worker, &inbox);
    assert_eq!(u.topics, S);
    let applied = hub.apply(u, &mut ui.sinks());
    assert!(applied.share_rows.total() > 0);
    // Within the same second, a tick shows nothing new: no update at all.
    worker.send(Command::MinuteTick).unwrap();
    assert!(settle(&worker, &inbox).is_empty());
    // Nothing running: ticks never produce an update, however much time passes.
    worker
        .send(Command::Pause(STARTED_TASK.to_owned()))
        .unwrap();
    hub.apply(one(&worker, &inbox), &mut ui.sinks());
    let writes_after_pause = (ui.tasks.writes, ui.day_pieces.writes);
    for _ in 0..3 {
        clock.advance(60);
        worker.send(Command::MinuteTick).unwrap();
    }
    assert!(settle(&worker, &inbox).is_empty());
    // The ticks themselves wrote nothing to the task list or the day axis.
    assert_eq!((ui.tasks.writes, ui.day_pieces.writes), writes_after_pause);
    assert_ne!(
        ui.tasks.writes, before.tasks.writes,
        "the pause did write the task list"
    );
}

#[test]
fn the_same_update_applied_twice_writes_nothing_the_second_time() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (_worker, first, _inbox) = spawn(dir.path(), &clock);
    let mut hub = Hub::new();
    let mut ui = Recorder::default();
    let once = hub.apply(first.clone(), &mut ui.sinks());
    assert_eq!(once.tasks.inserted, 10);
    assert_eq!((once.running, once.day_head, once.share_head), (1, 1, 1));
    let twice = hub.apply(first, &mut ui.sinks());
    assert_eq!(twice.total(), 0, "{twice:?}");
    assert_eq!(ui.running.values.len(), 1);
    assert_eq!(hub.tasks(), ui.tasks.rows.as_slice());
}

#[test]
fn a_project_rename_updates_the_task_list_and_the_share_but_not_the_day_axis() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, first, inbox) = spawn(dir.path(), &clock);
    let mut hub = Hub::new();
    let mut ui = Recorder::default();
    hub.apply(first, &mut ui.sinks());
    let rename = Mutation::new("rename project", |svc| {
        let p = svc.file().data.projects["p-brand"].clone();
        let input = ProjectInput {
            name: "Brand 2027".to_owned(),
            color: p.color,
            billable: p.billable,
            budget_hours: p.budget_hours,
            client_zone: p.client_zone,
            billing_type: p.billing_type,
            fixed_fee_cents: p.fixed_fee_cents,
            fee_currency: p.fee_currency,
        };
        svc.update_project("p-brand", &input)
    });
    worker.send(Command::Mutate(rename)).unwrap();
    let u = one(&worker, &inbox);
    assert_eq!(u.topics, Topics::PROJECTS | T | S);
    let applied = hub.apply(u, &mut ui.sinks());
    assert_eq!(
        applied.tasks.set, 3,
        "the three Brand refresh tasks, rewritten in place"
    );
    assert_eq!(applied.tasks.inserted + applied.tasks.removed, 0);
    assert_eq!(
        applied.day_pieces.total() + applied.chips.total() + applied.running,
        0
    );
    assert_eq!(applied.share_rows.set, 1, "the project's row");
}

/// Review R3: a task's name labels its blocks on the day axis, so a rename rewrites those blocks, in place, and no
/// other row of the axis; a renamed task with no block on the day shown moves nothing there.
#[test]
fn a_task_rename_rewrites_only_its_blocks_on_the_day_axis() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, first, inbox) = spawn(dir.path(), &clock);
    let mut hub = Hub::new();
    let mut ui = Recorder::default();
    hub.apply(first, &mut ui.sinks());
    let rename = |id: &'static str, name: &'static str| {
        Mutation::new("rename a task", move |svc| {
            let task = svc.file().data.tasks[id].clone();
            let input = z_work_core::machine::TaskInput {
                name: name.to_owned(),
                project_id: task.project_id,
                description: task.description,
                continued_from: None,
                tags: task.tags,
            };
            svc.update_task(id, &input)
        })
    };
    let blocks = |hub: &Hub, id: &str| hub.day_pieces().iter().filter(|p| p.task_id == id).count();
    // `t-logo` has one block today (its name on it is checked on screen: tests/ui_review.rs, zwt-app).
    assert_eq!(blocks(&hub, "t-logo"), 1);

    worker
        .send(Command::Mutate(rename("t-logo", "Logo, round two")))
        .unwrap();
    let u = one(&worker, &inbox);
    // The Share of time shows projects: a task's name is not on it, so its rows came out the same and were dropped.
    assert_eq!(u.topics, T | D, "{:?}", u.topics);
    let applied = hub.apply(u, &mut ui.sinks());
    assert_eq!(
        hub.day_pieces()
            .iter()
            .filter(|p| p.task_id == "t-logo")
            .count(),
        1
    );
    assert_eq!(
        (
            applied.day_pieces.set,
            applied.day_pieces.inserted,
            applied.day_pieces.removed
        ),
        (1, 0, 0),
        "its one block, rewritten in place"
    );
    assert_eq!(
        ui.day_pieces.rows,
        hub.day_pieces(),
        "the sink holds it too"
    );

    // `t-plan` has no block today: the day is recomputed and found the same, so nothing reaches the axis.
    assert_eq!(blocks(&hub, "t-plan"), 0);
    worker
        .send(Command::Mutate(rename("t-plan", "Planning")))
        .unwrap();
    let u = one(&worker, &inbox);
    assert!(u.day.is_none(), "no block of it today: {:?}", u.topics);
    let applied = hub.apply(u, &mut ui.sinks());
    assert_eq!(applied.day_pieces.total(), 0);
}

#[test]
fn an_idle_worker_does_no_work() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, _, inbox) = spawn(dir.path(), &clock);
    worker
        .send(Command::Pause(STARTED_TASK.to_owned()))
        .unwrap();
    settle(&worker, &inbox);
    let before = worker.stats();
    std::thread::sleep(Duration::from_millis(300));
    let after = worker.stats();
    assert_eq!(
        after, before,
        "no wakeup, no heartbeat, no update in 300 ms"
    );
}

#[test]
fn heartbeats_follow_their_deadline_and_send_no_update() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let inbox = Inbox::new();
    let mut config = common::config(dir.path(), &clock);
    config.heartbeat = Duration::from_millis(40);
    let (worker, _) = Worker::spawn(config, inbox.notify()).unwrap();
    clock.advance(5);
    let start = Instant::now();
    while worker.stats().heartbeats < 3 {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            worker.stats()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let stats = worker.stats();
    assert_eq!(stats.updates, 0, "a heartbeat changes no view");
    assert!(stats.wakeups >= stats.heartbeats);
    let (tx, rx) = mpsc::channel();
    worker
        .send(Command::Mutate(Mutation::new(
            "read the beat",
            move |svc| {
                tx.send(svc.file().last_heartbeat).unwrap();
                Ok(())
            },
        )))
        .unwrap();
    worker.flush().unwrap();
    assert_eq!(
        rx.recv().unwrap(),
        Some(clock.now()),
        "the beat was saved at the clock's time"
    );
    assert!(inbox.take().is_empty(), "reading changed nothing");
}

#[test]
fn midnight_rolls_the_day_over_on_its_own_deadline() {
    let dir = tempfile::tempdir().unwrap();
    // One second before midnight in Lisbon.
    let clock = ManualClock::at("2026-11-04T23:59:59Z");
    let (worker, first, inbox) = spawn(dir.path(), &clock);
    assert_eq!(first.day.unwrap().head.date, today());
    clock.set("2026-11-05T00:00:01Z");
    // No command: the worker wakes at its midnight deadline (one real second away) and catches up.
    let start = Instant::now();
    let rolled = loop {
        let mut got = inbox.take();
        if let Some(u) = got.pop() {
            break u;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            worker.stats()
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(rolled.topics.contains(D | C), "{:?}", rolled.topics);
    assert!(
        !rolled.topics.contains(T),
        "the task list does not depend on the date"
    );
    let day = rolled.day.unwrap();
    assert_eq!(day.head.date, date(2026, 11, 5));
    assert_eq!(day.head.label, "Today, Thu 5 Nov 2026");
    assert!(
        day.pieces.iter().any(|p| p.end.is_none()),
        "the running piece crosses midnight"
    );
    let chips = rolled.chips.unwrap();
    assert_eq!(chips[0].date, date(2026, 10, 23), "the oldest day first");
    assert_eq!(
        chips[chips.len() - 1].date,
        date(2026, 11, 5),
        "the new today last"
    );
}

#[test]
fn shutdown_pauses_what_runs_and_a_sent_command_fails_afterwards() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, _, _) = spawn(dir.path(), &clock);
    let commands = worker.commands();
    worker.shutdown().unwrap();
    assert!(commands.send(Command::MinuteTick).is_err());
    let reopened = zwt_bridge::sample::open_or_seed(
        dir.path(),
        std::sync::Arc::new(clock.clone()),
        common::ZONE_NAME,
    )
    .unwrap();
    assert!(!reopened.seeded);
    let tasks = &reopened.service.file().data.tasks;
    assert!(tasks.values().all(|t| t.state != TaskState::Running));
}

#[test]
fn a_bad_folder_is_a_start_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-folder");
    std::fs::write(&file, b"x").unwrap();
    let clock = ManualClock::at(NOW);
    let config = WorkerConfig {
        dir: file,
        ..common::config(dir.path(), &clock)
    };
    let err = Worker::spawn(config, Inbox::new().notify()).unwrap_err();
    assert!(
        err.to_string().starts_with("could not open the data in"),
        "{err}"
    );
}

/// The update type is plain data: it crosses threads (the UI forwards it to its event loop).
#[test]
fn updates_are_send() {
    fn is_send<T: Send + 'static>() {}
    is_send::<Update>();
    is_send::<Command>();
}

/// The core counts more than two minutes without an activity tick as a sleep: that time counts in nothing until
/// the user confirms it. The worker's ticks (heartbeats, minute ticks) are what keep a long run counted.
#[test]
fn time_without_ticks_is_a_sleep_and_ticks_keep_it_counted() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, _, inbox) = spawn(dir.path(), &clock);
    minutes(&worker, &inbox, &clock, 10);
    worker
        .send(Command::Pause(STARTED_TASK.to_owned()))
        .unwrap();
    let ticked = one(&worker, &inbox);
    assert_eq!(
        row(ticked.tasks.as_ref().unwrap(), STARTED_TASK).closed_secs,
        600
    );
    clock.advance(60);
    worker
        .send(Command::Resume(STARTED_TASK.to_owned()))
        .unwrap();
    settle(&worker, &inbox);
    clock.advance(600);
    worker
        .send(Command::Pause(STARTED_TASK.to_owned()))
        .unwrap();
    let slept = one(&worker, &inbox);
    let social = row(slept.tasks.as_ref().unwrap(), STARTED_TASK);
    assert_eq!(
        social.closed_secs, 600,
        "the ten minutes without a tick wait for confirmation"
    );
    let (tx, rx) = mpsc::channel();
    worker
        .send(Command::Mutate(Mutation::new("count gaps", move |svc| {
            tx.send(svc.file().data.gaps.len()).unwrap();
            Ok(())
        })))
        .unwrap();
    worker.flush().unwrap();
    assert_eq!(rx.recv().unwrap(), 1, "one pending sleep gap");
}

#[test]
fn the_running_value_follows_a_rename_and_a_sleep() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, _, inbox) = spawn(dir.path(), &clock);
    let rename = Mutation::new("rename the running task", |svc| {
        let task = svc.file().data.tasks[STARTED_TASK].clone();
        let input = z_work_core::machine::TaskInput {
            name: "Social templates, v2".to_owned(),
            project_id: task.project_id,
            description: task.description,
            continued_from: None,
            tags: task.tags,
        };
        svc.update_task(STARTED_TASK, &input)
    });
    worker.send(Command::Mutate(rename)).unwrap();
    let renamed = one(&worker, &inbox);
    assert!(renamed.topics.contains(T | R), "{:?}", renamed.topics);
    assert_eq!(renamed.running.unwrap().task_name, "Social templates, v2");
    // A ten-minute jump with no tick is a sleep: the piece being timed now starts after it.
    clock.advance(600);
    worker.send(Command::MinuteTick).unwrap();
    let slept = one(&worker, &inbox);
    assert!(slept.topics.contains(T | R), "{:?}", slept.topics);
    assert_eq!(slept.running.unwrap().started_at, Some(at(NOW) + 600));
    let rows = slept.tasks.unwrap();
    assert_eq!(row(&rows, STARTED_TASK).open_since, Some(at(NOW) + 600));
}

#[test]
fn a_panicking_callback_loses_its_update_but_not_the_worker() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let inbox = Inbox::new();
    let keep = inbox.notify();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = std::sync::Arc::clone(&calls);
    let notify: zwt_bridge::worker::Notify = Box::new(move |u| {
        if seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            panic!("the UI's event loop has gone");
        }
        keep(u);
    });
    let (worker, _) = Worker::spawn(common::config(dir.path(), &clock), notify).unwrap();
    worker.send(Command::SelectDay(date(2026, 11, 3))).unwrap();
    worker.send(Command::SelectDay(today())).unwrap();
    let got = settle(&worker, &inbox);
    assert_eq!(
        got.len(),
        1,
        "the first update was lost, the second arrived"
    );
    assert_eq!(got[0].day.as_ref().unwrap().head.date, today());
    assert_eq!(worker.stats().lost, 1);
    worker.shutdown().unwrap();
    let reopened = zwt_bridge::sample::open_or_seed(
        dir.path(),
        std::sync::Arc::new(clock.clone()),
        common::ZONE_NAME,
    )
    .unwrap();
    let social = &reopened.service.file().data.tasks[STARTED_TASK];
    assert_eq!(social.state, TaskState::Paused, "shutdown still paused it");
}
