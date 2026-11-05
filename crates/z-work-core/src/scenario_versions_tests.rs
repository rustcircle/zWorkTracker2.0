//! Two devices, a fake clock and a delayed folder: versions and restoring.
//!
//! DESKTOP and LAPTOP (TABLET where a third is needed) share cloud folders through `testkit`. A device's sync is a
//! `cycle` and then `versions::run_due`, as the app runs them, and its lease thread a `tick`. Where the lease isn't the
//! point, nobody ticks, so every device stays writable. Numbers are the design's test plan (section 12).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::adjust::ManualEntryInput;
use crate::backup;
use crate::epoch;
use crate::gz;
use crate::lease::LeaseStateName;
use crate::lease_io::{self, TakeOverExpected, TakeOverMode, TakeOverOutcome};
use crate::model::{
    DeviceFile, EpochInfo, GapKind, PauseReason, SCHEMA_VERSION, Shared, SyncProvider, SyncTarget,
    TaskState, VersionReason,
};
use crate::persist::Paths;
use crate::summary::SummaryRequest;
use crate::sync::{TargetStatus, devices_dir};
use crate::testkit::{Device, FakeCloud, LIMIT, pinput};
use crate::time::Ts;
use crate::versions::{self, VersionList, VersionName, version_name, versions_dir};

const MAIN: &str = "/cloud/OneDrive";
const SECOND: &str = "/cloud/GoogleDrive";
const OLD: &str = "/cloud/Old folder";
const MINE: LeaseStateName = LeaseStateName::HeldByMe;
const APP: &str = env!("CARGO_PKG_VERSION");
const SOMEONE: &str = "0e0e0e0e-0000-4000-8000-000000000000";

fn two() -> (Arc<FakeCloud>, Device, Device) {
    let cloud = FakeCloud::new(&[MAIN]);
    let a = Device::open(&cloud, "DESKTOP", &[MAIN]);
    let b = Device::open(&cloud, "LAPTOP", &[MAIN]);
    (cloud, a, b)
}

fn two_folders() -> (Arc<FakeCloud>, Device) {
    let cloud = FakeCloud::new(&[MAIN, SECOND]);
    let a = Device::open(&cloud, "DESKTOP", &[MAIN, SECOND]);
    (cloud, a)
}

/// A sync, then the versions that are due, as the app runs them after each.
fn sync(d: &Device) {
    d.cycle();
    versions::run_due(&d.svc, &d.view, LIMIT);
}

fn sync_all(devices: &[&Device]) {
    for _ in 0..2 {
        for d in devices {
            sync(d);
        }
    }
}

/// Time passes a minute at a time, and each device's lease thread does what is due.
fn minutes(cloud: &FakeCloud, devices: &[&Device], n: i64) {
    for _ in 0..n {
        cloud.advance(60);
        for d in devices {
            d.tick_if_due();
        }
    }
}

/// Starts timing a task, which keeps the lease, and holds it.
fn holding(d: &Device) -> (String, String) {
    let (p, t) = d.task("Client", "Design");
    d.s().start_task(&t).unwrap();
    d.tick();
    assert_eq!(d.state(), MINE);
    (p, t)
}

fn listed(d: &Device) -> VersionList {
    versions::list(&d.svc, &d.view, None, LIMIT)
}

/// The names of the versions made for `reason`, newest first.
fn named(d: &Device, reason: &str) -> Vec<String> {
    listed(d)
        .versions
        .into_iter()
        .filter(|v| v.reason == reason)
        .map(|v| v.name)
        .collect()
}

fn only(mut names: Vec<String>) -> String {
    assert_eq!(names.len(), 1, "{names:?}");
    names.remove(0)
}

fn roots_of(d: &Device, name: &str) -> Vec<String> {
    listed(d)
        .versions
        .into_iter()
        .find(|v| v.name == name)
        .map(|v| v.locations.into_iter().map(|l| l.root).collect())
        .unwrap_or_default()
}

fn file_in(root: &str, name: &str, ext: &str) -> PathBuf {
    let v = VersionName::parse(name).unwrap();
    versions_dir(Path::new(root))
        .join(v.month())
        .join(format!("{name}{ext}"))
}

/// What a version holds, as any unzip tool reads it.
fn opened(cloud: &FakeCloud, root: &str, name: &str) -> DeviceFile {
    let bytes = cloud.latest(&file_in(root, name, ".json.gz")).unwrap();
    serde_json::from_slice(&gz::decompress(&bytes, 1 << 26).unwrap()).unwrap()
}

fn changes_of(d: &Device, root: &str, name: &str) -> String {
    versions::read_changes(&d.view, Path::new(root), name).unwrap()
}

/// "Save a version now", and the version it saved.
fn save(d: &Device) -> String {
    versions::save_now(&d.svc, &d.view, LIMIT).unwrap();
    named(d, "manual").into_iter().next().unwrap()
}

fn restore(d: &Device, root: &str, name: &str) {
    versions::restore(&d.svc, &d.view, root, name, LIMIT).unwrap();
}

fn project_name(d: &Device, id: &str) -> String {
    d.s().file().data.projects[id].name.clone()
}

fn rename(d: &Device, id: &str, name: &str) {
    d.s().update_project(id, &pinput(name)).unwrap();
}

fn status(d: &Device, root: &str) -> TargetStatus {
    d.s()
        .snapshot()
        .sync
        .targets
        .into_iter()
        .find(|t| t.root == root)
        .unwrap()
        .status
}

/// A version file and its record, as another device left them, for what only the name decides.
fn plant(cloud: &FakeCloud, root: &str, at: Ts, reason: VersionReason) -> String {
    let name = version_name(at, SOMEONE, reason);
    cloud.put(&file_in(root, &name, ".json.gz"), b"not read here");
    cloud.put(&file_in(root, &name, ".changes.txt"), b"not read here");
    name
}

fn at(iso: &str) -> Ts {
    Ts::parse(iso).unwrap()
}

/// Test 31.
#[test]
fn version_conflict_copy_ignored_by_list_and_prune() {
    let (cloud, a, _) = two();
    let (p, _) = holding(&a);
    sync(&a);
    let first = only(named(&a, "auto"));
    let gz = file_in(MAIN, &first, ".json.gz");
    let bytes = cloud.latest(&gz).unwrap();
    // As cloud clients name them: on the last extension, or on the name.
    let mut copies = vec![
        cloud.conflict_copy(&gz, " (1)"),
        cloud.conflict_copy(&gz, "-LAPTOP"),
        gz.with_file_name(format!("{first} (1).json.gz")),
        gz.with_file_name(format!("{first}-PC2.json.gz")),
    ];
    for c in &copies[2..] {
        cloud.put(c, &bytes);
    }
    // Two old versions of one month, and a copy of an even older one there, which no rule may touch.
    let old = at("2025-06-10T10:00:00Z");
    let dropped = plant(&cloud, MAIN, old.later(-7200), VersionReason::Auto);
    let kept = plant(&cloud, MAIN, old.later(-3600), VersionReason::Auto);
    let old_copy = file_in(MAIN, &dropped, ".json.gz").with_file_name(format!(
        "{}.json (1).gz",
        version_name(old.later(-9000), SOMEONE, VersionReason::Auto)
    ));
    cloud.put(&old_copy, b"x");
    copies.push(old_copy);
    let names: Vec<String> = listed(&a).versions.into_iter().map(|v| v.name).collect();
    assert_eq!(names, [first.clone(), kept.clone(), dropped.clone()]);

    // The holder's next version thins out by the rule, and never touches a copy.
    rename(&a, &p, "Renamed");
    minutes(&cloud, &[&a], 61);
    sync(&a);
    assert!(!cloud.exists(&file_in(MAIN, &dropped, ".json.gz")));
    assert!(!cloud.exists(&file_in(MAIN, &dropped, ".changes.txt")));
    assert!(cloud.exists(&file_in(MAIN, &kept, ".json.gz")));
    for c in &copies {
        assert!(cloud.exists(c), "{}", c.display());
    }
    let left: Vec<String> = listed(&a).versions.into_iter().map(|v| v.name).collect();
    assert_eq!(left.len(), 3);
    assert!(left.contains(&first) && left.contains(&kept));
}

/// Test 44.
#[test]
fn snapshot_only_when_changed_and_holder() {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    let (p, _) = holding(&a);
    sync(&a);
    let first = only(named(&a, "auto"));
    assert!(first.starts_with("20260302T090000Z--"));
    assert_eq!(roots_of(&a, &first), [MAIN]);
    let text = changes_of(&a, MAIN, &first);
    assert!(
        text.starts_with(&format!(
            "zWorkTracker version saved 2 Mar 2026, 09:00 UTC\nSaved by DESKTOP (app {APP}). Reason: automatic hourly copy.\nThere is no earlier version to compare with, so this gives totals only.\n"
        )),
        "{text}"
    );
    let done = status(&a, MAIN);
    assert_eq!(done.last_version_at, Some(at("2026-03-02T09:00:00Z")));
    assert!(done.versions_bytes.is_some_and(|b| b > 0) && done.last_version_error.is_none());

    // Nothing changed, or changed within the hour: no new one.
    sync(&a);
    minutes(&cloud, &[&a], 30);
    rename(&a, &p, "Client A");
    sync(&a);
    assert_eq!(listed(&a).versions.len(), 1);
    // An hour after the last, with that change: one more, which says what changed since the first.
    minutes(&cloud, &[&a], 31);
    sync(&a);
    let autos = named(&a, "auto");
    assert_eq!(autos.len(), 2);
    let text = changes_of(&a, MAIN, &autos[0]);
    assert!(
        text.contains("Compared with the version from 09:00 UTC.\n"),
        "{text}"
    );
    assert!(
        text.contains("Changed since then: 1 project changed.\n"),
        "{text}"
    );
    // Long after, with no change, none.
    minutes(&cloud, &[&a], 90);
    sync(&a);
    assert_eq!(named(&a, "auto").len(), 2);

    // A computer that doesn't hold the lease writes none, though its data differs from any mark.
    sync(&b);
    b.tick();
    sync(&b);
    a.quit(LIMIT);
    b.tick();
    assert_eq!(b.state(), LeaseStateName::Free);
    minutes(&cloud, &[&b], 90);
    sync(&b);
    assert!(
        listed(&b)
            .versions
            .iter()
            .all(|v| v.device_name == "DESKTOP")
    );
    assert!(status(&b, MAIN).last_version_at.is_none());
}

/// The page's "when you close the app after changes": the lease released first or not.
#[test]
fn a_version_on_quitting_after_changes_once_the_last_is_a_quarter_hour_old() {
    let (cloud, a, _) = two();
    let (p, _) = holding(&a);
    sync(&a);
    rename(&a, &p, "Renamed");
    cloud.advance(600);
    versions::run_at_quit(&a.svc, &a.view, LIMIT);
    assert!(
        named(&a, "quit").is_empty(),
        "the last is only 10 minutes old"
    );
    cloud.advance(300);
    a.quit(LIMIT);
    assert_eq!(
        a.state(),
        LeaseStateName::Free,
        "released before the last sync"
    );
    sync(&a);
    let quit = only(named(&a, "quit"));
    assert!(changes_of(&a, MAIN, &quit).contains("Reason: copy when the app was closed."));
    // With nothing changed since, quitting again writes none.
    cloud.advance(3600);
    versions::run_at_quit(&a.svc, &a.view, LIMIT);
    assert_eq!(named(&a, "quit").len(), 1);
}

/// Test 45, and the holder's thinning out by the rule.
#[test]
fn read_only_never_snapshots_or_prunes() {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    let old = at("2025-11-10T10:00:00Z");
    let planted: Vec<String> = (0..3)
        .map(|h| plant(&cloud, MAIN, old.later(h * 3600), VersionReason::Auto))
        .collect();
    // A write that died long ago left its temporary file, and one under way now has its own.
    let dead = file_in(MAIN, &planted[0], ".json.gz")
        .with_file_name(format!(".{}.json.gz.tmp", planted[0]));
    let recent = version_name(cloud.now().later(-3600), SOMEONE, VersionReason::Auto);
    let under_way =
        file_in(MAIN, &recent, ".json.gz").with_file_name(format!(".{recent}.json.gz.tmp"));
    cloud.put(&dead, b"x");
    cloud.put(&under_way, b"x");
    let (_, t) = a.task("Client", "Design");
    a.s().start_task(&t).unwrap();
    a.tick();
    a.cycle();
    b.cycle();
    b.tick();
    assert_eq!(b.state(), LeaseStateName::HeldByOtherLive);

    // Whatever LAPTOP is asked, it writes no version and deletes none.
    sync(&b);
    versions::run_at_quit(&b.svc, &b.view, LIMIT);
    let refused = versions::save_now(&b.svc, &b.view, LIMIT).unwrap_err();
    assert_eq!(refused.code(), "read_only");
    let refused = versions::restore(&b.svc, &b.view, MAIN, &planted[2], LIMIT).unwrap_err();
    assert_eq!(refused.code(), "read_only");
    let preview = versions::preview_restore(&b.svc, &b.view, MAIN, &planted[2], LIMIT);
    assert!(
        preview.is_err(),
        "that one can't be read: it isn't a version"
    );
    assert_eq!(listed(&b).versions.len(), 3, "nothing written");
    for n in &planted {
        assert!(
            cloud.exists(&file_in(MAIN, n, ".json.gz")),
            "nothing deleted"
        );
    }
    assert!(cloud.exists(&dead));
    assert!(status(&b, MAIN).last_version_at.is_none());

    // The holder thins out after its first write: of that month only the newest stays, with its record.
    sync(&a);
    let left: Vec<String> = listed(&a).versions.into_iter().map(|v| v.name).collect();
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(left.contains(&planted[2]));
    for n in &planted[..2] {
        assert!(!cloud.exists(&file_in(MAIN, n, ".json.gz")));
        assert!(!cloud.exists(&file_in(MAIN, n, ".changes.txt")));
    }
    assert!(cloud.exists(&file_in(MAIN, &planted[2], ".changes.txt")));
    assert!(!cloud.exists(&dead), "a day old: gone");
    assert!(cloud.exists(&under_way), "an hour old: left alone");
}

/// D13: a computer that may change data but doesn't hold the lease can save a version by hand, and deletes none.
#[test]
fn only_the_holder_thins_out() {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    let old = at("2025-11-10T10:00:00Z");
    let planted: Vec<String> = (0..2)
        .map(|h| plant(&cloud, MAIN, old.later(h * 3600), VersionReason::Auto))
        .collect();
    b.task("Laptop work", "Notes");
    assert_eq!(b.state(), LeaseStateName::Free, "changed, not yet claimed");
    let by_hand = save(&b);
    assert_eq!(roots_of(&b, &by_hand), [MAIN]);
    assert!(cloud.exists(&file_in(MAIN, &planted[0], ".json.gz")));
    // Once it holds, its next version thins out by the rule.
    b.tick();
    assert_eq!(b.state(), MINE);
    cloud.advance(60);
    save(&b);
    assert!(!cloud.exists(&file_in(MAIN, &planted[0], ".json.gz")));
    assert!(cloud.exists(&file_in(MAIN, &planted[1], ".json.gz")));
    assert!(cloud.exists(&file_in(MAIN, &by_hand, ".json.gz")));
}

/// LAPTOP takes over from DESKTOP, which stopped responding while it held the lease with a timer running.
fn taken_over() -> (Arc<FakeCloud>, Device, Device) {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    holding(&a);
    sync_all(&[&a, &b]);
    b.tick();
    assert_eq!(b.state(), LeaseStateName::HeldByOtherLive);
    a.view.set_offline(true);
    minutes(&cloud, &[&b], 16);
    assert_eq!(b.state(), LeaseStateName::HeldByOtherStale);
    let preview = lease_io::preview_take_over(&b.svc, &b.view, LIMIT);
    let holder = preview.holder.unwrap();
    let done = lease_io::take_over(
        &b.svc,
        &b.view,
        TakeOverExpected {
            generation: holder.generation,
            beat_seq: holder.beat_seq,
        },
        TakeOverMode::Stale,
        LIMIT,
    )
    .unwrap();
    assert_eq!(done.outcome, TakeOverOutcome::Done);
    (cloud, a, b)
}

/// The page's "around a take over".
#[test]
fn a_take_over_saves_a_version_kept_a_year() {
    let (_cloud, _a, b) = taken_over();
    sync(&b);
    let name = only(named(&b, "takeover"));
    let info = listed(&b)
        .versions
        .into_iter()
        .find(|v| v.name == name)
        .unwrap();
    assert_eq!(info.device_name, "LAPTOP");
    assert!(info.pinned);
    let text = changes_of(&b, MAIN, &name);
    assert!(
        text.contains("Reason: copy when a computer took over."),
        "{text}"
    );
    assert!(
        text.contains("Compared with the version from 09:00 UTC."),
        "{text}"
    );
    assert!(
        text.contains(
            "LAPTOP  closed a time segment on \"Design\" for a computer that stopped responding"
        ),
        "{text}"
    );
}

/// "Save a version now" just after a take over doesn't take the place of the version the take over asked for, which is
/// the one kept a year.
#[test]
fn saving_by_hand_keeps_the_version_a_take_over_asked_for() {
    let (_cloud, _a, b) = taken_over();
    let by_hand = save(&b);
    sync(&b);
    let kept = only(named(&b, "takeover"));
    assert_ne!(kept, by_hand);
    assert_eq!(roots_of(&b, &kept), [MAIN]);
}

/// A deterministic store like the performance fixture's: 12 projects, 400 tasks, segments over three years, with ids as
/// random as real ones.
fn synthetic(segments: usize) -> DeviceFile {
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, bound: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) % bound
        }
        fn id(&mut self) -> String {
            let mut n = 0u128;
            for _ in 0..4 {
                n = (n << 32) | u128::from(self.next(1 << 32));
            }
            uuid::Uuid::from_u128(n).to_string()
        }
    }
    const DEVICE: &str = "11111111-1111-4111-8111-111111111111";
    const START: i64 = 1_704_067_200;
    const WORDS: [&str; 12] = [
        "review", "client", "draft", "meeting", "invoice", "design", "fix", "call", "notes",
        "plan", "report", "budget",
    ];
    let mut rng = Lcg(0x2545_f491_4f6c_dd1d);
    let iso = |s: i64| Ts(s).iso();
    let stamp = |s: i64| json!({"at": iso(s), "by": DEVICE});
    let words = |rng: &mut Lcg, n: u64| {
        (0..n)
            .map(|_| WORDS[usize::try_from(rng.next(12)).unwrap()])
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut projects = serde_json::Map::new();
    let project_ids: Vec<String> = (0..12).map(|_| rng.id()).collect();
    for (i, id) in project_ids.iter().enumerate() {
        projects.insert(id.clone(), json!({"id": id, "name": format!("Client {i}"), "color": format!("#{:06x}", rng.next(0xff_ffff)), "billable": true, "createdAt": iso(START), "stamp": stamp(START)}));
    }
    let mut tasks = serde_json::Map::new();
    let task_ids: Vec<String> = (0..400).map(|_| rng.id()).collect();
    for (i, id) in task_ids.iter().enumerate() {
        let at = START + i64::try_from(i).unwrap() * 3600;
        let text = words(&mut rng, 12);
        tasks.insert(id.clone(), json!({
            "id": id, "name": format!("Task {i} {}", words(&mut rng, 3)), "nameStamp": stamp(at),
            "projectId": project_ids[i % 12], "projectStamp": stamp(at),
            "description": {"type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": text}]}]},
            "descriptionStamp": stamp(at), "deleted": false, "deletedStamp": stamp(at), "createdAt": iso(at),
            "state": "paused", "pauseReason": {"kind": "user"}, "stateStamp": stamp(at), "tags": ["tag-concept"], "tagsStamp": stamp(at),
        }));
    }
    let mut segs = serde_json::Map::new();
    let mut audit = serde_json::Map::new();
    for i in 0..segments {
        let id = rng.id();
        let start = START + i64::try_from(rng.next(3 * 365 * 86_400)).unwrap();
        let end = start + 600 + i64::try_from(rng.next(3 * 3600)).unwrap();
        let task = &task_ids[usize::try_from(rng.next(400)).unwrap()];
        let mut seg = json!({"id": id, "taskId": task, "deviceId": DEVICE, "start": iso(start), "end": iso(end), "editStamp": stamp(start)});
        if i % 10 == 0 {
            let edit =
                json!({"start": iso(start + 300), "end": iso(end), "reason": words(&mut rng, 3)});
            seg["edit"] = edit.clone();
            let entry = rng.id();
            audit.insert(entry.clone(), json!({"id": entry, "at": iso(end + 60), "by": DEVICE, "entity": "segment", "entityId": id, "action": "correct", "reason": edit["reason"], "before": {"rawStart": iso(start), "rawEnd": iso(end), "edit": null}, "after": {"rawStart": iso(start), "rawEnd": iso(end), "edit": edit}}));
        }
        segs.insert(id, seg);
    }
    let data: Shared = serde_json::from_value(
        json!({"projects": projects, "tasks": tasks, "segments": segs, "auditLog": audit}),
    )
    .unwrap();
    DeviceFile {
        schema_version: SCHEMA_VERSION,
        device_id: DEVICE.to_owned(),
        device_name: "DESKTOP".to_owned(),
        written_at: Ts(START + 3 * 365 * 86_400),
        last_heartbeat: None,
        epoch_info: None,
        data,
    }
}

/// Test 48.
#[test]
fn compression_ratio_on_perf_fixture_at_least_4x() {
    let json = serde_json::to_vec(&synthetic(5_000)).unwrap();
    let packed = gz::compress(&json);
    let ratio = json.len() as f64 / packed.len() as f64;
    eprintln!(
        "{} bytes of JSON, {} gzipped: {ratio:.1}x",
        json.len(),
        packed.len()
    );
    assert!(ratio >= 4.0, "{ratio:.2}");
    assert_eq!(gz::decompress(&packed, json.len()).unwrap(), json);
}

/// Test 49.
#[test]
fn restore_replaces_data_including_tombstones_discarded_stopped_voided() {
    let (cloud, a, b) = two();
    let (client, design) = a.task("Client", "Design");
    let spare = a.s().create_project(&pinput("Spare")).unwrap();
    let (_, notes) = a.task("Other", "Notes");
    a.s().start_task(&design).unwrap();
    cloud.advance(1800);
    a.s().pause_task(&design).unwrap();
    let now = a.s().now();
    a.s()
        .add_manual_entry(&ManualEntryInput {
            task_id: notes.clone(),
            start: now.later(-1200),
            end: now.later(-600),
            reason: "forgot".into(),
            subtask_id: None,
            allow_overlap: true,
        })
        .unwrap();
    let summary = a
        .s()
        .create_summary(&SummaryRequest {
            project_id: client,
            from: "2026-03-02".into(),
            to: "2026-03-02".into(),
            zone: "UTC".into(),
        })
        .unwrap();
    sync_all(&[&a, &b]);
    let version = save(&a);

    // What no merge can take back: a deleted project, a task deleted with its time, a stopped task, a voided summary.
    cloud.advance(60);
    a.s().delete_project(&spare).unwrap();
    a.s().delete_task(&notes).unwrap();
    a.s().void_summary(&summary).unwrap();
    a.s().stop_task(&design).unwrap();
    sync_all(&[&a, &b]);
    assert_eq!(b.s().file().data.tasks[&design].state, TaskState::Stopped);
    cloud.advance(60);
    restore(&a, MAIN, &version);
    let check = |d: &Device| {
        let s = d.s();
        let data = &s.file().data;
        assert!(!data.projects[&spare].deleted, "{}", d.name());
        assert!(!data.tasks[&notes].deleted && data.tasks[&notes].discarded.is_none());
        assert_eq!(data.tasks[&design].state, TaskState::Paused);
        assert!(!data.summaries[&summary].voided);
    };
    check(&a);
    sync_all(&[&a, &b]);
    check(&a);
    check(&b);
}

/// Test 50.
#[test]
fn restore_makes_local_safety_copy_and_cloud_pre_restore() {
    let (cloud, a) = two_folders();
    let (p, t) = a.task("Client", "Design");
    a.s().start_task(&t).unwrap();
    sync_all(&[&a]);
    cloud.advance(600);
    let version = save(&a);
    let taken = VersionName::parse(&version).unwrap().taken_at;
    cloud.advance(600);
    rename(&a, &p, "Renamed since");
    let running = a.running();
    cloud.advance(300);
    let me = a.id();
    restore(&a, MAIN, &version);
    let now = a.s().now();
    {
        // The data of the version here, its timer paused at the moment it was taken, in one save with the new epoch.
        let s = a.s();
        let data = &s.file().data;
        assert_eq!(data.projects[&p].name, "Client");
        assert_eq!(data.segments[&running].end, Some(taken));
        assert_eq!(data.tasks[&t].state, TaskState::Paused);
        assert_eq!(
            data.tasks[&t].pause_reason,
            Some(PauseReason::BackupRestored)
        );
        let epoch = s.file().prefs.epoch.clone().unwrap();
        assert_eq!(
            (
                epoch.epoch,
                epoch.by,
                epoch.restored_from,
                epoch.base_taken_at,
                epoch.at
            ),
            (1, me, version.clone(), taken, now)
        );
    }
    // A copy of today's data went to both folders first, with the timer as it was then, and is kept a year.
    let pre = only(named(&a, "pre-restore"));
    assert_eq!(VersionName::parse(&pre).unwrap().taken_at, now);
    assert_eq!(roots_of(&a, &pre), [MAIN, SECOND]);
    assert!(
        listed(&a)
            .versions
            .iter()
            .find(|v| v.name == pre)
            .unwrap()
            .pinned
    );
    for root in [MAIN, SECOND] {
        let kept = opened(&cloud, root, &pre);
        assert_eq!(kept.data.projects[&p].name, "Renamed since");
        assert_eq!(kept.data.segments[&running].end, None);
    }
    // And here, as a safety copy, with the timer paused at the restore.
    let audit = a
        .s()
        .audit_entries(Some(&version))
        .into_iter()
        .find(|e| e.action == "restore")
        .unwrap();
    assert_eq!(audit.entity, "version");
    assert_eq!(audit.after["preRestore"], json!(pre));
    assert_eq!(audit.after["epoch"], json!(1));
    let safety = audit.after["safetyCopy"].as_str().unwrap().to_owned();
    let copy = backup::read(&Paths::new(a.s().data_dir()), &safety).unwrap();
    assert_eq!(copy.data.projects[&p].name, "Renamed since");
    assert_eq!(copy.data.segments[&running].end, Some(now));
    let notices = a.notices();
    assert!(notices.contains(&format!("Restored the version from 2026-03-02 09:10. Other computers that sync with these folders follow it at their next sync. Changes from a computer on an older app version are skipped until it is updated. The data from just before is kept as {safety}, and as the version of 2026-03-02 09:25 in your sync folders, so this can be undone by restoring that.")), "{notices:?}");
    assert!(notices.contains(&"Design was running when this version was taken. It is paused at the moment the version was taken (2026-03-02 09:10).".to_owned()), "{notices:?}");

    // The restored data is saved as a version of its own at the next sync, in both folders.
    sync(&a);
    let restored = only(named(&a, "restored"));
    assert_eq!(roots_of(&a, &restored), [MAIN, SECOND]);
    let text = changes_of(&a, SECOND, &restored);
    assert!(
        text.contains("Reason: copy of the restored data."),
        "{text}"
    );
    assert!(
        text.contains("Compared with the version from 09:25 UTC."),
        "{text}"
    );
    assert!(text.contains("Epoch 1."), "{text}");
    assert!(text.contains("restored the version "), "{text}");
}

/// Test 51.
#[test]
fn restore_refused_when_no_folder_can_take_the_pre_restore_copy() {
    let (cloud, a) = two_folders();
    let (p, t) = a.task("Client", "Design");
    sync_all(&[&a]);
    let version = save(&a);
    cloud.advance(60);
    rename(&a, &p, "Renamed since");
    a.s().start_task(&t).unwrap();
    let before = a.s().stored();
    for root in [MAIN, SECOND] {
        a.view.set_full(root, true);
    }
    let refused = versions::restore(&a.svc, &a.view, MAIN, &version, LIMIT).unwrap_err();
    assert_eq!(refused.code(), "conflict");
    assert_eq!(
        refused.to_string(),
        "Couldn't save a copy of today's data to any folder first, so nothing was changed."
    );
    assert_eq!(a.s().stored(), before, "nothing saved here");
    assert_eq!(project_name(&a, &p), "Renamed since");
    assert!(a.s().file().prefs.epoch.is_none());
    a.running();
    assert!(named(&a, "pre-restore").is_empty());
    assert_eq!(
        status(&a, MAIN).last_version_error.as_deref(),
        Some("Could not write the record of changes: There is not enough space on the disk.")
    );

    // Nothing is left asked for: once the folders take versions again, none says a restore happened.
    for root in [MAIN, SECOND] {
        a.view.set_full(root, false);
    }
    sync(&a);
    assert!(named(&a, "pre-restore").is_empty());
    assert_eq!(project_name(&a, &p), "Renamed since");

    // One folder that takes it is enough.
    a.view.set_full(MAIN, true);
    restore(&a, MAIN, &version);
    assert_eq!(project_name(&a, &p), "Client");
    let pre = only(named(&a, "pre-restore"));
    assert_eq!(roots_of(&a, &pre), [SECOND]);
}

/// Test 52: the core promise.
#[test]
fn restore_survives_next_cycle_with_own_and_other_stale_files() {
    let (cloud, a, b) = two();
    let (p, _) = a.task("Client", "Design");
    sync_all(&[&a, &b]);
    let version = save(&a);
    cloud.advance(60);
    rename(&a, &p, "Renamed since");
    let (_, later) = a.task("Later", "Added after the version");
    sync_all(&[&a, &b]);
    assert_eq!(project_name(&b, &p), "Renamed since");
    // LAPTOP sees DESKTOP's files late from now on.
    cloud.delay(&a.id(), &b.id(), 600);
    cloud.advance(60);
    restore(&a, MAIN, &version);
    let rolled_back = |d: &Device| {
        assert_eq!(project_name(d, &p), "Client", "{}", d.name());
        assert!(
            !d.s().file().data.tasks.contains_key(&later),
            "{}",
            d.name()
        );
    };
    rolled_back(&a);
    // DESKTOP's own file in the folder and LAPTOP's both still hold what came after the version, at the old epoch.
    sync(&a);
    rolled_back(&a);
    let fenced = "Skipped LAPTOP: it holds data from before a restored version. Update and sync that device.";
    assert!(status(&a, MAIN).warnings.contains(&fenced.to_owned()));
    // LAPTOP hasn't seen the restore: it merges as ever and writes its file again, newer than any DESKTOP has read.
    cloud.advance(60);
    sync(&b);
    assert_eq!(project_name(&b, &p), "Renamed since");
    sync(&a);
    rolled_back(&a);
    // Once the restore reaches LAPTOP it follows it, and keeps a copy of what it had.
    cloud.advance(600);
    sync(&b);
    rolled_back(&b);
    let pre = only(named(&b, "pre-adopt"));
    assert_eq!(
        opened(&cloud, MAIN, &pre).data.projects[&p].name,
        "Renamed since"
    );
    sync(&a);
    rolled_back(&a);
    assert!(!status(&a, MAIN).warnings.contains(&fenced.to_owned()));
}

/// Test 53.
#[test]
fn restore_from_either_folder() {
    let (cloud, a) = two_folders();
    let (p, _) = a.task("Client", "Design");
    sync_all(&[&a]);
    let version = save(&a);
    assert_eq!(roots_of(&a, &version), [MAIN, SECOND]);
    cloud.advance(60);
    rename(&a, &p, "Renamed since");
    // The first folder can't be reached: the copy in the second is read, and today's data is kept there.
    a.view.set_offline_root(MAIN, true);
    cloud.advance(60);
    restore(&a, MAIN, &version);
    assert_eq!(project_name(&a, &p), "Client");
    let list = listed(&a);
    assert_eq!(list.errors.len(), 1);
    assert_eq!(list.errors[0].root, MAIN);
    let pre = only(named(&a, "pre-restore"));
    assert_eq!(roots_of(&a, &pre), [SECOND]);
    a.view.set_offline_root(MAIN, false);
    // And from the second folder by choice.
    cloud.advance(60);
    rename(&a, &p, "Renamed again");
    restore(&a, SECOND, &version);
    assert_eq!(project_name(&a, &p), "Client");
    assert_eq!(a.s().file().prefs.epoch.as_ref().unwrap().epoch, 2);
}

/// A version made while the other folder was failing, restored while it still fails.
#[test]
fn restore_from_a_version_made_in_a_folder_whose_sibling_fails() {
    let (cloud, a) = two_folders();
    let (p, _) = a.task("Client", "Design");
    sync_all(&[&a]);
    a.view.set_full(SECOND, true);
    cloud.advance(60);
    rename(&a, &p, "In the version");
    let version = save(&a);
    assert_eq!(roots_of(&a, &version), [MAIN]);
    assert_eq!(
        status(&a, SECOND).last_version_error.as_deref(),
        Some("Could not write the record of changes: There is not enough space on the disk.")
    );
    assert!(status(&a, MAIN).last_version_error.is_none());
    cloud.advance(60);
    rename(&a, &p, "After it");
    cloud.advance(60);
    restore(&a, MAIN, &version);
    assert_eq!(project_name(&a, &p), "In the version");
    assert_eq!(roots_of(&a, &only(named(&a, "pre-restore"))), [MAIN]);
    // The failing folder catches up once it can be written.
    a.view.set_full(SECOND, false);
    sync(&a);
    let restored = only(named(&a, "restored"));
    assert_eq!(roots_of(&a, &restored), [MAIN, SECOND]);
    assert!(status(&a, SECOND).last_version_error.is_none());
}

/// Test 54.
#[test]
fn other_device_adopts_carries_forward_later_time_including_a_timer_spanning_the_restore_with_gaps_and_writes_pre_adopt()
 {
    let (cloud, a, b) = two();
    let (p, _) = a.task("Client", "Design");
    sync_all(&[&a, &b]);
    let version = save(&a);
    cloud.advance(600);
    // LAPTOP starts timing after the version was taken, and is still timing when DESKTOP restores it.
    let (lp, lt) = b.task("Laptop work", "Field notes");
    b.s().start_task(&lt).unwrap();
    let seg = b.running();
    let began = b.s().now();
    cloud.advance(300);
    rename(&a, &p, "Renamed since");
    sync(&a);
    sync(&b);
    cloud.advance(300);
    let restored_at = a.s().now();
    restore(&a, MAIN, &version);
    sync(&a);
    // LAPTOP was away from before the restore to after it.
    cloud.advance(600);
    let gap = b
        .s()
        .mutate(|f, c| {
            Ok(f.data.add_gap(
                c,
                &seg,
                restored_at.later(-300),
                restored_at.later(300),
                GapKind::Idle,
            ))
        })
        .unwrap()
        .unwrap();
    let a_id = a.id();
    sync(&b);
    {
        let s = b.s();
        let data = &s.file().data;
        assert_eq!(data.projects[&p].name, "Client", "rolled back here too");
        // Its own time since the restore stays: the run from the moment of the restore, closed now, with its task and
        // project, and its time away from that moment on.
        let kept = &data.segments[&seg];
        assert_eq!((kept.start, kept.end), (restored_at, Some(s.now())));
        assert!(data.tasks.contains_key(&lt) && data.projects.contains_key(&lp));
        let away = &data.gaps[&gap];
        assert_eq!(
            (away.start, away.end),
            (restored_at, restored_at.later(300))
        );
        assert_eq!(s.file().prefs.epoch.as_ref().unwrap().by, a_id);
    }
    // What it replaced is kept as a version in the folder, the whole run included.
    let pre = only(named(&b, "pre-adopt"));
    let info = listed(&b)
        .versions
        .into_iter()
        .find(|v| v.name == pre)
        .unwrap();
    assert_eq!(info.device_name, "LAPTOP");
    assert!(info.pinned);
    let kept = opened(&cloud, MAIN, &pre);
    assert_eq!(kept.data.projects[&p].name, "Renamed since");
    assert_eq!(kept.data.segments[&seg].start, began);
    // DESKTOP takes in the time since the restore, and its data stays restored.
    sync(&a);
    assert_eq!(project_name(&a, &p), "Client");
    assert_eq!(a.s().file().data.segments[&seg].start, restored_at);
}

/// An old app's device file: no epoch, and its own project besides what it merged.
fn old_app_file(cloud: &FakeCloud, template: &Device, projects: &[&str]) {
    let mut f = template.s().device_file();
    let id = "0d0d0d0d-0000-4000-8000-000000000000";
    f.device_id = id.to_owned();
    f.device_name = "OLDPC".to_owned();
    f.epoch_info = None;
    let base = f.data.projects.values().next().unwrap().clone();
    for name in projects {
        let mut project = base.clone();
        project.id = format!("old-{name}");
        project.name = (*name).to_owned();
        f.data.projects.insert(project.id.clone(), project);
    }
    let mut json = serde_json::to_value(&f).unwrap();
    json.as_object_mut().unwrap().remove("epochInfo");
    cloud.put(
        &devices_dir(Path::new(MAIN)).join(format!("{id}.json")),
        &serde_json::to_vec(&json).unwrap(),
    );
}

/// Test 55.
#[test]
fn old_format_file_fenced_after_restore_and_warned() {
    let (cloud, a, _) = two();
    a.task("Client", "Design");
    sync_all(&[&a]);
    let version = save(&a);
    // Until a restore, an old app's file merges like any other.
    cloud.advance(60);
    old_app_file(&cloud, &a, &["Old app work"]);
    sync(&a);
    assert!(a.s().file().data.projects.contains_key("old-Old app work"));
    cloud.advance(60);
    restore(&a, MAIN, &version);
    sync(&a);
    let fenced =
        "Skipped OLDPC: it holds data from before a restored version. Update and sync that device.";
    let check = |a: &Device| {
        let old = a
            .s()
            .file()
            .data
            .projects
            .keys()
            .any(|k| k.starts_with("old-"));
        assert!(!old, "nothing of the old app's comes back");
        assert!(status(a, MAIN).warnings.contains(&fenced.to_owned()));
    };
    check(&a);
    // What it writes later is left out too, until it is updated.
    cloud.advance(60);
    old_app_file(&cloud, &a, &["Old app work", "Later on the old app"]);
    sync(&a);
    check(&a);
}

/// Test 56.
#[test]
fn old_app_json_shape_parses_new_file() {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct OldDeviceFile {
        schema_version: u64,
        device_name: String,
        data: Shared,
    }
    let (cloud, a, _) = two();
    a.task("Client", "Design");
    sync_all(&[&a]);
    let version = save(&a);
    cloud.advance(60);
    restore(&a, MAIN, &version);
    sync(&a);
    let restored = only(named(&a, "restored"));
    let bytes = gz::decompress(
        &cloud.latest(&file_in(MAIN, &restored, ".json.gz")).unwrap(),
        1 << 26,
    )
    .unwrap();
    let raw: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(raw["epochInfo"]["epoch"], json!(1));
    let old: OldDeviceFile = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        (old.schema_version, old.device_name.as_str()),
        (2, "DESKTOP")
    );
    assert_eq!(old.data, a.s().file().data);
    // It is the very file the sync before it wrote.
    let device_file = cloud
        .latest(&devices_dir(Path::new(MAIN)).join(format!("{}.json", a.id())))
        .unwrap();
    assert_eq!(bytes, device_file);
}

/// Test 57.
#[test]
fn empty_store_joins_higher_epoch() {
    let (cloud, a, _) = two();
    let (p, _) = a.task("Client", "Design");
    sync_all(&[&a]);
    let version = save(&a);
    cloud.advance(60);
    rename(&a, &p, "Renamed since");
    restore(&a, MAIN, &version);
    sync(&a);
    let c = Device::open(&cloud, "TABLET", &[MAIN]);
    sync(&c);
    assert_eq!(project_name(&c, &p), "Client");
    let s = c.s();
    assert_eq!(s.file().prefs.epoch, a.s().file().prefs.epoch);
    assert_eq!(
        s.file().prefs.sync_joined.as_deref(),
        Some(&[MAIN.to_owned()][..])
    );
    assert!(s.file().prefs.join_pending.is_none());
}

/// A computer with data of its own connects a folder that has a restore in its past.
fn tablet_with_its_own_data(cloud: &Arc<FakeCloud>) -> (Device, String) {
    let c = Device::open(cloud, "TABLET", &[]);
    let (mine, _) = c.task("Tablet work", "Sketches");
    c.s()
        .add_sync_target(SyncTarget {
            provider: SyncProvider::Custom,
            root: MAIN.to_owned(),
        })
        .unwrap();
    (c, mine)
}

/// DESKTOP restores the version it took before renaming its project.
fn desktop_restored() -> (Arc<FakeCloud>, Device, String) {
    let (cloud, a, _) = two();
    let (p, _) = a.task("Client", "Design");
    sync_all(&[&a]);
    let version = save(&a);
    cloud.advance(60);
    rename(&a, &p, "Renamed since");
    restore(&a, MAIN, &version);
    sync(&a);
    (cloud, a, p)
}

/// Test 57a.
#[test]
fn join_with_local_data_does_not_undo_restore() {
    // Nothing is read from that folder or written to it until the user chooses.
    let (cloud, a, p) = desktop_restored();
    let (c, mine) = tablet_with_its_own_data(&cloud);
    let c_id = c.id();
    sync(&c);
    let waiting = c.s().file().prefs.join_pending.clone().unwrap();
    assert_eq!(waiting.root, MAIN);
    assert!(!c.s().file().data.projects.contains_key(&p));
    assert!(!cloud.exists(&FakeCloud::device_file_path(MAIN, &c_id)));
    let waiting_words = "Your sync folder is waiting for your choice in Settings, under Sync between devices, so no version can be saved there yet.";
    let refused = versions::save_now(&c.svc, &c.view, LIMIT).unwrap_err();
    assert_eq!(refused.to_string(), waiting_words);
    // Nor restored there: the copy of today's data that comes first could go nowhere.
    let any = listed(&a).versions[0].name.clone();
    let preview = versions::preview_restore(&c.svc, &c.view, MAIN, &any, LIMIT).unwrap();
    assert_eq!(preview.blocked_reason.as_deref(), Some(waiting_words));
    let refused = versions::restore(&c.svc, &c.view, MAIN, &any, LIMIT).unwrap_err();
    assert_eq!(refused.code(), "conflict");
    assert_eq!(
        refused.to_string(),
        format!("{waiting_words} Nothing was changed.")
    );
    assert!(!c.s().file().data.projects.contains_key(&p));
    assert!(c.s().file().prefs.epoch.is_none());
    versions::run_at_quit(&c.svc, &c.view, LIMIT);
    assert!(
        listed(&c)
            .versions
            .iter()
            .all(|v| v.device_name != "TABLET")
    );
    sync(&a);
    assert_eq!(project_name(&a, &p), "Client");

    // Following it: TABLET takes on the restored data, and its own goes into a version.
    epoch::resolve_join(&c.svc, &c.view, MAIN, true, LIMIT).unwrap();
    sync(&c);
    assert_eq!(project_name(&c, &p), "Client");
    assert!(!c.s().file().data.projects.contains_key(&mine));
    let pre = only(named(&c, "pre-adopt"));
    assert!(opened(&cloud, MAIN, &pre).data.projects.contains_key(&mine));
    sync(&a);
    assert_eq!(project_name(&a, &p), "Client");
    assert!(!a.s().file().data.projects.contains_key(&mine));

    // Keeping both: a new restore of the two together, which the others follow, its own work and all.
    let (cloud, a, p) = desktop_restored();
    let (c, mine) = tablet_with_its_own_data(&cloud);
    let c_id = c.id();
    sync(&c);
    epoch::resolve_join(&c.svc, &c.view, MAIN, false, LIMIT).unwrap();
    assert_eq!(c.s().file().prefs.epoch.as_ref().unwrap().by, c_id);
    sync(&c);
    sync(&a);
    assert_eq!(project_name(&a, &p), "Client", "the restore itself stands");
    assert!(a.s().file().data.projects.contains_key(&mine));
    assert_eq!(a.s().file().prefs.epoch.as_ref().unwrap().epoch, 2);
    assert_eq!(named(&a, "pre-adopt").len(), 1);
}

/// Test 57b.
#[test]
fn local_backup_from_before_a_restore_is_refused() {
    let (cloud, a, _) = two();
    let (p, _) = a.task("Client", "Design");
    sync_all(&[&a]);
    a.s().run_daily_backup().unwrap();
    let daily = a.s().list_backups().unwrap()[0].name.clone();
    let version = save(&a);
    cloud.advance(60);
    rename(&a, &p, "Renamed since");
    restore(&a, MAIN, &version);
    let refusal = "This backup is from before the version of 2026-03-02 09:00 was restored. Restoring it would undo that on every computer. Use Versions instead.";
    let refused = a.s().restore_backup(&daily).unwrap_err();
    assert_eq!(refused.to_string(), refusal);
    // The safety copy the restore made is from before it too: the way back is the pre-restore version.
    let safety = a
        .s()
        .list_backups()
        .unwrap()
        .into_iter()
        .find(|b| b.safety)
        .unwrap()
        .name;
    assert_eq!(
        a.s().restore_backup(&safety).unwrap_err().to_string(),
        refusal
    );
    assert_eq!(project_name(&a, &p), "Client");
}

/// Test 57c.
#[test]
fn crash_between_restore_steps_cannot_leave_data_at_the_old_key() {
    let (cloud, a, _) = two();
    let (p, _) = a.task("Client", "Design");
    sync_all(&[&a]);
    let version = save(&a);
    cloud.advance(60);
    rename(&a, &p, "Renamed since");
    // The save that installs the restore fails, as a crash at that step would leave it: nothing changes.
    a.s().set_saves_blocked(true);
    assert!(versions::restore(&a.svc, &a.view, MAIN, &version, LIMIT).is_err());
    assert_eq!(project_name(&a, &p), "Renamed since");
    assert!(a.s().file().prefs.epoch.is_none());
    a.s().set_saves_blocked(false);
    let a = a.relaunch();
    assert_eq!(project_name(&a, &p), "Renamed since");
    assert!(a.s().file().prefs.epoch.is_none());
    // When it goes through, the data and its epoch are in one save: a crash just after it finds both.
    cloud.advance(60);
    restore(&a, MAIN, &version);
    let a = a.relaunch();
    assert_eq!(project_name(&a, &p), "Client");
    assert_eq!(
        a.s()
            .file()
            .prefs
            .epoch
            .as_ref()
            .map(|e| e.restored_from.clone()),
        Some(version)
    );
}

/// Test 58.
#[test]
fn restore_of_pre_restore_undoes() {
    let (cloud, a, b) = two();
    let (p, _) = a.task("Client", "Design");
    sync_all(&[&a, &b]);
    let version = save(&a);
    cloud.advance(60);
    rename(&a, &p, "Renamed since");
    sync_all(&[&a, &b]);
    cloud.advance(60);
    restore(&a, MAIN, &version);
    sync_all(&[&a, &b]);
    assert_eq!(project_name(&b, &p), "Client");
    let pre = only(named(&a, "pre-restore"));
    cloud.advance(60);
    restore(&a, MAIN, &pre);
    sync_all(&[&a, &b]);
    for d in [&a, &b] {
        assert_eq!(project_name(d, &p), "Renamed since", "{}", d.name());
        assert_eq!(d.s().file().prefs.epoch.as_ref().unwrap().epoch, 2);
    }
}

/// Test 59.
#[test]
fn upgraded_store_fills_sync_joined() {
    let (cloud, a, b) = two();
    let (p, _) = a.task("Client", "Design");
    sync_all(&[&a, &b]);
    // LAPTOP's store is from before the list of followed folders.
    b.s()
        .mutate(|f, _| {
            f.prefs.sync_joined = None;
            Ok(())
        })
        .unwrap();
    let b = b.relaunch();
    assert_eq!(
        b.s().file().prefs.sync_joined.as_deref(),
        Some(&[MAIN.to_owned()][..])
    );
    let version = save(&a);
    cloud.advance(60);
    rename(&a, &p, "Renamed since");
    sync_all(&[&a, &b]);
    restore(&a, MAIN, &version);
    sync(&a);
    // It has merged with that folder all along, so it follows the restore without being asked.
    sync(&b);
    assert_eq!(project_name(&b, &p), "Client");
    assert!(b.s().file().prefs.join_pending.is_none());
}

#[test]
fn list_merges_folders_names_devices_and_looks_in_a_folder_no_longer_connected() {
    let cloud = FakeCloud::new(&[MAIN, SECOND, OLD]);
    let a = Device::open(&cloud, "DESKTOP", &[MAIN, SECOND]);
    let b = Device::open(&cloud, "LAPTOP", &[MAIN]);
    a.task("Client", "Design");
    sync_all(&[&a, &b]);
    let version = save(&a);
    let b_id = b.id();
    let theirs = version_name(at("2025-12-24T08:00:00Z"), &b_id, VersionReason::PreAdopt);
    cloud.put(&file_in(OLD, &theirs, ".json.gz"), b"0123456789");
    let unknown = plant(&cloud, OLD, at("2025-12-01T08:00:00Z"), VersionReason::Auto);
    a.view.set_offline_root(SECOND, true);
    let list = versions::list(&a.svc, &a.view, Some(OLD), LIMIT);
    let names: Vec<&str> = list.versions.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, [version.as_str(), theirs.as_str(), unknown.as_str()]);
    assert_eq!(list.errors.len(), 1);
    assert_eq!(
        (
            list.errors[0].root.as_str(),
            list.errors[0].message.as_str()
        ),
        (
            SECOND,
            "Folder not found. Is the drive connected and the desktop client running?"
        )
    );
    let by_name = |n: &str| list.versions.iter().find(|v| v.name == n).unwrap();
    assert_eq!(by_name(&theirs).device_name, "LAPTOP");
    assert!(by_name(&theirs).pinned, "a pre-adopt copy under a year old");
    assert_eq!(by_name(&unknown).device_name, "another computer");
    assert!(!list.over_budget);
    let gz_size =
        |root: &str, n: &str| cloud.latest(&file_in(root, n, ".json.gz")).unwrap().len() as u64;
    let txt_size = |root: &str, n: &str| {
        cloud
            .latest(&file_in(root, n, ".changes.txt"))
            .map_or(0, |b| b.len() as u64)
    };
    assert_eq!(
        list.total_bytes,
        gz_size(MAIN, &version)
            + txt_size(MAIN, &version)
            + 10
            + gz_size(OLD, &unknown)
            + txt_size(OLD, &unknown)
    );
    // The shape the page reads.
    let json = serde_json::to_value(&list).unwrap();
    assert_eq!(
        json["versions"][0],
        json!({
            "name": version,
            "takenAt": "2026-03-02T09:00:00Z",
            "deviceName": "DESKTOP",
            "reason": "manual",
            "pinned": false,
            "locations": [{"root": MAIN, "provider": "custom", "bytes": gz_size(MAIN, &version)}],
        })
    );
    assert_eq!(json["versions"][1]["locations"][0]["root"], json!(OLD));
    assert_eq!(json["errors"][0]["root"], json!(SECOND));
    assert!(json["totalBytes"].is_u64() && json["overBudget"] == json!(false));
    // Both folders again: one version, two copies.
    a.view.set_offline_root(SECOND, false);
    assert_eq!(roots_of(&a, &version), [MAIN, SECOND]);
}

#[test]
fn only_a_versions_own_record_can_be_read() {
    let (cloud, a, _) = two();
    a.task("Client", "Design");
    sync_all(&[&a]);
    let version = save(&a);
    let text = changes_of(&a, MAIN, &version);
    assert!(text.contains("Reason: saved by hand."), "{text}");
    assert_eq!(
        versions::read_changes_within(&a.svc, &a.view, MAIN, &version, LIMIT).unwrap(),
        text
    );
    for bad in [
        "../devices/x",
        &format!("{version}/../../devices/x"),
        &format!("../{version}"),
        "x",
        "",
    ] {
        let refused = versions::read_changes(&a.view, Path::new(MAIN), bad).unwrap_err();
        assert_eq!(refused.code(), "not_found", "{bad}");
    }
    // One that isn't there yet, such as one still syncing: an error the page shows.
    let missing = version_name(cloud.now().later(-86_400), &a.id(), VersionReason::Auto);
    let refused = versions::read_changes(&a.view, Path::new(MAIN), &missing).unwrap_err();
    assert_eq!(refused.code(), "conflict");
}

#[test]
fn preview_counts_what_a_restore_brings_and_says_why_one_cannot() {
    let (cloud, a, b) = two();
    a.tick();
    b.tick();
    let (p, _) = holding(&a);
    sync_all(&[&a, &b]);
    b.tick();
    let version = save(&a);
    a.task("Second", "More");
    let preview = versions::preview_restore(&a.svc, &a.view, MAIN, &version, LIMIT).unwrap();
    assert_eq!(
        serde_json::to_value(&preview).unwrap(),
        json!({
            "name": version,
            "takenAt": "2026-03-02T09:00:00Z",
            "reason": "manual",
            "version": {"projects": 1, "tasks": 1, "segments": 1},
            "current": {"projects": 2, "tasks": 2, "segments": 1},
            "warnings": [],
            "blockedReason": null,
        })
    );
    // Another computer while this one holds the lease: take over first.
    let theirs = versions::preview_restore(&b.svc, &b.view, MAIN, &version, LIMIT).unwrap();
    assert_eq!(
        theirs.blocked_reason.as_deref(),
        Some("Take over first to restore.")
    );

    // A record that can't be trusted is left out, and said so.
    let mut file = opened(&cloud, MAIN, &version);
    let mut bad = file.data.segments.values().next().unwrap().clone();
    bad.id = "bad-segment".into();
    bad.end = Some(bad.start.later(-60));
    file.data.segments.insert(bad.id.clone(), bad);
    let damaged = version_name(cloud.now().later(-60), SOMEONE, VersionReason::Auto);
    cloud.put(
        &file_in(MAIN, &damaged, ".json.gz"),
        &gz::compress(&serde_json::to_vec(&file).unwrap()),
    );
    let preview = versions::preview_restore(&a.svc, &a.view, MAIN, &damaged, LIMIT).unwrap();
    assert_eq!(preview.warnings.len(), 1, "{:?}", preview.warnings);
    assert!(preview.blocked_reason.is_none());

    // A copy cut short can't be read here, so the page can offer another; with no other, a restore is refused.
    cloud.advance(60);
    rename(&a, &p, "Renamed since");
    cloud.truncate(&file_in(MAIN, &version, ".json.gz"));
    let refused = versions::preview_restore(&a.svc, &a.view, MAIN, &version, LIMIT).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "It couldn't be unpacked: the file is incomplete or damaged."
    );
    let refused = versions::restore(&a.svc, &a.view, MAIN, &version, LIMIT).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "No copy of this version could be read: It couldn't be unpacked: the file is incomplete or damaged."
    );
    assert_eq!(project_name(&a, &p), "Renamed since");

    // One from a newer app is never restored here.
    let newer = version_name(cloud.now().later(-120), SOMEONE, VersionReason::Manual);
    let mut raw = serde_json::to_value(opened(&cloud, MAIN, &damaged)).unwrap();
    raw["schemaVersion"] = json!(SCHEMA_VERSION + 1);
    cloud.put(
        &file_in(MAIN, &newer, ".json.gz"),
        &gz::compress(&serde_json::to_vec(&raw).unwrap()),
    );
    let preview = versions::preview_restore(&a.svc, &a.view, MAIN, &newer, LIMIT).unwrap();
    assert_eq!(
        preview.blocked_reason.as_deref(),
        Some(
            "This version was saved by a newer version of the app. Update this computer to restore it."
        )
    );
    let refused = versions::restore(&a.svc, &a.view, MAIN, &newer, LIMIT).unwrap_err();
    assert_eq!(refused.code(), "future_version");
    // The damaged one restores, without the record it couldn't trust.
    versions::restore(&a.svc, &a.view, MAIN, &damaged, LIMIT).unwrap();
    assert!(!a.s().file().data.segments.contains_key("bad-segment"));
    assert_eq!(project_name(&a, &p), "Client");
}

/// One failing location must not block the other.
#[test]
fn a_hung_folder_does_not_hold_up_the_others_version() {
    let (cloud, a) = two_folders();
    a.task("Client", "Design");
    sync_all(&[&a]);
    cloud.advance(60);
    let now = a.s().now();
    let id = a.id();
    let name = version_name(now, &id, VersionReason::Manual);
    let gate = a
        .view
        .gate_next_write(&file_in(MAIN, &name, ".changes.txt"));
    versions::save_now(&a.svc, &a.view, Duration::from_millis(300)).unwrap();
    assert!(gate.wait_entered());
    assert_eq!(
        status(&a, MAIN).last_version_error.as_deref(),
        Some("The folder is not responding. Will try again later.")
    );
    assert_eq!(status(&a, SECOND).last_version_at, Some(a.s().now()));
    gate.open();
}

/// Review R3: the clock of the computer that thins out the versions jumps a year ahead. It deletes none of today's
/// versions, those kept a year around a restore included, with its first version after the jump or any after that.
#[test]
fn a_clock_a_year_ahead_deletes_none_of_todays_versions() {
    let (cloud, a, _) = two();
    let (p, _) = holding(&a);
    sync(&a);
    let first = save(&a);
    for i in 0..3 {
        cloud.advance(3600);
        rename(&a, &p, &format!("Work {i}"));
        save(&a);
    }
    cloud.advance(3600);
    restore(&a, MAIN, &first);
    sync(&a);
    let before: Vec<String> = listed(&a).versions.into_iter().map(|v| v.name).collect();
    assert!(
        before.iter().any(|n| n.ends_with("--pre-restore")),
        "{before:?}"
    );
    // This computer's clock alone jumps 400 days ahead, and it goes on working and saving versions.
    a.clock.advance(400 * 86_400);
    for i in 0..3 {
        a.tick();
        rename(&a, &p, &format!("Later {i}"));
        a.tick();
        assert_eq!(a.state(), MINE);
        save(&a);
        cloud.advance(3600);
    }
    let after: Vec<String> = listed(&a).versions.into_iter().map(|v| v.name).collect();
    let gone: Vec<&String> = before.iter().filter(|n| !after.contains(n)).collect();
    assert!(gone.is_empty(), "deleted by a clock a year ahead: {gone:?}");
}

/// What a computer timed after a restore it hadn't seen yet, and whether it is still there once it follows that
/// restore. The restoring computer's clock runs `skew` seconds ahead.
fn follower_keeps_time_after_restore(skew: i64) -> bool {
    let (cloud, a, b) = two();
    a.task("Shared", "Base");
    a.tick();
    sync(&a);
    sync(&b);
    let version = save(&a);
    a.clock.advance(skew);
    // Neither sees the other for a while, as cloud apps can be slow, and LAPTOP goes on working.
    cloud.hold_both(&a.id(), &b.id());
    cloud.advance(60);
    restore(&a, MAIN, &version);
    sync(&a);
    let (_, t) = b.task("Laptop", "After the restore");
    b.s().start_task(&t).unwrap();
    let seg = b.running();
    cloud.advance(20 * 60);
    b.s().pause_task(&t).unwrap();
    cloud.release_both(&a.id(), &b.id());
    cloud.advance(60);
    sync(&b);
    assert_eq!(
        b.s().file().prefs.epoch.as_ref().map(|e| e.epoch),
        Some(1),
        "followed"
    );
    b.s().file().data.segments.contains_key(&seg)
}

/// Review R4: a computer restoring with its clock hours ahead dates the restore hours late. A computer following it still
/// keeps the time it logged after the restore.
#[test]
fn a_restorer_with_its_clock_hours_ahead_takes_no_time_logged_after_the_restore() {
    assert!(follower_keeps_time_after_restore(3 * 3600));
}

/// R4's control: with the restorer's clock right, as ever.
#[test]
fn a_restorer_with_its_clock_right_takes_no_time_logged_after_the_restore() {
    assert!(follower_keeps_time_after_restore(0));
}

/// Review R6: two restores followed while the folder takes no version. Once it does again, the copy of what each
/// replaced is there, the first included, as the notice for each said.
#[test]
fn two_restores_followed_while_the_folder_is_full_each_leave_a_copy_of_what_they_replaced() {
    let (cloud, a, b) = two();
    let (p, _) = a.task("Client", "Design");
    sync_all(&[&a, &b]);
    let version = save(&a);
    rename(&b, &p, "Laptop 1");
    b.view.set_full(MAIN, true);
    cloud.advance(60);
    restore(&a, MAIN, &version);
    sync(&a);
    sync(&b);
    assert_eq!(project_name(&b, &p), "Client", "followed");
    rename(&b, &p, "Laptop 2");
    cloud.advance(60);
    restore(&a, MAIN, &version);
    sync(&a);
    sync(&b);
    assert_eq!(project_name(&b, &p), "Client", "followed again");
    assert!(named(&b, "pre-adopt").is_empty(), "the folder is full");
    b.view.set_full(MAIN, false);
    cloud.advance(60);
    sync(&b);
    let mut copies: Vec<String> = named(&b, "pre-adopt")
        .iter()
        .map(|n| opened(&cloud, MAIN, n).data.projects[&p].name.clone())
        .collect();
    copies.sort();
    assert_eq!(copies, ["Laptop 1", "Laptop 2"]);
}

/// Review R7: the version a take over asks for isn't dropped when this computer has to stop before writing it. It waits,
/// and is written once this computer may change data again.
#[test]
fn the_version_a_take_over_asks_for_waits_while_read_only_and_is_written_after() {
    let (cloud, a, b) = taken_over();
    // DESKTOP is back, and takes over again before LAPTOP has written that version. Take over is offered once LAPTOP looks
    // inactive, so DESKTOP watches it say nothing for 16 minutes first.
    a.view.set_offline(false);
    a.tick();
    minutes(&cloud, &[&a], 16);
    let preview = lease_io::preview_take_over(&a.svc, &a.view, LIMIT);
    assert!(!preview.live, "LAPTOP looks inactive");
    let holder = preview.holder.clone().unwrap();
    let back = lease_io::take_over(
        &a.svc,
        &a.view,
        TakeOverExpected {
            generation: holder.generation,
            beat_seq: holder.beat_seq,
        },
        TakeOverMode::Stale,
        LIMIT,
    )
    .unwrap();
    assert_eq!(back.outcome, TakeOverOutcome::Done);
    b.tick();
    assert!(b.s().read_only());
    sync(&b);
    assert!(
        named(&b, "takeover").is_empty(),
        "a computer that may not change data writes none"
    );
    // DESKTOP quits, so LAPTOP may change data again, and writes it.
    a.quit(LIMIT);
    cloud.advance(60);
    b.tick();
    assert!(!b.s().read_only());
    sync(&b);
    let name = only(named(&b, "takeover"));
    assert_eq!(roots_of(&b, &name), [MAIN]);
}

/// A forged device file in `root`: a restore numbered `epoch`, holding `data`.
fn plant_restore(cloud: &FakeCloud, root: &str, epoch: u64, data: Shared) {
    let now = cloud.now();
    let file = DeviceFile {
        schema_version: SCHEMA_VERSION,
        device_id: SOMEONE.to_owned(),
        device_name: "PLANTED".to_owned(),
        written_at: now,
        last_heartbeat: None,
        epoch_info: Some(EpochInfo {
            epoch,
            by: SOMEONE.to_owned(),
            at: now,
            restored_from: version_name(now, SOMEONE, VersionReason::Manual),
            base_taken_at: now,
        }),
        data,
    };
    cloud.put(
        &FakeCloud::device_file_path(root, SOMEONE),
        &serde_json::to_vec(&file).unwrap(),
    );
}

/// Audit SEC-2: a restore planted at the highest number a reader takes is followed like any other, but a restore made
/// after it would be numbered past what any reader takes, which cuts this computer off from the others. Restoring is
/// refused before anything is written, and so is keeping both against such a restore. Following one still works.
#[test]
fn no_restore_is_numbered_past_what_a_reader_takes() {
    const TOP: u64 = 1 << 40;
    let (cloud, a, b) = two();
    let (p, _) = holding(&a);
    sync_all(&[&a, &b]);
    let version = save(&a);
    plant_restore(&cloud, MAIN, TOP, a.s().file().data.clone());
    sync(&a);
    let epoch_of = |d: &Device| d.s().file().prefs.epoch.as_ref().map(|e| e.epoch);
    assert_eq!(epoch_of(&a), Some(TOP), "followed");
    rename(&a, &p, "Renamed");
    let data = a.s().file().data.clone();
    let count = listed(&a).versions.len();
    let refused = versions::restore(&a.svc, &a.view, MAIN, &version, LIMIT).unwrap_err();
    assert_eq!(refused.code(), "conflict");
    assert!(refused.to_string().contains("highest number"), "{refused}");
    assert_eq!(a.s().file().data, data, "nothing changed");
    assert_eq!(epoch_of(&a), Some(TOP));
    assert_eq!(
        listed(&a).versions.len(),
        count,
        "not even the copy of today's data was written"
    );

    let cloud = FakeCloud::new(&[MAIN, SECOND]);
    let d = Device::open(&cloud, "DESKTOP", &[MAIN]);
    let (mine, _) = d.task("Mine", "Design");
    sync(&d);
    plant_restore(&cloud, SECOND, TOP, Shared::default());
    d.s()
        .add_sync_target(SyncTarget {
            provider: SyncProvider::Custom,
            root: SECOND.to_owned(),
        })
        .unwrap();
    sync(&d);
    assert!(d.s().file().prefs.join_pending.is_some(), "asked");
    let refused = epoch::resolve_join(&d.svc, &d.view, SECOND, false, LIMIT).unwrap_err();
    assert_eq!(refused.code(), "conflict");
    assert!(refused.to_string().contains("highest number"), "{refused}");
    assert!(d.s().file().prefs.join_pending.is_some(), "still asked");
    assert!(d.s().file().data.projects.contains_key(&mine));
    assert_eq!(epoch_of(&d), None);
    epoch::resolve_join(&d.svc, &d.view, SECOND, true, LIMIT).unwrap();
    assert_eq!(epoch_of(&d), Some(TOP));
}
