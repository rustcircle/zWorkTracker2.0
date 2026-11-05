//! The first start of this version in a data folder (design section 4, tests 7.4; addendum A1.1 and A1.4).
//!
//! A 1.3.1 store gives its identity and nothing else: the data comes back from the sync folders through the sync path,
//! and nothing is written to a folder until it has. Real folders on disk (`RealIo`) in temporary directories, a fake
//! clock, `run_cycle_within` as the app runs it. Every test with an old store checks at its end that the store's bytes
//! and modification time are as they were.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use serde_json::{Value, json};

use crate::backup::LEGACY_COPY;
use crate::drafts::{self, Draft};
use crate::error::CoreError;
use crate::lease_io;
use crate::merge::merge;
use crate::model::{DeviceFile, PauseReason, Shared, SyncProvider, SyncTarget, TaskState};
use crate::persist::MAIN_FILE;
use crate::service::Service;
use crate::store::DB_FILE;
use crate::sync::{RealIo, devices_dir, run_cycle_within};
use crate::testkit::{LIMIT, doc, pinput, tinput};
use crate::time::{Clock, ManualClock, Ts};
use crate::versions;

const T0: &str = "2026-11-03T09:00:00Z";
const NAME: &str = "Studio Mac";
const BAK: &str = "z-work-data.json.bak";
/// Studio Mac in the C0 fixtures.
const FIXTURE_ID: &str = "aaaaaaaa-1111-4111-8111-111111111111";

const NOTICE_B: &str = "zWorkTracker now keeps its data in a database on this computer, and starts empty because no sync folder was connected. Your earlier data is still on this computer: restore the backup z-work-data-legacy-1.3.1.json in Settings > Backups to bring it back.";
const NOTICE_C: &str = "zWorkTracker now keeps its data in a database on this computer. Your projects and hours are being brought back from OneDrive, and appear after the first sync. The data file of the previous version was left as it was, and a copy of it is in Settings > Backups as z-work-data-legacy-1.3.1.json.";
const W1: &str = "This computer's data hasn't arrived from OneDrive yet, so nothing is written there until it does. Check that the folder is synced to this computer, then press Sync now in Settings > Sync between devices.";
const NO_VERSION_YET: &str = "This computer's data hasn't arrived from your sync folder yet, so no version can be saved there yet.";

fn lock(m: &Mutex<Service>) -> MutexGuard<'_, Service> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

/// A file as it is on disk.
#[derive(Debug, PartialEq, Eq)]
struct Untouched {
    path: PathBuf,
    bytes: Vec<u8>,
    modified: SystemTime,
}

fn untouched(path: &Path) -> Untouched {
    Untouched {
        path: path.to_path_buf(),
        bytes: fs::read(path).unwrap(),
        modified: fs::metadata(path).unwrap().modified().unwrap(),
    }
}

impl Untouched {
    fn check(&self) {
        assert_eq!(
            &untouched(&self.path),
            self,
            "a 1.3.1 file is never changed: {}",
            self.path.display()
        );
    }
}

fn target(root: &Path) -> SyncTarget {
    SyncTarget {
        provider: SyncProvider::OneDrive,
        root: root.to_string_lossy().into_owned(),
    }
}

/// The store 1.3.1 left on Studio Mac (the C0 fixture), with this computer's identity and folders put in. Its data, six
/// projects and nine segments, is never taken.
fn legacy_bytes(id: &str, name: &str, roots: &[&Path]) -> Vec<u8> {
    let mut file: Value =
        serde_json::from_slice(&fs::read(fixture("cloud-1.3.1.local.json")).unwrap()).unwrap();
    let roots: Vec<String> = roots
        .iter()
        .map(|r| r.to_string_lossy().into_owned())
        .collect();
    file["deviceId"] = json!(id);
    file["deviceName"] = json!(name);
    file["prefs"]["syncTargets"] = roots
        .iter()
        .map(|r| json!({"provider": "oneDrive", "root": r}))
        .collect();
    file["prefs"]["syncJoined"] = json!(roots);
    serde_json::to_vec(&file).unwrap()
}

fn write_legacy(dir: &Path, name: &str, bytes: &[u8]) -> Untouched {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    untouched(&path)
}

/// What a store 1.3.1 couldn't finish writing looks like: the first part of one.
fn truncated(bytes: &[u8]) -> Vec<u8> {
    bytes[..bytes.len() / 2].to_vec()
}

fn open(dir: &Path, clock: &ManualClock) -> Mutex<Service> {
    Mutex::new(Service::open(dir, Arc::new(clock.clone()), "this-host").unwrap())
}

fn notices(m: &Mutex<Service>) -> Vec<(&'static str, String)> {
    lock(m)
        .snapshot()
        .notices
        .into_iter()
        .map(|n| (n.level, n.message))
        .collect()
}

fn holds_nothing(data: &Shared) -> bool {
    data.projects.is_empty() && data.tasks.is_empty() && data.segments.is_empty()
}

/// Every record of `here` serialises as the one in `there`, kind by kind, and nothing else is in either.
fn assert_same_records(here: &Shared, there: &Shared) {
    let (here, there) = (
        serde_json::to_value(here).unwrap(),
        serde_json::to_value(there).unwrap(),
    );
    for (kind, records) in there.as_object().unwrap() {
        for (id, record) in records.as_object().unwrap() {
            assert_eq!(here[kind].get(id), Some(record), "{kind} {id}");
        }
        assert_eq!(
            here[kind].as_object().unwrap().len(),
            records.as_object().unwrap().len(),
            "{kind}"
        );
    }
}

fn backups_dir(dir: &Path) -> PathBuf {
    dir.join("backups")
}

/// A computer already on this version that ran a task in `root` and synced once, then is gone. What it left there is
/// what a first start elsewhere with its identity is to bring back.
struct Left {
    id: String,
    task: String,
    segment: String,
    data: Shared,
    beat: Option<Ts>,
    /// Its own file in the folder, and the bytes it wrote there.
    own: PathBuf,
    bytes: Vec<u8>,
}

/// The task runs for 15 minutes, with a heartbeat after 10. A crash leaves it running in the folder.
fn a_computer_syncs_and_is_gone(root: &Path, clock: &ManualClock, crash: bool) -> Left {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Service::open(dir.path(), Arc::new(clock.clone()), NAME).unwrap();
    s.add_sync_target(target(root)).unwrap();
    let project = s.create_project(&pinput("Client work")).unwrap();
    let task = s.create_task(&tinput("Design review", &project)).unwrap();
    s.start_task(&task).unwrap();
    clock.advance(600);
    s.heartbeat().unwrap();
    clock.advance(300);
    if !crash {
        s.pause_task(&task).unwrap();
    }
    let segment = s
        .file()
        .data
        .segments
        .values()
        .find(|seg| seg.task_id == task)
        .unwrap()
        .id
        .clone();
    let m = Mutex::new(s);
    run_cycle_within(&m, &RealIo, LIMIT);
    let s = lock(&m);
    let id = s.file().device_id.clone();
    let own = devices_dir(root).join(format!("{id}.json"));
    let bytes = fs::read(&own).unwrap();
    let written: DeviceFile = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(written.data, s.file().data, "the folder holds all of it");
    assert_eq!(
        written.data.segments[&segment].end.is_none(),
        crash,
        "a crash leaves the run open there"
    );
    Left {
        id,
        task,
        segment,
        data: s.file().data.clone(),
        beat: s.file().last_heartbeat,
        own,
        bytes,
    }
}

#[test]
fn no_legacy_file_starts_a_fresh_device_with_the_setup_wizard() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(T0);
    let m = open(dir.path(), &clock);
    let s = lock(&m);
    assert!(uuid::Uuid::parse_str(&s.file().device_id).is_ok());
    assert_eq!(s.file().device_name, "this-host");
    let snap = s.snapshot();
    assert!(snap.setup.pending, "the setup wizard shows");
    assert!(snap.notices.is_empty(), "{:?}", snap.notices);
    assert!(holds_nothing(&s.file().data));
    assert!(!s.awaiting_cloud());
    assert!(dir.path().join(DB_FILE).is_file());
    assert!(
        !dir.path().join(MAIN_FILE).exists(),
        "no JSON store is made"
    );
    assert!(!backups_dir(dir.path()).join(LEGACY_COPY).exists());
}

#[test]
fn a_legacy_file_without_a_folder_keeps_the_identity_and_starts_empty() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::at(T0);
    let legacy = write_legacy(dir.path(), MAIN_FILE, &legacy_bytes(FIXTURE_ID, NAME, &[]));
    // A daily backup 1.3.1 wrote, which is what that store was on 8 October.
    let daily = "z-work-data-2026-10-08.json";
    fs::create_dir_all(backups_dir(dir.path())).unwrap();
    let daily_bytes = fs::read(fixture("cloud-1.3.1.local.json")).unwrap();
    fs::write(backups_dir(dir.path()).join(daily), &daily_bytes).unwrap();

    let m = open(dir.path(), &clock);
    {
        let s = lock(&m);
        let file = s.file();
        assert_eq!(file.device_id, FIXTURE_ID);
        assert_eq!(file.device_name, NAME);
        assert!(file.prefs.setup_done && file.prefs.sync_prompt_done);
        assert!(file.prefs.client_zone_prompt_done);
        assert!(file.prefs.sync_targets.is_empty());
        assert!(!s.awaiting_cloud());
        assert!(
            holds_nothing(&file.data),
            "the old store's data is not taken"
        );
        assert!(
            !s.snapshot().setup.pending,
            "a finished setup stays finished"
        );
    }
    assert_eq!(notices(&m), [("warning", NOTICE_B.to_owned())]);
    assert_eq!(
        fs::read(backups_dir(dir.path()).join(LEGACY_COPY)).unwrap(),
        legacy.bytes,
        "the old store is in Settings > Backups byte for byte"
    );
    let listed: Vec<String> = lock(&m)
        .list_backups()
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert_eq!(listed, [daily.to_owned(), LEGACY_COPY.to_owned()]);

    // Restoring the 1.3.1 backup brings its data.
    let restored: crate::model::LocalFile = serde_json::from_slice(&daily_bytes).unwrap();
    lock(&m).restore_backup(daily).unwrap();
    {
        let s = lock(&m);
        let data = &s.file().data;
        assert_eq!(data.projects, restored.data.projects);
        assert_eq!(data.tasks, restored.data.tasks);
        assert_eq!(data.segments, restored.data.segments);
        assert_eq!(data.settings, restored.data.settings);
        assert_eq!(s.stored().data, s.file().data, "and the store holds it");
    }
    let listed: Vec<String> = lock(&m)
        .list_backups()
        .unwrap()
        .into_iter()
        .filter(|b| !b.safety)
        .map(|b| b.name)
        .collect();
    assert_eq!(listed, [daily.to_owned(), LEGACY_COPY.to_owned()]);
    legacy.check();
}

#[test]
fn a_legacy_file_with_a_folder_brings_the_data_back_as_it_is() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("OneDrive");
    fs::create_dir_all(&root).unwrap();
    let clock = ManualClock::at(T0);
    let x = a_computer_syncs_and_is_gone(&root, &clock, false);

    let dir = tempfile::tempdir().unwrap();
    let legacy = write_legacy(dir.path(), MAIN_FILE, &legacy_bytes(&x.id, NAME, &[&root]));
    let m = open(dir.path(), &clock);
    {
        let s = lock(&m);
        assert!(s.awaiting_cloud());
        assert_eq!(s.file().device_id, x.id);
        assert_eq!(s.file().prefs.sync_targets, [target(&root)]);
        assert!(holds_nothing(&s.file().data));
        assert_eq!(s.file().last_heartbeat, None);
    }
    assert_eq!(notices(&m), [("info", NOTICE_C.to_owned())]);

    assert!(run_cycle_within(&m, &RealIo, LIMIT));
    let now = clock.now();
    let mut s = lock(&m);
    assert_same_records(&s.file().data, &x.data);
    assert_eq!(
        serde_json::to_string(&s.file().data).unwrap(),
        serde_json::to_string(&x.data).unwrap()
    );
    assert!(!s.awaiting_cloud());
    assert_eq!(s.file().last_heartbeat, x.beat, "taken from its own file");
    assert_eq!(s.stored(), *s.file(), "and saved");
    assert_eq!(
        s.target_status_mut(&root.to_string_lossy()).last_written_at,
        Some(now),
        "its own file was written again"
    );
    assert_eq!(fs::read(&x.own).unwrap(), x.bytes, "byte for byte");
    assert!(!s.snapshot().warnings.iter().any(|w| w == W1));
    drop(s);
    legacy.check();
}

#[test]
fn a_legacy_folder_that_cannot_be_reached_writes_nothing_and_keeps_waiting() {
    let base = tempfile::tempdir().unwrap();
    let elsewhere = base.path().join("Elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let clock = ManualClock::at(T0);
    let x = a_computer_syncs_and_is_gone(&elsewhere, &clock, false);
    // The folder this computer used isn't there: the drive isn't connected, or the client hasn't made it yet.
    let root = base.path().join("OneDrive");

    let dir = tempfile::tempdir().unwrap();
    let legacy = write_legacy(dir.path(), MAIN_FILE, &legacy_bytes(&x.id, NAME, &[&root]));
    let m = open(dir.path(), &clock);
    assert_eq!(notices(&m), [("info", NOTICE_C.to_owned())]);
    assert!(!run_cycle_within(&m, &RealIo, LIMIT));
    versions::run_due(&m, &RealIo, LIMIT);
    assert!(!root.exists(), "nothing is made where the folder should be");
    {
        let s = lock(&m);
        assert!(s.awaiting_cloud());
        assert!(holds_nothing(&s.file().data));
        let warnings = s.snapshot().warnings;
        assert!(warnings.iter().any(|w| w == W1), "{warnings:?}");
    }
    match versions::save_now(&m, &RealIo, LIMIT) {
        Err(CoreError::Conflict(why)) => assert_eq!(why, NO_VERSION_YET),
        other => panic!("{other:?}"),
    }
    assert!(!root.exists());

    // The folder arrives, with what the other computer left in it.
    fs::rename(&elsewhere, &root).unwrap();
    clock.advance(60);
    assert!(run_cycle_within(&m, &RealIo, LIMIT));
    let s = lock(&m);
    assert!(!s.awaiting_cloud());
    assert_same_records(&s.file().data, &x.data);
    let warnings = s.snapshot().warnings;
    assert!(!warnings.iter().any(|w| w == W1), "{warnings:?}");
    // Where the folder now is.
    let own = devices_dir(&root).join(format!("{}.json", x.id));
    let own: DeviceFile = serde_json::from_slice(&fs::read(own).unwrap()).unwrap();
    assert_eq!(
        own.written_at,
        clock.now(),
        "written once the data was here"
    );
    assert_eq!(own.data, x.data);
    drop(s);
    legacy.check();
}

#[test]
fn an_empty_reachable_folder_gets_no_write_until_data_arrives() {
    let base = tempfile::tempdir().unwrap();
    // Connected, but the cloud client hasn't brought its files down yet.
    let root = base.path().join("OneDrive");
    fs::create_dir_all(&root).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let dir = tempfile::tempdir().unwrap();
    let legacy = write_legacy(dir.path(), MAIN_FILE, &legacy_bytes(&id, NAME, &[&root]));
    let clock = ManualClock::at(T0);
    let m = open(dir.path(), &clock);
    assert!(lock(&m).awaiting_cloud());

    let nothing_in = |root: &Path| fs::read_dir(root).unwrap().next().is_none();
    for _ in 0..2 {
        run_cycle_within(&m, &RealIo, LIMIT);
        versions::run_due(&m, &RealIo, LIMIT);
        clock.advance(60);
    }
    assert!(nothing_in(&root), "no file of this device yet");
    // Work goes on meanwhile, which wants the lease: no claim, beat or release is written, nor a lease/ folder.
    let project = lock(&m).create_project(&pinput("While waiting")).unwrap();
    lease_io::check_now(&m, &RealIo, LIMIT);
    clock.advance(120);
    lease_io::run_tick(&m, &RealIo, LIMIT);
    lease_io::release(&m, &RealIo, LIMIT);
    assert!(nothing_in(&root), "nothing at all under the folder");
    assert!(!root.join("zWorkTracker/lease").exists());
    {
        let s = lock(&m);
        assert!(s.awaiting_cloud());
        let warnings = s.snapshot().warnings;
        assert!(warnings.iter().any(|w| w == W1), "{warnings:?}");
    }

    // The user takes the folder away and connects it again: theirs to decide, so the wait ends.
    let root_text = root.to_string_lossy().into_owned();
    lock(&m).remove_sync_target(&root_text).unwrap();
    assert!(!lock(&m).awaiting_cloud());
    lock(&m).switch_targets(None, target(&root), None).unwrap();
    assert!(!lock(&m).awaiting_cloud());
    run_cycle_within(&m, &RealIo, LIMIT);
    let own = devices_dir(&root).join(format!("{id}.json"));
    let written: DeviceFile = serde_json::from_slice(&fs::read(&own).unwrap()).unwrap();
    assert!(written.data.projects.contains_key(&project));
    let warnings = lock(&m).snapshot().warnings;
    assert!(!warnings.iter().any(|w| w == W1), "{warnings:?}");
    legacy.check();
}

#[test]
fn a_restore_in_the_folder_is_followed_by_the_empty_store() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("OneDrive");
    copy_tree(&fixture("cloud-1.3.1-epoch"), &root);
    let read = |id: &str| -> DeviceFile {
        let path = devices_dir(&root).join(format!("{id}.json"));
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    };
    let a = read(FIXTURE_ID);
    let b = read("bbbbbbbb-2222-4222-8222-222222222222");
    let info = a.epoch_info.clone().unwrap();
    assert_eq!(info.epoch, 1, "fixture: a restore was made in this folder");
    assert_eq!(b.epoch_info.as_ref(), Some(&info));

    let dir = tempfile::tempdir().unwrap();
    let legacy = write_legacy(
        dir.path(),
        MAIN_FILE,
        &legacy_bytes(FIXTURE_ID, NAME, &[&root]),
    );
    let clock = ManualClock::at("2026-10-08T10:20:00Z");
    let m = open(dir.path(), &clock);
    assert!(run_cycle_within(&m, &RealIo, LIMIT));
    let s = lock(&m);
    let prefs = &s.file().prefs;
    assert_eq!(prefs.epoch.as_ref(), Some(&info));
    assert_eq!(
        prefs.join_pending, None,
        "an empty store follows a restore by itself"
    );
    assert_eq!(
        prefs.sync_joined,
        Some(vec![root.to_string_lossy().into_owned()])
    );
    assert!(!s.awaiting_cloud());
    let mut expected = merge(&a.data, &b.data);
    expected.normalize();
    assert_same_records(&s.file().data, &expected);
    assert_eq!(s.file().last_heartbeat, a.last_heartbeat);
    drop(s);
    legacy.check();
}

#[test]
fn an_unreadable_legacy_file_starts_a_new_device_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let junk = truncated(&legacy_bytes(FIXTURE_ID, NAME, &[]));
    let legacy = write_legacy(dir.path(), MAIN_FILE, &junk);
    let why = serde_json::from_slice::<Value>(&junk)
        .unwrap_err()
        .to_string();
    assert!(why.starts_with("EOF while parsing"), "{why}");

    let clock = ManualClock::at(T0);
    let m = open(dir.path(), &clock);
    let path = dir.path().join(MAIN_FILE);
    assert_eq!(
        notices(&m),
        [(
            "warning",
            format!(
                "zWorkTracker now keeps its data in a database on this computer. The data file of the previous version couldn't be read ({why}), so this computer starts as a new device. That file was left as it was: {}.",
                path.display()
            )
        )]
    );
    {
        let s = lock(&m);
        let file = s.file();
        assert!(uuid::Uuid::parse_str(&file.device_id).is_ok());
        assert_ne!(file.device_id, FIXTURE_ID);
        assert_eq!(file.device_name, "this-host");
        assert!(s.snapshot().setup.pending, "a new device is set up again");
        assert!(holds_nothing(&file.data));
    }
    assert_eq!(
        fs::read(backups_dir(dir.path()).join(LEGACY_COPY)).unwrap(),
        junk,
        "kept for hand recovery all the same"
    );
    legacy.check();
}

#[cfg(unix)]
#[test]
fn a_held_legacy_file_stops_the_start() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(MAIN_FILE);
    let legacy = write_legacy(dir.path(), MAIN_FILE, &legacy_bytes(FIXTURE_ID, NAME, &[]));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&path).is_ok() {
        // Mode 000 doesn't stop the superuser, so there is nothing here to hold the file.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        eprintln!("skipped: running as a user whom mode 000 doesn't stop");
        return;
    }
    let clock = ManualClock::at(T0);
    let Err(err) = Service::open(dir.path(), Arc::new(clock.clone()), "this-host") else {
        panic!("a store that can't be read stops the start");
    };
    assert!(err.is_held(), "{err:?}");
    assert!(
        !dir.path().join(DB_FILE).exists(),
        "no database, so no new device"
    );
    assert!(!backups_dir(dir.path()).exists());

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let m = open(dir.path(), &clock);
    assert_eq!(lock(&m).file().device_id, FIXTURE_ID);
    assert_eq!(notices(&m), [("warning", NOTICE_B.to_owned())]);
    legacy.check();
}

#[test]
fn the_legacy_bak_is_used_when_the_main_file_is_damaged() {
    let dir = tempfile::tempdir().unwrap();
    let good = legacy_bytes(FIXTURE_ID, NAME, &[]);
    let main = write_legacy(dir.path(), MAIN_FILE, &truncated(&good));
    let bak = write_legacy(dir.path(), BAK, &good);
    let clock = ManualClock::at(T0);
    let m = open(dir.path(), &clock);
    {
        let s = lock(&m);
        assert_eq!(s.file().device_id, FIXTURE_ID);
        assert_eq!(s.file().device_name, NAME);
        assert!(s.file().prefs.setup_done);
    }
    assert_eq!(notices(&m), [("warning", NOTICE_B.to_owned())]);
    assert_eq!(
        fs::read(backups_dir(dir.path()).join(LEGACY_COPY)).unwrap(),
        good,
        "the copy is of the file the identity came from"
    );
    main.check();
    bak.check();
}

/// A1.4 P5.
#[test]
fn an_open_segment_of_this_device_in_the_folder_closes_at_its_heartbeat() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("OneDrive");
    fs::create_dir_all(&root).unwrap();
    let clock = ManualClock::at(T0);
    let x = a_computer_syncs_and_is_gone(&root, &clock, true);
    let beat = x.beat.unwrap();
    assert_eq!(beat, Ts::parse(T0).unwrap().later(600));

    let dir = tempfile::tempdir().unwrap();
    let legacy = write_legacy(dir.path(), MAIN_FILE, &legacy_bytes(&x.id, NAME, &[&root]));
    clock.advance(3600);
    let m = open(dir.path(), &clock);
    assert!(run_cycle_within(&m, &RealIo, LIMIT));
    let s = lock(&m);
    let data = &s.file().data;
    assert_eq!(
        data.segments[&x.segment].end,
        Some(beat),
        "the last moment its own file vouches for"
    );
    assert!(data.own_open_segments(&x.id).is_empty());
    let task = &data.tasks[&x.task];
    assert_eq!(task.state, TaskState::Paused);
    assert_eq!(task.pause_reason, Some(PauseReason::SyncReconciled));
    assert_eq!(s.file().last_heartbeat, Some(beat));
    let own: DeviceFile = serde_json::from_slice(&fs::read(&x.own).unwrap()).unwrap();
    assert_eq!(
        own.data.segments[&x.segment].end,
        Some(beat),
        "and so it is in the folder"
    );
    drop(s);
    legacy.check();
}

/// A1.4 P14: words typed in 1.3.1 just before it closed are for a task that only arrives with the data.
#[test]
fn drafts_wait_on_disk_for_the_data_and_are_put_back_when_it_arrives() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("OneDrive");
    fs::create_dir_all(&root).unwrap();
    let clock = ManualClock::at(T0);
    let x = a_computer_syncs_and_is_gone(&root, &clock, false);

    let dir = tempfile::tempdir().unwrap();
    let legacy = write_legacy(dir.path(), MAIN_FILE, &legacy_bytes(&x.id, NAME, &[&root]));
    let typed = doc("Typed just before the update");
    let draft = Draft {
        description: typed.clone(),
        at: clock.now(),
        base: doc("Details"),
    };
    let drafts = BTreeMap::from([(x.task.clone(), draft)]);
    drafts::write(dir.path(), &drafts).unwrap();

    let m = open(dir.path(), &clock);
    assert!(lock(&m).awaiting_cloud());
    assert_eq!(
        drafts::read(dir.path()).unwrap(),
        Some(drafts.clone()),
        "they wait on disk"
    );
    assert_eq!(notices(&m), [("info", NOTICE_C.to_owned())]);

    clock.advance(60);
    assert!(run_cycle_within(&m, &RealIo, LIMIT));
    let s = lock(&m);
    assert!(!s.awaiting_cloud());
    assert_eq!(s.file().data.tasks[&x.task].description, typed);
    assert_eq!(s.stored().data.tasks[&x.task].description, typed, "saved");
    assert_eq!(
        drafts::read(dir.path()).unwrap(),
        Some(BTreeMap::new()),
        "and done with"
    );
    let messages: Vec<String> = s
        .snapshot()
        .notices
        .into_iter()
        .map(|n| n.message)
        .collect();
    assert!(
        messages.iter().any(|n| n
            == "Recovered an edit to Design review that was typed just before the app closed."),
        "{messages:?}"
    );
    let own: DeviceFile = serde_json::from_slice(&fs::read(&x.own).unwrap()).unwrap();
    assert_eq!(
        own.data.tasks[&x.task].description, typed,
        "and in the folder"
    );
    drop(s);
    legacy.check();
}

/// Office PC in the C0 fixtures.
const OTHER_ID: &str = "bbbbbbbb-2222-4222-8222-222222222222";

fn edit_json(path: &Path, edit: impl FnOnce(&mut Value)) {
    let mut value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    edit(&mut value);
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn device_file(path: &Path) -> DeviceFile {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// `legacy_bytes`, changed by `edit`.
fn legacy_edited(id: &str, roots: &[&Path], edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut file: Value = serde_json::from_slice(&legacy_bytes(id, NAME, roots)).unwrap();
    edit(&mut file);
    serde_json::to_vec(&file).unwrap()
}

/// Review F0. Office PC restored a version in the folder (epoch 1), and this computer's own file there is from before
/// that: it never followed the restore, and holds a run timed since. 1.3.1 followed such a restore keeping what it had
/// timed since (`carry_forward`), with the rest in a safety copy and a version. So does the first start, where an empty
/// store would follow the restore by itself and drop its own file below it.
#[test]
fn an_own_file_below_a_restore_keeps_what_this_device_timed_since() {
    const RUN: &str = "000000ff-a000-4000-8000-000000000000";
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("OneDrive");
    copy_tree(&fixture("cloud-1.3.1-epoch"), &root);
    let own = devices_dir(&root).join(format!("{FIXTURE_ID}.json"));
    let other = devices_dir(&root).join(format!("{OTHER_ID}.json"));
    edit_json(&other, |v| v["epochInfo"]["by"] = json!(OTHER_ID));
    let run = json!({
        "id": RUN, "taskId": "0000000b-a000-4000-8000-000000000000", "deviceId": FIXTURE_ID,
        "start": "2026-10-08T10:06:00Z", "end": "2026-10-08T10:11:00Z", "manual": false,
        "subtaskId": "0000000d-a000-4000-8000-000000000000", "edit": null,
        "editStamp": {"at": "2026-10-08T10:06:00Z", "by": FIXTURE_ID}
    });
    edit_json(&own, |v| {
        v.as_object_mut().unwrap().remove("epochInfo");
        v["data"]["segments"][RUN] = run.clone();
    });
    let info = device_file(&other).epoch_info.unwrap();
    assert_eq!(
        info.at.iso(),
        "2026-10-08T10:05:00Z",
        "the run is from after it"
    );
    // 1.3.1's store held the run too, and had followed no restore.
    let dir = tempfile::tempdir().unwrap();
    let legacy = write_legacy(
        dir.path(),
        MAIN_FILE,
        &legacy_edited(FIXTURE_ID, &[&root], |f| f["data"]["segments"][RUN] = run),
    );

    let clock = ManualClock::at("2026-10-08T10:20:00Z");
    let m = open(dir.path(), &clock);
    assert!(run_cycle_within(&m, &RealIo, LIMIT));
    let s = lock(&m);
    let span = |data: &Shared| {
        data.segments
            .get(RUN)
            .map(|seg| (seg.start.iso(), seg.end.map(|e| e.iso())))
    };
    let timed = Some((
        "2026-10-08T10:06:00Z".to_owned(),
        Some("2026-10-08T10:11:00Z".to_owned()),
    ));
    assert_eq!(span(&s.file().data), timed, "what was timed since is kept");
    assert_eq!(span(&s.stored().data), timed, "and saved");
    let prefs = &s.file().prefs;
    assert_eq!(prefs.epoch.as_ref(), Some(&info), "the restore is followed");
    assert_eq!(prefs.join_pending, None, "as 1.3.1 did, without a question");
    assert!(!s.awaiting_cloud());
    let written = device_file(&own);
    assert_eq!(written.epoch_info.as_ref(), Some(&info));
    assert_eq!(span(&written.data), timed, "and so it is in the folder");
    let messages: Vec<String> = s
        .snapshot()
        .notices
        .into_iter()
        .map(|n| n.message)
        .collect();
    assert!(
        messages
            .iter()
            .any(|n| n.starts_with("Office PC restored the version from")),
        "{messages:?}"
    );
    assert!(
        s.list_backups().unwrap().iter().any(|b| b.safety),
        "what was here from before the restore is in a safety copy"
    );
    drop(s);
    legacy.check();
}

/// Review F2. The legacy copy restored after a start with no folder brings back this device's billing prefix, and what
/// it knew of the other device's: the store started with none, and a prefix chosen afresh would bill under another.
#[test]
fn restoring_the_legacy_copy_keeps_this_devices_billing_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = write_legacy(
        dir.path(),
        MAIN_FILE,
        &legacy_edited(FIXTURE_ID, &[], |f| {
            f["data"]["prefixes"][FIXTURE_ID]["prefix"] = json!("ACME");
            for summary in f["data"]["summaries"].as_object_mut().unwrap().values_mut() {
                summary["prefix"] = json!("ACME");
                summary["reference"] = json!("ACME-001");
            }
            f["prefs"]["referenceMarks"] = json!({"ACME": 1});
        }),
    );
    let clock = ManualClock::at(T0);
    let m = open(dir.path(), &clock);
    assert!(
        lock(&m).file().data.prefixes.is_empty(),
        "the store starts with none"
    );
    lock(&m).restore_backup(LEGACY_COPY).unwrap();
    let s = lock(&m);
    let data = &s.file().data;
    assert_eq!(data.prefix_of(FIXTURE_ID), "ACME");
    assert_eq!(
        data.prefixes[OTHER_ID].prefix, "B",
        "the other device's comes back too"
    );
    assert!(data.prefix_clashes(FIXTURE_ID).is_empty());
    assert_eq!(s.stored().data.prefixes, data.prefixes, "and is saved");
    drop(s);
    legacy.check();
}

/// Review F5. 1.3.1 kept its latest heartbeat in a file beside its store, and read the two together. A run it was timing
/// when it was stopped ends there when its copy is restored, as 1.3.1 would have ended it, not at the store's last save.
/// The first start takes that heartbeat with the identity. The file is only read.
#[test]
fn restoring_the_legacy_copy_ends_a_run_at_the_heartbeat_1_3_1_kept_beside_it() {
    const RUN: &str = "00000024-a000-4000-8000-000000000000";
    const TASK: &str = "0000000b-a000-4000-8000-000000000000";
    let dir = tempfile::tempdir().unwrap();
    let legacy = write_legacy(
        dir.path(),
        MAIN_FILE,
        &legacy_edited(FIXTURE_ID, &[], |f| {
            // A run from 09:45 still open when 1.3.1 was stopped; the store was last saved at 09:50.
            f["lastHeartbeat"] = json!("2026-10-08T09:50:00Z");
            f["data"]["segments"][RUN]["end"] = Value::Null;
            f["data"]["tasks"][TASK]["state"] = json!("running");
            f["data"]["tasks"][TASK]["stateStamp"] =
                json!({"at": "2026-10-08T09:45:00Z", "by": FIXTURE_ID});
        }),
    );
    let beat_bytes =
        serde_json::to_vec(&json!({"deviceId": FIXTURE_ID, "at": "2026-10-08T17:00:00Z"})).unwrap();
    let beat = write_legacy(dir.path(), "z-work-heartbeat.json", &beat_bytes);
    let last_beat = Ts::parse("2026-10-08T17:00:00Z").unwrap();
    let clock = ManualClock::at("2026-10-08T17:30:00Z");
    let m = open(dir.path(), &clock);
    assert_eq!(lock(&m).file().last_heartbeat, Some(last_beat));
    lock(&m).restore_backup(LEGACY_COPY).unwrap();
    let s = lock(&m);
    let data = &s.file().data;
    assert_eq!(data.segments[RUN].end, Some(last_beat));
    assert_eq!(data.tasks[TASK].state, TaskState::Paused);
    assert_eq!(s.stored().data.segments[RUN].end, Some(last_beat), "saved");
    drop(s);
    legacy.check();
    beat.check();
}

/// Review F7. A version isn't restored while this computer's data hasn't arrived: its own file in the folders, all there
/// is of its data, would fall below the restore, with no copy of today's data saved first. Nothing changes, and it says
/// why rather than that no copy could be saved.
#[test]
fn a_version_is_not_restored_before_the_data_has_arrived() {
    const NOT_YET: &str = "This computer's data hasn't arrived from your sync folder yet, so no version can be restored yet. Nothing was changed. Check that the folder is synced to this computer, then press Sync now in Settings > Sync between devices.";
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("OneDrive");
    copy_tree(&fixture("cloud-1.3.1-epoch"), &root);
    // The cloud client has brought the versions down, and not the devices' files yet.
    fs::remove_dir_all(devices_dir(&root)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let legacy = write_legacy(
        dir.path(),
        MAIN_FILE,
        &legacy_bytes(FIXTURE_ID, NAME, &[&root]),
    );
    let clock = ManualClock::at("2026-10-08T10:20:00Z");
    let m = open(dir.path(), &clock);
    run_cycle_within(&m, &RealIo, LIMIT);
    assert!(lock(&m).awaiting_cloud());
    let files = |root: &Path| {
        let mut all = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(at) = stack.pop() {
            for entry in fs::read_dir(at).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    all.push((path.clone(), fs::read(&path).unwrap()));
                }
            }
        }
        all.sort();
        all
    };
    let before = files(&root);
    let root_text = root.to_string_lossy().into_owned();
    match versions::restore(
        &m,
        &RealIo,
        &root_text,
        "20261008T094000Z--aaaaaaaa--manual",
        LIMIT,
    ) {
        Err(CoreError::Conflict(why)) => assert_eq!(why, NOT_YET),
        other => panic!("{other:?}"),
    }
    let s = lock(&m);
    assert!(s.awaiting_cloud());
    assert!(holds_nothing(&s.file().data));
    assert_eq!(s.file().prefs.epoch, None);
    drop(s);
    assert_eq!(files(&root), before, "nothing written to the folder");
    legacy.check();
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), &dest).unwrap();
        }
    }
}
