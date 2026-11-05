//! `features/analysis/shareLevels.test.ts`: the default the filter asks for, the switch the user can move by hand,
//! and the fallback when a level has nothing to show. 1.x passes the filter; the port passes how many projects and
//! tasks it names, which is all `defaultLevel` reads.

use zwt_ui_logic::share::{
    Availability, LevelEvent, LevelState, ShareLevel, availability_of, default_level, reduce_level,
    shown_level, start_level,
};

use ShareLevel::{Projects, Subtasks, Tags, Tasks};

const ALL: Availability = Availability::ALL;
const NO_SUBTASKS: Availability = Availability {
    subtasks: false,
    ..Availability::ALL
};

/// `f(projectIds, taskIds)` of the TypeScript, as the counts `default_level` takes.
fn f(projects: &[&str], tasks: &[&str]) -> ShareLevel {
    default_level(projects.len(), tasks.len())
}

// ---- the segments of the switch

#[test]
fn are_projects_tasks_subtasks_and_tags_in_that_order() {
    // The segments' words are the UI's (ui/strings.slint, held to 1.x by tests/parity_strings.rs of zwt-app).
    assert_eq!(ShareLevel::ALL, [Projects, Tasks, Subtasks, Tags]);
}

// ---- the level the filter asks for

#[test]
fn is_projects_with_no_project_filter_or_with_several_projects() {
    assert_eq!(f(&[], &[]), Projects);
    assert_eq!(f(&["p1", "p2"], &[]), Projects);
    assert_eq!(f(&["p1", "p2", "p3"], &[]), Projects);
}

#[test]
fn is_tasks_with_exactly_one_project() {
    assert_eq!(f(&["p1"], &[]), Tasks);
}

#[test]
fn stays_tasks_with_one_project_and_several_tasks() {
    assert_eq!(f(&["p1"], &["t1", "t2"]), Tasks);
    assert_eq!(f(&[], &["t1", "t2"]), Projects);
}

#[test]
fn is_subtasks_with_exactly_one_task_alone_or_with_its_one_project() {
    assert_eq!(f(&[], &["t1"]), Subtasks);
    assert_eq!(f(&["p1"], &["t1"]), Subtasks);
    assert_eq!(f(&["p1", "p2"], &["t1"]), Subtasks);
}

#[test]
fn is_never_tags_that_one_is_chosen_by_hand() {
    for (p, t) in [(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (1, 2)] {
        assert_ne!(default_level(p, t), Tags);
    }
}

#[test]
fn does_not_look_at_the_filters_that_are_not_the_project_or_the_task() {
    // Tags, band, shift and search are not even an argument: no project and no task is Projects.
    assert_eq!(default_level(0, 0), Projects);
}

// ---- which levels have something to show

#[test]
fn needs_time_on_a_real_subtask_for_subtasks() {
    assert_eq!(availability_of([(true, 60)]), ALL);
    assert_eq!(availability_of([(false, 600)]), NO_SUBTASKS);
    assert_eq!(availability_of([(true, 0), (false, 600)]), NO_SUBTASKS);
    assert_eq!(availability_of([]), NO_SUBTASKS);
    assert!(availability_of([(false, 60), (true, 5)]).subtasks);
}

// ---- the level that is drawn

#[test]
fn is_the_one_asked_for_while_it_has_something_to_show() {
    for level in ShareLevel::ALL {
        assert_eq!(shown_level(level, Projects, ALL), level);
    }
}

#[test]
fn falls_back_to_the_default_of_the_filter_when_subtasks_is_empty_and_to_the_level_above() {
    assert_eq!(shown_level(Subtasks, Projects, NO_SUBTASKS), Projects);
    assert_eq!(shown_level(Subtasks, Tasks, NO_SUBTASKS), Tasks);
    // A filter on one task asks for Subtasks, which is empty: the nearest level above that has something is Tasks.
    assert_eq!(shown_level(Subtasks, Subtasks, NO_SUBTASKS), Tasks);
    assert_eq!(shown_level(Tasks, Subtasks, NO_SUBTASKS), Tasks);
    assert_eq!(shown_level(Tags, Subtasks, NO_SUBTASKS), Tags);
}

#[test]
fn never_leaves_the_chart_empty_while_any_level_has_rows() {
    for level in ShareLevel::ALL {
        for auto in ShareLevel::ALL {
            assert!(NO_SUBTASKS.has(shown_level(level, auto, NO_SUBTASKS)));
        }
    }
}

// ---- the level the card remembers

fn fold(start: LevelState, events: &[LevelEvent]) -> LevelState {
    events.iter().fold(start, |s, e| reduce_level(s, *e))
}

fn filters(auto: ShareLevel) -> LevelEvent {
    LevelEvent::Filters(auto)
}

fn pick(level: ShareLevel) -> LevelEvent {
    LevelEvent::Pick(level)
}

fn state(level: ShareLevel, auto: ShareLevel, picked: bool) -> LevelState {
    LevelState {
        level,
        auto,
        picked,
    }
}

#[test]
fn starts_on_the_default_and_has_not_been_picked() {
    assert_eq!(start_level(Tasks), state(Tasks, Tasks, false));
    assert_eq!(start_level(Tasks).pick(), None);
}

#[test]
fn follows_the_default_as_the_filter_moves_it_while_the_user_has_picked_nothing() {
    let s = fold(start_level(Projects), &[filters(Tasks)]);
    assert_eq!(s, state(Tasks, Tasks, false));
    assert_eq!(fold(s, &[filters(Subtasks)]).level, Subtasks);
    assert_eq!(
        fold(s, &[filters(Subtasks), filters(Projects)]).level,
        Projects
    );
}

#[test]
fn keeps_following_the_default_through_any_number_of_changes_before_the_first_pick() {
    let mut s = start_level(Projects);
    for auto in [Tasks, Subtasks, Tasks, Projects, Subtasks, Projects] {
        s = reduce_level(s, filters(auto));
        assert_eq!(s, state(auto, auto, false));
    }
}

#[test]
fn keeps_a_level_the_user_picked_when_the_filter_moves_the_default() {
    let s = fold(start_level(Projects), &[pick(Tags), filters(Tasks)]);
    assert_eq!(s, state(Tags, Tasks, true));
    assert_eq!(s.pick(), Some(Tags));
}

#[test]
fn never_applies_the_default_again() {
    let mut s = fold(start_level(Projects), &[pick(Tags)]);
    for auto in [Tasks, Subtasks, Tasks, Projects, Subtasks] {
        s = reduce_level(s, filters(auto));
        // `auto` still follows the filters, so the card can tell when they move it; the level does not.
        assert_eq!(s, state(Tags, auto, true));
    }
}

#[test]
fn lets_a_later_pick_replace_the_earlier_one_and_keeps_that_one_in_the_same_way() {
    let s = fold(
        start_level(Projects),
        &[
            pick(Tags),
            filters(Tasks),
            pick(Subtasks),
            filters(Projects),
            filters(Tasks),
        ],
    );
    assert_eq!(s, state(Subtasks, Tasks, true));
}

#[test]
fn does_nothing_when_the_filter_changes_but_the_default_stays() {
    let picked = fold(start_level(Projects), &[pick(Tags)]);
    assert_eq!(reduce_level(picked, filters(Projects)), picked);
    let after = fold(
        picked,
        &[
            filters(Projects),
            filters(Projects),
            filters(Projects),
            filters(Tasks),
        ],
    );
    assert_eq!(after.level, Tags);
}

#[test]
fn counts_a_pick_of_the_level_already_shown() {
    assert!(fold(start_level(Tasks), &[pick(Tasks)]).picked);
    assert_eq!(
        fold(start_level(Tasks), &[pick(Tasks), filters(Projects)]).level,
        Tasks
    );
    assert_eq!(
        fold(
            start_level(Tasks),
            &[pick(Tasks), filters(Projects), filters(Subtasks)]
        )
        .level,
        Tasks
    );
}

#[test]
fn walks_the_owners_filters_none_one_project_one_task_and_back() {
    let walk: Vec<ShareLevel> = [
        f(&[], &[]),
        f(&["p1"], &[]),
        f(&["p1"], &["t1"]),
        f(&["p1"], &[]),
        f(&[], &[]),
    ]
    .into();
    assert_eq!(walk, [Projects, Tasks, Subtasks, Tasks, Projects]);
    let mut s = start_level(walk[0]);
    let mut levels = Vec::new();
    for auto in &walk {
        s = reduce_level(s, filters(*auto));
        levels.push(s.level);
    }
    assert_eq!(levels, walk);
}

#[test]
fn walks_the_same_filters_after_a_pick_and_the_level_stays_where_the_user_put_it() {
    let walk = [
        f(&[], &[]),
        f(&["p1"], &[]),
        f(&["p1"], &["t1"]),
        f(&["p1"], &[]),
        f(&[], &[]),
        f(&[], &["t1"]),
    ];
    assert_eq!(walk, [Projects, Tasks, Subtasks, Tasks, Projects, Subtasks]);
    let mut s = fold(start_level(Projects), &[pick(Tags)]);
    let mut levels = Vec::new();
    for auto in walk {
        s = reduce_level(s, filters(auto));
        levels.push(s.level);
    }
    assert_eq!(levels, [Tags; 6]);
}

#[test]
fn is_moved_only_by_a_pick_once_there_is_one() {
    // The rule written without the reducer: the user's last pick if there has been one, else the latest default.
    let model = |start: ShareLevel, events: &[LevelEvent]| {
        let mut auto = start;
        let mut picked = None;
        for e in events {
            match e {
                LevelEvent::Pick(l) => picked = Some(*l),
                LevelEvent::Filters(a) => auto = *a,
            }
        }
        state(picked.unwrap_or(auto), auto, picked.is_some())
    };
    let defaults = [Projects, Tasks, Subtasks];
    let alphabet: Vec<LevelEvent> = ShareLevel::ALL
        .iter()
        .map(|l| pick(*l))
        .chain(defaults.iter().map(|l| filters(*l)))
        .collect();
    let mut sequences: Vec<Vec<LevelEvent>> = vec![Vec::new()];
    let mut checked = 0;
    let mut wrong = Vec::new();
    for length in 0..=5 {
        if length > 0 {
            sequences = sequences
                .iter()
                .flat_map(|events| {
                    alphabet.iter().map(move |e| {
                        let mut next = events.clone();
                        next.push(*e);
                        next
                    })
                })
                .collect();
        }
        for events in &sequences {
            for start in defaults {
                let got = fold(start_level(start), events);
                let want = model(start, events);
                checked += 1;
                if got != want {
                    wrong.push((start, events.clone(), got, want));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{:?}",
        wrong.iter().take(3).collect::<Vec<_>>()
    );
    // 1 + 7 + 7^2 + ... + 7^5 sequences, from each of the three defaults the card can start on.
    assert_eq!(checked, (7_i64.pow(6) - 1) / 6 * 3);
}
