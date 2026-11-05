//! Frames per clock second (PLAN.md 10.2, CPU with a task running). Each second of a running task must draw one
//! frame and leave nothing to draw after it. A second frame came from the ticker's text sitting in the rail's layout:
//! the rail's scroll container then marked its animated parts dirty every second, their no-op restart read the
//! animation clock, and the next animation tick asked for one more full-window frame (node CPU of the m1-resume run).
//!
//! Runs on Slint's `MinimalSoftwareWindow`, whose `draw_if_needed` says whether a frame was due, with the same
//! headless app as the other UI tests (tests/common).
#![allow(clippy::unwrap_used, reason = "a failed setup is a failed test")]

mod common;

use std::rc::Rc;
use std::time::Duration;

use common::{Harness, OneWindow};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::{LogicalSize, PhysicalSize};

const WIDTH: u32 = 1360;
const HEIGHT: u32 = 900;

/// Draws a frame if one is due; says whether it drew.
fn draw(window: &MinimalSoftwareWindow, buffer: &mut [Rgb565Pixel]) -> bool {
    window.draw_if_needed(|renderer| {
        renderer.render(buffer, WIDTH as usize);
    })
}

/// One display frame later (16 ms of mock time): animations and timers move on, as at the next vsync.
fn next_vsync() {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
}

#[test]
fn a_clock_second_draws_one_frame_and_nothing_after() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    window.set_size(PhysicalSize::new(WIDTH, HEIGHT));
    slint::platform::set_platform(Box::new(OneWindow(Rc::clone(&window)))).unwrap();
    let mut h = Harness::open(LogicalSize::new(WIDTH as f32, HEIGHT as f32));
    let mut buffer = vec![Rgb565Pixel::default(); (WIDTH * HEIGHT) as usize];
    assert!(
        h.ui.clock().is_running(),
        "the sample's task runs, so the clock ticks"
    );

    // Let the start settle: the first frames, the start notice, its animations.
    for _ in 0..3 {
        h.second();
        for _ in 0..30 {
            draw(&window, &mut buffer);
            next_vsync();
        }
    }

    for second in 0..5 {
        h.second();
        assert!(
            draw(&window, &mut buffer),
            "second {second}: the ticker changed, so a frame is due"
        );
        next_vsync();
        assert!(
            !draw(&window, &mut buffer),
            "second {second}: a second frame was due after the clock's frame"
        );
    }
}
