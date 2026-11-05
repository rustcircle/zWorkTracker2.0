//! `features/analysis/shareDonut.test.tsx`, the parts that are data rather than DOM: the card's rows, totals, levels
//! and names, read from a store with `share_of_time` (or, where 1.x filters, `ShareInput` of the store cut to the
//! filtered tasks with the level reducer, since M1 has no filter). Plus the rules of reading a store: Exact basis,
//! running segments, period clipping, subtasks and tags as they are now.

use z_work_core::model::{GapDecision, TaskState};
use zwt_ui_logic::share::{
    COMBINATION_NOTE, LevelEvent, LevelState, OTHER_COLOR, RowKind, ShareInput, ShareLevel,
    ShareModel, ShareRow, UNKNOWN_PROJECT, UNTITLED_SUBTASK, default_level, reduce_level,
    share_of_time, shown_level, start_level,
};

use crate::fixtures::{
    Store, at, card, dotted, figures, labels, month, sum, titles, week, written,
};

use ShareLevel::{Projects, Subtasks, Tags, Tasks};

/// A stretch of work on 10 March 2026 (UTC), as `work` of the TypeScript.
fn work(store: Store, task: &str, from: &str, to: &str) -> Store {
    store.seg(
        task,
        &format!("2026-03-10T{from}:00Z"),
        &format!("2026-03-10T{to}:00Z"),
    )
}

/// Three projects with one task each and an hour of work apiece: three equal thirds.
fn thirds() -> Store {
    let s = Store::new()
        .abc()
        .task("a1", "p-a")
        .task("b1", "p-b")
        .task("g1", "p-c");
    let s = work(s, "a1", "09:00", "10:00");
    let s = work(s, "b1", "11:00", "12:00");
    work(s, "g1", "13:00", "14:00")
}

/// Alpha 1h 05m and Beta 40m.
fn rounded() -> Store {
    let s = Store::new()
        .project("p-a", "Alpha", "#2563eb")
        .project("p-b", "Beta", "#dc2626")
        .task("a1", "p-a")
        .task("b1", "p-b");
    work(work(s, "a1", "09:00", "10:05"), "b1", "11:00", "11:40")
}

/// Minutes of work per task, each on a day of its own in March.
const MINUTES: [(&str, &str, i64); 12] = [
    ("a1", "p-a", 120),
    ("a2", "p-a", 90),
    ("a3", "p-a", 60),
    ("a4", "p-a", 45),
    ("a5", "p-a", 30),
    ("b1", "p-b", 100),
    ("b2", "p-b", 75),
    ("b3", "p-b", 50),
    ("b4", "p-b", 20),
    ("g1", "p-c", 80),
    ("g2", "p-c", 40),
    ("g3", "p-c", 10),
];

fn twelve(n: usize) -> Store {
    let mut s = Store::new().abc();
    for (i, (id, project, minutes)) in MINUTES.iter().take(n).enumerate() {
        let start = at(&format!("2026-03-{:02}T09:00:00Z", i + 1));
        let iso = |t: i64| z_work_core::Ts(t).iso();
        s = s
            .task(id, project)
            .seg(id, &iso(start), &iso(start + minutes * 60));
    }
    s
}

/// Alpha with a task worked for an hour and another that only has a 30 minute billable pause.
fn pause_only() -> Store {
    let s = Store::new()
        .project("p-a", "Alpha", "#2563eb")
        .task("x", "p-a")
        .task("y", "p-a");
    work(s, "x", "09:00", "10:00").pause("y", "2026-03-10T11:00:00Z", "2026-03-10T11:30:00Z")
}

/// Alpha and Beta each have a task called "Meetings": 1h 30m in Alpha and 1h in Beta.
fn same_name() -> Store {
    let s = Store::new()
        .project("p-a", "Alpha", "#2563eb")
        .project("p-b", "Beta", "#dc2626")
        .named("a1", "p-a", "Meetings")
        .named("b1", "p-b", "Meetings");
    work(work(s, "a1", "09:00", "10:30"), "b1", "11:00", "12:00")
}

/// `LEVELS`: Alpha has Logo (Moodboard 60 min, Sketches 30, none 30; tagged Design) and Pitch (20 min, no tag, no
/// list). Beta has Site (Copy 45, none 15; tagged Client then Design). The tag ids are chosen so that the core's
/// order of tags (by id), which is 1.x's Settings order, is Design then Client, as in the TypeScript fixture.
fn levels() -> Store {
    let s = Store::new()
        .project("p-a", "Alpha", "#2563eb")
        .project("p-b", "Beta", "#dc2626")
        .tag("t-a", "Design", "#7c3aed")
        .tag("t-b", "Client", "#0f766e")
        .named("a1", "p-a", "Logo")
        .tags("a1", &["t-a"])
        .subtasks("a1", &[("mood", "Moodboard"), ("sk", "Sketches")])
        .named("a2", "p-a", "Pitch")
        .named("b1", "p-b", "Site")
        .tags("b1", &["t-b", "t-a"])
        .subtasks("b1", &[("copy", "Copy")]);
    s.seg_on("a1", "2026-03-10T09:00:00Z", "2026-03-10T10:00:00Z", "mood")
        .seg_on("a1", "2026-03-10T11:00:00Z", "2026-03-10T11:30:00Z", "sk")
        .seg("a1", "2026-03-10T12:00:00Z", "2026-03-10T12:30:00Z")
        .seg("a2", "2026-03-10T15:00:00Z", "2026-03-10T15:20:00Z")
        .seg_on("b1", "2026-03-10T13:00:00Z", "2026-03-10T13:45:00Z", "copy")
        .seg("b1", "2026-03-10T14:00:00Z", "2026-03-10T14:15:00Z")
}

/// One task with twelve items, each with its own time.
fn twelve_items() -> Store {
    let items: Vec<(String, String)> = (0..12)
        .map(|i| (format!("i{i:02}"), format!("Item {i:02}")))
        .collect();
    let refs: Vec<(&str, &str)> = items
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let mut s = Store::new()
        .project("p-a", "Alpha", "#2563eb")
        .named("a1", "p-a", "Big")
        .subtasks("a1", &refs);
    for (i, (id, _)) in items.iter().enumerate() {
        let start = at(&format!("2026-03-{:02}T09:00:00Z", i + 2));
        let iso = |t: i64| z_work_core::Ts(t).iso();
        s = s.seg_on(
            "a1",
            &iso(start),
            &iso(start + (120 - i as i64 * 5) * 60),
            id,
        );
    }
    s
}

fn percents(rows: &[ShareRow]) -> i64 {
    sum(rows.iter().map(|r| r.whole_percent))
}

fn minutes(rows: &[ShareRow]) -> i64 {
    sum(rows.iter().map(|r| r.minutes))
}

/// What a screen reader says for a legend row: the button's name (or the label), then the figure.
fn said(r: &ShareRow) -> String {
    format!(
        "{} {}",
        r.spoken_name().unwrap_or_else(|| r.label.clone()),
        r.text
    )
}

/// The slice's accessible name: "Alpha > Logo: 2h 00m (86%)".
fn slice(r: &ShareRow) -> String {
    format!("{}: {}", r.full_path, r.text)
}

/// The card 1.x draws for a store cut to what a filter keeps, with the level state the reducer holds.
fn filtered(store: &Store, state: LevelState) -> ShareModel {
    let (from, to) = week();
    let input = ShareInput::from_file(&store.file, from, to, at(crate::fixtures::NOW));
    input.model(shown_level(state.level, state.auto, input.availability()))
}

// ---- the legend

#[test]
fn writes_one_row_per_project_as_text() {
    let v = card(&thirds(), week(), None);
    let got: Vec<(String, String, String, String)> = v
        .rows
        .iter()
        .map(|r| (r.label.clone(), r.text.clone(), r.hm.clone(), r.pct()))
        .collect();
    let want = [
        ("Alpha", "1h 00m (34%)", "1h 00m", "(34%)"),
        ("Beta", "1h 00m (33%)", "1h 00m", "(33%)"),
        ("Gamma", "1h 00m (33%)", "1h 00m", "(33%)"),
    ]
    .map(|(a, b, c, d)| (a.to_owned(), b.to_owned(), c.to_owned(), d.to_owned()));
    assert_eq!(got, want);
    // The swatch is the project's own colour.
    assert_eq!(v.rows[0].color, "#2563eb");
}

#[test]
fn adds_three_equal_thirds_up_to_100_percent_and_the_rows_up_to_the_total_in_the_middle() {
    let v = card(&thirds(), week(), None);
    assert_eq!(percents(&v.rows), 100);
    assert_eq!(v.total_text, "3h 00m");
    assert_eq!(minutes(&v.rows), 180);
}

#[test]
fn gives_a_long_name_the_whole_name_as_its_title() {
    let long = "Customer churn prediction model for the retention team, second round";
    let s = work(
        Store::new()
            .project("p-a", long, "#2563eb")
            .task("a1", "p-a"),
        "a1",
        "09:00",
        "10:00",
    );
    let v = card(&s, week(), None);
    assert_eq!(
        (v.rows[0].label.as_str(), v.rows[0].full_path.as_str()),
        (long, long)
    );
}

#[test]
fn follows_the_exact_basis() {
    // The Exact half of "follows the Billable and Exact switch" (Billable is milestone 5).
    let v = card(&rounded(), week(), None);
    assert_eq!(figures(&v.rows), ["1h 05m (62%)", "0h 40m (38%)"]);
    assert_eq!(v.total_text, "1h 45m");
}

#[test]
fn shows_no_rows_when_nothing_is_logged_and_keeps_the_switch() {
    let v = card(&Store::new().abc(), week(), None);
    assert!(v.rows.is_empty());
    assert_eq!((v.total_secs, v.total_text.as_str()), (0, "0h 00m"));
    assert_eq!(
        (v.shown, v.subtasks_available, v.note.clone()),
        (Projects, false, None)
    );
}

// ---- the Tasks view

#[test]
fn groups_the_shown_tasks_by_project_and_the_rest_as_other_4_tasks_last() {
    let v = card(&twelve(12), month(), Some(Tasks));
    // Alpha has the most time (345 min with its grouped task), then Beta (245), then Gamma (130).
    assert_eq!(
        labels(&v.rows),
        [
            "Task a1",
            "Task a2",
            "Task a3",
            "Task a4",
            "Task b1",
            "Task b2",
            "Task b3",
            "Task g1",
            "Other (4 tasks)"
        ]
    );
    assert_eq!(v.rows.last().unwrap().text, "1h 40m (14%)");
}

#[test]
fn adds_up_to_100_percent_and_to_the_total_in_the_middle_other_included() {
    let v = card(&twelve(12), month(), Some(Tasks));
    assert_eq!(v.total_text, "12h 00m");
    assert_eq!(percents(&v.rows), 100);
    assert_eq!(minutes(&v.rows), 720);
}

#[test]
fn says_what_other_is_made_of_and_cannot_be_opened() {
    let v = card(&twelve(12), month(), Some(Tasks));
    let other = v.rows.last().unwrap();
    assert_eq!(other.label, "Other (4 tasks)");
    assert_eq!(other.spoken_name(), None);
    assert_eq!(said(other), "Other (4 tasks) 1h 40m (14%)");
    // Its colour is the strong border token, a grey every theme defines, not one of the project shades.
    assert_eq!(other.color, OTHER_COLOR);
    assert_eq!(
        v.note.as_deref(),
        Some("Other adds up the 4 tasks with the least time. View data lists every task.")
    );
}

#[test]
fn draws_a_projects_only_shown_task_in_the_projects_colour_and_its_several_tasks_in_different_shades()
 {
    let v = card(&twelve(12), month(), Some(Tasks));
    let swatches: Vec<&str> = v.rows.iter().map(|r| r.color.as_str()).collect();
    // g1 is Gamma's only shown task; Alpha's four and Beta's three are all different.
    assert_eq!(swatches[7], "#059669");
    let distinct = |xs: &[&str]| xs.iter().collect::<std::collections::BTreeSet<_>>().len();
    assert_eq!(distinct(&swatches[..4]), 4);
    assert_eq!(distinct(&swatches[4..7]), 3);
    assert_eq!(distinct(&swatches), 9);
}

#[test]
fn shows_nine_tasks_or_fewer_without_an_other_row() {
    let v = card(&twelve(9), month(), Some(Tasks));
    assert_eq!(v.rows.len(), 9);
    assert!(!labels(&v.rows).iter().any(|n| n.starts_with("Other")));
}

#[test]
fn counts_a_billable_pause_nowhere_in_the_exact_basis() {
    // Adapted from "puts the centre of the view at the sum of its slices ... only a billable pause" (Billable): in
    // Exact the pause is no time at all, so both levels show the hour worked on x.
    let v = card(&pause_only(), week(), None);
    assert_eq!(
        (v.total_text.as_str(), figures(&v.rows)),
        ("1h 00m", vec!["1h 00m (100%)".to_owned()])
    );
    let tasks = card(&pause_only(), week(), Some(Tasks));
    assert_eq!(tasks.total_text, "1h 00m");
    assert_eq!(figures(&tasks.rows), ["1h 00m (100%)"]);
    assert_eq!(labels(&tasks.rows), ["Task x"]);
}

// ---- two tasks with the same name in different projects

#[test]
fn are_told_apart_by_the_project_written_after_the_task_name_in_the_row_and_in_its_title() {
    let v = card(&same_name(), week(), Some(Tasks));
    assert_eq!(v.rows.len(), 2);
    assert_eq!(labels(&v.rows), ["Meetings", "Meetings"]);
    assert_eq!(
        v.rows.iter().map(|r| r.context.clone()).collect::<Vec<_>>(),
        [vec!["Alpha"], vec!["Beta"]]
    );
    assert_eq!(written(&v.rows), ["Meetings · Alpha", "Meetings · Beta"]);
    assert_eq!(titles(&v.rows), ["Alpha > Meetings", "Beta > Meetings"]);
    assert_eq!(figures(&v.rows), ["1h 30m (60%)", "1h 00m (40%)"]);
    // The Projects view is as it was: the project's name alone.
    let projects = card(&same_name(), week(), Some(Projects));
    assert_eq!(written(&projects.rows), ["Alpha", "Beta"]);
}

// ---- what a screen reader says for a legend row

#[test]
fn names_a_button_after_the_task_and_its_project_or_after_the_project_alone_and_never_the_figures()
{
    let v = card(&same_name(), week(), Some(Tasks));
    let names: Vec<String> = v.rows.iter().filter_map(ShareRow::spoken_name).collect();
    assert_eq!(names, ["Meetings, project Alpha", "Meetings, project Beta"]);
    assert!(
        !names
            .iter()
            .any(|n| n.contains('%') || n.chars().any(|c| c.is_ascii_digit()))
    );
    let projects = card(&same_name(), week(), Some(Projects));
    assert!(projects.rows.iter().all(|r| r.spoken_name().is_none()));
    assert_eq!(labels(&projects.rows), ["Alpha", "Beta"]);
}

#[test]
fn reads_each_row_as_the_name_the_project_and_the_figure() {
    let v = card(&same_name(), week(), Some(Tasks));
    assert_eq!(
        v.rows.iter().map(said).collect::<Vec<_>>(),
        [
            "Meetings, project Alpha 1h 30m (60%)",
            "Meetings, project Beta 1h 00m (40%)"
        ]
    );
    let projects = card(&same_name(), week(), Some(Projects));
    assert_eq!(
        projects.rows.iter().map(said).collect::<Vec<_>>(),
        ["Alpha 1h 30m (60%)", "Beta 1h 00m (40%)"]
    );
}

// ---- the four segments

#[test]
fn start_on_projects() {
    let v = card(&levels(), week(), None);
    assert_eq!((v.shown, v.picked), (Projects, None));
}

#[test]
fn switches_subtasks_off_with_a_tooltip_while_no_time_is_logged_on_a_subtask() {
    // The tooltip's words are the UI's (ui/strings.slint `share-no-subtask-time`).
    assert!(!card(&thirds(), week(), None).subtasks_available);
}

#[test]
fn switches_subtasks_on_when_time_is_logged_on_one() {
    assert!(card(&levels(), week(), None).subtasks_available);
}

#[test]
fn keeps_the_same_total_in_the_middle_at_every_level() {
    let centres: Vec<String> = ShareLevel::ALL
        .iter()
        .map(|l| card(&levels(), week(), Some(*l)).total_text)
        .collect();
    assert_eq!(centres, ["3h 20m"; 4]);
}

// ---- the level the filter asks for (the filter emulated by cutting the store to the tasks it keeps)

#[test]
fn starts_on_tasks_with_one_project_chosen_and_the_rows_name_the_task_alone() {
    let auto = default_level(1, 0);
    let m = filtered(&levels().only(&["a1", "a2"]), start_level(auto));
    assert_eq!(m.level, Tasks);
    assert_eq!(dotted(&m.rows), [vec!["Logo"], vec!["Pitch"]]);
    assert_eq!(figures(&m.rows), ["2h 00m (86%)", "0h 20m (14%)"]);
}

#[test]
fn starts_on_subtasks_with_one_task_chosen_and_the_rows_name_the_subtask_alone() {
    let m = filtered(&levels().only(&["a1"]), start_level(default_level(0, 1)));
    assert_eq!(m.level, Subtasks);
    assert_eq!(
        dotted(&m.rows),
        [
            vec!["Moodboard"],
            vec!["Sketches"],
            vec!["Logo (no subtask)"]
        ]
    );
    assert_eq!(
        figures(&m.rows),
        ["1h 00m (50%)", "0h 30m (25%)", "0h 30m (25%)"]
    );
}

#[test]
fn falls_back_to_the_default_level_when_the_level_shown_has_no_rows_left_and_switches_subtasks_off()
{
    let picked = reduce_level(start_level(Projects), LevelEvent::Pick(Subtasks));
    // Pitch has no subtask time: the filter asks for Subtasks, which is empty, so the card shows Tasks.
    let state = reduce_level(picked, LevelEvent::Filters(default_level(0, 1)));
    let pitch = levels().only(&["a2"]);
    let m = filtered(&pitch, state);
    assert_eq!(m.level, Tasks);
    assert_eq!(labels(&m.rows), ["Pitch"]);
    let (from, to) = week();
    assert!(
        !ShareInput::from_file(&pitch.file, from, to, 0)
            .availability()
            .subtasks
    );
}

#[test]
fn draws_another_level_while_the_picked_one_has_nothing_to_show_and_goes_back_to_the_pick_when_its_time_returns()
 {
    let mut state = reduce_level(start_level(Projects), LevelEvent::Pick(Subtasks));
    state = reduce_level(state, LevelEvent::Filters(default_level(0, 1)));
    assert_eq!(filtered(&levels().only(&["a2"]), state).level, Tasks);
    assert_eq!(state.pick(), Some(Subtasks));
    // Beta's Site has time on a subtask. The filter now asks for Tasks, and still the pick is what is drawn.
    state = reduce_level(state, LevelEvent::Filters(default_level(1, 0)));
    let m = filtered(&levels().only(&["b1"]), state);
    assert_eq!(m.level, Subtasks);
    assert_eq!(labels(&m.rows), ["Copy", "Site (no subtask)"]);
}

#[test]
fn shows_subtasks_again_when_the_time_comes_back_if_that_is_still_the_level_asked_for() {
    let state = start_level(default_level(0, 1));
    assert_eq!(filtered(&levels().only(&["a2"]), state).level, Tasks);
    assert_eq!(filtered(&levels().only(&["a1"]), state).level, Subtasks);
}

#[test]
fn lets_the_user_pick_a_level_by_hand_over_the_default() {
    let state = reduce_level(start_level(default_level(1, 0)), LevelEvent::Pick(Projects));
    let m = filtered(&levels().only(&["a1", "a2"]), state);
    assert_eq!(labels(&m.rows), ["Alpha"]);
}

// ---- what a row says: only what the filter leaves open

#[test]
fn tasks_with_several_projects_in_the_data_the_task_then_its_project() {
    let v = card(&levels(), week(), Some(Tasks));
    // Grouped by project, the project with the most time first.
    assert_eq!(
        dotted(&v.rows),
        [
            vec!["Logo", "Alpha"],
            vec!["Pitch", "Alpha"],
            vec!["Site", "Beta"]
        ]
    );
    assert_eq!(
        written(&v.rows),
        ["Logo · Alpha", "Pitch · Alpha", "Site · Beta"]
    );
}

#[test]
fn tasks_with_one_project_in_the_data_the_task_alone_with_the_whole_path_still_in_the_title() {
    let one = levels().only(&["a1", "a2"]);
    let v = card(&one, week(), Some(Tasks));
    assert_eq!(written(&v.rows), ["Logo", "Pitch"]);
    assert_eq!(titles(&v.rows), ["Alpha > Logo", "Alpha > Pitch"]);
    assert_eq!(
        v.rows
            .iter()
            .filter_map(ShareRow::spoken_name)
            .collect::<Vec<_>>(),
        ["Logo, project Alpha", "Pitch, project Alpha"]
    );
    assert_eq!(
        v.rows.iter().map(said).collect::<Vec<_>>(),
        [
            "Logo, project Alpha 2h 00m (86%)",
            "Pitch, project Alpha 0h 20m (14%)"
        ]
    );
    assert_eq!(slice(&v.rows[0]), "Alpha > Logo: 2h 00m (86%)");
    assert_eq!(written(&card(&one, week(), Some(Projects)).rows), ["Alpha"]);
}

#[test]
fn subtasks_of_one_task_the_subtask_alone_and_the_tasks_time_on_none_as_task_no_subtask() {
    let v = card(&levels().only(&["a1"]), week(), Some(Subtasks));
    assert_eq!(
        written(&v.rows),
        ["Moodboard", "Sketches", "Logo (no subtask)"]
    );
    assert_eq!(
        titles(&v.rows),
        [
            "Alpha > Logo > Moodboard",
            "Alpha > Logo > Sketches",
            "Alpha > Logo > (no subtask)"
        ]
    );
    assert_eq!(
        v.rows
            .iter()
            .filter_map(ShareRow::spoken_name)
            .collect::<Vec<_>>(),
        [
            "Moodboard, task Logo, project Alpha",
            "Sketches, task Logo, project Alpha",
            "Logo (no subtask), project Alpha"
        ]
    );
    assert_eq!(
        said(&v.rows[0]),
        "Moodboard, task Logo, project Alpha 1h 00m (50%)"
    );
    assert_eq!(slice(&v.rows[1]), "Alpha > Logo > Sketches: 0h 30m (25%)");
}

#[test]
fn subtasks_across_the_tasks_of_one_project_the_subtask_then_its_task() {
    let v = card(&levels().only(&["a1", "a2"]), week(), Some(Subtasks));
    assert_eq!(
        dotted(&v.rows),
        [
            vec!["Moodboard", "Logo"],
            vec!["Sketches", "Logo"],
            vec!["Logo (no subtask)"],
            vec!["Pitch (no subtask)"]
        ]
    );
}

#[test]
fn subtasks_across_several_projects_the_subtask_then_its_task_then_its_project() {
    let v = card(&levels(), week(), Some(Subtasks));
    assert_eq!(
        dotted(&v.rows),
        [
            vec!["Moodboard", "Logo", "Alpha"],
            vec!["Sketches", "Logo", "Alpha"],
            vec!["Logo (no subtask)", "Alpha"],
            vec!["Pitch (no subtask)", "Alpha"],
            vec!["Copy", "Site", "Beta"],
            vec!["Site (no subtask)", "Beta"],
        ]
    );
    assert_eq!(written(&v.rows)[0], "Moodboard · Logo · Alpha");
    assert_eq!(v.rows[0].full_path, "Alpha > Logo > Moodboard");
    assert_eq!(
        said(&v.rows[0]),
        "Moodboard, task Logo, project Alpha 1h 00m (30%)"
    );
}

#[test]
fn adds_up_to_100_percent_and_to_the_total_at_the_subtasks_level_rows_for_none_included() {
    let v = card(&levels(), week(), Some(Subtasks));
    assert_eq!(v.rows.len(), 6);
    assert_eq!(percents(&v.rows), 100);
    assert_eq!(minutes(&v.rows), 200);
    assert_eq!(v.total_text, "3h 20m");
}

// ---- the Tags level

#[test]
fn has_a_row_per_combination_of_tags_named_in_the_settings_order_with_no_tag_for_the_rest() {
    let v = card(&levels(), week(), Some(Tags));
    assert_eq!(written(&v.rows), ["Design", "Design + Client", "No tag"]);
    assert_eq!(
        figures(&v.rows),
        ["2h 00m (60%)", "1h 00m (30%)", "0h 20m (10%)"]
    );
    assert_eq!(v.total_text, "3h 20m");
    assert_eq!(percents(&v.rows), 100);
    assert_eq!(titles(&v.rows), ["Design", "Design + Client", "No tag"]);
}

#[test]
fn draws_a_single_tag_in_its_colour_a_combination_in_stripes_and_no_tag_in_grey() {
    let v = card(&levels(), week(), Some(Tags));
    assert_eq!(
        v.rows.iter().map(|r| r.stripes.len()).collect::<Vec<_>>(),
        [0, 2, 0]
    );
    assert_eq!(v.rows[0].color, "#7c3aed");
    assert_eq!(v.rows[2].color, "#6b7280");
    assert_eq!(v.rows[1].stripes, ["#7c3aed", "#0f766e"]);
    // The other levels carry no stripes.
    assert!(
        card(&levels(), week(), Some(Tasks))
            .rows
            .iter()
            .all(|r| r.stripes.is_empty())
    );
}

#[test]
fn explains_the_combinations_under_the_legend_when_a_task_carries_more_than_one_tag() {
    assert_eq!(card(&levels(), week(), None).footnote, None);
    assert_eq!(
        card(&levels(), week(), Some(Tags)).footnote.as_deref(),
        Some(COMBINATION_NOTE)
    );
    assert_eq!(card(&levels(), week(), Some(Tasks)).footnote, None);
}

#[test]
fn says_nothing_about_combinations_when_no_task_carries_more_than_one_tag() {
    let v = card(&thirds(), week(), Some(Tags));
    assert_eq!(labels(&v.rows), ["No tag"]);
    assert_eq!((v.combinations, v.footnote), (0, None));
}

#[test]
fn shows_only_the_tasks_a_tag_filter_keeps_each_with_its_full_set_of_tags() {
    let v = card(&levels().only(&["b1"]), week(), Some(Tags));
    assert_eq!(written(&v.rows), ["Design + Client"]);
    assert_eq!(figures(&v.rows), ["1h 00m (100%)"]);
}

#[test]
fn shows_the_untagged_time_alone_for_the_no_tag_filter() {
    let v = card(&levels().only(&["a2"]), week(), Some(Tags));
    assert_eq!(written(&v.rows), ["No tag"]);
    assert_eq!(figures(&v.rows), ["0h 20m (100%)"]);
    assert_eq!(v.total_text, "0h 20m");
}

// ---- opening the time behind a row of the new levels (what a click would open)

#[test]
fn opens_a_subtask_with_its_task_and_the_time_on_none_with_the_tasks_time_on_no_subtask() {
    let v = card(&levels(), week(), Some(Subtasks));
    let by = |label: &str| {
        v.rows
            .iter()
            .find(|r| r.label == label)
            .unwrap()
            .kind
            .clone()
    };
    assert_eq!(
        by("Moodboard"),
        RowKind::Subtask {
            id: Some("mood".into()),
            task_id: "a1".into(),
            project_id: "p-a".into()
        }
    );
    assert_eq!(
        by("Logo (no subtask)"),
        RowKind::Subtask {
            id: None,
            task_id: "a1".into(),
            project_id: "p-a".into()
        }
    );
}

#[test]
fn opens_a_combination_with_the_tasks_it_counts() {
    let v = card(&levels(), week(), Some(Tags));
    let tasks = |label: &str| match &v.rows.iter().find(|r| r.label == label).unwrap().kind {
        RowKind::Tag { task_ids, .. } => task_ids.clone(),
        _ => Vec::new(),
    };
    assert_eq!(tasks("Design + Client"), ["b1"]);
    assert_eq!(tasks("No tag"), ["a2"]);
}

// ---- Other at the Subtasks level

#[test]
fn groups_all_but_the_top_eight_as_other_4_subtasks_says_what_it_is_made_of_and_cannot_be_opened() {
    let v = card(&twelve_items(), month(), Some(Subtasks));
    assert_eq!(v.shown, Subtasks);
    assert_eq!(v.rows.len(), 9);
    let want: Vec<String> = (0..8).map(|i| format!("Item {i:02}")).collect();
    assert_eq!(labels(&v.rows)[..8], want[..]);
    let other = v.rows.last().unwrap();
    assert_eq!(other.label, "Other (4 subtasks)");
    assert_eq!(other.spoken_name(), None);
    assert_eq!(other.color, OTHER_COLOR);
    assert_eq!(
        v.note.as_deref(),
        Some("Other adds up the 4 subtasks with the least time. View data lists every subtask.")
    );
    assert_eq!(percents(&v.rows), 100);
}

// ---- share_of_time: the picked level (PARITY 212, 213; acceptance (c))

#[test]
fn a_picked_level_stays_when_the_data_changes() {
    let v = card(&levels(), week(), Some(Tags));
    assert_eq!((v.shown, v.picked), (Tags, Some(Tags)));
    let changed = work(levels().task("new", "p-b"), "new", "16:00", "17:00");
    let v = card(&changed, week(), Some(Tags));
    assert_eq!((v.shown, v.picked), (Tags, Some(Tags)));
    // The new untagged hour moves "No tag" above the combination.
    assert_eq!(labels(&v.rows), ["Design", "No tag", "Design + Client"]);
    assert_eq!(v.total_text, "4h 20m");
}

#[test]
fn picked_subtasks_without_subtask_time_shows_another_level_and_stays_picked() {
    let v = card(&thirds(), week(), Some(Subtasks));
    assert_eq!(
        (v.shown, v.picked, v.subtasks_available),
        (Projects, Some(Subtasks), false)
    );
    assert_eq!(labels(&v.rows), ["Alpha", "Beta", "Gamma"]);
    // Time on a subtask comes back: the pick is drawn again.
    let back = thirds().subtasks("a1", &[("s1", "Draft")]).seg_on(
        "a1",
        "2026-03-11T09:00:00Z",
        "2026-03-11T09:30:00Z",
        "s1",
    );
    let v = card(&back, week(), Some(Subtasks));
    assert_eq!(
        (v.shown, v.picked, v.subtasks_available),
        (Subtasks, Some(Subtasks), true)
    );
    assert_eq!(labels(&v.rows)[0], "Task a1 (no subtask)");
    assert!(labels(&v.rows).contains(&"Draft".to_owned()));
}

#[test]
fn no_pick_is_the_default_level_projects() {
    for store in [thirds(), levels(), twelve(12)] {
        assert_eq!(card(&store, month(), None).shown, Projects);
    }
}

// ---- reading the store

#[test]
fn a_running_segment_counts_up_to_now() {
    let s = Store::new()
        .abc()
        .task("a1", "p-a")
        .open("a1", "2026-03-12T11:00:00Z");
    let (from, to) = week();
    let v = share_of_time(&s.file, from, to, at("2026-03-12T11:30:00Z"), None);
    assert_eq!(v.total_text, "0h 30m");
    // A minute later the text moves; the store is the same.
    let v = share_of_time(&s.file, from, to, at("2026-03-12T11:31:00Z"), None);
    assert_eq!(v.total_text, "0h 31m");
    assert_eq!(v.rows[0].text, "0h 31m (100%)");
}

#[test]
fn another_devices_running_segment_counts_up_to_its_last_heartbeat() {
    let s = Store::new().abc().task("a1", "p-a").open_elsewhere(
        "a1",
        "2026-03-12T09:00:00Z",
        "dev-2",
        "2026-03-12T09:40:00Z",
    );
    let v = card(&s, week(), None);
    assert_eq!(v.total_text, "0h 40m");
}

#[test]
fn counts_only_the_part_of_a_segment_inside_the_period() {
    let s = Store::new()
        .abc()
        .task("a1", "p-a")
        .seg("a1", "2026-03-08T23:00:00Z", "2026-03-09T01:00:00Z")
        .seg("a1", "2026-03-15T23:30:00Z", "2026-03-16T02:00:00Z");
    let v = card(&s, week(), None);
    assert_eq!(v.total_secs, 3600 + 1800);
}

#[test]
fn counts_thinking_and_leaves_out_time_away_that_was_dropped_or_is_unconfirmed() {
    let s = Store::new().abc().task("a1", "p-a").seg(
        "a1",
        "2026-03-10T09:00:00Z",
        "2026-03-10T12:00:00Z",
    );
    let seg = s.last_segment();
    let s = s
        .away(
            &seg,
            "2026-03-10T09:30:00Z",
            "2026-03-10T10:00:00Z",
            GapDecision::Thinking,
        )
        .away(
            &seg,
            "2026-03-10T10:00:00Z",
            "2026-03-10T10:30:00Z",
            GapDecision::Drop,
        )
        .away(
            &seg,
            "2026-03-10T11:00:00Z",
            "2026-03-10T11:15:00Z",
            GapDecision::Pending,
        )
        .away(
            &seg,
            "2026-03-10T11:30:00Z",
            "2026-03-10T11:45:00Z",
            GapDecision::Keep,
        );
    let v = card(&s, week(), None);
    // 3 h, less the dropped 30 min and the unconfirmed 15 min.
    assert_eq!(v.total_text, "2h 15m");
}

#[test]
fn a_subtask_taken_out_of_the_list_counts_as_time_on_no_subtask() {
    let s = Store::new()
        .abc()
        .task("a1", "p-a")
        .subtasks("a1", &[("kept", "Kept")])
        .seg_on("a1", "2026-03-10T09:00:00Z", "2026-03-10T10:00:00Z", "gone");
    let v = card(&s, week(), Some(Subtasks));
    assert!(!v.subtasks_available);
    assert_eq!(v.shown, Projects);
    let input = ShareInput::from_file(&s.file, week().0, week().1, 0);
    assert_eq!(input.subtasks.len(), 1);
    assert_eq!(
        (input.subtasks[0].id.clone(), input.subtasks[0].secs),
        (None, 3600)
    );
}

#[test]
fn a_subtask_with_no_text_is_an_untitled_subtask() {
    let s = Store::new()
        .abc()
        .task("a1", "p-a")
        .subtasks("a1", &[("x1", "   ")])
        .seg_on("a1", "2026-03-10T09:00:00Z", "2026-03-10T10:00:00Z", "x1");
    let v = card(&s, week(), Some(Subtasks));
    assert_eq!(labels(&v.rows), [UNTITLED_SUBTASK]);
    assert_eq!(UNTITLED_SUBTASK, "Untitled subtask");
}

#[test]
fn a_task_deleted_with_its_time_counts_nowhere() {
    let v = card(&thirds().discard("b1"), week(), None);
    assert_eq!(labels(&v.rows), ["Alpha", "Gamma"]);
    assert_eq!(v.total_text, "2h 00m");
}

#[test]
fn a_deleted_tag_is_not_counted() {
    let mut s = levels();
    s.file.data.tags.get_mut("t-b").unwrap().deleted = true;
    let v = card(&s, week(), Some(Tags));
    // Site keeps only Design: it joins Logo's row.
    assert_eq!(written(&v.rows), ["Design", "No tag"]);
    assert_eq!(figures(&v.rows), ["3h 00m (90%)", "0h 20m (10%)"]);
}

#[test]
fn a_task_whose_project_is_missing_counts_at_the_task_level_only_as_in_1x() {
    let s = work(thirds().task("lost", "p-gone"), "lost", "15:00", "16:00");
    let projects = card(&s, week(), None);
    assert_eq!(labels(&projects.rows), ["Alpha", "Beta", "Gamma"]);
    let tasks = card(&s, week(), Some(Tasks));
    let lost = tasks.rows.iter().find(|r| r.label == "Task lost").unwrap();
    assert_eq!(lost.context, [UNKNOWN_PROJECT]);
    assert_eq!(lost.color, "#6b7280");
    assert_eq!(tasks.total_text, "4h 00m");
}

#[test]
fn a_task_with_no_time_in_the_period_has_no_row() {
    // A draft with no time, and a task whose time is all outside the period.
    let s = thirds()
        .task("d", "p-a")
        .state("d", TaskState::Draft)
        .task("old", "p-b")
        .seg("old", "2026-02-10T09:00:00Z", "2026-02-10T10:00:00Z");
    assert_eq!(card(&s, week(), Some(Tasks)).rows.len(), 3);
}
