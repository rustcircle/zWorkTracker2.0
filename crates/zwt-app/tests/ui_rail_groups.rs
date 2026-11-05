//! The rail's groups as 1.x draws them (owner, 2026-11-05: the task list stays as in 1.x, "the running on top").
//! TaskRail.tsx lists Active, Drafts and Stopped, each a section named by its title under a heading with the title and
//! how many cards it lists (shell.css `.rail-group-title`, drawn in capitals), 14 px apart; a group with no task is not
//! drawn at all; the Stopped group shows 8 cards and "Show all N stopped tasks", then all and "Show fewer". Inside
//! each group the cards follow the owner's order (zwt-bridge `task_rows`; tests/order.rs there): the running task
//! first, then by last run, the tasks never run last.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use common::{DRAFT, DRAFT_NAME, Harness, RUNNING_NAME, encloses, rect_of};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::Model;
use slint::platform::PointerEventButton;
use z_work_core::machine::TaskInput;

const EMPTY_NOTE: &str = "No tasks yet. Use New task to add one, then press Start to begin timing.";
const SAMPLE_STOPPED: [&str; 4] = [
    "Homepage layout",
    "Box dieline",
    "Supplier call notes",
    "Weekly planning",
];

/// The rail's section named `title` ("Active", "Drafts", "Stopped"), if it is drawn.
fn section(h: &Harness, title: &str) -> Option<ElementHandle> {
    let wanted = title.to_owned();
    h.find(AccessibleRole::Groupbox, "Tasks")
        .query_descendants()
        .match_accessible_role(AccessibleRole::Groupbox)
        .match_predicate(move |e| e.accessible_label().is_some_and(|l| l == wanted.as_str()))
        .find_first()
}

/// The task cards of a section, top to bottom.
fn card_elements(section: &ElementHandle) -> Vec<ElementHandle> {
    let mut found = section
        .query_descendants()
        .match_inherits("TaskCard")
        .find_all();
    found.sort_by(|a, b| a.absolute_position().y.total_cmp(&b.absolute_position().y));
    found
}

/// The names of a section's cards, top to bottom.
fn cards(section: &ElementHandle) -> Vec<String> {
    card_elements(section)
        .iter()
        .map(|c| c.accessible_label().unwrap_or_default().to_string())
        .collect()
}

/// The vertical gap from the bottom of `upper` to the top of `lower`.
fn gap(upper: &ElementHandle, lower: &ElementHandle) -> f32 {
    let (_, uy, _, uh) = rect_of(upper);
    rect_of(lower).1 - (uy + uh)
}

/// The sample once open: one running and four paused tasks under Active, one draft, four stopped.
#[test]
fn the_groups_have_their_1x_headings_in_the_1x_order() {
    let h = Harness::new();
    let active = section(&h, "Active").expect("an Active section");
    let drafts = section(&h, "Drafts").expect("a Drafts section");
    let stopped = section(&h, "Stopped").expect("a Stopped section");
    // shell.css `.rail-scroll { gap: 14px }`: Active, then Drafts, then Stopped, 14 px apart.
    assert_eq!(gap(&active, &drafts), 14.0);
    assert_eq!(gap(&drafts, &stopped), 14.0);
    // TaskRail.tsx: the heading is `{title} <span>{tasks.length}</span>`.
    for (group, label, n) in [
        (&active, "Active 5", 5),
        (&drafts, "Drafts 1", 1),
        (&stopped, "Stopped 4", 4),
    ] {
        let heading = h.find(AccessibleRole::Text, label);
        assert!(encloses(group, &heading), "{label} is in its section");
        let cards = card_elements(group);
        assert_eq!(cards.len(), n, "{label}");
        assert!(cards.iter().all(|c| encloses(group, c)));
        // shell.css `.rail-group-title { margin: 0 2px 6px }`: on top of the section, 6 px above its first card, 2 px
        // in from the cards' left edge; the heading is 12 px x 1.45 on whole pixels.
        let (hx, hy, _, hh) = rect_of(&heading);
        assert_eq!(hy, rect_of(group).1, "{label} on top");
        assert_eq!(hh, 17.0, "{label}");
        assert_eq!(gap(&heading, &cards[0]), 6.0, "{label}");
        assert_eq!(hx - rect_of(&cards[0]).0, 2.0, "{label}");
        // `.task-list { gap: 8px }`, and the section ends with its last card.
        for pair in cards.windows(2) {
            assert_eq!(gap(&pair[0], &pair[1]), 8.0, "{label}");
        }
        let (_, gy, _, gh) = rect_of(group);
        let (_, ly, _, lh) = rect_of(&cards[n - 1]);
        assert_eq!(ly + lh, gy + gh, "{label}");
    }
    assert!(
        h.find_all(AccessibleRole::Button, "Show all 4 stopped tasks")
            .is_empty(),
        "4 stopped tasks: one page, no link"
    );
}

/// Inside each group, the owner's order: the running task first; then by when each task last ran, the most recent
/// first (the sample: paused today 10:25 and 07:55, 3 Nov, 2 Nov; stopped today 09:35 and 08:55, 3 Nov, 2 Nov).
#[test]
fn each_group_lists_its_cards_in_the_order_rule() {
    let h = Harness::new();
    assert_eq!(
        cards(&section(&h, "Active").unwrap()),
        [
            RUNNING_NAME,
            "Logo concepts",
            "Label artwork",
            "Blog redesign",
            "Brand guidelines"
        ]
    );
    assert_eq!(cards(&section(&h, "Drafts").unwrap()), [DRAFT_NAME]);
    assert_eq!(cards(&section(&h, "Stopped").unwrap()), SAMPLE_STOPPED);
}

/// The task that starts goes to the top of Active, the one it paused right under it; the Drafts group, left with no
/// task, is not drawn at all (1.x draws a group only with tasks: no heading, no empty list). An empty store draws no
/// group, only the rail's note.
#[test]
fn the_running_task_is_first_and_an_empty_group_is_not_drawn() {
    let mut h = Harness::new();
    h.button(DRAFT_NAME, "Start")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    assert_eq!(h.task(DRAFT).state, 1, "the draft runs");
    let active = section(&h, "Active").unwrap();
    assert_eq!(
        &cards(&active)[..2],
        [DRAFT_NAME, RUNNING_NAME],
        "the running task, then the task it paused"
    );
    h.find(AccessibleRole::Text, "Active 6");
    assert!(section(&h, "Drafts").is_none(), "no Drafts section");
    assert!(
        h.find_all(AccessibleRole::Text, "Drafts 0").is_empty(),
        "no Drafts heading"
    );
    assert_eq!(h.state().get_draft_tasks().row_count(), 0);
    assert_eq!(
        gap(&active, &section(&h, "Stopped").unwrap()),
        14.0,
        "the sections left are 14 px apart"
    );
    drop(h);

    let empty = Harness::open_empty(slint::LogicalSize::new(1360.0, 3600.0));
    for title in ["Active", "Drafts", "Stopped"] {
        assert!(section(&empty, title).is_none(), "{title}");
    }
    empty.find(AccessibleRole::Text, EMPTY_NOTE);
}

/// Creates, runs and stops 8 tasks, a minute each, "Wrap-up 8" last: 12 stopped tasks, the new ones run last.
fn twelve_stopped(h: &mut Harness) {
    let names: Vec<String> = (1..=8).map(|i| format!("Wrap-up {i}")).collect();
    stop_new(h, &names, &[]);
}

/// Creates, runs and stops a task per name, a minute each, the last one last; the first gets `first_tags`.
fn stop_new(h: &mut Harness, names: &[String], first_tags: &[&str]) {
    for (i, name) in names.iter().enumerate() {
        let name = name.clone();
        let create = name.clone();
        let tags: Vec<String> = if i == 0 {
            first_tags.iter().map(|t| (*t).to_owned()).collect()
        } else {
            Vec::new()
        };
        h.mutate("run a new task", move |svc| {
            let id = svc.create_task(&TaskInput {
                name: create,
                project_id: "p-brand".to_owned(),
                description: Default::default(),
                continued_from: None,
                tags,
            })?;
            svc.start_task(&id).map(|_| ())
        });
        h.clock.advance(60);
        h.mutate("stop it", move |svc| {
            let id = svc
                .file()
                .data
                .tasks
                .values()
                .find(|t| t.name == name)
                .map(|t| t.id.clone())
                .unwrap_or_default();
            svc.stop_task(&id)
        });
    }
}

/// 1.x `STOPPED_PAGE`: the 8 most recently run stopped cards, the heading counting the cards it lists ("Stopped 8"),
/// and right under the last card "Show all 12 stopped tasks"; it lists all 12 and reads "Show fewer", which lists the
/// first 8 again. The link works from the keyboard too (a 1.x <button>).
#[test]
fn the_stopped_group_shows_8_then_all_then_8() {
    let mut h = Harness::new();
    twelve_stopped(&mut h);
    let newest: Vec<String> = (1..=8).rev().map(|i| format!("Wrap-up {i}")).collect();

    let stopped = section(&h, "Stopped").unwrap();
    assert_eq!(cards(&stopped), newest, "the 8 most recently run");
    h.find(AccessibleRole::Text, "Stopped 8");
    let more = h.find(AccessibleRole::Button, "Show all 12 stopped tasks");
    assert!(
        encloses(&stopped, &more),
        "the link is in the Stopped section"
    );
    let last = card_elements(&stopped).pop().unwrap();
    assert_eq!(gap(&last, &more), 0.0, "right under the last card");
    assert_eq!(
        rect_of(&more).0,
        rect_of(&last).0,
        "at the cards' left edge"
    );

    more.mock_single_click(PointerEventButton::Left);
    h.pump();
    let all = cards(&section(&h, "Stopped").unwrap());
    assert_eq!(all.len(), 12);
    assert_eq!(&all[..8], newest.as_slice());
    assert_eq!(&all[8..], SAMPLE_STOPPED, "then the sample's, by last run");
    h.find(AccessibleRole::Text, "Stopped 12");
    let fewer = h.find(AccessibleRole::Button, "Show fewer");
    let last = card_elements(&section(&h, "Stopped").unwrap())
        .pop()
        .unwrap();
    assert_eq!(gap(&last, &fewer), 0.0, "right under the last card");
    assert!(
        h.find_all(AccessibleRole::Button, "Show all 12 stopped tasks")
            .is_empty()
    );

    h.find(AccessibleRole::Button, "Show fewer")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    assert_eq!(cards(&section(&h, "Stopped").unwrap()), newest);
    h.find(AccessibleRole::Text, "Stopped 8");

    // Enter on the focused link, as on any 1.x button.
    let more = h.find(AccessibleRole::Button, "Show all 12 stopped tasks");
    h.focus(&more);
    h.enter();
    h.pump();
    assert_eq!(cards(&section(&h, "Stopped").unwrap()).len(), 12);
    h.find(AccessibleRole::Button, "Show fewer");
}

/// A name of two lines in the rail's cards.
const LONG_NAME: &str =
    "Packaging proofs for the printer, second round, with the corrected dieline and the new label";

/// A card whose name and chips wrap at the rail's width (two lines of name; the project, the time and four tags on two
/// rows) is measured at that width: it holds every line and chip, the cards stay 8 px apart and on whole pixels, and
/// the link stays right under the last card, the wrapping one.
#[test]
fn a_card_that_wraps_is_measured_at_the_rails_width() {
    let mut h = Harness::new();
    let mut names = vec![LONG_NAME.to_owned()];
    names.extend((2..=8).map(|i| format!("Wrap-up {i}")));
    stop_new(
        &mut h,
        &names,
        &[
            "tag-concept",
            "tag-revision",
            "tag-client-call",
            "tag-admin",
        ],
    );

    for all in [false, true] {
        let stopped = section(&h, "Stopped").unwrap();
        let cards = card_elements(&stopped);
        assert_eq!(cards.len(), if all { 12 } else { 8 });
        let long = &cards[7];
        assert_eq!(
            long.accessible_label().unwrap_or_default(),
            LONG_NAME,
            "run first of the new tasks, so last of the page"
        );
        let name = long
            .query_descendants()
            .match_accessible_role(AccessibleRole::Text)
            .match_predicate(|e| e.accessible_label().is_some_and(|l| l == LONG_NAME))
            .find_first()
            .unwrap();
        assert!(
            rect_of(&name).3 > 30.0,
            "the name takes two lines: {:?}",
            rect_of(&name)
        );
        let tags = long
            .query_descendants()
            .match_inherits("TagChip")
            .find_all();
        assert_eq!(tags.len(), 4);
        let rows: Vec<f32> = tags.iter().map(|t| rect_of(t).1).collect();
        assert!(
            rows.iter().any(|y| *y > rows[0]),
            "the chips wrap onto a second row: {rows:?}"
        );
        for part in tags.iter().chain([&name]) {
            assert!(
                encloses(long, part),
                "the card holds {:?}",
                part.accessible_label()
            );
        }
        let controls = long
            .query_descendants()
            .match_inherits("TimerButton")
            .find_all();
        assert!(controls.iter().all(|b| encloses(long, b)));
        for pair in cards.windows(2) {
            assert_eq!(gap(&pair[0], &pair[1]), 8.0, "cards 8 px apart");
            assert_eq!(rect_of(&pair[1]).1.fract(), 0.0, "on whole pixels");
        }
        let link = h.find(
            AccessibleRole::Button,
            if all {
                "Show fewer"
            } else {
                "Show all 12 stopped tasks"
            },
        );
        let last = cards.last().unwrap();
        assert_eq!(gap(last, &link), 0.0, "the link right under the last card");
        if !all {
            link.mock_single_click(PointerEventButton::Left);
            h.pump();
        }
    }
}
