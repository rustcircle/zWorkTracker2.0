//! Review F1 of the day axis' sub-grid: nothing drawn reads the day's increment (the axis follows it in a `changed`
//! handler, which Slint runs when it draws a frame), so a new increment alone must ask the window for a frame, or the
//! old sub-grid would stay on screen until something else is drawn (up to a minute on an idle day). Slint's
//! `MinimalSoftwareWindow` draws only when asked, so it shows whether the app asked, and what that frame drew.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use std::rc::Rc;

use common::{Harness, OneWindow, rect_of, xs_of};
use slint::LogicalSize;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use z_work_core::machine::RoundingInput;
use z_work_core::model::RoundMethod;

const WIDTH: usize = 1360;
const HEIGHT: usize = 900;

/// The pixels of a frame, if the window asked for one.
fn frame(window: &MinimalSoftwareWindow) -> Option<Vec<PremultipliedRgbaColor>> {
    let mut drawn = None;
    window.draw_if_needed(|renderer| {
        let mut pixels = vec![PremultipliedRgbaColor::default(); WIDTH * HEIGHT];
        renderer.render(&mut pixels, WIDTH);
        drawn = Some(pixels);
    });
    drawn
}

/// Draws until the window asks for nothing more; the last frame drawn.
fn settle(window: &MinimalSoftwareWindow) -> Option<Vec<PremultipliedRgbaColor>> {
    let mut last = None;
    for _ in 0..10 {
        match frame(window) {
            Some(pixels) => last = Some(pixels),
            None => return last,
        }
    }
    panic!("the window keeps asking for frames");
}

/// Whether a line is drawn at `x` on row `y`: darker than the track's white.
fn line_at(pixels: &[PremultipliedRgbaColor], x: f32, y: u32) -> bool {
    pixels[y as usize * WIDTH + x.round() as usize].green < 250
}

#[test]
fn a_new_increment_alone_asks_for_the_frame_that_draws_its_grid() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(OneWindow(Rc::clone(&window)))).unwrap();
    let mut h = Harness::open(LogicalSize::new(WIDTH as f32, HEIGHT as f32));
    let first = settle(&window).expect("the first frame");
    // The first hour from the track's start, at 15 minutes (the default): lines at :15, :30 and :45.
    let hours = xs_of(&h, "HourLine");
    let quarters: Vec<f32> = xs_of(&h, "SubLine")
        .into_iter()
        .filter(|&x| x < hours[0])
        .collect();
    assert_eq!(quarters.len(), 3);
    let (_, y, _, height) = rect_of(&h.of_type("HourLine")[0]);
    let row = (y + height / 2.0) as u32;
    assert!(quarters.iter().all(|&x| line_at(&first, x, row)));

    // A rule of 30 minutes from today: of all the day axis shows, only its increment changes.
    let input = RoundingInput {
        project_id: None,
        effective_from: "2026-11-04".to_owned(),
        increment_minutes: 30,
        method: RoundMethod::Up,
        confirm_retroactive: false,
    };
    h.mutate("rounding", move |svc| {
        svc.set_rounding(&input, "2026-11-04")
    });
    assert_eq!(h.state().get_day_increment(), 30);
    let next = frame(&window).expect("the new increment asked for a frame");
    assert_eq!(
        quarters
            .iter()
            .map(|&x| line_at(&next, x, row))
            .collect::<Vec<_>>(),
        [false, true, false],
        "that frame drew the 30-minute grid: :30 only"
    );
    settle(&window);
}
