//! The day picker's arrows at the source: the worker moves the picker one page (14 days) into the past or towards
//! today, never past the page that holds the first day with time and never past today. The chips stay the oldest first
//! on every page; the day on the axis stays; nothing but the picker moves.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use chrono::NaiveDate;
use common::{NOW, date, minutes, one, settle, spawn, today};
use z_work_core::adjust::ManualEntryInput;
use z_work_core::{ManualClock, Ts};
use zwt_bridge::hub::{Hub, Recorder};
use zwt_bridge::topics::Topics;
use zwt_bridge::views::DayPage;
use zwt_bridge::worker::{Command, Mutation, Update};

/// An hour of "Logo concepts" logged by hand on Thursday 15 October 2026: before the page shown first (22 October to
/// 4 November, every day of it with time in the sample).
fn log_15_october() -> Command {
    let start = Ts::parse("2026-10-15T09:00:00Z").unwrap();
    let end = Ts::parse("2026-10-15T10:00:00Z").unwrap();
    Command::Mutate(Mutation::new("log 15 October", move |svc| {
        svc.add_manual_entry(&ManualEntryInput {
            task_id: "t-logo".to_owned(),
            start,
            end,
            reason: "Forgot to start the timer".to_owned(),
            subtask_id: None,
            allow_overlap: false,
        })
        .map(|_| ())
    }))
}

/// Voids the hour logged by hand: it no longer counts anywhere.
fn void_the_hand_logged_hour() -> Command {
    Command::Mutate(Mutation::new("void the manual entry", |svc| {
        let id = svc
            .file()
            .data
            .segments
            .values()
            .find(|s| s.manual)
            .map(|s| s.id.clone())
            .unwrap_or_default();
        svc.void_segment(&id, "Logged on the wrong day")
    }))
}

/// Every day from `first` to `last`, the oldest first.
fn days(first: NaiveDate, last: NaiveDate) -> Vec<NaiveDate> {
    first.iter_days().take_while(|d| *d <= last).collect()
}

fn dates(update: &Update) -> Vec<NaiveDate> {
    update
        .chips
        .as_ref()
        .unwrap()
        .iter()
        .map(|c| c.date)
        .collect()
}

#[test]
fn the_page_shown_first_ends_today_and_with_nothing_older_neither_arrow_moves_it() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, first, inbox) = spawn(dir.path(), &clock);
    assert_eq!(dates(&first), days(date(2026, 10, 22), today()));
    assert_eq!(
        first.page,
        Some(DayPage {
            first: date(2026, 10, 22),
            last: today(),
            older: false,
            newer: false,
        }),
        "the sample's time starts on the page's first day; no day comes after today"
    );
    worker.send(Command::OlderDays).unwrap();
    worker.send(Command::NewerDays).unwrap();
    assert_eq!(
        settle(&worker, &inbox),
        Vec::<Update>::new(),
        "both refused"
    );
}

#[test]
fn the_arrows_move_the_picker_a_page_at_a_time_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, first, inbox) = spawn(dir.path(), &clock);
    let mut hub = Hub::new();
    let mut ui = Recorder::default();
    hub.apply(first, &mut ui.sinks());

    // Time logged before the page reaches the picker: its left arrow now works.
    worker.send(log_15_october()).unwrap();
    let logged = one(&worker, &inbox);
    assert!(logged.topics.contains(Topics::CHIPS), "{:?}", logged.topics);
    assert_eq!(
        dates(&logged),
        days(date(2026, 10, 22), today()),
        "same days"
    );
    assert_eq!(logged.page.map(|p| (p.older, p.newer)), Some((true, false)));
    hub.apply(logged, &mut ui.sinks());

    // Left: the 14 days before, the oldest first. Only the picker moves; the axis keeps today.
    worker.send(Command::OlderDays).unwrap();
    let older = one(&worker, &inbox);
    assert_eq!(older.topics, Topics::CHIPS);
    assert_eq!(dates(&older), days(date(2026, 10, 8), date(2026, 10, 21)));
    assert_eq!(
        older.page,
        Some(DayPage {
            first: date(2026, 10, 8),
            last: date(2026, 10, 21),
            older: false,
            newer: true,
        }),
        "the first day with time (15 Oct) is on this page; today is after it"
    );
    let hour = older
        .chips
        .as_ref()
        .unwrap()
        .iter()
        .find(|c| c.date == date(2026, 10, 15))
        .map(|c| c.closed_secs);
    assert_eq!(hour, Some(3600), "15 Oct holds the hour logged by hand");
    let applied = hub.apply(older, &mut ui.sinks());
    assert_eq!(applied.page, 1, "the arrows' state written once");
    assert_eq!(
        applied.tasks.total()
            + applied.running
            + applied.day_head
            + applied.day_pieces.total()
            + applied.share_head
            + applied.share_rows.total(),
        0,
        "nothing but the picker is written"
    );
    assert_eq!(
        hub.day_head().map(|h| h.date),
        Some(today()),
        "the axis keeps today"
    );

    // Left again: refused, nothing older has time.
    worker.send(Command::OlderDays).unwrap();
    assert_eq!(settle(&worker, &inbox), Vec::<Update>::new());

    // Right: back to the page that ends today, and no further.
    worker.send(Command::NewerDays).unwrap();
    let newer = one(&worker, &inbox);
    assert_eq!(newer.topics, Topics::CHIPS);
    assert_eq!(dates(&newer), days(date(2026, 10, 22), today()));
    assert_eq!(newer.page.map(|p| (p.older, p.newer)), Some((true, false)));
    worker.send(Command::NewerDays).unwrap();
    assert_eq!(
        settle(&worker, &inbox),
        Vec::<Update>::new(),
        "no day after today"
    );
}

#[test]
fn voiding_the_only_older_time_turns_the_left_arrow_off() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, _first, inbox) = spawn(dir.path(), &clock);
    worker.send(log_15_october()).unwrap();
    assert_eq!(one(&worker, &inbox).page.map(|p| p.older), Some(true));
    worker.send(void_the_hand_logged_hour()).unwrap();
    let voided = one(&worker, &inbox);
    assert!(voided.topics.contains(Topics::CHIPS), "{:?}", voided.topics);
    assert_eq!(
        voided.page.map(|p| (p.older, p.newer)),
        Some((false, false))
    );
}

#[test]
fn a_page_back_moves_with_today_at_midnight_and_stays_the_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    // Two minutes before midnight in Lisbon (UTC+0 in November).
    let clock = ManualClock::at("2026-11-04T23:58:00Z");
    let (worker, _first, inbox) = spawn(dir.path(), &clock);
    worker.send(log_15_october()).unwrap();
    worker.send(Command::OlderDays).unwrap();
    let updates = settle(&worker, &inbox);
    let paged = updates.last().unwrap();
    assert_eq!(dates(paged), days(date(2026, 10, 8), date(2026, 10, 21)));

    // Three minutes later it is Thursday: the page is still one page before today's, so it moves on a day.
    let ticks = minutes(&worker, &inbox, &clock, 3);
    let rolled = ticks
        .iter()
        .rev()
        .find(|u| u.chips.is_some())
        .expect("the day rolled over");
    assert_eq!(dates(rolled), days(date(2026, 10, 9), date(2026, 10, 22)));
    assert_eq!(
        rolled.page,
        Some(DayPage {
            first: date(2026, 10, 9),
            last: date(2026, 10, 22),
            older: false,
            newer: true,
        })
    );
    assert!(rolled.day.is_some(), "the axis follows the new today");
}
