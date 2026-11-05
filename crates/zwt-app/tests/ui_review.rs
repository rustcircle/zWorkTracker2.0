//! Review findings R2 and R3 of milestone 1 (.stack/out/reviewer/20261104-1053-m1-slint-review.md), ported from the
//! tester's repro (.stack/out/tester/20261104-m1-review-repro/tests/review_repro.rs) with the same names and
//! assertions:
//! - R2: `now-sec` is written only by the 1 s clock, which runs only while a task runs. A notice must still go after
//!   8 s with nothing running (1.x Toasts.tsx), and the now line on today's axis must still follow the time.
//! - R3: renaming a task must reach its blocks on the day axis, not only its card.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use std::time::Duration;

use common::{Harness, RUNNING, RUNNING_NAME, settle};
use i_slint_backend_testing::AccessibleRole;
use slint::Model;
use slint::platform::{PointerEventButton, duration_until_next_timer_update};
use z_work_core::machine::TaskInput;
use zwt_bridge::worker::{Command, Mutation};

const COLD: &str = "After a restart, Resume asks for every field again and starts a new task that continues this one. This task stays as it is.";

/// The notice on screen, if any. Not its text: a paused card's cold-launch note has the same words (1.x uses one
/// sentence for both), so the text is on screen with or without the notice (the repro looked for the text, and so
/// could not tell).
fn notice_shown(h: &Harness) -> bool {
    !h.of_type("Notice").is_empty()
}

/// Cold launch (nothing runs), then a Resume that the core refuses: the notice appears.
fn cold_launch_with_notice() -> Harness {
    let mut h = Harness::new().relaunch();
    assert_eq!(h.state().get_running_id(), "", "a cold launch runs nothing");
    assert!(!h.ui.clock().is_running());
    h.button(RUNNING_NAME, "Resume")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    assert_eq!(h.state().get_notice_text(), COLD);
    assert!(
        notice_shown(&h),
        "the notice is on screen right after Resume"
    );
    assert_eq!(
        h.find_all(AccessibleRole::Text, COLD).len(),
        h.tasks().iter().filter(|t| t.cold_note == COLD).count() + 2,
        "the notice and its paragraph, besides the cards' notes"
    );
    h
}

/// The x of the now line on the day axis, if it is drawn.
fn now_line_x(h: &Harness) -> Option<f32> {
    h.find_all(AccessibleRole::Image, "Now")
        .first()
        .map(|e| e.absolute_position().x)
}

/// R2(a): with nothing running, a notice still goes away by itself 8 s after it appeared (1.x Toasts.tsx: a real
/// timeout), and not before.
#[test]
fn r2a_notice_goes_after_8_s_even_when_nothing_runs() {
    let mut h = cold_launch_with_notice();
    let now_before = h.state().get_now_sec();
    for _ in 0..7 {
        h.second();
    }
    assert!(notice_shown(&h), "still on screen 7 s after it appeared");
    h.second();
    let now_after = h.state().get_now_sec();
    eprintln!(
        "R2a now-sec before={now_before} after={now_after} core-clock-advanced=8s clock-running={}",
        h.ui.clock().is_running()
    );
    assert!(
        !notice_shown(&h),
        "notice still on screen 8 s after it appeared with nothing running: now-sec {now_before} -> {now_after}"
    );
    assert_eq!(h.state().get_notice_text(), "", "the notice is gone");
}

/// R2(b): the day axis' now line follows the current time on today, even when nothing runs.
#[test]
fn r2b_now_line_follows_time_when_nothing_runs() {
    let mut h = Harness::new().relaunch();
    assert_eq!(h.state().get_running_id(), "");
    assert_eq!(h.state().get_day_date(), h.state().get_today_date());
    let x_before = now_line_x(&h).expect("the now line is on today's axis");
    let now_before = h.state().get_now_sec();
    // Ten minutes pass with nothing running (one jump of the core clock and of Slint's mock time, then a pump).
    for _ in 0..600 {
        h.clock.advance(1);
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::from_secs(600));
    h.pump();
    let now_after = h.state().get_now_sec();
    let x_after = now_line_x(&h).expect("the now line is still drawn");
    eprintln!(
        "R2b now-sec before={now_before} after={now_after}; now-line x before={x_before} after={x_after}; core clock advanced 600 s"
    );
    assert!(
        x_after > x_before,
        "now line did not move in 10 min with nothing running: x {x_before} -> {x_after}, now-sec {now_before} -> {now_after}"
    );
    assert_eq!(
        now_after, now_before,
        "now-sec stays for the elapsed texts: nothing runs"
    );
}

/// R2(b), the second half of the finding: past midnight, with nothing running, the line starts the new day instead of
/// disappearing until a task starts. Nothing but the clocks moves here: the worker's own midnight deadline runs on
/// real time (and stops while the computer sleeps), so the minute clock's shot past the day's end is what asks the
/// worker to roll the day over.
#[test]
fn r2b_after_midnight_the_now_line_starts_the_new_day() {
    let mut h = Harness::new().relaunch();
    let sent = h.ui.sent_count();
    // From Wednesday 10:30 to Thursday 00:05 in Lisbon (UTC+0), nothing running and no command sent.
    h.clock.set("2026-11-05T00:05:00Z");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_secs(13 * 3600 + 35 * 60));
    h.pump();
    assert_eq!(
        h.ui.sent().last().map(String::as_str),
        Some("MinuteTick"),
        "the minute clock asked the worker to catch up"
    );
    assert_eq!(h.ui.sent_count(), sent + 1, "once");
    assert_eq!(h.state().get_today_date(), "2026-11-05");
    assert_eq!(
        h.state().get_day_date(),
        "2026-11-05",
        "the axis follows today"
    );
    let x = now_line_x(&h).expect("the now line is on the new day's axis");
    let axis = h.find(AccessibleRole::Groupbox, "Today, Thu 5 Nov 2026");
    let (ax, _, aw, _) = common::rect_of(&axis);
    assert!(
        x > ax && x < ax + aw * 0.1,
        "at 00:05 the line sits at the start of the axis: x {x}, card {ax}..{}",
        ax + aw
    );
}

/// R2: with nothing running, a timer runs only while a notice shows or today's axis is on screen. Today's axis wakes
/// the app once a minute (the now line), a notice once, 8 s after it appeared; otherwise nothing is due at all.
#[test]
fn r2_idle_runs_no_timer_unless_a_notice_or_todays_axis_shows() {
    let h = cold_launch_with_notice();
    // The click on Resume moved the mouse over the rail, which shows its scrollbar thumb for a second.
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1100));
    settle();
    let due = duration_until_next_timer_update().expect("a notice shows: its timer runs");
    assert!(
        due <= Duration::from_secs(7),
        "the notice's timer, 8 s after it appeared: {due:?}"
    );

    // Dismissed: its timer goes with it. The now line's clock is next, at the next whole minute (10:31:00).
    h.find(AccessibleRole::Button, "Dismiss message")
        .mock_single_click(PointerEventButton::Left);
    settle();
    assert_eq!(h.state().get_notice_text(), "");
    let due = duration_until_next_timer_update()
        .expect("today's axis is shown: the now line's clock runs");
    assert!(
        due > Duration::from_secs(7) && due <= Duration::from_secs(60),
        "no notice timer left, the next whole minute: {due:?}"
    );

    // Another day on the axis: nothing is due any more.
    let mut h = h;
    h.find(AccessibleRole::RadioButton, "Mon 2 Nov 2026")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    settle();
    assert_eq!(
        duration_until_next_timer_update(),
        None,
        "idle, a past day: no timer"
    );

    // Today again, then the About tab: today's axis is not on screen there either.
    h.find(AccessibleRole::RadioButton, "Wed 4 Nov 2026")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    settle();
    assert!(
        duration_until_next_timer_update().is_some(),
        "today's axis again"
    );
    h.find(AccessibleRole::Tab, "About")
        .mock_single_click(PointerEventButton::Left);
    settle();
    assert_eq!(
        duration_until_next_timer_update(),
        None,
        "idle, About: no timer"
    );
    h.find(AccessibleRole::Tab, "Timeline")
        .mock_single_click(PointerEventButton::Left);
    settle();
    assert!(
        duration_until_next_timer_update().is_some(),
        "back on today's axis"
    );
}

/// R3: a rename reaches the day axis' block labels, not only the task row.
#[test]
fn r3_rename_reaches_the_day_axis_block_labels() {
    let mut h = Harness::new();
    let new_name = "Social templates, v2";
    // The pieces of the renamed task are the ones labelled with its name (names are unique in the sample).
    let before: Vec<String> = h
        .state()
        .get_pieces()
        .iter()
        .filter(|p| p.label == RUNNING_NAME)
        .map(|p| p.id.to_string())
        .collect();
    assert!(!before.is_empty(), "the running task has a piece today");

    let rename = Mutation::new("rename the running task", |svc| {
        let task = svc.file().data.tasks[RUNNING].clone();
        let input = TaskInput {
            name: "Social templates, v2".to_owned(),
            project_id: task.project_id,
            description: task.description,
            continued_from: None,
            tags: task.tags,
        };
        svc.update_task(RUNNING, &input)
    });
    h.worker.send(Command::Mutate(rename)).unwrap();
    let updates = h.pump();

    assert_eq!(
        h.task(RUNNING).name,
        new_name,
        "control: the task row has the new name"
    );
    let labels: Vec<(String, String)> = h
        .state()
        .get_pieces()
        .iter()
        .filter(|p| before.contains(&p.id.to_string()))
        .map(|p| (p.id.to_string(), p.label.to_string()))
        .collect();
    eprintln!(
        "R3 updates applied={updates}; labels of the renamed task's pieces after the rename: {labels:?}"
    );
    assert!(!labels.is_empty(), "the pieces are still there");
    for (id, label) in &labels {
        assert_eq!(label, new_name, "piece {id} on the day axis");
    }
    // The block's accessible name follows too.
    assert!(
        !h.find_all(AccessibleRole::Image, new_name).is_empty(),
        "the running block is named after the task's new name"
    );
}
