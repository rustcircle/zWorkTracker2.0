//! The day picker's arrows on screen: "Previous days" (left) shows the page of 14 days before, "Next days" (right) the
//! page after, back to the page that ends today. Each is off, dimmed and deaf to clicks where there is nothing to
//! show: the left one when no day before the page has time, the right one on the page that ends today. The chips stay
//! the oldest first on every page; a click moves the picker only, never the day on the axis.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use common::{Harness, rect_of};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::Model;
use slint::platform::{Key, PointerEventButton, WindowEvent};

const PREVIOUS: &str = "Previous days";
const NEXT: &str = "Next days";

fn arrow(h: &Harness, label: &str) -> ElementHandle {
    h.find(AccessibleRole::Button, label)
}

/// The arrow's icon, as drawn: dimmed to half while the arrow is off.
fn icon_opacity(h: &Harness, label: &str) -> f32 {
    arrow(h, label)
        .query_descendants()
        .match_type_name("Icon")
        .find_first()
        .expect("the arrow draws an icon")
        .computed_opacity()
}

fn on(h: &Harness, label: &str) -> bool {
    arrow(h, label).accessible_enabled() == Some(true)
}

/// The chips' dates, in the model's order.
fn dates(h: &Harness) -> Vec<String> {
    h.state()
        .get_chips()
        .iter()
        .map(|c| c.date.to_string())
        .collect()
}

/// The chips' left edges on screen, in the model's order.
fn chip_xs(h: &Harness) -> Vec<f32> {
    h.state()
        .get_chips()
        .iter()
        .map(|c| {
            h.find(AccessibleRole::RadioButton, c.title.as_str())
                .absolute_position()
                .x
        })
        .collect()
}

/// `first` to `last` (both `2026-mm-dd`), the oldest first.
fn days(first: &str, last: &str) -> Vec<String> {
    let parse = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
    parse(first)
        .iter_days()
        .take_while(|d| *d <= parse(last))
        .map(|d| d.format("%Y-%m-%d").to_string())
        .collect()
}

fn ascending(xs: &[f32]) -> bool {
    xs.windows(2).all(|w| w[0] < w[1])
}

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

/// An hour logged by hand on 15 October, before the page shown first (22 October to today), whose days all have time
/// in the sample.
fn log_15_october(h: &mut Harness) {
    h.log_by_hand("2026-10-15T09:00:00Z", "2026-10-15T10:00:00Z");
}

/// The page shown first ends today: there is no newer day, so the right arrow is off, dimmed, and a click on it sends
/// nothing. The sample's time starts on the page's first day, so the left arrow is off too.
#[test]
fn on_the_page_that_ends_today_the_right_arrow_is_off_and_with_nothing_older_the_left_one_too() {
    let mut h = Harness::new();
    assert_eq!(
        dates(&h),
        days("2026-10-22", "2026-11-04"),
        "today on the page"
    );
    for label in [NEXT, PREVIOUS] {
        assert!(!on(&h, label), "{label} is off");
        let dim = icon_opacity(&h, label);
        assert!((dim - 0.5).abs() < 0.01, "{label} dimmed to half: {dim}");
        let sent = h.ui.sent_count();
        arrow(&h, label).mock_single_click(PointerEventButton::Left);
        h.pump();
        assert_eq!(h.ui.sent_count(), sent, "{label}: a click sends nothing");
    }
    assert_eq!(
        dates(&h),
        days("2026-10-22", "2026-11-04"),
        "the page stays"
    );
}

/// With time before the page, the left arrow works: it shows the 14 days before, the oldest first, and is off there
/// (nothing older has time); the right arrow comes on and brings the page that ends today back. Only the picker is
/// rewritten; the axis keeps the day shown.
#[test]
fn the_left_arrow_shows_older_days_until_the_first_day_with_time_and_the_right_one_comes_back() {
    let mut h = Harness::new();
    log_15_october(&mut h);
    assert!(
        on(&h, PREVIOUS),
        "time before the page: the left arrow is on"
    );
    assert!(
        (icon_opacity(&h, PREVIOUS) - 1.0).abs() < 0.01,
        "not dimmed"
    );
    assert!(!on(&h, NEXT));

    let before = h.ui.writes();
    arrow(&h, PREVIOUS).mock_single_click(PointerEventButton::Left);
    assert_eq!(h.ui.sent().last().unwrap(), "OlderDays");
    h.pump();
    assert_eq!(
        dates(&h),
        days("2026-10-08", "2026-10-21"),
        "the page before, oldest first"
    );
    assert!(ascending(&chip_xs(&h)), "left to right: {:?}", chip_xs(&h));
    assert!(
        !on(&h, PREVIOUS),
        "15 Oct, the first day with time, is on this page"
    );
    assert!(on(&h, NEXT), "today is after this page");
    let after = h.ui.writes();
    assert_eq!(
        (
            after.tasks,
            after.pieces,
            after.share_rows,
            after.share_arcs
        ),
        (
            before.tasks,
            before.pieces,
            before.share_rows,
            before.share_arcs
        ),
        "a click rewrites the picker only"
    );
    assert_ne!(after.chips, before.chips, "the chips were rewritten");
    assert_eq!(
        h.state().get_day_date(),
        "2026-11-04",
        "the axis keeps the day shown"
    );
    assert_eq!(h.state().get_day_label(), "Today, Wed 4 Nov 2026");

    arrow(&h, NEXT).mock_single_click(PointerEventButton::Left);
    assert_eq!(h.ui.sent().last().unwrap(), "NewerDays");
    h.pump();
    assert_eq!(
        dates(&h),
        days("2026-10-22", "2026-11-04"),
        "back to today's page"
    );
    assert!(ascending(&chip_xs(&h)));
    assert!(!on(&h, NEXT), "no day after today");
    assert!(on(&h, PREVIOUS));
    assert_eq!(
        h.find(AccessibleRole::RadioButton, "Wed 4 Nov 2026")
            .accessible_checked(),
        Some(true),
        "today's chip is marked again"
    );
}

/// The keyboard: Enter on a focused arrow acts once, Space acts once when released (as on every button but the timer
/// buttons), and focus that came by Tab shows the focus ring around the arrow.
#[test]
fn the_arrows_take_enter_and_space_and_show_the_focus_ring_for_the_keyboard() {
    let mut h = Harness::new();
    log_15_october(&mut h);

    let previous = arrow(&h, PREVIOUS);
    h.focus(&previous);
    let sent = h.ui.sent_count();
    h.enter();
    assert_eq!(h.ui.sent_count(), sent + 1, "Enter: one command");
    assert_eq!(h.ui.sent().last().unwrap(), "OlderDays");
    h.pump();
    assert_eq!(dates(&h), days("2026-10-08", "2026-10-21"));

    let next = arrow(&h, NEXT);
    h.focus(&next);
    let sent = h.ui.sent_count();
    h.hold_space();
    assert_eq!(
        h.ui.sent_count(),
        sent + 1,
        "Space held: one command, on release"
    );
    assert_eq!(h.ui.sent().last().unwrap(), "NewerDays");
    h.pump();
    assert_eq!(dates(&h), days("2026-10-22", "2026-11-04"));

    // Focus by mouse shows no ring; Tab to the first chip and Shift+Tab back does.
    let previous = arrow(&h, PREVIOUS);
    h.focus(&previous);
    assert!(!ring_around(&h, &previous), "no ring for a mouse focus");
    press(&h, Key::Tab);
    press(&h, Key::Backtab);
    assert!(
        ring_around(&h, &arrow(&h, PREVIOUS)),
        "the ring around the arrow focused by the keyboard"
    );
    let sent = h.ui.sent_count();
    h.enter();
    assert_eq!(h.ui.sent().last().unwrap(), "OlderDays");
    assert_eq!(h.ui.sent_count(), sent + 1);
}

/// An arrow that turns off while it has the keyboard's focus keeps the focus, dimmed and deaf to Enter and Space, until
/// Tab moves on; then its ring goes with the focus, so one ring is left, around the first chip. (Slint ignores every
/// focus event of a disabled FocusScope, so an arrow off through `enabled` never heard the focus leave and kept its
/// ring: review F1.)
#[test]
fn an_arrow_turned_off_under_the_keyboard_lets_its_ring_go_with_the_focus() {
    let mut h = Harness::new();
    log_15_october(&mut h);
    h.focus(&arrow(&h, PREVIOUS));
    press(&h, Key::Tab);
    press(&h, Key::Backtab);
    assert!(ring_around(&h, &arrow(&h, PREVIOUS)), "keyboard focus");

    h.enter();
    h.pump();
    assert_eq!(dates(&h), days("2026-10-08", "2026-10-21"));
    assert!(!on(&h, PREVIOUS), "off: nothing older than 8 Oct has time");
    let sent = h.ui.sent_count();
    h.enter();
    h.hold_space();
    assert_eq!(
        h.ui.sent_count(),
        sent,
        "Enter and Space do nothing while off"
    );

    press(&h, Key::Tab);
    let rings = h.of_type("FocusRing");
    assert_eq!(rings.len(), 1, "one focus ring on screen");
    let first = h.find(AccessibleRole::RadioButton, "Thu 8 Oct 2026");
    assert!(
        ring_around(&h, &first),
        "around the chip that took the focus"
    );
    assert!(!ring_around(&h, &arrow(&h, PREVIOUS)));
}
