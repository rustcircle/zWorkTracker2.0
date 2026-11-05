//! The shell: Timeline and About in the tab bar, the About screen (AboutSlint, version, MIT line, third-party notices),
//! the refusal to start without a sample folder, and the running clock's format against zwt-ui-logic.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use common::Harness;
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::ComponentHandle;
use slint::platform::PointerEventButton;
use zwt_app::launch::{self, REFUSED};
use zwt_app::{AppWindow, Fmt, RefusalWindow};
use zwt_bridge::datadir::Launch;
use zwt_bridge::format::ticker;

#[test]
fn navigation_reaches_about_and_shows_about_slint_and_the_notices() {
    let h = Harness::new();
    let about_slint =
        |h: &Harness| ElementHandle::find_by_element_type_name(&h.app, "AboutSlint").count();
    assert_eq!(h.app.get_tab(), 0, "the Timeline shows first");
    assert_eq!(about_slint(&h), 0);
    assert!(
        h.find(AccessibleRole::Tab, "Timeline")
            .accessible_item_selected()
            == Some(true)
    );

    h.find(AccessibleRole::Tab, "About")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(h.app.get_tab(), 1);
    assert_eq!(about_slint(&h), 1, "About shows Slint's AboutSlint");
    assert_eq!(
        h.find(AccessibleRole::Tab, "About")
            .accessible_item_selected(),
        Some(true)
    );
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(version, "2.0.0");
    h.find(AccessibleRole::Text, version);
    h.find(
        AccessibleRole::Text,
        "zWorkTracker is MIT licensed (see LICENSE).",
    );

    // The notices open in place, from the embedded file; no file or browser is opened.
    let link = h.find(AccessibleRole::Button, "Third-party notices");
    assert_eq!(link.accessible_expanded(), Some(false));
    link.mock_single_click(PointerEventButton::Left);
    assert_eq!(
        h.find(AccessibleRole::Button, "Third-party notices")
            .accessible_expanded(),
        Some(true)
    );
    let notices = h.find(AccessibleRole::Text, zwt_app::ui::NOTICES);
    assert!(
        notices
            .accessible_label()
            .unwrap()
            .contains("Slint Royalty-free Desktop, Mobile, and Web Applications License 2.0")
    );

    h.find(AccessibleRole::Tab, "Timeline")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(h.app.get_tab(), 0);
    assert_eq!(about_slint(&h), 0);
    h.find(AccessibleRole::Groupbox, "Share of time");
}

#[test]
fn the_app_refuses_to_start_without_a_sample_folder() {
    let launch = Launch {
        args: Vec::new(),
        data_dir_var: None,
        home: None,
        appdata: None,
        xdg_data_home: None,
        cwd: None,
    };
    let refused = launch::data_dir(&launch).unwrap_err();
    assert_eq!(refused.code, REFUSED);
    assert_ne!(refused.code, 0);
    assert_eq!(
        refused.message,
        "zWorkTracker 2.0 milestone 1 runs on sample data only: start it with --sample <folder> or set ZWT_DATA_DIR=<folder>"
    );

    let flag_alone = Launch {
        args: vec!["--sample".into()],
        ..launch
    };
    assert_eq!(launch::data_dir(&flag_alone).unwrap_err().code, REFUSED);
}

#[test]
fn a_sample_folder_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let launch = Launch {
        args: vec!["--sample".into(), dir.path().as_os_str().to_owned()],
        ..Launch::default()
    };
    let found = launch::data_dir(&launch).unwrap();
    assert_eq!(found.path, dir.path().canonicalize().unwrap());
}

#[test]
fn the_refusal_window_shows_the_message() {
    i_slint_backend_testing::init_no_event_loop();
    let window = RefusalWindow::new().unwrap();
    window.set_message("start it with --sample <folder>".into());
    window.show().unwrap();
    let text =
        ElementHandle::find_by_accessible_label(&window, "start it with --sample <folder>").count();
    assert_eq!(text, 1);
}

/// The running clock is formatted in Slint (no Rust call per second): it must read as 1.x's `formatHMS`, which
/// zwt_ui_logic::format::ticker ports.
#[test]
fn the_slint_ticker_matches_zwt_ui_logic() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().unwrap();
    let fmt = app.global::<Fmt>();
    let values = [
        i32::MIN,
        -3600,
        -1,
        0,
        1,
        9,
        10,
        59,
        60,
        61,
        599,
        600,
        3599,
        3600,
        3601,
        3661,
        35_999,
        36_000,
        86_399,
        86_400,
        359_999,
        360_000,
        1_000_000,
        1_793_779_200,
        i32::MAX,
    ];
    for secs in values {
        assert_eq!(
            fmt.invoke_ticker(secs).as_str(),
            ticker(i64::from(secs)),
            "{secs} s"
        );
    }
}
