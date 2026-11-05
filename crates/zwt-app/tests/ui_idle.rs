//! Which of the UI's own timers run (review R2): the 1 s clock while a task runs; the now line's minute clock while
//! today's axis is on screen and nothing runs; a notice's timer while it shows. Each is asked directly, so a timer
//! hidden behind another one (the 1 s clock is always due first) is seen too.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use common::{DRAFT_NAME, FakeTime, Harness, RUNNING_NAME, settle};
use i_slint_backend_testing::AccessibleRole;
use slint::platform::PointerEventButton;
use zwt_app::clock::MinuteClock;

/// Mid-minute start: the current second at once, then each whole minute on its boundary; asked again, nothing
/// changes; stopped, it stays quiet.
#[test]
fn the_minute_clock_fires_on_whole_minutes_and_stops_when_not_wanted() {
    i_slint_backend_testing::init_no_event_loop();
    // 00:10:20.5 of the epoch.
    let ms = Rc::new(Cell::new(620_500));
    let written: Rc<RefCell<Vec<i64>>> = Rc::default();
    let clock = {
        let written = Rc::clone(&written);
        MinuteClock::new(Rc::new(FakeTime(Rc::clone(&ms))), move |s| {
            written.borrow_mut().push(s);
        })
    };
    let advance = |by: i64| {
        ms.set(ms.get() + by);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(
            u64::try_from(by).unwrap(),
        ));
    };

    clock.follow(true);
    assert_eq!(
        *written.borrow(),
        [620],
        "starting writes the current second"
    );
    advance(39_499);
    assert_eq!(clock.shots(), 0, "not yet on the whole minute");
    advance(1);
    assert_eq!(
        *written.borrow(),
        [620, 660],
        "the first shot is on the whole minute"
    );
    advance(60_000);
    assert_eq!(*written.borrow(), [620, 660, 720]);
    clock.follow(true);
    assert_eq!(written.borrow().len(), 3, "asked again: nothing changes");

    clock.follow(false);
    assert!(!clock.is_running());
    advance(180_000);
    assert_eq!(written.borrow().len(), 3, "a stopped clock stays quiet");
}

#[test]
fn the_now_line_clock_runs_only_on_todays_axis_while_nothing_runs() {
    let mut h = Harness::new();
    settle();
    assert!(h.ui.clock().is_running(), "the sample's task runs");
    assert!(
        !h.ui.now_line().is_running(),
        "a task runs: the 1 s clock moves the line"
    );
    assert!(!h.ui.notice_timer().is_running(), "no notice");

    h.button(RUNNING_NAME, "Pause")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    settle();
    assert!(!h.ui.clock().is_running());
    assert!(
        h.ui.now_line().is_running(),
        "nothing runs, today's axis shows"
    );

    h.find(AccessibleRole::Tab, "About")
        .mock_single_click(PointerEventButton::Left);
    settle();
    assert!(!h.ui.now_line().is_running(), "About: no axis on screen");
    h.find(AccessibleRole::Tab, "Timeline")
        .mock_single_click(PointerEventButton::Left);
    settle();
    assert!(h.ui.now_line().is_running());

    // Starting a draft: the 1 s clock again, and the start pauses nothing, so no notice either.
    h.button(DRAFT_NAME, "Start")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    settle();
    assert!(h.ui.clock().is_running());
    assert!(!h.ui.now_line().is_running());
    assert!(!h.ui.notice_timer().is_running());

    // Resuming the paused task pauses the draft and says so: the notice's timer runs for its 8 s, then stops.
    h.button(RUNNING_NAME, "Resume")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    assert_ne!(h.state().get_notice_text(), "");
    assert!(h.ui.notice_timer().is_running());
    for _ in 0..8 {
        h.second();
    }
    assert_eq!(h.state().get_notice_text(), "");
    assert!(!h.ui.notice_timer().is_running(), "gone with its notice");
}

/// The notice's close button dismisses it from the keyboard or a screen reader too (its accessible action), and the
/// timer goes with it there as well.
#[test]
fn dismissing_by_the_accessible_action_stops_the_notice_timer() {
    let mut h = Harness::new();
    h.button(DRAFT_NAME, "Start")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    assert_ne!(h.state().get_notice_text(), "", "the auto-pause notice");
    assert!(h.ui.notice_timer().is_running());
    h.find(AccessibleRole::Button, "Dismiss message")
        .invoke_accessible_default_action();
    settle();
    assert_eq!(h.state().get_notice_text(), "", "dismissed");
    assert!(!h.ui.notice_timer().is_running(), "its timer stopped");
}
