//! The owner's rule for 2.0 (2026-11-04): the Timeline runs from the past to the future. At the source of the order
//! (`views::chips`, which the worker sends), the day picker's chips come oldest first and end with today; the day
//! axis' pieces come by start.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use chrono::{Days, NaiveDate};
use common::{NOW, ZONE, spawn, today};
use z_work_core::{ManualClock, Ts};
use zwt_bridge::{sample, views};

/// The 14 days ending today, the oldest first.
fn two_weeks_to_today() -> Vec<NaiveDate> {
    (0..14u64)
        .rev()
        .map(|back| today().checked_sub_days(Days::new(back)).unwrap())
        .collect()
}

#[test]
fn the_day_picker_runs_from_the_past_to_the_future() {
    let file = sample::build(Ts::parse(NOW).unwrap(), ZONE);
    let source: Vec<NaiveDate> = views::chips(&file, today(), ZONE)
        .iter()
        .map(|c| c.date)
        .collect();
    assert_eq!(source, two_weeks_to_today(), "oldest first, today last");

    // What the worker sends the UI on open: the same order, today included.
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (_worker, first, _inbox) = spawn(dir.path(), &clock);
    let sent: Vec<NaiveDate> = first.chips.unwrap().iter().map(|c| c.date).collect();
    assert_eq!(sent, two_weeks_to_today(), "oldest first, today last");
    let day = first.day.unwrap();
    assert!(day.pieces.len() >= 4, "today has blocks");
    assert!(
        day.pieces.windows(2).all(|w| w[0].start <= w[1].start),
        "the day's pieces by start: {:?}",
        day.pieces.iter().map(|p| p.start).collect::<Vec<_>>()
    );
}
