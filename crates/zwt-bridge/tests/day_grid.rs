//! The increment of the day axis' sub-grid (2.0, owner 2026-11-04): the day view carries the rounding increment in
//! force on its day (1.x Settings > Rounding, the global rule; 15 minutes when none is saved or rounding is off), and
//! saving a rounding rule redraws the day axis and nothing else.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use std::sync::Arc;

use common::{NOW, ZONE, ZONE_NAME, date, one, spawn, today};
use z_work_core::ManualClock;
use z_work_core::machine::RoundingInput;
use z_work_core::model::{LocalFile, RoundMethod};
use z_work_core::service::Service;
use zwt_bridge::sample;
use zwt_bridge::topics::{Topics, diff};
use zwt_bridge::views::{day_view, scope_of, week_of};
use zwt_bridge::worker::{Command, Mutation};

/// A rounding rule of `minutes` from `from`, for every project or for `project`, saved on 4 November.
fn save(svc: &mut Service, minutes: u32, from: &str, project: Option<&str>) {
    let input = RoundingInput {
        project_id: project.map(str::to_owned),
        effective_from: from.to_owned(),
        increment_minutes: minutes,
        method: RoundMethod::Nearest,
        confirm_retroactive: true,
    };
    svc.set_rounding(&input, "2026-11-04").unwrap();
}

fn sample_service(dir: &std::path::Path) -> Service {
    let clock = ManualClock::at(NOW);
    sample::open_or_seed(dir, Arc::new(clock), ZONE_NAME)
        .unwrap()
        .service
}

fn increment_on(svc: &Service, day: chrono::NaiveDate) -> u32 {
    day_view(svc.file(), day, today(), ZONE)
        .head
        .increment_minutes
}

#[test]
fn the_day_view_carries_the_increment_in_force_on_its_day() {
    let dir = tempfile::tempdir().unwrap();
    let mut svc = sample_service(dir.path());
    assert_eq!(increment_on(&svc, today()), 15, "the sample saves no rule");
    save(&mut svc, 30, "2026-11-04", None);
    assert_eq!(increment_on(&svc, today()), 30);
    assert_eq!(
        increment_on(&svc, date(2026, 11, 3)),
        15,
        "the day before the rule"
    );
    save(&mut svc, 10, "2026-11-01", Some("p-brand"));
    assert_eq!(increment_on(&svc, today()), 30, "a project's own rule");
    save(&mut svc, 0, "2026-11-04", None);
    assert_eq!(increment_on(&svc, today()), 15, "rounding off: the default");
}

#[test]
fn saving_a_rounding_rule_moves_the_day_axis_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut svc = sample_service(dir.path());
    let scope = scope_of(today(), today(), &week_of(today(), ZONE), ZONE);
    let before: LocalFile = svc.file().clone();
    save(&mut svc, 30, "2026-11-04", None);
    assert_eq!(diff(&before, svc.file(), &scope), Topics::DAY);
}

#[test]
fn the_worker_redraws_the_day_axis_when_a_rounding_rule_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(NOW);
    let (worker, first, inbox) = spawn(dir.path(), &clock);
    assert_eq!(first.day.unwrap().head.increment_minutes, 15);
    worker
        .send(Command::Mutate(Mutation::new("rounding", |svc| {
            save(svc, 30, "2026-11-04", None);
            Ok(())
        })))
        .unwrap();
    let update = one(&worker, &inbox);
    assert_eq!(update.topics, Topics::DAY, "{update:#?}");
    let day = update.day.expect("a new day head");
    assert_eq!(day.head.increment_minutes, 30);
    assert!(update.tasks.is_none() && update.chips.is_none() && update.share.is_none());
    worker.shutdown().unwrap();
}
