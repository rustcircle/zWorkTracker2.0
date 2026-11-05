//! `features/analysis/shareRows.test.ts`: what a row is called below the Projects level (only what the data leaves
//! open), the subtasks and a task's time on none, the tag combinations and the Other row of each level.

use std::collections::BTreeSet;

use zwt_ui_logic::color::{contrast, to_oklch};
use zwt_ui_logic::share::{
    COMBINATION_NOTE, NO_SUBTASK_LABEL, NO_TAG_LABEL, OTHER_COLOR, OtherUnit, PATH_SEPARATOR,
    RowKind, ShareLevel, ShareModel, ShareProject, ShareRow, ShareSubtask, ShareTag, ShareTagTask,
    ShareTask, TAG_SEPARATOR, UNTAGGED_COLOR, other_label, other_note, other_tip, project_shares,
    subtask_shares, tag_key_of, tag_shares, tag_weights_of, task_shares,
};

use crate::fixtures::{color_of, expect_consistent, sum, t};

/// `lib/themes.ts`: (theme, borderStrong, surface) of the four themes.
pub const THEMES: [(&str, &str, &str); 4] = [
    ("light", "#6b7280", "#ffffff"),
    ("solarizedLight", "#657b83", "#fdf6e3"),
    ("solarizedDark", "#839496", "#002b36"),
    ("dark", "#9ca3af", "#212226"),
];

fn s(task_id: &str, project_id: &str, id: Option<&str>, secs: i64) -> ShareSubtask {
    ShareSubtask {
        task_id: task_id.to_owned(),
        task_name: format!("Task {task_id}"),
        project_id: project_id.to_owned(),
        project_name: format!("Project {project_id}"),
        id: id.map(str::to_owned),
        name: id.map_or_else(String::new, |id| format!("Sub {id}")),
        secs,
    }
}

fn context_of(m: &ShareModel) -> Vec<Vec<String>> {
    m.rows.iter().map(|r| r.context.clone()).collect()
}

fn labels(m: &ShareModel) -> Vec<String> {
    m.rows.iter().map(|r| r.label.clone()).collect()
}

fn label_and_context(m: &ShareModel) -> Vec<Vec<String>> {
    crate::fixtures::dotted(&m.rows)
}

// ---- the words after a task's name: only what the filter leaves open

#[test]
fn names_the_project_when_the_tasks_belong_to_several_projects() {
    let m = task_shares(&[t("t1", "A", 600), t("t2", "B", 300)], color_of);
    assert_eq!(context_of(&m), [vec!["Project A"], vec!["Project B"]]);
    assert_eq!(labels(&m), ["Task t1", "Task t2"]);
}

#[test]
fn leaves_the_project_out_when_every_task_belongs_to_one_project() {
    let m = task_shares(&[t("t1", "A", 600), t("t2", "A", 300)], color_of);
    assert_eq!(context_of(&m), [Vec::<String>::new(), Vec::new()]);
    assert_eq!(labels(&m), ["Task t1", "Task t2"]);
}

#[test]
fn keeps_the_whole_path_in_the_title_the_slices_label_and_the_spoken_name() {
    let m = task_shares(&[t("t1", "A", 600)], color_of);
    let row = &m.rows[0];
    assert!(row.context.is_empty());
    assert_eq!(row.path, ["Project A", "Task t1"]);
    assert_eq!(row.full_path, "Project A > Task t1");
    assert_eq!(
        row.spoken_name().as_deref(),
        Some("Task t1, project Project A")
    );
}

#[test]
fn counts_the_projects_of_the_tasks_other_groups() {
    // Eight tasks of A are shown; the two that Other groups belong to B, so the data holds two projects.
    let mut tasks: Vec<ShareTask> = (0..8).map(|i| t(&format!("a{i}"), "A", 1000 - i)).collect();
    tasks.extend([t("b1", "B", 20), t("b2", "B", 10)]);
    let m = task_shares(&tasks, color_of);
    assert_eq!(m.rows.last().unwrap().label, "Other (2 tasks)");
    assert!(
        m.rows[..8]
            .iter()
            .all(|r| r.context.first().map(String::as_str) == Some("Project A"))
    );
    // Whereas ten tasks of one project name none.
    let one: Vec<ShareTask> = (0..10)
        .map(|i| t(&format!("a{i}"), "A", 1000 - i))
        .collect();
    assert!(
        task_shares(&one, color_of)
            .rows
            .iter()
            .all(|r| r.context.is_empty())
    );
}

#[test]
fn names_no_project_on_the_other_row() {
    let tasks: Vec<ShareTask> = (0..10)
        .map(|i| {
            t(
                &format!("t{i}"),
                if i % 2 == 1 { "A" } else { "B" },
                1000 - i,
            )
        })
        .collect();
    let m = task_shares(&tasks, color_of);
    let other = m.rows.last().unwrap();
    assert!(matches!(other.kind, RowKind::Other { .. }));
    assert!(other.context.is_empty());
    assert_eq!(other.path, ["Other (2 tasks)"]);
}

#[test]
fn does_not_touch_the_projects_level() {
    let m = project_shares(&[ShareProject {
        id: "A".into(),
        name: "Alpha".into(),
        color: "#2563eb".into(),
        secs: 600,
    }]);
    let row = &m.rows[0];
    assert_eq!(
        (
            row.label.as_str(),
            row.context.len(),
            row.path.clone(),
            row.full_path.as_str()
        ),
        ("Alpha", 0, vec!["Alpha".to_owned()], "Alpha")
    );
    assert_eq!(row.spoken_name(), None);
}

// ---- the subtask rows

#[test]
fn within_one_task_writes_the_subtasks_name_alone_and_task_no_subtask_for_time_on_none() {
    let m = subtask_shares(
        &[
            s("t1", "A", Some("s1"), 3600),
            s("t1", "A", Some("s2"), 1800),
            s("t1", "A", None, 600),
        ],
        color_of,
    );
    assert_eq!(m.level, ShareLevel::Subtasks);
    assert_eq!(
        labels(&m),
        ["Sub s1", "Sub s2", &format!("Task t1 {NO_SUBTASK_LABEL}")]
    );
    assert_eq!(labels(&m).last().unwrap(), "Task t1 (no subtask)");
    assert_eq!(
        context_of(&m),
        [Vec::<String>::new(), Vec::new(), Vec::new()]
    );
    expect_consistent(&m);
}

#[test]
fn keeps_the_whole_path_of_each_row_project_task_subtask() {
    let m = subtask_shares(
        &[s("t1", "A", Some("s1"), 3600), s("t1", "A", None, 600)],
        color_of,
    );
    let got: Vec<&str> = m.rows.iter().map(|r| r.full_path.as_str()).collect();
    assert_eq!(
        got,
        [
            "Project A > Task t1 > Sub s1",
            "Project A > Task t1 > (no subtask)"
        ]
    );
    let spoken: Vec<Option<String>> = m.rows.iter().map(ShareRow::spoken_name).collect();
    assert_eq!(
        spoken,
        [
            Some("Sub s1, task Task t1, project Project A".to_owned()),
            Some("Task t1 (no subtask), project Project A".to_owned())
        ]
    );
}

#[test]
fn across_several_tasks_of_one_project_follows_the_subtask_with_its_task() {
    let m = subtask_shares(
        &[
            s("t1", "A", Some("s1"), 3600),
            s("t2", "A", Some("s2"), 1800),
            s("t2", "A", None, 600),
        ],
        color_of,
    );
    assert_eq!(
        label_and_context(&m),
        [
            vec!["Sub s1", "Task t1"],
            vec!["Sub s2", "Task t2"],
            vec!["Task t2 (no subtask)"]
        ]
    );
}

#[test]
fn adds_the_project_only_when_the_data_holds_several_projects() {
    let m = subtask_shares(
        &[
            s("t1", "A", Some("s1"), 3600),
            s("t2", "B", Some("s2"), 1800),
            s("t2", "B", None, 600),
        ],
        color_of,
    );
    assert_eq!(
        label_and_context(&m),
        [
            vec!["Sub s1", "Task t1", "Project A"],
            vec!["Sub s2", "Task t2", "Project B"],
            vec!["Task t2 (no subtask)", "Project B"],
        ]
    );
    // One task alone still names nothing.
    assert!(
        subtask_shares(&[s("t1", "A", Some("s1"), 3600)], color_of).rows[0]
            .context
            .is_empty()
    );
}

#[test]
fn puts_the_subtasks_of_one_project_together_the_biggest_task_first_the_biggest_subtask_first_inside_it()
 {
    let m = subtask_shares(
        &[
            s("t1", "A", Some("s1"), 100),
            s("t2", "B", Some("s2"), 500),
            s("t1", "A", Some("s3"), 300),
            s("t3", "A", Some("s4"), 250),
            s("t2", "B", None, 200),
        ],
        color_of,
    );
    // Project A has 650 and B 700. Inside A: t1 (400) before t3 (250); inside t1: s3 before s1.
    assert_eq!(
        labels(&m),
        [
            "Sub s2",
            "Task t2 (no subtask)",
            "Sub s3",
            "Sub s1",
            "Sub s4"
        ]
    );
    let projects: Vec<&str> = m.rows.iter().map(|r| r.project_id().unwrap()).collect();
    assert_eq!(projects, ["B", "B", "A", "A", "A"]);
}

#[test]
fn colours_the_subtasks_of_a_project_in_shades_of_the_projects_colour() {
    let m = subtask_shares(
        &[
            s("t1", "A", Some("s1"), 300),
            s("t1", "A", Some("s2"), 200),
            s("t1", "A", None, 100),
            s("t2", "B", Some("s3"), 50),
        ],
        color_of,
    );
    let hue = |hex: &str| to_oklch(hex).map_or(f64::NAN, |o| o.h);
    let a: Vec<&str> = m
        .rows
        .iter()
        .filter(|r| r.project_id() == Some("A"))
        .map(|r| r.color.as_str())
        .collect();
    assert_eq!(a.iter().collect::<BTreeSet<_>>().len(), 3);
    for c in a {
        assert!((hue(c) - hue("#2563eb")).abs() < 3.0);
    }
    let b = m.rows.iter().find(|r| r.project_id() == Some("B")).unwrap();
    assert_eq!(b.color, "#dc2626");
}

#[test]
fn groups_all_but_the_top_eight_past_nine_rows_as_other_n_subtasks() {
    let rows: Vec<ShareSubtask> = (0..12)
        .map(|i| {
            s(
                &format!("t{}", i % 3),
                "A",
                Some(&format!("s{i}")),
                1000 - i * 10,
            )
        })
        .collect();
    let m = subtask_shares(&rows, color_of);
    assert_eq!(m.rows.len(), 9);
    let other = m.rows.last().unwrap();
    assert_eq!(
        other.kind,
        RowKind::Other {
            grouped: 4,
            unit: OtherUnit::Subtasks
        }
    );
    assert_eq!(
        (other.label.as_str(), other.color.as_str()),
        ("Other (4 subtasks)", OTHER_COLOR)
    );
    assert!(other.context.is_empty());
    assert_eq!(other.secs, sum(rows[8..].iter().map(|r| r.secs)));
    expect_consistent(&m);
    // Nine rows are all shown.
    let nine = subtask_shares(&rows[..9], color_of);
    assert!(
        !nine
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Other { .. }))
    );
}

#[test]
fn has_no_rows_without_time_and_a_row_of_tiny_time_still_gets_less_than_one_percent() {
    let m = subtask_shares(
        &[
            s("t1", "A", Some("s1"), 99_999),
            s("t1", "A", Some("s2"), 1),
            s("t1", "A", Some("s3"), 0),
        ],
        color_of,
    );
    let pct: Vec<String> = m.rows.iter().map(ShareRow::pct).collect();
    assert_eq!(pct, ["(100%)", "(<1%)"]);
    expect_consistent(&m);
}

#[test]
fn adds_up_to_the_same_total_in_the_centre_as_the_tasks_the_subtasks_come_from() {
    let tasks = [t("t1", "A", 3600 + 1800 + 600), t("t2", "B", 1234)];
    let subs = [
        s("t1", "A", Some("s1"), 3600),
        s("t1", "A", Some("s2"), 1800),
        s("t1", "A", None, 600),
        s("t2", "B", None, 1234),
    ];
    assert_eq!(
        subtask_shares(&subs, color_of).total_minutes,
        task_shares(&tasks, color_of).total_minutes
    );
}

// ---- the tag combination a task is counted under

fn tags() -> Vec<ShareTag> {
    vec![
        ShareTag {
            id: "design".into(),
            name: "Design".into(),
            color: "#7c3aed".into(),
        },
        ShareTag {
            id: "client".into(),
            name: "Client".into(),
            color: "#0f766e".into(),
        },
        ShareTag {
            id: "urgent".into(),
            name: "Urgent".into(),
            color: "#c2410c".into(),
        },
    ]
}

#[test]
fn is_the_exact_set_of_tags_the_task_carries_in_the_order_of_the_settings_list() {
    let tags = tags();
    assert_eq!(
        tag_key_of(&["design", "client"], &tags).label,
        "Design + Client"
    );
    assert_eq!(
        tag_key_of(&["client", "design"], &tags).label,
        "Design + Client"
    );
    assert_eq!(
        tag_key_of(&["client", "design"], &tags).key,
        tag_key_of(&["design", "client"], &tags).key
    );
    let all = tag_key_of(&["urgent", "client", "design"], &tags);
    assert_eq!(all.label, "Design + Client + Urgent");
    assert_eq!(all.tag_ids, ["design", "client", "urgent"]);
    assert_eq!(all.colors, ["#7c3aed", "#0f766e", "#c2410c"]);
}

#[test]
fn uses_the_settings_order_not_the_alphabet() {
    let mut reversed = tags();
    reversed.reverse();
    assert_eq!(
        tag_key_of(&["design", "client"], &reversed).label,
        "Client + Design"
    );
}

#[test]
fn is_a_single_tags_own_row_for_one_tag_and_no_tag_for_none() {
    let tags = tags();
    let one = tag_key_of(&["client"], &tags);
    assert_eq!(
        (one.label.as_str(), one.tag_ids.clone(), one.colors.clone()),
        (
            "Client",
            vec!["client".to_owned()],
            vec!["#0f766e".to_owned()]
        )
    );
    let none = tag_key_of::<&str>(&[], &tags);
    assert_eq!(
        (none.label.as_str(), none.tag_ids.len(), none.colors.clone()),
        (NO_TAG_LABEL, 0, vec![UNTAGGED_COLOR.to_owned()])
    );
}

#[test]
fn does_not_count_a_tag_that_no_longer_exists() {
    let tags = tags();
    assert_eq!(tag_key_of(&["gone"], &tags).label, "No tag");
    assert_eq!(tag_key_of(&["gone", "design"], &tags).label, "Design");
}

#[test]
fn is_one_different_row_for_every_different_set() {
    let tags = tags();
    let sets: [&[&str]; 5] = [
        &["design"],
        &["client"],
        &["design", "client"],
        &["design", "client", "urgent"],
        &[],
    ];
    let keys: BTreeSet<String> = sets.iter().map(|t| tag_key_of(t, &tags).key).collect();
    assert_eq!(keys.len(), 5);
}

#[test]
fn is_the_one_place_the_counting_rule_lives_a_tasks_time_goes_whole_to_one_row() {
    let tags = tags();
    let weights = tag_weights_of(&["client", "design"], &tags);
    assert_eq!(weights.len(), 1);
    assert_eq!(weights[0].weight, 1.0);
    assert_eq!(weights[0].row.label, "Design + Client");
    assert_eq!(
        tag_weights_of::<&str>(&[], &tags)
            .iter()
            .map(|w| w.weight)
            .sum::<f64>(),
        1.0
    );
}

// ---- the Tags level

fn x(id: &str, secs: i64, tag_ids: &[&str]) -> ShareTagTask {
    ShareTagTask {
        id: id.to_owned(),
        secs,
        exact: secs,
        billable_secs: secs + 60,
        tag_ids: tag_ids.iter().map(|s| (*s).to_owned()).collect(),
    }
}

#[test]
fn has_one_row_per_combination_a_single_tags_row_for_one_tag_and_no_tag_for_untagged_time() {
    let m = tag_shares(
        &[
            x("t1", 7200, &["design"]),
            x("t2", 3600, &["client", "design"]),
            x("t3", 1800, &["design", "client", "urgent"]),
            x("t4", 600, &[]),
        ],
        &tags(),
    );
    assert_eq!(m.level, ShareLevel::Tags);
    let got: Vec<(&str, &str)> = m
        .rows
        .iter()
        .map(|r| (r.label.as_str(), r.text.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("Design", "2h 00m (55%)"),
            ("Design + Client", "1h 00m (27%)"),
            ("Design + Client + Urgent", "0h 30m (14%)"),
            ("No tag", "0h 10m (4%)"),
        ]
    );
    expect_consistent(&m);
}

#[test]
fn counts_each_task_once_so_the_rows_add_up_to_the_total_and_to_100_percent() {
    let tasks = [
        x("t1", 7777, &["design", "client"]),
        x("t2", 3333, &["client"]),
        x("t3", 1111, &["design", "client", "urgent"]),
        x("t4", 999, &[]),
    ];
    let m = tag_shares(&tasks, &tags());
    assert_eq!(m.total_secs, sum(tasks.iter().map(|t| t.secs)));
    assert_eq!(sum(m.rows.iter().map(|r| r.secs)), m.total_secs);
    expect_consistent(&m);
    // The same tasks at the Tasks level have the same centre.
    let as_tasks: Vec<ShareTask> = tasks.iter().map(|x| t(&x.id, "A", x.secs)).collect();
    assert_eq!(
        m.total_minutes,
        task_shares(&as_tasks, color_of).total_minutes
    );
}

#[test]
fn puts_tasks_with_the_same_set_of_tags_in_one_row_whatever_order_they_were_tagged_in() {
    let m = tag_shares(
        &[
            x("t1", 3600, &["design", "client"]),
            x("t2", 1800, &["client", "design"]),
        ],
        &tags(),
    );
    assert_eq!(m.rows.len(), 1);
    assert_eq!(
        (m.rows[0].label.as_str(), m.rows[0].secs),
        ("Design + Client", 5400)
    );
    assert_eq!(
        m.rows[0].kind,
        RowKind::Tag {
            tag_ids: vec!["design".into(), "client".into()],
            task_ids: vec!["t1".into(), "t2".into()]
        }
    );
}

#[test]
fn draws_a_single_tag_in_its_colour_no_tag_in_the_grey_and_a_combination_as_stripes() {
    let m = tag_shares(
        &[
            x("t1", 4000, &["design"]),
            x("t2", 3000, &["design", "client"]),
            x("t3", 2000, &["urgent", "client", "design"]),
            x("t4", 1000, &[]),
        ],
        &tags(),
    );
    let by = |label: &str| m.rows.iter().find(|r| r.label == label).unwrap();
    assert_eq!(by("Design").color, "#7c3aed");
    assert!(by("Design").stripes.is_empty());
    assert_eq!(by("No tag").color, UNTAGGED_COLOR);
    assert!(by("No tag").stripes.is_empty());
    assert_eq!(by("Design + Client").color, "#7c3aed");
    assert_eq!(by("Design + Client").stripes, ["#7c3aed", "#0f766e"]);
    assert_eq!(
        by("Design + Client + Urgent").stripes,
        ["#7c3aed", "#0f766e", "#c2410c"]
    );
}

#[test]
fn counts_the_combinations_for_the_footnote_that_explains_them() {
    let tags = tags();
    assert_eq!(
        tag_shares(&[x("t1", 100, &["design"]), x("t2", 100, &[])], &tags).combinations,
        0
    );
    assert_eq!(
        tag_shares(&[x("t1", 100, &["design", "client"])], &tags).combinations,
        1
    );
    assert_eq!(
        tag_shares(
            &[
                x("t1", 100, &["design", "client"]),
                x("t2", 100, &["design", "urgent"]),
                x("t3", 100, &["design"])
            ],
            &tags
        )
        .combinations,
        2
    );
    assert_eq!(
        COMBINATION_NOTE,
        "Tasks with several tags are counted once, under their combination."
    );
}

#[test]
fn has_no_rows_for_tasks_without_time() {
    let m = tag_shares(&[x("t1", 0, &["design"]), x("t2", 600, &[])], &tags());
    assert_eq!(labels(&m), ["No tag"]);
}

#[test]
fn groups_past_nine_rows_as_other_n_combinations_and_still_lists_every_combination_for_the_table() {
    let many: Vec<ShareTag> = (0..5)
        .map(|i| ShareTag {
            id: format!("g{i}"),
            name: format!("Tag {i}"),
            color: "#0891b2".into(),
        })
        .collect();
    let mut tasks: Vec<ShareTagTask> = many
        .iter()
        .enumerate()
        .map(|(i, t)| x(&format!("s{i}"), 5000 - i as i64 * 100, &[t.id.as_str()]))
        .collect();
    tasks.extend([
        x("p01", 4000, &["g0", "g1"]),
        x("p02", 3900, &["g0", "g2"]),
        x("p03", 3800, &["g1", "g2"]),
        x("p04", 3700, &["g0", "g1", "g2"]),
        x("p05", 3600, &["g3", "g4"]),
        x("p06", 3500, &[]),
    ]);
    let m = tag_shares(&tasks, &many);
    assert_eq!(m.rows.len(), 9);
    let other = m.rows.last().unwrap();
    assert_eq!(
        other.kind,
        RowKind::Other {
            grouped: 3,
            unit: OtherUnit::Combinations
        }
    );
    assert_eq!(
        (other.label.as_str(), other.color.as_str()),
        ("Other (3 combinations)", OTHER_COLOR)
    );
    assert_eq!(other.path, ["Other (3 combinations)"]);
    assert_eq!(m.lines.len(), 11);
    assert_eq!(sum(m.lines.iter().map(|l| l.secs)), m.total_secs);
    let mut by_time = m.lines.clone();
    by_time.sort_by_key(|l| std::cmp::Reverse(l.secs));
    assert_eq!(
        m.lines.iter().map(|l| &l.label).collect::<Vec<_>>(),
        by_time.iter().map(|l| &l.label).collect::<Vec<_>>()
    );
    expect_consistent(&m);
    // The grouped rows are the three with the least time.
    assert_eq!(other.secs, sum(m.lines[8..].iter().map(|l| l.secs)));
}

#[test]
fn keeps_the_sums_of_exact_and_billable_for_the_table_per_combination() {
    let m = tag_shares(
        &[x("t1", 3600, &["design"]), x("t2", 1800, &["design"])],
        &tags(),
    );
    let line = &m.lines[0];
    assert_eq!(
        (
            line.label.as_str(),
            line.secs,
            line.exact,
            line.billable_secs,
            line.task_ids.clone()
        ),
        (
            "Design",
            5400,
            5400,
            5520,
            vec!["t1".to_owned(), "t2".to_owned()]
        )
    );
}

#[test]
fn tags_are_the_same_whatever_order_the_tasks_come_in() {
    let tasks = [
        x("t1", 100, &["design"]),
        x("t2", 100, &["client"]),
        x("t3", 100, &["design", "client"]),
        x("t4", 100, &[]),
    ];
    let view = |m: ShareModel| {
        m.rows
            .into_iter()
            .map(|r| (r.key, r.text))
            .collect::<Vec<_>>()
    };
    let mut reversed = tasks.to_vec();
    reversed.reverse();
    assert_eq!(
        view(tag_shares(&reversed, &tags())),
        view(tag_shares(&tasks, &tags()))
    );
}

#[test]
fn opens_its_rows_by_the_tasks_they_count() {
    let m = tag_shares(
        &[x("t1", 100, &["design", "client"]), x("t2", 50, &[])],
        &tags(),
    );
    let ids: Vec<Vec<String>> = m
        .rows
        .iter()
        .map(|r| match &r.kind {
            RowKind::Tag { task_ids, .. } => task_ids.clone(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(ids, [vec!["t1"], vec!["t2"]]);
    assert_eq!(
        m.rows.iter().map(ShareRow::spoken_name).collect::<Vec<_>>(),
        [None, None]
    );
}

// ---- the colours that mark a tag row stay visible against the card

#[test]
fn keeps_the_grey_of_no_tag_the_strong_border_and_the_grey_of_other_at_3_to_1_on_every_themes_card()
{
    for (theme, border_strong, surface) in THEMES {
        assert!(
            contrast(UNTAGGED_COLOR, surface) >= 3.0,
            "{theme}: No tag on the card"
        );
        assert!(
            contrast(border_strong, surface) >= 3.0,
            "{theme}: the swatch's border"
        );
    }
    assert_eq!(OTHER_COLOR, "var(--border-strong)");
}

// ---- Other at each level

#[test]
fn is_worded_for_what_it_adds_up() {
    assert_eq!(other_label(2, OtherUnit::Tasks), "Other (2 tasks)");
    assert_eq!(other_label(5, OtherUnit::Subtasks), "Other (5 subtasks)");
    assert_eq!(
        other_label(3, OtherUnit::Combinations),
        "Other (3 combinations)"
    );
}

#[test]
fn writes_the_note_under_the_donut_and_the_tooltip_of_the_slice_for_each_unit() {
    assert_eq!(
        other_note(2, OtherUnit::Tasks),
        "Other adds up the 2 tasks with the least time. View data lists every task."
    );
    assert_eq!(
        other_note(4, OtherUnit::Subtasks),
        "Other adds up the 4 subtasks with the least time. View data lists every subtask."
    );
    assert_eq!(
        other_note(3, OtherUnit::Combinations),
        "Other adds up the 3 combinations with the least time. View data lists every combination."
    );
    assert_eq!(
        other_tip(2, OtherUnit::Tasks),
        "The 2 tasks with the least time. View data lists every task."
    );
    assert_eq!(
        other_tip(3, OtherUnit::Combinations),
        "The 3 combinations with the least time. View data lists every combination."
    );
}

#[test]
fn is_only_ever_the_projects_levels_exception_it_never_groups() {
    let projects: Vec<ShareProject> = (0..14)
        .map(|i| ShareProject {
            id: format!("p{i}"),
            name: format!("Project {i}"),
            color: "#2563eb".into(),
            secs: 100 + i,
        })
        .collect();
    let m = project_shares(&projects);
    assert_eq!(m.rows.len(), 14);
    assert!(
        !m.rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Other { .. }))
    );
}

#[test]
fn uses_one_separator_for_a_tag_combination_and_another_for_a_path() {
    assert_eq!(TAG_SEPARATOR, " + ");
    assert_eq!(PATH_SEPARATOR, " > ");
}
