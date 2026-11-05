//! Folder sync. Each device writes only `<root>/zWorkTracker/devices/<device-uuid>.json` to every
//! enabled folder and reads the other devices' files from all of them. A desktop client (OneDrive,
//! Google Drive) does the upload; this code never touches the network. Nothing here can block time
//! logging or damage the local store: every failure only ends up in the status, of the folder it is about.
//! A file from before a restored version this device follows never reaches the merge: see `epoch`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::epoch::{at_key, fence, only_at_key};
use crate::merge::merge;
use crate::model::{DeviceFile, PendingCopy, RemoteDevice, SCHEMA_VERSION, Shared, SyncTarget};
use crate::persist;
use crate::service::Service;
use crate::time::Ts;
use crate::validate::{
    BadRecord, OwnFileCheck, RecordKind, keep_valid_records, only_dated_ahead, prepare_own_file,
};

pub const APP_FOLDER: &str = "zWorkTracker";
pub const DEVICES_FOLDER: &str = "devices";
pub const MAX_REMOTE_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_DEVICE_FILES: usize = 64;
pub const TARGET_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_BASE_MS: u64 = 100;
const RETRY_ATTEMPTS: u32 = 5;

/// Filesystem access, injected so locks and offline placeholders can be simulated in tests.
pub trait SyncIo: Send + Sync {
    fn is_dir(&self, path: &Path) -> bool;
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn list(&self, path: &Path) -> io::Result<Vec<String>>;
    fn file_size(&self, path: &Path) -> io::Result<u64>;
    /// Reads a whole file of at most `max` bytes. A longer one is `too_large`, whatever its size said a moment before,
    /// so a file that grows while it is read costs no more memory than that.
    fn read(&self, path: &Path, max: u64) -> io::Result<Vec<u8>>;
    fn write_atomic(&self, dest: &Path, tmp: &Path, bytes: &[u8]) -> io::Result<()>;
    fn sleep(&self, d: Duration);
    /// Removes one file. Only thinning out saved versions does this, so where it isn't provided they stay.
    fn remove_file(&self, _path: &Path) -> io::Result<()> {
        Err(ErrorKind::Unsupported.into())
    }
}

/// What `SyncIo::read` says of a file longer than it was allowed to read.
#[must_use]
pub fn too_large() -> io::Error {
    io::Error::new(ErrorKind::FileTooLarge, "the file is unreasonably large")
}

/// `SyncIo::read` with retries, refusing a file past `max` bytes even from a reader that returned it whole.
pub(crate) fn read_at_most(io_impl: &dyn SyncIo, path: &Path, max: u64) -> io::Result<Vec<u8>> {
    let bytes = with_retry(io_impl, || io_impl.read(path, max))?;
    if bytes.len() as u64 > max {
        return Err(too_large());
    }
    Ok(bytes)
}

#[derive(Clone, Copy)]
pub struct RealIo;

impl<T: SyncIo + ?Sized> SyncIo for Arc<T> {
    fn is_dir(&self, path: &Path) -> bool {
        (**self).is_dir(path)
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        (**self).create_dir_all(path)
    }
    fn list(&self, path: &Path) -> io::Result<Vec<String>> {
        (**self).list(path)
    }
    fn file_size(&self, path: &Path) -> io::Result<u64> {
        (**self).file_size(path)
    }
    fn read(&self, path: &Path, max: u64) -> io::Result<Vec<u8>> {
        (**self).read(path, max)
    }
    fn write_atomic(&self, dest: &Path, tmp: &Path, bytes: &[u8]) -> io::Result<()> {
        (**self).write_atomic(dest, tmp, bytes)
    }
    fn sleep(&self, d: Duration) {
        (**self).sleep(d);
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        (**self).remove_file(path)
    }
}

impl SyncIo for RealIo {
    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path)
    }
    fn list(&self, path: &Path) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(path)? {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    }
    fn file_size(&self, path: &Path) -> io::Result<u64> {
        Ok(std::fs::metadata(path)?.len())
    }
    fn read(&self, path: &Path, max: u64) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        // One byte past the limit is enough to know the file is past it.
        std::fs::File::open(path)?
            .take(max.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max {
            return Err(too_large());
        }
        Ok(bytes)
    }
    fn write_atomic(&self, dest: &Path, tmp: &Path, bytes: &[u8]) -> io::Result<()> {
        persist::write_atomic(dest, tmp, bytes)
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }
}

/// Worth retrying: sharing violations, lock contention and cloud placeholders that are not ready.
#[must_use]
pub fn is_transient(e: &io::Error) -> bool {
    if matches!(
        e.kind(),
        ErrorKind::PermissionDenied
            | ErrorKind::WouldBlock
            | ErrorKind::Interrupted
            | ErrorKind::TimedOut
    ) {
        return true;
    }
    cfg!(windows) && e.raw_os_error().is_some_and(is_windows_transient_code)
}

/// Access denied, sharing violation, lock violation and the cloud-files provider errors.
#[must_use]
pub fn is_windows_transient_code(code: i32) -> bool {
    matches!(code, 5 | 32 | 33 | 362 | 379 | 381 | 388 | 390)
}

fn with_retry<T>(io_impl: &dyn SyncIo, mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut attempt = 0;
    loop {
        match op() {
            Err(e) if is_transient(&e) && attempt + 1 < RETRY_ATTEMPTS => {
                io_impl.sleep(Duration::from_millis(RETRY_BASE_MS << attempt));
                attempt += 1;
            }
            other => return other,
        }
    }
}

#[must_use]
pub fn devices_dir(root: &Path) -> PathBuf {
    root.join(APP_FOLDER).join(DEVICES_FOLDER)
}

/// Exactly `<lowercase hyphenated uuid>.json`. Conflict copies such as `<uuid> (1).json` or
/// `<uuid>-PC2.json`, and anything else in the folder, are ignored and never deleted.
#[must_use]
pub fn device_id_from_name(name: &str) -> Option<&str> {
    let id = name.strip_suffix(".json")?;
    let parsed = uuid::Uuid::parse_str(id).ok()?;
    (parsed.hyphenated().to_string() == id).then_some(id)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionProbe {
    schema_version: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetStatus {
    pub last_written_at: Option<Ts>,
    pub last_read_at: Option<Ts>,
    pub last_error: Option<String>,
    pub last_error_at: Option<Ts>,
    pub warnings: Vec<String>,
    pub ignored_files: usize,
    pub devices_seen: usize,
    /// When this device's lease record was last written in this folder.
    pub last_lease_at: Option<Ts>,
    /// Why the lease record couldn't be read or written here.
    pub lease_error: Option<String>,
    /// When a saved version was last written in this folder.
    pub last_version_at: Option<Ts>,
    pub last_version_error: Option<String>,
    /// The size of the saved versions in this folder.
    pub versions_bytes: Option<u64>,
    /// How far this folder's copy of another device's file is behind another folder's, when that is worth saying.
    pub lag_secs: Option<i64>,
}

#[derive(Debug, Default)]
pub struct TargetRead {
    pub files: Vec<DeviceFile>,
    /// This device's own file from this folder. A restored store may be older than it.
    pub own: Option<DeviceFile>,
    /// This device's own file was written by a newer app version, so it must not be overwritten.
    pub own_newer: bool,
    /// This device's own file exists but could not be read, so its contents are unknown. Writing
    /// would destroy a file from a newer version, or the only copy of a restored store's data.
    pub own_unread: bool,
    pub warnings: Vec<String>,
    pub ignored: usize,
    pub error: Option<String>,
}

const FOLDER_NOT_FOUND: &str =
    "Folder not found. Is the drive connected and the desktop client running?";
const DAMAGED: &str = "the file is incomplete or damaged. Keeping the last good data.";
const WRITTEN_AHEAD: &str = "it is dated ahead of this computer's clock. Keeping the last good data. Check the clocks on both devices. If they are right, the file is damaged.";

/// How many records a message names before it just counts the rest.
const RECORDS_NAMED: usize = 3;

fn name_records(records: &[&BadRecord], name: impl Fn(&BadRecord) -> String) -> String {
    let mut named: Vec<String> = records
        .iter()
        .take(RECORDS_NAMED)
        .map(|r| name(r))
        .collect();
    if records.len() > RECORDS_NAMED {
        named.push(format!("and {} more", records.len() - RECORDS_NAMED));
    }
    named.join(", ")
}

fn name_and_why(r: &BadRecord) -> String {
    format!("{} ({})", r.name, r.why)
}

/// What a reader says about the records it left out of the file `name`. One only dated ahead isn't damaged,
/// and can be read when that date comes.
#[must_use]
pub fn skipped_warning(name: &str, records: &[BadRecord]) -> String {
    let (ahead, damaged): (Vec<&BadRecord>, Vec<&BadRecord>) =
        records.iter().partition(|r| r.ahead);
    let mut told = Vec::new();
    match damaged.as_slice() {
        [] => {}
        [one] => told.push(format!(
            "Skipped a damaged record in {name}: {}.",
            name_and_why(one)
        )),
        all => told.push(format!(
            "Skipped {} damaged records in {name}: {}.",
            all.len(),
            name_records(all, name_and_why)
        )),
    }
    match ahead.as_slice() {
        [] => {}
        [one] => told.push(format!(
            "Skipped a record in {name} dated ahead of this computer's clock: {}. It can be read when that date comes, so check the clocks on both devices.",
            one.name
        )),
        all => told.push(format!(
            "Skipped {} records in {name} dated ahead of this computer's clock: {}. They can be read when those dates come, so check the clocks on both devices.",
            all.len(),
            name_records(all, |r| r.name.clone())
        )),
    }
    told.push("The rest of the file was read.".to_owned());
    told.join(" ")
}

/// What a person can do here about a record no device takes. Correcting an entry keeps the date it was made
/// with, a new price leaves the old one where it is, and some records are nobody's to change.
fn what_to_do(kind: RecordKind) -> Option<&'static str> {
    match kind {
        RecordKind::Segment => Some("Void it here and enter it again."),
        RecordKind::Rate => Some("Remove it here."),
        RecordKind::Project
        | RecordKind::Task
        | RecordKind::Shift
        | RecordKind::Rounding
        | RecordKind::Pause
        | RecordKind::Tag
        | RecordKind::Template
        | RecordKind::Summary => Some("Correct or remove it here."),
        RecordKind::Setting
        | RecordKind::Gap
        | RecordKind::PauseRequest
        | RecordKind::Prefix
        | RecordKind::Audit
        | RecordKind::Heartbeat => None,
    }
}

/// What this device says about records of its own that other devices can't take. One no device will ever
/// take and that nothing counts is nobody's loss, so that says nothing. One only dated ahead is different,
/// even if deleted: the others go on counting the copy they have until that date.
fn own_file_warnings(check: &OwnFileCheck) -> Vec<String> {
    let left_out: Vec<&BadRecord> = check.left_out.iter().filter(|r| !r.gone).collect();
    let ahead: Vec<&BadRecord> = check.ahead.iter().collect();
    let mut out = Vec::new();
    match left_out.as_slice() {
        [] => {}
        [one] => {
            let told = format!(
                "Other devices can't use {}. It stays on this device, and everything else still syncs.",
                name_and_why(one)
            );
            out.push(match what_to_do(one.kind) {
                Some(asked) => format!("{told} {asked}"),
                None => told,
            });
        }
        all => {
            let told = format!(
                "Other devices can't use {} records: {}. They stay on this device, and everything else still syncs.",
                all.len(),
                name_records(all, name_and_why)
            );
            out.push(if all.iter().any(|r| what_to_do(r.kind).is_some()) {
                format!("{told} Correct, void or remove what you can here.")
            } else {
                told
            });
        }
    }
    match ahead.as_slice() {
        [] => {}
        [one] => out.push(format!(
            "Other devices may skip {} until its date, as that is ahead of this computer's clock. Check that the clock is right.",
            one.name
        )),
        all => out.push(format!(
            "Other devices may skip {} records until their dates, as those are ahead of this computer's clock: {}. Check that the clock is right.",
            all.len(),
            name_records(all, |r| r.name.clone())
        )),
    }
    out
}

/// Reads every device's file. Damaged, newer, oversized or unreadable files are skipped with a warning,
/// and so is any record that can't be trusted in a file that otherwise can.
pub fn read_target(io_impl: &dyn SyncIo, root: &Path, own_id: &str, now: Ts) -> TargetRead {
    let mut out = TargetRead::default();
    let dir = devices_dir(root);
    if !io_impl.is_dir(root) {
        out.error = Some(FOLDER_NOT_FOUND.to_owned());
        return out;
    }
    if !io_impl.is_dir(&dir) {
        return out;
    }
    let mut names = match with_retry(io_impl, || io_impl.list(&dir)) {
        Ok(n) => n,
        Err(e) => {
            out.error = Some(format!("Could not list the folder: {e}"));
            return out;
        }
    };
    names.sort();
    let mut seen_files = 0usize;
    let mut total_bytes = 0u64;
    for name in names {
        let Some(id) = device_id_from_name(&name) else {
            out.ignored += 1;
            continue;
        };
        let is_own = id == own_id;
        if !is_own {
            seen_files += 1;
        }
        if !is_own && seen_files > MAX_DEVICE_FILES {
            out.warnings.push(format!(
                "Skipped {name}: the folder holds too many device files."
            ));
            continue;
        }
        let path = dir.join(&name);
        // Fail closed: a size that cannot be read is treated like a file that cannot be read.
        let size = match io_impl.file_size(&path) {
            Ok(size) => size,
            Err(e) => {
                out.own_unread |= is_own;
                out.warnings
                    .push(format!("Could not read {name} right now, will retry: {e}"));
                continue;
            }
        };
        let over = size > MAX_REMOTE_FILE_BYTES
            || (!is_own && total_bytes.saturating_add(size) > MAX_TOTAL_BYTES);
        if !is_own && !over {
            total_bytes += size;
        }
        if over {
            out.own_unread |= is_own;
            out.warnings
                .push(format!("Skipped {name}: the file is unreasonably large."));
            continue;
        }
        // A failed read is not corruption: a cloud placeholder may simply not be available offline.
        let bytes = match read_at_most(io_impl, &path, MAX_REMOTE_FILE_BYTES) {
            Ok(b) => b,
            // It grew past the limit after its size was taken.
            Err(e) if e.kind() == ErrorKind::FileTooLarge => {
                out.own_unread |= is_own;
                out.warnings
                    .push(format!("Skipped {name}: the file is unreasonably large."));
                continue;
            }
            Err(e) => {
                out.own_unread |= is_own;
                out.warnings
                    .push(format!("Could not read {name} right now, will retry: {e}"));
                continue;
            }
        };
        let version = serde_json::from_slice::<VersionProbe>(&bytes)
            .ok()
            .and_then(|p| p.schema_version);
        match version {
            Some(v) if v > SCHEMA_VERSION => {
                if is_own {
                    out.own_newer = true;
                } else {
                    out.warnings.push(format!("Skipped {name}: it was written by a newer version of the app. Update this device to read it."));
                }
                continue;
            }
            Some(v) if v > 0 => {}
            _ => {
                if !is_own {
                    out.warnings.push(format!("Skipped {name}: {DAMAGED}"));
                }
                continue;
            }
        }
        let parsed = serde_json::from_slice::<DeviceFile>(&bytes)
            .ok()
            .filter(|file| file.device_id == id);
        let dated_ahead = parsed
            .as_ref()
            .is_some_and(|file| only_dated_ahead(file, now));
        let usable = parsed.and_then(|mut file| {
            let left_out = keep_valid_records(&mut file, now).ok()?;
            Some((file, left_out))
        });
        match usable {
            // This device says what it holds that others can't use when it writes, in `run_cycle`.
            Some((file, _)) if is_own => out.own = Some(file),
            Some((file, left_out)) => {
                if !left_out.is_empty() {
                    out.warnings.push(skipped_warning(&name, &left_out));
                }
                out.files.push(file);
            }
            None if is_own => {}
            None if dated_ahead => out
                .warnings
                .push(format!("Skipped {name}: {WRITTEN_AHEAD}")),
            None => out.warnings.push(format!("Skipped {name}: {DAMAGED}")),
        }
    }
    out
}

/// Writes this device's own file.
pub fn write_target(
    io_impl: &dyn SyncIo,
    root: &Path,
    device_id: &str,
    bytes: &[u8],
) -> Result<(), String> {
    if !io_impl.is_dir(root) {
        return Err(FOLDER_NOT_FOUND.to_owned());
    }
    let dir = devices_dir(root);
    with_retry(io_impl, || io_impl.create_dir_all(&dir))
        .map_err(|e| format!("Could not create the folder: {e}"))?;
    let dest = dir.join(format!("{device_id}.json"));
    // A dot-prefixed temporary never matches a device file name, so readers ignore it.
    let tmp = dir.join(format!(".{device_id}.tmp"));
    with_retry(io_impl, || io_impl.write_atomic(&dest, &tmp, bytes))
        .map_err(|e| format!("Could not write the file: {e}"))
}

/// What a folder holds, for the "connect and merge" preview.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderPreview {
    pub has_data: bool,
    pub devices: Vec<String>,
    pub projects: usize,
    pub tasks: usize,
    pub segments: usize,
    pub warnings: Vec<String>,
}

/// The segments a person would count. Those of a task deleted with its time stay in the file, unseen.
fn segments_shown(data: &Shared) -> usize {
    data.segments
        .values()
        .filter(|s| {
            data.tasks
                .get(&s.task_id)
                .is_none_or(|t| t.discarded.is_none())
        })
        .count()
}

#[must_use]
pub fn preview_folder(io_impl: &dyn SyncIo, root: &Path, own_id: &str, now: Ts) -> FolderPreview {
    let read = read_target(io_impl, root, own_id, now);
    let mut merged = Shared::default();
    let mut devices = Vec::new();
    for f in &read.files {
        merged = merge(&merged, &f.data);
        devices.push(f.device_name.clone());
    }
    let mut warnings = read.warnings;
    warnings.extend(read.error);
    FolderPreview {
        has_data: !read.files.is_empty(),
        devices,
        projects: merged.projects.values().filter(|p| !p.deleted).count(),
        tasks: merged.tasks.values().filter(|t| !t.deleted).count(),
        segments: segments_shown(&merged),
        warnings,
    }
}

const MAJOR_DEBOUNCE_MS: u64 = 2_000;
const EDIT_DEBOUNCE_MS: u64 = 15_000;
const PERIOD_MS: u64 = 30 * 60 * 1000;
const CYCLE_RETRY_BASE_MS: u64 = 30_000;
const CYCLE_RETRY_MAX_MS: u64 = 15 * 60 * 1000;

/// When the next sync runs. Pure, with times as milliseconds since the app started.
///
/// One opt-in, then automatic: at launch, shortly after each major action, after text edits settle,
/// every 30 minutes, and on quit. A failed attempt retries with backoff rather than waiting for the
/// 30-minute timer. Debounces only coalesce: a burst never postpones a sync already scheduled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncSchedule {
    due: Option<u64>,
    next_periodic: u64,
    retry_at: Option<u64>,
    failures: u32,
}

/// The wait before retry number `failures`: 30 s, 1 min, 2 min and so on, up to 15 min.
#[must_use]
pub fn retry_delay_ms(failures: u32) -> u64 {
    let shift = failures.saturating_sub(1).min(16);
    CYCLE_RETRY_BASE_MS
        .saturating_mul(1 << shift)
        .min(CYCLE_RETRY_MAX_MS)
}

impl SyncSchedule {
    /// Due at once: the app syncs at launch.
    #[must_use]
    pub fn new(now: u64) -> Self {
        Self {
            due: Some(now),
            next_periodic: now + PERIOD_MS,
            retry_at: None,
            failures: 0,
        }
    }

    fn earliest(&mut self, at: u64) {
        self.due = Some(self.due.map_or(at, |d| d.min(at)));
    }

    pub fn on_major_action(&mut self, now: u64) {
        self.earliest(now + MAJOR_DEBOUNCE_MS);
    }

    pub fn on_text_edit(&mut self, now: u64) {
        self.earliest(now + EDIT_DEBOUNCE_MS);
    }

    pub fn on_now(&mut self, now: u64) {
        self.earliest(now);
    }

    #[must_use]
    pub fn next_wake(&self) -> u64 {
        [self.due, self.retry_at]
            .into_iter()
            .flatten()
            .fold(self.next_periodic, u64::min)
    }

    #[must_use]
    pub fn is_due(&self, now: u64) -> bool {
        self.next_wake() <= now
    }

    pub fn cycle_done(&mut self, now: u64, failed: bool) {
        self.due = None;
        self.next_periodic = now + PERIOD_MS;
        if failed {
            self.failures = self.failures.saturating_add(1);
            self.retry_at = Some(now + retry_delay_ms(self.failures));
        } else {
            self.failures = 0;
            self.retry_at = None;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub projects: usize,
    pub tasks: usize,
    pub segments: usize,
}

pub(crate) fn counts(data: &Shared) -> Counts {
    Counts {
        projects: data.projects.values().filter(|p| !p.deleted).count(),
        tasks: data.tasks.values().filter(|t| !t.deleted).count(),
        segments: segments_shown(data),
    }
}

/// FNV-1a over the canonical JSON of the merged data. It detects a damaged copy; it isn't security.
pub(crate) fn checksum(data: &Shared) -> String {
    let bytes = serde_json::to_vec(data).unwrap_or_default();
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{hash:016x}")
}

/// What a move would do, for the confirmation screen.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovePreview {
    pub source_available: bool,
    pub source_devices: Vec<String>,
    pub source_counts: Counts,
    pub destination_has_data: bool,
    pub destination_devices: Vec<String>,
    pub destination_counts: Counts,
    pub local_counts: Counts,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveResult {
    pub files_copied: usize,
    pub files_already_there: usize,
    pub files_ignored: usize,
    /// The previous folder couldn't be read, so this went ahead from local data.
    pub source_missing: bool,
    pub counts: Counts,
    pub checksum: String,
    pub warnings: Vec<String>,
}

fn merged_of(read: &TargetRead) -> Shared {
    let mut out = read
        .own
        .as_ref()
        .map_or_else(Shared::default, |f| f.data.clone());
    for f in &read.files {
        out = merge(&out, &f.data);
    }
    out
}

fn names_of(read: &TargetRead) -> Vec<String> {
    read.files.iter().map(|f| f.device_name.clone()).collect()
}

#[must_use]
pub fn preview_move(
    io_impl: &dyn SyncIo,
    from: Option<&Path>,
    to: &Path,
    own_id: &str,
    now: Ts,
    local: &Shared,
) -> MovePreview {
    let mut out = MovePreview {
        local_counts: counts(local),
        ..MovePreview::default()
    };
    if let Some(from) = from {
        let read = read_target(io_impl, from, own_id, now);
        out.source_available = read.error.is_none();
        out.source_devices = names_of(&read);
        out.source_counts = counts(&merged_of(&read));
        out.warnings.extend(read.warnings);
        out.warnings.extend(read.error);
    }
    let dest = read_target(io_impl, to, own_id, now);
    out.destination_has_data = dest.own.is_some() || !dest.files.is_empty();
    out.destination_devices = names_of(&dest);
    out.destination_counts = counts(&merged_of(&dest));
    out.warnings.extend(dest.warnings);
    out.warnings.extend(dest.error);
    out
}

/// Copies data into a new folder and only then switches to it. The previous folder is only read, and
/// the lock is only held for the merges, so time logging carries on. If anything fails, the settings
/// stay as they were and the error says why; running it again is safe.
///
/// `replacing` is the folder to stop using, which may differ from `from` on a retry.
pub fn move_target<I: SyncIo>(
    service: &Mutex<Service>,
    io_impl: &I,
    from: Option<&str>,
    target: &SyncTarget,
    replacing: Option<&str>,
    progress: &mut dyn FnMut(&str, usize, usize),
) -> Result<MoveResult, String> {
    let lock = || service.lock().unwrap_or_else(PoisonError::into_inner);
    let (own_id, now) = {
        let s = lock();
        s.folder_gate(&target.root).map_err(|e| e.to_string())?;
        (s.file().device_id.clone(), s.now())
    };
    let to = Path::new(&target.root);
    if !io_impl.is_dir(to) {
        return Err(FOLDER_NOT_FOUND.to_owned());
    }
    progress("Reading the folders", 0, 1);
    let source = from.map(|f| read_target(io_impl, Path::new(f), &own_id, now));
    let mut dest = read_target(io_impl, to, &own_id, now);
    if let Some(e) = dest.error.take() {
        return Err(e);
    }
    if dest.own_newer {
        return Err("The new folder holds this device's file from a newer app version. Update this app first.".to_owned());
    }
    if dest.own_unread {
        return Err(
            "This device's file in the new folder could not be read right now. Try again later."
                .to_owned(),
        );
    }
    let mut source_ok = source.filter(|r| r.error.is_none());

    // Everything readable joins local data first, by the normal merge rules. A move never replaces that data by
    // itself, so a restore this device doesn't follow yet stops it here, and one from before what it follows stays out.
    {
        let mut s = lock();
        let mut reads = vec![(target.root.clone(), &dest)];
        if let (Some(f), Some(read)) = (from, &source_ok) {
            reads.push((f.to_owned(), read));
        }
        s.restores_before_move(&reads)?;
        let local = s.file().prefs.epoch.clone();
        fence(&mut dest, local.as_ref());
        if let Some(read) = &mut source_ok {
            fence(read, local.as_ref());
        }
        let mut incoming = Shared::default();
        let mut seen: BTreeMap<String, RemoteDevice> = BTreeMap::new();
        let mut own_files = Vec::new();
        for read in source_ok.iter().chain(std::iter::once(&dest)) {
            own_files.extend(read.own.clone());
            for f in &read.files {
                incoming = merge(&incoming, &f.data);
                let entry = RemoteDevice {
                    name: f.device_name.clone(),
                    last_heartbeat: f.last_heartbeat,
                    written_at: f.written_at,
                };
                match seen.get(&f.device_id) {
                    Some(old) if old.written_at >= entry.written_at => {}
                    _ => {
                        seen.insert(f.device_id.clone(), entry);
                    }
                }
            }
        }
        s.absorb_remote(&incoming, seen, own_files);
        if let Some(e) = s.local_error() {
            return Err(e.to_owned());
        }
    }

    let mut result = MoveResult::default();
    result.warnings.extend(dest.warnings.iter().cloned());
    result.files_ignored = dest.ignored;
    if from.is_some() && source_ok.is_none() {
        result.source_missing = true;
        result.warnings.push(
            "The previous folder could not be read, so this used the data on this computer. You can retry the copy later.".to_owned(),
        );
    }
    if let Some(read) = &source_ok {
        result.warnings.extend(read.warnings.iter().cloned());
        result.files_ignored += read.ignored;
    }

    // The other devices' files go across as they are, unless the new folder has a fresher copy.
    let dir = devices_dir(to);
    with_retry(io_impl, || io_impl.create_dir_all(&dir))
        .map_err(|e| format!("Could not create the folder: {e}"))?;
    let mut expected: BTreeMap<String, DeviceFile> = dest
        .files
        .iter()
        .map(|f| (f.device_id.clone(), f.clone()))
        .collect();
    let others: &[DeviceFile] = source_ok.as_ref().map_or(&[], |r| r.files.as_slice());
    let total = others.len() + 1;
    for (i, file) in others.iter().enumerate() {
        progress("Copying device files", i, total);
        let fresher = expected
            .get(&file.device_id)
            .is_some_and(|d| d.written_at >= file.written_at);
        if fresher {
            result.files_already_there += 1;
            continue;
        }
        let name = format!("{}.json", file.device_id);
        let src_path = devices_dir(Path::new(from.unwrap_or_default())).join(&name);
        let bytes = read_at_most(io_impl, &src_path, MAX_REMOTE_FILE_BYTES)
            .map_err(|e| format!("Could not read {name}: {e}"))?;
        let tmp = dir.join(format!(".{}.move.tmp", file.device_id));
        with_retry(io_impl, || {
            io_impl.write_atomic(&dir.join(&name), &tmp, &bytes)
        })
        .map_err(|e| format!("Could not write {name}: {e}"))?;
        expected.insert(file.device_id.clone(), file.clone());
        result.files_copied += 1;
    }

    // This device's file last, holding the merged data.
    progress("Writing this device's data", total - 1, total);
    let mut own_file = lock().device_file();
    result
        .warnings
        .extend(own_file_warnings(&prepare_own_file(&mut own_file, now)));
    let bytes = serde_json::to_vec(&own_file).map_err(|e| e.to_string())?;
    let own_name = format!("{own_id}.json");
    let tmp = dir.join(format!(".{own_id}.move.tmp"));
    with_retry(io_impl, || {
        io_impl.write_atomic(&dir.join(&own_name), &tmp, &bytes)
    })
    .map_err(|e| format!("Could not write this device's file: {e}"))?;

    // Read back, and compare with what should be there, before calling it done.
    progress("Checking the copy", total - 1, total);
    let mut check = read_target(io_impl, to, &own_id, now);
    if let Some(e) = check.error.take() {
        return Err(format!("The copy could not be checked: {e}"));
    }
    // What a restore keeps out of the merge stays out of this too, even if a sync has followed one since the start.
    let local = lock().file().prefs.epoch.clone();
    only_at_key(&mut check, local.as_ref());
    if check.own.is_none() {
        return Err(
            "The copy could not be checked: this device's file did not read back.".to_owned(),
        );
    }
    // The same reader goes over this device's file as went over the copy.
    let mut own_as_read = own_file;
    let _ = keep_valid_records(&mut own_as_read, now);
    let mut want = own_as_read.data;
    for f in expected.values().filter(|f| at_key(f, local.as_ref())) {
        want = merge(&want, &f.data);
    }
    let got = merged_of(&check);
    if counts(&got) != counts(&want) || checksum(&got) != checksum(&want) {
        return Err(
            "The copy did not match when read back, so the folder was not switched.".to_owned(),
        );
    }
    result.counts = counts(&got);
    result.checksum = checksum(&got);

    let pending = (result.source_missing)
        .then(|| {
            from.map(|f| PendingCopy {
                from: f.to_owned(),
                to: target.root.clone(),
            })
        })
        .flatten();
    lock()
        .switch_targets(replacing, target.clone(), pending)
        .map_err(|e| e.to_string())?;
    progress("Done", total, total);
    Ok(result)
}

/// Roots whose worker is still stuck, so a hung folder never collects more threads.
pub type HungTargets = Arc<Mutex<BTreeSet<String>>>;

/// Below this, a folder a little behind another is only a cloud app doing its job.
const LAG_WORTH_TELLING_SECS: i64 = 3600;

/// How far each folder's copies of other devices' files are behind the freshest copy read, for the folders well behind.
/// A merge makes a stale copy harmless, so this is only ever something to say about the folder.
fn lags(reads: &[(SyncTarget, TargetRead)]) -> BTreeMap<String, i64> {
    let mut freshest: BTreeMap<&str, Ts> = BTreeMap::new();
    for f in reads.iter().flat_map(|(_, read)| &read.files) {
        freshest
            .entry(&f.device_id)
            .and_modify(|at| *at = (*at).max(f.written_at))
            .or_insert(f.written_at);
    }
    reads
        .iter()
        .filter_map(|(target, read)| {
            let behind = read
                .files
                .iter()
                .filter_map(|f| {
                    let newest = freshest.get(f.device_id.as_str())?;
                    Some(newest.secs() - f.written_at.secs())
                })
                .max()?;
            (behind > LAG_WORTH_TELLING_SECS).then(|| (target.root.clone(), behind))
        })
        .collect()
}

/// Runs folder work on its own thread with a time limit, so a hung network folder cannot hold up
/// other folders. A worker that overran stays registered until it returns.
pub fn bounded<T: Send + 'static>(
    hung: &HungTargets,
    root: &str,
    limit: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    const NOT_RESPONDING: &str = "The folder is not responding. Will try again later.";
    let lock = || hung.lock().unwrap_or_else(PoisonError::into_inner);
    if !lock().insert(root.to_owned()) {
        return Err(NOT_RESPONDING.to_owned());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let registry = Arc::clone(hung);
    let key = root.to_owned();
    std::thread::spawn(move || {
        let out = work();
        registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key);
        let _ = tx.send(out);
    });
    rx.recv_timeout(limit)
        .map_err(|_| NOT_RESPONDING.to_owned())
}

/// One full sync over every enabled folder. The lock is never held during file I/O, and a failing
/// or hung target never affects another. Returns true when the merge changed local data.
pub fn run_cycle<I: SyncIo + Clone + 'static>(service: &Mutex<Service>, io_impl: &I) -> bool {
    run_cycle_within(service, io_impl, TARGET_TIMEOUT)
}

pub fn run_cycle_within<I: SyncIo + Clone + 'static>(
    service: &Mutex<Service>,
    io_impl: &I,
    limit: Duration,
) -> bool {
    let lock = || service.lock().unwrap_or_else(PoisonError::into_inner);
    let (targets, own_id, now, hung): (Vec<SyncTarget>, String, Ts, HungTargets) = {
        let s = lock();
        (
            s.file().prefs.sync_targets.clone(),
            s.file().device_id.clone(),
            s.now(),
            s.hung_targets(),
        )
    };
    if targets.is_empty() {
        return false;
    }

    let mut reads: Vec<(SyncTarget, TargetRead)> = Vec::with_capacity(targets.len());
    for t in targets {
        let (io, root, own) = (io_impl.clone(), t.root.clone(), own_id.clone());
        let read = bounded(&hung, &t.root, limit, move || {
            read_target(&io, Path::new(&root), &own, now)
        })
        .unwrap_or_else(|e| TargetRead {
            error: Some(e),
            ..TargetRead::default()
        });
        reads.push((t, read));
    }
    let lag = lags(&reads);

    let changed = {
        let mut s = lock();
        let was_awaiting = s.awaiting_cloud();
        // A first start's own files are its data, before any restore is planned against it.
        let taken = s.take_own_files(&reads);
        // In the lock, so a restore that landed during the reads counts.
        let followed = s.follow_restores(&mut reads);
        // What a first start waits for: data from a folder, read whole and taken at this device's key. Counted after
        // the restores are followed, as a folder left alone for one gives nothing.
        let arrived = reads.iter().any(|(_, r)| {
            r.error.is_none() && !r.own_unread && (r.own.is_some() || !r.files.is_empty())
        });
        let own_beat = reads
            .iter()
            .filter_map(|(_, r)| r.own.as_ref().and_then(|f| f.last_heartbeat))
            .max();
        let mut incoming = Shared::default();
        let mut seen: BTreeMap<String, RemoteDevice> = BTreeMap::new();
        let mut own_files = Vec::new();
        for (_, read) in &mut reads {
            own_files.extend(read.own.take());
            for f in &read.files {
                incoming = merge(&incoming, &f.data);
                let entry = RemoteDevice {
                    name: f.device_name.clone(),
                    last_heartbeat: f.last_heartbeat,
                    written_at: f.written_at,
                };
                // The same device can be stale in one folder and fresh in another: keep the freshest.
                match seen.get(&f.device_id) {
                    Some(old) if old.written_at >= entry.written_at => {}
                    _ => {
                        seen.insert(f.device_id.clone(), entry);
                    }
                }
            }
        }
        let absorbed = s.absorb_remote(&incoming, seen, own_files);
        // Only once what arrived is saved here: a merge that couldn't be saved would leave nothing to write but an
        // empty store.
        let merged = arrived && s.local_error().is_none();
        let finished = s.finish_awaiting(merged, own_beat);
        // A restore followed while waiting ended the wait with its data (`install_restored`): the drafts that waited for
        // the data go back now, as `finish_awaiting` puts them back.
        if was_awaiting && !finished && !s.awaiting_cloud() {
            s.put_back_drafts();
        }
        taken || followed || absorbed || finished
    };

    // Checked as a reader will check it before anything is written, and the status says what that found.
    let (mut file, now) = {
        let s = lock();
        (s.device_file(), s.now())
    };
    let checked = prepare_own_file(&mut file, now);
    let own_warnings = own_file_warnings(&checked);
    let own_refused = checked
        .refused
        .map(|why| format!("Other devices will ignore this device's file: {why}."));
    let bytes = match serde_json::to_vec(&file) {
        Ok(b) => Arc::new(b),
        Err(e) => {
            lock().record_sync_failure(&format!("Could not prepare the sync file: {e}"));
            return changed;
        }
    };

    // While a first start waits for its data, nothing is written: an empty folder that isn't synced yet must not get an
    // empty file of this device. A skipped write is not a folder failure, so the last write and error stand. A folder
    // that couldn't be read still says so.
    let awaiting = lock().awaiting_cloud();
    for (target, read) in reads {
        let written = if let Some(e) = read.error.clone() {
            Some(Err(e))
        } else if awaiting {
            None
        } else if read.own_unread {
            Some(Err("This device's file could not be read right now, so it was not overwritten. Will retry.".to_owned()))
        } else if read.own_newer {
            Some(Err(
                "This device's file was written by a newer app version; it was not overwritten."
                    .to_owned(),
            ))
        } else {
            let (io, root, id, data) = (
                io_impl.clone(),
                target.root.clone(),
                own_id.clone(),
                Arc::clone(&bytes),
            );
            Some(
                bounded(&hung, &target.root, limit, move || {
                    write_target(&io, Path::new(&root), &id, &data)
                })
                .and_then(|r| r),
            )
        };
        let mut s = lock();
        let now = s.now();
        let status = s.target_status_mut(&target.root);
        // A folder that could not be read says nothing new: the last known figures stand.
        if read.error.is_none() {
            status.warnings = read.warnings;
            status.warnings.extend(own_warnings.iter().cloned());
            status.ignored_files = read.ignored;
            status.devices_seen = read.files.len();
            status.last_read_at = Some(now);
            status.lag_secs = lag.get(&target.root).copied();
        }
        let Some(written) = written else {
            continue;
        };
        match written {
            Ok(()) => {
                status.last_written_at = Some(now);
                status.last_error_at = own_refused.as_ref().map(|_| now);
                status.last_error.clone_from(&own_refused);
            }
            Err(e) => {
                status.last_error = Some(e);
                status.last_error_at = Some(now);
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::{ProjectInput, TaskInput};
    use crate::model::{SyncProvider, TaskState};
    use crate::time::{Clock, ManualClock};
    use crate::validate::check_device_file;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    const T0: &str = "2026-03-01T09:00:00Z";

    struct Device {
        svc: Mutex<Service>,
        _dir: tempfile::TempDir,
    }

    fn device(clock: &ManualClock, roots: &[&Path]) -> Device {
        let dir = tempfile::tempdir().unwrap();
        let svc = Service::open(dir.path(), Arc::new(clock.clone()), "pc").unwrap();
        let svc = Mutex::new(svc);
        for r in roots {
            svc.lock()
                .unwrap()
                .add_sync_target(SyncTarget {
                    provider: SyncProvider::Custom,
                    root: r.to_string_lossy().into_owned(),
                })
                .unwrap();
        }
        Device { svc, _dir: dir }
    }

    fn pinput(name: &str) -> ProjectInput {
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

    fn tinput(name: &str, p: &str) -> TaskInput {
        TaskInput {
            name: name.into(),
            project_id: p.into(),
            description: serde_json::json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Details"}]}]}),
            continued_from: None,
            tags: vec!["tag-concept".to_owned()],
        }
    }

    impl Device {
        fn s(&self) -> std::sync::MutexGuard<'_, Service> {
            self.svc.lock().unwrap()
        }
        fn cycle(&self) -> bool {
            run_cycle(&self.svc, &RealIo)
        }
        fn data(&self) -> Shared {
            self.s().file().data.clone()
        }
        fn id(&self) -> String {
            self.s().file().device_id.clone()
        }
    }

    fn own_file(d: &Device, root: &Path) -> PathBuf {
        devices_dir(root).join(format!("{}.json", d.id()))
    }

    #[test]
    fn name_filter_accepts_only_exact_device_files() {
        let id = "123e4567-e89b-42d3-a456-426614174000";
        assert_eq!(device_id_from_name(&format!("{id}.json")), Some(id));
        for bad in [
            format!("{id} (1).json"),
            format!("{id}-DESKTOP-PC.json"),
            format!("{id}(conflicted copy).json"),
            format!(".{id}.tmp"),
            format!("{id}.json.tmp"),
            id.to_uppercase() + ".json",
            "notes.txt".to_owned(),
            "desktop.ini".to_owned(),
        ] {
            assert_eq!(device_id_from_name(&bad), None, "{bad}");
        }
    }

    #[test]
    fn three_devices_converge() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            device(&clock, &[root.path()]),
            device(&clock, &[root.path()]),
            device(&clock, &[root.path()]),
        );
        let pa = a.s().create_project(&pinput("A")).unwrap();
        clock.advance(60);
        b.s().create_project(&pinput("B")).unwrap();
        clock.advance(60);
        let tc = c.s().create_project(&pinput("C")).unwrap();
        c.s().create_task(&tinput("Task", &tc)).unwrap();
        a.s().create_task(&tinput("TaskA", &pa)).unwrap();
        for _ in 0..3 {
            for d in [&a, &b, &c] {
                d.cycle();
            }
        }
        assert_eq!(a.data(), b.data());
        assert_eq!(b.data(), c.data());
        assert_eq!(a.data().projects.len(), 3);
        assert_eq!(a.data().tasks.len(), 2);
        // Further cycles change nothing: merging is idempotent.
        assert!(!a.cycle());
    }

    #[test]
    fn conflict_copies_and_unknown_files_are_ignored_and_kept() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let b = device(&clock, &[root.path()]);
        b.s().create_project(&pinput("Real")).unwrap();
        b.cycle();
        let bfile = own_file(&b, root.path());
        let bytes = std::fs::read(&bfile).unwrap();
        let mut evil = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap();
        evil["data"]["projects"] = serde_json::json!({});
        let dir = devices_dir(root.path());
        let copies = [
            format!("{} (1).json", b.id()),
            format!("{}-DESKTOP.json", b.id()),
            "notes.txt".to_owned(),
        ];
        for c in &copies {
            std::fs::write(dir.join(c), evil.to_string()).unwrap();
        }
        a.cycle();
        assert_eq!(a.data().projects.len(), 1);
        for c in &copies {
            assert!(dir.join(c).exists(), "{c} must not be deleted");
        }
        let status = a
            .s()
            .target_status_mut(&root.path().to_string_lossy())
            .clone();
        assert_eq!(status.ignored_files, 3);
        assert!(status.last_error.is_none());
    }

    #[test]
    fn corrupt_remote_is_skipped_and_last_good_merge_is_kept() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let b = device(&clock, &[root.path()]);
        b.s().create_project(&pinput("Real")).unwrap();
        b.cycle();
        a.cycle();
        assert_eq!(a.data().projects.len(), 1);
        let good = std::fs::read(own_file(&b, root.path())).unwrap();
        std::fs::write(own_file(&b, root.path()), &good[..good.len() / 2]).unwrap();
        clock.advance(60);
        a.s().create_project(&pinput("Mine")).unwrap();
        a.cycle();
        assert_eq!(a.data().projects.len(), 2);
        let status = a
            .s()
            .target_status_mut(&root.path().to_string_lossy())
            .clone();
        assert!(
            status.warnings.iter().any(|w| w.contains("damaged")),
            "{:?}",
            status.warnings
        );
        assert!(status.last_written_at.is_some());
    }

    #[test]
    fn invalid_currency_in_a_remote_file_skips_it_without_crashing() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let b = device(&clock, &[root.path()]);
        let p = b.s().create_project(&pinput("Real")).unwrap();
        b.s()
            .mutate(|f, ctx| {
                f.data.set_rate(
                    ctx,
                    &crate::machine::RateInput {
                        project_id: p.clone(),
                        rate_cents: Some(1),
                        currency: "USD".into(),
                        effective_from: None,
                        confirm_retroactive: false,
                    },
                )
            })
            .unwrap();
        b.cycle();
        let path = own_file(&b, root.path());
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\"USD\"", "\"GBP\"");
        std::fs::write(&path, text).unwrap();
        a.cycle();
        assert!(a.data().rates.is_empty());
        let status = a
            .s()
            .target_status_mut(&root.path().to_string_lossy())
            .clone();
        assert!(status.warnings.iter().any(|w| w.contains("damaged")));
    }

    #[test]
    fn newer_schema_is_never_read_or_overwritten() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let b = device(&clock, &[root.path()]);
        b.s().create_project(&pinput("Future")).unwrap();
        b.cycle();
        let path = own_file(&b, root.path());
        let text = std::fs::read_to_string(&path).unwrap().replace(
            &format!("\"schemaVersion\":{}", crate::model::SCHEMA_VERSION),
            "\"schemaVersion\":99",
        );
        std::fs::write(&path, &text).unwrap();
        a.cycle();
        assert!(a.data().projects.is_empty());
        let status = a
            .s()
            .target_status_mut(&root.path().to_string_lossy())
            .clone();
        assert!(status.warnings.iter().any(|w| w.contains("newer version")));
        // The newer file's owner, running old code, must not overwrite it.
        b.cycle();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let status = b
            .s()
            .target_status_mut(&root.path().to_string_lossy())
            .clone();
        assert!(status.last_error.unwrap().contains("newer"));
    }

    #[test]
    fn missing_folder_reports_and_does_not_harm_local_data_or_create_it() {
        let clock = ManualClock::at(T0);
        let base = tempfile::tempdir().unwrap();
        let gone = base.path().join("unplugged");
        let a = device(&clock, &[gone.as_path()]);
        a.s().create_project(&pinput("Local")).unwrap();
        assert!(!a.cycle());
        assert_eq!(a.data().projects.len(), 1);
        assert!(!gone.exists());
        let status = a.s().target_status_mut(&gone.to_string_lossy()).clone();
        assert!(status.last_error.unwrap().contains("not found"));
        // Logging continues.
        let p = a.data().projects.keys().next().unwrap().clone();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        a.s().start_task(&t).unwrap();
        assert_eq!(a.data().tasks[&t].state, TaskState::Running);
    }

    #[cfg(unix)]
    #[test]
    fn locked_folder_does_not_block_and_clears_after_unlock() {
        use std::os::unix::fs::PermissionsExt;
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        a.cycle();
        let dir = devices_dir(root.path());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        struct Fast;
        impl SyncIo for Fast {
            fn is_dir(&self, p: &Path) -> bool {
                RealIo.is_dir(p)
            }
            fn create_dir_all(&self, p: &Path) -> io::Result<()> {
                RealIo.create_dir_all(p)
            }
            fn list(&self, p: &Path) -> io::Result<Vec<String>> {
                RealIo.list(p)
            }
            fn file_size(&self, p: &Path) -> io::Result<u64> {
                RealIo.file_size(p)
            }
            fn read(&self, p: &Path, max: u64) -> io::Result<Vec<u8>> {
                RealIo.read(p, max)
            }
            fn write_atomic(&self, d: &Path, t: &Path, b: &[u8]) -> io::Result<()> {
                RealIo.write_atomic(d, t, b)
            }
            fn sleep(&self, _: Duration) {}
        }
        run_cycle(&a.svc, &Arc::new(Fast));
        let status = a
            .s()
            .target_status_mut(&root.path().to_string_lossy())
            .clone();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(status.last_error.is_some());
        a.cycle();
        let status = a
            .s()
            .target_status_mut(&root.path().to_string_lossy())
            .clone();
        assert!(status.last_error.is_none());
    }

    /// Fails with a sharing violation a set number of times, like a client holding the file.
    struct Flaky {
        fails: AtomicU32,
        sleeps: AtomicU32,
    }
    impl SyncIo for Flaky {
        fn is_dir(&self, p: &Path) -> bool {
            RealIo.is_dir(p)
        }
        fn create_dir_all(&self, p: &Path) -> io::Result<()> {
            RealIo.create_dir_all(p)
        }
        fn list(&self, p: &Path) -> io::Result<Vec<String>> {
            RealIo.list(p)
        }
        fn file_size(&self, p: &Path) -> io::Result<u64> {
            RealIo.file_size(p)
        }
        fn read(&self, p: &Path, max: u64) -> io::Result<Vec<u8>> {
            RealIo.read(p, max)
        }
        fn write_atomic(&self, d: &Path, t: &Path, b: &[u8]) -> io::Result<()> {
            if self.fails.load(Ordering::SeqCst) > 0 {
                self.fails.fetch_sub(1, Ordering::SeqCst);
                return Err(io::Error::from(ErrorKind::PermissionDenied));
            }
            RealIo.write_atomic(d, t, b)
        }
        fn sleep(&self, _: Duration) {
            self.sleeps.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn sharing_violations_are_retried_with_backoff() {
        assert!(
            is_windows_transient_code(32)
                && is_windows_transient_code(33)
                && is_windows_transient_code(362)
        );
        assert!(!is_windows_transient_code(2));
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let flaky = Arc::new(Flaky {
            fails: AtomicU32::new(2),
            sleeps: AtomicU32::new(0),
        });
        run_cycle(&a.svc, &flaky);
        assert_eq!(flaky.sleeps.load(Ordering::SeqCst), 2);
        assert!(own_file(&a, root.path()).exists());
        let status = a
            .s()
            .target_status_mut(&root.path().to_string_lossy())
            .clone();
        assert!(status.last_error.is_none());
    }

    #[test]
    fn unreadable_placeholder_is_not_called_corrupt_and_retries_later() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let b = device(&clock, &[root.path()]);
        b.s().create_project(&pinput("Remote")).unwrap();
        b.cycle();
        struct Offline;
        impl SyncIo for Offline {
            fn is_dir(&self, p: &Path) -> bool {
                RealIo.is_dir(p)
            }
            fn create_dir_all(&self, p: &Path) -> io::Result<()> {
                RealIo.create_dir_all(p)
            }
            fn list(&self, p: &Path) -> io::Result<Vec<String>> {
                RealIo.list(p)
            }
            fn file_size(&self, p: &Path) -> io::Result<u64> {
                RealIo.file_size(p)
            }
            fn read(&self, _: &Path, _: u64) -> io::Result<Vec<u8>> {
                Err(io::Error::new(
                    ErrorKind::TimedOut,
                    "cloud file not available",
                ))
            }
            fn write_atomic(&self, d: &Path, t: &Path, b: &[u8]) -> io::Result<()> {
                RealIo.write_atomic(d, t, b)
            }
            fn sleep(&self, _: Duration) {}
        }
        run_cycle(&a.svc, &Arc::new(Offline));
        assert!(a.data().projects.is_empty());
        let status = a
            .s()
            .target_status_mut(&root.path().to_string_lossy())
            .clone();
        assert!(
            status.warnings.iter().any(|w| w.contains("will retry")),
            "{:?}",
            status.warnings
        );
        assert!(!status.warnings.iter().any(|w| w.contains("damaged")));
        a.cycle();
        assert_eq!(a.data().projects.len(), 1);
    }

    #[test]
    fn two_targets_one_failing_does_not_affect_the_other() {
        let clock = ManualClock::at(T0);
        let good = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let bad = base.path().join("gone");
        let a = device(&clock, &[good.path(), bad.as_path()]);
        a.s().create_project(&pinput("X")).unwrap();
        a.cycle();
        assert!(own_file(&a, good.path()).exists());
        let ok = a
            .s()
            .target_status_mut(&good.path().to_string_lossy())
            .clone();
        let ko = a.s().target_status_mut(&bad.to_string_lossy()).clone();
        assert!(ok.last_written_at.is_some() && ok.last_error.is_none());
        assert!(ko.last_written_at.is_none() && ko.last_error.is_some());
    }

    #[test]
    fn same_device_stale_in_one_target_and_fresh_in_the_other() {
        let clock = ManualClock::at(T0);
        let t1 = tempfile::tempdir().unwrap();
        let t2 = tempfile::tempdir().unwrap();
        let b = device(&clock, &[t1.path(), t2.path()]);
        let a = device(&clock, &[t1.path(), t2.path()]);
        let p = b.s().create_project(&pinput("Old")).unwrap();
        b.cycle();
        let stale = std::fs::read(own_file(&b, t1.path())).unwrap();
        clock.advance(600);
        b.s().update_project(&p, &pinput("New")).unwrap();
        b.cycle();
        std::fs::write(own_file(&b, t1.path()), &stale).unwrap();
        a.cycle();
        assert_eq!(a.data().projects[&p].name, "New");
        assert_eq!(
            a.s().file().remote_devices[&b.id()].written_at.iso(),
            "2026-03-01T09:10:00Z"
        );
        a.cycle();
        assert_eq!(a.data(), b.data());
    }

    #[test]
    fn tombstones_and_archives_do_not_resurrect() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let b = device(&clock, &[root.path()]);
        let gone = a.s().create_project(&pinput("Gone")).unwrap();
        let arch = a.s().create_project(&pinput("Arch")).unwrap();
        let t = a.s().create_task(&tinput("T", &arch)).unwrap();
        a.cycle();
        b.cycle();
        clock.advance(60);
        a.s().delete_project(&gone).unwrap();
        a.s().set_project_archived(&arch, true).unwrap();
        a.cycle();
        b.cycle();
        a.cycle();
        assert!(b.data().projects[&gone].deleted);
        assert!(b.data().projects[&arch].archived);
        assert!(a.data().projects[&gone].deleted);
        assert_eq!(a.data().tasks[&t].state, TaskState::Draft);
    }

    #[test]
    fn segments_open_on_two_devices_are_kept_warned_and_unaltered() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let b = device(&clock, &[root.path()]);
        let p = a.s().create_project(&pinput("P")).unwrap();
        let ta = a.s().create_task(&tinput("TA", &p)).unwrap();
        let tb = a.s().create_task(&tinput("TB", &p)).unwrap();
        a.cycle();
        b.cycle();
        a.s().start_task(&ta).unwrap();
        clock.advance(10);
        b.s().start_task(&tb).unwrap();
        a.cycle();
        b.cycle();
        a.cycle();
        let d = a.data();
        assert_eq!(d.segments.values().filter(|s| s.end.is_none()).count(), 2);
        assert_eq!(d.tasks[&ta].state, TaskState::Running);
        assert_eq!(d.tasks[&tb].state, TaskState::Running);
        let warning = a.s().snapshot().warnings;
        assert!(
            warning.iter().any(|w| w.contains("more than one device")),
            "{warning:?}"
        );
        // Pausing the other device's task from here is refused, and nothing of theirs changes.
        assert!(a.s().pause_task(&tb).is_err());
        assert_eq!(
            a.data()
                .segments
                .values()
                .filter(|s| s.end.is_none())
                .count(),
            2
        );
    }

    #[test]
    fn a_task_stopped_elsewhere_closes_only_the_owners_segment_on_its_next_cycle() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let b = device(&clock, &[root.path()]);
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        a.cycle();
        b.cycle();
        b.s().start_task(&t).unwrap();
        b.cycle();
        a.cycle();
        clock.advance(600);
        a.s().stop_task(&t).unwrap();
        assert!(
            a.data().segments.values().next().unwrap().end.is_none(),
            "A may not alter B's segment"
        );
        a.cycle();
        b.cycle();
        let seg = b.data().segments.values().next().unwrap().clone();
        assert_eq!(seg.end.unwrap().iso(), "2026-03-01T09:10:00Z");
        a.cycle();
        assert_eq!(a.data(), b.data());
        assert_eq!(a.data().tasks[&t].state, TaskState::Stopped);
    }

    #[test]
    fn a_hung_folder_does_not_hold_up_the_others_and_is_not_piled_on() {
        #[derive(Clone)]
        struct Hangs(
            PathBuf,
            Arc<AtomicU32>,
            Arc<(Mutex<bool>, std::sync::Condvar)>,
        );
        impl SyncIo for Hangs {
            fn is_dir(&self, p: &Path) -> bool {
                if p.starts_with(&self.0) {
                    self.1.fetch_add(1, Ordering::SeqCst);
                    // Stuck until the test says otherwise, whatever the machine's load.
                    let (released, wake) = &*self.2;
                    let mut done = released.lock().unwrap();
                    while !*done {
                        done = wake.wait(done).unwrap();
                    }
                }
                RealIo.is_dir(p)
            }
            fn create_dir_all(&self, p: &Path) -> io::Result<()> {
                RealIo.create_dir_all(p)
            }
            fn list(&self, p: &Path) -> io::Result<Vec<String>> {
                RealIo.list(p)
            }
            fn file_size(&self, p: &Path) -> io::Result<u64> {
                RealIo.file_size(p)
            }
            fn read(&self, p: &Path, max: u64) -> io::Result<Vec<u8>> {
                RealIo.read(p, max)
            }
            fn write_atomic(&self, d: &Path, t: &Path, b: &[u8]) -> io::Result<()> {
                RealIo.write_atomic(d, t, b)
            }
            fn sleep(&self, _: Duration) {}
        }
        let clock = ManualClock::at(T0);
        let stuck = tempfile::tempdir().unwrap();
        let good = tempfile::tempdir().unwrap();
        let a = device(&clock, &[stuck.path(), good.path()]);
        let calls = Arc::new(AtomicU32::new(0));
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let io = Hangs(
            stuck.path().to_path_buf(),
            Arc::clone(&calls),
            Arc::clone(&release),
        );
        // The limit only has to outlast a loaded machine's disk for the healthy folder. The stuck one
        // costs it once, because the second cycle refuses a folder whose worker is still stuck.
        run_cycle_within(&a.svc, &io, Duration::from_secs(5));
        run_cycle_within(&a.svc, &io, Duration::from_secs(5));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no second worker while the first is stuck"
        );
        assert!(own_file(&a, good.path()).exists());
        let ok = a
            .s()
            .target_status_mut(&good.path().to_string_lossy())
            .clone();
        let ko = a
            .s()
            .target_status_mut(&stuck.path().to_string_lossy())
            .clone();
        assert!(ok.last_error.is_none());
        assert!(ko.last_error.unwrap().contains("not responding"));
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
    }

    #[test]
    fn a_restored_store_recovers_what_only_its_own_sync_file_still_holds() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let id = a.s().create_project(&pinput("Only in sync file")).unwrap();
        a.cycle();
        // Simulates a restore from an older backup: the local store has lost the project.
        a.s()
            .mutate(|f, _| {
                f.data.projects.clear();
                Ok(())
            })
            .unwrap();
        a.cycle();
        assert!(a.data().projects.contains_key(&id));
    }

    #[test]
    fn an_open_segment_recovered_from_its_own_file_closes_at_that_files_heartbeat() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        a.s().start_task(&t).unwrap();
        clock.advance(120);
        a.s().heartbeat().unwrap();
        a.cycle();
        clock.advance(600);
        a.s()
            .mutate(|f, _| {
                f.data.segments.clear();
                f.data.tasks.get_mut(&t).unwrap().state = TaskState::Paused;
                f.last_heartbeat = None;
                Ok(())
            })
            .unwrap();
        a.cycle();
        let d = a.data();
        let seg = d.segments.values().next().unwrap();
        assert_eq!(seg.end.unwrap().iso(), "2026-03-01T09:02:00Z");
    }

    fn poison(d: &Device, root: &Path, from: &str, to: &str) {
        let path = own_file(d, root);
        let text = std::fs::read_to_string(&path).unwrap().replace(from, to);
        std::fs::write(&path, text).unwrap();
    }

    #[test]
    fn implausible_remote_data_is_skipped_and_leaves_the_store_loadable() {
        let t0 = "2026-03-01T09:00:00Z";
        for bad in [
            "9999-12-31T23:59:59-01:00",
            "9999-12-31T23:59:59Z",
            "2031-01-01T00:00:00Z",
            "1970-01-01T00:00:00Z",
        ] {
            let clock = ManualClock::at(T0);
            let root = tempfile::tempdir().unwrap();
            let a = device(&clock, &[root.path()]);
            let b = device(&clock, &[root.path()]);
            b.s().create_project(&pinput("Poison")).unwrap();
            b.cycle();
            poison(&b, root.path(), t0, bad);
            a.cycle();
            assert!(a.data().projects.is_empty(), "{bad}");
            let status = a
                .s()
                .target_status_mut(&root.path().to_string_lossy())
                .clone();
            assert!(
                status.warnings.iter().any(|w| w.contains("damaged")),
                "{bad}"
            );
            let dir = a.s().data_dir().to_path_buf();
            let names: Vec<String> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert!(!names.iter().any(|n| n.contains("corrupt")), "{names:?}");
        }
    }

    #[test]
    fn an_impossible_segment_and_a_bad_setting_are_rejected() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let b = device(&clock, &[root.path()]);
        let p = b.s().create_project(&pinput("P")).unwrap();
        let t = b.s().create_task(&tinput("T", &p)).unwrap();
        b.s().start_task(&t).unwrap();
        b.s().pause_task(&t).unwrap();
        b.cycle();
        let path = own_file(&b, root.path());
        let good = std::fs::read_to_string(&path).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&good).unwrap();
        let seg = v["data"]["segments"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        seg["start"] = serde_json::json!("2001-01-01T00:00:00Z");
        std::fs::write(&path, v.to_string()).unwrap();
        a.cycle();
        assert!(
            a.data().segments.is_empty(),
            "a span of many years is refused"
        );
        let mut v: serde_json::Value = serde_json::from_str(&good).unwrap();
        v["data"]["settings"] =
            serde_json::json!({"clientWorkStart":{"value":5,"stamp":{"at":T0,"by":"x"}}});
        std::fs::write(&path, v.to_string()).unwrap();
        a.cycle();
        assert!(
            a.data().settings.is_empty(),
            "a mistyped setting is refused"
        );
        let mut v: serde_json::Value = serde_json::from_str(&good).unwrap();
        v["data"]["settings"] = serde_json::json!({"junk":{"value":1,"stamp":{"at":T0,"by":"x"}}});
        std::fs::write(&path, v.to_string()).unwrap();
        a.cycle();
        assert!(
            a.data().settings.is_empty(),
            "an unknown setting is refused"
        );
    }

    #[test]
    fn too_many_or_oversized_files_are_skipped_without_reading_them() {
        #[derive(Clone)]
        struct Huge(Arc<AtomicU32>);
        impl SyncIo for Huge {
            fn is_dir(&self, p: &Path) -> bool {
                RealIo.is_dir(p)
            }
            fn create_dir_all(&self, p: &Path) -> io::Result<()> {
                RealIo.create_dir_all(p)
            }
            fn list(&self, _: &Path) -> io::Result<Vec<String>> {
                Ok((0..MAX_DEVICE_FILES + 3)
                    .map(|i| format!("{}.json", uuid::Uuid::from_u128(i as u128 + 1)))
                    .collect())
            }
            fn file_size(&self, _: &Path) -> io::Result<u64> {
                Ok(MAX_REMOTE_FILE_BYTES + 1)
            }
            fn read(&self, _: &Path, _: u64) -> io::Result<Vec<u8>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new())
            }
            fn write_atomic(&self, _: &Path, _: &Path, _: &[u8]) -> io::Result<()> {
                Ok(())
            }
            fn sleep(&self, _: Duration) {}
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(devices_dir(root.path())).unwrap();
        let reads = Arc::new(AtomicU32::new(0));
        let r = read_target(
            &Huge(Arc::clone(&reads)),
            root.path(),
            "me",
            Ts::parse(T0).unwrap(),
        );
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert_eq!(r.warnings.len(), MAX_DEVICE_FILES + 3);
        assert!(r.warnings.iter().any(|w| w.contains("too many")));
    }

    /// Audit SEC-3: the real reader stops one byte past what it may read, so a file that grew after its size was taken
    /// costs no more memory than that, and is refused as too large.
    #[test]
    fn the_real_reader_reads_no_more_than_it_may() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grown.json");
        std::fs::write(&path, vec![b'x'; 11]).unwrap();
        assert_eq!(RealIo.read(&path, 11).unwrap().len(), 11);
        assert_eq!(
            RealIo.read(&path, 10).unwrap_err().kind(),
            ErrorKind::FileTooLarge
        );
        assert_eq!(
            read_at_most(&RealIo, &path, 10).unwrap_err().kind(),
            ErrorKind::FileTooLarge
        );
    }

    #[test]
    fn an_unreadable_own_file_is_not_overwritten() {
        #[derive(Clone)]
        struct Unhydrated;
        impl SyncIo for Unhydrated {
            fn is_dir(&self, p: &Path) -> bool {
                RealIo.is_dir(p)
            }
            fn create_dir_all(&self, p: &Path) -> io::Result<()> {
                RealIo.create_dir_all(p)
            }
            fn list(&self, p: &Path) -> io::Result<Vec<String>> {
                RealIo.list(p)
            }
            fn file_size(&self, p: &Path) -> io::Result<u64> {
                RealIo.file_size(p)
            }
            fn read(&self, _: &Path, _: u64) -> io::Result<Vec<u8>> {
                Err(io::Error::new(
                    ErrorKind::PermissionDenied,
                    "not available offline",
                ))
            }
            fn write_atomic(&self, d: &Path, t: &Path, b: &[u8]) -> io::Result<()> {
                RealIo.write_atomic(d, t, b)
            }
            fn sleep(&self, _: Duration) {}
        }
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let path = own_file(&a, root.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, br#"{"schemaVersion":999}"#).unwrap();
        run_cycle(&a.svc, &Unhydrated);
        assert_eq!(std::fs::read(&path).unwrap(), br#"{"schemaVersion":999}"#);
        let status = a
            .s()
            .target_status_mut(&root.path().to_string_lossy())
            .clone();
        assert!(status.last_error.unwrap().contains("not overwritten"));
    }

    #[test]
    fn a_missing_folder_keeps_the_last_known_counts() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("drive");
        std::fs::create_dir_all(&folder).unwrap();
        let a = device(&clock, &[&folder]);
        let b = device(&clock, &[&folder]);
        b.cycle();
        a.cycle();
        let key = folder.to_string_lossy().into_owned();
        assert_eq!(a.s().target_status_mut(&key).devices_seen, 1);
        std::fs::remove_dir_all(&folder).unwrap();
        a.cycle();
        let status = a.s().target_status_mut(&key).clone();
        assert_eq!(status.devices_seen, 1);
        assert!(status.last_error.is_some());
    }

    #[test]
    fn the_schedule_syncs_at_launch_soon_after_actions_and_every_30_minutes() {
        let mut s = SyncSchedule::new(0);
        assert!(s.is_due(0), "launch");
        s.cycle_done(0, false);
        assert!(!s.is_due(1));
        s.on_major_action(1000);
        assert!(!s.is_due(2999));
        assert!(s.is_due(3000));
        s.cycle_done(3000, false);
        assert_eq!(s.next_wake(), 3000 + 30 * 60 * 1000);
        assert!(s.is_due(3000 + 30 * 60 * 1000), "the timer needs no action");
    }

    #[test]
    fn a_burst_coalesces_and_never_postpones_a_scheduled_sync() {
        let mut s = SyncSchedule::new(0);
        s.cycle_done(0, false);
        s.on_text_edit(1000);
        assert_eq!(s.next_wake(), 16_000);
        s.on_text_edit(5000);
        assert_eq!(s.next_wake(), 16_000);
        s.on_major_action(6000);
        assert_eq!(s.next_wake(), 8000, "a major action goes sooner");
        s.on_major_action(7000);
        assert_eq!(s.next_wake(), 8000);
    }

    #[test]
    fn failures_retry_with_backoff_without_waiting_for_the_timer() {
        let mut s = SyncSchedule::new(0);
        s.cycle_done(0, true);
        assert_eq!(s.next_wake(), 30_000);
        s.cycle_done(30_000, true);
        assert_eq!(s.next_wake(), 90_000);
        for _ in 0..20 {
            s.cycle_done(100_000, true);
        }
        assert_eq!(s.next_wake(), 100_000 + 15 * 60 * 1000, "capped");
        s.cycle_done(200_000, false);
        assert_eq!(s.next_wake(), 200_000 + 30 * 60 * 1000, "success clears it");
    }

    #[test]
    fn a_failing_folder_shows_as_failed_for_the_schedule() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let gone = root.path().join("gone");
        std::fs::create_dir_all(&gone).unwrap();
        let a = device(&clock, &[&gone]);
        a.cycle();
        assert!(!a.s().any_target_failed());
        std::fs::remove_dir_all(&gone).unwrap();
        a.cycle();
        assert!(a.s().any_target_failed());
    }

    fn target(root: &Path) -> SyncTarget {
        SyncTarget {
            provider: SyncProvider::Custom,
            root: root.to_string_lossy().into_owned(),
        }
    }
    fn mv(a: &Device, from: &Path, to: &Path) -> Result<MoveResult, String> {
        let f = from.to_string_lossy().into_owned();
        move_target(
            &a.svc,
            &RealIo,
            Some(&f),
            &target(to),
            Some(&f),
            &mut |_, _, _| {},
        )
    }
    fn roots(a: &Device) -> Vec<String> {
        a.s()
            .file()
            .prefs
            .sync_targets
            .iter()
            .map(|t| t.root.clone())
            .collect()
    }
    fn names(root: &Path) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(devices_dir(root))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn moving_carries_every_devices_data_and_leaves_the_old_folder_alone() {
        let clock = ManualClock::at(T0);
        let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = device(&clock, &[old.path()]);
        let b = device(&clock, &[old.path()]);
        let c = device(&clock, &[old.path()]);
        b.s().create_project(&pinput("From B")).unwrap();
        c.s().create_project(&pinput("From C")).unwrap();
        a.s().create_project(&pinput("From A")).unwrap();
        for d in [&b, &c, &a] {
            d.cycle();
        }
        let before: Vec<(String, Vec<u8>)> = names(old.path())
            .into_iter()
            .map(|n| {
                let bytes = std::fs::read(devices_dir(old.path()).join(&n)).unwrap();
                (n, bytes)
            })
            .collect();
        let got = mv(&a, old.path(), new.path()).unwrap();
        assert_eq!(got.files_copied, 2);
        assert_eq!(got.counts.projects, 3);
        assert!(!got.source_missing);
        assert_eq!(names(new.path()).len(), 3);
        assert_eq!(a.data().projects.len(), 3, "merged into local too");
        assert_eq!(roots(&a), vec![new.path().to_string_lossy().into_owned()]);
        for (n, bytes) in before {
            assert_eq!(
                std::fs::read(devices_dir(old.path()).join(&n)).unwrap(),
                bytes,
                "{n}"
            );
        }
    }

    #[test]
    fn moving_into_a_folder_with_data_merges_and_doesnt_overwrite_fresher_files() {
        let clock = ManualClock::at(T0);
        let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = device(&clock, &[old.path()]);
        let b = device(&clock, &[old.path()]);
        let d = device(&clock, &[new.path()]);
        b.s().create_project(&pinput("Stale in old")).unwrap();
        b.cycle();
        d.s().create_project(&pinput("Only in new")).unwrap();
        d.cycle();
        // B's file in the new folder is newer than the one in the old folder.
        clock.advance(600);
        let fresh_b = {
            b.s().create_project(&pinput("Fresh B")).unwrap();
            let f = b.s().device_file();
            let path = devices_dir(new.path()).join(format!("{}.json", b.id()));
            std::fs::write(&path, serde_json::to_vec(&f).unwrap()).unwrap();
            path
        };
        let kept = std::fs::read(&fresh_b).unwrap();
        clock.advance(600);
        let got = mv(&a, old.path(), new.path()).unwrap();
        assert_eq!(got.files_already_there, 1);
        assert_eq!(std::fs::read(&fresh_b).unwrap(), kept);
        let projects: Vec<String> = a.data().projects.values().map(|p| p.name.clone()).collect();
        for want in ["Stale in old", "Only in new", "Fresh B"] {
            assert!(projects.iter().any(|p| p == want), "{want}");
        }
    }

    #[test]
    fn a_missing_previous_folder_uses_local_data_says_so_and_can_be_retried() {
        let clock = ManualClock::at(T0);
        let (base, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let old = base.path().join("old");
        std::fs::create_dir_all(&old).unwrap();
        let a = device(&clock, &[&old]);
        let b = device(&clock, &[&old]);
        b.s().create_project(&pinput("Only in old")).unwrap();
        b.cycle();
        a.s().create_project(&pinput("Local")).unwrap();
        let hidden = base.path().join("offline");
        std::fs::rename(&old, &hidden).unwrap();
        let got = mv(&a, &old, new.path()).unwrap();
        assert!(got.source_missing);
        assert!(got.warnings.iter().any(|w| w.contains("previous folder")));
        assert_eq!(got.counts.projects, 1);
        let pending = a.s().file().prefs.pending_copy.clone().unwrap();
        assert_eq!(pending.to, new.path().to_string_lossy());
        // The drive comes back, and the copy is retried.
        std::fs::rename(&hidden, &old).unwrap();
        let from = pending.from.clone();
        let again = move_target(
            &a.svc,
            &RealIo,
            Some(&from),
            &target(new.path()),
            None,
            &mut |_, _, _| {},
        )
        .unwrap();
        assert!(!again.source_missing);
        assert_eq!(again.files_copied, 1);
        assert_eq!(a.data().projects.len(), 2);
        assert!(a.s().file().prefs.pending_copy.is_none());
    }

    #[derive(Clone)]
    struct FailsWriting(String, Arc<AtomicU32>);
    impl SyncIo for FailsWriting {
        fn is_dir(&self, p: &Path) -> bool {
            RealIo.is_dir(p)
        }
        fn create_dir_all(&self, p: &Path) -> io::Result<()> {
            RealIo.create_dir_all(p)
        }
        fn list(&self, p: &Path) -> io::Result<Vec<String>> {
            RealIo.list(p)
        }
        fn file_size(&self, p: &Path) -> io::Result<u64> {
            RealIo.file_size(p)
        }
        fn read(&self, p: &Path, max: u64) -> io::Result<Vec<u8>> {
            RealIo.read(p, max)
        }
        fn write_atomic(&self, d: &Path, t: &Path, b: &[u8]) -> io::Result<()> {
            if d.to_string_lossy().contains(&self.0) {
                self.1.fetch_add(1, Ordering::SeqCst);
                return Err(io::Error::other("disk full"));
            }
            RealIo.write_atomic(d, t, b)
        }
        fn sleep(&self, _: Duration) {}
    }

    #[test]
    fn a_failed_copy_keeps_the_old_folder_as_the_setting_and_local_data_is_unharmed() {
        let clock = ManualClock::at(T0);
        let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = device(&clock, &[old.path()]);
        let b = device(&clock, &[old.path()]);
        b.s().create_project(&pinput("B")).unwrap();
        b.cycle();
        a.s().create_project(&pinput("A")).unwrap();
        // The other device's file copies; this device's own write then fails.
        let io = FailsWriting(a.id(), Arc::new(AtomicU32::new(0)));
        let from = old.path().to_string_lossy().into_owned();
        let err = move_target(
            &a.svc,
            &io,
            Some(&from),
            &target(new.path()),
            Some(&from),
            &mut |_, _, _| {},
        )
        .unwrap_err();
        assert!(err.contains("disk full"), "{err}");
        assert_eq!(roots(&a), vec![from.clone()], "the setting is unchanged");
        assert!(a.data().projects.values().any(|p| p.name == "A"));
        // Time can still be logged, and a later attempt finishes the job.
        let t = {
            let p = a.data().projects.keys().next().unwrap().clone();
            a.s().create_task(&tinput("T", &p)).unwrap()
        };
        a.s().start_task(&t).unwrap();
        assert!(mv(&a, old.path(), new.path()).is_ok());
    }

    #[test]
    fn conflict_copies_and_damaged_files_arent_copied_and_a_repeat_changes_nothing() {
        let clock = ManualClock::at(T0);
        let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = device(&clock, &[old.path()]);
        let b = device(&clock, &[old.path()]);
        b.s().create_project(&pinput("B")).unwrap();
        b.cycle();
        let dir = devices_dir(old.path());
        std::fs::write(dir.join(format!("{} (1).json", b.id())), "{}").unwrap();
        std::fs::write(
            dir.join("123e4567-e89b-42d3-a456-426614174000.json"),
            "{nope",
        )
        .unwrap();
        std::fs::write(
            dir.join("223e4567-e89b-42d3-a456-426614174000.json"),
            r#"{"schemaVersion":999}"#,
        )
        .unwrap();
        let first = mv(&a, old.path(), new.path()).unwrap();
        assert_eq!(first.files_copied, 1);
        assert_eq!(first.files_ignored, 1);
        assert!(first.warnings.iter().any(|w| w.contains("damaged")));
        assert!(first.warnings.iter().any(|w| w.contains("newer version")));
        assert_eq!(names(new.path()).len(), 2);
        let from = old.path().to_string_lossy().into_owned();
        let again = move_target(
            &a.svc,
            &RealIo,
            Some(&from),
            &target(new.path()),
            None,
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(again.files_copied, 0);
        assert_eq!(again.files_already_there, 1);
        assert_eq!(again.checksum, first.checksum, "idempotent");
    }

    #[test]
    fn the_preview_reports_both_folders() {
        let clock = ManualClock::at(T0);
        let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = device(&clock, &[old.path()]);
        let b = device(&clock, &[old.path()]);
        b.s().create_project(&pinput("B")).unwrap();
        b.cycle();
        let pv = preview_move(
            &RealIo,
            Some(old.path()),
            new.path(),
            &a.id(),
            Ts::parse(T0).unwrap(),
            &a.data(),
        );
        assert!(pv.source_available);
        assert_eq!(pv.source_counts.projects, 1);
        assert_eq!(pv.source_devices.len(), 1);
        assert!(!pv.destination_has_data);
        let gone = old.path().join("nope");
        let pv = preview_move(
            &RealIo,
            Some(&gone),
            new.path(),
            &a.id(),
            Ts::parse(T0).unwrap(),
            &a.data(),
        );
        assert!(!pv.source_available);
    }

    #[test]
    fn preview_counts_what_a_folder_holds() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let b = device(&clock, &[root.path()]);
        let p = b.s().create_project(&pinput("P")).unwrap();
        b.s().create_task(&tinput("T", &p)).unwrap();
        b.cycle();
        let pv = preview_folder(&RealIo, root.path(), "someone-else", Ts::parse(T0).unwrap());
        assert!(pv.has_data);
        assert_eq!((pv.projects, pv.tasks, pv.segments), (1, 1, 0));
        assert_eq!(pv.devices, ["pc"]);
        let empty = tempfile::tempdir().unwrap();
        assert!(!preview_folder(&RealIo, empty.path(), "x", Ts::parse(T0).unwrap()).has_data);
    }

    #[test]
    fn sync_failure_never_rolls_back_a_saved_local_change() {
        let clock = ManualClock::at(T0);
        let dir = tempfile::tempdir().unwrap();
        let svc = Service::open(dir.path(), Arc::new(clock.clone()), "pc").unwrap();
        let svc = Mutex::new(svc);
        svc.lock()
            .unwrap()
            .add_sync_target(SyncTarget {
                provider: SyncProvider::OneDrive,
                root: "/nonexistent/place".into(),
            })
            .unwrap();
        let id = svc.lock().unwrap().create_project(&pinput("Kept")).unwrap();
        run_cycle(&svc, &RealIo);
        drop(svc);
        let reopened = Service::open(dir.path(), Arc::new(clock), "pc").unwrap();
        assert!(reopened.file().data.projects.contains_key(&id));
    }

    #[test]
    fn tag_and_project_colors_reach_the_other_device_and_the_later_edit_wins() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let b = device(&clock, &[root.path()]);
        let tag = a
            .s()
            .save_tag(&crate::catalog::TagInput {
                id: None,
                name: "Spectrum".into(),
                color: "#123456".into(),
            })
            .unwrap();
        let project = a
            .s()
            .create_project(&ProjectInput {
                color: "#654321".into(),
                ..pinput("Painted")
            })
            .unwrap();
        a.cycle();
        b.cycle();
        assert_eq!(b.data().tags[&tag].color, "#123456");
        assert_eq!(b.data().projects[&project].color, "#654321");
        clock.advance(5);
        b.s()
            .save_tag(&crate::catalog::TagInput {
                id: Some(tag.clone()),
                name: "Spectrum".into(),
                color: "#fedcba".into(),
            })
            .unwrap();
        b.cycle();
        a.cycle();
        assert_eq!(a.data().tags[&tag].color, "#fedcba");
    }

    #[test]
    fn a_task_deleted_with_its_time_stays_gone_through_the_folder_on_every_device() {
        use crate::time::Clock;
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let b = device(&clock, &[root.path()]);
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        a.s().start_task(&t).unwrap();
        clock.advance(3600);
        a.s().pause_task(&t).unwrap();
        a.cycle();
        b.cycle();
        let now = clock.now();
        let pv = preview_folder(&RealIo, root.path(), "someone-else", now);
        assert_eq!((pv.tasks, pv.segments), (1, 1));

        assert_eq!(
            b.s().delete_task(&t).map_err(|e| e.code()),
            Ok(()),
            "paused, with its time synced"
        );
        clock.advance(60);
        // Unaware, A edits it. This goes on to the folder too, and on to B.
        a.s()
            .patch_task(
                &t,
                &crate::machine::TaskPatch {
                    name: Some("Renamed".into()),
                    ..Default::default()
                },
                &Default::default(),
            )
            .unwrap();
        clock.advance(60);
        a.cycle();
        b.cycle();
        a.cycle();
        b.cycle();
        for d in [&a, &b] {
            assert!(d.s().snapshot().tasks.is_empty());
            assert!(d.s().snapshot().segments.is_empty());
            assert_eq!(d.data().segments.len(), 1, "the record stays");
            assert!(d.data().tasks[&t].discarded.is_some());
            let own = std::fs::read_to_string(own_file(d, root.path())).unwrap();
            let v: serde_json::Value = serde_json::from_str(&own).unwrap();
            assert!(v["data"]["tasks"][&t]["discarded"]["at"].is_string());
        }
        assert_eq!(a.data().tasks[&t], b.data().tasks[&t]);
        // A folder's preview counts what a person would see.
        let pv = preview_folder(&RealIo, root.path(), "someone-else", clock.now());
        assert_eq!((pv.tasks, pv.segments), (0, 0));
    }

    #[test]
    fn a_task_deleted_while_another_device_times_it_stops_there_through_the_folder() {
        use crate::time::Clock;
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let b = device(&clock, &[root.path()]);
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        a.s().start_task(&t).unwrap();
        a.cycle();
        b.cycle();
        assert!(b.s().snapshot().tasks[0].running_elsewhere.is_some());

        b.s().delete_task(&t).unwrap();
        b.cycle();
        assert!(b.s().snapshot().tasks.is_empty());
        // A hears only when it next looks in the folder, and goes on timing until then.
        clock.advance(60);
        assert_eq!(a.s().snapshot().tasks.len(), 1);
        assert_eq!(a.data().own_open_segments(&a.id()).len(), 1);

        a.cycle();
        assert!(a.data().own_open_segments(&a.id()).is_empty());
        let told = a.s().snapshot().notices;
        assert!(
            told.iter().any(|n| n.message.contains("was deleted on")),
            "{told:?}"
        );
        // Its own file now says the segment is closed, so the other device has that too.
        clock.advance(60);
        b.cycle();
        a.cycle();
        for d in [&a, &b] {
            let snap = d.s().snapshot();
            assert!(snap.tasks.is_empty());
            assert!(snap.segments.is_empty());
            assert!(snap.raw_segments.is_empty());
            assert!(snap.warnings.is_empty(), "{:?}", snap.warnings);
            assert!(d.data().tasks[&t].discarded.is_some());
            assert!(d.data().segments.values().all(|s| s.end.is_some()));
        }
        assert_eq!(a.data().tasks[&t], b.data().tasks[&t]);
        assert_eq!(a.data().segments, b.data().segments);
        let pv = preview_folder(&RealIo, root.path(), "someone-else", clock.now());
        assert_eq!((pv.tasks, pv.segments), (0, 0));
    }

    #[test]
    fn a_far_future_delete_marker_is_refused_with_its_record() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let b = device(&clock, &[root.path()]);
        let p = b.s().create_project(&pinput("P")).unwrap();
        let t = b.s().create_task(&tinput("T", &p)).unwrap();
        b.cycle();
        let path = own_file(&b, root.path());
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["data"]["tasks"][&t]["discarded"] =
            serde_json::json!({"at": "2099-01-01T00:00:00Z", "by": "x"});
        std::fs::write(&path, v.to_string()).unwrap();
        a.cycle();
        assert!(a.data().tasks.is_empty());
        assert!(a.data().projects.contains_key(&p), "the rest is read");
    }

    /// A price no reader takes, as `set_rate` could write before it had limits.
    fn poison_rate(d: &Device, project: &str) -> String {
        let at = Ts::parse("2206-03-02T00:00:00Z").unwrap();
        d.s()
            .mutate(|f, c| {
                let id = "bad-price".to_owned();
                f.data.rates.insert(
                    id.clone(),
                    crate::model::RateVersion {
                        id: id.clone(),
                        project_id: project.to_owned(),
                        effective_from: at,
                        rate_cents: Some(5000),
                        currency: "EUR".into(),
                        deleted: false,
                        stamp: c.stamp(),
                    },
                );
                Ok(id)
            })
            .unwrap()
    }

    fn status_of(d: &Device, root: &Path) -> TargetStatus {
        d.s().target_status_mut(&root.to_string_lossy()).clone()
    }

    fn file_on_disk(d: &Device, root: &Path) -> DeviceFile {
        serde_json::from_slice(&std::fs::read(own_file(d, root)).unwrap()).unwrap()
    }

    #[test]
    fn r1_a_store_holding_one_bad_record_still_syncs_the_rest() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&clock, &[root.path()]),
            device(&clock, &[root.path()]),
        );
        let p = a.s().create_project(&pinput("Acme")).unwrap();
        a.cycle();
        b.cycle();
        let bad = poison_rate(&a, &p);
        clock.advance(60);
        let later = a.s().create_project(&pinput("Later")).unwrap();
        a.cycle();
        b.cycle();
        assert!(
            b.data().projects.contains_key(&later),
            "the rest of the file reaches the other device"
        );
        assert!(!b.data().rates.contains_key(&bad));
        // What went out passes the strict check, so a device on an older version reads it as well.
        check_device_file(&file_on_disk(&a, root.path()), clock.now()).unwrap();
        // The device holding it keeps it, and says which record that is.
        assert!(a.data().rates.contains_key(&bad));
        let status = status_of(&a, root.path());
        assert!(
            status
                .warnings
                .iter()
                .any(|w| w.contains("the price of \"Acme\"") && w.contains("2206-03-02")),
            "{status:?}"
        );
        assert!(status.last_written_at.is_some());
    }

    #[test]
    fn r1_the_reader_skips_each_bad_record_and_reports_it() {
        type Damage = fn(&mut serde_json::Value, &str, &str, &str, &str);
        // What is only dated ahead isn't damaged: it can be read when that date comes.
        const DAMAGED: bool = false;
        const AHEAD: bool = true;
        let cases: [(&str, bool, Damage); 7] = [
            ("the price of \"P\"", DAMAGED, |v, _, _, _, rate| {
                v["data"]["rates"][rate]["effectiveFrom"] = "2206-03-02T00:00:00Z".into();
            }),
            ("the time on \"T\"", DAMAGED, |v, _, seg, _, _| {
                v["data"]["segments"][seg]["start"] = "0026-03-01T09:00:00Z".into();
                v["data"]["segments"][seg]["end"] = "0026-03-01T10:00:00Z".into();
            }),
            ("the time on \"T\"", DAMAGED, |v, _, seg, _, _| {
                v["data"]["segments"][seg]["edit"] = serde_json::json!({
                    "start": "0026-03-01T09:00:00Z", "end": "0026-03-01T10:00:00Z", "reason": "x",
                });
            }),
            (
                "the billable pause on \"T\"",
                DAMAGED,
                |v, _, _, pause, _| {
                    v["data"]["pauses"][pause]["start"] = "1999-01-01T00:00:00Z".into();
                },
            ),
            ("task \"T\"", AHEAD, |v, task, _, _, _| {
                v["data"]["tasks"][task]["discarded"] =
                    serde_json::json!({"at": "2099-01-01T00:00:00Z", "by": "x"});
            }),
            ("the setting junk", DAMAGED, |v, _, _, _, _| {
                v["data"]["settings"]["junk"] =
                    serde_json::json!({"value": 1, "stamp": {"at": T0, "by": "x"}});
            }),
            ("a line in the record of changes", AHEAD, |v, _, _, _, _| {
                v["data"]["auditLog"]["bad"] = serde_json::json!({
                    "id": "bad", "at": "2099-01-01T00:00:00Z", "by": "x", "entity": "x",
                    "entityId": "x", "action": "x",
                });
            }),
        ];
        for (i, (name, ahead, damage)) in cases.into_iter().enumerate() {
            let clock = ManualClock::at("2026-03-03T09:00:00Z");
            let root = tempfile::tempdir().unwrap();
            let (a, b) = (
                device(&clock, &[root.path()]),
                device(&clock, &[root.path()]),
            );
            let p = b.s().create_project(&pinput("P")).unwrap();
            let t = b.s().create_task(&tinput("T", &p)).unwrap();
            let seg = b
                .s()
                .add_manual_entry(&crate::adjust::ManualEntryInput {
                    task_id: t.clone(),
                    start: Ts::parse("2026-03-01T09:00:00Z").unwrap(),
                    end: Ts::parse("2026-03-01T10:00:00Z").unwrap(),
                    reason: String::new(),
                    subtask_id: None,
                    allow_overlap: true,
                })
                .unwrap();
            let pause = b
                .s()
                .mark_billable_pause(&crate::adjust::PauseInput {
                    task_id: t.clone(),
                    start: Ts::parse("2026-03-01T11:00:00Z").unwrap(),
                    end: Ts::parse("2026-03-01T12:00:00Z").unwrap(),
                    note: String::new(),
                })
                .unwrap();
            b.s()
                .set_rate(&crate::machine::RateInput {
                    project_id: p.clone(),
                    rate_cents: Some(1000),
                    currency: "EUR".into(),
                    effective_from: None,
                    confirm_retroactive: false,
                })
                .unwrap();
            let rate = b.data().rates.keys().next().unwrap().clone();
            b.cycle();
            let path = own_file(&b, root.path());
            let mut v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            damage(&mut v, &t, &seg, &pause, &rate);
            std::fs::write(&path, v.to_string()).unwrap();
            a.cycle();
            let got = a.data();
            assert!(
                got.projects.contains_key(&p),
                "case {i}: the rest of the file is read"
            );
            let status = status_of(&a, root.path());
            assert!(
                status.warnings.iter().any(|w| w.contains("Skipped")
                    && w.contains(name)
                    && w.contains("damaged") != ahead
                    && w.contains("ahead of this computer's clock") == ahead),
                "case {i}: {status:?}"
            );
            check_shared_is_clean(&got, clock.now(), i);
        }
    }

    fn check_shared_is_clean(s: &Shared, now: Ts, case: usize) {
        crate::validate::check_shared(s, now).unwrap_or_else(|e| panic!("case {case}: {e}"));
    }

    #[test]
    fn r1_a_heartbeat_from_a_clock_put_back_costs_the_heartbeat_not_the_file() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&clock, &[root.path()]),
            device(&clock, &[root.path()]),
        );
        let p = b.s().create_project(&pinput("P")).unwrap();
        b.cycle();
        let path = own_file(&b, root.path());
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["lastHeartbeat"] = "2031-01-01T00:00:00Z".into();
        std::fs::write(&path, v.to_string()).unwrap();
        a.cycle();
        assert!(a.data().projects.contains_key(&p));
        assert_eq!(a.s().file().remote_devices[&b.id()].last_heartbeat, None);
    }

    #[test]
    fn r1_a_clock_put_back_keeps_everything_in_the_file_and_says_so() {
        let (here, there) = (ManualClock::at(T0), ManualClock::at(T0));
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&here, &[root.path()]),
            device(&there, &[root.path()]),
        );
        let p = a.s().create_project(&pinput("P")).unwrap();
        // This clock is the wrong one. The other device, with the right one, can use all of this.
        here.set("2026-01-01T09:00:00Z");
        a.cycle();
        b.cycle();
        assert!(b.data().projects.contains_key(&p));
        assert!(file_on_disk(&a, root.path()).data.projects.contains_key(&p));
        let status = status_of(&a, root.path());
        assert!(
            status
                .warnings
                .iter()
                .any(|w| w.contains("project \"P\"") && w.contains("clock")),
            "{status:?}"
        );
        assert!(status.last_error.is_none(), "{status:?}");
    }

    #[test]
    fn r1_a_clock_no_reader_would_believe_is_reported_as_an_error_until_it_is_right() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        clock.set("1999-06-01T09:00:00Z");
        a.cycle();
        let status = status_of(&a, root.path());
        assert!(
            status
                .last_error
                .as_ref()
                .is_some_and(|e| e.contains("will ignore")),
            "{status:?}"
        );
        assert!(a.s().any_target_failed(), "so it is tried again soon");
        clock.set(T0);
        clock.advance(60);
        a.cycle();
        assert_eq!(status_of(&a, root.path()).last_error, None);
    }

    #[test]
    fn r1_many_bad_records_are_named_by_a_few_and_counted() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&clock, &[root.path()]),
            device(&clock, &[root.path()]),
        );
        let p = b.s().create_project(&pinput("P")).unwrap();
        b.cycle();
        let path = own_file(&b, root.path());
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for i in 0..5 {
            let id = format!("bad-{i}");
            v["data"]["rates"][&id] = serde_json::json!({
                "id": id, "projectId": p, "effectiveFrom": "2206-03-02T00:00:00Z", "rateCents": 1,
                "currency": "EUR", "stamp": {"at": T0, "by": "x"},
            });
        }
        std::fs::write(&path, v.to_string()).unwrap();
        a.cycle();
        assert!(a.data().rates.is_empty());
        let status = status_of(&a, root.path());
        let warning = status
            .warnings
            .iter()
            .find(|w| w.contains("Skipped"))
            .unwrap();
        assert!(warning.contains("5 damaged records"), "{warning}");
        assert!(warning.contains("and 2 more"), "{warning}");
        assert_eq!(warning.matches("the price of").count(), 3, "{warning}");
    }

    #[test]
    fn r1_records_whose_parent_was_left_out_do_no_harm() {
        let clock = ManualClock::at("2026-03-03T09:00:00Z");
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&clock, &[root.path()]),
            device(&clock, &[root.path()]),
        );
        let p = b.s().create_project(&pinput("P")).unwrap();
        let t = b.s().create_task(&tinput("T", &p)).unwrap();
        b.s()
            .add_manual_entry(&crate::adjust::ManualEntryInput {
                task_id: t.clone(),
                start: Ts::parse("2026-03-01T09:00:00Z").unwrap(),
                end: Ts::parse("2026-03-01T10:00:00Z").unwrap(),
                reason: String::new(),
                subtask_id: None,
                allow_overlap: true,
            })
            .unwrap();
        let pause = b
            .s()
            .mark_billable_pause(&crate::adjust::PauseInput {
                task_id: t.clone(),
                start: Ts::parse("2026-03-01T11:00:00Z").unwrap(),
                end: Ts::parse("2026-03-01T12:00:00Z").unwrap(),
                note: String::new(),
            })
            .unwrap();
        b.cycle();
        let path = own_file(&b, root.path());
        let good = std::fs::read_to_string(&path).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&good).unwrap();
        v["data"]["projects"][&p]["stamp"]["at"] = "2031-01-01T00:00:00Z".into();
        std::fs::write(&path, v.to_string()).unwrap();
        a.cycle();
        // A task with no project to go with it: nothing panics, and it reads as it would with a project gone.
        assert!(a.data().tasks.contains_key(&t) && a.data().projects.is_empty());
        let snap = a.s().snapshot();
        assert_eq!(snap.tasks.len(), 1);
        let rows = a.s().export_source().rows(chrono_tz::UTC);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].project_name, "(unknown project)");

        // And a billable pause with no task to go with it counts nowhere until the task arrives.
        let (root, c) = (tempfile::tempdir().unwrap(), device(&clock, &[]));
        c.s()
            .add_sync_target(SyncTarget {
                provider: SyncProvider::Custom,
                root: root.path().to_string_lossy().into_owned(),
            })
            .unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&good).unwrap();
        v["data"]["tasks"][&t]["stateStamp"]["at"] = "2031-01-01T00:00:00Z".into();
        let dir = devices_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{}.json", b.id())), v.to_string()).unwrap();
        c.cycle();
        assert!(c.data().tasks.is_empty());
        assert!(c.data().pauses.contains_key(&pause));
        let snap = c.s().snapshot();
        assert!(snap.tasks.is_empty() && snap.segments.is_empty() && snap.raw_segments.is_empty());
        assert!(c.s().export_source().rows(chrono_tz::UTC).is_empty());
    }

    #[test]
    fn r1_a_bad_record_in_this_devices_own_file_costs_only_that_record() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let id = a.s().create_project(&pinput("Only in sync file")).unwrap();
        a.cycle();
        let path = own_file(&a, root.path());
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["data"]["rates"] = serde_json::json!({"bad": {
            "id": "bad", "projectId": id, "effectiveFrom": "2206-03-02T00:00:00Z", "rateCents": 1,
            "currency": "EUR", "stamp": {"at": T0, "by": "x"},
        }});
        std::fs::write(&path, v.to_string()).unwrap();
        // Simulates a restore from an older backup: the local store has lost the project.
        a.s()
            .mutate(|f, _| {
                f.data.projects.clear();
                Ok(())
            })
            .unwrap();
        a.cycle();
        assert!(a.data().projects.contains_key(&id));
        assert!(a.data().rates.is_empty());
    }

    #[test]
    fn r1_removing_a_price_version_repairs_a_poisoned_store() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let p = a.s().create_project(&pinput("Acme")).unwrap();
        let bad = poison_rate(&a, &p);
        a.cycle();
        assert!(!status_of(&a, root.path()).warnings.is_empty());
        // Far in the future, so taking it away changes no earnings and needs no confirmation.
        a.s().delete_rate_version(&bad, false).unwrap();
        assert!(a.s().snapshot().rates.is_empty());
        assert!(a.s().delete_rate_version(&bad, false).is_err(), "only once");
        assert!(
            a.s()
                .audit_entries(Some(&bad))
                .iter()
                .any(|e| e.entity == "price" && e.action == "removed")
        );
        clock.advance(60);
        a.cycle();
        let status = status_of(&a, root.path());
        assert!(status.warnings.is_empty(), "{status:?}");
        assert!(status.last_error.is_none(), "{status:?}");
        check_device_file(&file_on_disk(&a, root.path()), clock.now()).unwrap();
    }

    #[test]
    fn r1_removing_a_good_price_version_asks_first_for_the_past_and_reaches_other_devices() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&clock, &[root.path()]),
            device(&clock, &[root.path()]),
        );
        let p = a.s().create_project(&pinput("Acme")).unwrap();
        a.s()
            .set_rate(&crate::machine::RateInput {
                project_id: p.clone(),
                rate_cents: Some(5000),
                currency: "EUR".into(),
                effective_from: Some("2026-01-01T00:00:00Z".into()),
                confirm_retroactive: true,
            })
            .unwrap();
        let id = a.data().rates.keys().next().unwrap().clone();
        a.cycle();
        b.cycle();
        assert!(!b.data().rates[&id].deleted);
        assert!(matches!(
            a.s().delete_rate_version(&id, false),
            Err(crate::error::CoreError::Conflict(_))
        ));
        assert!(!a.data().rates[&id].deleted, "refused, so nothing changed");
        clock.advance(60);
        a.s().delete_rate_version(&id, true).unwrap();
        a.cycle();
        b.cycle();
        assert!(b.data().rates[&id].deleted, "the tombstone travels");
        assert!(b.s().snapshot().rates.is_empty());
    }

    /// An entry no reader takes, as `add_manual_entry` could write before it had limits.
    fn bad_entry(d: &Device, task: &str) -> String {
        d.s()
            .mutate(|f, c| {
                let id = "bad-entry".to_owned();
                f.data.segments.insert(
                    id.clone(),
                    crate::model::Segment {
                        id: id.clone(),
                        task_id: task.to_owned(),
                        device_id: c.device.to_owned(),
                        start: Ts::parse("0026-03-01T09:00:00Z").unwrap(),
                        end: Some(Ts::parse("0026-03-01T10:00:00Z").unwrap()),
                        manual: true,
                        edit_stamp: c.stamp(),
                        ..Default::default()
                    },
                );
                Ok(id)
            })
            .unwrap()
    }

    /// Time away no reader takes, on an entry that may be fine or may not.
    fn bad_gap(d: &Device, segment: &str) {
        d.s()
            .mutate(|f, c| {
                let (task_id, device_id) = {
                    let seg = &f.data.segments[segment];
                    (seg.task_id.clone(), seg.device_id.clone())
                };
                f.data.gaps.insert(
                    "bad-gap".to_owned(),
                    crate::model::IdleGap {
                        id: "bad-gap".to_owned(),
                        segment_id: segment.to_owned(),
                        task_id,
                        device_id,
                        start: Ts::parse("0026-03-01T09:10:00Z").unwrap(),
                        end: Ts::parse("0026-03-01T09:20:00Z").unwrap(),
                        kind: crate::model::GapKind::Idle,
                        decision: crate::model::GapDecision::Pending,
                        decision_stamp: c.stamp(),
                    },
                );
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn r1_voiding_an_entry_with_an_impossible_date_ends_the_report() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&clock, &[root.path()]),
            device(&clock, &[root.path()]),
        );
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        let seg = bad_entry(&a, &t);
        bad_gap(&a, &seg);
        a.cycle();
        b.cycle();
        assert!(b.data().tasks.contains_key(&t), "the rest arrives");
        assert!(!b.data().segments.contains_key(&seg));
        let status = status_of(&a, root.path());
        assert!(
            status
                .warnings
                .iter()
                .any(|w| w.contains("the time on \"T\"")),
            "{status:?}"
        );
        a.s().void_segment(&seg, "typed the wrong year").unwrap();
        clock.advance(60);
        a.cycle();
        let status = status_of(&a, root.path());
        assert!(status.warnings.is_empty(), "{status:?}");
        check_device_file(&file_on_disk(&a, root.path()), clock.now()).unwrap();
    }

    #[test]
    fn r1_a_store_holding_a_bad_record_can_still_move_to_a_new_folder() {
        let clock = ManualClock::at(T0);
        let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = device(&clock, &[old.path()]);
        let p = a.s().create_project(&pinput("Acme")).unwrap();
        poison_rate(&a, &p);
        a.cycle();
        let got = mv(&a, old.path(), new.path()).unwrap();
        assert_eq!(got.counts.projects, 1);
        assert!(
            got.warnings
                .iter()
                .any(|w| w.contains("the price of \"Acme\"")),
            "{got:?}"
        );
        check_device_file(&file_on_disk(&a, new.path()), clock.now()).unwrap();
    }

    fn manual_entry(d: &Device, task: &str) -> String {
        d.s()
            .add_manual_entry(&crate::adjust::ManualEntryInput {
                task_id: task.to_owned(),
                start: Ts::parse("2026-02-01T09:00:00Z").unwrap(),
                end: Ts::parse("2026-02-01T10:00:00Z").unwrap(),
                reason: String::new(),
                subtask_id: None,
                allow_overlap: true,
            })
            .unwrap()
    }

    #[test]
    fn r1_a_task_left_out_keeps_its_time_with_it_so_a_timer_elsewhere_runs_on() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&clock, &[root.path()]),
            device(&clock, &[root.path()]),
        );
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        a.s().start_task(&t).unwrap();
        a.cycle();
        b.cycle();
        assert_eq!(b.data().tasks[&t].state, TaskState::Running);
        // One edit while this clock ran a week fast, and it has been put right since. The task reads as
        // ahead until that date, whatever else happens to it.
        clock.set("2026-03-08T09:00:00Z");
        let renamed = crate::machine::TaskPatch {
            name: Some("T renamed".into()),
            ..Default::default()
        };
        a.s().patch_task(&t, &renamed, &Default::default()).unwrap();
        clock.set("2026-03-01T10:00:00Z");
        // The other device reads the file from between this pause and this resume.
        a.s().pause_task(&t).unwrap();
        a.cycle();
        clock.advance(10);
        a.s().resume_task(&t).unwrap();
        clock.advance(10);
        b.cycle();
        clock.advance(10);
        b.cycle();
        clock.advance(10);
        a.cycle();

        let own_open = a.data().own_open_segments(&a.id());
        assert_eq!(own_open.len(), 1, "the timer here runs on");
        assert_eq!(a.data().tasks[&t].state, TaskState::Running);
        let seen = status_of(&b, root.path()).warnings;
        assert!(
            seen.iter()
                .any(|w| w.contains("task \"T renamed\" and the time on it")),
            "{seen:?}"
        );
        // That device still has the task and its timer as they last agreed.
        let there = b.data();
        assert_eq!(there.tasks[&t].state, TaskState::Running);
        assert_eq!(there.tasks[&t].name, "T");
        assert!(
            there
                .segments
                .values()
                .any(|s| s.task_id == t && s.end.is_none())
        );

        // When the date comes they agree.
        clock.set("2026-03-07T10:00:00Z");
        a.cycle();
        b.cycle();
        a.cycle();
        assert_eq!(a.data().tasks[&t], b.data().tasks[&t]);
        assert_eq!(a.data().segments, b.data().segments);
        assert_eq!(a.data().tasks[&t].state, TaskState::Running);
    }

    #[test]
    fn r1_a_task_that_cant_travel_keeps_its_time_with_it() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        let seg = manual_entry(&a, &t);
        a.s()
            .mutate(|f, _| {
                let task = f.data.tasks.get_mut(&t).unwrap();
                task.stopped_at = Some(Ts::parse("1999-01-01T00:00:00Z").unwrap());
                Ok(())
            })
            .unwrap();
        a.cycle();
        let written = file_on_disk(&a, root.path());
        assert!(written.data.projects.contains_key(&p));
        assert!(!written.data.tasks.contains_key(&t));
        assert!(
            !written.data.segments.contains_key(&seg),
            "not without its task"
        );
        check_device_file(&written, clock.now()).unwrap();
        assert!(
            a.data().segments.contains_key(&seg),
            "it stays on this device"
        );
        let warnings = status_of(&a, root.path()).warnings;
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("task \"T\" and the time on it")),
            "{warnings:?}"
        );
    }

    #[test]
    fn r1_a_delete_dated_ahead_says_so_here_and_arrives_when_the_date_comes() {
        let (here, there) = (ManualClock::at(T0), ManualClock::at(T0));
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&here, &[root.path()]),
            device(&there, &[root.path()]),
        );
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        manual_entry(&a, &t);
        a.cycle();
        b.cycle();
        assert_eq!(b.s().export_source().rows(chrono_tz::UTC).len(), 1);
        // This clock runs a week fast while the task is deleted, and is then put right.
        here.set("2026-03-08T09:00:00Z");
        a.s().delete_task(&t).unwrap();
        here.set("2026-03-01T09:10:00Z");
        there.set("2026-03-01T09:10:00Z");
        a.cycle();
        b.cycle();
        // The other device goes on counting it, so this one says which it is.
        assert_eq!(b.s().export_source().rows(chrono_tz::UTC).len(), 1);
        let own = status_of(&a, root.path()).warnings;
        assert!(
            own.iter()
                .any(|w| w.contains("task \"T\"") && w.contains("clock")),
            "{own:?}"
        );
        let seen = status_of(&b, root.path()).warnings;
        assert!(
            seen.iter().any(|w| w.contains("task \"T\"")
                && w.contains("ahead of this computer's clock")
                && !w.contains("damaged")),
            "{seen:?}"
        );
        here.set("2026-03-07T10:00:00Z");
        there.set("2026-03-07T10:00:00Z");
        a.cycle();
        b.cycle();
        assert!(b.s().export_source().rows(chrono_tz::UTC).is_empty());
        assert_eq!(status_of(&a, root.path()).warnings, Vec::<String>::new());
        assert_eq!(status_of(&b, root.path()).warnings, Vec::<String>::new());
    }

    #[test]
    fn r1_a_file_written_ahead_points_at_the_clocks() {
        let (here, there) = (ManualClock::at(T0), ManualClock::at(T0));
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&here, &[root.path()]),
            device(&there, &[root.path()]),
        );
        there.set("2026-03-08T09:00:00Z");
        b.s().create_project(&pinput("P")).unwrap();
        b.cycle();
        a.cycle();
        let seen = status_of(&a, root.path()).warnings;
        assert!(
            seen.iter().any(|w| w.contains("Skipped")
                && w.contains("ahead of this computer's clock")
                && w.contains("Check the clocks")),
            "{seen:?}"
        );
    }

    #[test]
    fn r1_a_date_no_clock_can_show_is_left_out_and_removing_it_ends_the_report() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        // A clock set a century out for one edit, and put right. That stamp can never be lowered.
        clock.set("2126-03-01T09:00:00Z");
        let renamed = crate::machine::TaskPatch {
            name: Some("T renamed".into()),
            ..Default::default()
        };
        a.s().patch_task(&t, &renamed, &Default::default()).unwrap();
        clock.set(T0);
        clock.advance(60);
        a.cycle();
        let written = file_on_disk(&a, root.path());
        assert!(written.data.projects.contains_key(&p));
        assert!(!written.data.tasks.contains_key(&t));
        // So the file still passes the strict check, and a device on an older version reads the rest.
        check_shared_is_clean(&written.data, clock.now(), 0);
        let warnings = status_of(&a, root.path()).warnings;
        assert!(
            warnings.iter().any(|w| w.contains("can't use")
                && w.contains("task \"T renamed\"")
                && w.ends_with("Correct or remove it here.")),
            "{warnings:?}"
        );
        a.s().delete_task(&t).unwrap();
        clock.advance(60);
        a.cycle();
        assert_eq!(status_of(&a, root.path()).warnings, Vec::<String>::new());
    }

    #[test]
    fn r1_what_a_warning_asks_for_can_be_done() {
        type Setup = fn(&Device, &str, &str);
        let cases: [(&str, &[&str], Setup); 4] = [
            // Correcting an entry keeps the date it was made with, so voiding is what ends this.
            (
                "Void it here and enter it again.",
                &["Correct", "Remove"],
                |d, _, t| {
                    bad_entry(d, t);
                },
            ),
            // Its date can't be changed, and a new price leaves this one where it is.
            ("Remove it here.", &["Correct", "Void"], |d, p, _| {
                poison_rate(d, p);
            }),
            // Nothing a person can reach: it says so by asking for nothing.
            (
                "everything else still syncs.",
                &["Correct", "Void", "Remove"],
                |d, _, t| {
                    let good = manual_entry(d, t);
                    bad_gap(d, &good);
                },
            ),
            (
                "Correct, void or remove what you can here.",
                &[],
                |d, p, t| {
                    bad_entry(d, t);
                    poison_rate(d, p);
                },
            ),
        ];
        for (i, (asks, never, setup)) in cases.into_iter().enumerate() {
            let clock = ManualClock::at(T0);
            let root = tempfile::tempdir().unwrap();
            let a = device(&clock, &[root.path()]);
            let p = a.s().create_project(&pinput("Acme")).unwrap();
            let t = a.s().create_task(&tinput("T", &p)).unwrap();
            setup(&a, &p, &t);
            a.cycle();
            let warnings = status_of(&a, root.path()).warnings;
            let told = warnings.iter().find(|w| w.contains("can't use"));
            let told = told.unwrap_or_else(|| panic!("case {i}: {warnings:?}"));
            assert!(told.ends_with(asks), "case {i}: {told}");
            for word in never {
                assert!(!told.contains(word), "case {i}: {told}");
            }
        }
    }

    #[test]
    fn r1_a_line_in_the_record_of_changes_no_device_takes_needs_nothing_done() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        a.s()
            .mutate(|f, _| {
                f.data.audit_log.insert(
                    "bad-line".to_owned(),
                    crate::model::AuditEntry {
                        id: "bad-line".to_owned(),
                        at: Ts::parse("1999-01-01T00:00:00Z").unwrap(),
                        by: "x".to_owned(),
                        entity: "x".to_owned(),
                        entity_id: "x".to_owned(),
                        action: "x".to_owned(),
                        reason: String::new(),
                        before: serde_json::Value::Null,
                        after: serde_json::Value::Null,
                    },
                );
                Ok(())
            })
            .unwrap();
        a.cycle();
        let status = status_of(&a, root.path());
        assert_eq!(status.warnings, Vec::<String>::new());
        let written = file_on_disk(&a, root.path());
        assert!(!written.data.audit_log.contains_key("bad-line"));
        check_device_file(&written, clock.now()).unwrap();
    }

    /// Nobody at the keyboard for a while, as the app sees it: a tick each minute.
    fn idle_for(d: &Device, clock: &ManualClock, mins: i64) {
        let mut idle = 0u64;
        for _ in 0..mins {
            clock.advance(60);
            idle += 60;
            d.s().observe_activity(Some(idle)).unwrap();
        }
    }

    fn unbilled(d: &Device, task: &str) -> i64 {
        d.s()
            .snapshot()
            .tasks
            .iter()
            .find(|x| x.id == task)
            .unwrap()
            .unbilled_secs
    }

    /// A timer left running here, and a device elsewhere that knows of it.
    fn left_running(clock: &ManualClock, root: &Path) -> (Device, Device, String) {
        let (a, b) = (device(clock, &[root]), device(clock, &[root]));
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        a.s().start_task(&t).unwrap();
        a.s().observe_activity(Some(0)).unwrap();
        a.cycle();
        b.cycle();
        (a, b, t)
    }

    #[test]
    fn r2_a_pause_asked_for_elsewhere_does_not_count_idle_time_as_work() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b, t) = left_running(&clock, root.path());
        idle_for(&a, &clock, 60);
        b.cycle();
        b.s().request_remote_pause(&t).unwrap();
        b.cycle();
        a.cycle();
        clock.advance(5);
        a.s().observe_activity(Some(5)).unwrap();
        let gaps = a.s().snapshot().gaps;
        assert_eq!(gaps.len(), 1, "the hour away is on record");
        assert_eq!(gaps[0].kind, crate::model::GapKind::Idle);
        assert!(
            unbilled(&a, &t) < 300,
            "an hour of nobody there is not work: {}",
            unbilled(&a, &t)
        );
        assert_eq!(a.data().own_open_segments(&a.id()).len(), 0);
    }

    #[test]
    fn r2_a_sync_before_the_first_tick_after_a_wake_does_not_count_sleep_as_work() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b, t) = left_running(&clock, root.path());
        clock.advance(2 * 3600);
        b.s().request_remote_pause(&t).unwrap();
        b.cycle();
        clock.advance(3600);
        a.cycle();
        a.s().observe_activity(Some(0)).unwrap();
        let gaps = a.s().snapshot().gaps;
        assert_eq!(gaps.len(), 1, "the sleep is on record");
        assert_eq!(gaps[0].kind, crate::model::GapKind::Sleep);
        assert_eq!(unbilled(&a, &t), 0);
        // It says what is at stake: the time up to the request, not the hour after it too.
        let seg = a.data().segments.values().next().unwrap().clone();
        assert_eq!(gaps[0].end, seg.end.unwrap());
        assert_eq!(
            a.s().snapshot().tasks[0].unconfirmed_secs,
            2 * 3600,
            "the segment ended when that was asked"
        );
    }

    #[test]
    fn r2_a_stop_elsewhere_during_a_lock_records_the_lock() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b, t) = left_running(&clock, root.path());
        a.s().observe_lock(Some(true)).unwrap();
        clock.advance(1800);
        a.s().observe_lock(Some(true)).unwrap();
        a.s().observe_activity(Some(1800)).unwrap();
        b.s().stop_task(&t).unwrap();
        b.cycle();
        a.cycle();
        let gaps = a.s().snapshot().gaps;
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].kind, crate::model::GapKind::Lock);
        assert_eq!(unbilled(&a, &t), 0);
        // The unlock finds nothing left to record, and the absence isn't left on file for a crash.
        clock.advance(5);
        a.s().observe_lock(Some(false)).unwrap();
        assert_eq!(a.s().snapshot().gaps.len(), 1);
        assert_eq!(a.s().file().prefs.away_since, None);
    }

    #[test]
    fn r2_a_delete_elsewhere_settles_too_and_the_gap_is_gone_with_the_task() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b, t) = left_running(&clock, root.path());
        idle_for(&a, &clock, 30);
        b.cycle();
        b.s().delete_task(&t).unwrap();
        b.cycle();
        a.cycle();
        assert_eq!(a.data().gaps.len(), 1);
        assert!(a.s().snapshot().gaps.is_empty());
        assert_eq!(a.data().own_open_segments(&a.id()).len(), 0);
    }

    #[test]
    fn r2_time_away_found_when_the_merged_save_fails_is_kept_for_the_next_save() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b, t) = left_running(&clock, root.path());
        idle_for(&a, &clock, 60);
        b.cycle();
        b.s().request_remote_pause(&t).unwrap();
        b.cycle();
        // Every save fails inside its transaction, as on a full disk, and not for long.
        a.s().set_saves_blocked(true);
        a.cycle();
        assert!(a.s().local_error().is_some());
        assert_eq!(a.data().own_open_segments(&a.id()).len(), 1);
        assert!(a.data().gaps.is_empty());
        a.s().set_saves_blocked(false);
        a.cycle();
        assert_eq!(a.s().local_error(), None);
        assert_eq!(a.data().own_open_segments(&a.id()).len(), 0);
        assert_eq!(a.s().snapshot().gaps.len(), 1);
        assert!(unbilled(&a, &t) < 300, "{}", unbilled(&a, &t));
    }

    #[test]
    fn r2_a_sync_that_closes_nothing_leaves_an_absence_in_progress_alone() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b, t) = left_running(&clock, root.path());
        idle_for(&a, &clock, 30);
        b.s().create_project(&pinput("Other")).unwrap();
        b.cycle();
        a.cycle();
        assert!(a.data().gaps.is_empty(), "still running, still away");
        assert!(a.s().file().prefs.away_since.is_some());
        assert_eq!(a.data().tasks[&t].state, TaskState::Running);
    }

    #[test]
    fn r13_a_timer_closed_by_a_stop_elsewhere_says_so() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b, t) = left_running(&clock, root.path());
        b.s().stop_task(&t).unwrap();
        b.cycle();
        let before = a.s().snapshot().notices.len();
        a.cycle();
        assert_eq!(a.data().own_open_segments(&a.id()).len(), 0);
        let told = a.s().snapshot().notices;
        assert_eq!(told.len(), before + 1, "{told:?}");
        assert!(
            told.iter()
                .any(|n| n.message.contains("\"T\" was stopped on pc")),
            "{told:?}"
        );
        assert_eq!(
            a.s().take_pause_alerts().len(),
            1,
            "for a hidden window too"
        );
        // Told once: a later cycle finds nothing left to close.
        clock.advance(60);
        a.cycle();
        assert_eq!(a.s().snapshot().notices.len(), before + 1);
    }

    #[test]
    fn r13_a_timer_closed_by_a_later_pause_elsewhere_says_so() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&clock, &[root.path()]),
            device(&clock, &[root.path()]),
        );
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        let other = a.s().create_task(&tinput("Other", &p)).unwrap();
        a.s().start_task(&t).unwrap();
        clock.advance(60);
        a.s().pause_task(&t).unwrap();
        a.cycle();
        b.cycle();
        b.s().start_task(&other).unwrap();
        clock.advance(60);
        b.s().pause_task(&other).unwrap();
        // Both resume it, neither knowing of the other, and the later pause stands.
        clock.advance(60);
        a.s().resume_task(&t).unwrap();
        clock.advance(10);
        b.s().resume_task(&t).unwrap();
        clock.advance(10);
        b.s().pause_task(&t).unwrap();
        b.cycle();
        a.cycle();
        assert_eq!(a.data().own_open_segments(&a.id()).len(), 0);
        let told = a.s().snapshot().notices;
        assert!(
            told.iter()
                .any(|n| n.message.contains("\"T\" was paused on pc")),
            "{told:?}"
        );
    }

    #[test]
    fn r13_a_pause_asked_for_or_a_delete_is_told_once_as_before() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b, t) = left_running(&clock, root.path());
        b.s().request_remote_pause(&t).unwrap();
        b.cycle();
        a.cycle();
        let told = a.s().snapshot().notices;
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].message.contains("asked for it"));

        let root = tempfile::tempdir().unwrap();
        let (a, b, t) = left_running(&clock, root.path());
        b.s().delete_task(&t).unwrap();
        b.cycle();
        a.cycle();
        let told = a.s().snapshot().notices;
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].message.contains("was deleted on"));
    }

    #[test]
    fn r10_a_later_rename_elsewhere_keeps_a_fixed_fee_and_a_budget() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&clock, &[root.path()]),
            device(&clock, &[root.path()]),
        );
        let p = a.s().create_project(&pinput("P")).unwrap();
        a.cycle();
        b.cycle();
        clock.advance(60);
        a.s()
            .update_project(
                &p,
                &ProjectInput {
                    billing_type: crate::model::BillingType::FixedFee,
                    fixed_fee_cents: Some(500_000),
                    fee_currency: Some("EUR".into()),
                    budget_hours: Some(40.0),
                    ..pinput("P")
                },
            )
            .unwrap();
        clock.advance(60);
        b.s().update_project(&p, &pinput("P renamed")).unwrap();
        a.cycle();
        b.cycle();
        a.cycle();
        for d in [&a, &b] {
            let got = d.data().projects[&p].clone();
            assert_eq!(got.name, "P renamed");
            assert_eq!(got.fixed_fee_cents, Some(500_000));
            assert_eq!(got.fee_currency.as_deref(), Some("EUR"));
            assert_eq!(got.billing_type, crate::model::BillingType::FixedFee);
            assert_eq!(got.budget_hours, Some(40.0));
        }
        assert_eq!(a.data().projects[&p], b.data().projects[&p]);
    }

    #[test]
    fn r10_a_file_from_a_build_without_the_clocks_is_read_and_merged() {
        let clock = ManualClock::at(T0);
        let root = tempfile::tempdir().unwrap();
        let a = device(&clock, &[root.path()]);
        let p = a.s().create_project(&pinput("P")).unwrap();
        a.s()
            .update_project(
                &p,
                &ProjectInput {
                    billing_type: crate::model::BillingType::FixedFee,
                    fixed_fee_cents: Some(500_000),
                    fee_currency: Some("EUR".into()),
                    ..pinput("P")
                },
            )
            .unwrap();
        a.cycle();
        // What this build writes, as that build reads and then writes it: it knows nothing of the clocks.
        let older = "123e4567-e89b-42d3-a456-426614174000";
        let mut file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(own_file(&a, root.path())).unwrap()).unwrap();
        file["deviceId"] = serde_json::json!(older);
        clock.advance(60);
        let later = crate::model::Stamp {
            at: Ts::parse(T0).unwrap().later(60),
            by: older.to_owned(),
        };
        let project = file["data"]["projects"][p.as_str()]
            .as_object_mut()
            .unwrap();
        for name in [
            "identityStamp",
            "billingStamp",
            "budgetStamp",
            "archiveStamp",
        ] {
            assert!(project.remove(name).is_some(), "{name} goes out");
        }
        project.insert("name".into(), serde_json::json!("Renamed there"));
        project.insert("stamp".into(), serde_json::to_value(&later).unwrap());
        std::fs::write(
            devices_dir(root.path()).join(format!("{older}.json")),
            serde_json::to_vec(&file).unwrap(),
        )
        .unwrap();
        clock.advance(60);
        a.cycle();
        let got = a.data().projects[&p].clone();
        assert_eq!(got.name, "Renamed there");
        assert_eq!(
            got.fixed_fee_cents,
            Some(500_000),
            "it held the fee as well"
        );
        let status = status_of(&a, root.path());
        assert!(status.warnings.is_empty(), "{:?}", status.warnings);
        assert_eq!(status.last_error, None);
    }

    #[test]
    fn r9_two_devices_that_bill_the_same_time_are_told_which_summaries_and_to_void_one() {
        let clock = ManualClock::at("2026-03-03T09:00:00Z");
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&clock, &[root.path()]),
            device(&clock, &[root.path()]),
        );
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        a.s()
            .add_manual_entry(&crate::adjust::ManualEntryInput {
                task_id: t,
                start: Ts::parse("2026-03-02T09:00:00Z").unwrap(),
                end: Ts::parse("2026-03-02T10:00:00Z").unwrap(),
                reason: "x".into(),
                subtask_id: None,
                allow_overlap: true,
            })
            .unwrap();
        a.cycle();
        b.cycle();
        a.s().set_device_prefix("A").unwrap();
        b.s().set_device_prefix("B").unwrap();
        let day = crate::summary::SummaryRequest {
            project_id: p,
            from: "2026-03-02".into(),
            to: "2026-03-02".into(),
            zone: "UTC".into(),
        };
        let from_a = a.s().create_summary(&day).unwrap();
        b.s().create_summary(&day).unwrap();
        let warned = |d: &Device| -> Vec<String> {
            d.s()
                .snapshot()
                .warnings
                .into_iter()
                .filter(|w| w.contains("billing summary holds the same time"))
                .collect()
        };
        assert!(warned(&a).is_empty(), "alone, a summary is the normal case");
        a.cycle();
        b.cycle();
        a.cycle();
        for d in [&a, &b] {
            let told = warned(d);
            assert_eq!(told.len(), 1, "{told:?}");
            assert!(told[0].contains("A-001, B-001"), "{told:?}");
            assert!(told[0].contains("Void one of them"), "{told:?}");
        }
        a.s().void_summary(&from_a).unwrap();
        a.cycle();
        b.cycle();
        for d in [&a, &b] {
            assert!(warned(d).is_empty());
        }
    }

    #[test]
    fn r6_a_pause_marked_where_the_summary_had_not_arrived_bills_only_what_is_left() {
        let clock = ManualClock::at("2026-03-03T09:00:00Z");
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&clock, &[root.path()]),
            device(&clock, &[root.path()]),
        );
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        a.s()
            .set_rate(&crate::machine::RateInput {
                project_id: p.clone(),
                rate_cents: Some(6000),
                currency: "EUR".into(),
                effective_from: Some("2026-01-01T00:00:00Z".into()),
                confirm_retroactive: true,
            })
            .unwrap();
        a.cycle();
        b.cycle();
        let ts = |x: &str| Ts::parse(x).unwrap();
        let pause = |from: &str, to: &str| crate::adjust::PauseInput {
            task_id: t.clone(),
            start: ts(from),
            end: ts(to),
            note: String::new(),
        };
        let day = crate::summary::SummaryRequest {
            project_id: p.clone(),
            from: "2026-03-02".into(),
            to: "2026-03-02".into(),
            zone: "UTC".into(),
        };
        a.s()
            .mark_billable_pause(&pause("2026-03-02T10:00:00Z", "2026-03-02T11:00:00Z"))
            .unwrap();
        a.s().create_summary(&day).unwrap();
        // Here it looks free, so nothing can refuse it.
        b.s()
            .mark_billable_pause(&pause("2026-03-02T09:30:00Z", "2026-03-02T10:30:00Z"))
            .unwrap();
        a.cycle();
        b.cycle();
        a.cycle();
        for d in [&a, &b] {
            assert_eq!(d.s().summary_preview(&day).unwrap().billable_secs, 1800);
        }
        b.s().create_summary(&day).unwrap();
        b.cycle();
        a.cycle();
        let billed: i64 = a
            .data()
            .summaries
            .values()
            .flat_map(|x| x.rows.iter().map(|r| r.billable_secs))
            .sum();
        assert_eq!(billed, 5400, "1.5 h of real time");
        for d in [&a, &b] {
            assert!(told_short(d).is_empty(), "each second is billed once");
        }
    }

    /// The warning for time that reached two summaries under different ids.
    fn told_short(d: &Device) -> Vec<String> {
        d.s()
            .snapshot()
            .warnings
            .into_iter()
            .filter(|w| w.contains("could be billed twice"))
            .collect()
    }

    #[test]
    fn r6_an_entry_made_where_the_summary_had_not_arrived_is_told_before_it_can_bill_twice() {
        let clock = ManualClock::at("2026-03-03T09:00:00Z");
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&clock, &[root.path()]),
            device(&clock, &[root.path()]),
        );
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        a.cycle();
        b.cycle();
        a.s().set_device_prefix("A").unwrap();
        b.s().set_device_prefix("B").unwrap();
        let ts = |x: &str| Ts::parse(x).unwrap();
        let day = crate::summary::SummaryRequest {
            project_id: p,
            from: "2026-03-02".into(),
            to: "2026-03-02".into(),
            zone: "UTC".into(),
        };
        a.s()
            .mark_billable_pause(&crate::adjust::PauseInput {
                task_id: t.clone(),
                start: ts("2026-03-02T10:00:00Z"),
                end: ts("2026-03-02T11:00:00Z"),
                note: String::new(),
            })
            .unwrap();
        let on_a = a.s().create_summary(&day).unwrap();
        assert!(told_short(&a).is_empty(), "nothing is wrong yet");
        b.s()
            .add_manual_entry(&crate::adjust::ManualEntryInput {
                task_id: t,
                start: ts("2026-03-02T10:00:00Z"),
                end: ts("2026-03-02T11:00:00Z"),
                reason: "x".into(),
                subtask_id: None,
                allow_overlap: false,
            })
            .unwrap();
        a.cycle();
        b.cycle();
        a.cycle();
        for d in [&a, &b] {
            let told = told_short(d);
            assert_eq!(told.len(), 1, "{told:?}");
            assert!(told[0].contains("A-001"), "{told:?}");
        }
        // Voiding what it names ends it, and the time then bills once.
        a.s().void_summary(&on_a).unwrap();
        a.cycle();
        b.cycle();
        assert!(told_short(&a).is_empty());
        b.s().create_summary(&day).unwrap();
        b.cycle();
        a.cycle();
        let live: i64 = a
            .data()
            .summaries
            .values()
            .filter(|x| !x.voided)
            .flat_map(|x| x.rows.iter().map(|r| r.billable_secs))
            .sum();
        assert_eq!(live, 3600);
        assert!(told_short(&a).is_empty());
    }

    #[test]
    fn r6_two_overlapping_pauses_each_billed_before_the_files_crossed_are_told() {
        let clock = ManualClock::at("2026-03-03T09:00:00Z");
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&clock, &[root.path()]),
            device(&clock, &[root.path()]),
        );
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        a.cycle();
        b.cycle();
        a.s().set_device_prefix("A").unwrap();
        b.s().set_device_prefix("B").unwrap();
        let ts = |x: &str| Ts::parse(x).unwrap();
        let day = crate::summary::SummaryRequest {
            project_id: p,
            from: "2026-03-02".into(),
            to: "2026-03-02".into(),
            zone: "UTC".into(),
        };
        let pause = crate::adjust::PauseInput {
            task_id: t,
            start: ts("2026-03-02T10:00:00Z"),
            end: ts("2026-03-02T11:00:00Z"),
            note: String::new(),
        };
        a.s().mark_billable_pause(&pause).unwrap();
        a.s().create_summary(&day).unwrap();
        clock.advance(60);
        b.s().mark_billable_pause(&pause).unwrap();
        let on_b = b.s().create_summary(&day).unwrap();
        a.cycle();
        b.cycle();
        a.cycle();
        for d in [&a, &b] {
            let told = told_short(d);
            assert_eq!(told.len(), 1, "{told:?}");
            assert!(told[0].contains("B-001"), "the later one: {told:?}");
        }
        // Each device's first look at the two gives the same answer, and voiding the one named ends it.
        b.s().void_summary(&on_b).unwrap();
        b.cycle();
        a.cycle();
        for d in [&a, &b] {
            assert!(told_short(d).is_empty());
        }
    }

    #[test]
    fn r4_a_stop_elsewhere_right_after_the_clock_went_back_keeps_the_work() {
        let here = ManualClock::at("2026-03-02T10:00:00Z");
        let there = ManualClock::at("2026-03-02T09:00:00Z");
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            device(&here, &[root.path()]),
            device(&there, &[root.path()]),
        );
        let p = a.s().create_project(&pinput("P")).unwrap();
        let t = a.s().create_task(&tinput("T", &p)).unwrap();
        a.s().start_task(&t).unwrap();
        a.s().observe_activity(Some(0)).unwrap();
        for _ in 0..5 {
            here.advance(60);
            a.s().observe_activity(Some(0)).unwrap();
        }
        a.cycle();
        b.cycle();
        b.s().stop_task(&t).unwrap();
        b.cycle();
        // The sync gets here before the tick that would have found the new time.
        here.set("2026-03-02T09:10:00Z");
        a.cycle();
        let (worked, told) = {
            let s = a.s();
            let snap = s.snapshot();
            let worked = snap
                .tasks
                .iter()
                .find(|x| x.id == t)
                .map(|x| x.unbilled_secs);
            (worked, snap.notices)
        };
        assert_eq!(worked, Some(5 * 60), "{told:?}");
        assert!(a.data().own_open_segments(&a.id()).is_empty());
        assert!(
            told.iter().any(|n| n.message.contains("clock went back")),
            "{told:?}"
        );
    }

    #[test]
    fn a_file_is_removed_only_where_the_folder_can_remove_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.json.gz");
        std::fs::write(&path, b"x").unwrap();
        let fake = FailsWriting(String::new(), Arc::new(AtomicU32::new(0)));
        assert_eq!(
            fake.remove_file(&path).unwrap_err().kind(),
            ErrorKind::Unsupported
        );
        assert!(path.exists(), "a folder that can't remove keeps the file");
        Arc::new(RealIo).remove_file(&path).unwrap();
        assert!(!path.exists());
        assert_eq!(
            RealIo.remove_file(&path).unwrap_err().kind(),
            ErrorKind::NotFound
        );
    }

    #[test]
    fn a_folders_status_reads_as_the_page_expects() {
        let v = serde_json::to_value(TargetStatus::default()).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "lastWrittenAt": null,
                "lastReadAt": null,
                "lastError": null,
                "lastErrorAt": null,
                "warnings": [],
                "ignoredFiles": 0,
                "devicesSeen": 0,
                "lastLeaseAt": null,
                "leaseError": null,
                "lastVersionAt": null,
                "lastVersionError": null,
                "versionsBytes": null,
                "lagSecs": null,
            })
        );
    }

    /// Restores, the fence, and folders with a restore in their past.
    mod restores {
        use super::*;
        use crate::epoch;
        use crate::model::{EpochInfo, GapKind, PauseReason, VersionReason};
        use crate::service::RestoreNote;
        use serde_json::{Value, json};
        use std::sync::atomic::AtomicBool;

        const VERSION: &str = "20260301T090000Z--a1b2c3d4--auto";

        fn root_of(p: &Path) -> String {
            p.to_string_lossy().into_owned()
        }

        fn has(d: &Device, name: &str) -> bool {
            d.data()
                .projects
                .values()
                .any(|p| p.name == name && !p.deleted)
        }

        fn key_on_disk(d: &Device, root: &Path) -> Option<u64> {
            file_on_disk(d, root).epoch_info.map(|e| e.epoch)
        }

        fn notices(d: &Device) -> Vec<String> {
            d.s()
                .snapshot()
                .notices
                .into_iter()
                .map(|n| n.message)
                .collect()
        }

        /// A restore made on `d` now, of the version taken at `T0`.
        fn restored_by(d: &Device, epoch: u64) -> EpochInfo {
            let s = d.s();
            EpochInfo {
                epoch,
                by: s.file().device_id.clone(),
                at: s.now(),
                restored_from: VERSION.to_owned(),
                base_taken_at: Ts::parse(T0).unwrap(),
            }
        }

        fn install(s: &mut Service, data: Shared, info: &EpochInfo) {
            s.install_restored(
                data,
                RestoreNote {
                    entity: "version",
                    name: info.restored_from.clone(),
                    after: Value::Null,
                    closed_at: info.base_taken_at,
                    reason: PauseReason::BackupRestored,
                    last_heartbeat: None,
                    epoch: Some(info.clone()),
                },
            )
            .unwrap();
        }

        /// `d` installs `data` at `info`, in one save, as restoring a version does.
        fn restore_to(d: &Device, data: Shared, info: &EpochInfo) {
            install(&mut d.s(), data, info);
        }

        fn only(d: &Device, names: &[&str]) -> Shared {
            let mut data = d.data();
            data.projects
                .retain(|_, p| names.contains(&p.name.as_str()));
            data
        }

        #[test]
        fn a_failing_folder_is_told_on_its_own_and_the_other_is_still_written_and_read() {
            let clock = ManualClock::at(T0);
            let (good, full) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let a = device(&clock, &[full.path(), good.path()]);
            let b = device(&clock, &[good.path()]);
            b.s().create_project(&pinput("From B")).unwrap();
            b.cycle();
            a.s().create_project(&pinput("From A")).unwrap();
            let io = FailsWriting(root_of(full.path()), Arc::new(AtomicU32::new(0)));
            run_cycle(&a.svc, &io);
            assert!(has(&a, "From B"), "read from the folder that works");
            assert!(own_file(&a, good.path()).exists());
            assert!(!own_file(&a, full.path()).exists());
            b.cycle();
            assert!(has(&b, "From A"), "and written to it");
            let (ok, ko) = (status_of(&a, good.path()), status_of(&a, full.path()));
            assert!(ok.last_error.is_none() && ok.last_written_at.is_some());
            assert!(ok.last_read_at.is_some());
            assert_eq!(ok.devices_seen, 1);
            assert_eq!(
                ko.last_error.as_deref(),
                Some("Could not write the file: disk full")
            );
            assert!(ko.last_written_at.is_none());
            assert!(ko.last_read_at.is_some(), "it could still be read");
            assert!(a.s().any_target_failed());

            // Once it has gone on a while, the warning shown everywhere names that folder, and the other as safe.
            clock.advance(31 * 60);
            run_cycle(&a.svc, &io);
            let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
            let warnings = a.s().snapshot().warnings;
            let told: Vec<&String> = warnings
                .iter()
                .filter(|w| w.contains("hasn't been written"))
                .collect();
            assert_eq!(
                told,
                vec![&format!(
                    "{} folder hasn't been written since 31 min ago: Could not write the file: disk full. {} folder is fine, so your data is safe there.",
                    name(full.path()),
                    name(good.path())
                )]
            );
        }

        #[test]
        fn a_folder_well_behind_the_other_says_by_how_much() {
            let clock = ManualClock::at(T0);
            let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let a = device(&clock, &[one.path(), two.path()]);
            let b = device(&clock, &[one.path(), two.path()]);
            b.s().create_project(&pinput("Early")).unwrap();
            b.cycle();
            let first = std::fs::read(own_file(&b, two.path())).unwrap();
            clock.advance(2 * 3600);
            b.s().create_project(&pinput("Late")).unwrap();
            b.cycle();
            let second = std::fs::read(own_file(&b, two.path())).unwrap();
            // The second folder's cloud app is paused, so it still has the first copy.
            std::fs::write(own_file(&b, two.path()), &first).unwrap();
            a.cycle();
            assert!(has(&a, "Late"), "the fresher copy is what counts");
            assert_eq!(status_of(&a, two.path()).lag_secs, Some(2 * 3600));
            assert_eq!(status_of(&a, one.path()).lag_secs, None);
            // An hour behind is a cloud app at work, nothing to tell.
            clock.advance(3600);
            b.cycle();
            std::fs::write(own_file(&b, two.path()), &second).unwrap();
            a.cycle();
            assert_eq!(status_of(&a, two.path()).lag_secs, None);
        }

        #[test]
        fn a_conflict_copy_holding_a_later_restore_is_ignored_and_kept() {
            let clock = ManualClock::at(T0);
            let root = tempfile::tempdir().unwrap();
            let a = device(&clock, &[root.path()]);
            let b = device(&clock, &[root.path()]);
            b.s().create_project(&pinput("Real")).unwrap();
            b.cycle();
            a.cycle();
            let bytes = std::fs::read(own_file(&b, root.path())).unwrap();
            let mut copy: Value = serde_json::from_slice(&bytes).unwrap();
            copy["epochInfo"] = json!({"epoch": 9, "by": b.id(), "at": T0, "restoredFrom": VERSION, "baseTakenAt": T0});
            copy["data"]["projects"] = json!({});
            let dir = devices_dir(root.path());
            let copies = [
                format!("{} (1).json", b.id()),
                format!("{}-PC2.json", b.id()),
                format!("{} (conflicted copy).json", b.id()),
            ];
            for c in &copies {
                std::fs::write(dir.join(c), copy.to_string()).unwrap();
            }
            a.cycle();
            assert_eq!(a.s().file().prefs.epoch, None, "nothing followed");
            assert_eq!(a.s().file().prefs.join_pending, None);
            assert!(has(&a, "Real"));
            for c in &copies {
                assert!(dir.join(c).exists(), "{c} is never deleted");
            }
            assert_eq!(status_of(&a, root.path()).ignored_files, 3);
        }

        #[test]
        fn a_file_in_the_old_format_merges_until_a_restore_and_is_fenced_after_it() {
            let clock = ManualClock::at(T0);
            let root = tempfile::tempdir().unwrap();
            let a = device(&clock, &[root.path()]);
            let b = device(&clock, &[root.path()]);
            b.s().create_project(&pinput("Old")).unwrap();
            b.cycle();
            let raw: Value =
                serde_json::from_slice(&std::fs::read(own_file(&b, root.path())).unwrap()).unwrap();
            assert!(raw.get("epochInfo").is_none(), "as an older app writes it");
            a.cycle();
            assert!(has(&a, "Old"));
            let info = restored_by(&a, 1);
            restore_to(&a, a.data(), &info);
            a.cycle();
            assert_eq!(key_on_disk(&a, root.path()), Some(1));
            // B goes on as an older app would, knowing nothing of restores.
            clock.advance(60);
            b.s().create_project(&pinput("After")).unwrap();
            let old_app = b.s().device_file();
            std::fs::write(
                own_file(&b, root.path()),
                serde_json::to_vec(&old_app).unwrap(),
            )
            .unwrap();
            let seen = a.s().file().remote_devices[&b.id()].written_at;
            a.cycle();
            assert!(!has(&a, "After"));
            let status = status_of(&a, root.path());
            assert_eq!(
                status.warnings,
                [
                    "Skipped pc: it holds data from before a restored version. Update and sync that device."
                ]
            );
            assert_eq!(status.devices_seen, 0);
            assert_eq!(
                a.s().file().remote_devices[&b.id()].written_at,
                seen,
                "nothing is taken from it"
            );
            assert!(own_file(&b, root.path()).exists());
        }

        #[test]
        fn a_restore_holds_through_the_next_syncs_and_the_other_device_follows_it_keeping_copies() {
            let clock = ManualClock::at(T0);
            let root = tempfile::tempdir().unwrap();
            let a = device(&clock, &[root.path()]);
            let b = device(&clock, &[root.path()]);
            let p = a.s().create_project(&pinput("Kept")).unwrap();
            a.cycle();
            b.cycle();
            let version = a.data();
            clock.advance(600);
            // After the version, a project made and one deleted. Both reach B, and A's own file.
            a.s().create_project(&pinput("Rolled back")).unwrap();
            a.s().delete_project(&p).unwrap();
            a.cycle();
            b.cycle();
            assert!(has(&b, "Rolled back") && !has(&b, "Kept"));
            clock.advance(600);
            let info = restored_by(&a, 1);
            restore_to(&a, version, &info);
            a.cycle();
            assert!(
                has(&a, "Kept") && !has(&a, "Rolled back"),
                "nothing undid it"
            );
            let mine = file_on_disk(&a, root.path());
            assert_eq!(mine.epoch_info.as_ref(), Some(&info));
            assert!(!mine.data.projects.values().any(|x| x.name == "Rolled back"));

            let wanted = b.s().lease.want_claim;
            assert!(b.cycle(), "B's data changed");
            assert!(has(&b, "Kept") && !has(&b, "Rolled back"));
            assert_eq!(b.s().file().prefs.epoch.as_ref(), Some(&info));
            assert_eq!(
                b.s().lease.want_claim,
                wanted,
                "following isn't B's own change"
            );
            let safety = b
                .s()
                .list_backups()
                .unwrap()
                .into_iter()
                .find(|x| x.safety)
                .unwrap()
                .name;
            assert_eq!(
                notices(&b).last(),
                Some(&format!(
                    "pc restored the version from 2026-03-01 09:00. This computer now follows it. 1 record here that isn't in it is kept in {safety}. The data from before this is also being saved as a version in your sync folders."
                ))
            );
            let before = b.s().take_pre_adopt().unwrap();
            assert!(
                before
                    .data
                    .projects
                    .values()
                    .any(|x| x.name == "Rolled back")
            );
            assert_eq!(before.epoch_info, None);
            assert_eq!(b.s().take_version_due(), Some(VersionReason::PreAdopt));
            let restore = b
                .data()
                .audit_log
                .into_values()
                .find(|e| e.entity == "version" && e.by == b.id())
                .unwrap();
            assert_eq!(
                (restore.action.as_str(), restore.after),
                (
                    "restore",
                    json!({"safetyCopy": safety, "epoch": 1, "by": a.id()})
                )
            );
            // Every file follows it now, so nothing is skipped any more.
            b.cycle();
            a.cycle();
            assert_eq!(key_on_disk(&b, root.path()), Some(1));
            assert!(status_of(&a, root.path()).warnings.is_empty());
            assert!(has(&a, "Kept") && !has(&a, "Rolled back"));
        }

        #[derive(Clone)]
        struct RestoresMidRead {
            svc: Arc<Mutex<Service>>,
            other: String,
            done: Arc<AtomicBool>,
        }
        impl SyncIo for RestoresMidRead {
            fn is_dir(&self, p: &Path) -> bool {
                RealIo.is_dir(p)
            }
            fn create_dir_all(&self, p: &Path) -> io::Result<()> {
                RealIo.create_dir_all(p)
            }
            fn list(&self, p: &Path) -> io::Result<Vec<String>> {
                RealIo.list(p)
            }
            fn file_size(&self, p: &Path) -> io::Result<u64> {
                RealIo.file_size(p)
            }
            fn read(&self, p: &Path, max: u64) -> io::Result<Vec<u8>> {
                let bytes = RealIo.read(p, max)?;
                if p.to_string_lossy().contains(&self.other)
                    && !self.done.swap(true, Ordering::SeqCst)
                {
                    let mut s = self.svc.lock().unwrap();
                    let info = EpochInfo {
                        epoch: 1,
                        by: s.file().device_id.clone(),
                        at: s.now(),
                        restored_from: VERSION.to_owned(),
                        base_taken_at: Ts::parse(T0).unwrap(),
                    };
                    let data = s.file().data.clone();
                    install(&mut s, data, &info);
                }
                Ok(bytes)
            }
            fn write_atomic(&self, d: &Path, t: &Path, b: &[u8]) -> io::Result<()> {
                RealIo.write_atomic(d, t, b)
            }
            fn sleep(&self, _: Duration) {}
        }

        #[test]
        fn a_restore_made_while_the_folders_are_read_is_respected() {
            let clock = ManualClock::at(T0);
            let root = tempfile::tempdir().unwrap();
            let b = device(&clock, &[root.path()]);
            b.s().create_project(&pinput("From before")).unwrap();
            b.cycle();
            let dir = tempfile::tempdir().unwrap();
            let svc = Arc::new(Mutex::new(
                Service::open(dir.path(), Arc::new(clock.clone()), "pc").unwrap(),
            ));
            svc.lock()
                .unwrap()
                .add_sync_target(target(root.path()))
                .unwrap();
            let io = RestoresMidRead {
                svc: Arc::clone(&svc),
                other: b.id(),
                done: Arc::new(AtomicBool::new(false)),
            };
            run_cycle(&svc, &io);
            assert!(io.done.load(Ordering::SeqCst));
            let s = svc.lock().unwrap();
            assert!(
                !s.file()
                    .data
                    .projects
                    .values()
                    .any(|p| p.name == "From before")
            );
            assert_eq!(s.file().prefs.epoch.as_ref().map(|e| e.epoch), Some(1));
        }

        #[test]
        fn a_timer_running_across_a_restore_stops_and_keeps_the_time_since_with_its_time_away() {
            let clock = ManualClock::at(T0);
            let root = tempfile::tempdir().unwrap();
            let a = device(&clock, &[root.path()]);
            let b = device(&clock, &[root.path()]);
            let p = a.s().create_project(&pinput("P")).unwrap();
            a.cycle();
            b.cycle();
            let version = a.data();
            let t0 = Ts::parse(T0).unwrap();
            let mins = |m: i64| t0.later(m * 60);
            // B is awake and in use throughout, so the only time away is what is recorded below.
            let awake = |n: i64| {
                for _ in 0..n {
                    clock.advance(60);
                    b.s().observe_activity(Some(0)).unwrap();
                }
            };
            awake(10);
            let t = b.s().create_task(&tinput("Late task", &p)).unwrap();
            b.s().start_task(&t).unwrap();
            let seg = b.data().own_open_segments(&b.id())[0].clone();
            awake(60);
            let info = restored_by(&a, 1);
            restore_to(&a, version, &info);
            a.cycle();
            awake(30);
            b.s()
                .mutate_system(|f, c| {
                    for (from, to) in [(20, 30), (60, 80), (85, 90)] {
                        f.data
                            .add_gap(c, &seg, mins(from), mins(to), GapKind::Sleep);
                    }
                    Ok(())
                })
                .unwrap();
            b.cycle();
            let data = b.data();
            assert_eq!(data.tasks[&t].state, TaskState::Paused);
            assert_eq!(
                data.tasks[&t].pause_reason,
                Some(PauseReason::SyncReconciled)
            );
            let kept = &data.segments[&seg];
            assert_eq!((kept.start, kept.end), (info.at, Some(mins(100))));
            let mut gaps: Vec<(Ts, Ts)> = data.gaps.values().map(|g| (g.start, g.end)).collect();
            gaps.sort();
            assert_eq!(gaps, [(mins(70), mins(80)), (mins(85), mins(90))]);
            assert_eq!(
                b.s().take_pause_alerts(),
                [
                    "\"Late task\" was running when pc restored a saved version, so its timer here has stopped. The time since that restore is kept."
                ]
            );
            let before = b.s().take_pre_adopt().unwrap();
            assert_eq!(
                before.data.segments[&seg].start,
                mins(10),
                "the copy has it all"
            );
        }

        #[test]
        fn a_timer_the_version_knew_of_keeps_its_whole_run() {
            let clock = ManualClock::at(T0);
            let root = tempfile::tempdir().unwrap();
            let a = device(&clock, &[root.path()]);
            let b = device(&clock, &[root.path()]);
            let p = a.s().create_project(&pinput("P")).unwrap();
            a.cycle();
            b.cycle();
            let t = b.s().create_task(&tinput("T", &p)).unwrap();
            b.s().start_task(&t).unwrap();
            let seg = b.data().own_open_segments(&b.id())[0].clone();
            b.cycle();
            a.cycle();
            clock.advance(30 * 60);
            let version = a.data();
            assert_eq!(version.segments[&seg].end, None);
            clock.advance(30 * 60);
            restore_to(&a, version, &restored_by(&a, 1));
            a.cycle();
            clock.advance(30 * 60);
            b.cycle();
            let data = b.data();
            let t0 = Ts::parse(T0).unwrap();
            assert_eq!(
                (data.segments[&seg].start, data.segments[&seg].end),
                (t0, Some(t0.later(90 * 60)))
            );
            assert_eq!(data.tasks[&t].state, TaskState::Paused);
        }

        #[test]
        fn a_restore_that_cant_be_saved_here_changes_nothing_and_that_folder_waits() {
            let clock = ManualClock::at(T0);
            let root = tempfile::tempdir().unwrap();
            let a = device(&clock, &[root.path()]);
            let b = device(&clock, &[root.path()]);
            let p = a.s().create_project(&pinput("P")).unwrap();
            a.cycle();
            b.cycle();
            let t = b.s().create_task(&tinput("T", &p)).unwrap();
            b.s().start_task(&t).unwrap();
            restore_to(&a, a.data(), &restored_by(&a, 1));
            a.cycle();
            let before = b.data();
            let on_disk = std::fs::read(own_file(&b, root.path())).unwrap();
            b.s().set_saves_blocked(true);
            assert!(!b.cycle());
            assert_eq!(b.data(), before, "the timer runs on");
            assert!(b.s().local_error().is_some());
            let status = status_of(&b, root.path());
            assert!(
                status.last_error.as_deref().is_some_and(|e| e.starts_with(
                    "This folder holds a restored version that couldn't be saved on this computer, so nothing was read from it or written to it. Will retry:"
                )),
                "{status:?}"
            );
            assert_eq!(
                std::fs::read(own_file(&b, root.path())).unwrap(),
                on_disk,
                "nothing written there"
            );
            b.s().set_saves_blocked(false);
            assert!(b.cycle());
            assert_eq!(b.s().file().prefs.epoch.as_ref().map(|e| e.epoch), Some(1));
            assert!(b.data().own_open_segments(&b.id()).is_empty());
        }

        #[test]
        fn an_empty_store_takes_on_a_restore_in_a_folder_it_connects() {
            let clock = ManualClock::at(T0);
            let root = tempfile::tempdir().unwrap();
            let a = device(&clock, &[root.path()]);
            let b = device(&clock, &[root.path()]);
            b.s().create_project(&pinput("From before")).unwrap();
            b.cycle();
            a.s().create_project(&pinput("Restored")).unwrap();
            a.cycle();
            restore_to(&a, only(&a, &["Restored"]), &restored_by(&a, 1));
            a.cycle();
            let c = device(&clock, &[root.path()]);
            c.cycle();
            assert!(has(&c, "Restored") && !has(&c, "From before"));
            assert_eq!(c.s().file().prefs.epoch.as_ref().map(|e| e.epoch), Some(1));
            assert_eq!(
                c.s().file().prefs.sync_joined,
                Some(vec![root_of(root.path())])
            );
            assert_eq!(c.s().file().prefs.join_pending, None);
            assert!(
                status_of(&c, root.path())
                    .warnings
                    .iter()
                    .any(|w| w.starts_with("Skipped pc: it holds data from before"))
            );
            assert_eq!(key_on_disk(&c, root.path()), Some(1));
        }

        struct Asked {
            a: Device,
            d: Device,
            restored: tempfile::TempDir,
            mine: tempfile::TempDir,
            info: EpochInfo,
        }

        /// A restored on its folder, and D with data of its own connecting that folder as a second one.
        fn asked(clock: &ManualClock) -> Asked {
            let (restored, mine) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let a = device(clock, &[restored.path()]);
            a.s().create_project(&pinput("Restored")).unwrap();
            a.cycle();
            let info = restored_by(&a, 1);
            restore_to(&a, a.data(), &info);
            a.cycle();
            let d = device(clock, &[mine.path()]);
            d.s().create_project(&pinput("Mine")).unwrap();
            d.cycle();
            d.s().add_sync_target(target(restored.path())).unwrap();
            d.cycle();
            Asked {
                a,
                d,
                restored,
                mine,
                info,
            }
        }

        #[test]
        fn a_store_with_data_asks_and_nothing_crosses_that_folder_until_it_follows() {
            let clock = ManualClock::at(T0);
            let Asked {
                a: _a,
                d,
                restored,
                mine,
                info,
            } = asked(&clock);
            let root = root_of(restored.path());
            let waiting = d.s().file().prefs.join_pending.clone().unwrap();
            assert_eq!((&waiting.root, &waiting.info), (&root, &info));
            assert!(!has(&d, "Restored"), "nothing read from it");
            assert!(
                !own_file(&d, restored.path()).exists(),
                "nothing written to it"
            );
            let status = status_of(&d, restored.path());
            assert_eq!(
                status.last_error.as_deref(),
                Some(
                    "This folder holds data restored from a saved version, and this computer has data from before that. Its data is not read or written until you choose what to do in Settings, under Sync between devices."
                )
            );
            assert!(status.last_written_at.is_none());
            let other = status_of(&d, mine.path());
            assert!(
                other.last_error.is_none() && other.last_written_at.is_some(),
                "the other folder goes on"
            );
            assert_eq!(
                d.s().snapshot().sync.join_pending.unwrap().device_name,
                "pc",
                "named, though nothing was taken from it"
            );
            let revision = d.s().revision();
            d.cycle();
            assert_eq!(d.s().revision(), revision, "asking again saves nothing");

            epoch::resolve_join(&d.svc, &RealIo, &root, true, TARGET_TIMEOUT).unwrap();
            assert!(has(&d, "Restored") && !has(&d, "Mine"));
            assert_eq!(d.s().file().prefs.epoch.as_ref(), Some(&info));
            assert_eq!(d.s().file().prefs.join_pending, None);
            assert!(
                d.s()
                    .file()
                    .prefs
                    .sync_joined
                    .as_ref()
                    .unwrap()
                    .contains(&root)
            );
            let before = d.s().take_pre_adopt().unwrap();
            assert!(before.data.projects.values().any(|p| p.name == "Mine"));
            d.cycle();
            assert_eq!(key_on_disk(&d, restored.path()), Some(1));
            assert_eq!(key_on_disk(&d, mine.path()), Some(1));
            assert!(status_of(&d, restored.path()).last_error.is_none());
            assert!(!has(&d, "Mine"), "its own older file doesn't bring it back");
        }

        #[test]
        fn keeping_both_merges_them_as_a_new_restore_the_others_follow() {
            let clock = ManualClock::at(T0);
            let Asked {
                a,
                d,
                restored,
                mine: _mine,
                info,
            } = asked(&clock);
            epoch::resolve_join(
                &d.svc,
                &RealIo,
                &root_of(restored.path()),
                false,
                TARGET_TIMEOUT,
            )
            .unwrap();
            assert!(has(&d, "Restored") && has(&d, "Mine"));
            let minted = d.s().file().prefs.epoch.clone().unwrap();
            assert_eq!((minted.epoch, minted.by.clone()), (2, d.id()));
            assert_eq!(minted.restored_from, info.restored_from);
            assert_eq!(d.s().file().prefs.join_pending, None);
            assert!(notices(&d).contains(&"Kept this computer's data together with the version restored on pc. Rolled-back items may return on every computer.".to_owned()));
            assert!(
                d.data()
                    .audit_log
                    .values()
                    .any(|e| e.action == "keepWithRestore" && e.after == json!({"epoch": 2}))
            );
            d.cycle();
            a.cycle();
            assert!(has(&a, "Mine") && has(&a, "Restored"));
            assert_eq!(a.s().file().prefs.epoch.as_ref(), Some(&minted));
        }

        #[test]
        fn an_answer_for_a_folder_not_waiting_or_one_that_changed_since_is_refused() {
            let clock = ManualClock::at(T0);
            let x = asked(&clock);
            let err = epoch::resolve_join(
                &x.d.svc,
                &RealIo,
                &root_of(x.mine.path()),
                true,
                TARGET_TIMEOUT,
            )
            .unwrap_err();
            assert_eq!(
                (err.code(), err.to_string()),
                (
                    "conflict",
                    "that folder is not waiting for a choice".to_owned()
                )
            );
            // A restores again before the answer.
            let again = restored_by(&x.a, 2);
            restore_to(&x.a, x.a.data(), &again);
            x.a.cycle();
            let root = root_of(x.restored.path());
            let err =
                epoch::resolve_join(&x.d.svc, &RealIo, &root, true, TARGET_TIMEOUT).unwrap_err();
            assert_eq!(
                err.to_string(),
                "That folder has changed since this was asked, so nothing was changed. Try again in a moment."
            );
            assert!(has(&x.d, "Mine"));
            assert_eq!(x.d.s().file().prefs.epoch, None);
            // The next sync asks about what is there now.
            x.d.cycle();
            assert_eq!(
                x.d.s()
                    .file()
                    .prefs
                    .join_pending
                    .as_ref()
                    .map(|w| w.info.epoch),
                Some(2)
            );
            assert_eq!(x.info.epoch, 1);
        }

        #[test]
        fn a_folder_with_nothing_in_it_yet_is_followed_only_once_it_shows_something() {
            let clock = ManualClock::at(T0);
            let (late, quiet, elsewhere) = (
                tempfile::tempdir().unwrap(),
                tempfile::tempdir().unwrap(),
                tempfile::tempdir().unwrap(),
            );
            let d = device(&clock, &[late.path(), quiet.path()]);
            d.s().create_project(&pinput("Mine")).unwrap();
            d.cycle();
            assert_eq!(
                d.s().file().prefs.sync_joined,
                Some(Vec::new()),
                "an empty read joins nothing"
            );
            // Its cloud app then brings in a restore made elsewhere. Had the empty read joined, the data here would be
            // replaced with no question asked.
            let a = device(&clock, &[elsewhere.path()]);
            a.s().create_project(&pinput("Restored")).unwrap();
            a.cycle();
            restore_to(&a, a.data(), &restored_by(&a, 1));
            a.cycle();
            std::fs::copy(own_file(&a, elsewhere.path()), own_file(&a, late.path())).unwrap();
            d.cycle();
            let waiting = d.s().file().prefs.join_pending.clone().unwrap();
            assert_eq!(waiting.root, root_of(late.path()));
            assert!(has(&d, "Mine") && !has(&d, "Restored"));
            // The other now holds this computer's own file, and is followed from here on.
            assert_eq!(
                d.s().file().prefs.sync_joined,
                Some(vec![root_of(quiet.path())])
            );
        }

        #[test]
        fn a_folder_at_the_same_restore_merges_and_is_followed_and_what_is_older_stays_out() {
            let clock = ManualClock::at(T0);
            let (shared, other) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let a = device(&clock, &[shared.path()]);
            let b = device(&clock, &[shared.path()]);
            b.s().create_project(&pinput("B, before")).unwrap();
            b.cycle();
            a.s().create_project(&pinput("A")).unwrap();
            a.cycle();
            let info = restored_by(&a, 1);
            restore_to(&a, only(&a, &["A"]), &info);
            a.cycle();
            // E follows the same restore, through another folder, and then connects this one.
            let e = device(&clock, &[other.path()]);
            e.s().create_project(&pinput("E")).unwrap();
            restore_to(&e, e.data(), &info);
            e.cycle();
            e.s().add_sync_target(target(shared.path())).unwrap();
            e.cycle();
            assert!(has(&e, "A") && has(&e, "E") && !has(&e, "B, before"));
            assert!(
                e.s()
                    .file()
                    .prefs
                    .sync_joined
                    .as_ref()
                    .unwrap()
                    .contains(&root_of(shared.path()))
            );
            assert_eq!(e.s().file().prefs.join_pending, None);
            assert!(
                status_of(&e, shared.path())
                    .warnings
                    .iter()
                    .any(|w| w.starts_with("Skipped pc: it holds data from before"))
            );
        }

        #[test]
        fn a_move_into_a_folder_past_a_restore_stops_and_asks_and_goes_through_once_followed() {
            let clock = ManualClock::at(T0);
            let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let a = device(&clock, &[new.path()]);
            a.s().create_project(&pinput("Restored")).unwrap();
            a.cycle();
            restore_to(&a, a.data(), &restored_by(&a, 1));
            a.cycle();
            let b = device(&clock, &[old.path()]);
            let d = device(&clock, &[old.path()]);
            b.s().create_project(&pinput("From B")).unwrap();
            b.cycle();
            d.s().create_project(&pinput("Mine")).unwrap();
            d.cycle();
            let err = mv(&d, old.path(), new.path()).unwrap_err();
            assert_eq!(
                err,
                "A folder in this move holds data restored from a saved version, and this computer has data from before that, so nothing was copied. Choose what to do in Settings, under Sync between devices, then try again."
            );
            assert_eq!(
                roots(&d),
                vec![root_of(old.path())],
                "the setting is unchanged"
            );
            assert_eq!(
                names(new.path()),
                vec![format!("{}.json", a.id())],
                "nothing copied"
            );
            assert!(has(&d, "Mine") && !has(&d, "Restored"));
            let waiting = d.s().file().prefs.join_pending.clone().unwrap();
            assert_eq!(waiting.root, root_of(new.path()));

            epoch::resolve_join(&d.svc, &RealIo, &waiting.root, true, TARGET_TIMEOUT).unwrap();
            let done = mv(&d, old.path(), new.path()).unwrap();
            assert_eq!(roots(&d), vec![root_of(new.path())]);
            let mut want = vec![format!("{}.json", a.id()), format!("{}.json", d.id())];
            want.sort();
            assert_eq!(names(new.path()), want, "what is from before stays behind");
            assert_eq!(done.files_copied, 0);
            assert!(
                done.warnings
                    .iter()
                    .any(|w| w.starts_with("Skipped pc: it holds data from before")),
                "{:?}",
                done.warnings
            );
            assert_eq!(key_on_disk(&d, new.path()), Some(1));
            assert!(has(&d, "Restored") && !has(&d, "From B"));
        }

        #[test]
        fn an_empty_store_moves_into_a_folder_past_a_restore_and_takes_it_on() {
            let clock = ManualClock::at(T0);
            let new = tempfile::tempdir().unwrap();
            let a = device(&clock, &[new.path()]);
            a.s().create_project(&pinput("Restored")).unwrap();
            a.cycle();
            restore_to(&a, a.data(), &restored_by(&a, 1));
            a.cycle();
            let e = device(&clock, &[]);
            move_target(
                &e.svc,
                &RealIo,
                None,
                &target(new.path()),
                None,
                &mut |_, _, _| {},
            )
            .unwrap();
            assert!(has(&e, "Restored"));
            assert_eq!(e.s().file().prefs.epoch.as_ref().map(|i| i.epoch), Some(1));
            assert_eq!(key_on_disk(&e, new.path()), Some(1));
        }

        #[test]
        fn a_move_waits_for_a_sync_when_a_followed_folder_has_a_restore_not_yet_taken_on() {
            let clock = ManualClock::at(T0);
            let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let a = device(&clock, &[old.path()]);
            let d = device(&clock, &[old.path()]);
            a.s().create_project(&pinput("A")).unwrap();
            a.cycle();
            d.cycle();
            restore_to(&a, a.data(), &restored_by(&a, 1));
            a.cycle();
            let err = mv(&d, old.path(), new.path()).unwrap_err();
            assert_eq!(
                err,
                "A restored version has just reached this computer, so nothing was copied. Sync now, then try again."
            );
            assert!(!devices_dir(new.path()).exists());
            d.cycle();
            assert!(mv(&d, old.path(), new.path()).is_ok());
            assert_eq!(key_on_disk(&d, new.path()), Some(1));
        }
    }
}
