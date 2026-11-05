//! Reading and writing lease records, and what follows from them on the service. See `lease` for the pure part.
//!
//! A pass reads every folder's `lease/` without the service lock, decides under it, writes without it, and then notes
//! what the writes did, so the lock is never held across I/O. Each folder's I/O runs under `bounded`, so a hung folder
//! holds up neither the others nor the sync: passes use the key `lease:<root>` and releases `lease-release:<root>`, as
//! `bounded` refuses a key in use. Passes run one at a time. A Held record planned before a release is never written
//! after it: each folder takes one writer at a time, and a release moves the ticket that Held writes are checked against.
//!
//! For the shell: on a lease thread, `run_tick` every 15 s and whenever data changes, `check_now` when the user asks for
//! a sync, as "Check again" does, and `release` when quitting, before the last sync. The take over dialog uses
//! `preview_take_over` and `take_over`, "Release now" uses `release_now`, and removing a folder runs `release_one` first.
//! None of these may be called with the service lock held.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::adjust::settled_end;
use crate::error::CoreError;
use crate::lease::{
    Claim, Evaluation, HolderInfo, Inputs, LEASE_BEAT_SECS, LEASE_FOLDER, LEASE_IDLE_RELEASE_SECS,
    LEASE_READ_SECS, LEASE_VERSION, LeaseRecord, LeaseRecordState, LeaseStateName, LostWhy,
    MAX_LEASE_FILE_BYTES, MAX_OPEN_SEGMENTS, above, at_ceiling, evaluate, freshest,
    next_generation, observe, parse_record,
};
use crate::model::{LeaseMemo, PauseReason, Segment, Task, VersionReason};
use crate::report::ExportSource;
use crate::service::{Service, folder_label};
use crate::sync::{
    APP_FOLDER, HungTargets, SyncIo, bounded, device_id_from_name, is_transient, read_at_most,
};
use crate::time::Ts;
use crate::validate::{MAX_NAME, MAX_SEGMENT_SECS};
use crate::view::{LeaseHolderView, Snapshot};

/// D2 (design section 15). Off, as the owner asked: take over only when the holder looks dead. When on, take over is also allowed from a holder that looks like
/// it is still in use, which the dialog offers behind stronger words. A computer left on at the office with a timer
/// running would otherwise keep this one read-only with no way out. False refuses that here, whatever the page sends.
/// It has to match `OFFER_TAKE_OVER_TO_A_LIVE_HOLDER` in `src/components/TakeOverDialog.tsx`: flip both together.
pub const OFFER_TAKE_OVER_TO_A_LIVE_HOLDER: bool = false;

/// What a record says about the app that wrote it. The core and the app share one version.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const TICK_KEY: &str = "lease";
const RELEASE_KEY: &str = "lease-release";
const RETRY_BASE_MS: u64 = 100;
const RETRY_ATTEMPTS: u32 = 5;
/// The memo of another holder is saved again this often while it goes on holding, so "last using it" stays near the
/// truth while no folder can be read, without rewriting the whole store at every read.
const MEMO_REFRESH_SECS: i64 = 3600;
/// How long a folder `release_one` let go of is kept from lease writes. The shell removes it straight after; one still
/// there after this is in use after all, and gets this device's record back at the next pass.
pub(crate) const LEAVING_SECS: i64 = 60;
const FOLDER_NOT_FOUND: &str =
    "Folder not found. Is the drive connected and the desktop client running?";
const QUITTING: &str = "the app is closing, so nothing was taken over";

/// Why no claim can be made here: a record at the highest generation, which only a forged one can be.
fn ceiling_note(device: Option<&str>) -> String {
    let file = device.map_or_else(
        || "the record with the highest number".to_owned(),
        |id| format!("{id}.json"),
    );
    format!(
        "A lease record in your sync folders has a number no claim can go above, so this computer can't hold the lease. Delete {file} in zWorkTracker/lease to fix this."
    )
}

/// The folder of lease records in a sync folder.
#[must_use]
pub fn lease_dir(root: &Path) -> PathBuf {
    root.join(APP_FOLDER).join(LEASE_FOLDER)
}

/// What a pass over the lease folders came to, for the shell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TickOutcome {
    /// Something the page shows changed, and the revision moved: tell the page.
    pub changed: bool,
    /// Another computer holds the lease now and a timer here may have stopped: show the pause alerts.
    pub lost: bool,
}

/// The holder a take over was shown. If it is no longer the top record, the answer is `changed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeOverExpected {
    pub generation: u64,
    pub beat_seq: u64,
}

/// Which words the dialog showed: a holder that looks inactive, one that looks in use, or no folder to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TakeOverMode {
    Stale,
    Live,
    Offline,
}

/// A task the holder shows as running, and where a take over would close it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeOverSegment {
    pub segment_id: String,
    pub task_name: String,
    pub end: Ts,
    pub secs: i64,
}

/// What the take over dialog shows, from a fresh read.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeOverPreview {
    pub holder: Option<LeaseHolderView>,
    /// The holder looks like it's still in use.
    pub live: bool,
    /// No folder could be read just now.
    pub unreachable: bool,
    pub segments: Vec<TakeOverSegment>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TakeOverOutcome {
    /// This computer holds the lease, or may work offline as chosen.
    Done,
    /// The holder isn't the one the dialog showed any more. Nothing was taken over.
    Changed,
    /// No folder could be read or written. Nothing changed.
    Unreachable,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeOverResult {
    pub outcome: TakeOverOutcome,
    /// The fresh view, only with `changed`.
    pub preview: Option<TakeOverPreview>,
    pub snapshot: Snapshot,
}

/// What a take over found of the holder taken over from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TakeOverEvidence {
    pub holder: HolderInfo,
    /// The beat this device's claim was written with.
    pub beat_seq: u64,
}

/// What one folder's `lease/` holds.
#[derive(Clone, Debug, Default)]
pub(crate) struct FolderRecords {
    pub records: Vec<LeaseRecord>,
    /// Files no reader takes: conflict copies, damage, a newer version. They are never deleted.
    pub ignored: usize,
    /// Records that couldn't be read just now, which says nothing about them.
    pub unread: Vec<String>,
}

/// What reading one folder came to, by root.
pub(crate) type FolderRead = Result<FolderRecords, String>;
/// What writing one folder came to, by root. `Ok(false)`: not written, as a release came after it was planned.
pub(crate) type FolderWrite = Result<bool, String>;

/// The order this device's records are written in.
#[derive(Debug, Default)]
pub(crate) struct Writers {
    /// Moved by every release. A Held record planned at an older ticket isn't written.
    ticket: AtomicU64,
    /// One writer at a time in each folder, so a release always lands after a Held write already under way.
    folders: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    /// One pass at a time, so each decides on what the last one did.
    pass: Mutex<()>,
}

impl Writers {
    fn ticket(&self) -> u64 {
        self.ticket.load(Ordering::SeqCst)
    }

    fn bump(&self) {
        self.ticket.fetch_add(1, Ordering::SeqCst);
    }

    fn folder(&self, root: &str) -> Arc<Mutex<()>> {
        let mut folders = self.folders.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(folders.entry(root.to_owned()).or_default())
    }
}

/// What this module keeps on the service, beside `LeaseState`.
#[derive(Debug, Default)]
pub(crate) struct LeaseIo {
    writers: Arc<Writers>,
    /// Files in each folder's `lease/` that no reader takes, as last read, by root.
    ignored: BTreeMap<String, usize>,
    /// The highest beat this device has written or seen of itself, so a new record always comes after an old copy.
    own_seq: u64,
    /// Folders `release_one` let go of, by when: no pass writes there until the folder is gone, or `LEAVING_SECS`
    /// have passed and it is still there, as when removing it failed.
    leaving: BTreeMap<String, Ts>,
    /// The Held record last planned, until what was written is taken in. It may have landed, so quitting releases it
    /// too, though no claim is held here yet.
    held_planned: Option<Claim>,
}

impl LeaseIo {
    /// The files in a folder's `lease/` that no reader takes, for the folder's count of ignored files.
    #[must_use]
    pub(crate) fn ignored(&self, root: &str) -> usize {
        self.ignored.get(root).copied().unwrap_or(0)
    }
}

/// What a pass needs from the service, taken under the lock.
pub(crate) struct LeasePlan {
    me: String,
    /// Every folder, in the user's order.
    roots: Vec<String>,
    /// The folders a record may go to: all but one waiting for the user's answer about a restore.
    writable: Vec<String>,
    hung: HungTargets,
    writers: Arc<Writers>,
    /// A first start waiting for its data from the folders: records are read, and none is written.
    awaiting: bool,
}

/// Why a record is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    /// A claim at the record's generation: from free, a reclaim, a step past a tie, or a take over.
    Claim {
        taking_over: Option<HolderInfo>,
    },
    Beat,
    /// No timer and no change for long enough.
    IdleRelease,
    /// Another device won a tie, which is said at once.
    TieRelease,
    /// Quitting, or "Release now".
    Release,
    /// Leaving one folder while holding on in the others.
    LeaveFolder,
}

/// A record about to be written.
pub(crate) struct Planned {
    pub(crate) intent: Intent,
    pub(crate) record: LeaseRecord,
    pub(crate) roots: Vec<String>,
    /// For a Held record, the ticket it was planned at.
    pub(crate) ticket: Option<u64>,
    /// The last change when it was planned.
    changed_at: Option<Ts>,
}

/// What a pass decided, for after the writes.
pub(crate) struct LeaseStep {
    pub(crate) write: Option<Planned>,
    pub(crate) lost: bool,
    before: Fingerprint,
}

/// What the page shows of the lease, to tell when it changed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    state: LeaseStateName,
    read_only: bool,
    holder: Option<(String, u64, u64, bool)>,
    claim: Option<u64>,
    offline: Option<u64>,
    note: Option<String>,
    reachable: Vec<String>,
    unreachable: Vec<String>,
}

fn with_retry<T>(io: &dyn SyncIo, mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut attempt = 0;
    loop {
        match op() {
            Err(e) if is_transient(&e) && attempt + 1 < RETRY_ATTEMPTS => {
                io.sleep(Duration::from_millis(RETRY_BASE_MS << attempt));
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// Reads every record in one folder. A folder that isn't there can't be reached; one with no `lease/` yet holds none.
pub(crate) fn read_folder(io: &dyn SyncIo, root: &Path) -> FolderRead {
    if !io.is_dir(root) {
        return Err(FOLDER_NOT_FOUND.to_owned());
    }
    let dir = lease_dir(root);
    let mut out = FolderRecords::default();
    if !io.is_dir(&dir) {
        return Ok(out);
    }
    let mut names = with_retry(io, || io.list(&dir))
        .map_err(|e| format!("Could not list the lease folder: {e}"))?;
    names.sort();
    for name in names {
        // This app's temporary files, which no reader takes either.
        if name.starts_with('.') {
            continue;
        }
        if device_id_from_name(&name).is_none() {
            out.ignored += 1;
            continue;
        }
        let path = dir.join(&name);
        let Ok(size) = io.file_size(&path) else {
            out.unread.push(name);
            continue;
        };
        if size > MAX_LEASE_FILE_BYTES as u64 {
            out.ignored += 1;
            continue;
        }
        match read_at_most(io, &path, MAX_LEASE_FILE_BYTES as u64) {
            Ok(bytes) => match parse_record(&name, &bytes) {
                Some(record) => out.records.push(record),
                None => out.ignored += 1,
            },
            // It grew past the limit after its size was taken.
            Err(e) if e.kind() == io::ErrorKind::FileTooLarge => out.ignored += 1,
            Err(_) => out.unread.push(name),
        }
    }
    Ok(out)
}

/// Writes this device's record to one folder. A Held record planned at an older ticket than now isn't written.
fn write_record(
    io: &dyn SyncIo,
    root: &str,
    me: &str,
    bytes: &[u8],
    writers: &Writers,
    ticket: Option<u64>,
) -> FolderWrite {
    let folder = writers.folder(root);
    let _one_writer = folder.lock().unwrap_or_else(PoisonError::into_inner);
    if ticket.is_some_and(|t| t != writers.ticket()) {
        return Ok(false);
    }
    let root = Path::new(root);
    if !io.is_dir(root) {
        return Err(FOLDER_NOT_FOUND.to_owned());
    }
    let dir = lease_dir(root);
    with_retry(io, || io.create_dir_all(&dir))
        .map_err(|e| format!("Could not create the lease folder: {e}"))?;
    let dest = dir.join(format!("{me}.json"));
    // A dot-prefixed temporary never matches a record's name, so readers ignore it.
    let tmp = dir.join(format!(".{me}.lease.tmp"));
    with_retry(io, || io.write_atomic(&dest, &tmp, bytes))
        .map_err(|e| format!("Could not write the lease record: {e}"))?;
    Ok(true)
}

pub(crate) fn read_all<I: SyncIo + Clone + 'static>(
    io: &I,
    plan: &LeasePlan,
    limit: Duration,
) -> Vec<(String, FolderRead)> {
    plan.roots
        .iter()
        .map(|root| {
            let (io, folder) = (io.clone(), root.clone());
            let read = bounded(
                &plan.hung,
                &format!("{TICK_KEY}:{root}"),
                limit,
                move || read_folder(&io, Path::new(&folder)),
            )
            .and_then(|r| r);
            (root.clone(), read)
        })
        .collect()
}

pub(crate) fn write_all<I: SyncIo + Clone + 'static>(
    io: &I,
    plan: &LeasePlan,
    planned: &Planned,
    limit: Duration,
    key: &str,
) -> Vec<(String, FolderWrite)> {
    // Not written, as with a release that came after a write was planned: nothing goes to a folder, not even `lease/`,
    // until this device's data has come from one.
    if plan.awaiting {
        return planned
            .roots
            .iter()
            .map(|r| (r.clone(), Ok(false)))
            .collect();
    }
    let bytes = match serde_json::to_vec(&planned.record) {
        Ok(b) => Arc::new(b),
        Err(e) => {
            let why = format!("Could not prepare the lease record: {e}");
            return planned
                .roots
                .iter()
                .map(|r| (r.clone(), Err(why.clone())))
                .collect();
        }
    };
    planned
        .roots
        .iter()
        .map(|root| {
            let (io, folder, me) = (io.clone(), root.clone(), plan.me.clone());
            let (data, writers, ticket) = (
                Arc::clone(&bytes),
                Arc::clone(&plan.writers),
                planned.ticket,
            );
            let wrote = bounded(&plan.hung, &format!("{key}:{root}"), limit, move || {
                write_record(&io, &folder, &me, &data, &writers, ticket)
            })
            .and_then(|r| r);
            (root.clone(), wrote)
        })
        .collect()
}

fn landed(wrote: &[(String, FolderWrite)]) -> bool {
    wrote.iter().any(|(_, w)| matches!(w, Ok(true)))
}

fn folder_error(read: &FolderRead, written: Option<&FolderWrite>) -> Option<String> {
    match (read, written) {
        (Err(e), _) | (_, Some(Err(e))) => Some(e.clone()),
        (Ok(folder), _) if !folder.unread.is_empty() => Some(format!(
            "Could not read {} right now, will retry.",
            folder.unread.join(", ")
        )),
        _ => None,
    }
}

/// The folders that could be read and may be written.
fn reachable_writable(plan: &LeasePlan, reads: &[(String, FolderRead)]) -> Vec<String> {
    reads
        .iter()
        .filter(|(root, read)| read.is_ok() && plan.writable.contains(root))
        .map(|(root, _)| root.clone())
        .collect()
}

/// A folder that may be written lacks this device's record as it stands: a new folder, a deleted file, or an old copy
/// put back by a cloud client. It gets the record at once rather than at the next beat.
fn record_missing(plan: &LeasePlan, reads: &[(String, FolderRead)], claim: &Claim) -> bool {
    reads
        .iter()
        .filter(|(root, _)| plan.writable.contains(root))
        .filter_map(|(_, read)| read.as_ref().ok())
        .any(|folder| {
            !folder.records.iter().any(|r| {
                r.device_id == plan.me
                    && r.state == LeaseRecordState::Held
                    && r.generation == claim.generation
                    && r.beat_seq >= claim.beat_seq
            })
        })
}

/// Where a take over closes one of the holder's open segments: what the reports count, or the holder's last lease beat
/// when that says this segment's timer was running on a clock that can be believed, and never past a pause request.
fn take_over_end(source: &ExportSource, holder: &HolderInfo, seg: &Segment, now: Ts) -> Ts {
    let known = source.end_of(seg);
    let asked = source.data.earliest_pause_request(&seg.id);
    let beat =
        (holder.open_segments.contains(&seg.id) && !holder.clock_suspect).then_some(holder.beat_at);
    settled_end(seg, known, beat, asked, now).min(seg.start.later(MAX_SEGMENT_SECS))
}

impl Service {
    /// What a pass needs, when one is due or `force`d. `None` with no folder.
    pub(crate) fn lease_plan(&mut self, force: bool) -> Option<LeasePlan> {
        if self.file.prefs.sync_targets.is_empty() || !(force || self.lease_due()) {
            return None;
        }
        let waiting = self
            .file
            .prefs
            .join_pending
            .as_ref()
            .map(|p| p.root.clone());
        let roots: Vec<String> = self
            .file
            .prefs
            .sync_targets
            .iter()
            .map(|t| t.root.clone())
            .collect();
        let now = self.now();
        self.lease_io
            .leaving
            .retain(|r, at| roots.contains(r) && now.secs() - at.secs() < LEAVING_SECS);
        let leaving = &self.lease_io.leaving;
        let writable = roots
            .iter()
            .filter(|r| Some(*r) != waiting.as_ref() && !leaving.contains_key(*r))
            .cloned()
            .collect();
        Some(LeasePlan {
            me: self.file.device_id.clone(),
            roots,
            writable,
            hung: self.hung_targets(),
            writers: Arc::clone(&self.lease_io.writers),
            awaiting: self.awaiting_cloud(),
        })
    }

    /// A read is due by the clock, or something can't wait for it: the first look, a change that needs a claim, a beat or
    /// a release.
    fn lease_due(&self) -> bool {
        let now = self.now();
        let l = &self.lease;
        let since = |t: Option<Ts>, secs: i64| t.is_none_or(|t| now.secs() - t.secs() >= secs);
        l.state == LeaseStateName::Checking
            || since(l.last_read_at, LEASE_READ_SECS)
            || (l.want_claim && l.claim.is_none())
            || (l.claim.is_some()
                && (since(l.last_beat_at, LEASE_BEAT_SECS) || self.lease_idle(now)))
    }

    fn timing_here(&self) -> bool {
        !self
            .file
            .data
            .own_open_segments(&self.file.device_id)
            .is_empty()
    }

    /// No timer here, and no change for long enough to let go.
    fn lease_idle(&self, now: Ts) -> bool {
        !self.timing_here()
            && self
                .lease
                .last_change_at
                .is_none_or(|t| now.secs() - t.secs() >= LEASE_IDLE_RELEASE_SECS)
    }

    fn lease_fingerprint(&self) -> Fingerprint {
        Fingerprint {
            state: self.lease.state,
            read_only: self.read_only(),
            holder: self.lease.holder.as_ref().map(|h| {
                (
                    h.device_id.clone(),
                    h.generation,
                    h.beat_seq,
                    h.clock_suspect,
                )
            }),
            claim: self.lease.claim.as_ref().map(|c| c.generation),
            offline: self.lease.offline_claim,
            note: self.lease.note.clone(),
            reachable: self.lease.reachable_roots.clone(),
            unreachable: self.lease.unreachable_roots.clone(),
        }
    }

    /// The next beat of this device's records, after any it has written or seen.
    fn next_seq(&mut self) -> u64 {
        let claimed = self.lease.claim.as_ref().map_or(0, |c| c.beat_seq);
        let seq = self.lease_io.own_seq.max(claimed).saturating_add(1);
        self.lease_io.own_seq = seq;
        seq
    }

    fn lease_record(
        &self,
        generation: u64,
        state: LeaseRecordState,
        acquired_at: Ts,
        beat_seq: u64,
        now: Ts,
    ) -> LeaseRecord {
        let open_segments = if state == LeaseRecordState::Held {
            let mut ids = self.file.data.own_open_segments(&self.file.device_id);
            ids.sort();
            ids.truncate(MAX_OPEN_SEGMENTS);
            ids
        } else {
            Vec::new()
        };
        LeaseRecord {
            lease_version: LEASE_VERSION,
            device_id: self.file.device_id.clone(),
            // A name no reader takes would hide the whole record.
            device_name: self
                .file
                .device_name
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_NAME)
                .collect(),
            app_version: APP_VERSION.to_owned(),
            generation,
            state,
            acquired_at,
            beat_at: now,
            beat_seq,
            open_segments,
        }
    }

    fn plan_write(
        &mut self,
        plan: &LeasePlan,
        roots: Vec<String>,
        intent: Intent,
        generation: u64,
        state: LeaseRecordState,
        acquired_at: Ts,
    ) -> Option<Planned> {
        if roots.is_empty() {
            return None;
        }
        let now = self.now();
        let seq = self.next_seq();
        let record = self.lease_record(generation, state, acquired_at, seq, now);
        if state == LeaseRecordState::Held {
            self.lease_io.held_planned = Some(Claim {
                generation,
                acquired_at,
                beat_seq: seq,
            });
        }
        Some(Planned {
            intent,
            record,
            roots,
            ticket: (state == LeaseRecordState::Held).then(|| plan.writers.ticket()),
            changed_at: self.lease.last_change_at,
        })
    }

    /// A line for the banner when some folders can be read and others can't.
    fn partly_reachable_note(
        &self,
        reachable: &[String],
        unreachable: &[String],
    ) -> Option<String> {
        if reachable.is_empty() || unreachable.is_empty() {
            return None;
        }
        let names = |roots: &[String]| {
            let labels: Vec<String> = roots
                .iter()
                .filter_map(|r| self.file.prefs.sync_targets.iter().find(|t| &t.root == r))
                .map(folder_label)
                .collect();
            let word = if labels.len() == 1 {
                "folder"
            } else {
                "folders"
            };
            format!("{} {word}", labels.join(" and "))
        };
        Some(format!(
            "The {} can't be read right now, so this computer goes by the {} alone.",
            names(unreachable),
            names(reachable)
        ))
    }

    /// Decides what a pass read comes to, under the lock. A device that has to stop is stopped here.
    pub(crate) fn lease_decide(
        &mut self,
        plan: &LeasePlan,
        reads: &[(String, FolderRead)],
    ) -> LeaseStep {
        let now = self.now();
        let before = self.lease_fingerprint();
        let mut records = Vec::new();
        let (mut reachable, mut unreachable) = (Vec::new(), Vec::new());
        for (root, read) in reads {
            match read {
                Ok(folder) => {
                    reachable.push(root.clone());
                    records.extend(folder.records.iter().cloned());
                    self.lease_io.ignored.insert(root.clone(), folder.ignored);
                }
                Err(_) => unreachable.push(root.clone()),
            }
        }
        self.lease.note = self.partly_reachable_note(&reachable, &unreachable);
        self.lease.reachable_roots = reachable;
        self.lease.unreachable_roots = unreachable;
        self.lease.last_read_at = Some(now);
        observe(&mut self.lease.observed, &records, now);
        let seen = records
            .iter()
            .filter(|r| r.device_id == plan.me)
            .map(|r| r.beat_seq)
            .max()
            .unwrap_or(0);
        self.lease_io.own_seq = self.lease_io.own_seq.max(seen);
        let timing = self.timing_here();
        let eval = evaluate(&Inputs {
            me: &plan.me,
            configured: true,
            reachable: !self.lease.reachable_roots.is_empty(),
            records: &records,
            claim: self.lease.claim.as_ref(),
            want_claim: self.lease.want_claim,
            open_own_segment: timing,
            last_change_at: self.lease.last_change_at,
            observed: &self.lease.observed,
            memo: self.file.prefs.lease_memo.as_ref(),
            now,
        });
        let mut step = LeaseStep {
            write: None,
            lost: false,
            before,
        };
        if self.decide_offline_claim(plan, reads, &records, &eval, now, &mut step) {
            return step;
        }
        let memo = self.file.prefs.lease_memo.clone();
        match eval {
            Evaluation::Off => self.lease.targets_changed(false),
            Evaluation::Unknown => {
                self.lease.state = LeaseStateName::Unknown;
                self.lease.holder = None;
            }
            Evaluation::Free { drop_claim } => {
                if drop_claim {
                    self.lease.claim = None;
                }
                self.lease.state = LeaseStateName::Free;
                self.lease.holder = None;
                if (self.lease.want_claim || timing) && !self.lease.closing {
                    match next_generation(&records, memo.as_ref(), self.lease.claim.as_ref()) {
                        Some(generation) => {
                            let claim = Intent::Claim { taking_over: None };
                            let roots = reachable_writable(plan, reads);
                            step.write = self.plan_write(
                                plan,
                                roots,
                                claim,
                                generation,
                                LeaseRecordState::Held,
                                now,
                            );
                        }
                        None => self.lease.note = Some(ceiling_note(at_ceiling(&records))),
                    }
                }
            }
            Evaluation::Reclaim { generation } => {
                self.lease.state = LeaseStateName::HeldByMe;
                self.lease.holder = None;
                if !self.lease.closing {
                    let claim = Intent::Claim { taking_over: None };
                    let roots = reachable_writable(plan, reads);
                    step.write = self.plan_write(
                        plan,
                        roots,
                        claim,
                        generation,
                        LeaseRecordState::Held,
                        now,
                    );
                }
            }
            Evaluation::HeldByMe { contended } => {
                self.lease.state = LeaseStateName::HeldByMe;
                self.lease.holder = None;
                self.lease.want_claim = false;
                if self.lease.claim.is_none() {
                    self.resume_claim(&plan.me, &records);
                }
                if let Some(claim) = self.lease.claim.clone()
                    && !self.lease.closing
                {
                    let roots = reachable_writable(plan, reads);
                    let beat_due = self
                        .lease
                        .last_beat_at
                        .is_none_or(|t| now.secs() - t.secs() >= LEASE_BEAT_SECS);
                    // Past a tie at once, which ends it, unless nothing can go above it. This device wins it then, and
                    // the other releases.
                    let past_tie = next_generation(&records, memo.as_ref(), Some(&claim))
                        .filter(|_| contended);
                    step.write = if let Some(generation) = past_tie {
                        let intent = Intent::Claim { taking_over: None };
                        self.plan_write(
                            plan,
                            roots,
                            intent,
                            generation,
                            LeaseRecordState::Held,
                            now,
                        )
                    } else if self.lease_idle(now) {
                        self.plan_write(
                            plan,
                            roots,
                            Intent::IdleRelease,
                            claim.generation,
                            LeaseRecordState::Released,
                            claim.acquired_at,
                        )
                    } else if beat_due || record_missing(plan, reads, &claim) {
                        self.plan_write(
                            plan,
                            roots,
                            Intent::Beat,
                            claim.generation,
                            LeaseRecordState::Held,
                            claim.acquired_at,
                        )
                    } else {
                        None
                    };
                }
            }
            Evaluation::Lost { by, why, live } => {
                let tie = (why == LostWhy::LostTie)
                    .then(|| self.lease.claim.clone())
                    .flatten();
                self.lease.claim = None;
                self.lease.want_claim = false;
                self.demote(&by, why, false);
                step.lost = true;
                self.show_holder(by, live, now);
                if let Some(c) = tie {
                    let roots = reachable_writable(plan, reads);
                    step.write = self.plan_write(
                        plan,
                        roots,
                        Intent::TieRelease,
                        c.generation,
                        LeaseRecordState::Released,
                        c.acquired_at,
                    );
                }
            }
            Evaluation::HeldByOther { holder, live } => {
                self.lease.want_claim = false;
                self.show_holder(holder, live, now);
            }
        }
        step
    }

    /// "Work here anyway" was chosen against the holder of generation `base`. Once a folder can be read again, that
    /// holder is taken over from, unless another took over in the meantime, or it is in use and D2 is off. True when
    /// this decided the pass.
    fn decide_offline_claim(
        &mut self,
        plan: &LeasePlan,
        reads: &[(String, FolderRead)],
        records: &[LeaseRecord],
        eval: &Evaluation,
        now: Ts,
        step: &mut LeaseStep,
    ) -> bool {
        let Some(base) = self.lease.offline_claim else {
            return false;
        };
        // Quitting: the lease was released, and nothing claims it again on the way out.
        if self.lease.closing {
            return true;
        }
        let (other, live) = match eval {
            // Still nothing to read: it stays as chosen.
            Evaluation::Unknown => return false,
            Evaluation::Lost { by, live, .. } => (by.clone(), *live),
            Evaluation::HeldByOther { holder, live } => (holder.clone(), *live),
            _ => {
                self.lease.offline_claim = None;
                return false;
            }
        };
        let in_use = live && !OFFER_TAKE_OVER_TO_A_LIVE_HOLDER;
        let next = next_generation(
            records,
            self.file.prefs.lease_memo.as_ref(),
            self.lease.claim.as_ref(),
        )
        .zip(above(base))
        .map(|(next, past)| next.max(past));
        if other.generation <= base
            && !in_use
            && let Some(generation) = next
        {
            let intent = Intent::Claim {
                taking_over: Some(other),
            };
            let roots = reachable_writable(plan, reads);
            step.write =
                self.plan_write(plan, roots, intent, generation, LeaseRecordState::Held, now);
            return true;
        }
        if next.is_none() {
            self.lease.note = Some(ceiling_note(at_ceiling(records)));
        }
        self.lease.offline_claim = None;
        self.lease.claim = None;
        self.lease.want_claim = false;
        let why = if other.generation <= base {
            LostWhy::NoClaim
        } else {
            LostWhy::Overtaken
        };
        self.demote(&other, why, true);
        step.lost = true;
        self.show_holder(other, live, now);
        true
    }

    /// After a crash or a restart this device's own record is still on top: it goes on holding at that generation, and
    /// the idle clock starts at the launch.
    fn resume_claim(&mut self, me: &str, records: &[LeaseRecord]) {
        let own = freshest(records)
            .into_iter()
            .find(|r| r.device_id == me && r.state == LeaseRecordState::Held);
        if let Some(r) = own {
            self.lease.claim = Some(Claim {
                generation: r.generation,
                acquired_at: r.acquired_at,
                beat_seq: r.beat_seq,
            });
            self.lease.last_change_at = self.lease.last_change_at.or(Some(self.opened));
        }
    }

    fn show_holder(&mut self, holder: HolderInfo, live: bool, now: Ts) {
        self.lease.state = if live {
            LeaseStateName::HeldByOtherLive
        } else {
            LeaseStateName::HeldByOtherStale
        };
        self.remember_holder(&holder, now);
        self.lease.holder = Some(holder);
    }

    /// The memo goes in the preferences, and every save rewrites the whole store, so it is saved only when the holder or
    /// its generation changes, and hourly while it goes on holding.
    fn remember_holder(&mut self, h: &HolderInfo, now: Ts) {
        let at = h.last_sign_at.min(now);
        let fresh = self.file.prefs.lease_memo.as_ref().is_some_and(|m| {
            m.device_id == h.device_id
                && m.generation == h.generation
                && at.secs() - m.at.secs() < MEMO_REFRESH_SECS
        });
        if !fresh {
            self.save_memo(LeaseMemo {
                device_id: h.device_id.clone(),
                device_name: h.device_name.clone(),
                generation: h.generation,
                at,
            });
        }
    }

    fn remember_claim(&mut self, generation: u64, now: Ts) {
        let me = self.file.device_id.clone();
        let same = self
            .file
            .prefs
            .lease_memo
            .as_ref()
            .is_some_and(|m| m.device_id == me && m.generation == generation);
        if !same {
            self.save_memo(LeaseMemo {
                device_id: me,
                device_name: self.file.device_name.clone(),
                generation,
                at: now,
            });
        }
    }

    fn save_memo(&mut self, memo: LeaseMemo) {
        // A memo that couldn't be saved is saved at the next change of holder, and meanwhile the run knows it.
        let _ = self.mutate_system(|f, _| {
            f.prefs.lease_memo = Some(memo);
            Ok(())
        });
    }

    /// What to call the other computer.
    fn holder_label(&self, h: &HolderInfo) -> String {
        Some(h.device_name.clone())
            .filter(|n| !n.trim().is_empty())
            .or_else(|| {
                self.file
                    .remote_devices
                    .get(&h.device_id)
                    .map(|d| d.name.clone())
                    .filter(|n| !n.trim().is_empty())
            })
            .unwrap_or_else(|| "Another computer".to_owned())
    }

    /// A time of day as the user reads it, in the zone they are viewing.
    fn clock_text(&self, at: Ts) -> String {
        let zone = self.viewing_zone();
        chrono::DateTime::from_timestamp(at.secs(), 0).map_or_else(
            || at.iso(),
            |t| t.with_timezone(&zone).format("%H:%M").to_string(),
        )
    }

    /// Notes what the writes did in each folder's status.
    fn note_writes(&mut self, wrote: &[(String, FolderWrite)], now: Ts) {
        for (root, w) in wrote {
            let status = self.target_status_mut(root);
            match w {
                Ok(true) => {
                    status.last_lease_at = Some(now);
                    status.lease_error = None;
                }
                Ok(false) => {}
                Err(e) => status.lease_error = Some(e.clone()),
            }
        }
    }

    /// Notes what a pass read and wrote, and takes on what landed.
    pub(crate) fn lease_apply(
        &mut self,
        reads: &[(String, FolderRead)],
        step: LeaseStep,
        wrote: &[(String, FolderWrite)],
    ) -> TickOutcome {
        let now = self.now();
        let mut changed = step.lost;
        for (root, read) in reads {
            let written = wrote.iter().find(|(r, _)| r == root).map(|(_, w)| w);
            let error = folder_error(read, written);
            let status = self.target_status_mut(root);
            if status.lease_error != error {
                status.lease_error = error;
                changed = true;
            }
            if matches!(written, Some(Ok(true))) {
                status.last_lease_at = Some(now);
                changed = true;
            }
        }
        if let Some(planned) = step.write {
            self.lease_landed(planned, wrote, now);
        }
        changed |= self.lease_fingerprint() != step.before;
        if changed {
            self.revision += 1;
        }
        TickOutcome {
            changed,
            lost: step.lost,
        }
    }

    /// Takes on a record that landed in at least one folder. A Held record planned before a release no longer counts.
    fn lease_landed(&mut self, planned: Planned, wrote: &[(String, FolderWrite)], now: Ts) {
        self.lease_io.held_planned = None;
        let current = planned
            .ticket
            .is_none_or(|t| t == self.lease_io.writers.ticket());
        if !landed(wrote) || !current {
            return;
        }
        let r = planned.record;
        match planned.intent {
            Intent::Claim {
                taking_over: Some(holder),
            } => {
                let evidence = TakeOverEvidence {
                    holder,
                    beat_seq: r.beat_seq,
                };
                if let Err(e) = self.finish_take_over(evidence, r.generation) {
                    self.local_error = Some(format!("Could not save taking over: {e}"));
                }
            }
            Intent::Claim { taking_over: None } => {
                self.lease.claim = Some(Claim {
                    generation: r.generation,
                    acquired_at: r.acquired_at,
                    beat_seq: r.beat_seq,
                });
                self.lease.state = LeaseStateName::HeldByMe;
                self.lease.holder = None;
                self.lease.want_claim = false;
                self.lease.last_beat_at = Some(now);
                self.remember_claim(r.generation, now);
            }
            Intent::Beat => {
                if let Some(c) = &mut self.lease.claim {
                    c.beat_seq = c.beat_seq.max(r.beat_seq);
                }
                self.lease.last_beat_at = Some(now);
            }
            Intent::IdleRelease | Intent::Release => {
                self.let_go();
                // A change made while this was being written still needs the lease.
                self.lease.want_claim = self.lease.last_change_at != planned.changed_at;
            }
            Intent::TieRelease | Intent::LeaveFolder => {}
        }
    }

    fn let_go(&mut self) {
        self.lease.claim = None;
        self.lease.state = LeaseStateName::Free;
        self.lease.holder = None;
        self.lease.last_beat_at = None;
    }

    /// This device has to stop changing data. Time away under way is settled first, a timer here closes now, the audit
    /// trail records it, and the user hears it, with a pause alert for a window that may be hidden. A save that fails is
    /// tried again at the next pass, as the timer is then still open.
    fn demote(&mut self, by: &HolderInfo, why: LostWhy, while_offline: bool) -> bool {
        if let Err(e) = self.settle_away() {
            self.local_error = Some(format!("Could not save data on handing over: {e}"));
            return false;
        }
        let data = &self.file.data;
        let mut tasks: Vec<String> = data
            .own_open_segments(&self.file.device_id)
            .iter()
            .filter_map(|id| data.segments.get(id))
            .map(|s| s.task_id.clone())
            .collect();
        tasks.sort();
        tasks.dedup();
        let saved = self.mutate_system(|f, c| {
            let names = f
                .data
                .close_own_open(c, c.now, &PauseReason::SyncReconciled);
            if !names.is_empty() {
                f.last_heartbeat = Some(c.now);
            }
            f.data.audit(
                c,
                "lease",
                &by.device_id,
                "leaseLost",
                "",
                Value::Null,
                json!({"by": by.device_name, "generation": by.generation, "closed": tasks}),
            );
            Ok(names)
        });
        let mut names = match saved {
            Ok(names) => names,
            Err(e) => {
                self.local_error = Some(format!("Could not save data on handing over: {e}"));
                return false;
            }
        };
        names.dedup();
        let message = self.lost_message(by, why, while_offline, &names);
        self.pause_alerts.push(message.clone());
        self.notify("warning", message);
        true
    }

    fn lost_message(
        &self,
        by: &HolderInfo,
        why: LostWhy,
        while_offline: bool,
        names: &[String],
    ) -> String {
        let name = self.holder_label(by);
        let first = match (why, while_offline) {
            (LostWhy::Overtaken, true) => format!(
                "{name} took over while this computer was offline, so this computer is now read-only."
            ),
            (LostWhy::Overtaken, false) => {
                format!("{name} took over, so this computer is now read-only.")
            }
            (LostWhy::LostTie, _) => format!(
                "{name} started making changes at the same time, so this computer is now read-only."
            ),
            (LostWhy::NoClaim, _) => {
                format!("{name} is using zWorkTracker, so this computer is now read-only.")
            }
        };
        let timer = if names.is_empty() {
            String::new()
        } else {
            let quoted: Vec<String> = names.iter().map(|n| format!("\"{n}\"")).collect();
            format!(
                " {} stopped timing here at {}.",
                quoted.join(", "),
                self.clock_text(self.now())
            )
        };
        let last = if while_offline {
            " What you did here is kept and will merge."
        } else {
            " Nothing was lost: it will sync."
        };
        format!("{first}{timer}{last}")
    }

    /// Takes the lease over from `evidence.holder`, now that this device's claim at `generation` is in at least one
    /// folder. The time the holder left running is closed at its last known moment, which the audit trail records, and
    /// the holder's own record replaces that when it comes back. Returns the segments closed.
    pub(crate) fn finish_take_over(
        &mut self,
        evidence: TakeOverEvidence,
        generation: u64,
    ) -> Result<Vec<String>, CoreError> {
        let now = self.now();
        let holder = evidence.holder;
        self.lease.claim = Some(Claim {
            generation,
            acquired_at: now,
            beat_seq: evidence.beat_seq,
        });
        self.lease.state = LeaseStateName::HeldByMe;
        self.lease.holder = None;
        self.lease.want_claim = false;
        self.lease.offline_claim = None;
        self.lease.last_change_at = Some(now);
        self.lease.last_beat_at = Some(now);
        let name = self.holder_label(&holder);
        let reason = format!(
            "Closed from {}: {name} stopped responding. Ended at its last known moment.",
            self.file.device_name
        );
        let source = self.end_source();
        let memo = LeaseMemo {
            device_id: self.file.device_id.clone(),
            device_name: self.file.device_name.clone(),
            generation,
            at: now,
        };
        let settled = self.mutate_system(|f, c| {
            f.prefs.lease_memo = Some(memo);
            let end_of = |seg: &Segment| take_over_end(&source, &holder, seg, c.now);
            Ok(f.data
                .settle_foreign_device(c, &holder.device_id, &end_of, &reason))
        })?;
        self.request_version(VersionReason::TakeOver);
        let closed = match settled.len() {
            0 => String::new(),
            1 => " 1 running task was closed at its last known time.".to_owned(),
            n => format!(" {n} running tasks were closed at their last known time."),
        };
        self.notify("info", format!("You took over from {name}.{closed}"));
        Ok(settled)
    }

    /// What the take over dialog shows, from what the last read found.
    #[must_use]
    pub fn take_over_preview(&self) -> TakeOverPreview {
        let other = matches!(
            self.lease.state,
            LeaseStateName::HeldByOtherLive | LeaseStateName::HeldByOtherStale
        );
        let segments = match &self.lease.holder {
            Some(h) if other => self.take_over_segments(h),
            _ => Vec::new(),
        };
        TakeOverPreview {
            holder: self.lease_view().holder,
            live: self.lease.state == LeaseStateName::HeldByOtherLive,
            unreachable: self.lease.state == LeaseStateName::Unknown,
            segments,
        }
    }

    fn take_over_segments(&self, h: &HolderInfo) -> Vec<TakeOverSegment> {
        let source = self.end_source();
        let now = self.now();
        let data = &self.file.data;
        data.segments
            .values()
            .filter(|s| s.device_id == h.device_id && s.is_open())
            .filter_map(|s| {
                let task = data.tasks.get(&s.task_id).filter(|t| Task::is_live(t))?;
                let end = take_over_end(&source, h, s, now);
                Some(TakeOverSegment {
                    segment_id: s.id.clone(),
                    task_name: task.name.clone(),
                    end,
                    secs: (end.secs() - s.start.secs()).max(0),
                })
            })
            .collect()
    }

    fn take_over_result(&self, outcome: TakeOverOutcome) -> TakeOverResult {
        TakeOverResult {
            outcome,
            preview: (outcome == TakeOverOutcome::Changed).then(|| self.take_over_preview()),
            snapshot: self.snapshot(),
        }
    }

    /// Checks a take over against what the last read found, and plans the claim.
    fn plan_take_over(
        &mut self,
        expected: TakeOverExpected,
        mode: TakeOverMode,
        offer_to_live: bool,
    ) -> Result<TakeOverStep, CoreError> {
        if self.lease.closing {
            return Err(CoreError::Conflict(QUITTING.into()));
        }
        let Some(plan) = self.lease_plan(true) else {
            return Err(CoreError::Conflict(
                "there is no sync folder, so nothing is held".into(),
            ));
        };
        let now = self.now();
        match self.lease.state {
            LeaseStateName::Unknown => {
                if mode != TakeOverMode::Offline {
                    return Ok(TakeOverStep::Unreachable);
                }
                let memo = self
                    .file
                    .prefs
                    .lease_memo
                    .as_ref()
                    .filter(|m| m.device_id != plan.me)
                    .map(|m| m.generation);
                if memo.is_some_and(|g| g != expected.generation) {
                    return Ok(TakeOverStep::Changed);
                }
                if self.read_only() {
                    // Only a claim for now: nothing is settled until a folder can be read again.
                    self.lease.offline_claim = Some(expected.generation);
                    self.revision += 1;
                }
                Ok(TakeOverStep::Done)
            }
            LeaseStateName::HeldByOtherLive | LeaseStateName::HeldByOtherStale => {
                let Some(holder) = self.lease.holder.clone() else {
                    return Ok(TakeOverStep::Changed);
                };
                let live = self.lease.state == LeaseStateName::HeldByOtherLive;
                let shown = (holder.generation, holder.beat_seq)
                    == (expected.generation, expected.beat_seq);
                // A holder that came back to life since the dialog showed it as inactive is shown again.
                if !shown || (live && mode == TakeOverMode::Stale) {
                    return Ok(TakeOverStep::Changed);
                }
                if live && !offer_to_live {
                    return Err(CoreError::Conflict(format!(
                        "{} looks like it's still in use. Take over is offered once it looks inactive.",
                        self.holder_label(&holder)
                    )));
                }
                let next = next_generation(
                    &[],
                    self.file.prefs.lease_memo.as_ref(),
                    self.lease.claim.as_ref(),
                )
                .zip(above(holder.generation))
                .map(|(next, past)| next.max(past));
                let Some(generation) = next else {
                    return Err(CoreError::Conflict(format!(
                        "The lease record of {} has a number no claim can go above, so taking over from it can't work. Delete {}.json in zWorkTracker/lease in your sync folders, then try again.",
                        self.holder_label(&holder),
                        holder.device_id
                    )));
                };
                let roots: Vec<String> = plan
                    .writable
                    .iter()
                    .filter(|r| self.lease.reachable_roots.contains(r))
                    .cloned()
                    .collect();
                let intent = Intent::Claim {
                    taking_over: Some(holder.clone()),
                };
                let planned = self.plan_write(
                    &plan,
                    roots,
                    intent,
                    generation,
                    LeaseRecordState::Held,
                    now,
                );
                Ok(planned.map_or(TakeOverStep::Unreachable, |p| {
                    TakeOverStep::Write(Box::new((plan, p, holder)))
                }))
            }
            _ => Ok(TakeOverStep::Changed),
        }
    }

    /// Plans a release of the claim, if this device holds one or a pass under way has written one it hasn't taken in
    /// yet, at the higher generation of the two. From here on no Held record planned before it is written, and with
    /// `closing` none at all.
    fn plan_release(&mut self, closing: bool) -> Option<(LeasePlan, Planned)> {
        if closing {
            self.lease.closing = true;
        }
        self.lease_io.writers.bump();
        let claim = self
            .lease
            .claim
            .iter()
            .chain(&self.lease_io.held_planned)
            .max_by_key(|c| c.generation)
            .cloned()?;
        let plan = self.lease_plan(true)?;
        let roots = plan.writable.clone();
        let planned = self.plan_write(
            &plan,
            roots,
            Intent::Release,
            claim.generation,
            LeaseRecordState::Released,
            claim.acquired_at,
        )?;
        Some((plan, planned))
    }

    fn released(&mut self, planned: Planned, wrote: &[(String, FolderWrite)]) -> bool {
        let now = self.now();
        self.note_writes(wrote, now);
        let done = landed(wrote);
        self.lease_landed(planned, wrote, now);
        self.revision += 1;
        done
    }

    /// Plans leaving one folder while holding: a beat in the others, then a release in that one, below that beat.
    /// Whether holding or not, no pass writes there from now on, so none puts a Held record back before it goes.
    fn plan_release_one(&mut self, root: &str) -> Option<OneFolder> {
        let now = self.now();
        self.lease_io.leaving.insert(root.to_owned(), now);
        let claim = self.lease.claim.clone()?;
        let plan = self.lease_plan(true)?;
        if !plan.roots.iter().any(|r| r == root) {
            return None;
        }
        let others: Vec<String> = plan
            .writable
            .iter()
            .filter(|r| *r != root)
            .cloned()
            .collect();
        if others.is_empty() {
            self.lease_io.writers.bump();
            let release = self.plan_write(
                &plan,
                vec![root.to_owned()],
                Intent::Release,
                claim.generation,
                LeaseRecordState::Released,
                claim.acquired_at,
            )?;
            return Some(OneFolder {
                plan,
                beat: None,
                release,
            });
        }
        let beat = self.plan_write(
            &plan,
            others,
            Intent::Beat,
            claim.generation,
            LeaseRecordState::Held,
            claim.acquired_at,
        );
        // Below the last beat written, so a device reading this folder and another goes by the Held record there,
        // and one reading only this folder finds it free.
        let record = self.lease_record(
            claim.generation,
            LeaseRecordState::Released,
            claim.acquired_at,
            claim.beat_seq.saturating_sub(1),
            now,
        );
        let release = Planned {
            intent: Intent::LeaveFolder,
            record,
            roots: vec![root.to_owned()],
            ticket: None,
            changed_at: self.lease.last_change_at,
        };
        Some(OneFolder {
            plan,
            beat,
            release,
        })
    }
}

/// A take over checked and planned.
enum TakeOverStep {
    Done,
    Changed,
    Unreachable,
    Write(Box<(LeasePlan, Planned, HolderInfo)>),
}

/// Leaving one folder while holding.
struct OneFolder {
    plan: LeasePlan,
    beat: Option<Planned>,
    release: Planned,
}

fn lock(svc: &Mutex<Service>) -> std::sync::MutexGuard<'_, Service> {
    svc.lock().unwrap_or_else(PoisonError::into_inner)
}

fn writers_of(svc: &Mutex<Service>) -> Arc<Writers> {
    Arc::clone(&lock(svc).lease_io.writers)
}

/// One pass: read, decide, write, note. The caller holds the pass lock.
fn pass<I: SyncIo + Clone + 'static>(
    svc: &Mutex<Service>,
    io: &I,
    limit: Duration,
    force: bool,
) -> TickOutcome {
    let Some(plan) = lock(svc).lease_plan(force) else {
        return TickOutcome::default();
    };
    let reads = read_all(io, &plan, limit);
    let step = lock(svc).lease_decide(&plan, &reads);
    let wrote = step
        .write
        .as_ref()
        .map(|p| write_all(io, &plan, p, limit, TICK_KEY))
        .unwrap_or_default();
    lock(svc).lease_apply(&reads, step, &wrote)
}

/// A pass when one is due: a read every minute, a beat every two while holding, and at once for a change that needs a
/// claim or the first look after opening. For the lease thread, every 15 s and whenever data changes.
pub fn run_tick<I: SyncIo + Clone + 'static>(
    svc: &Mutex<Service>,
    io: &I,
    limit: Duration,
) -> TickOutcome {
    let writers = writers_of(svc);
    let _one_pass = writers.pass.lock().unwrap_or_else(PoisonError::into_inner);
    pass(svc, io, limit, false)
}

/// A pass now, due or not: for "Sync now" and "Check again".
pub fn check_now<I: SyncIo + Clone + 'static>(
    svc: &Mutex<Service>,
    io: &I,
    limit: Duration,
) -> TickOutcome {
    let writers = writers_of(svc);
    let _one_pass = writers.pass.lock().unwrap_or_else(PoisonError::into_inner);
    pass(svc, io, limit, true)
}

/// Reads the folders again and says what a take over would do now.
pub fn preview_take_over<I: SyncIo + Clone + 'static>(
    svc: &Mutex<Service>,
    io: &I,
    limit: Duration,
) -> TakeOverPreview {
    let writers = writers_of(svc);
    let _one_pass = writers.pass.lock().unwrap_or_else(PoisonError::into_inner);
    pass(svc, io, limit, true);
    lock(svc).take_over_preview()
}

/// Takes the lease over from the holder the dialog showed, after reading the folders again. `changed` when that isn't
/// the holder any more, `unreachable` when no folder could be read or written: nothing changes then. With `offline`
/// and no folder to read, this computer only becomes writable, and the take over happens once a folder can be read,
/// if nobody else took over meanwhile.
pub fn take_over<I: SyncIo + Clone + 'static>(
    svc: &Mutex<Service>,
    io: &I,
    expected: TakeOverExpected,
    mode: TakeOverMode,
    limit: Duration,
) -> Result<TakeOverResult, CoreError> {
    take_over_offering(
        svc,
        io,
        expected,
        mode,
        limit,
        OFFER_TAKE_OVER_TO_A_LIVE_HOLDER,
    )
}

/// `take_over`, with D2 as a parameter so both answers can be tested.
pub(crate) fn take_over_offering<I: SyncIo + Clone + 'static>(
    svc: &Mutex<Service>,
    io: &I,
    expected: TakeOverExpected,
    mode: TakeOverMode,
    limit: Duration,
    offer_to_live: bool,
) -> Result<TakeOverResult, CoreError> {
    let writers = writers_of(svc);
    let _one_pass = writers.pass.lock().unwrap_or_else(PoisonError::into_inner);
    pass(svc, io, limit, true);
    let planned = lock(svc).plan_take_over(expected, mode, offer_to_live)?;
    let (plan, planned, holder) = match planned {
        TakeOverStep::Done => return Ok(lock(svc).take_over_result(TakeOverOutcome::Done)),
        TakeOverStep::Changed => {
            return Ok(lock(svc).take_over_result(TakeOverOutcome::Changed));
        }
        TakeOverStep::Unreachable => {
            return Ok(lock(svc).take_over_result(TakeOverOutcome::Unreachable));
        }
        TakeOverStep::Write(w) => *w,
    };
    let wrote = write_all(io, &plan, &planned, limit, TICK_KEY);
    let mut s = lock(svc);
    s.lease_io.held_planned = None;
    let now = s.now();
    s.note_writes(&wrote, now);
    s.revision += 1;
    let current = planned.ticket.is_none_or(|t| t == writers.ticket());
    if !landed(&wrote) || !current {
        return Ok(s.take_over_result(TakeOverOutcome::Unreachable));
    }
    let evidence = TakeOverEvidence {
        holder,
        beat_seq: planned.record.beat_seq,
    };
    s.finish_take_over(evidence, planned.record.generation)?;
    Ok(s.take_over_result(TakeOverOutcome::Done))
}

/// Quitting: from here on no Held record is written, and a lease held here is released in every folder that answers
/// within `limit`. A read-only device writes nothing. Doesn't wait for a pass under way, whose Held write can't land
/// after this one's release.
pub fn release<I: SyncIo + Clone + 'static>(svc: &Mutex<Service>, io: &I, limit: Duration) {
    let Some((plan, planned)) = lock(svc).plan_release(true) else {
        return;
    };
    let wrote = write_all(io, &plan, &planned, limit, RELEASE_KEY);
    lock(svc).released(planned, &wrote);
}

/// "Release now": this computer stops holding, until its next change. Refused while a timer runs here, as that
/// keeps it in use, and when no folder could be written.
pub fn release_now<I: SyncIo + Clone + 'static>(
    svc: &Mutex<Service>,
    io: &I,
    limit: Duration,
) -> Result<(), CoreError> {
    let writers = writers_of(svc);
    let _one_pass = writers.pass.lock().unwrap_or_else(PoisonError::into_inner);
    let job = {
        let mut s = lock(svc);
        if s.lease.claim.is_none() {
            return Ok(());
        }
        if s.timing_here() {
            return Err(CoreError::Conflict(
                "a task is running here, so this computer is still in use. Pause it first.".into(),
            ));
        }
        s.plan_release(false)
    };
    let Some((plan, planned)) = job else {
        return Ok(());
    };
    let wrote = write_all(io, &plan, &planned, limit, RELEASE_KEY);
    if lock(svc).released(planned, &wrote) {
        Ok(())
    } else {
        Err(CoreError::Conflict(
            "no sync folder could be written just now, so this computer still holds it. Try again when a folder can be reached.".into(),
        ))
    }
}

/// Before a folder is removed: a lease held here is released in that folder, so a device reading only it finds it
/// free, and beaten in the others first, so a device reading both still finds this one holding. Best effort; files
/// stay. Call it just before `Service::remove_sync_target`: from here on no pass writes to that folder for a minute,
/// and one still connected after that gets this device's record back.
pub fn release_one<I: SyncIo + Clone + 'static>(
    svc: &Mutex<Service>,
    io: &I,
    root: &str,
    limit: Duration,
) {
    let writers = writers_of(svc);
    let _one_pass = writers.pass.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(job) = lock(svc).plan_release_one(root) else {
        return;
    };
    let beat = job
        .beat
        .as_ref()
        .map(|p| write_all(io, &job.plan, p, limit, TICK_KEY))
        .unwrap_or_default();
    let release = write_all(io, &job.plan, &job.release, limit, RELEASE_KEY);
    let mut s = lock(svc);
    let now = s.now();
    s.note_writes(&beat, now);
    s.note_writes(&release, now);
    if let Some(p) = job.beat {
        s.lease_landed(p, &beat, now);
    } else {
        s.lease_landed(job.release, &release, now);
    }
    s.revision += 1;
}
