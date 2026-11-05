//! Topic bits of real core changes: each action through `Service` sets exactly the expected bits.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use std::sync::Arc;

use common::{NOW, ZONE, ZONE_NAME, date, today};
use z_work_core::ManualClock;
use z_work_core::catalog::TagInput;
use z_work_core::machine::ProjectInput;
use z_work_core::model::{LocalFile, Theme};
use z_work_core::service::Service;
use zwt_bridge::sample::{self, STARTED_TASK};
use zwt_bridge::topics::{Scope, Topics, diff};
use zwt_bridge::views::{scope_of, week_of};

const T: Topics = Topics::TASKS;
const R: Topics = Topics::RUNNING;
const D: Topics = Topics::DAY;
const C: Topics = Topics::CHIPS;
const S: Topics = Topics::SHARE;

struct Fixture {
    svc: Service,
    clock: ManualClock,
    _dir: tempfile::TempDir,
}

/// A fresh sample store: `t-social` runs since 10:30.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let opened = sample::open_or_seed(dir.path(), Arc::new(clock.clone()), ZONE_NAME).unwrap();
    Fixture {
        svc: opened.service,
        clock,
        _dir: dir,
    }
}

fn scope_on(day: chrono::NaiveDate) -> Scope {
    scope_of(day, today(), &week_of(today(), ZONE), ZONE)
}

/// The topics `change` sets, with the day axis on today.
fn topics_of(f: &mut Fixture, change: impl FnOnce(&mut Service)) -> Topics {
    topics_on(f, today(), change)
}

fn topics_on(f: &mut Fixture, day: chrono::NaiveDate, change: impl FnOnce(&mut Service)) -> Topics {
    let before: LocalFile = f.svc.file().clone();
    change(&mut f.svc);
    diff(&before, f.svc.file(), &scope_on(day))
}

#[test]
fn nothing_changed_sets_nothing() {
    let f = fixture();
    assert_eq!(
        diff(f.svc.file(), f.svc.file(), &scope_on(today())),
        Topics::NONE
    );
}

#[test]
fn start_with_nothing_running() {
    let mut f = fixture();
    f.svc.pause_task(STARTED_TASK).unwrap();
    f.clock.advance(60);
    let t = topics_of(&mut f, |s| {
        s.start_task("t-a11y").unwrap();
    });
    // An open segment adds no closed time, so the chips stay.
    assert_eq!(t, T | R | D | S);
}

#[test]
fn starting_a_second_task_while_one_runs() {
    let mut f = fixture();
    f.clock.advance(600);
    let t = topics_of(&mut f, |s| {
        let moved = s.start_task("t-a11y").unwrap();
        assert_eq!(moved.auto_paused_task_id.as_deref(), Some(STARTED_TASK));
    });
    assert_eq!(
        t,
        T | R | D | C | S,
        "the paused task's closed segment reaches the chips"
    );
}

#[test]
fn pause() {
    let mut f = fixture();
    f.clock.advance(600);
    let t = topics_of(&mut f, |s| s.pause_task(STARTED_TASK).unwrap());
    assert_eq!(t, T | R | D | C | S);
}

#[test]
fn resume() {
    let mut f = fixture();
    f.clock.advance(600);
    f.svc.pause_task(STARTED_TASK).unwrap();
    f.clock.advance(60);
    let t = topics_of(&mut f, |s| {
        s.resume_task(STARTED_TASK).unwrap();
    });
    assert_eq!(t, T | R | D | S);
}

#[test]
fn stop_a_running_task_and_a_paused_one() {
    let mut f = fixture();
    f.clock.advance(600);
    let running = topics_of(&mut f, |s| s.stop_task(STARTED_TASK).unwrap());
    assert_eq!(running, T | R | D | C | S);
    let paused = topics_of(&mut f, |s| s.stop_task("t-label").unwrap());
    assert_eq!(paused, T, "no segment moves when a paused task stops");
}

#[test]
fn a_project_rename_never_touches_the_day_axis() {
    let mut f = fixture();
    let t = topics_of(&mut f, |s| {
        let p = s.file().data.projects["p-web"].clone();
        let input = ProjectInput {
            name: "Website 2026".to_owned(),
            color: p.color,
            billable: p.billable,
            budget_hours: p.budget_hours,
            client_zone: p.client_zone,
            billing_type: p.billing_type,
            fixed_fee_cents: p.fixed_fee_cents,
            fee_currency: p.fee_currency,
        };
        s.update_project("p-web", &input).unwrap();
    });
    assert_eq!(t, Topics::PROJECTS | T | S);
}

#[test]
fn a_project_colour_reaches_every_view_that_draws_it() {
    let mut f = fixture();
    let t = topics_of(&mut f, |s| {
        let p = s.file().data.projects["p-pack"].clone();
        let input = ProjectInput {
            name: p.name,
            color: "#d97706".to_owned(),
            billable: p.billable,
            budget_hours: p.budget_hours,
            client_zone: p.client_zone,
            billing_type: p.billing_type,
            fixed_fee_cents: p.fixed_fee_cents,
            fee_currency: p.fee_currency,
        };
        s.update_project("p-pack", &input).unwrap();
    });
    assert_eq!(t, Topics::PROJECTS | T | D | C | S);
}

#[test]
fn a_tag_change() {
    let mut f = fixture();
    let t = topics_of(&mut f, |s| {
        s.save_tag(&TagInput {
            id: Some("tag-concept".to_owned()),
            name: "Concepts".to_owned(),
            color: "#7c3aed".to_owned(),
        })
        .unwrap();
    });
    assert_eq!(t, Topics::TAGS | T | S);
}

#[test]
fn an_old_segment_moves_only_the_views_whose_span_it_touches() {
    // seg-10-0 is on Sunday 25 October: in the chips window, before this week, not on today's axis.
    let shorten = |s: &mut Service| {
        s.mutate(|file, _| {
            let seg = file.data.segments.get_mut("seg-10-0").unwrap();
            seg.end = seg.end.map(|e| e.later(-600));
            Ok(())
        })
        .unwrap();
    };
    let mut f = fixture();
    assert_eq!(topics_of(&mut f, shorten), T | C);
    let mut f = fixture();
    assert_eq!(
        topics_on(&mut f, date(2026, 10, 25), shorten),
        T | D | C,
        "with that day on the axis"
    );
}

/// Time before the day picker's page is on no chip, but it can move the first day with time, which says whether the
/// picker's left arrow (older days) works: `CHIPS` is set, so the worker recomputes the page.
#[test]
fn a_segment_before_the_pickers_page_reaches_the_picker() {
    let mut f = fixture();
    let t = topics_of(&mut f, |s| {
        s.add_manual_entry(&z_work_core::adjust::ManualEntryInput {
            task_id: "t-logo".to_owned(),
            start: z_work_core::Ts::parse("2026-10-15T09:00:00Z").unwrap(),
            end: z_work_core::Ts::parse("2026-10-15T10:00:00Z").unwrap(),
            reason: "Forgot to start the timer".to_owned(),
            subtask_id: None,
            allow_overlap: false,
        })
        .unwrap();
    });
    assert_eq!(t, T | C);
}

/// A timer running here since before the picker's page counts from its start for the first day with time: when it goes
/// (here straight from the data, as a merge or a restore could), `CHIPS` is set although it was open here in every
/// version that had it (review F2).
#[test]
fn a_segment_open_here_since_before_the_page_reaches_the_picker_when_it_goes() {
    let f = fixture();
    let mut before: LocalFile = f.svc.file().clone();
    let open = before
        .data
        .segments
        .values_mut()
        .find(|s| s.end.is_none())
        .unwrap();
    open.start = z_work_core::Ts::parse("2026-10-15T09:00:00Z").unwrap();
    let mut after = before.clone();
    after.data.segments.retain(|_, s| s.end.is_some());
    let t = diff(&before, &after, &scope_on(today()));
    assert!(t.contains(C), "{t:?}");
}

#[test]
fn settings_and_preferences() {
    let mut f = fixture();
    let settings = topics_of(&mut f, |s| {
        s.set_setting("showBoth", serde_json::json!(true)).unwrap()
    });
    assert_eq!(settings, Topics::SETTINGS);
    let prefs = topics_of(&mut f, |s| s.set_theme(Theme::Dark).unwrap());
    assert_eq!(prefs, Topics::PREFS);
}

#[test]
fn a_heartbeat_changes_no_topic() {
    let mut f = fixture();
    f.clock.advance(30);
    let rev = f.svc.revision();
    let t = topics_of(&mut f, |s| s.heartbeat().unwrap());
    assert_eq!(t, Topics::NONE);
    assert_eq!(
        f.svc.revision(),
        rev,
        "and no revision either: the worker does not even diff"
    );
}

#[test]
fn a_sleep_gap_in_the_running_segment_moves_the_running_start() {
    let mut f = fixture();
    // Ten minutes with no activity tick: the core takes them for a sleep and cuts them out of the running piece.
    f.clock.advance(600);
    let t = topics_of(&mut f, |s| {
        s.observe_activity(None).unwrap();
    });
    assert_eq!(t, T | R | D | C | S);
}

#[test]
fn renaming_the_running_task_moves_the_running_name() {
    let mut f = fixture();
    let t = topics_of(&mut f, |s| {
        let task = s.file().data.tasks[STARTED_TASK].clone();
        let input = z_work_core::machine::TaskInput {
            name: "Social templates, v2".to_owned(),
            project_id: task.project_id,
            description: task.description,
            continued_from: None,
            tags: task.tags,
        };
        s.update_task(STARTED_TASK, &input).unwrap();
    });
    // DAY: the day axis' blocks are labelled with the task's name (review R3).
    assert_eq!(t, T | R | D | S);
}
