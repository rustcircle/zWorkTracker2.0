//! V2-SS1 of the visual review (docs/visual/REVIEW.md): a legend row of the Share of time card writes its name and
//! what follows it (the task and project of a subtask, muted) as one line, as 1.x does (charts.tsx `ShareLegend`,
//! analysis.css `.a-share-name`): a row too narrow for all of it cuts the end of that line, once, so what follows the
//! name goes first and the name stays whole. Before, the name and each name after it were texts that each cut
//! themselves short ("Colour … · Logo con… · Brand re…").
//!
//! A text that is cut is narrower than the text needs. What it needs is measured on the same screen: the same rows with
//! nothing after their names, where a name has its whole column.
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
use slint::platform::PointerEventButton;
use slint::{LogicalSize, Model, ModelRc, VecModel};
use zwt_app::ShareRowData;

/// The legend row of `row` on screen.
fn row_on_screen(h: &Harness, row: &ShareRowData) -> ElementHandle {
    h.find(
        AccessibleRole::ListItem,
        &format!("{}, {}", row.spoken, row.text),
    )
}

/// The texts of a legend row before its figures, left to right: the name, then what follows it.
fn name_texts(h: &Harness, row: &ShareRowData) -> Vec<ElementHandle> {
    let mut texts: Vec<ElementHandle> = row_on_screen(h, row)
        .query_descendants()
        .match_accessible_role(AccessibleRole::Text)
        .find_all()
        .into_iter()
        .filter(|t| {
            t.accessible_label()
                .is_some_and(|l| l != row.hm.as_str() && l != row.pct.as_str())
        })
        .collect();
    texts.sort_by(|a, b| a.absolute_position().x.total_cmp(&b.absolute_position().x));
    assert_eq!(
        texts[0].accessible_label().as_deref(),
        Some(row.label.as_str()),
        "the name comes first"
    );
    texts
}

/// The Share card shows `level` (Projects, Tasks, Subtasks, Tags).
fn show_level(h: &mut Harness, level: &str) {
    h.find(AccessibleRole::RadioButton, level)
        .mock_single_click(PointerEventButton::Left);
    h.pump();
}

/// The width each row's name has with nothing after it: the rows drawn again without their context, then as they were.
fn names_alone(h: &Harness, rows: &[ShareRowData]) -> Vec<f32> {
    let shown = h.state().get_share_rows();
    let bare: Vec<ShareRowData> = rows
        .iter()
        .map(|r| ShareRowData {
            context: Default::default(),
            ..r.clone()
        })
        .collect();
    h.state()
        .set_share_rows(ModelRc::new(VecModel::from(bare.clone())));
    let widths = bare
        .iter()
        .map(|r| {
            let texts = name_texts(h, r);
            assert_eq!(texts.len(), 1, "{:?} alone", r.label);
            rect_of(&texts[0]).2
        })
        .collect();
    h.state().set_share_rows(shown);
    widths
}

/// What is wrong with the rows of `rows` on screen: a name drawn narrower than it is alone while something after it is
/// still written, what follows a name in more than one text (each cut on its own), or a gap between the two.
fn faults(h: &Harness, rows: &[ShareRowData], level: &str) -> Vec<String> {
    let alone = names_alone(h, rows);
    let mut faults = Vec::new();
    for (row, alone) in rows.iter().zip(alone) {
        let texts = name_texts(h, row);
        let (nx, _, nw, _) = rect_of(&texts[0]);
        let whole = (nw - alone).abs() <= 0.01;
        if texts.len() > 2 {
            faults.push(format!(
                "{level}, {:?}: what follows the name is {} texts, each cut on its own: {:?}",
                row.label,
                texts.len() - 1,
                texts
                    .iter()
                    .map(|t| t.accessible_label().unwrap_or_default().to_string())
                    .collect::<Vec<_>>()
            ));
        }
        let Some(rest) = texts.get(1) else {
            if !whole {
                faults.push(format!(
                    "{level}, {:?}: {nw:.1} px of the {alone:.1} it needs, alone",
                    row.label
                ));
            }
            continue;
        };
        let (rx, _, rw, _) = rect_of(rest);
        if (rx - (nx + nw)).abs() >= 0.01 {
            faults.push(format!(
                "{level}, {:?}: what follows starts {:.1} px after the name ends",
                row.label,
                rx - (nx + nw)
            ));
        }
        if !whole && rw > 0.0 {
            faults.push(format!(
                "{level}, {:?}: the name is cut ({nw:.1} px of the {alone:.1} it needs) while {rw:.1} px are left \
                 for what follows",
                row.label
            ));
        }
    }
    faults
}

/// The Subtasks level of the sample in the 1,360 x 900 window of the screenshots, where 6 of its 9 rows hold more than
/// their column: no name is cut while something after it is still written. Each keeps the width it has alone, what
/// follows it starts where it ends (no gap before the " · "), and what follows is one text, so a row is cut once, at
/// the end of its line.
#[test]
fn a_subtask_keeps_its_name_whole_and_what_follows_is_cut_once_at_the_end() {
    let mut h = Harness::rendered(LogicalSize::new(1360.0, 900.0));
    show_level(&mut h, "Subtasks");
    let rows: Vec<ShareRowData> = h.state().get_share_rows().iter().collect();
    assert!(
        rows.iter().any(|r| r.spoken.contains(", task ")),
        "the sample has a subtask with its task and project after it"
    );
    let found = faults(&h, &rows, "Subtasks");
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// Projects, Tasks and Tags in that window: every name is whole there too (their screenshots stay as they were).
#[test]
fn projects_tasks_and_tags_keep_their_names_whole() {
    let mut h = Harness::rendered(LogicalSize::new(1360.0, 900.0));
    for level in ["Projects", "Tasks", "Tags"] {
        show_level(&mut h, level);
        let rows: Vec<ShareRowData> = h.state().get_share_rows().iter().collect();
        assert!(!rows.is_empty(), "{level}: the sample has rows");
        let found = faults(&h, &rows, level);
        assert!(found.is_empty(), "{}", found.join("\n"));
    }
}
