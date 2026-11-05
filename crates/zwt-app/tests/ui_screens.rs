//! Screenshots for the visual comparison with 1.x (PLAN.md 7.2): the software renderer under the testing backend, at
//! the Tauri e2e viewport (1360 x 900 logical, scale 1), from a fresh sample store on a fixed clock. Ignored by default;
//! run it with a folder to write into (docs/visual/README.md):
//!
//! `ZWT_SCREENSHOTS=docs/visual cargo test -p zwt-app --test ui_screens -- --ignored`
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use std::path::{Path, PathBuf};

use common::{Harness, TestResult, write_png};
use i_slint_backend_testing::AccessibleRole;
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, LogicalSize, Rgba8Pixel, SharedPixelBuffer};
use z_work_core::Ts;
use z_work_core::adjust::ManualEntryInput;

/// How long the sample's task has run in the screenshots: 1:19:59, as in 1.x's timeline screenshots.
const RUN_SECS: u32 = 4799;

fn save(frame: &SharedPixelBuffer<Rgba8Pixel>, path: &Path) -> TestResult {
    write_png(path, frame.width(), frame.height(), frame.as_bytes())?;
    eprintln!("wrote {}", path.display());
    Ok(())
}

/// The part of `frame` at `x, y` of `width x height` (logical = physical pixels at scale 1).
fn crop(
    frame: &SharedPixelBuffer<Rgba8Pixel>,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    path: &Path,
) -> TestResult {
    let width = width.min(frame.width().saturating_sub(x));
    let height = height.min(frame.height().saturating_sub(y));
    let stride = frame.width() as usize * 4;
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    let bytes = frame.as_bytes();
    for row in y..y + height {
        let start = row as usize * stride + x as usize * 4;
        out.extend_from_slice(&bytes[start..start + width as usize * 4]);
    }
    write_png(path, width, height, &out)?;
    eprintln!("wrote {}", path.display());
    Ok(())
}

#[test]
#[ignore = "writes PNG files: ZWT_SCREENSHOTS=<folder> cargo test -p zwt-app --test ui_screens -- --ignored"]
fn screenshots_for_the_visual_comparison() -> TestResult {
    let Some(out) = std::env::var_os("ZWT_SCREENSHOTS").map(PathBuf::from) else {
        eprintln!("ZWT_SCREENSHOTS is not set: nothing written");
        return Ok(());
    };
    // Tests run in the crate's folder: a relative folder is taken from the workspace root.
    let out = if out.is_relative() {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(out)
    } else {
        out
    };
    std::fs::create_dir_all(&out)?;
    let mut h = Harness::rendered(LogicalSize::new(1360.0, 900.0));
    // The running task has run 1:19:59 (minute ticks included, as the app sends them).
    for _ in 0..RUN_SECS {
        h.second();
    }
    // Close the notices of the start, if any, so the frame shows the screen only.
    h.state().set_notice_text("".into());

    for (theme, name) in [("light", "light"), ("solarizedDark", "solarized-dark")] {
        zwt_app::set_theme(&h.app, theme);
        let frame = h.app.window().take_snapshot()?;
        save(&frame, &out.join(format!("timeline-{name}-slint.png")))?;
    }
    zwt_app::set_theme(&h.app, "light");

    // The narrowest window (app.slint `min-width`), where the day picker's chips are compact (review V2-DP1); then the
    // screenshots' window again (the day axis follows its width before the next frame, as in the app).
    h.app.window().set_size(LogicalSize::new(960.0, 900.0));
    h.settle();
    let frame = h.app.window().take_snapshot()?;
    save(&frame, &out.join("timeline-narrow-light-slint.png"))?;
    h.app.window().set_size(LogicalSize::new(1360.0, 900.0));
    h.settle();

    // The Share of time card at each level, cropped to the card (1.x share-light-*.png are the card).
    for (level, name) in [
        ("Projects", "projects"),
        ("Tasks", "tasks"),
        ("Subtasks", "subtasks"),
        ("Tags", "tags"),
    ] {
        h.find(AccessibleRole::RadioButton, level)
            .mock_single_click(PointerEventButton::Left);
        h.pump();
        let card = h.find(AccessibleRole::Groupbox, "Share of time");
        let at = card.absolute_position();
        let size = card.size();
        let frame = h.app.window().take_snapshot()?;
        crop(
            &frame,
            at.x.max(0.0) as u32,
            at.y.max(0.0) as u32,
            size.width.ceil() as u32,
            size.height.ceil() as u32,
            &out.join(format!("share-light-{name}-slint.png")),
        )?;
    }
    h.find(AccessibleRole::RadioButton, "Projects")
        .mock_single_click(PointerEventButton::Left);
    h.pump();

    h.find(AccessibleRole::Tab, "About")
        .mock_single_click(PointerEventButton::Left);
    let frame = h.app.window().take_snapshot()?;
    save(&frame, &out.join("about-light-slint.png"))?;

    // Time before the day picker's first page (15 October, on a task below the rail's fold): the left arrow comes on,
    // the right one stays off on the page that ends today. Built here; the sample stays as it is.
    h.find(AccessibleRole::Tab, "Timeline")
        .mock_single_click(PointerEventButton::Left);
    h.mutate("time before the page", |svc| {
        svc.add_manual_entry(&ManualEntryInput {
            task_id: OLDER_TASK.to_owned(),
            start: Ts::parse("2026-10-15T09:00:00Z").expect("a time"),
            end: Ts::parse("2026-10-15T10:00:00Z").expect("a time"),
            reason: "Forgot to start the timer".to_owned(),
            subtask_id: None,
            allow_overlap: false,
        })
        .map(|_| ())
    });
    h.settle();
    assert!(arrow_on(&h, "Previous days") && !arrow_on(&h, "Next days"));
    let frame = h.app.window().take_snapshot()?;
    save(&frame, &out.join("timeline-older-light-slint.png"))?;
    drop(h);

    // An empty store: the same page of days, axis, hour labels and grid, with no block (owner, 2026-11-04).
    let h = Harness::open_empty(LogicalSize::new(1360.0, 900.0));
    h.state().set_notice_text("".into());
    let frame = h.app.window().take_snapshot()?;
    save(&frame, &out.join("timeline-empty-light-slint.png"))?;
    Ok(())
}

/// "Weekly planning", a stopped task of the sample listed below the fold of a 900 px window: an hour logged on it on
/// 15 October moves no card and changes none on screen.
const OLDER_TASK: &str = "t-plan";

fn arrow_on(h: &Harness, label: &str) -> bool {
    h.find(AccessibleRole::Button, label).accessible_enabled() == Some(true)
}
