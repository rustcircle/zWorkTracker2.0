//! A fake cloud folder with delays, for scenarios with more than one device.
//!
//! The cloud keeps every version of every file, with who wrote it and when, on the cloud's own clock. A device sees its
//! own writes at once and another's once that writer's delay to it has passed, unless visibility from that writer is
//! held, in which case it goes on seeing what it saw when the hold began. The cloud itself can make conflict copies,
//! delete a file or put an old copy back, as cloud clients do, and everyone sees that at once. Each device has a clock
//! of its own that moves with the cloud's, so a skew is one `advance` of that clock alone.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::lease::{LeaseRecord, LeaseStateName};
use crate::lease_io::{self, TickOutcome, lease_dir};
use crate::machine::{ProjectInput, TaskInput};
use crate::model::{SyncProvider, SyncTarget};
use crate::service::Service;
use crate::sync::{SyncIo, devices_dir, run_cycle_within};
use crate::time::{ManualClock, Ts};

pub(crate) const T0: &str = "2026-03-02T09:00:00Z";
/// For `bounded`. The fake answers at once, so this is only reached by a write held at a gate.
pub(crate) const LIMIT: Duration = Duration::from_secs(5);
/// Who the cloud's own changes are written by. Everyone sees them at once.
pub(crate) const CLOUD: &str = "cloud";

fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "not found")
}

#[derive(Clone)]
struct Version {
    /// `None` for a deletion.
    bytes: Option<Arc<Vec<u8>>>,
    writer: String,
    at: i64,
    /// The order of writing, as two writes can share a second.
    seq: u64,
}

#[derive(Default)]
struct Store {
    files: BTreeMap<PathBuf, Vec<Version>>,
    dirs: BTreeSet<PathBuf>,
    /// The next version's `seq`.
    next: u64,
}

pub(crate) struct FakeCloud {
    store: Mutex<Store>,
    clock: ManualClock,
    roots: Vec<PathBuf>,
    /// Seconds before a file written by the first device shows on the second.
    delays: Mutex<BTreeMap<(String, String), i64>>,
    /// From the first device to the second, nothing written from this moment on shows, nor anything that hadn't arrived
    /// by then: the time, and the `seq` of the first version written after it.
    held: Mutex<BTreeMap<(String, String), (i64, u64)>>,
    /// Every device's clock, so that time passes for everyone at once.
    clocks: Mutex<Vec<ManualClock>>,
}

impl FakeCloud {
    pub(crate) fn new(roots: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            store: Mutex::default(),
            clock: ManualClock::at(T0),
            roots: roots.iter().map(PathBuf::from).collect(),
            delays: Mutex::default(),
            held: Mutex::default(),
            clocks: Mutex::default(),
        })
    }

    pub(crate) fn now(&self) -> Ts {
        use crate::time::Clock;
        self.clock.now()
    }

    /// Time passes, here and on every device.
    pub(crate) fn advance(&self, secs: i64) {
        self.clock.advance(secs);
        for c in guard(&self.clocks).iter() {
            c.advance(secs);
        }
    }

    /// A clock for a new device, at the cloud's time and moving with it.
    fn device_clock(&self) -> ManualClock {
        let clock = ManualClock::at(&self.now().iso());
        guard(&self.clocks).push(clock.clone());
        clock
    }

    pub(crate) fn view(self: &Arc<Self>, me: &str) -> CloudView {
        CloudView {
            cloud: Arc::clone(self),
            me: me.to_owned(),
            flags: Arc::default(),
        }
    }

    /// What `from` writes shows on `to` this many seconds later.
    pub(crate) fn delay(&self, from: &str, to: &str, secs: i64) {
        guard(&self.delays).insert((from.to_owned(), to.to_owned()), secs);
    }

    /// `to` goes on seeing what it sees of `from` now, until `release`.
    pub(crate) fn hold(&self, from: &str, to: &str) {
        let mark = (self.now().secs(), guard(&self.store).next);
        guard(&self.held).insert((from.to_owned(), to.to_owned()), mark);
    }

    pub(crate) fn release(&self, from: &str, to: &str) {
        guard(&self.held).remove(&(from.to_owned(), to.to_owned()));
    }

    pub(crate) fn hold_both(&self, a: &str, b: &str) {
        self.hold(a, b);
        self.hold(b, a);
    }

    pub(crate) fn release_both(&self, a: &str, b: &str) {
        self.release(a, b);
        self.release(b, a);
    }

    fn root_of(&self, path: &Path) -> Option<&PathBuf> {
        self.roots.iter().find(|r| path.starts_with(r))
    }

    fn record(&self, path: &Path, bytes: Option<Vec<u8>>, writer: &str) {
        let at = self.now().secs();
        let mut store = guard(&self.store);
        let seq = store.next;
        store.next += 1;
        store
            .files
            .entry(path.to_path_buf())
            .or_default()
            .push(Version {
                bytes: bytes.map(Arc::new),
                writer: writer.to_owned(),
                at,
                seq,
            });
    }

    /// The newest version `reader` can see.
    fn seen_by<'a>(&self, versions: &'a [Version], reader: &str) -> Option<&'a Version> {
        let now = self.now().secs();
        let delays = guard(&self.delays);
        let held = guard(&self.held);
        versions.iter().rev().find(|v| {
            if v.writer == reader || v.writer == CLOUD {
                return true;
            }
            let key = (v.writer.clone(), reader.to_owned());
            let shown = v.at + delays.get(&key).copied().unwrap_or(0);
            match held.get(&key) {
                Some(&(at, first_after)) => v.seq < first_after && shown <= at,
                None => shown <= now,
            }
        })
    }

    /// The newest bytes at `path`, whoever can see them.
    pub(crate) fn latest(&self, path: &Path) -> Option<Vec<u8>> {
        let store = guard(&self.store);
        let v = store.files.get(path)?.last()?;
        v.bytes.as_ref().map(|b| b.to_vec())
    }

    pub(crate) fn exists(&self, path: &Path) -> bool {
        self.latest(path).is_some()
    }

    /// Who wrote each version at `path`, oldest first, with what it held.
    pub(crate) fn history(&self, path: &Path) -> Vec<(String, Option<Vec<u8>>)> {
        guard(&self.store)
            .files
            .get(path)
            .map(|vs| {
                vs.iter()
                    .map(|v| (v.writer.clone(), v.bytes.as_ref().map(|b| b.to_vec())))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A file the cloud writes itself, such as another app's or a forged one. Everyone sees it at once.
    pub(crate) fn put(&self, path: &Path, bytes: &[u8]) {
        let mut store = guard(&self.store);
        let mut dir = path.to_path_buf();
        while dir.pop() && self.root_of(&dir).is_some_and(|r| dir != *r) {
            store.dirs.insert(dir.clone());
        }
        drop(store);
        self.record(path, Some(bytes.to_vec()), CLOUD);
    }

    /// A conflict copy of the newest version, as a cloud client makes one: `name.json` becomes `name<suffix>.json`.
    pub(crate) fn conflict_copy(&self, path: &Path, suffix: &str) -> PathBuf {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let copy = path.with_file_name(format!("{stem}{suffix}{ext}"));
        let bytes = self.latest(path).unwrap_or_default();
        self.record(&copy, Some(bytes), CLOUD);
        copy
    }

    /// The cloud client deletes the file everywhere.
    pub(crate) fn delete(&self, path: &Path) {
        self.record(path, None, CLOUD);
    }

    /// The cloud client puts back the version `back` versions before the newest.
    pub(crate) fn restore_old(&self, path: &Path, back: usize) {
        let old = {
            let store = guard(&self.store);
            let versions = store.files.get(path).map(Vec::as_slice).unwrap_or_default();
            versions
                .len()
                .checked_sub(back + 1)
                .and_then(|i| versions.get(i))
                .and_then(|v| v.bytes.as_ref().map(|b| b.to_vec()))
        };
        self.record(path, old, CLOUD);
    }

    /// Only half of the newest version arrived.
    pub(crate) fn truncate(&self, path: &Path) {
        let bytes = self.latest(path).unwrap_or_default();
        self.record(path, Some(bytes[..bytes.len() / 2].to_vec()), CLOUD);
    }

    /// The newest lease record of a device in a folder, as a reader would take it.
    pub(crate) fn lease_record(&self, root: &str, device: &str) -> Option<LeaseRecord> {
        let name = format!("{device}.json");
        let bytes = self.latest(&lease_dir(Path::new(root)).join(&name))?;
        crate::lease::parse_record(&name, &bytes)
    }

    /// Every lease record a device has written to a folder, oldest first.
    pub(crate) fn lease_history(&self, root: &str, device: &str) -> Vec<LeaseRecord> {
        let name = format!("{device}.json");
        self.history(&lease_dir(Path::new(root)).join(&name))
            .into_iter()
            .filter(|(writer, _)| writer == device)
            .filter_map(|(_, bytes)| crate::lease::parse_record(&name, &bytes?))
            .collect()
    }

    pub(crate) fn lease_path(root: &str, device: &str) -> PathBuf {
        lease_dir(Path::new(root)).join(format!("{device}.json"))
    }

    pub(crate) fn device_file_path(root: &str, device: &str) -> PathBuf {
        devices_dir(Path::new(root)).join(format!("{device}.json"))
    }
}

/// A write held until the test lets it go, to catch something in flight.
pub(crate) struct WriteGate {
    path: PathBuf,
    /// Entered, and opened.
    state: Mutex<(bool, bool)>,
    cv: Condvar,
}

impl WriteGate {
    /// Waits until the write has reached the gate.
    pub(crate) fn wait_entered(&self) -> bool {
        let state = guard(&self.state);
        let (state, _) = self
            .cv
            .wait_timeout_while(state, Duration::from_secs(5), |s| !s.0)
            .unwrap_or_else(PoisonError::into_inner);
        state.0
    }

    pub(crate) fn open(&self) {
        guard(&self.state).1 = true;
        self.cv.notify_all();
    }

    fn pass(&self) {
        let mut state = guard(&self.state);
        state.0 = true;
        self.cv.notify_all();
        let _ = self
            .cv
            .wait_timeout_while(state, Duration::from_secs(10), |s| !s.1)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

#[derive(Default)]
struct Flags {
    offline: Mutex<BTreeSet<PathBuf>>,
    full: Mutex<BTreeSet<PathBuf>>,
    gate: Mutex<Option<Arc<WriteGate>>>,
}

/// One device's view of the cloud: what its desktop client has synced so far.
#[derive(Clone)]
pub(crate) struct CloudView {
    cloud: Arc<FakeCloud>,
    me: String,
    flags: Arc<Flags>,
}

impl CloudView {
    /// Every folder can't be reached from this device, or can again.
    pub(crate) fn set_offline(&self, offline: bool) {
        let mut set = guard(&self.flags.offline);
        set.clear();
        if offline {
            set.extend(self.cloud.roots.iter().cloned());
        }
    }

    pub(crate) fn set_offline_root(&self, root: &str, offline: bool) {
        let mut set = guard(&self.flags.offline);
        if offline {
            set.insert(PathBuf::from(root));
        } else {
            set.remove(Path::new(root));
        }
    }

    /// Writes to this folder fail as on a full disk. Reads still work.
    pub(crate) fn set_full(&self, root: &str, full: bool) {
        let mut set = guard(&self.flags.full);
        if full {
            set.insert(PathBuf::from(root));
        } else {
            set.remove(Path::new(root));
        }
    }

    /// Holds the next write to `path` at a gate until it is opened.
    pub(crate) fn gate_next_write(&self, path: &Path) -> Arc<WriteGate> {
        let gate = Arc::new(WriteGate {
            path: path.to_path_buf(),
            state: Mutex::new((false, false)),
            cv: Condvar::new(),
        });
        *guard(&self.flags.gate) = Some(Arc::clone(&gate));
        gate
    }

    fn reachable_root(&self, path: &Path) -> io::Result<PathBuf> {
        let root = self.cloud.root_of(path).ok_or_else(not_found)?;
        if guard(&self.flags.offline).contains(root) {
            return Err(not_found());
        }
        Ok(root.clone())
    }

    fn bytes(&self, path: &Path) -> io::Result<Arc<Vec<u8>>> {
        self.reachable_root(path)?;
        let store = guard(&self.cloud.store);
        let versions = store.files.get(path).ok_or_else(not_found)?;
        self.cloud
            .seen_by(versions, &self.me)
            .and_then(|v| v.bytes.clone())
            .ok_or_else(not_found)
    }
}

impl SyncIo for CloudView {
    fn is_dir(&self, path: &Path) -> bool {
        let Ok(root) = self.reachable_root(path) else {
            return false;
        };
        path == root || guard(&self.cloud.store).dirs.contains(path)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        let root = self.reachable_root(path)?;
        let mut store = guard(&self.cloud.store);
        let mut dir = path.to_path_buf();
        while dir != root && dir.starts_with(&root) {
            store.dirs.insert(dir.clone());
            if !dir.pop() {
                break;
            }
        }
        Ok(())
    }

    fn list(&self, path: &Path) -> io::Result<Vec<String>> {
        if !self.is_dir(path) {
            return Err(not_found());
        }
        let name = |p: &Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let store = guard(&self.cloud.store);
        let mut names = BTreeSet::new();
        for (p, versions) in &store.files {
            let shown = self
                .cloud
                .seen_by(versions, &self.me)
                .is_some_and(|v| v.bytes.is_some());
            if p.parent() == Some(path) && shown {
                names.insert(name(p));
            }
        }
        for d in &store.dirs {
            if d.parent() == Some(path) {
                names.insert(name(d));
            }
        }
        Ok(names.into_iter().collect())
    }

    fn file_size(&self, path: &Path) -> io::Result<u64> {
        self.bytes(path).map(|b| b.len() as u64)
    }

    fn read(&self, path: &Path, max: u64) -> io::Result<Vec<u8>> {
        let bytes = self.bytes(path)?;
        if bytes.len() as u64 > max {
            return Err(crate::sync::too_large());
        }
        Ok(bytes.to_vec())
    }

    fn write_atomic(&self, dest: &Path, _tmp: &Path, bytes: &[u8]) -> io::Result<()> {
        let root = self.reachable_root(dest)?;
        if guard(&self.flags.full).contains(&root) {
            return Err(io::Error::other("There is not enough space on the disk."));
        }
        if !dest.parent().is_some_and(|p| self.is_dir(p)) {
            return Err(not_found());
        }
        let gate = {
            let mut slot = guard(&self.flags.gate);
            match slot.as_ref() {
                Some(g) if g.path == dest => slot.take(),
                _ => None,
            }
        };
        if let Some(gate) = gate {
            gate.pass();
        }
        self.cloud.record(dest, Some(bytes.to_vec()), &self.me);
        Ok(())
    }

    fn sleep(&self, _d: Duration) {}

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.bytes(path)?;
        self.cloud.record(path, None, &self.me);
        Ok(())
    }
}

pub(crate) fn pinput(name: &str) -> ProjectInput {
    ProjectInput {
        name: name.into(),
        color: "#336699".into(),
        billable: true,
        budget_hours: None,
        client_zone: None,
        billing_type: Default::default(),
        fixed_fee_cents: None,
        fee_currency: None,
    }
}

pub(crate) fn doc(text: &str) -> serde_json::Value {
    serde_json::json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":text}]}]})
}

pub(crate) fn tinput(name: &str, project: &str) -> TaskInput {
    TaskInput {
        name: name.into(),
        project_id: project.into(),
        description: doc("Details"),
        continued_from: None,
        tags: vec!["tag-concept".to_owned()],
    }
}

/// A computer running the app, with its own data folder, clock and view of the cloud.
pub(crate) struct Device {
    pub(crate) svc: Mutex<Service>,
    pub(crate) view: CloudView,
    pub(crate) clock: ManualClock,
    dir: tempfile::TempDir,
    name: String,
}

impl Device {
    /// Opens the app on a new computer with these sync folders.
    pub(crate) fn open(cloud: &Arc<FakeCloud>, name: &str, roots: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let clock = cloud.device_clock();
        let svc = Service::open(dir.path(), Arc::new(clock.clone()), name).unwrap();
        let view = cloud.view(&svc.file().device_id);
        let device = Self {
            svc: Mutex::new(svc),
            view,
            clock,
            dir,
            name: name.to_owned(),
        };
        for root in roots {
            device
                .s()
                .add_sync_target(SyncTarget {
                    provider: SyncProvider::Custom,
                    root: (*root).to_owned(),
                })
                .unwrap();
        }
        device
    }

    pub(crate) fn s(&self) -> MutexGuard<'_, Service> {
        guard(&self.svc)
    }

    pub(crate) fn id(&self) -> String {
        self.s().file().device_id.clone()
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// This device's one running segment.
    pub(crate) fn running(&self) -> String {
        let s = self.s();
        let open = s.file().data.own_open_segments(&s.file().device_id);
        assert_eq!(open.len(), 1, "one timer running here");
        open[0].clone()
    }

    pub(crate) fn notices(&self) -> Vec<String> {
        self.s()
            .snapshot()
            .notices
            .into_iter()
            .map(|n| n.message)
            .collect()
    }

    pub(crate) fn state(&self) -> LeaseStateName {
        self.s().lease.state
    }

    /// A pass now, as "Check again" does.
    pub(crate) fn tick(&self) -> TickOutcome {
        lease_io::check_now(&self.svc, &self.view, LIMIT)
    }

    /// A pass if one is due, as the lease thread does every 15 s.
    pub(crate) fn tick_if_due(&self) -> TickOutcome {
        lease_io::run_tick(&self.svc, &self.view, LIMIT)
    }

    /// A full sync of the data.
    pub(crate) fn cycle(&self) -> bool {
        run_cycle_within(&self.svc, &self.view, LIMIT)
    }

    /// A project and a task in it.
    pub(crate) fn task(&self, project: &str, task: &str) -> (String, String) {
        let mut s = self.s();
        let p = s.create_project(&pinput(project)).unwrap();
        let t = s.create_task(&tinput(task, &p)).unwrap();
        (p, t)
    }

    /// The app is killed and opened again, with no chance to close anything.
    pub(crate) fn relaunch(self) -> Self {
        let Self {
            svc,
            view,
            clock,
            dir,
            name,
        } = self;
        drop(svc);
        let svc = Service::open(dir.path(), Arc::new(clock.clone()), &name).unwrap();
        Self {
            svc: Mutex::new(svc),
            view,
            clock,
            dir,
            name,
        }
    }

    /// Quits cleanly: timers close, then the lease is released.
    pub(crate) fn quit(&self, limit: Duration) {
        self.s().on_exit().unwrap();
        lease_io::release(&self.svc, &self.view, limit);
    }
}

/// What a crash would leave of a data folder: its files as they are on disk now, the database and its WAL with them,
/// copied while the `Service` that owns them is still alive. Reopening the copy is the next launch after a kill.
pub(crate) fn crash_image(dir: &Path) -> tempfile::TempDir {
    let image = tempfile::tempdir().unwrap();
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), image.path().join(entry.file_name())).unwrap();
        }
    }
    image
}

/// Both devices read and write each other's files once, in turn.
pub(crate) fn sync_all(devices: &[&Device]) {
    for _ in 0..2 {
        for d in devices {
            d.cycle();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/cloud/OneDrive";

    fn file(cloud: &FakeCloud) -> PathBuf {
        let path = PathBuf::from(ROOT).join("zWorkTracker/x.json");
        cloud.put(&path, b"one");
        path
    }

    #[test]
    fn a_write_shows_on_another_device_only_after_its_delay_and_on_its_own_at_once() {
        let cloud = FakeCloud::new(&[ROOT]);
        let (a, b) = (cloud.view("a"), cloud.view("b"));
        let path = file(&cloud);
        cloud.delay("a", "b", 120);
        a.write_atomic(&path, Path::new("/tmp"), b"two").unwrap();
        assert_eq!(a.read(&path, u64::MAX).unwrap(), b"two");
        assert_eq!(
            b.read(&path, u64::MAX).unwrap(),
            b"one",
            "still the version before"
        );
        cloud.advance(119);
        assert_eq!(b.read(&path, u64::MAX).unwrap(), b"one");
        cloud.advance(1);
        assert_eq!(b.read(&path, u64::MAX).unwrap(), b"two");
        assert_eq!(b.file_size(&path).unwrap(), 3);
        let new = PathBuf::from(ROOT).join("zWorkTracker/y.json");
        a.write_atomic(&new, Path::new("/tmp"), b"y").unwrap();
        assert_eq!(
            b.list(&PathBuf::from(ROOT).join("zWorkTracker")).unwrap(),
            ["x.json"]
        );
        cloud.advance(120);
        assert_eq!(
            b.list(&PathBuf::from(ROOT).join("zWorkTracker")).unwrap(),
            ["x.json", "y.json"]
        );
    }

    #[test]
    fn a_hold_keeps_what_another_device_saw_until_it_is_released() {
        let cloud = FakeCloud::new(&[ROOT]);
        let (a, b) = (cloud.view("a"), cloud.view("b"));
        let path = file(&cloud);
        a.write_atomic(&path, Path::new("/tmp"), b"two").unwrap();
        cloud.hold("a", "b");
        a.write_atomic(&path, Path::new("/tmp"), b"three").unwrap();
        cloud.advance(3600);
        assert_eq!(b.read(&path, u64::MAX).unwrap(), b"two");
        cloud.release("a", "b");
        assert_eq!(b.read(&path, u64::MAX).unwrap(), b"three");
    }

    #[test]
    fn the_clouds_own_changes_show_everywhere_at_once() {
        let cloud = FakeCloud::new(&[ROOT]);
        let (a, b) = (cloud.view("a"), cloud.view("b"));
        cloud.delay("a", "b", 600);
        let path = file(&cloud);
        a.write_atomic(&path, Path::new("/tmp"), b"two").unwrap();
        let copy = cloud.conflict_copy(&path, " (1)");
        assert_eq!(copy.file_name().unwrap(), "x (1).json");
        assert_eq!(b.read(&copy, u64::MAX).unwrap(), b"two");
        cloud.restore_old(&path, 1);
        assert_eq!(a.read(&path, u64::MAX).unwrap(), b"one");
        assert_eq!(b.read(&path, u64::MAX).unwrap(), b"one");
        cloud.truncate(&path);
        assert_eq!(b.read(&path, u64::MAX).unwrap(), b"o");
        cloud.delete(&path);
        assert!(a.read(&path, u64::MAX).is_err() && b.read(&path, u64::MAX).is_err());
        assert!(cloud.exists(&copy), "a conflict copy stays");
        let names = b.list(&PathBuf::from(ROOT).join("zWorkTracker")).unwrap();
        assert_eq!(names, ["x (1).json"]);
    }

    #[test]
    fn a_folder_offline_or_full_fails_like_a_real_one_and_removing_a_file_is_seen_late() {
        let cloud = FakeCloud::new(&[ROOT, "/cloud/Drive"]);
        let (a, b) = (cloud.view("a"), cloud.view("b"));
        let path = file(&cloud);
        a.set_offline_root(ROOT, true);
        assert!(!a.is_dir(Path::new(ROOT)));
        assert!(
            a.is_dir(Path::new("/cloud/Drive")),
            "the other folder is fine"
        );
        assert!(a.read(&path, u64::MAX).is_err());
        assert!(a.write_atomic(&path, Path::new("/tmp"), b"x").is_err());
        a.set_offline(false);
        a.set_full(ROOT, true);
        assert_eq!(
            a.read(&path, u64::MAX).unwrap(),
            b"one",
            "reading still works"
        );
        let full = a.write_atomic(&path, Path::new("/tmp"), b"x").unwrap_err();
        assert!(full.to_string().contains("not enough space"));
        a.set_full(ROOT, false);
        // No folder to write into.
        let nowhere = PathBuf::from(ROOT).join("missing/x.json");
        assert!(a.write_atomic(&nowhere, Path::new("/tmp"), b"x").is_err());
        a.create_dir_all(nowhere.parent().unwrap()).unwrap();
        a.write_atomic(&nowhere, Path::new("/tmp"), b"x").unwrap();
        // Removing is a write like any other.
        cloud.delay("a", "b", 60);
        a.remove_file(&path).unwrap();
        assert!(a.read(&path, u64::MAX).is_err());
        assert_eq!(b.read(&path, u64::MAX).unwrap(), b"one");
        cloud.advance(60);
        assert!(b.read(&path, u64::MAX).is_err());
        assert!(a.remove_file(&path).is_err(), "nothing left to remove");
        assert_eq!(
            cloud
                .history(&path)
                .iter()
                .map(|(w, b)| (w.as_str(), b.is_some()))
                .collect::<Vec<_>>(),
            [("cloud", true), ("a", false)]
        );
    }
}
