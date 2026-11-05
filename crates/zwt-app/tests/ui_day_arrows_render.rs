//! The day picker's arrows as the software renderer draws them (1.x ui.css `.icon-btn`): no frame at rest, the text
//! colour at 7 % under the icon on hover (`--hover`) and at 13 % while pressed (`--press`); an arrow that is off shows
//! neither and its icon is dimmed to half.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use common::{Harness, hex, rect_of};
use i_slint_backend_testing::AccessibleRole;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, LogicalSize};

/// Light theme tokens (theme.slint `tokens-light`).
const SURFACE: &str = "#ffffff";
const TEXT: &str = "#111827";

/// `over` at alpha `a` on `under`, per channel, as the renderer blends.
fn mix(over: [u8; 3], under: [u8; 3], a: f32) -> [u8; 3] {
    [0, 1, 2].map(|i| (f32::from(over[i]) * a + f32::from(under[i]) * (1.0 - a)).round() as u8)
}

/// The arrow's centre, and a pixel of its backdrop: 4 px in from its left edge, half way down, left of the icon.
fn spots(h: &Harness, label: &str) -> (LogicalPosition, (u32, u32)) {
    let (x, y, w, height) = rect_of(&h.find(AccessibleRole::Button, label));
    assert_eq!(
        (w, height),
        (30.0, 30.0),
        "{label}: 1.x .icon-btn is 30 px square"
    );
    let centre = LogicalPosition::new(x + w / 2.0, y + height / 2.0);
    (centre, (x as u32 + 4, (y + height / 2.0) as u32))
}

#[test]
fn the_arrows_show_hover_and_press_and_nothing_while_off() {
    let mut h = Harness::rendered(LogicalSize::new(1360.0, 900.0));
    h.log_by_hand("2026-10-15T09:00:00Z", "2026-10-15T10:00:00Z");
    let window = h.app.window();
    let (left, (lx, ly)) = spots(&h, "Previous days");
    let (right, (rx, ry)) = spots(&h, "Next days");
    let away = LogicalPosition::new(2.0, 2.0);
    let surface = hex(SURFACE);

    let rest = h.snapshot();
    assert!(
        rest.distance(lx, ly, surface) <= 1,
        "at rest: {:?}",
        rest.rgb(lx, ly)
    );

    window.dispatch_event(WindowEvent::PointerMoved { position: left });
    let hover = h.snapshot();
    let want = mix(hex(TEXT), surface, 0.07);
    assert!(
        hover.distance(lx, ly, want) <= 2,
        "hover: {:?}, want {want:?}",
        hover.rgb(lx, ly)
    );

    window.dispatch_event(WindowEvent::PointerPressed {
        position: left,
        button: PointerEventButton::Left,
    });
    let pressed = h.snapshot();
    let want = mix(hex(TEXT), surface, 0.13);
    assert!(
        pressed.distance(lx, ly, want) <= 2,
        "pressed: {:?}, want {want:?}",
        pressed.rgb(lx, ly)
    );
    // Released away from it: no click.
    window.dispatch_event(WindowEvent::PointerMoved { position: away });
    window.dispatch_event(WindowEvent::PointerReleased {
        position: away,
        button: PointerEventButton::Left,
    });
    assert!(
        !h.ui.sent().iter().any(|c| c == "OlderDays"),
        "{:?}",
        h.ui.sent()
    );

    // The right arrow is off on the page that ends today: hovering and pressing it draw nothing under it.
    window.dispatch_event(WindowEvent::PointerMoved { position: right });
    window.dispatch_event(WindowEvent::PointerPressed {
        position: right,
        button: PointerEventButton::Left,
    });
    let off = h.snapshot();
    assert!(
        off.distance(rx, ry, surface) <= 1,
        "off: {:?}",
        off.rgb(rx, ry)
    );
    window.dispatch_event(WindowEvent::PointerReleased {
        position: right,
        button: PointerEventButton::Left,
    });
    assert!(
        !h.ui.sent().iter().any(|c| c == "NewerDays"),
        "{:?}",
        h.ui.sent()
    );
}
