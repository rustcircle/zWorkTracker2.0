//! The owner's rule for 2.0 (2026-11-04): the Timeline runs from the past to the future. On screen, the day picker's
//! chips go left to right from the oldest day to today, and a day's blocks sit left to right by their start. A day with
//! no time yet (the new day after midnight) keeps a chip as wide as the others.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use std::time::Duration;

use common::Harness;
use i_slint_backend_testing::AccessibleRole;
use slint::Model;

/// The day picker's chips as the model holds them: date and accessible name.
fn chips(h: &Harness) -> Vec<(String, String)> {
    h.state()
        .get_chips()
        .iter()
        .map(|c| (c.date.to_string(), c.title.to_string()))
        .collect()
}

#[test]
fn the_timeline_runs_from_the_past_to_the_future() {
    let h = Harness::new();
    let chips = chips(&h);
    let dates: Vec<&str> = chips.iter().map(|(d, _)| d.as_str()).collect();
    assert_eq!(dates.len(), 14, "two weeks of chips");
    let mut ascending = dates.clone();
    ascending.sort_unstable();
    ascending.dedup();
    assert_eq!(dates, ascending, "the oldest day first");
    assert_eq!(dates.first(), Some(&"2026-10-22"));
    assert_eq!(dates.last(), Some(&"2026-11-04"), "today last");

    // On screen: each chip right of the one before, so today is the rightmost.
    let xs: Vec<f32> = chips
        .iter()
        .map(|(_, title)| {
            h.find(AccessibleRole::RadioButton, title)
                .absolute_position()
                .x
        })
        .collect();
    assert!(
        xs.windows(2).all(|w| w[0] < w[1]),
        "chips left to right in date order: {xs:?}"
    );

    // Today's axis: the blocks in the model by start, and on screen left to right by start.
    let starts: Vec<i32> = h.state().get_pieces().iter().map(|p| p.start).collect();
    assert!(starts.len() >= 4, "today has blocks: {starts:?}");
    assert!(
        starts.windows(2).all(|w| w[0] <= w[1]),
        "pieces by start: {starts:?}"
    );
    let blocks: Vec<f32> = h
        .of_type("Block")
        .iter()
        .map(|b| b.absolute_position().x)
        .collect();
    assert_eq!(blocks.len(), starts.len(), "one block per piece");
    assert!(
        blocks.windows(2).all(|w| w[0] <= w[1]),
        "blocks left to right by start: {blocks:?}"
    );
}

/// The first day after midnight has no closed time yet: its chip is as wide as the others, so its labels are not cut
/// to an ellipsis (it was, in the REN run at 00:03: an empty time bar capped the chip at the width of "…").
#[test]
fn a_day_without_time_keeps_a_chip_as_wide_as_the_others() {
    let mut h = Harness::new().relaunch();
    // Thursday 00:05 in Lisbon: the minute clock of the now line asks the worker to roll the day over.
    h.clock.set("2026-11-05T00:05:00Z");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_secs(13 * 3600 + 35 * 60));
    h.pump();
    assert_eq!(h.state().get_today_date(), "2026-11-05");
    let new_day = h
        .state()
        .get_chips()
        .iter()
        .find(|c| c.date == "2026-11-05")
        .expect("a chip for the new day");
    assert_eq!(new_day.parts.row_count(), 0, "no closed time on it yet");
    let width = |title: &str| h.find(AccessibleRole::RadioButton, title).size().width;
    let (empty, full) = (width("Thu 5 Nov 2026"), width("Wed 4 Nov 2026"));
    assert!(
        (empty - full).abs() <= 1.0,
        "the new day's chip is {empty} px wide, a day with time {full} px"
    );
}
