//! Sample data for milestone 1: a deterministic store built from a moment and a zone, written with the core's
//! `store::seed_store` and opened with `Service::open`. The real data folder is never involved: the caller passes
//! a folder that [`crate::datadir`] has already checked.
//!
//! The store holds 3 projects in 1.x palette colours, the core's starter tags, 10 tasks (drafts, paused and stopped
//! ones, several with subtask lists), and 14 days of closed segments, several of them today and some on subtasks.
//! It is seeded with nothing open, because opening closes an open segment at the last heartbeat (crash recovery);
//! [`open_or_seed`] then starts [`STARTED_TASK`], a draft that has every field a cold launch asks for, so exactly
//! one task runs once a fresh sample store is open. That start also makes the launch warm.

use std::path::Path;
use std::sync::Arc;

use chrono::{Datelike, Days, NaiveDate, NaiveTime, TimeZone, Weekday};
use chrono_tz::Tz;
use serde_json::{Value, json};
use z_work_core::model::{
    LocalFile, LocalPrefs, PauseReason, Project, SCHEMA_VERSION, Segment, Shared, Stamp, Task,
    TaskState,
};
use z_work_core::persist::MAIN_FILE;
use z_work_core::service::Service;
use z_work_core::store::{DB_FILE, seed_store};
use z_work_core::validate::parse_zone;
use z_work_core::{Clock, CoreError, Ts};

/// The sample store's device id (a fixed UUID, so a rebuilt sample is byte-for-byte the same).
pub const DEVICE_ID: &str = "5a3b1e00-0000-4000-8000-000000000001";
/// The sample store's device name.
pub const DEVICE_NAME: &str = "Sample computer";
/// The draft started right after a fresh sample store opens.
pub const STARTED_TASK: &str = "t-social";
/// How many days back the closed segments reach, today included.
pub const DAYS: u64 = 14;

const PROJECTS: [(&str, &str, &str); 3] = [
    ("p-brand", "Brand refresh", "#2563eb"),
    ("p-web", "Website", "#dc2626"),
    ("p-pack", "Packaging", "#059669"),
];

struct TaskSpec {
    id: &'static str,
    name: &'static str,
    project: &'static str,
    state: TaskState,
    tags: &'static [&'static str],
    subtasks: &'static [&'static str],
    about: &'static str,
}

const TASKS: [TaskSpec; 10] = [
    TaskSpec {
        id: "t-logo",
        name: "Logo concepts",
        project: "p-brand",
        state: TaskState::Paused,
        tags: &["tag-concept"],
        subtasks: &["Sketch three directions", "Pick a typeface", "Colour study"],
        about: "Three logo directions for the brand refresh.",
    },
    TaskSpec {
        id: "t-guide",
        name: "Brand guidelines",
        project: "p-brand",
        state: TaskState::Paused,
        tags: &["tag-revision"],
        subtasks: &[],
        about: "Colours, type and logo use in one document.",
    },
    TaskSpec {
        id: STARTED_TASK,
        name: "Social templates",
        project: "p-brand",
        state: TaskState::Draft,
        tags: &["tag-concept", "tag-admin"],
        subtasks: &["Square post", "Banner"],
        about: "Templates for the launch posts.",
    },
    TaskSpec {
        id: "t-home",
        name: "Homepage layout",
        project: "p-web",
        state: TaskState::Stopped,
        tags: &["tag-revision"],
        subtasks: &["Wireframe", "Hero section", "Footer"],
        about: "New homepage with the refreshed brand.",
    },
    TaskSpec {
        id: "t-blog",
        name: "Blog redesign",
        project: "p-web",
        state: TaskState::Paused,
        tags: &["tag-concept", "tag-revision"],
        subtasks: &[],
        about: "Article and list pages.",
    },
    TaskSpec {
        id: "t-a11y",
        name: "Accessibility fixes",
        project: "p-web",
        state: TaskState::Draft,
        tags: &["tag-revision"],
        subtasks: &["Contrast", "Focus order"],
        about: "Findings of the accessibility review.",
    },
    TaskSpec {
        id: "t-box",
        name: "Box dieline",
        project: "p-pack",
        state: TaskState::Stopped,
        tags: &["tag-concept"],
        subtasks: &[],
        about: "Dieline for the gift box.",
    },
    TaskSpec {
        id: "t-label",
        name: "Label artwork",
        project: "p-pack",
        state: TaskState::Paused,
        tags: &["tag-revision"],
        subtasks: &["Front label", "Back label"],
        about: "Labels for the three jar sizes.",
    },
    TaskSpec {
        id: "t-call",
        name: "Supplier call notes",
        project: "p-pack",
        state: TaskState::Stopped,
        tags: &["tag-client-call"],
        subtasks: &[],
        about: "Print run and paper stock.",
    },
    TaskSpec {
        id: "t-plan",
        name: "Weekly planning",
        project: "p-web",
        state: TaskState::Stopped,
        tags: &["tag-admin"],
        subtasks: &[],
        about: "Plan of the week.",
    },
];

/// Tasks with logged time, in the order the past days take them.
const WORKED: [&str; 8] = [
    "t-logo", "t-guide", "t-home", "t-blog", "t-box", "t-label", "t-call", "t-plan",
];

/// Work slots of a past weekday: local start (hour, minute) and base length in minutes. A weekend day has the
/// first slot only.
const SLOTS: [(u32, u32, i64); 4] = [(9, 0, 75), (10, 45, 50), (13, 30, 90), (15, 30, 45)];

/// Today, newest first: task, minutes, subtask index, then the gap in minutes before the next (older) one. The
/// newest ends 5 minutes before `now`; a piece that would begin before local midnight is left out.
const TODAY: [(&str, i64, Option<usize>, i64); 4] = [
    ("t-logo", 40, Some(1), 10),
    ("t-home", 30, None, 10),
    ("t-box", 45, None, 15),
    ("t-label", 25, Some(0), 0),
];

fn stamp(at: Ts) -> Stamp {
    Stamp {
        at,
        by: DEVICE_ID.to_owned(),
    }
}

fn subtask_id(task: &str, index: usize) -> String {
    // The core's `subtasks::stable_ids` form, so opening the store leaves the ids as they are.
    format!("{task}-{index}")
}

/// A TipTap document: one paragraph, then a task list of the subtasks with their ids.
fn description(spec: &TaskSpec) -> Value {
    let mut content = vec![json!({
        "type": "paragraph",
        "content": [{ "type": "text", "text": spec.about }]
    })];
    if !spec.subtasks.is_empty() {
        let items: Vec<Value> = spec
            .subtasks
            .iter()
            .enumerate()
            .map(|(i, text)| {
                json!({
                    "type": "taskItem",
                    "attrs": { "checked": false, "subtaskId": subtask_id(spec.id, i) },
                    "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": text }] }]
                })
            })
            .collect();
        content.push(json!({ "type": "taskList", "content": items }));
    }
    json!({ "type": "doc", "content": content })
}

fn local(day: NaiveDate, hour: u32, minute: u32, zone: Tz) -> Option<Ts> {
    let time = NaiveTime::from_hms_opt(hour, minute, 0)?;
    let at = zone.from_local_datetime(&day.and_time(time)).earliest()?;
    Some(Ts(at.timestamp()))
}

fn spec_of(task: &str) -> Option<&'static TaskSpec> {
    TASKS.iter().find(|t| t.id == task)
}

fn segment(id: String, task: &str, start: Ts, end: Ts, subtask: Option<usize>) -> Segment {
    Segment {
        id,
        task_id: task.to_owned(),
        device_id: DEVICE_ID.to_owned(),
        start,
        end: Some(end),
        manual: false,
        subtask_id: subtask.map(|i| subtask_id(task, i)),
        edit: None,
        edit_stamp: stamp(end),
    }
}

/// The closed segments: the past 13 days on a fixed rota, then today's, which end before `now`.
fn segments(now: Ts, zone: Tz) -> Vec<Segment> {
    let today = now.datetime().with_timezone(&zone).date_naive();
    let mut out = Vec::new();
    for back in 1..DAYS {
        let Some(day) = today.checked_sub_days(Days::new(back)) else {
            continue;
        };
        let weekend = matches!(day.weekday(), Weekday::Sat | Weekday::Sun);
        let slots = SLOTS.iter().take(if weekend { 1 } else { SLOTS.len() });
        for (k, &(hour, minute, minutes)) in (0u64..).zip(slots) {
            let Some(start) = local(day, hour, minute, zone) else {
                continue;
            };
            let pick = usize::try_from((back * 3 + k) % 8).unwrap_or(0);
            let Some(task) = WORKED.get(pick).copied() else {
                continue;
            };
            let length = minutes + i64::try_from((back * 7 + k * 5) % 20).unwrap_or(0);
            let subtask = spec_of(task)
                .map(|s| s.subtasks.len())
                .filter(|&n| n > 0 && (back + k) % 2 == 0)
                .map(|n| usize::try_from(back).unwrap_or(0) % n);
            let id = format!("seg-{back:02}-{k}");
            out.push(segment(id, task, start, start.later(length * 60), subtask));
        }
    }
    let midnight = local(today, 0, 0, zone).unwrap_or(Ts(i64::MAX));
    let mut end = now.later(-5 * 60);
    for (k, &(task, minutes, subtask, gap)) in TODAY.iter().enumerate() {
        let start = end.later(-minutes * 60);
        if start < midnight {
            break;
        }
        out.push(segment(format!("seg-00-{k}"), task, start, end, subtask));
        end = start.later(-gap * 60);
    }
    out
}

/// The sample store as of `now`, with days and times of day in `zone`. The same arguments give the same file.
#[must_use]
pub fn build(now: Ts, zone: Tz) -> LocalFile {
    let mut data = Shared::default();
    // The core's own starter tags: Concept, Revision, Client call, Admin.
    data.seed_tags();
    let month_ago = now.later(-30 * 86_400);
    for (id, name, color) in PROJECTS {
        data.projects.insert(
            id.to_owned(),
            Project {
                id: id.to_owned(),
                name: name.to_owned(),
                color: color.to_owned(),
                archived: false,
                deleted: false,
                billable: true,
                budget_hours: None,
                client_zone: None,
                billing_type: z_work_core::model::BillingType::Hourly,
                fixed_fee_cents: None,
                fee_currency: None,
                created_at: month_ago,
                stamp: stamp(month_ago),
                identity_stamp: Some(stamp(month_ago)),
                billing_stamp: Some(stamp(month_ago)),
                budget_stamp: Some(stamp(month_ago)),
                archive_stamp: Some(stamp(month_ago)),
            },
        );
    }
    let segments = segments(now, zone);
    for spec in &TASKS {
        let created = if spec.state == TaskState::Draft {
            now.later(-86_400)
        } else {
            now.later(-20 * 86_400)
        };
        let last_end = segments
            .iter()
            .filter(|s| s.task_id == spec.id)
            .filter_map(|s| s.end)
            .max();
        let pause_reason = match (spec.state, spec.id) {
            (TaskState::Paused, "t-guide") => Some(PauseReason::AutoPaused {
                by_task_id: "t-logo".to_owned(),
            }),
            (TaskState::Paused, "t-blog") => Some(PauseReason::AppQuit),
            (TaskState::Paused, _) => Some(PauseReason::User),
            _ => None,
        };
        data.tasks.insert(
            spec.id.to_owned(),
            Task {
                id: spec.id.to_owned(),
                name: spec.name.to_owned(),
                name_stamp: stamp(created),
                project_id: spec.project.to_owned(),
                project_stamp: stamp(created),
                description: description(spec),
                description_stamp: stamp(created),
                deleted: false,
                deleted_stamp: stamp(created),
                discarded: None,
                continued_from: None,
                created_at: created,
                state: spec.state,
                pause_reason,
                state_stamp: stamp(last_end.unwrap_or(created)),
                stopped_at: (spec.state == TaskState::Stopped).then(|| last_end.unwrap_or(created)),
                tags: spec.tags.iter().map(|t| (*t).to_owned()).collect(),
                tags_stamp: stamp(created),
                active_subtask: None,
                active_subtask_stamp: stamp(created),
            },
        );
    }
    for s in segments {
        data.segments.insert(s.id.clone(), s);
    }
    LocalFile {
        version: SCHEMA_VERSION,
        device_id: DEVICE_ID.to_owned(),
        device_name: DEVICE_NAME.to_owned(),
        last_heartbeat: None,
        prefs: LocalPrefs {
            // A sample store skips the first-run setup of later milestones.
            setup_done: true,
            ..LocalPrefs::default()
        },
        remote_devices: std::collections::BTreeMap::new(),
        newest_stamp: None,
        latest_change_here: None,
        data,
    }
}

/// An open store and how it came to be.
pub struct Opened {
    pub service: Service,
    /// True when the folder had no store and was seeded with [`build`]; [`STARTED_TASK`] then runs.
    pub seeded: bool,
}

/// True when `dir` holds a store already: the database, or a 1.x JSON file the core would migrate.
pub fn has_store(dir: &Path) -> Result<bool, CoreError> {
    let db = dir.join(DB_FILE).try_exists().map_err(CoreError::Read)?;
    let legacy = dir.join(MAIN_FILE).try_exists().map_err(CoreError::Read)?;
    Ok(db || legacy)
}

/// Opens the store in `dir`, seeding it first when it has none. `system_zone` is the computer's zone (an IANA
/// name; an unknown one counts as UTC): the sample's days follow it, and the service views data in it.
///
/// A seeded store then has [`STARTED_TASK`] started, so exactly one task runs; a store that was there already
/// opens as it is, cold (Resume is refused until a task starts, as 1.x does after a restart).
pub fn open_or_seed(
    dir: &Path,
    clock: Arc<dyn Clock>,
    system_zone: &str,
) -> Result<Opened, CoreError> {
    let (zone, zone_name) = match parse_zone(system_zone) {
        Ok(z) => (z, system_zone.to_owned()),
        Err(_) => (chrono_tz::UTC, "UTC".to_owned()),
    };
    let seeded = !has_store(dir)?;
    if seeded {
        let now = clock.now();
        seed_store(dir, &build(now, zone), now)?;
    }
    let mut service = Service::open(dir, clock, DEVICE_NAME)?;
    service.set_system_zone(zone_name);
    if seeded {
        service.start_task(STARTED_TASK)?;
    }
    Ok(Opened { service, seeded })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use z_work_core::ManualClock;

    const ZONE: Tz = chrono_tz::Europe::Lisbon;
    /// A Wednesday, 10:30 in Lisbon (UTC+0 in November).
    const NOW: &str = "2026-11-04T10:30:00Z";

    fn now() -> Ts {
        Ts::parse(NOW).unwrap()
    }

    #[test]
    fn the_same_moment_and_zone_build_the_same_store() {
        assert_eq!(build(now(), ZONE), build(now(), ZONE));
        assert_ne!(build(now(), ZONE), build(now().later(86_400), ZONE));
    }

    #[test]
    fn the_sample_has_what_milestone_1_shows() {
        let f = build(now(), ZONE);
        let d = &f.data;
        assert_eq!(d.projects.len(), 3);
        let colors: BTreeSet<&str> = d.projects.values().map(|p| p.color.as_str()).collect();
        assert_eq!(colors.len(), 3, "distinct colours");
        assert_eq!(d.tasks.len(), 10);
        for state in [TaskState::Draft, TaskState::Paused, TaskState::Stopped] {
            assert!(d.tasks.values().any(|t| t.state == state), "{state:?}");
        }
        let tag_ids: BTreeSet<&str> = d.tags.keys().map(String::as_str).collect();
        assert_eq!(
            tag_ids,
            BTreeSet::from([
                "tag-admin",
                "tag-client-call",
                "tag-concept",
                "tag-revision"
            ]),
            "the core's starter tags"
        );
        assert!(d.tasks.values().all(|t| !t.tags.is_empty()));
        let with_subtasks = d
            .tasks
            .values()
            .filter(|t| !z_work_core::subtasks::items(&t.description).is_empty())
            .count();
        assert!(with_subtasks >= 4, "{with_subtasks}");
        assert!(d.segments.values().all(|s| s.end.is_some()), "nothing open");
        assert!(
            d.segments.values().all(|s| s.end <= Some(now())),
            "nothing ends after now"
        );
        let days: BTreeSet<NaiveDate> = d
            .segments
            .values()
            .map(|s| s.start.datetime().with_timezone(&ZONE).date_naive())
            .collect();
        assert_eq!(days.len(), 14, "{days:?}");
        let today = now().datetime().with_timezone(&ZONE).date_naive();
        let today_segments: Vec<&Segment> = d
            .segments
            .values()
            .filter(|s| s.start.datetime().with_timezone(&ZONE).date_naive() == today)
            .collect();
        assert!(today_segments.len() >= 3, "several today");
        let today_projects: BTreeSet<&str> = today_segments
            .iter()
            .filter_map(|s| d.tasks.get(&s.task_id))
            .map(|t| t.project_id.as_str())
            .collect();
        assert_eq!(today_projects.len(), 3, "today spans the three projects");
        assert!(today_segments.iter().any(|s| s.subtask_id.is_some()));
        // Every subtask a segment names exists in its task's list.
        for s in d.segments.values() {
            if let Some(sub) = &s.subtask_id {
                let items = z_work_core::subtasks::items(&d.tasks[&s.task_id].description);
                assert!(items.iter().any(|i| &i.id == sub), "{sub}");
            }
        }
        // No two segments overlap.
        let mut spans: Vec<(Ts, Ts)> = d
            .segments
            .values()
            .map(|s| (s.start, s.end.unwrap()))
            .collect();
        spans.sort();
        assert!(spans.windows(2).all(|w| w[0].1 <= w[1].0));
    }

    #[test]
    fn seeded_after_midnight_today_keeps_only_what_fits() {
        let early = Ts::parse("2026-11-04T00:50:00Z").unwrap();
        let f = build(early, ZONE);
        let today = early.datetime().with_timezone(&ZONE).date_naive();
        let midnight = local(today, 0, 0, ZONE).unwrap();
        let today_starts: Vec<Ts> = f
            .data
            .segments
            .values()
            .map(|s| s.start)
            .filter(|&s| s >= midnight)
            .collect();
        assert_eq!(today_starts.len(), 1, "only the newest fits in 45 minutes");
        assert!(f.data.segments.values().all(|s| s.end <= Some(early)));
    }

    #[test]
    fn a_fresh_folder_is_seeded_and_opens_with_exactly_one_task_running() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(NOW);
        let opened = open_or_seed(dir.path(), Arc::new(clock.clone()), "Europe/Lisbon").unwrap();
        assert!(opened.seeded);
        let svc = opened.service;
        let running: Vec<&Task> = svc
            .file()
            .data
            .tasks
            .values()
            .filter(|t| t.state == TaskState::Running)
            .collect();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, STARTED_TASK);
        assert_eq!(svc.file().data.own_open_segments(DEVICE_ID).len(), 1);
        assert!(svc.is_warm());
        for state in [TaskState::Draft, TaskState::Paused, TaskState::Stopped] {
            assert!(
                svc.file().data.tasks.values().any(|t| t.state == state),
                "{state:?}"
            );
        }
        // Nothing the sample holds was dropped or rewritten on the way through the store.
        let built = build(clock.now(), ZONE);
        assert_eq!(
            svc.file().data.segments.len(),
            built.data.segments.len() + 1
        );
        assert_eq!(svc.file().data.projects, built.data.projects);
        assert_eq!(svc.viewing_zone(), ZONE);
    }

    #[test]
    fn a_folder_with_a_store_opens_as_it_is_and_cold() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(NOW);
        let first = open_or_seed(dir.path(), Arc::new(clock.clone()), "Europe/Lisbon").unwrap();
        let mut svc = first.service;
        clock.advance(600);
        svc.on_exit().unwrap();
        drop(svc);
        clock.advance(60);
        let again = open_or_seed(dir.path(), Arc::new(clock.clone()), "Europe/Lisbon").unwrap();
        assert!(!again.seeded);
        let svc = again.service;
        assert!(!svc.is_warm());
        assert!(
            svc.file()
                .data
                .tasks
                .values()
                .all(|t| t.state != TaskState::Running)
        );
        assert_eq!(
            svc.file().data.tasks[STARTED_TASK].pause_reason,
            Some(PauseReason::AppQuit)
        );
    }

    #[test]
    fn an_unknown_zone_counts_as_utc() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(NOW);
        let opened = open_or_seed(dir.path(), Arc::new(clock), "Not/AZone").unwrap();
        assert_eq!(opened.service.viewing_zone(), chrono_tz::UTC);
    }
}
