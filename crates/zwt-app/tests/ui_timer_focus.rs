//! A timer button that turns itself off under the keyboard's focus (Enter on Pause pauses the task, and Pause is off
//! for a paused task) keeps the focus, and its ring, until Tab moves on; then the ring goes with the focus. Slint
//! ignores every focus event of a disabled FocusScope, so a button off through FocusScope's `enabled` never heard the
//! focus leave and kept its ring beside the next one (the day picker's arrows had the same fault: review F1).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use common::{Harness, RUNNING, RUNNING_NAME, rect_of};
use i_slint_backend_testing::ElementHandle;
use slint::platform::{Key, WindowEvent};

/// A focus ring is drawn round `element`.
fn ring_around(h: &Harness, element: &ElementHandle) -> bool {
    let (ex, ey, ew, eh) = rect_of(element);
    h.of_type("FocusRing").iter().any(|r| {
        let (rx, ry, rw, rh) = rect_of(r);
        rx < ex && ry < ey && rx + rw > ex + ew && ry + rh > ey + eh
    })
}

/// `key` pressed and released.
fn press(h: &Harness, key: Key) {
    h.key(WindowEvent::KeyPressed { text: key.into() });
    h.key(WindowEvent::KeyReleased { text: key.into() });
}

fn on(button: &ElementHandle) -> bool {
    button.accessible_enabled() == Some(true)
}

#[test]
fn a_timer_button_turned_off_under_the_keyboard_lets_its_ring_go_with_the_focus() {
    let mut h = Harness::new();
    // Keyboard focus on the running task's Pause: a press puts the focus there without a ring, Tab moves it on to Stop
    // (Resume is off and out of the Tab order) and Shift+Tab brings it back by keyboard.
    h.focus(&h.button(RUNNING_NAME, "Pause"));
    press(&h, Key::Tab);
    assert!(
        ring_around(&h, &h.button(RUNNING_NAME, "Stop")),
        "Tab skips the Resume that is off"
    );
    press(&h, Key::Backtab);
    let pause = h.button(RUNNING_NAME, "Pause");
    assert!(ring_around(&h, &pause), "keyboard focus on Pause");

    h.enter();
    h.pump();
    assert_eq!(h.task(RUNNING).state, 2, "Enter paused the task");
    let pause = h.button(RUNNING_NAME, "Pause");
    assert!(!on(&pause), "Pause is off for a paused task");
    assert!(
        ring_around(&h, &pause),
        "the focus, and its ring, stay until Tab moves them"
    );
    let resume = h.button(RUNNING_NAME, "Resume");
    assert!(on(&resume), "Resume is on for a paused task");

    press(&h, Key::Tab);
    assert_eq!(h.of_type("FocusRing").len(), 1, "one focus ring on screen");
    assert!(
        ring_around(&h, &h.button(RUNNING_NAME, "Resume")),
        "around the button that took the focus"
    );
    assert!(!ring_around(&h, &h.button(RUNNING_NAME, "Pause")));
}
