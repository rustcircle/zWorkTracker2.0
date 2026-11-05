//! The Timeline screen beyond the acceptance criteria: the day picker, the day axis, the notices (auto-pause and a
//! cold Resume), and the state table of the timer buttons.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use common::{DRAFT, DRAFT_NAME, Harness, RUNNING, RUNNING_NAME};
use i_slint_backend_testing::AccessibleRole;
use slint::Model;
use slint::platform::PointerEventButton;

/// The four buttons of a task card, on or off: Start, Pause, Resume, Stop.
fn buttons(h: &Harness, task_name: &str) -> [bool; 4] {
    ["Start", "Pause", "Resume", "Stop"]
        .map(|label| h.button(task_name, label).accessible_enabled() == Some(true))
}

/// PARITY 81: Draft Start; Running Pause and Stop; Paused Resume and Stop; Stopped none.
#[test]
fn the_timer_buttons_follow_the_state_table() {
    let mut h = Harness::new();
    assert_eq!(
        buttons(&h, DRAFT_NAME),
        [true, false, false, false],
        "draft"
    );
    assert_eq!(
        buttons(&h, RUNNING_NAME),
        [false, true, false, true],
        "running"
    );
    assert_eq!(
        buttons(&h, "Logo concepts"),
        [false, false, true, true],
        "paused (warm launch)"
    );
    assert_eq!(
        buttons(&h, "Homepage layout"),
        [false, false, false, false],
        "stopped"
    );
    h.button(RUNNING_NAME, "Stop")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    assert_eq!(
        buttons(&h, RUNNING_NAME),
        [false, false, false, false],
        "stopped is final"
    );
}

/// PARITY 91: starting a task pauses the running one, says so on screen and on the paused task's card.
#[test]
fn starting_a_task_pauses_the_running_one_and_says_so() {
    let mut h = Harness::new();
    assert_eq!(h.state().get_notice_text(), "");
    h.button(DRAFT_NAME, "Start")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    assert_eq!(h.task(DRAFT).state, 1);
    assert_eq!(h.task(RUNNING).state, 2);
    assert_eq!(h.state().get_running_id(), DRAFT);
    let notice = format!(
        "Paused \"{RUNNING_NAME}\" because you started \"{DRAFT_NAME}\". Only one task is timed at a time."
    );
    assert_eq!(h.state().get_notice_text(), notice.as_str());
    h.find(AccessibleRole::Text, &notice);
    assert_eq!(
        h.task(RUNNING).note,
        format!("Auto-paused because \"{DRAFT_NAME}\" was started.").as_str()
    );
    // The notice goes after eight seconds of the running clock, or when dismissed.
    for _ in 0..8 {
        h.second();
    }
    assert!(
        h.find_all(AccessibleRole::Text, &notice).is_empty(),
        "gone after 8 s"
    );
}

/// A cold launch: Resume is refused by the core (PARITY 86). Until milestone 2 brings the form of a new linked task,
/// the app says why in a notice that does not block anything.
#[test]
fn a_cold_resume_is_refused_with_a_notice_that_blocks_nothing() {
    let h = Harness::new();
    let mut h = h.relaunch();
    assert_eq!(h.state().get_running_id(), "", "a cold launch runs nothing");
    assert!(!h.ui.clock().is_running());
    let paused = h.task(RUNNING);
    assert_eq!(paused.state, 2, "1.x Quit paused it");
    assert!(paused.resume_on, "Resume stays on, as in 1.x");
    assert_ne!(paused.cold_note, "");

    h.button(RUNNING_NAME, "Resume")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    let cold = "After a restart, Resume asks for every field again and starts a new task that continues this one. This task stays as it is.";
    assert_eq!(h.state().get_notice_text(), cold);
    assert_eq!(h.task(RUNNING).state, 2, "still paused");

    // Nothing is blocked: the draft starts, and the notice can be dismissed.
    h.button(DRAFT_NAME, "Start")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    assert_eq!(h.task(DRAFT).state, 1);
    h.find(AccessibleRole::Button, "Dismiss message")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(h.state().get_notice_text(), "", "dismissed");
    assert!(h.find_all(AccessibleRole::Text, cold).is_empty());
}

/// Picking a day sends SelectDay; the day's axis shows that day, the chips are not rewritten.
#[test]
fn the_day_picker_shows_the_picked_day() {
    let mut h = Harness::new();
    let chips = h.state().get_chips();
    assert_eq!(chips.row_count(), 14, "two weeks of chips");
    // The past to the future (owner, 2026-11-04): the oldest day first, today last.
    assert_eq!(
        chips.row_data(0).unwrap().date,
        "2026-10-22",
        "oldest first"
    );
    assert_eq!(chips.row_data(13).unwrap().date, "2026-11-04", "today last");
    assert_eq!(h.state().get_day_date(), "2026-11-04");
    assert_eq!(h.state().get_day_label(), "Today, Wed 4 Nov 2026");
    let today = h.find(AccessibleRole::RadioButton, "Wed 4 Nov 2026");
    assert_eq!(today.accessible_checked(), Some(true));
    let open_today = h.state().get_pieces().iter().filter(|p| p.open).count();
    assert_eq!(open_today, 1, "today holds the running piece");

    let chip_writes = h.ui.writes().chips;
    h.find(AccessibleRole::RadioButton, "Mon 2 Nov 2026")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    assert_eq!(h.ui.sent().last().unwrap(), "SelectDay(2026-11-02)");
    assert_eq!(h.state().get_day_date(), "2026-11-02");
    assert_eq!(h.state().get_day_label(), "Mon 2 Nov 2026");
    assert_eq!(
        h.find(AccessibleRole::RadioButton, "Mon 2 Nov 2026")
            .accessible_checked(),
        Some(true)
    );
    assert_eq!(
        h.find(AccessibleRole::RadioButton, "Wed 4 Nov 2026")
            .accessible_checked(),
        Some(false)
    );
    assert_eq!(
        h.ui.writes().chips,
        chip_writes,
        "the chips stay as they are"
    );
    let pieces = h.state().get_pieces();
    assert!(pieces.row_count() > 0, "Monday has logged time");
    let start = h.state().get_day_start();
    let end = h.state().get_day_end();
    assert_eq!(end - start, 86_400);
    assert!(
        pieces
            .iter()
            .all(|p| !p.open && p.start >= start && p.end <= end)
    );
}
