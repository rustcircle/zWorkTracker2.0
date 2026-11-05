//! The headless harness itself: the software renderer draws the real window, and the 1 s clock keeps to whole seconds
//! of the time it reads under Slint's mock time.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use common::{FakeTime, Harness, TestResult, write_png};
use slint::{ComponentHandle, LogicalSize, Rgba8Pixel};
use zwt_app::clock::Clock1s;

/// Mid-second start: the first shot comes at the next whole second, then one per second; the minute boundary sends
/// one minute tick; stopping silences it.
#[test]
fn the_clock_fires_on_whole_seconds_and_stops_with_the_task() {
    i_slint_backend_testing::init_no_event_loop();
    let ms = Rc::new(Cell::new(59_000 - 400));
    let seconds: Rc<RefCell<Vec<i64>>> = Rc::default();
    let minutes = Rc::new(Cell::new(0));
    let clock = {
        let seconds = Rc::clone(&seconds);
        let minutes = Rc::clone(&minutes);
        Clock1s::new(
            Rc::new(FakeTime(Rc::clone(&ms))),
            move |s| seconds.borrow_mut().push(s),
            move || minutes.set(minutes.get() + 1),
        )
    };
    let advance = |by: i64| {
        ms.set(ms.get() + by);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(
            u64::try_from(by).unwrap(),
        ));
    };

    clock.follow(true);
    assert_eq!(
        *seconds.borrow(),
        [58],
        "starting writes the current second"
    );
    advance(399);
    assert_eq!(clock.shots(), 0, "not yet on the whole second");
    advance(1);
    assert_eq!(
        *seconds.borrow(),
        [58, 59],
        "the first shot is on the whole second"
    );
    advance(1000);
    assert_eq!(*seconds.borrow(), [58, 59, 60]);
    assert_eq!(minutes.get(), 1, "59 -> 60 crossed a minute");
    for _ in 0..3 {
        advance(1000);
    }
    assert_eq!(*seconds.borrow(), [58, 59, 60, 61, 62, 63]);
    assert_eq!(minutes.get(), 1);
    clock.follow(true);
    assert_eq!(
        seconds.borrow().len(),
        6,
        "following a running task again changes nothing"
    );

    clock.follow(false);
    assert!(!clock.is_running());
    for _ in 0..3 {
        advance(1000);
    }
    assert_eq!(seconds.borrow().len(), 6, "a stopped clock stays quiet");
}

const WIDTH: u32 = 1360;
const HEIGHT: u32 = 900;
/// Brand refresh, the sample's first project (`#2563eb`): its slice and blocks.
const BRAND: Rgba8Pixel = Rgba8Pixel {
    r: 0x25,
    g: 0x63,
    b: 0xeb,
    a: 0xff,
};

/// Renders the Timeline with the software renderer. Set `ZWT_RENDER_PNG=<file.png>` to also save the frame.
#[test]
fn the_software_renderer_draws_the_timeline() -> TestResult {
    let h = Harness::rendered(LogicalSize::new(1360.0, 900.0));
    let frame = h.app.window().take_snapshot()?;
    assert_eq!((frame.width(), frame.height()), (WIDTH, HEIGHT));
    let pixels = frame.as_slice();
    let brand = pixels.iter().filter(|p| **p == BRAND).count();
    // The Brand refresh slice of the donut alone covers several thousand pixels.
    assert!(brand > 4_000, "Brand refresh pixels: {brand}");
    let ink = pixels
        .iter()
        .filter(|p| p.r < 0x60 && p.g < 0x60 && p.b < 0x60)
        .count();
    assert!(ink > 3_000, "dark text pixels: {ink}");

    if let Some(path) = std::env::var_os("ZWT_RENDER_PNG") {
        write_png(
            Path::new(&path),
            frame.width(),
            frame.height(),
            frame.as_bytes(),
        )?;
    }
    Ok(())
}
