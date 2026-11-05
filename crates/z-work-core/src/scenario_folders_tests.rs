//! Two devices, a fake clock and a delayed folder: two sync folders.
//!
//! DESKTOP and LAPTOP (TABLET where a third is needed) share OneDrive and Google Drive as plain folders through
//! `testkit`. A device's sync is a `cycle` and then `versions::run_due`, as the app runs them, and its lease thread a
//! `tick`. A folder that fails for one device is `set_offline_root` or `set_full` on that device's view only, and the
//! cloud's own faults are done on the `FakeCloud`. Numbers are the design's test plan (section 12). Every test asserts
//! what the user would see: data, notices, statuses and warnings.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::epoch;
use crate::lease::{LEASE_BEAT_SECS, LeaseRecord, LeaseRecordState, LeaseStateName};
use crate::lease_io::{
    self, TakeOverExpected, TakeOverMode, TakeOverOutcome, TakeOverPreview, TakeOverResult,
};
use crate::model::{SyncProvider, SyncTarget, TaskState};
use crate::sync::{MoveResult, SyncIo, TargetStatus, move_target};
use crate::testkit::{Device, FakeCloud, LIMIT, pinput};
use crate::versions::{self, VersionList, VersionName, versions_dir};

const MAIN: &str = "/cloud/OneDrive";
const SECOND: &str = "/cloud/GoogleDrive";
const THIRD: &str = "/cloud/Dropbox";
const FREE: LeaseStateName = LeaseStateName::Free;
const MINE: LeaseStateName = LeaseStateName::HeldByMe;
const LIVE: LeaseStateName = LeaseStateName::HeldByOtherLive;
const STALE: LeaseStateName = LeaseStateName::HeldByOtherStale;
/// What the fake folder says when a write finds no room.
const NO_ROOM: &str = "not enough space";

fn cloud() -> Arc<FakeCloud> {
    FakeCloud::new(&[MAIN, SECOND, THIRD])
}

/// DESKTOP and LAPTOP, each with both folders.
fn pair() -> (Arc<FakeCloud>, Device, Device) {
    let cloud = cloud();
    let a = Device::open(&cloud, "DESKTOP", &[MAIN, SECOND]);
    let b = Device::open(&cloud, "LAPTOP", &[MAIN, SECOND]);
    (cloud, a, b)
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

/// What the page's always-visible warnings say about folders that haven't been written for a while.
fn folder_warnings(d: &Device) -> Vec<String> {
    d.s()
        .snapshot()
        .warnings
        .into_iter()
        .filter(|w| w.contains("hasn't been written"))
        .collect()
}

fn record(cloud: &FakeCloud, root: &str, d: &Device) -> Option<LeaseRecord> {
    cloud.lease_record(root, &d.id())
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

/// A device's lease record as `reader` sees it in a folder just now, delays and holds included.
fn read_as(reader: &Device, root: &str, of: &Device) -> Option<LeaseRecord> {
    let name = format!("{}.json", of.id());
    let bytes = reader
        .view
        .read(&lease_io::lease_dir(Path::new(root)).join(&name), u64::MAX)
        .ok()?;
    crate::lease::parse_record(&name, &bytes)
}

fn holder_beat(d: &Device) -> Option<u64> {
    d.s().lease_view().holder.map(|h| h.beat_seq)
}

fn expected(preview: &TakeOverPreview) -> TakeOverExpected {
    let h = preview.holder.as_ref().unwrap();
    TakeOverExpected {
        generation: h.generation,
        beat_seq: h.beat_seq,
    }
}

/// Takes over from the holder the dialog shows as inactive.
fn take_over_stale(d: &Device) -> TakeOverResult {
    let preview = lease_io::preview_take_over(&d.svc, &d.view, LIMIT);
    assert!(!preview.live, "shown as inactive");
    lease_io::take_over(
        &d.svc,
        &d.view,
        expected(&preview),
        TakeOverMode::Stale,
        LIMIT,
    )
    .unwrap()
}

fn custom(root: &str) -> SyncTarget {
    SyncTarget {
        provider: SyncProvider::Custom,
        root: root.to_owned(),
    }
}

/// The folder is added the way the move flow adds one: other devices' files go across, then this device's.
fn add_by_move(d: &Device, from: &str, to: &str) -> MoveResult {
    move_target(
        &d.svc,
        &d.view,
        Some(from),
        &custom(to),
        None,
        &mut |_, _, _| {},
    )
    .unwrap()
}

fn roots(d: &Device) -> Vec<String> {
    d.s()
        .file()
        .prefs
        .sync_targets
        .iter()
        .map(|t| t.root.clone())
        .collect()
}

/// DESKTOP starts timing, which claims the lease in every folder, and LAPTOP has seen it. Returns the project, the
/// task and its segment.
fn desktop_timing(a: &Device, b: &Device) -> (String, String, String) {
    a.tick();
    b.tick();
    let (p, t) = a.task("Client", "Design");
    a.s().start_task(&t).unwrap();
    a.tick();
    assert_eq!(a.state(), MINE);
    sync_all(&[a, b]);
    b.tick();
    assert_eq!(b.state(), LIVE);
    (p, t, a.running())
}

/// Test 34. The data, the lease and the versions each go on in the folder that works, and say what failed in the other.
#[test]
fn one_folder_failing_does_not_stop_the_other() {
    // The first folder can't be reached from the desktop.
    let (cloud, a, b) = pair();
    a.tick();
    b.tick();
    let (_, t) = a.task("Client", "Design");
    a.s().start_task(&t).unwrap();
    a.view.set_offline_root(MAIN, true);
    a.tick();
    assert_eq!(a.state(), MINE, "a lease in one folder is a lease");
    sync(&a);
    let version = save(&a);

    let (id, running) = (a.id(), a.running());
    assert!(record(&cloud, SECOND, &a).is_some_and(|r| r.open_segments == [running.clone()]));
    assert!(record(&cloud, MAIN, &a).is_none());
    assert!(cloud.exists(&FakeCloud::device_file_path(SECOND, &id)));
    assert!(!cloud.exists(&FakeCloud::device_file_path(MAIN, &id)));
    assert_eq!(roots_of(&a, &version), [SECOND]);

    let (bad, good) = (status(&a, MAIN), status(&a, SECOND));
    assert!(bad.last_error.is_some(), "the data failure is said");
    assert!(bad.lease_error.is_some(), "the lease failure is said");
    assert!(
        bad.last_version_error.is_some(),
        "the version failure is said"
    );
    assert!(bad.last_written_at.is_none() && bad.last_lease_at.is_none());
    assert!(good.last_error.is_none() && good.lease_error.is_none());
    assert!(good.last_version_error.is_none());
    assert!(good.last_written_at.is_some() && good.last_lease_at.is_some());
    assert!(good.last_version_at.is_some());
    assert_eq!(
        listed(&a)
            .errors
            .iter()
            .map(|e| e.root.as_str())
            .collect::<Vec<_>>(),
        [MAIN],
        "the version list says which folder it couldn't look in"
    );

    // The laptop can reach both and learns everything from the one that works.
    sync(&b);
    b.tick();
    assert_eq!(b.state(), LIVE);
    assert_eq!(b.s().file().data.tasks[&t].name, "Design");
    assert!(b.s().file().data.segments.contains_key(&running));
    assert_eq!(roots_of(&b, &version), [SECOND]);

    // The first folder comes back and gets what it missed.
    a.view.set_offline_root(MAIN, false);
    a.tick();
    sync(&a);
    assert!(record(&cloud, MAIN, &a).is_some());
    assert!(cloud.exists(&FakeCloud::device_file_path(MAIN, &id)));
    let healed = status(&a, MAIN);
    assert!(healed.last_error.is_none() && healed.lease_error.is_none());
    assert!(healed.last_written_at.is_some() && healed.last_lease_at.is_some());

    // The second folder is full instead: the first carries on, and the failure is said of the second.
    let (cloud, a, b) = pair();
    a.tick();
    b.tick();
    let (p, t) = a.task("Client", "Design");
    a.view.set_full(SECOND, true);
    a.s().start_task(&t).unwrap();
    a.tick();
    assert_eq!(a.state(), MINE);
    sync(&a);
    let version = save(&a);
    assert_eq!(roots_of(&a, &version), [MAIN]);
    assert!(record(&cloud, MAIN, &a).is_some() && record(&cloud, SECOND, &a).is_none());
    let (bad, good) = (status(&a, SECOND), status(&a, MAIN));
    for why in [&bad.last_error, &bad.lease_error, &bad.last_version_error] {
        assert!(
            why.as_deref().is_some_and(|w| w.contains(NO_ROOM)),
            "{why:?}"
        );
    }
    assert!(
        good.last_error.is_none()
            && good.lease_error.is_none()
            && good.last_version_error.is_none()
    );
    sync(&b);
    b.tick();
    assert_eq!(b.state(), LIVE);
    assert_eq!(project_name(&b, &p), "Client");
}

/// Test 35.
#[test]
fn lease_works_with_main_folder_unreachable() {
    let (cloud, a, b) = pair();
    for d in [&a, &b] {
        d.view.set_offline_root(MAIN, true);
    }
    a.tick();
    b.tick();
    for d in [&a, &b] {
        let lease = d.s().lease_view();
        assert_eq!(lease.state, FREE, "not 'unknown': one folder answers");
        assert_eq!(lease.reachable_roots, [SECOND]);
        assert_eq!(lease.unreachable_roots, [MAIN]);
    }
    let (_, t) = a.task("Client", "Design");
    a.s().start_task(&t).unwrap();
    a.tick();
    assert_eq!(a.state(), MINE);
    sync_all(&[&a, &b]);
    b.tick();
    assert_eq!(b.state(), LIVE, "seen through the second folder alone");
    assert_eq!(
        b.s().create_project(&pinput("Q")).unwrap_err().code(),
        "read_only"
    );
    assert!(record(&cloud, MAIN, &a).is_none());

    // The desktop goes quiet, and the laptop takes over without the first folder.
    a.view.set_offline(true);
    minutes(&cloud, &[&b], 16);
    assert_eq!(b.state(), STALE);
    assert_eq!(take_over_stale(&b).outcome, TakeOverOutcome::Done);
    assert_eq!(b.state(), MINE);
    let taken = record(&cloud, SECOND, &b).unwrap();
    assert_eq!((taken.state, taken.generation), (LeaseRecordState::Held, 2));
    assert!(record(&cloud, MAIN, &b).is_none());
    assert!(b.notices().contains(
        &"You took over from DESKTOP. 1 running task was closed at its last known time.".to_owned()
    ));

    // The first folder comes back for the laptop: nothing changes hands, and the record gets there at the next beat.
    b.view.set_offline_root(MAIN, false);
    minutes(&cloud, &[&b], 3);
    assert_eq!(b.state(), MINE);
    assert!(record(&cloud, MAIN, &b).is_some_and(|r| r.generation == 2));

    // And quitting releases in the folder that works.
    b.quit(LIMIT);
    assert_eq!(
        record(&cloud, SECOND, &b).unwrap().state,
        LeaseRecordState::Released
    );
}

/// Test 36. A folder that is behind says the holder is where it was, and the other folder is not outvoted by it.
#[test]
fn union_takes_freshest_record_per_device() {
    let (cloud, a, b) = pair();
    let (_, _, seg) = desktop_timing(&a, &b);
    // The first folder can't be written from the desktop for a while, so it keeps an old beat.
    a.view.set_offline_root(MAIN, true);
    minutes(&cloud, &[&a], 10);
    let old = record(&cloud, MAIN, &a).unwrap();
    let new = record(&cloud, SECOND, &a).unwrap();
    assert!(new.beat_seq > old.beat_seq && new.beat_at > old.beat_at);
    assert_eq!(old.generation, new.generation);

    b.tick();
    assert_eq!(
        holder_beat(&b),
        Some(new.beat_seq),
        "the freshest of the two"
    );
    assert_eq!(b.state(), LIVE);
    // Reading the stale folder alone it's the old beat; reading both again, the new.
    b.view.set_offline_root(SECOND, true);
    b.tick();
    assert_eq!(holder_beat(&b), Some(old.beat_seq));
    assert_eq!(b.s().lease_view().reachable_roots, [MAIN]);
    b.view.set_offline_root(SECOND, false);
    b.tick();
    assert_eq!(holder_beat(&b), Some(new.beat_seq));
    assert_eq!(b.s().lease_view().reachable_roots, [MAIN, SECOND]);

    // A release that reached only the second folder frees it, though the first still says held.
    a.quit(LIMIT);
    let last = record(&cloud, SECOND, &a).unwrap();
    assert_eq!(last.state, LeaseRecordState::Released);
    assert_eq!(
        record(&cloud, MAIN, &a).unwrap().state,
        LeaseRecordState::Held
    );
    b.tick();
    assert_eq!(b.state(), FREE, "a release in one folder is a release");
    assert!(b.s().lease_view().holder.is_none());
    b.s().create_project(&pinput("After")).unwrap();
    b.tick();
    assert_eq!(b.state(), MINE);
    assert!(record(&cloud, MAIN, &b).is_some() && record(&cloud, SECOND, &b).is_some());
    assert!(b.s().file().data.segments.contains_key(&seg));
}

/// Test 37.
#[test]
fn full_folder_reports_each_error_and_warning_after_30_min() {
    let (cloud, a, _b) = pair();
    a.tick();
    a.task("Client", "Design");
    a.tick();
    sync(&a);
    let written = status(&a, SECOND).last_written_at.unwrap();
    assert!(folder_warnings(&a).is_empty());

    a.view.set_full(SECOND, true);
    cloud.advance(LEASE_BEAT_SECS);
    a.s().create_project(&pinput("More")).unwrap();
    a.tick();
    sync(&a);
    versions::save_now(&a.svc, &a.view, LIMIT).unwrap();
    let full = status(&a, SECOND);
    for (what, why) in [
        ("data", &full.last_error),
        ("lease", &full.lease_error),
        ("version", &full.last_version_error),
    ] {
        assert!(
            why.as_deref().is_some_and(|w| w.contains(NO_ROOM)),
            "{what}: {why:?}"
        );
    }
    assert_eq!(full.last_written_at, Some(written), "nothing landed since");
    let fine = status(&a, MAIN);
    assert!(
        fine.last_error.is_none()
            && fine.lease_error.is_none()
            && fine.last_version_error.is_none()
    );
    assert!(
        fine.last_written_at > Some(written),
        "the first folder took everything"
    );

    // Not mentioned at once, nor at 29 min 59 s; at 30 min the always-visible warning says which and where it's safe.
    while a.s().now().secs() - written.secs() < 1799 {
        cloud.advance(300.min(1799 - (a.s().now().secs() - written.secs())));
        a.tick();
        sync(&a);
        assert!(folder_warnings(&a).is_empty(), "{:?}", folder_warnings(&a));
    }
    cloud.advance(1);
    let warned = folder_warnings(&a);
    assert_eq!(warned.len(), 1, "{warned:?}");
    assert!(
        warned[0].starts_with("GoogleDrive folder hasn't been written since 30 min ago: ")
            && warned[0].contains(NO_ROOM)
            && warned[0].ends_with("OneDrive folder is fine, so your data is safe there."),
        "{warned:?}"
    );
    assert!(a.s().snapshot().warnings.contains(&warned[0]));

    // Room again: the next sync ends the warning and every error.
    a.view.set_full(SECOND, false);
    a.tick();
    sync(&a);
    versions::save_now(&a.svc, &a.view, LIMIT).unwrap();
    assert!(folder_warnings(&a).is_empty());
    let healed = status(&a, SECOND);
    assert!(
        healed.last_error.is_none()
            && healed.lease_error.is_none()
            && healed.last_version_error.is_none()
    );
}

/// Test 38.
#[test]
fn lag_reported_when_one_folder_behind() {
    let (cloud, a, b) = pair();
    sync_all(&[&a, &b]);
    assert_eq!(
        (status(&a, MAIN).lag_secs, status(&a, SECOND).lag_secs),
        (None, None)
    );
    // The laptop's cloud app has stopped taking files in the first folder, and it carries on in the second.
    b.view.set_full(MAIN, true);
    for i in 0..6 {
        cloud.advance(600);
        b.s().create_project(&pinput(&format!("Work {i}"))).unwrap();
        sync(&b);
        sync(&a);
    }
    // An hour is not worth saying; what's more than that is.
    assert_eq!(status(&a, MAIN).lag_secs, None, "just an hour");
    cloud.advance(600);
    b.s().create_project(&pinput("Work 6")).unwrap();
    sync(&b);
    sync(&a);
    let behind = status(&a, MAIN)
        .lag_secs
        .expect("the first folder is behind");
    assert!(behind > 3600 && behind <= 7 * 600, "{behind}");
    assert_eq!(
        status(&a, SECOND).lag_secs,
        None,
        "the second is the fresh one"
    );
    // Nothing is lost for it: the data came through the second folder.
    assert_eq!(
        a.s().file().data.projects.len(),
        b.s().file().data.projects.len()
    );

    // The laptop's folder works again, and the lag goes.
    b.view.set_full(MAIN, false);
    sync(&b);
    sync(&a);
    assert_eq!(status(&a, MAIN).lag_secs, None);
}

/// Test 39. Leaving a folder: the lease is let go there, every file stays, and the folder is still somewhere to look.
#[test]
fn removing_a_folder_releases_there_and_keeps_files() {
    let cloud = cloud();
    let a = Device::open(&cloud, "DESKTOP", &[MAIN, SECOND]);
    let b = Device::open(&cloud, "LAPTOP", &[MAIN, SECOND]);
    let c = Device::open(&cloud, "TABLET", &[SECOND]);
    for d in [&a, &b, &c] {
        d.tick();
    }
    let (p, t) = a.task("Client", "Design");
    a.s().start_task(&t).unwrap();
    a.tick();
    sync_all(&[&a, &b, &c]);
    let version = save(&a);
    assert_eq!(roots_of(&a, &version), [MAIN, SECOND]);
    let id = a.id();
    let in_second = |name: &str| file_in(SECOND, &version, name);
    let device_file = FakeCloud::device_file_path(SECOND, &id);
    let files = (
        cloud.latest(&device_file).unwrap(),
        cloud.latest(&in_second(".json.gz")).unwrap(),
        cloud.latest(&in_second(".changes.txt")).unwrap(),
    );

    cloud.advance(LEASE_BEAT_SECS);
    lease_io::release_one(&a.svc, &a.view, SECOND, LIMIT);
    a.s().remove_sync_target(SECOND).unwrap();
    assert_eq!(roots(&a), [MAIN]);
    assert_eq!(
        record(&cloud, SECOND, &a).unwrap().state,
        LeaseRecordState::Released,
        "let go there"
    );
    assert_eq!(
        record(&cloud, MAIN, &a).unwrap().state,
        LeaseRecordState::Held
    );

    // Nothing was deleted.
    assert_eq!(cloud.latest(&device_file).unwrap(), files.0);
    assert_eq!(cloud.latest(&in_second(".json.gz")).unwrap(), files.1);
    assert_eq!(cloud.latest(&in_second(".changes.txt")).unwrap(), files.2);

    // The tablet reads only that folder, where it's free; the laptop reads both, where it's still held.
    c.tick();
    assert_eq!(c.state(), FREE);
    assert!(!c.s().read_only());
    b.tick();
    assert_eq!(b.state(), LIVE);
    minutes(&cloud, &[&a, &b], 4);
    assert_eq!((a.state(), b.state()), (MINE, LIVE));

    // No pass writes there again, and the desktop's work goes on through the folder it kept.
    let history = (
        cloud.history(&device_file).len(),
        cloud.lease_history(SECOND, &id).len(),
    );
    cloud.advance(60);
    rename(&a, &p, "Renamed since");
    sync(&a);
    sync(&b);
    assert_eq!(project_name(&b, &p), "Renamed since");
    assert_eq!(
        (
            cloud.history(&device_file).len(),
            cloud.lease_history(SECOND, &id).len()
        ),
        history
    );

    // The versions in the folder it left can still be listed and restored.
    let list = versions::list(&a.svc, &a.view, Some(SECOND), LIMIT);
    assert!(list.errors.is_empty());
    let there = list.versions.iter().find(|v| v.name == version).unwrap();
    assert_eq!(
        there
            .locations
            .iter()
            .map(|l| l.root.as_str())
            .collect::<Vec<_>>(),
        [MAIN, SECOND]
    );
    assert!(
        listed(&a).versions.iter().any(|v| v.name == version),
        "still in the first"
    );
    restore(&a, SECOND, &version);
    assert_eq!(project_name(&a, &p), "Client");
}

/// Test 40. A second folder added by the move flow gets the others' files at once, and the lease is written there at
/// the next beat; a replaced folder is let go of first.
#[test]
fn move_flow_seeds_second_folder_and_lease_follows() {
    let cloud = cloud();
    let a = Device::open(&cloud, "DESKTOP", &[MAIN]);
    let b = Device::open(&cloud, "LAPTOP", &[MAIN]);
    let (p, t, seg) = desktop_timing(&a, &b);
    let (a_id, b_id) = (a.id(), b.id());

    let moved = add_by_move(&a, MAIN, SECOND);
    assert_eq!((moved.files_copied, moved.files_already_there), (1, 0));
    assert_eq!(roots(&a), [MAIN, SECOND]);
    assert!(
        cloud.exists(&FakeCloud::device_file_path(SECOND, &b_id)),
        "the laptop's file went across"
    );
    assert!(cloud.exists(&FakeCloud::device_file_path(SECOND, &a_id)));
    assert!(
        record(&cloud, SECOND, &a).is_none(),
        "the lease follows at the next beat"
    );
    cloud.advance(LEASE_BEAT_SECS);
    a.tick_if_due();
    let (main, second) = (
        record(&cloud, MAIN, &a).unwrap(),
        record(&cloud, SECOND, &a).unwrap(),
    );
    assert_eq!(
        (
            second.generation,
            second.state,
            second.open_segments.clone()
        ),
        (main.generation, LeaseRecordState::Held, vec![seg.clone()])
    );
    assert_eq!(a.state(), MINE, "same claim, nothing handed over");

    // A computer that only has the new folder sees the holder and the data.
    let c = Device::open(&cloud, "TABLET", &[SECOND]);
    sync_all(&[&c]);
    c.tick();
    assert_eq!(c.state(), LIVE);
    assert_eq!(c.s().lease_view().holder.unwrap().device_name, "DESKTOP");
    assert_eq!(project_name(&c, &p), "Client");
    assert_eq!(c.s().file().data.tasks[&t].state, TaskState::Running);

    // The laptop adds it too, and goes on seeing the same holder.
    let moved = add_by_move(&b, MAIN, SECOND);
    assert_eq!(roots(&b), [MAIN, SECOND]);
    assert_eq!(moved.files_ignored, 0);
    b.tick();
    assert_eq!(b.state(), LIVE);
    assert_eq!(b.s().lease_view().reachable_roots, [MAIN, SECOND]);

    // Replacing the first folder: it is let go of, and the new one gets the record at the next beat.
    lease_io::release_one(&a.svc, &a.view, MAIN, LIMIT);
    move_target(
        &a.svc,
        &a.view,
        Some(MAIN),
        &custom(THIRD),
        Some(MAIN),
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(roots(&a), [SECOND, THIRD]);
    assert_eq!(
        record(&cloud, MAIN, &a).unwrap().state,
        LeaseRecordState::Released
    );
    assert!(record(&cloud, THIRD, &a).is_none());
    minutes(&cloud, &[&a], 3);
    assert_eq!(
        record(&cloud, THIRD, &a).unwrap().state,
        LeaseRecordState::Held
    );
    assert_eq!(a.state(), MINE);
    // The laptop still reads the first folder and the second: the second says held.
    minutes(&cloud, &[&b], 2);
    assert_eq!(b.state(), LIVE);
}

// Adversarial scenarios from the failure-mode table (section 10) that the other scenario files don't cover.

fn hm(at: crate::time::Ts) -> String {
    at.datetime().format("%H:%M").to_string()
}

/// F1 and F3 with slow clouds both ways. The desktop goes quiet with its timer running and the laptop takes over,
/// but what each writes takes ten minutes to show on the other. The desktop's late beats, which reach the laptop
/// after it holds, must not hand the lease back; the laptop's claim reaches the desktop late, and then the desktop
/// stops. Everything either did is kept.
#[test]
fn take_over_with_slow_clouds_both_ways_neither_flaps_and_all_work_merges() {
    const SLOW: i64 = 600;
    let (cloud, a, b) = pair();
    cloud.delay(&a.id(), &b.id(), SLOW);
    cloud.delay(&b.id(), &a.id(), SLOW);
    a.tick();
    b.tick();
    let (p, t) = a.task("Client", "Design");
    a.s().start_task(&t).unwrap();
    a.tick();
    let seg = a.running();
    for _ in 0..30 {
        cloud.advance(60);
        for d in [&a, &b] {
            d.tick_if_due();
            sync(d);
        }
    }
    assert_eq!(
        (a.state(), b.state()),
        (MINE, LIVE),
        "ten minutes late, still live"
    );

    // The desktop's network goes, its timer runs on. The laptop sees what was already on its way, then silence.
    a.view.set_offline(true);
    let mut waited = 0;
    while b.state() != STALE {
        assert!(waited < 40, "never looked inactive");
        cloud.advance(60);
        b.tick_if_due();
        sync(&b);
        waited += 1;
    }
    let taken = take_over_stale(&b);
    assert_eq!(taken.outcome, TakeOverOutcome::Done);
    assert_eq!(b.state(), MINE);
    let claimed_at = b.s().now();
    // The laptop's own new work goes on at once.
    let (_, other) = b.task("Home", "Admin");
    b.s().start_task(&other).unwrap();
    sync(&b);

    // The desktop is back on the network, but hears nothing from the laptop for ten minutes and beats on.
    a.view.set_offline(false);
    cloud.advance(60);
    a.tick_if_due();
    sync(&a);
    assert_eq!(a.state(), MINE, "nothing to hear yet");
    // What the desktop wrote after the take over shows on the laptop ten minutes later, and is from the old generation.
    let mut late_beat_read = false;
    while b.s().now().secs() - claimed_at.secs() < SLOW + 300 {
        cloud.advance(60);
        for d in [&a, &b] {
            d.tick_if_due();
            sync(d);
        }
        late_beat_read |= read_as(&b, MAIN, &a).is_some_and(|r| r.beat_at > claimed_at);
        assert_eq!(
            b.state(),
            MINE,
            "late beats from a lower generation change nothing"
        );
    }
    assert!(
        late_beat_read,
        "the laptop did read the desktop's late beats"
    );
    // By now the claim has reached the desktop, which stopped its timer and went read-only.
    assert_eq!(a.state(), LIVE);
    assert!(a.s().read_only());
    let stopped = a.s().file().data.segments[&seg]
        .end
        .expect("its own timer is closed");
    assert!(
        a.notices().contains(&format!(
            "LAPTOP took over, so this computer is now read-only. \"Design\" stopped timing here at {}. Nothing was lost: it will sync.",
            hm(stopped)
        )),
        "{:#?}",
        a.notices()
    );
    assert!(
        !b.notices().iter().any(|n| n.contains("read-only")),
        "the laptop never stopped"
    );

    // Let everything arrive.
    for _ in 0..4 {
        cloud.advance(SLOW);
        for d in [&a, &b] {
            d.tick_if_due();
            sync(d);
        }
    }
    for d in [&a, &b] {
        let s = d.s();
        let seg_now = &s.file().data.segments[&seg];
        assert_eq!(
            seg_now.effective(),
            Some((seg_now.start, Some(stopped))),
            "the desktop's real end, not the laptop's guess, on {}",
            d.name()
        );
        assert_eq!(
            s.file().data.tasks[&other].name,
            "Admin",
            "the laptop's work, on {}",
            d.name()
        );
        assert_eq!(s.file().data.projects[&p].name, "Client");
    }
    assert_eq!((a.state(), b.state()), (LIVE, MINE));
}

/// F3 and F12. The desktop did real work while it was cut off and unaware: a new project, a task and a time entry.
/// When it hears of the take over it stops, and that work still reaches the laptop and nothing is asked of the user.
#[test]
fn holder_returning_after_take_over_with_unsynced_local_work_loses_nothing() {
    let (cloud, a, b) = pair();
    let (_, t, seg) = desktop_timing(&a, &b);
    minutes(&cloud, &[&a, &b], 30);
    a.view.set_offline(true);
    minutes(&cloud, &[&b], 16);
    assert_eq!(b.state(), STALE);
    take_over_stale(&b);
    sync(&b);
    minutes(&cloud, &[&b], 14);

    // Cut off, the desktop is writable as ever, and it works.
    assert_eq!(a.state(), MINE);
    let (extra, extra_task) = a.task("Offline client", "Report");
    let at = a.s().now();
    a.s()
        .add_manual_entry(&crate::adjust::ManualEntryInput {
            task_id: extra_task.clone(),
            start: at.later(-1800),
            end: at.later(-900),
            reason: "worked on paper".into(),
            subtask_id: None,
            allow_overlap: true,
        })
        .unwrap();
    let entries = a.s().file().data.segments.len();

    // It comes back: the first look says another computer holds the lease, and the timer stops.
    a.view.set_offline(false);
    let out = a.tick();
    assert!(out.lost);
    assert_eq!(a.state(), LIVE);
    assert!(a.s().read_only());
    assert_eq!(a.s().file().data.tasks[&t].state, TaskState::Paused);
    assert_eq!(
        a.s()
            .create_project(&pinput("Too late"))
            .unwrap_err()
            .code(),
        "read_only"
    );
    sync_all(&[&a, &b]);

    // Read-only still sends its own file, so the laptop has everything the desktop did.
    for d in [&a, &b] {
        let s = d.s();
        assert_eq!(
            s.file().data.projects[&extra].name,
            "Offline client",
            "{}",
            d.name()
        );
        assert_eq!(s.file().data.tasks[&extra_task].name, "Report");
        assert!(s.file().data.segments.len() >= entries, "{}", d.name());
    }
    let real_end = a.s().file().data.segments[&seg].end.unwrap();
    for d in [&a, &b] {
        let s = d.s();
        let back = &s.file().data.segments[&seg];
        assert_eq!(
            back.effective(),
            Some((back.start, Some(real_end))),
            "{}",
            d.name()
        );
    }
    // The laptop is still the writer and may carry on at once.
    assert_eq!(b.state(), MINE);
    b.s().create_project(&pinput("Next")).unwrap();
    sync_all(&[&a, &b]);
    assert!(
        a.s()
            .file()
            .data
            .projects
            .values()
            .any(|p| p.name == "Next")
    );
    assert_eq!(cloud.lease_record(MAIN, &b.id()).unwrap().generation, 2);
}

/// What a cloud client does to a file that two computers wrote at once: `name.ext.more` becomes `name (1).ext.more`.
fn fork(cloud: &FakeCloud, path: &Path, suffix: &str) -> PathBuf {
    let file = path.file_name().unwrap().to_string_lossy().into_owned();
    let (stem, rest) = file.split_once('.').unwrap();
    let copy = path.with_file_name(format!("{stem}{suffix}.{rest}"));
    cloud.put(&copy, &cloud.latest(path).unwrap());
    copy
}

/// F4, with the first folder out of reach: the second folder is all there is, and a cloud client forks the lease
/// record, the device file and a version's files in it. The copies are neither read nor deleted nor listed.
#[test]
fn conflict_copies_of_lease_and_version_files_in_the_second_folder_change_nothing() {
    let (cloud, a, b) = pair();
    let (p, _, _) = desktop_timing(&a, &b);
    let version = save(&a);
    for d in [&a, &b] {
        d.view.set_offline_root(MAIN, true);
    }
    let id = a.id();
    let known: Vec<String> = listed(&b).versions.into_iter().map(|v| v.name).collect();
    assert!(known.contains(&version));
    let copies = [
        fork(&cloud, &FakeCloud::lease_path(SECOND, &id), " (1)"),
        fork(&cloud, &FakeCloud::device_file_path(SECOND, &id), "-PC2"),
        fork(&cloud, &file_in(SECOND, &version, ".json.gz"), " (1)"),
        fork(&cloud, &file_in(SECOND, &version, ".changes.txt"), " (1)"),
    ];
    // A copy that would outrank the real record if it were read, in a name the real one never has.
    let forged = LeaseRecord {
        generation: 99,
        ..record(&cloud, SECOND, &a).unwrap()
    };
    let forged_path = lease_io::lease_dir(Path::new(SECOND)).join(format!("{id} (2).json"));
    cloud.put(&forged_path, &serde_json::to_vec(&forged).unwrap());

    minutes(&cloud, &[&a, &b], 3);
    sync_all(&[&a, &b]);
    assert_eq!((a.state(), b.state()), (MINE, LIVE));
    assert_eq!(
        b.s().lease_view().holder.unwrap().generation,
        1,
        "not the copy's 99"
    );
    for d in [&a, &b] {
        assert!(status(d, SECOND).ignored_files >= 2, "said, as ignored");
    }
    let after: Vec<String> = listed(&b).versions.into_iter().map(|v| v.name).collect();
    assert_eq!(after, known, "the same versions, not one more per copy");
    assert_eq!(roots_of(&b, &version), [SECOND]);
    assert!(
        versions::preview_restore(&b.svc, &b.view, SECOND, &format!("{version} (1)"), LIMIT)
            .is_err(),
        "a copy is not a version"
    );

    // Releasing, claiming, saving and thinning out never remove one.
    a.quit(LIMIT);
    b.tick();
    assert_eq!(b.state(), FREE);
    rename(&b, &p, "Renamed");
    b.tick();
    assert_eq!(b.state(), MINE);
    cloud.advance(7200);
    sync(&b);
    save(&b);
    for copy in copies.iter().chain([&forged_path]) {
        assert!(cloud.exists(copy), "{} is still there", copy.display());
    }
    // And the real version restores from the folder that is left.
    restore(&b, SECOND, &version);
    assert_eq!(project_name(&b, &p), "Client");
}

/// F5, hours either way. A computer whose clock is hours off is still seen as in use while it beats and as inactive
/// once it stops, the dialog says its clock differs, a take over from it closes its timer at a time that makes sense,
/// and its real end comes back when it returns.
#[test]
fn clock_skewed_by_hours_neither_frees_nor_freezes_the_lease_and_work_still_merges() {
    for skew in [5 * 3600, -5 * 3600] {
        let (cloud, a, b) = pair();
        a.clock.advance(skew);
        let (p, t, seg) = desktop_timing(&a, &b);
        minutes(&cloud, &[&a, &b], 6);
        sync_all(&[&a, &b]);
        let holder = b.s().lease_view().holder.unwrap();
        assert_eq!(b.state(), LIVE, "skew {skew}");
        assert!(
            holder.clock_suspect,
            "skew {skew}: its clock seems to differ"
        );
        assert!(
            holder.age_secs.is_some_and(|age| age < 300),
            "skew {skew}: age on this computer's clock, {:?}",
            holder.age_secs
        );
        assert_eq!(project_name(&b, &p), "Client", "its data still merges");

        // It goes quiet: inactive after a quarter of an hour of nothing new, not before.
        a.view.set_offline(true);
        minutes(&cloud, &[&b], 10);
        assert_eq!(b.state(), LIVE, "skew {skew}: ten minutes is not enough");
        minutes(&cloud, &[&b], 7);
        assert_eq!(b.state(), STALE, "skew {skew}");
        let preview = lease_io::preview_take_over(&b.svc, &b.view, LIMIT);
        assert!(!preview.live && preview.holder.as_ref().unwrap().clock_suspect);
        assert_eq!(preview.segments.len(), 1);
        let shown = &preview.segments[0];
        assert!(shown.secs >= 0, "skew {skew}: a length, not {}", shown.secs);

        let result = take_over_stale(&b);
        assert_eq!(result.outcome, TakeOverOutcome::Done, "skew {skew}");
        let settled = b.s().file().data.segments[&seg]
            .edit
            .clone()
            .expect("closed");
        assert!(
            settled.settled && settled.end >= settled.start,
            "skew {skew}: {settled:?}"
        );
        assert_eq!(b.s().file().data.tasks[&t].state, TaskState::Paused);

        // It returns with its real end, which replaces the guess.
        sync(&b);
        cloud.advance(600);
        a.view.set_offline(false);
        assert!(a.tick().lost, "skew {skew}");
        let real_end = a.s().file().data.segments[&seg].end.unwrap();
        sync_all(&[&a, &b]);
        for d in [&a, &b] {
            let s = d.s();
            let back = &s.file().data.segments[&seg];
            assert_eq!(
                back.effective(),
                Some((back.start, Some(real_end))),
                "skew {skew}: {}",
                d.name()
            );
        }
        assert_eq!(b.state(), MINE, "skew {skew}");
    }
}

/// F5, days. A file dated more than two days ahead is refused by the reader, as it always was; the lease record is
/// not a data file and goes on working, and the warning says what to check.
#[test]
fn a_clock_days_ahead_is_warned_about_while_the_lease_still_works() {
    let (cloud, a, b) = pair();
    a.clock.advance(3 * 86_400);
    desktop_timing_without_data(&a, &b);
    minutes(&cloud, &[&a, &b], 4);
    assert_eq!(b.state(), LIVE);
    assert!(b.s().lease_view().holder.unwrap().clock_suspect);
    for root in [MAIN, SECOND] {
        let said = status(&b, root).warnings;
        assert!(
            said.iter()
                .any(|w| w.contains("dated ahead of this computer's clock")),
            "{root}: {said:?}"
        );
    }
    assert!(
        b.s().file().data.tasks.is_empty(),
        "the desktop's data is held back until the clocks agree"
    );
}

/// As `desktop_timing`, for a desktop whose file the laptop won't take, so the laptop never learns of its data.
fn desktop_timing_without_data(a: &Device, b: &Device) {
    a.tick();
    b.tick();
    let (_, t) = a.task("Client", "Design");
    a.s().start_task(&t).unwrap();
    a.tick();
    sync_all(&[a, b]);
    b.tick();
}

/// F6, with two folders. The cloud client puts back an old copy of the desktop's lease record in one folder, a
/// release from an earlier time. The other folder's newer record outvotes it, so the laptop stays read-only; and when
/// the cloud client does it in both, the desktop is stopped only by a real claim, and all work merges.
#[test]
fn cloud_restoring_an_old_lease_copy_in_one_folder_is_outvoted_by_the_other() {
    let (cloud, a, b) = pair();
    a.tick();
    b.tick();
    a.s().create_project(&pinput("P")).unwrap();
    a.tick();
    minutes(&cloud, &[&a, &b], 15);
    assert_eq!(a.state(), FREE, "released when idle");
    let (p, t) = a.task("Client", "Design");
    a.s().start_task(&t).unwrap();
    a.tick();
    minutes(&cloud, &[&a, &b], 3);
    sync_all(&[&a, &b]);
    assert_eq!(record(&cloud, MAIN, &a).unwrap().generation, 2);
    let released_copy = |root: &str| {
        let path = FakeCloud::lease_path(root, &a.id());
        let history = cloud.history(&path);
        let at = history
            .iter()
            .rposition(|(_, bytes)| {
                bytes.as_deref().is_some_and(|b| {
                    crate::lease::parse_record(&format!("{}.json", a.id()), b)
                        .is_some_and(|r| r.state == LeaseRecordState::Released)
                })
            })
            .unwrap();
        (path, history.len() - 1 - at)
    };

    // One folder.
    let (path, back) = released_copy(MAIN);
    cloud.restore_old(&path, back);
    assert_eq!(
        record(&cloud, MAIN, &a).unwrap().state,
        LeaseRecordState::Released
    );
    b.tick();
    assert_eq!(
        b.state(),
        LIVE,
        "the second folder still says held, at a higher generation"
    );
    assert!(b.s().read_only());
    assert_eq!(
        b.s().create_project(&pinput("Q")).unwrap_err().code(),
        "read_only"
    );
    // The desktop puts it right at its next beat.
    minutes(&cloud, &[&a, &b], 2);
    assert_eq!(a.state(), MINE);
    assert_eq!(
        record(&cloud, MAIN, &a).unwrap().state,
        LeaseRecordState::Held
    );
    assert!(
        !a.notices().iter().any(|n| n.contains("read-only")),
        "it never stopped"
    );

    // Both folders.
    for root in [MAIN, SECOND] {
        let (path, back) = released_copy(root);
        cloud.restore_old(&path, back);
    }
    b.tick();
    assert_eq!(b.state(), FREE, "nothing says held now");
    rename(&b, &p, "Renamed by the laptop");
    b.tick();
    assert_eq!(
        b.state(),
        MINE,
        "its change claims the lease above the desktop's"
    );
    a.tick();
    assert_eq!(a.state(), LIVE, "the desktop hears of it at its next look");
    assert!(a.s().read_only());
    sync_all(&[&a, &b]);
    for d in [&a, &b] {
        assert_eq!(project_name(d, &p), "Renamed by the laptop", "{}", d.name());
        assert_eq!(d.s().file().data.tasks[&t].name, "Design");
    }
}

/// F13, on the take over. The laptop's second folder is full, so the claim, the safety version and the status go to
/// the first, the take over goes through, and the second folder catches up once there is room.
#[test]
fn second_folder_full_does_not_block_take_over_versions_or_restore() {
    let (cloud, a, b) = pair();
    let (p, _, seg) = desktop_timing(&a, &b);
    let before = save(&a);
    minutes(&cloud, &[&a, &b], 20);
    sync_all(&[&a, &b]);
    a.view.set_offline(true);
    minutes(&cloud, &[&b], 16);
    assert_eq!(b.state(), STALE);
    b.view.set_full(SECOND, true);

    assert_eq!(take_over_stale(&b).outcome, TakeOverOutcome::Done);
    assert_eq!(b.state(), MINE);
    assert!(record(&cloud, MAIN, &b).is_some_and(|r| r.generation == 2));
    assert!(record(&cloud, SECOND, &b).is_none());
    assert!(
        b.s().file().data.segments[&seg].edit.is_some(),
        "the desktop's timer was settled"
    );
    sync(&b);
    let kept = only(named(&b, "takeover"));
    assert_eq!(
        roots_of(&b, &kept),
        [MAIN],
        "the safety version went where there was room"
    );
    let full = status(&b, SECOND);
    for why in [
        &full.last_error,
        &full.lease_error,
        &full.last_version_error,
    ] {
        assert!(
            why.as_deref().is_some_and(|w| w.contains(NO_ROOM)),
            "{why:?}"
        );
    }
    assert!(status(&b, MAIN).last_error.is_none());

    // A restore needs only one folder to take the copy of today's data.
    cloud.advance(60);
    rename(&b, &p, "After the take over");
    restore(&b, MAIN, &before);
    assert_eq!(project_name(&b, &p), "Client");
    let pre = only(named(&b, "pre-restore"));
    assert_eq!(roots_of(&b, &pre), [MAIN]);

    // Room again: the folder gets the lease record and the data at once, and the next version that is due.
    b.view.set_full(SECOND, false);
    minutes(&cloud, &[&b], 3);
    sync(&b);
    assert!(record(&cloud, SECOND, &b).is_some_and(|r| r.generation == 2));
    let healed = status(&b, SECOND);
    assert!(healed.last_error.is_none() && healed.lease_error.is_none());
    // The list says honestly where each copy is.
    assert_eq!(roots_of(&b, &kept), [MAIN]);
    assert_eq!(roots_of(&b, &pre), [MAIN]);
    cloud.advance(3600);
    rename(&b, &p, "An hour later");
    sync(&b);
    let hourly = named(&b, "auto");
    assert!(
        hourly.iter().any(|n| roots_of(&b, n) == [MAIN, SECOND]),
        "{hourly:?}"
    );
    assert!(status(&b, SECOND).last_version_error.is_none());
}

/// F14. Half of a version's files arrived in the first folder. Looking at that copy says it can't be read, restoring
/// goes to the whole copy in the second folder, and a version that is whole nowhere changes nothing.
#[test]
fn snapshot_partly_synced_in_one_folder_is_restored_from_the_other_and_refused_when_in_neither() {
    let (cloud, a) = {
        let cloud = cloud();
        let a = Device::open(&cloud, "DESKTOP", &[MAIN, SECOND]);
        (cloud, a)
    };
    let (p, _) = a.task("Client", "Design");
    sync_all(&[&a]);
    let version = save(&a);
    assert_eq!(roots_of(&a, &version), [MAIN, SECOND]);
    cloud.advance(60);
    rename(&a, &p, "Changed since");
    sync(&a);

    // The record of changes hasn't arrived in the first folder: the version is listed and the other copy reads.
    let record_in_main = file_in(MAIN, &version, ".changes.txt");
    cloud.delete(&record_in_main);
    assert_eq!(roots_of(&a, &version), [MAIN, SECOND]);
    assert!(versions::read_changes(&a.view, Path::new(MAIN), &version).is_err());
    assert!(
        versions::read_changes(&a.view, Path::new(SECOND), &version)
            .unwrap()
            .contains("DESKTOP")
    );

    // Only half of the data arrived in the first folder.
    cloud.truncate(&file_in(MAIN, &version, ".json.gz"));
    let why = versions::preview_restore(&a.svc, &a.view, MAIN, &version, LIMIT).unwrap_err();
    assert_eq!(why.code(), "conflict", "{why}");
    let whole = versions::preview_restore(&a.svc, &a.view, SECOND, &version, LIMIT).unwrap();
    assert!(whole.blocked_reason.is_none());

    // Asked for the first folder's copy, the restore uses the second's.
    restore(&a, MAIN, &version);
    assert_eq!(project_name(&a, &p), "Client");
    assert_eq!(a.s().file().prefs.epoch.as_ref().map(|e| e.epoch), Some(1));

    // Nothing whole anywhere: refused, and nothing about today's data changed.
    cloud.advance(60);
    rename(&a, &p, "Today");
    cloud.truncate(&file_in(SECOND, &version, ".json.gz"));
    let versions_before = listed(&a).versions.len();
    let refused = versions::restore(&a.svc, &a.view, MAIN, &version, LIMIT).unwrap_err();
    assert!(
        refused
            .to_string()
            .starts_with("No copy of this version could be read"),
        "{refused}"
    );
    assert_eq!(project_name(&a, &p), "Today");
    assert_eq!(
        a.s().file().prefs.epoch.as_ref().map(|e| e.epoch),
        Some(1),
        "no new restore"
    );
    assert_eq!(
        listed(&a).versions.len(),
        versions_before,
        "no copy of today was made for nothing"
    );
}

/// The epoch a device file in a folder carries, as any other computer reads it.
fn epoch_in(cloud: &FakeCloud, root: &str, d: &Device) -> Option<u64> {
    let bytes = cloud.latest(&FakeCloud::device_file_path(root, &d.id()))?;
    let file: crate::model::DeviceFile = serde_json::from_slice(&bytes).ok()?;
    file.epoch_info.map(|e| e.epoch)
}

/// Restoring from the second folder while the first can't be reached from the restorer. The restored data reaches
/// the other computers through the second folder, and the first folder, which still holds the old files, can't undo it
/// when it comes back; not even for a computer that has only the first folder.
#[test]
fn restore_from_second_folder_while_first_is_offline_holds_for_others_and_when_first_returns() {
    let cloud = cloud();
    let a = Device::open(&cloud, "DESKTOP", &[MAIN, SECOND]);
    let b = Device::open(&cloud, "LAPTOP", &[MAIN, SECOND]);
    let c = Device::open(&cloud, "TABLET", &[MAIN]);
    let (p, _) = a.task("Client", "Design");
    sync_all(&[&a, &b, &c]);
    let version = save(&a);
    cloud.advance(60);
    rename(&a, &p, "Renamed since");
    sync_all(&[&a, &b, &c]);
    for d in [&a, &b, &c] {
        assert_eq!(project_name(d, &p), "Renamed since", "{}", d.name());
    }
    assert_eq!(epoch_in(&cloud, MAIN, &a), None);

    // The first folder goes away for the desktop, which restores from the second.
    a.view.set_offline_root(MAIN, true);
    cloud.advance(60);
    restore(&a, SECOND, &version);
    assert_eq!(project_name(&a, &p), "Client");
    let pre = only(named(&a, "pre-restore"));
    assert_eq!(
        roots_of(&a, &pre),
        [SECOND],
        "today's data went where it could"
    );
    sync(&a);
    assert_eq!(epoch_in(&cloud, SECOND, &a), Some(1));
    assert_eq!(
        epoch_in(&cloud, MAIN, &a),
        None,
        "the first folder still holds the old file"
    );

    // The laptop reads both and follows; the old file in the first folder is left out, with a warning.
    sync(&b);
    assert_eq!(project_name(&b, &p), "Client");
    assert!(
        b.notices()
            .iter()
            .any(|n| n.contains("DESKTOP restored the version from")),
        "{:#?}",
        b.notices()
    );
    assert_eq!(
        epoch_in(&cloud, MAIN, &b),
        Some(1),
        "and it tells the first folder"
    );
    // The tablet has only the first folder, which now has the laptop's file at the new epoch.
    sync(&c);
    assert_eq!(project_name(&c, &p), "Client");

    // The first folder is back for the desktop. Nothing it holds brings the rolled-back name back.
    a.view.set_offline_root(MAIN, false);
    for _ in 0..3 {
        cloud.advance(60);
        sync_all(&[&a, &b, &c]);
        for d in [&a, &b, &c] {
            assert_eq!(project_name(d, &p), "Client", "{}", d.name());
        }
    }
    assert_eq!(epoch_in(&cloud, MAIN, &a), Some(1));
    let tablet = c.s().file().prefs.epoch.clone().unwrap();
    assert_eq!(tablet.epoch, 1);
    // Undoing it is one more restore of the copy made first, from the folder that has it.
    restore(&a, SECOND, &pre);
    sync_all(&[&a, &b, &c]);
    for d in [&a, &b, &c] {
        assert_eq!(project_name(d, &p), "Renamed since", "{}", d.name());
    }
}

/// F2 across folders. The desktop can reach only the second folder and the laptop only the first, so each finds it
/// free and claims at the same generation. When both can see both, the lower id keeps it, the other is told, and all
/// work merges.
#[test]
fn devices_seeing_different_folders_split_then_settle_on_one_holder() {
    let (cloud, a, b) = pair();
    a.view.set_offline_root(MAIN, true);
    b.view.set_offline_root(SECOND, true);
    a.tick();
    b.tick();
    assert_eq!((a.state(), b.state()), (FREE, FREE));
    let (pa, ta) = a.task("From desktop", "Design");
    let (pb, _) = b.task("From laptop", "Admin");
    a.s().start_task(&ta).unwrap();
    a.tick();
    b.tick();
    assert_eq!(
        (a.state(), b.state()),
        (MINE, MINE),
        "each believes it is alone"
    );
    assert_eq!(record(&cloud, SECOND, &a).unwrap().generation, 1);
    assert_eq!(record(&cloud, MAIN, &b).unwrap().generation, 1);
    minutes(&cloud, &[&a, &b], 3);

    // Both folders reach both computers again.
    a.view.set_offline_root(MAIN, false);
    b.view.set_offline_root(SECOND, false);
    let (winner, loser) = if a.id() < b.id() { (&a, &b) } else { (&b, &a) };
    let outcome = (winner.tick(), loser.tick());
    assert!(outcome.1.lost, "the one with the higher id is told");
    winner.tick();
    assert_eq!((winner.state(), loser.state()), (MINE, LIVE));
    assert!(loser.s().read_only());
    // Told at the first look: of a tie, or, when the winner has already written above it, of its claim.
    let told = loser.notices();
    let tie = format!(
        "{} started making changes at the same time, so this computer is now read-only.",
        winner.name()
    );
    let took = format!(
        "{} took over, so this computer is now read-only.",
        winner.name()
    );
    assert!(
        told.iter()
            .any(|n| (n.starts_with(&tie) || n.starts_with(&took))
                && n.ends_with("Nothing was lost: it will sync.")),
        "{told:#?}"
    );
    // It stays settled, whatever the loser's own record still says in either folder.
    minutes(&cloud, &[&a, &b], 5);
    assert_eq!((winner.state(), loser.state()), (MINE, LIVE));
    sync_all(&[&a, &b]);
    for d in [&a, &b] {
        let s = d.s();
        assert_eq!(
            s.file().data.projects[&pa].name,
            "From desktop",
            "{}",
            d.name()
        );
        assert_eq!(
            s.file().data.projects[&pb].name,
            "From laptop",
            "{}",
            d.name()
        );
    }
}

/// F13 and F9, both folders at once. Neither can be reached: time logging goes on, the page says the lease can't be
/// checked, each folder is reported after half an hour with where the data is, and everything reaches both when they
/// return.
#[test]
fn both_folders_failing_is_reported_for_each_and_nothing_blocks_logging() {
    let cloud = cloud();
    let a = Device::open(&cloud, "DESKTOP", &[MAIN, SECOND]);
    a.view.set_offline(true);
    a.tick();
    let lease = a.s().lease_view();
    assert_eq!(lease.state, LeaseStateName::Unknown);
    assert!(!lease.read_only, "nothing blocks time logging");
    assert_eq!(lease.unreachable_roots, [MAIN, SECOND]);

    let (p, t) = a.task("Client", "Design");
    a.s().start_task(&t).unwrap();
    minutes(&cloud, &[&a], 20);
    a.s().pause_task(&t).unwrap();
    sync(&a);
    assert!(folder_warnings(&a).is_empty(), "not yet");
    minutes(&cloud, &[&a], 12);
    sync(&a);
    let said = folder_warnings(&a);
    let why = "Folder not found. Is the drive connected and the desktop client running?";
    assert_eq!(
        said,
        [
            format!(
                "OneDrive folder hasn't been written since 32 min ago: {why} Your data is safe on this computer."
            ),
            format!(
                "GoogleDrive folder hasn't been written since 32 min ago: {why} Your data is safe on this computer."
            ),
        ]
    );
    for root in [MAIN, SECOND] {
        assert!(
            status(&a, root).last_error.as_deref() == Some(why),
            "{root}"
        );
    }

    // They come back, and get the whole of what was logged meanwhile.
    a.view.set_offline(false);
    a.tick();
    sync(&a);
    assert!(folder_warnings(&a).is_empty());
    for root in [MAIN, SECOND] {
        assert!(status(&a, root).last_error.is_none(), "{root}");
        let bytes = cloud
            .latest(&FakeCloud::device_file_path(root, &a.id()))
            .unwrap();
        let file: crate::model::DeviceFile = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(file.data.projects[&p].name, "Client", "{root}");
        assert_eq!(file.data.tasks[&t].name, "Design", "{root}");
        assert_eq!(file.data.segments.len(), 1, "{root}");
    }
    assert_eq!(a.state(), MINE, "the work done meanwhile claims it");
}

/// F14, for the small files. Half of the desktop's lease record and half of its device file arrived in the first
/// folder. The whole copies in the second folder are used, the cut ones are said to be ignored or damaged, and the
/// holder is neither lost from view nor is its work.
#[test]
fn lease_record_and_device_file_cut_short_in_one_folder_cost_nothing_while_the_other_is_whole() {
    let (cloud, a, b) = pair();
    let (p, _, _) = desktop_timing(&a, &b);
    minutes(&cloud, &[&a, &b], 2);
    let id = a.id();
    cloud.truncate(&FakeCloud::lease_path(MAIN, &id));
    cloud.advance(60);
    rename(&a, &p, "Renamed meanwhile");
    // Only the second folder takes the desktop's new file; the first has the cut one.
    a.view.set_offline_root(MAIN, true);
    sync(&a);
    a.view.set_offline_root(MAIN, false);
    cloud.truncate(&FakeCloud::device_file_path(MAIN, &id));
    b.view.set_offline_root(SECOND, true);
    sync(&b);
    b.tick();
    assert_ne!(
        project_name(&b, &p),
        "Renamed meanwhile",
        "from the first folder alone, the cut file gives nothing"
    );
    assert!(
        status(&b, MAIN)
            .warnings
            .iter()
            .any(|w| w.contains("DAMAGED") || w.contains("incomplete or damaged")),
        "{:?}",
        status(&b, MAIN).warnings
    );
    b.view.set_offline_root(SECOND, false);
    sync(&b);
    b.tick();
    assert_eq!(
        project_name(&b, &p),
        "Renamed meanwhile",
        "from the whole copy in the second"
    );
    assert_eq!(
        b.state(),
        LIVE,
        "the holder is seen through the whole record"
    );
    assert_eq!(b.s().lease_view().holder.unwrap().device_name, "DESKTOP");
    assert!(
        status(&b, MAIN).ignored_files >= 1,
        "the cut record is said to be ignored"
    );
    // The desktop's next beat and cycle put both files right.
    minutes(&cloud, &[&a, &b], 2);
    sync_all(&[&a, &b]);
    assert!(record(&cloud, MAIN, &a).is_some());
    assert_eq!(b.state(), LIVE);
    assert!(
        status(&b, MAIN).warnings.is_empty(),
        "{:?}",
        status(&b, MAIN).warnings
    );
}

/// F15 and F19 with the folders split. Two computers each restore a version while each can reach only one of the
/// folders, so neither sees the other's restore. When they see both folders, they settle on one restore, the same on
/// both, nothing brings the other back, and the copy of what the other had stays in a version.
#[test]
fn two_restores_made_without_seeing_each_other_converge_on_one_and_keep_the_other_copy() {
    let (cloud, a, b) = pair();
    let (p, _) = a.task("Client", "Design");
    sync_all(&[&a, &b]);
    let first = save(&a);
    cloud.advance(120);
    rename(&a, &p, "Second name");
    let second = save(&a);
    assert_ne!(first, second);
    cloud.advance(120);
    sync_all(&[&a, &b]);

    a.view.set_offline_root(MAIN, true);
    b.view.set_offline_root(SECOND, true);
    restore(&a, SECOND, &first);
    restore(&b, MAIN, &second);
    sync(&a);
    sync(&b);
    assert_eq!(project_name(&a, &p), "Client");
    assert_eq!(project_name(&b, &p), "Second name");

    a.view.set_offline_root(MAIN, false);
    b.view.set_offline_root(SECOND, false);
    let (winner, loser) = if a.id() > b.id() { (&a, &b) } else { (&b, &a) };
    let kept = if a.id() > b.id() {
        "Client"
    } else {
        "Second name"
    };
    for _ in 0..4 {
        cloud.advance(60);
        sync_all(&[&a, &b]);
        for d in [&a, &b] {
            assert!(
                matches!(project_name(d, &p).as_str(), "Client" | "Second name"),
                "{}",
                project_name(d, &p)
            );
        }
    }
    assert_eq!(project_name(winner, &p), kept);
    assert_eq!(
        project_name(loser, &p),
        kept,
        "the one with the higher id decides"
    );
    assert!(
        loser
            .notices()
            .iter()
            .any(|n| n.contains(&format!("{} restored the version from", winner.name()))),
        "{:#?}",
        loser.notices()
    );
    assert_eq!(
        named(loser, "pre-adopt").len(),
        1,
        "what the loser had is kept"
    );
    // And the next syncs, days later, leave it alone.
    cloud.advance(3 * 86_400);
    sync_all(&[&a, &b]);
    for d in [&a, &b] {
        assert_eq!(project_name(d, &p), kept, "{}", d.name());
    }
}

/// A second folder that waits for the user's choice (it holds a restore this computer doesn't follow, and this
/// computer has data of its own) is neither read nor written, and the first folder carries on as ever.
#[test]
fn a_second_folder_waiting_for_a_choice_gets_nothing_while_the_first_carries_on() {
    let cloud = cloud();
    let c = Device::open(&cloud, "TABLET", &[SECOND]);
    let (restored, _) = c.task("Restored project", "Sketches");
    sync_all(&[&c]);
    let version = save(&c);
    cloud.advance(60);
    rename(&c, &restored, "Renamed since");
    restore(&c, SECOND, &version);
    sync(&c);
    let a = Device::open(&cloud, "DESKTOP", &[MAIN]);
    let (mine, t) = a.task("Mine", "Design");
    a.s().start_task(&t).unwrap();
    a.tick();
    sync(&a);

    // The move stops with the question, and nothing went into the folder.
    let stopped = move_target(
        &a.svc,
        &a.view,
        Some(MAIN),
        &custom(SECOND),
        None,
        &mut |_, _, _| {},
    );
    assert!(stopped.is_err());
    assert!(!cloud.exists(&FakeCloud::device_file_path(SECOND, &a.id())));
    // Connected anyway, it is still left alone.
    a.s().add_sync_target(custom(SECOND)).unwrap();
    let before = cloud
        .history(&FakeCloud::device_file_path(SECOND, &c.id()))
        .len();
    for _ in 0..3 {
        cloud.advance(LEASE_BEAT_SECS);
        a.tick();
        sync(&a);
    }
    let page = a
        .s()
        .snapshot()
        .sync
        .join_pending
        .expect("the question is on the page");
    assert_eq!(
        (page.root.as_str(), page.device_name.as_str()),
        (SECOND, "TABLET")
    );
    assert_eq!(a.state(), MINE, "the lease runs on the first folder");
    assert!(record(&cloud, SECOND, &a).is_none());
    assert!(!cloud.exists(&FakeCloud::device_file_path(SECOND, &a.id())));
    assert!(
        !a.s().file().data.projects.contains_key(&restored),
        "nothing read from it"
    );
    let saved = save(&a);
    assert_eq!(
        roots_of(&a, &saved),
        [MAIN],
        "the version went where it is safe to"
    );
    assert_eq!(
        cloud
            .history(&FakeCloud::device_file_path(SECOND, &c.id()))
            .len(),
        before,
        "the tablet's own files are as they were"
    );
    assert!(status(&a, MAIN).last_error.is_none());

    // Keeping both: the folder is joined, this computer's work goes in, and the tablet follows the union.
    epoch::resolve_join(&a.svc, &a.view, SECOND, false, LIMIT).unwrap();
    for _ in 0..3 {
        cloud.advance(LEASE_BEAT_SECS);
        a.tick();
        sync_all(&[&a, &c]);
    }
    assert!(a.s().snapshot().sync.join_pending.is_none());
    assert!(cloud.exists(&FakeCloud::device_file_path(SECOND, &a.id())));
    assert!(record(&cloud, SECOND, &a).is_some_and(|r| r.state == LeaseRecordState::Held));
    assert_eq!(
        project_name(&a, &restored),
        "Restored project",
        "the restore stands"
    );
    for d in [&a, &c] {
        assert_eq!(project_name(d, &mine), "Mine", "{}", d.name());
    }
}

/// DEFECT D2 (wording): the text for a folder waiting for a choice sends the user to "Settings, under Sync", but the
/// section is called "Sync between devices" (src/features/settings/SettingsTab.tsx:32), as the lease and version texts
/// say. Expected: all of them name the section as the page does. Actual: epoch.rs:27 and :28 say "under Sync".
#[test]
fn texts_about_a_folder_waiting_for_a_choice_name_the_settings_section_as_the_page_does() {
    let cloud = cloud();
    let c = Device::open(&cloud, "TABLET", &[SECOND]);
    let (restored, _) = c.task("Restored project", "Sketches");
    sync_all(&[&c]);
    let version = save(&c);
    cloud.advance(60);
    rename(&c, &restored, "Renamed since");
    restore(&c, SECOND, &version);
    sync(&c);
    let a = Device::open(&cloud, "DESKTOP", &[MAIN]);
    a.task("Mine", "Design");
    sync(&a);
    let stopped = move_target(
        &a.svc,
        &a.view,
        Some(MAIN),
        &custom(SECOND),
        None,
        &mut |_, _, _| {},
    )
    .unwrap_err();
    a.s().add_sync_target(custom(SECOND)).unwrap();
    sync(&a);
    let on_status = status(&a, SECOND).last_error.unwrap();
    for text in [&stopped, &on_status] {
        assert!(
            text.contains("Settings, under Sync between devices"),
            "names the section as the page does: {text}"
        );
    }
}

/// DEFECT D1 (no guard): a folder that is already connected can be added again under another spelling (here a trailing
/// slash; on Windows, another letter case). Expected: it is not a second folder, as it is no second copy. Actual: two
/// folders are listed with "second copy" on the same files, and removing the one releases the lease the other
/// still holds. Design 6.1 says nothing; see the report's Open doubts.
#[test]
fn the_same_folder_under_another_spelling_is_not_added_as_a_second_copy() {
    let cloud = cloud();
    let a = Device::open(&cloud, "DESKTOP", &[MAIN]);
    let b = Device::open(&cloud, "LAPTOP", &[MAIN]);
    let (_, t) = a.task("Client", "Design");
    a.s().start_task(&t).unwrap();
    a.tick();
    sync_all(&[&a, &b]);
    let alias = format!("{MAIN}/");
    let _ = move_target(
        &a.svc,
        &a.view,
        Some(MAIN),
        &custom(&alias),
        None,
        &mut |_, _, _| {},
    );
    let _ = a.s().add_sync_target(custom(&alias));
    let listed_roots = roots(&a);
    assert_eq!(
        listed_roots.len(),
        1,
        "one folder, spelled twice: {listed_roots:?}"
    );
}

/// Review R1: "Keep this computer's data too" makes a restore that every computer follows, so a read-only computer can't
/// choose it. What it has seen of the holder's folder may be behind, and the holder would lose what it did since.
/// Following changes only this computer's data, so it stays open to a read-only computer.
#[test]
fn keeping_both_is_refused_on_a_read_only_computer_and_following_is_not() {
    let cloud = cloud();
    // TABLET holds the lease in SECOND and has a restore in its past.
    let c = Device::open(&cloud, "TABLET", &[SECOND]);
    let (restored, _) = c.task("Restored project", "Sketches");
    c.tick();
    sync(&c);
    let version = save(&c);
    cloud.advance(60);
    rename(&c, &restored, "Renamed since");
    restore(&c, SECOND, &version);
    c.tick();
    sync(&c);
    assert_eq!(c.state(), MINE);

    // DESKTOP has data of its own and connects SECOND: it is asked, and TABLET's lease there makes it read-only.
    let a = Device::open(&cloud, "DESKTOP", &[MAIN]);
    let (mine, _) = a.task("Mine", "Design");
    a.tick();
    sync(&a);
    lease_io::release_now(&a.svc, &a.view, LIMIT).unwrap();
    a.s().add_sync_target(custom(SECOND)).unwrap();
    sync(&a);
    a.tick();
    assert!(a.s().file().prefs.join_pending.is_some(), "asked");
    assert!(a.s().read_only(), "TABLET holds the lease");

    // TABLET goes on working, and its cloud app is slow to bring that to DESKTOP.
    cloud.hold(&c.id(), &a.id());
    cloud.advance(120);
    let (tablet_work, t) = c.task("Tablet work", "Drawing");
    c.s().start_task(&t).unwrap();
    cloud.advance(600);
    c.s().pause_task(&t).unwrap();
    c.tick();
    sync(&c);

    let before = a.s().file().clone();
    let refused = epoch::resolve_join(&a.svc, &a.view, SECOND, false, LIMIT).unwrap_err();
    assert_eq!(refused.code(), "read_only");
    assert_eq!(a.s().file().data, before.data, "nothing merged");
    assert_eq!(
        a.s().file().prefs.epoch,
        before.prefs.epoch,
        "no restore made"
    );
    assert!(a.s().file().prefs.join_pending.is_some(), "still asked");
    sync(&a);
    cloud.release(&c.id(), &a.id());
    cloud.advance(60);
    sync(&c);
    assert!(
        c.s().file().data.projects.contains_key(&tablet_work),
        "TABLET keeps its work"
    );
    assert_eq!(project_name(&c, &restored), "Restored project");

    // Following: this computer takes the restore as it stands, and the tablet's work with it.
    epoch::resolve_join(&a.svc, &a.view, SECOND, true, LIMIT).unwrap();
    assert!(a.s().file().prefs.join_pending.is_none());
    sync_all(&[&a, &c]);
    assert!(a.s().file().data.projects.contains_key(&tablet_work));
    assert!(
        !c.s().file().data.projects.contains_key(&mine),
        "nothing of DESKTOP's reached the tablet"
    );
}

/// UI review R1, the backend's half: a move stopped by another computer's restore asks about that folder, which isn't
/// connected. The question can be answered for it either way, after which the move goes through. Removing that folder,
/// which isn't connected, drops the question and changes nothing else.
#[test]
fn a_move_stopped_by_a_restore_can_be_answered_for_that_folder_and_then_goes_through() {
    for follow in [false, true] {
        let cloud = cloud();
        let c = Device::open(&cloud, "TABLET", &[SECOND]);
        let (restored, _) = c.task("Restored project", "Sketches");
        sync_all(&[&c]);
        let version = save(&c);
        cloud.advance(60);
        rename(&c, &restored, "Renamed since");
        restore(&c, SECOND, &version);
        sync(&c);
        let a = Device::open(&cloud, "DESKTOP", &[MAIN]);
        let (mine, _) = a.task("Mine", "Design");
        sync(&a);
        let move_in = || {
            move_target(
                &a.svc,
                &a.view,
                Some(MAIN),
                &custom(SECOND),
                None,
                &mut |_, _, _| {},
            )
        };
        let stopped = move_in().unwrap_err();
        assert!(
            stopped.contains("Settings, under Sync between devices"),
            "{stopped}"
        );
        let asked = a.s().snapshot().sync.join_pending.unwrap();
        assert_eq!(asked.root, SECOND);
        assert_eq!(roots(&a), [MAIN], "not connected");

        // Removing that folder, which isn't connected, only drops the question.
        a.s().remove_sync_target(SECOND).unwrap();
        assert!(a.s().snapshot().sync.join_pending.is_none());
        assert_eq!(roots(&a), [MAIN]);

        // Asked again, and answered for that folder: the move then goes through.
        move_in().unwrap_err();
        epoch::resolve_join(&a.svc, &a.view, SECOND, follow, LIMIT).unwrap();
        assert!(a.s().snapshot().sync.join_pending.is_none());
        move_in().unwrap();
        assert_eq!(roots(&a), [MAIN, SECOND]);
        for _ in 0..2 {
            cloud.advance(60);
            sync_all(&[&a, &c]);
        }
        for d in [&a, &c] {
            assert_eq!(project_name(d, &restored), "Restored project", "{follow}");
            assert_eq!(
                d.s().file().data.projects.contains_key(&mine),
                !follow,
                "kept both, or followed: {follow}"
            );
        }
    }
}

/// D1, as the owner decided: a folder that is already connected, spelled another way, is refused by name wherever a
/// folder is added, before anything is read from it or written to it. The same spelling again is no new folder either.
#[test]
fn the_same_folder_under_another_spelling_is_refused_naming_the_folder_connected() {
    let cloud = cloud();
    let a = Device::open(&cloud, "DESKTOP", &[MAIN]);
    a.task("Client", "Design");
    sync(&a);
    let alias = format!("{MAIN}/");
    let said = format!("That folder is already connected as {MAIN}.");
    for replacing in [None, Some(MAIN)] {
        let moved = move_target(
            &a.svc,
            &a.view,
            Some(MAIN),
            &custom(&alias),
            replacing,
            &mut |_, _, _| {},
        )
        .unwrap_err();
        assert_eq!(moved, said);
    }
    let added = a.s().add_sync_target(custom(&alias)).unwrap_err();
    assert_eq!(
        (added.code(), added.to_string()),
        ("validation", said.clone())
    );
    let switched = a
        .s()
        .switch_targets(None, custom(&alias), None)
        .unwrap_err();
    assert_eq!(switched.to_string(), said);
    assert_eq!(roots(&a), [MAIN]);
    a.s().add_sync_target(custom(MAIN)).unwrap();
    assert_eq!(roots(&a), [MAIN]);
}
