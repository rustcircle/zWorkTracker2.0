//! Milestone 1 acceptance criteria (PLAN.md 6, AC a-d), driven through the real `AppWindow` and `Ui` with the
//! testing backend, Slint's mock time and the core's `ManualClock`, on a sample store in a temp dir.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use std::time::Duration;

use common::{DRAFT, DRAFT_NAME, Harness, RUNNING, RUNNING_NAME};
use i_slint_backend_testing::AccessibleRole;
use slint::platform::PointerEventButton;
use zwt_bridge::format::ticker;

// Task states in the model (`TaskRowData.state`).
const DRAFT_STATE: i32 = 0;
const RUNNING_STATE: i32 = 1;
const PAUSED_STATE: i32 = 2;
const STOPPED_STATE: i32 = 3;
// Share levels (`AppState.share-shown`).
const PROJECTS: i32 = 0;
const TAGS: i32 = 3;

/// AC (a): while a task runs, its elapsed label changes once per second, on the second and never between; once it is
/// paused, the label stays as it is.
#[test]
fn ac_a_elapsed_label_ticks_once_per_second_while_running_and_stays_after_pause() {
    let mut h = Harness::new();
    assert_eq!(h.task(RUNNING).state, RUNNING_STATE);
    assert!(h.ui.clock().is_running(), "a task runs, so the clock runs");
    let mut last = h.elapsed(RUNNING).unwrap();
    assert_eq!(last, "0:00:00");

    for second in 1..=8_i64 {
        // Half a second of mock time: the clock has not reached the next second, nothing moves.
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(500));
        assert_eq!(
            h.elapsed(RUNNING).unwrap(),
            last,
            "no change within a second"
        );
        // The core's clock reaches the next whole second with the rest of it.
        h.clock.advance(1);
        let shots = h.ui.clock().shots();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(500));
        assert_eq!(h.ui.clock().shots(), shots + 1, "one shot per second");
        h.pump();
        let now = h.elapsed(RUNNING).unwrap();
        assert_ne!(now, last, "the label changes on the second");
        assert_eq!(now, ticker(second), "the 1.x format, from now-sec");
        last = now;
    }

    // Pause through its button.
    h.button(RUNNING_NAME, "Pause")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    assert_eq!(h.task(RUNNING).state, PAUSED_STATE);
    assert!(
        !h.ui.clock().is_running(),
        "nothing runs: the clock stopped"
    );
    let paused = h.elapsed(RUNNING).unwrap();
    assert_eq!(paused, "8s logged", "the paused card shows the logged time");
    let now_sec = h.state().get_now_sec();
    for _ in 0..8 {
        h.second();
        assert_eq!(
            h.elapsed(RUNNING).unwrap(),
            paused,
            "paused: the label stays"
        );
    }
    assert_eq!(
        h.state().get_now_sec(),
        now_sec,
        "no second is written while nothing runs"
    );
}

/// Space, pressed and held, on a focused timer button: no command, the task keeps its state. Enter then activates.
fn space_then_enter(
    h: &mut Harness,
    task_id: &str,
    task_name: &str,
    label: &str,
    before: i32,
    after: i32,
) {
    let button = h.button(task_name, label);
    assert_eq!(button.accessible_enabled(), Some(true), "{label} is on");
    h.focus(&button);
    h.pump();
    let sent = h.ui.sent_count();
    let handled = h.handled();
    assert_eq!(h.task(task_id).state, before);

    h.hold_space();
    assert_eq!(h.ui.sent_count(), sent, "{label}: Space sends nothing");
    assert_eq!(h.handled(), handled, "{label}: the worker got nothing");
    h.pump();
    assert_eq!(
        h.task(task_id).state,
        before,
        "{label}: Space leaves the task as it was"
    );
    let row = h.ui.hub().tasks().iter().find(|r| r.id == task_id).unwrap();
    assert_eq!(zwt_app::convert::state_index(row.state), before);

    // The same button, still focused, takes Enter.
    h.enter();
    assert_eq!(
        h.ui.sent_count(),
        sent + 1,
        "{label}: Enter sends one command"
    );
    assert_eq!(
        h.ui.sent().last().unwrap(),
        &format!("{label}({task_id:?})")
    );
    h.pump();
    assert_eq!(h.task(task_id).state, after, "{label}: Enter acted");
}

/// AC (b): Space does nothing on Start, Pause, Resume and Stop, pressed or held; Enter on the focused button works.
#[test]
fn ac_b_space_never_acts_on_a_timer_button_and_enter_does() {
    let mut h = Harness::new();
    assert_eq!(h.task(DRAFT).state, DRAFT_STATE);
    space_then_enter(
        &mut h,
        DRAFT,
        DRAFT_NAME,
        "Start",
        DRAFT_STATE,
        RUNNING_STATE,
    );
    assert_eq!(
        h.task(RUNNING).state,
        PAUSED_STATE,
        "starting one pauses the other"
    );
    space_then_enter(
        &mut h,
        DRAFT,
        DRAFT_NAME,
        "Pause",
        RUNNING_STATE,
        PAUSED_STATE,
    );
    space_then_enter(
        &mut h,
        DRAFT,
        DRAFT_NAME,
        "Resume",
        PAUSED_STATE,
        RUNNING_STATE,
    );
    space_then_enter(
        &mut h,
        DRAFT,
        DRAFT_NAME,
        "Stop",
        RUNNING_STATE,
        STOPPED_STATE,
    );
}

/// AC (c): the level picked stays when the data changes (PARITY 212, 349).
#[test]
fn ac_c_share_level_stays_tags_when_the_data_changes() {
    let mut h = Harness::new();
    assert_eq!(h.state().get_share_shown(), PROJECTS);
    let switch = |h: &Harness| h.find(AccessibleRole::RadioButton, "Tags");
    switch(&h).mock_single_click(PointerEventButton::Left);
    h.pump();
    assert_eq!(h.state().get_share_shown(), TAGS);
    assert_eq!(h.state().get_share_picked(), TAGS);
    assert_eq!(switch(&h).accessible_checked(), Some(true));
    let rows_before = h.ui.writes().share_rows;

    let check = |h: &Harness, after: &str| {
        assert_eq!(
            h.state().get_share_shown(),
            TAGS,
            "still Tags after {after}"
        );
        assert_eq!(
            h.state().get_share_picked(),
            TAGS,
            "still picked after {after}"
        );
        assert_eq!(
            switch(h).accessible_checked(),
            Some(true),
            "the switch shows Tags after {after}"
        );
    };
    h.button(RUNNING_NAME, "Pause")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    check(&h, "Pause");
    h.button(DRAFT_NAME, "Start")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    check(&h, "Start");
    for _ in 0..61 {
        h.second();
    }
    check(&h, "a minute of running time");
    h.button(DRAFT_NAME, "Stop")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    check(&h, "Stop");
    let rows_after = h.ui.writes().share_rows;
    assert!(
        rows_after.total() > rows_before.total(),
        "the Tags rows were recomputed: {rows_before:?} -> {rows_after:?}"
    );
}

/// AC (d): a tick of the clock writes nothing to the task list or the day axis models, across a minute boundary too,
/// while now-sec moves every second.
#[test]
fn ac_d_clock_ticks_write_nothing_to_the_task_list_or_the_day_axis() {
    let mut h = Harness::new();
    assert!(h.ui.clock().is_running());
    let before = h.ui.writes();
    let start = h.state().get_now_sec();
    let mut seconds = Vec::new();
    // From 10:30:00 to 10:31:05: crosses 10:31:00, where the clock sends a minute tick.
    for _ in 0..65 {
        h.second();
        seconds.push(h.state().get_now_sec());
    }
    let after = h.ui.writes();
    assert_eq!(after.tasks, before.tasks, "no write to the task list");
    assert_eq!(after.pieces, before.pieces, "no write to the day axis");
    assert_eq!(after.chips, before.chips, "no write to the day chips");
    let expected: Vec<i32> = (1..=65).map(|s| start + s).collect();
    assert_eq!(seconds, expected, "now-sec moved once per second");
    let ticks = h.ui.sent().iter().filter(|c| *c == "MinuteTick").count();
    assert_eq!(ticks, 1, "one minute boundary, one minute tick");
    // The running block followed now-sec by binding: its piece row still has no end.
    let pieces = h.state().get_pieces();
    assert!(
        slint::Model::iter(&pieces).any(|p| p.open),
        "the running piece is drawn to now-sec"
    );
}
