//! The rail's scrollbar (V-T10, V-D8; owner decision 5): an overlay thumb in the theme's colours instead of the
//! std-widgets ScrollView's, hidden at rest, shown while the rail scrolls or the mouse moves over it, hidden again
//! once both are still. One single-shot timer does the hiding: at rest no timer runs at all.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use std::time::Duration;

use common::{Harness, rect_of, settle};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::platform::{WindowEvent, duration_until_next_timer_update};
use slint::{ComponentHandle, LogicalPosition, LogicalSize};

/// How long the rail must be still before the thumb hides (rail.slint `TaskRail.thumb-idle`).
const IDLE: Duration = Duration::from_millis(1000);

/// A cold launch in the screenshots' window, where the rail is taller than the window: nothing runs, so not even the
/// 1 s clock has a timer. The day axis shows a past day: on today's, the now line's minute clock would run (review
/// R2), and the rail's own timer is the one these tests watch.
fn cold_rail() -> Harness {
    i_slint_backend_testing::init_no_event_loop();
    let h = Harness::open(LogicalSize::new(1360.0, 900.0));
    let mut h = h.relaunch_at(LogicalSize::new(1360.0, 900.0));
    assert!(!h.ui.clock().is_running(), "nothing runs");
    h.state().invoke_select_day("2026-11-02".into());
    h.pump();
    assert_eq!(h.state().get_day_date(), "2026-11-02");
    assert!(!h.ui.now_line().is_running(), "no now line on a past day");
    h
}

fn thumbs(h: &Harness) -> Vec<ElementHandle> {
    h.of_type("RailThumb")
}

#[test]
fn the_rail_has_no_std_widgets_scrollbar() {
    let h = cold_rail();
    let rail = h.find(AccessibleRole::Groupbox, "Tasks");
    assert!(
        rail.query_descendants()
            .match_inherits("ScrollView")
            .find_first()
            .is_none(),
        "the rail scrolls in a Flickable with its own thumb"
    );
}

#[test]
fn the_thumb_shows_while_the_rail_scrolls_and_hides_when_it_is_still() {
    let h = cold_rail();
    settle();
    assert!(thumbs(&h).is_empty(), "hidden at rest");
    assert_eq!(duration_until_next_timer_update(), None, "no timer at rest");

    // The wheel over a card scrolls the rail.
    let card = h.find(AccessibleRole::Groupbox, "Logo concepts");
    let before = rect_of(&card).1;
    card.scroll(0.0, -120.0);
    settle();
    assert!(rect_of(&card).1 < before, "the rail scrolled");
    let shown = thumbs(&h);
    assert_eq!(shown.len(), 1, "shown while scrolling");
    let (x, _, w, height) = rect_of(&shown[0]);
    let rail = h.find(AccessibleRole::Groupbox, "Tasks");
    let (rx, _, rw, _) = rect_of(&rail);
    assert!(
        x + w <= rx + rw - 1.0 && x + w >= rx + rw - 16.0,
        "at the rail's right edge"
    );
    assert!(height >= 24.0);
    assert!(
        duration_until_next_timer_update().is_some(),
        "the hide timer runs"
    );

    // Scrolling again before it fires keeps it up for another full second.
    i_slint_backend_testing::mock_elapsed_time(IDLE / 2);
    card.scroll(0.0, -40.0);
    settle();
    i_slint_backend_testing::mock_elapsed_time(IDLE * 3 / 4);
    assert_eq!(
        thumbs(&h).len(),
        1,
        "still shown 3/4 s after the last scroll"
    );

    // Still for the whole delay: hidden, and nothing runs any more.
    i_slint_backend_testing::mock_elapsed_time(IDLE / 4 + Duration::from_millis(10));
    settle();
    assert!(thumbs(&h).is_empty(), "hidden once still");
    assert_eq!(duration_until_next_timer_update(), None, "no timer left");
}

#[test]
fn the_thumb_shows_while_the_mouse_moves_over_the_rail() {
    let h = cold_rail();
    let window = h.app.window();
    let rail = h.find(AccessibleRole::Groupbox, "Tasks");
    let (rx, ry, rw, rh) = rect_of(&rail);
    let at = |dx: f32, dy: f32| LogicalPosition::new(rx + rw * dx, ry + rh * dy);

    window.dispatch_event(WindowEvent::PointerMoved {
        position: at(0.5, 0.5),
    });
    settle();
    assert_eq!(
        thumbs(&h).len(),
        1,
        "shown when the mouse moves over the rail"
    );
    window.dispatch_event(WindowEvent::PointerMoved {
        position: at(0.52, 0.55),
    });
    settle();
    i_slint_backend_testing::mock_elapsed_time(IDLE + Duration::from_millis(10));
    settle();
    assert!(thumbs(&h).is_empty(), "hidden once the mouse stops");
    assert_eq!(duration_until_next_timer_update(), None);

    // Outside the rail, the mouse does not show it.
    window.dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(rx + rw + 300.0, ry + 200.0),
    });
    settle();
    assert!(thumbs(&h).is_empty());
    assert_eq!(duration_until_next_timer_update(), None);
}

#[test]
fn dragging_the_thumb_scrolls_the_rail_and_keeps_it_shown() {
    let h = cold_rail();
    let card = h.find(AccessibleRole::Groupbox, "Logo concepts");
    card.scroll(0.0, -10.0);
    settle();
    let thumb = thumbs(&h).pop().expect("shown by the wheel");
    let (tx, ty, tw, th) = rect_of(&thumb);
    let card_y = rect_of(&card).1;
    // Down by 200 px: the rail scrolls down by more (the thumb's run is shorter than the content's).
    thumb.mock_drag(
        LogicalPosition::new(tx + tw / 2.0, ty + th / 2.0 + 200.0),
        slint::platform::PointerEventButton::Left,
    );
    settle();
    let moved = card_y - rect_of(&card).1;
    assert!(moved > 200.0, "the rail scrolled by {moved} px");
    let after = thumbs(&h);
    assert_eq!(after.len(), 1, "still shown after the drag");
    assert!(
        rect_of(&after[0]).1 > ty + 150.0,
        "the thumb followed the pointer"
    );
    i_slint_backend_testing::mock_elapsed_time(IDLE + Duration::from_millis(10));
    settle();
    assert!(thumbs(&h).is_empty(), "hidden once still");
    assert_eq!(duration_until_next_timer_update(), None);
}
