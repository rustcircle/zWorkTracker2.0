//! The application service: owns the store, applies each change transactionally and builds the view.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::adjust::{CorrectionInput, ManualEntryInput, PauseInput, Reclaimed};
use crate::backup::{self, BackupInfo};
use crate::catalog::{TagInput, TemplateInput};
use crate::colors;
use crate::detect::Detected;
use crate::drafts::{Draft, Kept, KeptBecause};
use crate::error::CoreError;
use crate::hlc::{self, CLOCK_BEHIND_WARN_SECS, HLC_MAX_LEAD_SECS};
use crate::idle::{ClockStep, IdleTracker, NewGap};
use crate::lease::{LOST_DEVICE_SILENCE_SECS, LeaseState, LeaseStateName};
use crate::machine::{
    Ctx, PatchOutcome, PauseRequestDue, ProjectInput, RateInput, RequiredField, RoundingInput,
    ShiftInput, TaskExpected, TaskInput, TaskPatch, Transition, cold_required, doc_has_text,
};
use crate::merge::merge;
use crate::model::{
    AuditEntry, DeviceFile, EpochInfo, GapDecision, GapKind, LocalFile, PauseReason, PendingCopy,
    RemoteDevice, SCHEMA_VERSION, Segment, Shared, Stamp, SyncProvider, SyncTarget, Task,
    TaskState, Theme, VersionReason,
};
use crate::persist::Paths;
use crate::pieces;
use crate::reminder::{self, Reminder};
use crate::report::ExportSource;
use crate::store::{Opened, SaveStats, Store, take_legacy_beat};
use crate::subtasks;
use crate::summary::{
    ProjectFigures, SummaryPreview, SummaryRequest, project_figures, task_totals,
};
use crate::sync::{APP_FOLDER, HungTargets, TargetStatus};
use crate::time::{Clock, Ts};
use crate::validate::MAX_SKEW_SECS;
use crate::view::{
    BillingView, ClockBehindView, CopySource, JoinPendingView, LaunchView, LeaseHolderView,
    LeaseView, Notice, PlatformView, PrefsView, ProjectView, RawSegmentView, ReminderSettings,
    SegmentView, SetupView, Snapshot, SubtaskView, SyncView, TargetView, TaskView,
};

/// How long after a launch an edit stamped by another device may still have been made without the words put back
/// at that launch. Each device stamps by its own clock, which can be off, and the words take time to get there. Too
/// long a time costs a notice that only needs dismissing, and too short a time costs the words.
const WITHOUT_WORDS_PUT_BACK_SECS: i64 = 3600;

/// A folder that has failed for this long is mentioned wherever the user is looking.
const FOLDER_FAILING_WARNING_SECS: i64 = 30 * 60;

/// Replicated settings that only choose what this device is looking at. A read-only device can still change them.
const VIEW_SETTINGS: [&str; 2] = ["activeZone", "showBoth"];

pub struct Service {
    pub(crate) paths: Paths,
    /// The database `file` was loaded from, and is saved to by every change.
    store: Store,
    /// The heartbeat the store held at this launch, not cut to now: for a run that began after now.
    opened_beat: Option<Ts>,
    pub(crate) file: LocalFile,
    clock: Arc<dyn Clock>,
    notices: Vec<Notice>,
    next_notice: u64,
    pub(crate) pause_alerts: Vec<String>,
    targets: BTreeMap<String, TargetStatus>,
    detected: Vec<Detected>,
    system_zone: String,
    /// Bumped on every committed change, so the app knows when to schedule a sync, and when words are kept
    /// or a notice is dismissed, so that no older reply on its way to the page can undo that.
    pub(crate) revision: u64,
    /// Words kept out of the store, by the id of the notice showing them.
    kept_out: BTreeMap<String, Kept>,
    /// Their file couldn't be read or moved aside at this launch, so this run writes nothing over it.
    kept_file_stuck: bool,
    /// The notice telling the user that their kept words may not outlast this run, once there has been one.
    kept_at_risk_notice: Option<String>,
    /// Kept words dismissed in this run. A file of them read late may still hold them.
    dismissed_kept: Vec<Kept>,
    /// Drafts this launch left on disk, as it couldn't write their words to a file of their own.
    drafts_left: BTreeMap<String, Draft>,
    /// The file of drafts couldn't be read or moved aside at this launch. What it holds is for the next launch to put
    /// back, and a draft is one lot of words for a task, so this run writes nothing over it, even once it can be read.
    drafts_file_stuck: bool,
    /// When this launch began. Nothing it had to write can have reached another device by then.
    pub(crate) opened: Ts,
    /// Drafts put back at this launch, as stored, until something else replaces them.
    recovered: BTreeMap<String, Draft>,
    /// False on every real launch, until the first task of this launch has started. Never saved.
    pub(crate) warm: bool,
    /// The task most recently worked in this launch: what a warm start copies.
    pub(crate) last_worked: Option<String>,
    /// A failure to save merged data. Folder writes must not clear it.
    pub(crate) local_error: Option<String>,
    hung: HungTargets,
    idle: IdleTracker,
    /// The reminder is quiet until this moment.
    reminder_quiet_until: Option<Ts>,
    /// The reminder last shown as a system notification, so it isn't repeated each minute.
    reminder_told: Option<Reminder>,
    /// Time away found but not yet saved, with the segment it belongs to. Kept until a save works.
    unsaved_gaps: Vec<(String, NewGap)>,
    /// Clocks put back under a running timer and not yet saved. Kept until a save works.
    unsaved_steps: Vec<ClockStep>,
    /// Clocks put back, not yet offered as a system notification.
    clock_alerts: Vec<String>,
    /// Where a replacement colour's randomness comes from. Injected in tests.
    random: Box<dyn FnMut() -> u64 + Send>,
    color_replaced: Option<String>,
    /// Running from an MSIX package, whose files Windows removes with the app.
    packaged: bool,
    /// Whether this computer may change replicated data, as far as can be known.
    pub(crate) lease: LeaseState,
    /// What reading and writing the lease records keeps, see `lease_io`.
    pub(crate) lease_io: crate::lease_io::LeaseIo,
    /// A version to write at the next chance, whatever the hourly cadence says.
    version_due: Option<VersionReason>,
    /// The data each restored version followed here replaced, oldest first, until a version of it is written.
    pre_adopt: VecDeque<DeviceFile>,
    /// The newest stamp this device has seen, its own or another's: every stamp a commit writes is later, up to the
    /// lead cap (see `hlc`). Never goes down in a run, not even after a restore. Saved as `LocalFile::newest_stamp`.
    newest: Option<Stamp>,
    /// When each other device wrote its file, by its own clock, as last read here: what the clock-behind warning holds
    /// this clock against. Replaced at every read, never kept at the highest, so a clock that was put right is believed
    /// from its next write. Not kept across launches.
    others_written: BTreeMap<String, Ts>,
}

/// What a restore installs along with the data, and in the same save, so a crash can't leave half of it.
pub(crate) struct RestoreNote {
    /// `backup` or `version`, and which one, for the record of changes.
    pub entity: &'static str,
    pub name: String,
    pub after: Value,
    /// The moment a task these data hold running stopped being timed.
    pub closed_at: Ts,
    pub reason: PauseReason,
    /// The last heartbeat these data know of.
    pub last_heartbeat: Option<Ts>,
    /// The restore every device is to follow from here on. `None` leaves that as it is.
    pub epoch: Option<EpochInfo>,
}

/// Saved colours and what happened to them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColorOutcome {
    pub color: String,
    /// What was asked for, when it matched another's exactly and so was replaced.
    pub replaced: Option<String>,
}

fn default_random() -> u64 {
    let (hi, _) = uuid::Uuid::new_v4().as_u64_pair();
    hi
}

/// A length of time as the user reads it, to the minute.
pub(crate) fn span_text(secs: i64) -> String {
    let minutes = secs.max(0) / 60;
    match (minutes / (24 * 60), minutes / 60, minutes % 60) {
        (0, 0, 0) => "less than a minute".to_owned(),
        (0, 0, m) => format!("{m} min"),
        (0, h, 0) => format!("{h} h"),
        (0, h, m) => format!("{h} h {m} min"),
        (1, _, _) => "1 day".to_owned(),
        (d, _, _) => format!("{d} days"),
    }
}

/// The folders, as the user calls them: "OneDrive", "OneDrive and Google Drive", "A, B and C".
pub(crate) fn folder_list(targets: &[SyncTarget]) -> String {
    let labels: Vec<String> = targets.iter().map(folder_label).collect();
    match labels.as_slice() {
        [] => "your sync folder".to_owned(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What the user calls a folder.
pub(crate) fn folder_label(target: &SyncTarget) -> String {
    match target.provider {
        SyncProvider::OneDrive => "OneDrive".to_owned(),
        SyncProvider::GoogleDrive => "Google Drive".to_owned(),
        SyncProvider::Custom => Path::new(&target.root)
            .file_name()
            .map_or_else(|| "Sync".to_owned(), |n| n.to_string_lossy().into_owned()),
    }
}

/// The setting's value in minutes, or the default when it was never set. `null` or 0 is off.
fn minutes_setting(data: &Shared, key: &str, default: i64) -> i64 {
    match data.settings.get(key).map(|e| &e.value) {
        None => default,
        Some(v) => v.as_i64().unwrap_or(0).max(0),
    }
}

const DEFAULT_IDLE_MINUTES: i64 = 10;
pub const PDF_LOGO_FILE: &str = "pdf-logo.png";

/// Saves `next` over `prev`, which must be what the store holds, with the floor of the hybrid logical clock raised and
/// stored in the same transaction: `floor` and `next.newest_stamp` go up to the newest stamp among the records this save
/// writes. Records it doesn't write hold nothing newer than the store already did, so this equals a walk of every stamp,
/// at the cost of the diff the save does anyway (addendum P11, `hlc` tests), as far as a reader takes a stamp
/// (`seen_in`). `floor` goes up even if the save fails, as the caller may keep what it has in memory. When records are
/// written, `next.latest_change_here` goes up to `now`.
fn save_with_floor(
    store: &mut Store,
    floor: &mut Option<Stamp>,
    prev: &LocalFile,
    next: &mut LocalFile,
    restored: bool,
    now: Ts,
) -> Result<SaveStats, CoreError> {
    let planned = store.plan(&prev.data, &next.data)?;
    // The newest stamp the save writes is nearly always one a reader takes. When it isn't, such as one a restore brought
    // back, the walk of `seen_in` finds the newest that is.
    let newest = match planned.newest() {
        Some(s) if s.at > now.later(MAX_SKEW_SECS) => seen_in(&next.data, now),
        other => other.cloned(),
    };
    *floor = hlc::later(floor.take(), newest);
    next.newest_stamp = hlc::later(next.newest_stamp.take(), floor.clone());
    if planned.writes_records() {
        next.latest_change_here = next.latest_change_here.max(Some(now));
    }
    store.save_planned(planned, next, restored)
}

/// The newest stamp in `data` that the floor follows: none dated further past `now` than a reader takes a record
/// (`validate::MAX_SKEW_SECS`). Data brought back by a restore can hold one dated far ahead, and a floor following it
/// would hold every stamp made here at the lead cap, ahead of every other device's edits, until the clock got there.
fn seen_in(data: &Shared, now: Ts) -> Option<Stamp> {
    data.newest_stamp_until(now.later(MAX_SKEW_SECS)).cloned()
}

/// Tries `op` again while something holds the store, with a `wait` before each try.
fn retry_while_held<T>(
    mut op: impl FnMut() -> Result<T, CoreError>,
    mut wait: impl FnMut(std::time::Duration),
) -> Result<T, CoreError> {
    for _ in 0..4 {
        match op() {
            Err(e) if e.is_held() => wait(std::time::Duration::from_secs(1)),
            other => return other,
        }
    }
    op()
}

impl Service {
    /// Opens the store for a launch. At login antivirus can hold the file for longer than one save waits,
    /// so this waits a few seconds rather than leave the app without its data until the next launch.
    pub fn open_when_free(
        dir: &Path,
        clock: Arc<dyn Clock>,
        device_name: &str,
    ) -> Result<Self, CoreError> {
        retry_while_held(
            || Self::open(dir, Arc::clone(&clock), device_name),
            std::thread::sleep,
        )
    }

    /// Opens the store. An open segment left by a crash is closed at the last heartbeat here.
    pub fn open(dir: &Path, clock: Arc<dyn Clock>, device_name: &str) -> Result<Self, CoreError> {
        let now = clock.now();
        let Opened {
            store,
            file,
            raw_beat,
            origin: _,
            notices,
        } = Store::open(dir, device_name, now)?;
        let paths = Paths::new(dir).with_backups_from(store.created_at());
        let loaded = file.clone();
        let lease = LeaseState::new(!file.prefs.sync_targets.is_empty());
        let mut svc = Self {
            paths,
            store,
            opened_beat: raw_beat,
            file,
            clock,
            notices: Vec::new(),
            pause_alerts: Vec::new(),
            next_notice: 1,
            targets: BTreeMap::new(),
            detected: Vec::new(),
            system_zone: "UTC".to_owned(),
            revision: 0,
            warm: false,
            last_worked: None,
            local_error: None,
            hung: HungTargets::default(),
            idle: IdleTracker::default(),
            reminder_quiet_until: None,
            reminder_told: None,
            unsaved_gaps: Vec::new(),
            unsaved_steps: Vec::new(),
            clock_alerts: Vec::new(),
            random: Box::new(default_random),
            color_replaced: None,
            kept_out: BTreeMap::new(),
            kept_file_stuck: false,
            kept_at_risk_notice: None,
            dismissed_kept: Vec::new(),
            drafts_left: BTreeMap::new(),
            drafts_file_stuck: false,
            opened: now,
            recovered: BTreeMap::new(),
            packaged: false,
            lease,
            lease_io: crate::lease_io::LeaseIo::default(),
            version_due: None,
            pre_adopt: VecDeque::new(),
            newest: None,
            others_written: BTreeMap::new(),
        };
        svc.keep_folder_lists();
        svc.file.data.seed_tags();
        svc.file.data.normalize();
        // What this device had seen, before anything here writes a stamp: the stored floor, which outlives data a
        // restore took away, and every stamp in the data as far as a reader would take it (`seen_in`).
        let seen = seen_in(&svc.file.data, now);
        svc.newest = hlc::later(svc.file.newest_stamp.clone(), seen);
        svc.load_kept();
        // Drafts are words for tasks that a first start hasn't got yet: they wait, on disk, for the data to arrive.
        let waiting = svc.awaiting_cloud();
        let kept_drafts = if waiting {
            BTreeMap::new()
        } else {
            svc.replay_drafts()
        };
        svc.file
            .data
            .raise_reference_marks(&mut svc.file.prefs.reference_marks);
        for (level, n) in notices {
            svc.notify(level, n);
        }
        svc.recover_crash()?;
        // Whatever opening changed, such as recovered drafts or a segment closed after a crash.
        save_with_floor(
            &mut svc.store,
            &mut svc.newest,
            &loaded,
            &mut svc.file,
            false,
            now,
        )?;
        if !waiting {
            svc.settle_drafts(kept_drafts);
        }
        Ok(svc)
    }

    /// Only once the store has saved recovered drafts are they safe in it, and words it won't take safe in their own
    /// file. Either failing keeps them as drafts for next time.
    fn settle_drafts(&mut self, kept_drafts: BTreeMap<String, Draft>) {
        let stay = if self.save_kept().is_ok() {
            BTreeMap::new()
        } else {
            kept_drafts
        };
        if !self.drafts_file_stuck {
            let _ = crate::drafts::write(&self.paths.dir, &stay);
        }
        self.drafts_left = stay;
    }

    /// What is known of each folder's restores goes with the folder. A store from before these lists follows the
    /// folders it already has, as it has been merging with them all along.
    fn keep_folder_lists(&mut self) {
        let prefs = &mut self.file.prefs;
        let roots: Vec<String> = prefs.sync_targets.iter().map(|t| t.root.clone()).collect();
        prefs
            .sync_joined
            .get_or_insert_with(|| roots.clone())
            .retain(|r| roots.contains(r));
        if prefs
            .join_pending
            .as_ref()
            .is_some_and(|p| !roots.contains(&p.root))
        {
            prefs.join_pending = None;
        }
    }

    fn recover_crash(&mut self) -> Result<(), CoreError> {
        let device = self.file.device_id.clone();
        let away = self.file.prefs.away_since.take();
        let away_kind = self.file.prefs.away_kind.take();
        let Some(segment) = self.file.data.own_open_segments(&device).into_iter().next() else {
            return Ok(());
        };
        let now = self.now();
        let ctx = Ctx::hlc(now, &device, self.newest.as_ref());
        // The last heartbeat is the last moment the app is known to have been alive. Loading cuts it to now,
        // but a run that began after now was timed on a clock put back while the app was off. Its beat is on
        // that same clock, and cutting it would lose the whole run.
        let began_ahead = self
            .file
            .data
            .segments
            .get(&segment)
            .is_some_and(|s| s.start > now);
        let last_seen = if began_ahead {
            self.file.last_heartbeat.max(self.opened_beat)
        } else {
            self.file.last_heartbeat
        };
        let at = last_seen.unwrap_or(Ts(i64::MIN / 2));
        // The app was alive but nobody was there, so that time stays unconfirmed.
        if let Some(away) = away {
            self.file
                .data
                .add_gap(&ctx, &segment, away, at, away_kind.unwrap_or(GapKind::Idle));
        }
        let names = self
            .file
            .data
            .close_own_open(&ctx, at, &PauseReason::CrashRecovery);
        self.notify(
            "warning",
            format!(
                "The app did not shut down cleanly. {} was paused at its last recorded moment ({}). Time after that was not logged. The next start asks for every field.",
                names.join(", "),
                last_seen.map_or_else(|| "its start".to_owned(), |h| h.iso()),
            ),
        );
        Ok(())
    }

    pub(crate) fn notify(&mut self, level: &'static str, message: String) {
        self.notices.push(Notice {
            id: self.next_notice.to_string(),
            level,
            message,
        });
        self.next_notice += 1;
    }

    #[must_use]
    pub fn now(&self) -> Ts {
        self.clock.now()
    }

    #[must_use]
    pub fn file(&self) -> &LocalFile {
        &self.file
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.paths.dir
    }

    #[must_use]
    pub fn snapshot_system_zone(&self) -> String {
        self.system_zone.clone()
    }

    /// The zone the user is viewing: the active one, else their own, else this computer's.
    #[must_use]
    pub fn viewing_zone(&self) -> chrono_tz::Tz {
        let settings = &self.file.data.settings;
        settings
            .get("activeZone")
            .filter(|e| e.value.is_string())
            .or_else(|| settings.get("myZone"))
            .and_then(|e| e.value.as_str())
            .and_then(|z| crate::validate::parse_zone(z).ok())
            .or_else(|| crate::validate::parse_zone(&self.system_zone).ok())
            .unwrap_or(chrono_tz::UTC)
    }

    pub fn set_system_zone(&mut self, zone: String) {
        self.system_zone = zone;
    }

    /// Takes the zone the OS reports now. True when that changed the zone in use, so what is on screen
    /// needs building again. A read that failed, or a name this build doesn't know, changes nothing.
    pub fn observe_system_zone(&mut self, reported: Option<String>) -> bool {
        let Some(zone) =
            reported.filter(|z| *z != self.system_zone && crate::validate::parse_zone(z).is_ok())
        else {
            return false;
        };
        self.set_system_zone(zone);
        // So that no older reply on its way to the page can undo it.
        self.revision += 1;
        true
    }

    pub fn set_detected(&mut self, detected: Vec<Detected>) {
        self.detected = detected;
    }

    /// Applies a change to a copy, saves it, and only then adopts it. If saving fails nothing changes. A change
    /// to replicated data is refused while another computer holds the lease, and counts as this one needing it.
    /// Preferences are this device's own and always pass.
    pub fn mutate<R>(
        &mut self,
        f: impl FnOnce(&mut LocalFile, &Ctx<'_>) -> Result<R, CoreError>,
    ) -> Result<R, CoreError> {
        self.commit(true, f)
    }

    /// As `mutate`, for what isn't the user changing data: what arrives from a sync, a timer closing, a pause asked
    /// of the holder. A read-only device has to go on doing these.
    pub(crate) fn mutate_system<R>(
        &mut self,
        f: impl FnOnce(&mut LocalFile, &Ctx<'_>) -> Result<R, CoreError>,
    ) -> Result<R, CoreError> {
        self.commit(false, f)
    }

    fn commit<R>(
        &mut self,
        gated: bool,
        f: impl FnOnce(&mut LocalFile, &Ctx<'_>) -> Result<R, CoreError>,
    ) -> Result<R, CoreError> {
        let now = self.now();
        let device = self.file.device_id.clone();
        let ctx = Ctx::hlc(now, &device, self.newest.as_ref());
        let mut next = self.file.clone();
        let out = f(&mut next, &ctx)?;
        let shared_change = gated && self.lease.active() && next.data != self.file.data;
        if shared_change {
            self.lease_gate()?;
        }
        save_with_floor(
            &mut self.store,
            &mut self.newest,
            &self.file,
            &mut next,
            false,
            now,
        )?;
        self.file = next;
        self.revision += 1;
        self.local_error = None;
        if shared_change {
            self.lease.note_change(now);
        }
        Ok(out)
    }

    /// `Err(ReadOnly)` while another computer holds the lease.
    pub(crate) fn lease_gate(&self) -> Result<(), CoreError> {
        self.lease.gate(
            self.file.prefs.lease_memo.as_ref(),
            &self.file.device_id,
            self.now(),
        )
    }

    /// Changes to replicated data are refused for now.
    #[must_use]
    pub fn read_only(&self) -> bool {
        self.lease.read_only(
            self.file.prefs.lease_memo.as_ref(),
            &self.file.device_id,
            self.now(),
        )
    }

    /// The lease, as far as the page goes.
    #[must_use]
    pub fn lease_view(&self) -> LeaseView {
        let now = self.now();
        let memo = self.file.prefs.lease_memo.as_ref();
        let holder = self
            .lease
            .holder
            .as_ref()
            .map(|h| LeaseHolderView::of(h, now))
            .or_else(|| {
                // With no folder to read, the last one known is all there is to say.
                memo.filter(|m| {
                    self.lease.state == LeaseStateName::Unknown
                        && m.device_id != self.file.device_id
                })
                .map(|m| LeaseHolderView {
                    device_id: m.device_id.clone(),
                    device_name: m.device_name.clone(),
                    generation: m.generation,
                    beat_seq: 0,
                    last_sign_at: Some(m.at),
                    age_secs: Some((now.secs() - m.at.secs()).max(0)),
                    clock_suspect: false,
                })
            });
        LeaseView {
            state: self.lease.state,
            read_only: self.read_only(),
            generation: self
                .lease
                .claim
                .as_ref()
                .map(|c| c.generation)
                .or_else(|| holder.as_ref().map(|h| h.generation)),
            holder,
            note: self.lease.note.clone(),
            reachable_roots: self.lease.reachable_roots.clone(),
            unreachable_roots: self.lease.unreachable_roots.clone(),
        }
    }

    pub fn set_packaged(&mut self, packaged: bool) {
        self.packaged = packaged;
    }

    #[must_use]
    pub fn packaged(&self) -> bool {
        self.packaged
    }

    /// Asks for a version at the next chance, whatever the hourly cadence says.
    pub fn request_version(&mut self, reason: VersionReason) {
        self.version_due = Some(reason);
    }

    /// The version asked for, once, for the one about to write it.
    pub fn take_version_due(&mut self) -> Option<VersionReason> {
        self.version_due.take()
    }

    /// Keeps the data a restored version is about to replace, until a version of it is written. One kept before it and
    /// not written yet waits too, and a version asked for already keeps its place.
    pub fn keep_pre_adopt(&mut self, file: DeviceFile) {
        let at = self
            .pre_adopt
            .partition_point(|f| f.written_at <= file.written_at);
        self.pre_adopt.insert(at, file);
        self.version_due.get_or_insert(VersionReason::PreAdopt);
    }

    /// The oldest data kept by `keep_pre_adopt`, once.
    pub fn take_pre_adopt(&mut self) -> Option<DeviceFile> {
        self.pre_adopt.pop_front()
    }

    /// Replaces a colour that exactly matches another active project's. A saved colour left unchanged
    /// stays, so two devices that picked the same one offline don't fight over it.
    fn project_with_unique_color(
        &mut self,
        id: Option<&str>,
        input: &ProjectInput,
    ) -> Result<ProjectInput, CoreError> {
        let taken: Vec<String> = self
            .file
            .data
            .projects
            .values()
            .filter(|p| !p.deleted && !p.archived && Some(p.id.as_str()) != id)
            .map(|p| p.color.clone())
            .collect();
        let unchanged = id
            .and_then(|i| self.file.data.projects.get(i))
            .is_some_and(|p| p.color.eq_ignore_ascii_case(&input.color));
        self.color_replaced = None;
        if unchanged {
            return Ok(input.clone());
        }
        let (color, replaced) = colors::resolve(&input.color, &taken, &mut *self.random)?;
        if replaced {
            self.color_replaced = Some(input.color.clone());
        }
        Ok(ProjectInput {
            color,
            ..input.clone()
        })
    }

    /// The colour just saved, and what was asked for if it had to be replaced.
    pub fn take_color_replaced(&mut self) -> Option<String> {
        self.color_replaced.take()
    }

    #[cfg(test)]
    pub fn set_random(&mut self, random: Box<dyn FnMut() -> u64 + Send>) {
        self.random = random;
    }

    pub fn create_project(&mut self, input: &ProjectInput) -> Result<String, CoreError> {
        let input = self.project_with_unique_color(None, input)?;
        self.mutate(|f, c| f.data.create_project(c, &input))
    }
    pub fn update_project(&mut self, id: &str, input: &ProjectInput) -> Result<(), CoreError> {
        let input = self.project_with_unique_color(Some(id), input)?;
        self.mutate(|f, c| f.data.update_project(c, id, &input))
    }
    pub fn set_project_archived(&mut self, id: &str, archived: bool) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.set_project_archived(c, id, archived))
    }
    pub fn delete_project(&mut self, id: &str) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.delete_project(c, id))
    }
    pub fn create_task(&mut self, input: &TaskInput) -> Result<String, CoreError> {
        self.mutate(|f, c| f.data.create_task(c, input))
    }
    pub fn update_task(&mut self, id: &str, input: &TaskInput) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.update_task(c, id, input))
    }
    pub fn patch_task(
        &mut self,
        id: &str,
        patch: &TaskPatch,
        expected: &TaskExpected,
    ) -> Result<PatchOutcome, CoreError> {
        self.mutate(|f, c| f.data.patch_task(c, id, patch, expected))
    }
    pub fn delete_draft(&mut self, id: &str) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.delete_draft(c, id))
    }
    pub fn delete_task(&mut self, id: &str) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.delete_task(c, id))
    }

    fn with_beat<R>(f: &mut LocalFile, c: &Ctx<'_>, r: R) -> R {
        f.last_heartbeat = Some(c.now);
        r
    }

    /// A cold launch: every required field must be filled before the first start, enforced here.
    fn check_cold_fields(&self, id: &str) -> Result<(), CoreError> {
        if self.warm {
            return Ok(());
        }
        let task = self
            .file
            .data
            .tasks
            .get(id)
            .filter(|t| t.is_live())
            .ok_or(CoreError::NotFound("task"))?;
        for field in cold_required(&self.file.data) {
            let missing = match field {
                RequiredField::Name => task.name.trim().is_empty(),
                RequiredField::Project => task.project_id.is_empty(),
                RequiredField::Description => !doc_has_text(&task.description),
                RequiredField::Tags => !task
                    .tags
                    .iter()
                    .any(|t| self.file.data.tags.get(t).is_some_and(|x| !x.deleted)),
            };
            if missing {
                let label = match field {
                    RequiredField::Name => "Task name",
                    RequiredField::Project => "Project",
                    RequiredField::Description => "Description",
                    RequiredField::Tags => "A tag",
                };
                return Err(CoreError::Validation(format!(
                    "{label} is required for the first start after launch"
                )));
            }
        }
        Ok(())
    }

    pub fn start_task(&mut self, id: &str) -> Result<Transition, CoreError> {
        self.check_cold_fields(id)?;
        self.settle_away()?;
        let out = self.mutate(|f, c| {
            let out = f.data.start(c, id)?;
            Ok(Self::with_beat(f, c, out))
        })?;
        self.warm = true;
        self.last_worked = Some(id.to_owned());
        Ok(out)
    }

    /// Refused on a cold launch: continuing then means a new linked task with every field filled in.
    pub fn resume_task(&mut self, id: &str) -> Result<Transition, CoreError> {
        if !self.warm {
            return Err(CoreError::ColdStartRequired);
        }
        self.settle_away()?;
        let out = self.mutate(|f, c| {
            let out = f.data.resume(c, id)?;
            Ok(Self::with_beat(f, c, out))
        })?;
        self.last_worked = Some(id.to_owned());
        Ok(out)
    }

    pub fn pause_task(&mut self, id: &str) -> Result<(), CoreError> {
        self.settle_away()?;
        self.mutate(|f, c| {
            f.data
                .pause(c, id)
                .map_err(|e| name_device(e, f.remote_devices.clone()))
        })
    }

    pub fn stop_task(&mut self, id: &str) -> Result<(), CoreError> {
        self.settle_away()?;
        self.mutate(|f, c| f.data.stop(c, id))
    }

    #[must_use]
    pub fn is_warm(&self) -> bool {
        self.warm
    }

    #[must_use]
    pub fn hung_targets(&self) -> HungTargets {
        Arc::clone(&self.hung)
    }

    fn copy_source(&self) -> Option<CopySource> {
        let data = &self.file.data;
        let worked = self.last_worked.as_ref()?;
        // If that one has been deleted since, whichever was worked last before it.
        let task = data.tasks.get(worked).filter(|t| t.is_live()).or_else(|| {
            data.segments
                .values()
                .filter(|s| data.tasks.get(&s.task_id).is_some_and(Task::is_live))
                .filter_map(|s| {
                    let (start, end) = s.effective()?;
                    Some((end.unwrap_or(start), &s.task_id))
                })
                .max()
                .and_then(|(_, id)| data.tasks.get(id))
        })?;
        let project = data.projects.get(&task.project_id);
        Some(CopySource {
            task_id: task.id.clone(),
            name: task.name.clone(),
            project_id: task.project_id.clone(),
            project_archived: project.is_none_or(|p| p.archived || p.deleted),
            // A copy keeps subtask text, with ticks cleared and no time carried over.
            description: crate::subtasks::reset(&task.description),
            tags: task
                .tags
                .iter()
                .filter(|t| data.tags.get(*t).is_some_and(|x| !x.deleted))
                .cloned()
                .collect(),
            last_worked_at: data
                .segments
                .values()
                .filter(|s| s.task_id == task.id)
                .map(|s| s.end.unwrap_or(s.start))
                .max(),
        })
    }

    pub fn add_manual_entry(&mut self, input: &ManualEntryInput) -> Result<String, CoreError> {
        self.mutate(|f, c| f.data.add_manual_entry(c, input))
    }
    pub fn correct_segment(&mut self, input: &CorrectionInput) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.correct_segment(c, input))
    }
    pub fn void_segment(&mut self, id: &str, reason: &str) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.void_segment(c, id, reason))
    }
    pub fn restore_segment(&mut self, id: &str, reason: &str) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.restore_segment(c, id, reason))
    }
    pub fn decide_gap(&mut self, id: &str, decision: GapDecision) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.decide_gap(c, id, decision))
    }
    pub fn mark_billable_pause(&mut self, input: &PauseInput) -> Result<String, CoreError> {
        self.mutate(|f, c| f.data.mark_billable_pause(c, input))
    }
    pub fn unmark_billable_pause(&mut self, id: &str) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.unmark_billable_pause(c, id))
    }
    pub fn set_active_subtask(
        &mut self,
        task: &str,
        subtask: Option<&str>,
    ) -> Result<(), CoreError> {
        self.settle_away()?;
        self.mutate(|f, c| f.data.set_active_subtask(c, task, subtask))
    }
    pub fn save_tag(&mut self, input: &TagInput) -> Result<String, CoreError> {
        let own = input.id.as_deref();
        let taken: Vec<String> = self
            .file
            .data
            .tags
            .values()
            .filter(|t| !t.deleted && Some(t.id.as_str()) != own)
            .map(|t| t.color.clone())
            .collect();
        let unchanged = own
            .and_then(|i| self.file.data.tags.get(i))
            .is_some_and(|t| t.color.eq_ignore_ascii_case(&input.color));
        self.color_replaced = None;
        let mut input = input.clone();
        if !unchanged {
            let (color, replaced) = colors::resolve(&input.color, &taken, &mut *self.random)?;
            if replaced {
                self.color_replaced = Some(input.color.clone());
            }
            input.color = color;
        }
        self.mutate(|f, c| f.data.save_tag(c, &input))
    }

    pub fn revert_shift_version(&mut self, version_id: &str) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.revert_shift_version(c, version_id))
    }

    /// Closes time left running on another device at the last moment anyone knows of it. Never automatic: the user
    /// asks, and only once that device was asked to pause or has said nothing for a long time, as otherwise it may
    /// simply be in use. If it comes back, its own record replaces this.
    pub fn settle_lost_segment(&mut self, segment_id: &str, reason: &str) -> Result<(), CoreError> {
        let seg = self
            .file
            .data
            .segments
            .get(segment_id)
            .ok_or(CoreError::NotFound("work segment"))?;
        if seg.device_id == self.file.device_id || !seg.is_open() {
            return Err(CoreError::Conflict(
                "this segment isn't running on another computer".into(),
            ));
        }
        let name = self.device_name_of(&seg.device_id);
        let asked = self.file.data.earliest_pause_request(segment_id).is_some();
        // A device this one has never read a file from is known only by this segment.
        let last_seen = self
            .file
            .remote_devices
            .get(&seg.device_id)
            .map_or(seg.start, |d| d.written_at);
        let silent = self.now().secs() - last_seen.secs();
        if !asked && silent < LOST_DEVICE_SILENCE_SECS {
            return Err(CoreError::Conflict(format!(
                "{name} was seen {} ago, so it may still be running this",
                span_text(silent),
            )));
        }
        let end = self.end_source().end_of(seg);
        let reason = if reason.trim().is_empty() {
            format!(
                "Closed from {}: {name} stopped responding. Ended at its last known moment.",
                self.file.device_name
            )
        } else {
            reason.to_owned()
        };
        self.mutate(|f, c| {
            f.data
                .settle_foreign_open_segment(c, segment_id, end, &reason)
        })
    }

    /// Asking the holder to pause is what a read-only device must be able to do.
    pub fn request_remote_pause(&mut self, task_id: &str) -> Result<String, CoreError> {
        self.mutate_system(|f, c| f.data.request_remote_pause(c, task_id))
    }

    /// Stores what is being typed, so a shutdown can't lose it. Validated like a real edit. `base` is
    /// the stored description the words were typed over: only if it is still there can they replace it.
    pub fn stash_draft(
        &self,
        task_id: &str,
        description: &Value,
        base: &Value,
    ) -> Result<(), CoreError> {
        crate::validate::check_description(description)?;
        // Ids make a list longer. Better the page hears now than the words wait for a launch that can't use them.
        let stored = self.file.data.tasks.get(task_id).map(|t| &t.description);
        crate::validate::check_description(&crate::subtasks::assign_ids(description, stored))?;
        let mut all = self.drafts_to_change()?;
        // The page writes a stash again in case it failed, and that makes its words no newer than they were.
        let at = all
            .get(task_id)
            .filter(|d| d.description == *description && d.base == *base)
            .map_or_else(|| self.now(), |d| d.at);
        all.insert(
            task_id.to_owned(),
            Draft {
                description: description.clone(),
                at,
                base: base.clone(),
            },
        );
        crate::drafts::write(&self.paths.dir, &all)
    }

    pub fn clear_draft(&self, task_id: &str) -> Result<(), CoreError> {
        let mut all = self.drafts_to_change()?;
        if all.remove(task_id).is_some() {
            crate::drafts::write(&self.paths.dir, &all)?;
        }
        Ok(())
    }

    /// The drafts on disk, for a stash or a clear to write back changed. Other tasks' words are in there, some of them
    /// perhaps the only copy, so what can't be read comes back as an error, and that file stays as it is.
    fn drafts_to_change(&self) -> Result<BTreeMap<String, Draft>, CoreError> {
        if self.drafts_file_stuck {
            return Err(CoreError::StoreUnavailable(
                "the file of drafts couldn't be read at this launch".to_owned(),
            ));
        }
        crate::drafts::read(&self.paths.dir)?
            .ok_or_else(|| CoreError::StoreUnavailable("the file of drafts is damaged".to_owned()))
    }

    /// Puts back words a shutdown left unsaved, where that replaces nothing the user has not seen: the
    /// description they were typed over is still stored, or what is stored is this device's own older
    /// text. Any other stored text stays, and the words go in a notice instead. Returns those drafts,
    /// which stay on disk until the words are safe in a file of their own.
    fn replay_drafts(&mut self) -> BTreeMap<String, Draft> {
        let all = self.load_drafts();
        let device = self.file.device_id.clone();
        let mut restored = Vec::new();
        let mut kept = Vec::new();
        for (task_id, draft) in all {
            let stored = self.file.data.tasks.get_mut(&task_id);
            let saved = stored.as_ref().is_some_and(|t| {
                crate::subtasks::assign_ids(&draft.description, Some(&t.description))
                    == t.description
            });
            if saved || crate::validate::check_description(&draft.description).is_err() {
                continue;
            }
            let because = match stored {
                Some(task) if task.is_live() => {
                    let own = task.description_stamp.by == device;
                    if draft.base != task.description
                        && !(own && draft.at > task.description_stamp.at)
                    {
                        KeptBecause::ChangedElsewhere
                    } else {
                        let description = crate::subtasks::assign_ids(
                            &draft.description,
                            Some(&task.description),
                        );
                        // With its ids it may be more than the store can hold, and a store that can't
                        // be saved would stop every launch from here on.
                        if crate::validate::check_description(&description).is_ok() {
                            let ctx = Ctx::hlc(draft.at, &device, self.newest.as_ref());
                            task.description = description;
                            task.description_stamp = ctx.next(&task.description_stamp);
                            crate::machine::clear_missing_active(task, &ctx);
                            restored.push(task.name.clone());
                            self.recovered.insert(
                                task_id,
                                Draft {
                                    description: task.description.clone(),
                                    ..draft
                                },
                            );
                            continue;
                        }
                        KeptBecause::TooLong
                    }
                }
                _ => KeptBecause::TaskGone,
            };
            if !crate::subtasks::plain_text(&draft.description)
                .trim()
                .is_empty()
            {
                kept.push((task_id, because, draft));
            }
        }
        if !restored.is_empty() {
            self.notify(
                "info",
                format!(
                    "Recovered an edit to {} that was typed just before the app closed.",
                    restored.join(", ")
                ),
            );
        }
        let mut stay = BTreeMap::new();
        for (task_id, because, draft) in kept {
            self.keep_draft(&task_id, &draft, because);
            stay.insert(task_id, draft);
        }
        stay
    }

    /// The drafts the last run left, or none if their file can't be read, which gets said.
    fn load_drafts(&mut self) -> BTreeMap<String, Draft> {
        let read = crate::drafts::read(&self.paths.dir);
        let Ok(Some(all)) = read else {
            // A damaged file goes aside. One that can't be read may only be in use, so it stays for the next launch.
            let aside = match read {
                Ok(None) => crate::drafts::set_aside_drafts(&self.paths.dir, self.now()).ok(),
                _ => None,
            };
            let end = aside.as_ref().map_or_else(
                || "It is left as it is, for the next launch to try again. Restart the app when you can, as until then what you type has no second copy in case the app closes before it saves.".to_owned(),
                |name| format!("It is now {name} in the data folder."),
            );
            self.drafts_file_stuck = aside.is_none();
            self.notify(
                "warning",
                format!(
                    "Words typed before the app last closed can't be put back, because the file holding them can't be read. {end}"
                ),
            );
            return BTreeMap::new();
        };
        all
    }

    /// Words kept at an earlier launch and not yet dismissed.
    fn load_kept(&mut self) {
        let read = crate::drafts::read_kept(&self.paths.dir);
        let Ok(Some(all)) = read else {
            // A damaged file goes aside. One that can't be read may only be in use, so it stays for the next launch.
            let aside = match read {
                Ok(None) => crate::drafts::set_aside_kept(&self.paths.dir, self.now()).ok(),
                _ => None,
            };
            let end = aside.as_ref().map_or_else(
                || "It is left as it is, for the next launch to try again.".to_owned(),
                |name| format!("It is now {name} in the data folder."),
            );
            self.kept_file_stuck = aside.is_none();
            self.notify(
                "warning",
                format!(
                    "Words kept for you from an earlier launch can't be shown, because the file holding them can't be read. {end}"
                ),
            );
            return;
        };
        self.show_kept(all);
    }

    fn show_kept(&mut self, mut all: Vec<Kept>) {
        all.sort_by_key(|k| k.at);
        for mut kept in all {
            if let Some(task) = self.file.data.tasks.get(&kept.task_id) {
                kept.name.clone_from(&task.name);
            }
            self.keep_once(kept);
        }
    }

    /// Writes the words kept out of the store to their file, or removes it if none are left. Words it won't
    /// take may last only as long as their notices, so that gets said, by one notice at a time.
    fn save_kept(&mut self) -> Result<(), CoreError> {
        // A file that couldn't be read at launch may only have been in use, and a run can last for weeks.
        if self.kept_file_stuck
            && let Ok(Some(all)) = crate::drafts::read_kept(&self.paths.dir)
        {
            self.kept_file_stuck = false;
            self.show_kept(all);
        }
        let written = if self.kept_file_stuck {
            Err(CoreError::StoreUnavailable(
                "the file of kept words can't be read".to_owned(),
            ))
        } else {
            let all: Vec<&Kept> = self.kept_out.values().collect();
            crate::drafts::write_kept(&self.paths.dir, &all)
        };
        let saying_so = self
            .kept_at_risk_notice
            .as_ref()
            .is_some_and(|id| self.notices.iter().any(|n| n.id == *id));
        if written.is_err() && !self.kept_out.is_empty() && !saying_so {
            self.kept_at_risk_notice = Some(self.next_notice.to_string());
            self.notify(
                "warning",
                "Words kept for you in a notice can't be written to the data folder just now, so they may not come back at the next launch. Copy any you want from their notice before you close the app.".to_owned(),
            );
        }
        if written.is_ok() {
            self.clear_drafts_left();
        }
        written
    }

    /// The words of the drafts this launch left are in their file now, or their notice was dismissed, and either way
    /// those drafts would only tell of them again. One stashed over since is words of its own, and stays.
    fn clear_drafts_left(&mut self) {
        if self.drafts_left.is_empty() {
            return;
        }
        let Ok(mut all) = self.drafts_to_change() else {
            return;
        };
        let before = all.len();
        all.retain(|id, draft| self.drafts_left.get(id) != Some(draft));
        if all.len() == before || crate::drafts::write(&self.paths.dir, &all).is_ok() {
            self.drafts_left.clear();
        }
    }

    /// Words of a draft the store won't take. A launch cut short after it wrote them finds that draft again.
    fn keep_draft(&mut self, task_id: &str, draft: &Draft, because: KeptBecause) {
        let name = self
            .file
            .data
            .tasks
            .get(task_id)
            .map_or("a deleted task", |t| t.name.as_str());
        self.keep_once(Kept {
            task_id: task_id.to_owned(),
            name: name.to_owned(),
            words: crate::subtasks::plain_text(&draft.description),
            at: draft.at,
            because,
        });
    }

    fn keep_once(&mut self, kept: Kept) {
        let known =
            |k: &Kept| k.task_id == kept.task_id && k.at == kept.at && k.words == kept.words;
        if !self.kept_out.values().any(known) && !self.dismissed_kept.iter().any(known) {
            self.notify_kept_out(kept);
        }
    }

    /// The words of an edit that no save can take, because its task no longer exists. Their stash goes once
    /// their own file has them. Until then it is what the next launch can tell of them from.
    pub fn keep_unsaved_words(
        &mut self,
        task_id: &str,
        name: &str,
        new_name: Option<&str>,
        description: Option<&Value>,
    ) {
        let text = description.map(crate::subtasks::plain_text);
        let words = [new_name, text.as_deref()].into_iter().flatten();
        self.notify_kept_out(Kept {
            task_id: task_id.to_owned(),
            name: name.to_owned(),
            words: words.collect::<Vec<_>>().join("\n"),
            at: self.now(),
            because: KeptBecause::TaskGone,
        });
        self.revision += 1;
        if self.save_kept().is_ok() {
            let _ = self.clear_draft(task_id);
        }
    }

    /// Tells of words the store won't take, in the only place left to read them.
    fn notify_kept_out(&mut self, kept: Kept) {
        let why = match kept.because {
            KeptBecause::ChangedElsewhere => {
                "were not put back, because its description changed while they waited. The newer text is kept."
            }
            KeptBecause::TaskGone => "were not saved, because the task no longer exists.",
            KeptBecause::TooLong => {
                "were not put back, because the list is too long to store. The text it had before is kept."
            }
        };
        let message = format!(
            "Your unsaved words on “{}” {why} Copy your words from here if you want them, as dismissing this notice ends them: {}",
            kept.name, kept.words
        );
        self.kept_out.insert(self.next_notice.to_string(), kept);
        self.notify("warning", message);
    }

    /// Words put back at launch lose to an edit from elsewhere, or to the task being deleted there, when a
    /// sync brings that. It stands, as any other does, and the words go in a notice rather than vanish, if it
    /// may have been done without them. Stamps can't say what the other device had seen, so that goes by time:
    /// see [`WITHOUT_WORDS_PUT_BACK_SECS`]. After that they are text like any other, which another device may
    /// well go on from.
    fn check_recovered(&mut self) {
        if self.recovered.is_empty() {
            return;
        }
        let device = self.file.device_id.clone();
        for (task_id, draft) in std::mem::take(&mut self.recovered) {
            let Some(task) = self.file.data.tasks.get(&task_id) else {
                continue;
            };
            let (stamp, because) = if task.is_live() {
                (&task.description_stamp, KeptBecause::ChangedElsewhere)
            } else {
                (&task.deleted_stamp, KeptBecause::TaskGone)
            };
            let without_them = stamp.by != device
                && stamp.at.secs() <= self.opened.secs() + WITHOUT_WORDS_PUT_BACK_SECS;
            if task.is_live() && task.description == draft.description {
                self.recovered.insert(task_id, draft);
            } else if without_them {
                self.keep_draft(&task_id, &draft, because);
                let _ = self.save_kept();
            }
        }
    }

    /// A fresh store, or a setup already under way. Someone who updated with data, or has already
    /// answered the sync prompt, is not asked.
    fn setup_pending(&self) -> bool {
        let prefs = &self.file.prefs;
        if prefs.setup_done {
            return false;
        }
        let data = &self.file.data;
        prefs.setup_step > 0
            || (!prefs.sync_prompt_done && data.projects.is_empty() && data.tasks.is_empty())
    }

    pub fn set_setup_step(&mut self, step: u32) -> Result<(), CoreError> {
        self.mutate(|f, _| {
            f.prefs.setup_step = step;
            Ok(())
        })
    }

    /// Ends the first-run setup, which also answers the prompts it replaced.
    pub fn finish_setup(&mut self) -> Result<(), CoreError> {
        self.mutate(|f, _| {
            f.prefs.setup_done = true;
            f.prefs.sync_prompt_done = true;
            f.prefs.client_zone_prompt_done = true;
            Ok(())
        })
    }

    pub fn delete_tag(&mut self, id: &str) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.delete_tag(c, id))
    }
    pub fn save_template(&mut self, input: &TemplateInput) -> Result<String, CoreError> {
        self.mutate(|f, c| f.data.save_template(c, input))
    }
    pub fn delete_template(&mut self, id: &str) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.delete_template(c, id))
    }
    pub fn task_from_template(
        &mut self,
        id: &str,
        project: Option<&str>,
    ) -> Result<String, CoreError> {
        self.mutate(|f, c| f.data.task_from_template(c, id, project))
    }
    pub fn summary_preview(&self, req: &SummaryRequest) -> Result<SummaryPreview, CoreError> {
        self.file.data.summary_preview(req)
    }
    pub fn create_summary(&mut self, req: &SummaryRequest) -> Result<String, CoreError> {
        self.mutate(|f, c| {
            let id = f
                .data
                .create_summary_above(c, req, &f.prefs.reference_marks)?;
            f.data.raise_reference_marks(&mut f.prefs.reference_marks);
            Ok(id)
        })
    }
    pub fn void_summary(&mut self, id: &str) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.void_summary(c, id))
    }
    pub fn set_device_prefix(&mut self, prefix: &str) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.set_device_prefix(c, prefix))
    }

    /// The record of changes to one item, newest first, or all of them.
    #[must_use]
    pub fn audit_entries(&self, entity_id: Option<&str>) -> Vec<AuditEntry> {
        let mut out: Vec<AuditEntry> = self
            .file
            .data
            .audit_log
            .values()
            .filter(|a| entity_id.is_none_or(|id| a.entity_id == id))
            .cloned()
            .collect();
        out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// Called every few seconds with the OS's idle time. Returns true when time away was found.
    pub fn observe_activity(&mut self, idle_secs: Option<u64>) -> Result<bool, CoreError> {
        let device = self.file.device_id.clone();
        let running = self.file.data.own_open_segments(&device).into_iter().next();
        let threshold = minutes_setting(&self.file.data, "idleMinutes", DEFAULT_IDLE_MINUTES);
        let gaps = self.idle.tick(
            self.now(),
            running.is_some(),
            idle_secs,
            u64::try_from(threshold).unwrap_or(0) * 60,
        );
        if let Some(segment) = running {
            self.unsaved_gaps
                .extend(gaps.into_iter().map(|g| (segment.clone(), g)));
        }
        self.save_away()
    }

    /// The OS says whether the session is locked. `None` when it can't say, which ends a lock if it
    /// goes on too long.
    pub fn observe_lock(&mut self, locked: Option<bool>) -> Result<bool, CoreError> {
        self.observe_lock_at(0, locked)
    }

    /// As `observe_lock`, as if it had been reported this many seconds ago.
    pub fn observe_lock_at(&mut self, ago: i64, locked: Option<bool>) -> Result<bool, CoreError> {
        let at = Ts(self.now().secs() - ago);
        let device = self.file.device_id.clone();
        let running = self.file.data.own_open_segments(&device).into_iter().next();
        // A lock taken from here on is on the new clock, if it has just been put back.
        let mut gaps = self.idle.notice_clock(self.now());
        gaps.extend(match locked {
            Some(locked) => self.idle.set_locked(at, locked),
            None => self.idle.lock_unknown(at),
        });
        if let Some(segment) = running {
            self.unsaved_gaps
                .extend(gaps.into_iter().map(|g| (segment.clone(), g)));
        }
        self.save_away()
    }

    /// Writes found time away, the absence in progress and any clock put back under a running timer. True
    /// when time away was recorded.
    fn save_away(&mut self) -> Result<bool, CoreError> {
        self.unsaved_steps.extend(self.idle.take_steps_back());
        // With no timer running, a clock put back has nothing to do here.
        if !self.unsaved_steps.is_empty()
            && self
                .file
                .data
                .own_open_segments(&self.file.device_id)
                .is_empty()
        {
            self.unsaved_steps.clear();
        }
        let away = self.idle.away_since();
        let kind = self.idle.away_kind();
        if self.unsaved_gaps.is_empty()
            && self.unsaved_steps.is_empty()
            && self.file.prefs.away_since == away
            && self.file.prefs.away_kind == kind
        {
            return Ok(false);
        }
        let gaps = self.unsaved_gaps.clone();
        let steps = self.unsaved_steps.clone();
        // After a crash the later of the store's own heartbeat and its beat is believed, and a beat on the old
        // clock would run the new stretch on to it. The beat goes back first: a crash between the two writes then
        // ends the old stretch early, which the notice says, rather than the new one too late.
        if !steps.is_empty() {
            let _ = self.store.rewind_heartbeat(self.now());
        }
        let restarted = self.mutate_system(|f, c| {
            for (segment, g) in &gaps {
                f.data.add_gap(c, segment, g.start, g.end, g.kind);
            }
            let mut restarted = Vec::new();
            for step in &steps {
                // From when the clock was found changed, even if the save that says so comes later. Its stamps are
                // this commit's.
                let at = Ctx {
                    now: step.now,
                    stamp_at: c.stamp_at,
                    device: c.device,
                };
                restarted.push((*step, f.data.restart_after_clock_step(&at, step.last)));
            }
            if !steps.is_empty() {
                f.last_heartbeat = Some(c.now);
            }
            f.prefs.away_since = away;
            f.prefs.away_kind = kind;
            Ok(restarted)
        })?;
        self.unsaved_gaps.clear();
        self.unsaved_steps.clear();
        for (step, tasks) in &restarted {
            self.tell_clock_put_back(*step, tasks);
        }
        Ok(!gaps.is_empty())
    }

    /// A timer stays running through this, so nothing else would tell the user why its time changed.
    fn tell_clock_put_back(&mut self, step: ClockStep, tasks: &[String]) {
        if tasks.is_empty() {
            return;
        }
        let names: Vec<String> = tasks.iter().map(|t| format!("\"{t}\"")).collect();
        let message = format!(
            "This computer's clock went back from {} to {} while {} was running. The time so far stays as it was logged, and the timer goes on from the new time. Check those times in the task's details.",
            self.when_text(step.last),
            self.when_text(step.now),
            names.join(", "),
        );
        self.clock_alerts.push(message.clone());
        self.notify("warning", message);
    }

    /// Every path that closes the running segment comes through here first, so time away already
    /// under way is recorded and never silently counts as work.
    pub(crate) fn settle_away(&mut self) -> Result<(), CoreError> {
        let device = self.file.device_id.clone();
        let running = self.file.data.own_open_segments(&device).into_iter().next();
        let gaps = self.idle.settle(self.now());
        if let Some(segment) = running {
            self.unsaved_gaps
                .extend(gaps.into_iter().map(|g| (segment.clone(), g)));
        }
        self.save_away().map(|_| ())
    }

    #[must_use]
    pub fn current_reminder(&self) -> Option<Reminder> {
        if self.reminder_quiet_until.is_some_and(|q| q > self.now()) {
            return None;
        }
        let data = &self.file.data;
        reminder::due(
            data,
            &self.file.device_id,
            self.now(),
            minutes_setting(data, "longRunMinutes", reminder::DEFAULT_LONG_RUN_MINUTES),
            data.settings
                .get("remindPastShift")
                .and_then(|e| e.value.as_bool())
                .unwrap_or(true),
        )
    }

    /// Quiets the reminder for a while. Keeps running: this isn't a pause.
    pub fn snooze_reminder(&mut self, minutes: i64) {
        self.reminder_quiet_until = Some(self.now().later(minutes.clamp(1, 24 * 60) * 60));
    }

    fn figures(&self) -> BTreeMap<String, ProjectFigures> {
        let tz = crate::validate::parse_zone(&self.system_zone).unwrap_or(chrono_tz::UTC);
        let source = self.end_source();
        project_figures(&self.file.data, tz, &|s: &Segment| source.end_of(s))
    }

    /// Pauses by request and timers a delete elsewhere stopped, not yet offered as a system notification,
    /// for a window that may be hidden. Each is also a dismissible notice, so a person who doesn't see this
    /// still reads it in the app.
    pub fn take_pause_alerts(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pause_alerts)
    }

    /// What the owner of a task needs to hear: it stopped by itself, and time since the request is gone.
    fn pause_request_message(&self, due: &PauseRequestDue) -> String {
        let task = self
            .file
            .data
            .tasks
            .get(&due.task_id)
            .map_or("A task", |t| t.name.as_str());
        let device = self
            .file
            .remote_devices
            .get(&due.requested_by)
            .map_or("another device", |d| d.name.as_str());
        let when = self.when_text(due.at);
        format!(
            "\"{task}\" was paused because {device} asked for it. Its time stopped at {when}, when that was asked, so anything you did on it after then isn't counted. Resume it if you're still working."
        )
    }

    /// A moment as the user reads it, in the zone they are viewing.
    pub(crate) fn when_text(&self, at: Ts) -> String {
        let zone = self.viewing_zone();
        chrono::DateTime::from_timestamp(at.secs(), 0).map_or_else(
            || at.iso(),
            |t| t.with_timezone(&zone).format("%Y-%m-%d %H:%M").to_string(),
        )
    }

    /// What deserves a system notification now. Each budget threshold and reminder notifies once.
    pub fn take_alerts(&mut self) -> Vec<String> {
        let mut out = std::mem::take(&mut self.clock_alerts);
        let figures = self.figures();
        let mut seen = self.file.prefs.budget_alerts_seen.clone();
        for (id, f) in &figures {
            let told = seen.get(id).copied().unwrap_or(0);
            let level = f.budget.as_ref().map_or(0, |b| b.level);
            // Fallen below what was last told, as when a budget is raised: reaching it again is news.
            if level < told {
                seen.insert(id.clone(), level);
            }
            let Some(b) = &f.budget else { continue };
            if level > told
                && let Some(p) = self.file.data.projects.get(id).filter(|p| !p.archived)
            {
                out.push(format!(
                    "{} has reached {}% of its {}.",
                    p.name,
                    b.percent,
                    if b.basis == "fee" {
                        "fixed fee"
                    } else {
                        "budget"
                    }
                ));
                seen.insert(id.clone(), b.level);
            }
        }
        if seen != self.file.prefs.budget_alerts_seen {
            let _ = self.mutate(|f, _| {
                f.prefs.budget_alerts_seen = seen;
                Ok(())
            });
        }
        let reminder = self.current_reminder();
        if reminder != self.reminder_told {
            if let Some(r) = &reminder {
                let name = |id: &str| {
                    self.file
                        .data
                        .tasks
                        .get(id)
                        .map_or_else(String::new, |t| t.name.clone())
                };
                out.push(match r {
                    Reminder::LongRun { task_id, .. } => {
                        format!(
                            "{} has been running a long time. Still working on it?",
                            name(task_id)
                        )
                    }
                    Reminder::PastShift { task_id, shift, .. } => {
                        format!("{} is still running after {shift} ended.", name(task_id))
                    }
                });
            }
            self.reminder_told = reminder;
        }
        out
    }

    /// Writes today's dated copy if it's missing. None while a first start waits for its data from the folders.
    pub fn run_daily_backup(&self) -> Result<bool, CoreError> {
        if self.awaiting_cloud() {
            return Ok(false);
        }
        backup::daily(&self.paths, &self.file, self.now(), self.backup_keep())
    }

    fn backup_keep(&self) -> u32 {
        self.file.prefs.backup_keep.unwrap_or(backup::DEFAULT_KEEP)
    }

    pub fn set_backup_keep(&mut self, keep: u32) -> Result<(), CoreError> {
        if !(1..=365).contains(&keep) {
            return Err(CoreError::Validation(
                "keep between 1 and 365 backups".into(),
            ));
        }
        self.mutate(|f, _| {
            f.prefs.backup_keep = Some(keep);
            Ok(())
        })
    }

    pub fn list_backups(&self) -> Result<Vec<BackupInfo>, CoreError> {
        Ok(backup::list(&self.paths)?)
    }

    /// Rolls replicated data and settings back to a backup, keeping a safety copy of the current
    /// store first. What belongs to this device stays: its preferences and sync folders, the reference
    /// prefixes it has (one only the backup knows comes back) and the marks, so no number is reused, and
    /// the audit trail, which only grows.
    pub fn restore_backup(&mut self, name: &str) -> Result<String, CoreError> {
        self.lease_gate()?;
        let mut restored = backup::read(&self.paths, name)?;
        if backup::is_legacy_copy(name) {
            // 1.3.1 kept its latest heartbeat in a file beside its store and read the two together, so a run it timed
            // up to then isn't cut back to the store's last save. That file is only read.
            take_legacy_beat(&self.paths.dir, &mut restored, self.now());
        }
        self.refuse_backup_from_before_a_restore(&restored)?;
        self.on_exit()?;
        let safety = backup::safety_copy(&self.paths, &self.file, self.now())?;
        let now = self.now();
        let names = self.install_restored(
            restored.data,
            RestoreNote {
                entity: "backup",
                name: name.to_owned(),
                after: serde_json::json!({"safetyCopy": safety}),
                // A task the backup held running stopped being timed when the backup was taken.
                closed_at: restored.last_heartbeat.unwrap_or(Ts(i64::MIN / 2)),
                reason: PauseReason::BackupRestored,
                last_heartbeat: restored.last_heartbeat,
                epoch: None,
            },
        )?;
        if self.lease.active() {
            self.lease.note_change(now);
        }
        self.notify(
            "info",
            format!("Restored {name}. The data just before it is kept as {safety}."),
        );
        if !names.is_empty() {
            self.notify(
                "info",
                format!(
                    "{} was running when this backup was taken. It is paused at the last moment the backup knows about ({}).",
                    names.join(", "),
                    self.file.last_heartbeat.map_or_else(|| "its start".to_owned(), |h| h.iso()),
                ),
            );
        }
        if !self.file.prefs.sync_targets.is_empty() {
            self.notify(
                "warning",
                "Sync is on, so newer records in your sync folders will merge back in at the next sync.".to_owned(),
            );
        }
        Ok(safety)
    }

    /// A backup keeps this device's preferences, so it would carry data from before a restored version past the
    /// fence, and undo that restore on every computer.
    fn refuse_backup_from_before_a_restore(&self, backup: &LocalFile) -> Result<(), CoreError> {
        let current = self.file.prefs.epoch.as_ref();
        let older = EpochInfo::key(backup.prefs.epoch.as_ref()) < EpochInfo::key(current);
        match current {
            Some(info) if older && !self.file.prefs.sync_targets.is_empty() => {
                Err(CoreError::Conflict(format!(
                    "This backup is from before the version of {} was restored. Restoring it would undo that on every computer. Use Versions instead.",
                    self.when_text(info.base_taken_at),
                )))
            }
            _ => Ok(()),
        }
    }

    /// Replaces the replicated data, in one save, keeping what belongs to this device: its preferences and devices,
    /// the reference prefixes it has and the marks, and the audit trail. Starts cold. Returns the tasks these data held
    /// running here, now paused. Callers answer for the lease and for telling the user.
    pub(crate) fn install_restored(
        &mut self,
        mut data: Shared,
        note: RestoreNote,
    ) -> Result<Vec<String>, CoreError> {
        let mut prefs = self.file.prefs.clone();
        self.file
            .data
            .raise_reference_marks(&mut prefs.reference_marks);
        data.raise_reference_marks(&mut prefs.reference_marks);
        prefs.away_since = None;
        prefs.away_kind = None;
        // The user's choice of data, so the wait for the folders' is over.
        prefs.awaiting_cloud = false;
        if note.epoch.is_some() {
            prefs.epoch = note.epoch;
        }
        // The prefixes this device has stand, and those only the restored data knows come back with it: a store that
        // started empty doesn't give up the prefix this device was billing under, nor what it knew of the others'.
        let mut prefixes = std::mem::take(&mut data.prefixes);
        prefixes.extend(self.file.data.prefixes.clone());
        data.prefixes = prefixes;
        data.audit_log.extend(self.file.data.audit_log.clone());
        data.seed_tags();
        data.normalize();
        let now = self.now();
        let device = self.file.device_id.clone();
        // What these data hold has been seen too, as far as a reader takes a stamp (`seen_in`), and the floor never goes
        // down, so stamps made from here on are still later than those the restore took away.
        self.newest = hlc::later(self.newest.take(), seen_in(&data, now));
        let ctx = Ctx::hlc(now, &device, self.newest.as_ref());
        let names = data.close_own_open(&ctx, note.closed_at, &note.reason);
        data.audit(
            &ctx,
            note.entity,
            &note.name,
            "restore",
            "",
            Value::Null,
            note.after,
        );
        let mut next = LocalFile {
            version: self.file.version,
            device_id: device.clone(),
            device_name: self.file.device_name.clone(),
            last_heartbeat: note.last_heartbeat,
            prefs,
            remote_devices: self.file.remote_devices.clone(),
            newest_stamp: self.newest.clone(),
            latest_change_here: self.file.latest_change_here,
            data,
        };
        // The beat of the data replaced goes with it, in the same transaction.
        save_with_floor(
            &mut self.store,
            &mut self.newest,
            &self.file,
            &mut next,
            true,
            now,
        )?;
        self.file = next;
        self.revision += 1;
        self.warm = false;
        self.last_worked = None;
        Ok(names)
    }

    pub fn save_shift(&mut self, input: &ShiftInput) -> Result<String, CoreError> {
        self.mutate(|f, c| f.data.save_shift(c, input))
    }
    pub fn delete_shift(&mut self, shift_id: &str) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.delete_shift(c, shift_id))
    }
    pub fn set_rate(&mut self, input: &RateInput) -> Result<(), CoreError> {
        self.mutate(|f, c| {
            f.data.set_rate(c, input)?;
            f.prefs.last_currency = Some(input.currency.clone());
            Ok(())
        })
    }
    pub fn delete_rate_version(
        &mut self,
        id: &str,
        confirm_retroactive: bool,
    ) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.delete_rate_version(c, id, confirm_retroactive))
    }
    pub fn set_rounding(&mut self, input: &RoundingInput, today: &str) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.set_rounding(c, input, today))
    }
    pub fn delete_rounding(&mut self, id: &str) -> Result<(), CoreError> {
        self.mutate(|f, c| f.data.delete_rounding(c, id))
    }
    pub fn set_setting(&mut self, key: &str, value: Value) -> Result<(), CoreError> {
        if VIEW_SETTINGS.contains(&key) {
            return self.mutate_system(|f, c| f.data.set_setting(c, key, value));
        }
        self.mutate(|f, c| f.data.set_setting(c, key, value))
    }

    pub fn set_theme(&mut self, theme: Theme) -> Result<(), CoreError> {
        self.mutate(|f, _| {
            f.prefs.theme = theme;
            Ok(())
        })
    }
    pub fn set_hide_earnings(&mut self, hide: bool) -> Result<(), CoreError> {
        self.mutate(|f, _| {
            f.prefs.hide_earnings = hide;
            Ok(())
        })
    }
    pub fn mark_client_zone_prompt_done(&mut self) -> Result<(), CoreError> {
        self.mutate(|f, _| {
            f.prefs.client_zone_prompt_done = true;
            Ok(())
        })
    }
    pub fn mark_sync_prompt_done(&mut self) -> Result<(), CoreError> {
        self.mutate(|f, _| {
            f.prefs.sync_prompt_done = true;
            Ok(())
        })
    }
    pub fn mark_hide_notice_shown(&mut self) -> Result<(), CoreError> {
        self.mutate(|f, _| {
            f.prefs.hide_notice_shown = true;
            Ok(())
        })
    }

    /// The spelling a connected folder has, when `root` is that folder spelled another way.
    #[must_use]
    pub fn connected_as(&self, root: &str) -> Option<&str> {
        let targets = &self.file.prefs.sync_targets;
        if targets.iter().any(|t| t.root == root) {
            return None;
        }
        let key = folder_key(root);
        targets
            .iter()
            .map(|t| t.root.as_str())
            .find(|r| folder_key(r) == key)
    }

    /// Refuses a folder that is connected already under another spelling, as it holds the same files.
    pub fn folder_gate(&self, root: &str) -> Result<(), CoreError> {
        match self.connected_as(root) {
            Some(existing) => Err(CoreError::Validation(format!(
                "That folder is already connected as {existing}."
            ))),
            None => Ok(()),
        }
    }

    pub fn add_sync_target(&mut self, target: SyncTarget) -> Result<(), CoreError> {
        if target.root.trim().is_empty() {
            return Err(CoreError::Validation("choose a folder".into()));
        }
        self.folder_gate(&target.root)?;
        let under_way = self.setup_pending();
        self.mutate(|f, _| {
            if under_way {
                f.prefs.setup_step = f.prefs.setup_step.max(1);
            }
            f.prefs.sync_prompt_done = true;
            if !f.prefs.sync_targets.iter().any(|t| t.root == target.root) {
                f.prefs.sync_targets.push(target);
            }
            Ok(())
        })?;
        self.lease.targets_changed(true);
        Ok(())
    }

    /// Stops using a folder. Everything already in it stays where it is. Connecting it again merges with it, as
    /// with a folder never connected before.
    pub fn remove_sync_target(&mut self, root: &str) -> Result<(), CoreError> {
        self.targets.remove(root);
        self.mutate(|f, _| {
            f.prefs.sync_targets.retain(|t| t.root != root);
            Self::forget_folder(f, root);
            f.prefs.awaiting_cloud = false;
            Ok(())
        })?;
        self.lease
            .targets_changed(!self.file.prefs.sync_targets.is_empty());
        Ok(())
    }

    fn forget_folder(f: &mut LocalFile, root: &str) {
        if let Some(joined) = &mut f.prefs.sync_joined {
            joined.retain(|r| r != root);
        }
        if f.prefs
            .join_pending
            .as_ref()
            .is_some_and(|p| p.root == root)
        {
            f.prefs.join_pending = None;
        }
    }

    /// True while a first start waits for its data from the sync folders (see `LocalPrefs::awaiting_cloud`).
    #[must_use]
    pub fn awaiting_cloud(&self) -> bool {
        self.file.prefs.awaiting_cloud
    }

    /// Ends a first start once a folder has given data. Takes this device's heartbeat from its own file, so a run that
    /// file knows of is not forgotten, then puts back the drafts that waited for the data. Returns true when it ended it
    /// now.
    pub(crate) fn finish_awaiting(&mut self, arrived: bool, own_beat: Option<Ts>) -> bool {
        if !self.file.prefs.awaiting_cloud || !arrived {
            return false;
        }
        let now = self.now();
        let ended = self
            .mutate_system(|f, _| {
                f.prefs.awaiting_cloud = false;
                f.last_heartbeat = f.last_heartbeat.max(own_beat.map(|b| b.min(now)));
                Ok(())
            })
            .is_ok();
        if ended {
            self.put_back_drafts();
        }
        ended
    }

    /// The drafts a first start left on disk until its data had arrived, put back as a launch puts them back. If the
    /// store can't take them they stay on disk for the next launch, and the next save writes every row.
    pub(crate) fn put_back_drafts(&mut self) {
        let before = self.file.clone();
        let kept_drafts = self.replay_drafts();
        let now = self.now();
        match save_with_floor(
            &mut self.store,
            &mut self.newest,
            &before,
            &mut self.file,
            false,
            now,
        ) {
            Ok(_) => {
                self.revision += 1;
                self.settle_drafts(kept_drafts);
            }
            Err(e) => eprintln!("could not save the words put back: {e}"),
        }
    }

    /// Makes every save fail inside its transaction, as a full disk would, or lets saves through again.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn set_saves_blocked(&mut self, blocked: bool) {
        self.store.block_saves(blocked);
    }

    /// The store as it is on disk, read again.
    #[cfg(test)]
    pub(crate) fn stored(&self) -> LocalFile {
        self.store.reload().unwrap()
    }

    /// The store's record rows as they are on disk (kind, id, JSON text).
    #[cfg(test)]
    pub(crate) fn stored_rows(&self) -> Vec<(String, String, String)> {
        self.store.rows()
    }

    /// Records a heartbeat while something is running, so a crash loses at most one interval.
    pub fn heartbeat(&mut self) -> Result<(), CoreError> {
        if self
            .file
            .data
            .own_open_segments(&self.file.device_id)
            .is_empty()
        {
            return Ok(());
        }
        let now = self.now();
        self.store.save_heartbeat(now)?;
        self.file.last_heartbeat = Some(now);
        Ok(())
    }

    /// The real exit: closes this device's open segments now and marks their tasks paused.
    pub fn on_exit(&mut self) -> Result<(), CoreError> {
        // Words a failed write left only in a notice get one more try.
        let _ = self.save_kept();
        self.settle_away()?;
        if self
            .file
            .data
            .own_open_segments(&self.file.device_id)
            .is_empty()
        {
            return Ok(());
        }
        self.mutate_system(|f, c| {
            f.data.close_own_open(c, c.now, &PauseReason::AppQuit);
            f.last_heartbeat = Some(c.now);
            Ok(())
        })
    }

    /// Merges what other devices wrote. Local invariants are then restored, touching only segments
    /// this device owns. Persisted only when something changed.
    pub fn absorb_remote(
        &mut self,
        incoming: &Shared,
        seen: BTreeMap<String, RemoteDevice>,
        own_files: Vec<DeviceFile>,
    ) -> bool {
        // Where each other clock was when its file was written, as just read, for the clock-behind warning.
        for (id, device) in &seen {
            if *id != self.file.device_id {
                self.others_written.insert(id.clone(), device.written_at);
            }
        }
        // What this may close has to end where it ended on the old clock, if that has just been put back.
        let found = self.idle.notice_clock(self.now());
        self.unsaved_steps.extend(self.idle.take_steps_back());
        if !self.unsaved_steps.is_empty() {
            let device = self.file.device_id.clone();
            let running = self.file.data.own_open_segments(&device).into_iter().next();
            if let Some(segment) = running {
                self.unsaved_gaps
                    .extend(found.into_iter().map(|g| (segment.clone(), g)));
            }
            // If this can't be saved it stays for the next save, and so does what it found.
            let _ = self.save_away();
        }
        let mut merged = merge(&self.file.data, incoming);
        // This device's own file can hold what a restored store lost.
        let mut last_beat = None;
        for own in &own_files {
            last_beat = last_beat.max(own.last_heartbeat);
            merged = merge(&merged, &own.data);
        }
        merged.normalize();
        // An open segment of this device that the store doesn't hold belongs to a past run, whichever
        // file brought it back. Nothing is timing it now, so it closes at the last known heartbeat.
        for seg in merged.segments.values_mut() {
            let past_run = seg.end.is_none()
                && seg.device_id == self.file.device_id
                && !self.file.data.segments.contains_key(&seg.id);
            if past_run {
                seg.end = Some(last_beat.unwrap_or(seg.start).max(seg.start));
            }
        }
        let mut known = self.file.remote_devices.clone();
        for (id, device) in seen {
            match known.get(&id) {
                Some(old) if old.written_at >= device.written_at => {}
                _ => {
                    known.insert(id, device);
                }
            }
        }
        let data_changed = merged != self.file.data;
        let devices_changed = known != self.file.remote_devices;
        if data_changed {
            // Seen now, whether or not the save below works, so what `reconcile` stamps in it beats what just arrived.
            // Only what passed the readers' checks arrived, so nothing more than two days ahead, but what was here may be
            // a restore's (`seen_in`).
            let seen = seen_in(&merged, self.now());
            self.newest = hlc::later(self.newest.take(), seen);
        }
        if !data_changed && !devices_changed {
            let device = self.file.device_id.clone();
            let mut probe = self.file.data.clone();
            let reconciled = probe.reconcile(&Ctx::hlc(self.now(), &device, self.newest.as_ref()));
            // A settlement the owner's own record replaces waits while it is on a summary, until that is voided.
            if !reconciled && !probe.settlement_due(&device) {
                return false;
            }
        }
        // What `reconcile` is about to close here, and what the user is told about each.
        let device = self.file.device_id.clone();
        let closing = merged.own_segments_to_close(&device);
        let mut paused = merged.pause_requests_due(&device);
        paused.retain(|due| merged.live_task(&due.task_id).is_ok());
        let lost_tasks: BTreeSet<&str> = closing
            .iter()
            .filter_map(|id| merged.segments.get(id))
            .map(|s| s.task_id.as_str())
            .filter(|t| !paused.iter().any(|due| due.task_id == *t))
            .collect();
        let timers_lost: Vec<TimerLost> = lost_tasks
            .into_iter()
            .map(|t| TimerLost::of(merged.tasks.get(t)))
            .collect();
        // As `settle_away` for every other path that closes the running segment, or time away counts as
        // work. If the save fails the gaps wait for the next one, as the segment stays open.
        if !closing.is_empty() {
            let gaps = self.idle.settle(self.now());
            for segment in merged.own_open_segments(&device) {
                self.unsaved_gaps
                    .extend(gaps.iter().map(|g| (segment.clone(), *g)));
            }
        }
        let (away, away_kind) = (self.idle.away_since(), self.idle.away_kind());
        let gaps = self.unsaved_gaps.clone();
        let outcome = self.mutate_system(|f, c| {
            f.data = merged;
            f.remote_devices = known;
            f.data.reconcile(c);
            // Whether this device or a sync closed the raw end, it's better evidence than a settlement.
            let lifting = f.data.settlement_due(c.device);
            let reclaimed = f.data.reclaim_settled(c);
            for (segment, g) in &gaps {
                // A segment closed at the moment a pause was asked for only has that much at stake.
                let closed = f.data.segments.get(segment).and_then(|s| s.end);
                let end = closed.map_or(g.end, |e| g.end.min(e));
                f.data.add_gap(c, segment, g.start, end, g.kind);
            }
            f.prefs.away_since = away;
            f.prefs.away_kind = away_kind;
            f.data.raise_reference_marks(&mut f.prefs.reference_marks);
            Ok((lifting, reclaimed))
        });
        match outcome {
            Ok((lifting, reclaimed)) => {
                self.unsaved_gaps.clear();
                self.check_recovered();
                // Saved with the segment closed, so a later cycle finds nothing left to do for it.
                for due in &paused {
                    let message = self.pause_request_message(due);
                    self.pause_alerts.push(message.clone());
                    self.notify("warning", message);
                }
                for lost in &timers_lost {
                    let message = format!(
                        "\"{}\" was {} on {}, so its timer here has stopped. {}",
                        lost.name,
                        lost.how,
                        self.device_name_of(&lost.by),
                        lost.then
                    );
                    self.pause_alerts.push(message.clone());
                    self.notify("warning", message);
                }
                for told in &reclaimed {
                    let message = self.reclaim_message(told);
                    self.notify("warning", message);
                }
                data_changed || lifting || !paused.is_empty() || !timers_lost.is_empty()
            }
            Err(e) => {
                self.local_error = Some(format!(
                    "Could not save data merged from the sync folder: {e}"
                ));
                false
            }
        }
    }

    /// What the owner of a segment hears about a settlement that its own record replaced, or couldn't.
    fn reclaim_message(&self, told: &Reclaimed) -> String {
        match told {
            Reclaimed::Returned {
                task_name,
                settled_by,
                secs,
            } => format!(
                "\"{task_name}\": {} logged here after {} closed it is back. Correct it in task details if that time wasn't work.",
                span_text(*secs),
                self.device_name_of(settled_by),
            ),
            Reclaimed::OnSummary {
                task_name,
                reference,
                secs,
            } => format!(
                "\"{task_name}\": the time on {reference} differs from what this computer recorded by {}. To change it, void the summary, then restore the segment or add the time by hand.",
                span_text(*secs),
            ),
        }
    }

    /// A save of merged data failed. Folder writes don't clear this.
    #[must_use]
    pub fn local_error(&self) -> Option<&str> {
        self.local_error.as_deref()
    }

    /// Any enabled folder's last attempt failed, so the schedule should retry.
    #[must_use]
    pub fn any_target_failed(&self) -> bool {
        self.local_error.is_some()
            || self.file.prefs.sync_targets.iter().any(|t| {
                self.targets
                    .get(&t.root)
                    .is_some_and(|s| s.last_error.is_some())
            })
    }

    /// Makes `to` a sync folder, in place of `replacing` when given. One atomic settings change.
    pub fn switch_targets(
        &mut self,
        replacing: Option<&str>,
        to: SyncTarget,
        pending: Option<PendingCopy>,
    ) -> Result<(), CoreError> {
        self.folder_gate(&to.root)?;
        let under_way = self.setup_pending();
        let result = self.mutate(|f, _| {
            if under_way {
                f.prefs.setup_step = f.prefs.setup_step.max(1);
            }
            f.prefs.sync_prompt_done = true;
            if let Some(old) = replacing {
                f.prefs.sync_targets.retain(|t| t.root != old);
                Self::forget_folder(f, old);
            }
            if !f.prefs.sync_targets.iter().any(|t| t.root == to.root) {
                f.prefs.sync_targets.push(to);
            }
            f.prefs.pending_copy = pending;
            // A folder the user chose: what it holds, or that it holds nothing, is theirs to decide.
            f.prefs.awaiting_cloud = false;
            Ok(())
        });
        if result.is_ok() {
            if let Some(old) = replacing {
                self.targets.remove(old);
            }
            self.lease
                .targets_changed(!self.file.prefs.sync_targets.is_empty());
        }
        result
    }

    pub fn record_sync_failure(&mut self, message: &str) {
        let now = self.now();
        for status in self.targets.values_mut() {
            status.last_error = Some(message.to_owned());
            status.last_error_at = Some(now);
        }
    }

    pub fn target_status_mut(&mut self, root: &str) -> &mut TargetStatus {
        self.targets.entry(root.to_owned()).or_default()
    }

    /// When a folder was last read whole in this run, by this computer's clock.
    pub(crate) fn last_read_at(&self, root: &str) -> Option<Ts> {
        self.targets.get(root).and_then(|s| s.last_read_at)
    }

    #[must_use]
    pub fn device_file(&self) -> DeviceFile {
        DeviceFile {
            schema_version: SCHEMA_VERSION,
            device_id: self.file.device_id.clone(),
            device_name: self.file.device_name.clone(),
            written_at: self.now(),
            last_heartbeat: self.file.last_heartbeat,
            epoch_info: self.file.prefs.epoch.clone(),
            data: self.file.data.clone(),
        }
    }

    #[must_use]
    pub fn export_source(&self) -> ExportSource {
        ExportSource {
            only: None,
            data: self.file.data.clone(),
            device_id: self.file.device_id.clone(),
            remote_devices: self.file.remote_devices.clone(),
            now: self.now(),
        }
    }

    /// What `end_of` reads, without a copy of the data it doesn't read. For reports use `export_source`.
    pub(crate) fn end_source(&self) -> ExportSource {
        ExportSource {
            only: None,
            data: Shared {
                pause_requests: self.file.data.pause_requests.clone(),
                ..Shared::default()
            },
            device_id: self.file.device_id.clone(),
            remote_devices: self.file.remote_devices.clone(),
            now: self.now(),
        }
    }

    pub fn dismiss_notice(&mut self, id: &str) {
        self.notices.retain(|n| n.id != id);
        self.revision += 1;
        // Words kept out of the store end here, and nowhere else. If this write fails the notice just
        // comes back at the next launch.
        if let Some(kept) = self.kept_out.remove(id) {
            self.dismissed_kept.push(kept);
            let _ = self.save_kept();
        }
    }

    #[must_use]
    pub fn hide_notice_shown(&self) -> bool {
        self.file.prefs.hide_notice_shown
    }

    #[must_use]
    pub fn device_name_of(&self, id: &str) -> String {
        self.file
            .remote_devices
            .get(id)
            .map_or_else(|| "another device".to_owned(), |d| d.name.clone())
    }

    /// The newest stamp this device has seen: what a commit's stamps follow (see `hlc`).
    #[must_use]
    pub(crate) fn newest_seen(&self) -> Option<&Stamp> {
        self.newest.as_ref()
    }

    /// This computer's clock is behind another device's, or behind itself, by `hlc::CLOCK_BEHIND_WARN_SECS` or more.
    ///
    /// The clock is compared with wall clocks only, never with a stamp (addendum P13, review F3): with the time each
    /// other device wrote its file by its own clock, as last read (`others_written`), and with the latest time, by this
    /// clock, at which a change was saved here (`LocalFile::latest_change_here`). The hybrid logical clock puts a stamp
    /// past the floor, so a burst of saves on any device reads ahead of every clock, and a stamp made while a clock was
    /// fast stays ahead after it is put right; a file's write time does neither. A change saved here at a time this clock
    /// has not reached again means this clock was set back. The further ahead of the two is named; another device wins a
    /// tie. `capped` is whether this device's stamps are held at the lead cap now, as its floor is a day or more past
    /// the clock. Nothing is kept: the next snapshot after the clock is right says nothing.
    #[must_use]
    pub fn clock_behind(&self) -> Option<ClockBehindView> {
        let now = self.now();
        let other = self
            .others_written
            .iter()
            .max_by_key(|(_, at)| **at)
            .map(|(id, at)| (*at, Some(id.as_str())));
        let here = self.file.latest_change_here.map(|at| (at, None));
        let (newest_at, by) = match (other, here) {
            (Some(o), Some(h)) => {
                if h.0 > o.0 {
                    h
                } else {
                    o
                }
            }
            (o, h) => o.or(h)?,
        };
        let behind_secs = newest_at.secs().saturating_sub(now.secs());
        if behind_secs < CLOCK_BEHIND_WARN_SECS {
            return None;
        }
        let lead = self
            .newest
            .as_ref()
            .map_or(0, |n| n.at.secs().saturating_sub(now.secs()));
        Some(ClockBehindView {
            behind_secs,
            newest_at,
            cause_device: by.map(|id| self.device_name_of(id)),
            capped: lead >= HLC_MAX_LEAD_SECS,
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let data = &self.file.data;
        let own = self.file.device_id.as_str();
        // A summary counts as a task here: it names its project, and a delete can leave it the last thing that does.
        let with_tasks: BTreeSet<&str> = data
            .tasks
            .values()
            .filter(|t| t.is_live())
            .map(|t| t.project_id.as_str())
            .chain(data.summaries.values().map(|b| b.project_id.as_str()))
            .collect();
        // This device goes by the raw end, as it still closes its own. For another, a settlement closes it.
        let open_here_or_there = |s: &Segment| {
            if s.device_id == own {
                s.end.is_none()
            } else {
                s.is_open()
            }
        };
        let open_devices: BTreeSet<&str> = data
            .segments
            .values()
            .filter(|s| {
                open_here_or_there(s) && data.tasks.get(&s.task_id).is_some_and(Task::is_live)
            })
            .map(|s| s.device_id.as_str())
            .collect();

        let figures = self.figures();
        let source = self.end_source();
        let end_of = |s: &Segment| source.end_of(s);
        let all_pieces = pieces::pieces(data, &end_of);
        let totals = task_totals(data, &all_pieces);
        let unconfirmed = pieces::unconfirmed_secs(data);
        let billed = data.billed_segments();

        let projects = data
            .projects
            .values()
            // A tombstoned project that gained a task or a summary on another device stays visible as archived.
            .filter(|p| !p.deleted || with_tasks.contains(p.id.as_str()))
            .map(|p| ProjectView {
                id: p.id.clone(),
                name: p.name.clone(),
                color: p.color.clone(),
                archived: p.archived || p.deleted,
                billable: p.billable,
                budget_hours: p.budget_hours,
                client_zone: p.client_zone.clone(),
                created_at: p.created_at,
                has_tasks: with_tasks.contains(p.id.as_str()),
                billing_type: p.billing_type,
                fixed_fee_cents: p.fixed_fee_cents,
                fee_currency: p.fee_currency.clone(),
                figures: figures.get(&p.id).cloned().unwrap_or_default(),
            })
            .collect();

        let mut open_elsewhere: BTreeMap<&str, &str> = BTreeMap::new();
        let mut open_here: BTreeSet<&str> = BTreeSet::new();
        for s in data.segments.values().filter(|s| open_here_or_there(s)) {
            if s.device_id == own {
                open_here.insert(s.task_id.as_str());
            } else {
                open_elsewhere.insert(s.task_id.as_str(), s.device_id.as_str());
            }
        }
        let tasks = data
            .tasks
            .values()
            .filter(|t| t.is_live())
            .map(|t| TaskView {
                id: t.id.clone(),
                name: t.name.clone(),
                project_id: t.project_id.clone(),
                description: t.description.clone(),
                state: t.state,
                pause_reason: t.pause_reason.clone(),
                created_at: t.created_at,
                stopped_at: t.stopped_at,
                // A task deleted with its time leaves no trace, not even "an earlier task".
                continued_from: t
                    .continued_from
                    .clone()
                    .filter(|from| data.tasks.get(from).is_none_or(|f| f.discarded.is_none())),
                tags: t.tags.clone(),
                active_subtask: t.active_subtask.clone(),
                subtasks: subtasks::items(&t.description)
                    .into_iter()
                    .map(|i| SubtaskView {
                        secs: totals
                            .get(&t.id)
                            .and_then(|x| x.subtask_secs.get(&i.id))
                            .copied()
                            .unwrap_or(0),
                        id: i.id,
                        text: i.text,
                        checked: i.checked,
                    })
                    .collect(),
                billed_secs: totals.get(&t.id).map_or(0, |x| x.billed_secs),
                unbilled_secs: totals.get(&t.id).map_or(0, |x| x.unbilled_secs),
                thinking_secs: totals.get(&t.id).map_or(0, |x| x.thinking_secs),
                pause_secs: totals.get(&t.id).map_or(0, |x| x.pause_secs),
                unconfirmed_secs: unconfirmed.get(&t.id).copied().unwrap_or(0),
                running_elsewhere: if t.state == TaskState::Running
                    && !open_here.contains(t.id.as_str())
                {
                    open_elsewhere
                        .get(t.id.as_str())
                        .map(|d| self.device_name_of(d))
                } else {
                    None
                },
            })
            .collect();

        let mut segments: Vec<SegmentView> = all_pieces
            .iter()
            .map(|p| {
                let device = p.device_id.clone().unwrap_or_default();
                SegmentView {
                    id: p.id.clone(),
                    task_id: p.task_id.clone(),
                    start: p.start,
                    end: (!p.open).then_some(p.end),
                    provisional_end: (p.open && device != own).then_some(p.end),
                    device_id: device,
                    segment_id: p.source_id.clone(),
                    kind: p.kind,
                    subtask_id: p.subtask_id.clone(),
                }
            })
            .collect();
        segments.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.id.cmp(&b.id)));
        let raw_segments = data
            .segments
            .values()
            .filter(|s| data.tasks.get(&s.task_id).is_some_and(Task::is_live))
            .map(|s| {
                let effective = s.effective();
                RawSegmentView {
                    id: s.id.clone(),
                    task_id: s.task_id.clone(),
                    device_id: s.device_id.clone(),
                    raw_start: s.start,
                    raw_end: s.end,
                    start: effective.map_or(s.start, |e| e.0),
                    end: effective.map_or(s.end, |e| e.1),
                    voided: effective.is_none(),
                    corrected: s.edit.as_ref().is_some_and(|e| !e.voided),
                    manual: s.manual,
                    reason: s
                        .edit
                        .as_ref()
                        .map_or_else(String::new, |e| e.reason.clone()),
                    billed: billed.contains(s.id.as_str()),
                }
            })
            .collect();

        let mut warnings = Vec::new();
        if open_devices.len() >= 2 {
            let names: Vec<String> = open_devices
                .iter()
                .map(|d| {
                    if *d == own {
                        format!("{} (this device)", self.file.device_name)
                    } else {
                        self.device_name_of(d)
                    }
                })
                .collect();
            warnings.push(format!(
                "Tasks are running on more than one device at once: {}. Both are kept as logged; pause one of them on its own device.",
                names.join(", ")
            ));
        }

        warnings.extend(self.local_error.clone());
        if self.packaged && self.file.prefs.sync_targets.is_empty() {
            warnings.push(
                "This copy of zWorkTracker keeps its data in a private Windows folder. Uninstalling the app deletes that folder and your hours with it. Connect a OneDrive or Google Drive folder in Settings > Sync between devices.".to_owned(),
            );
        }
        warnings.extend(self.folders_failing());
        warnings.extend(self.awaiting_warning());
        for (kind, colors) in [
            (
                "projects",
                data.projects
                    .values()
                    .filter(|p| !p.deleted && !p.archived)
                    .map(|p| (p.name.as_str(), p.color.as_str()))
                    .collect::<Vec<_>>(),
            ),
            (
                "tags",
                data.tags
                    .values()
                    .filter(|t| !t.deleted)
                    .map(|t| (t.name.as_str(), t.color.as_str()))
                    .collect::<Vec<_>>(),
            ),
        ] {
            let mut by_color: BTreeMap<String, Vec<&str>> = BTreeMap::new();
            for (name, color) in colors {
                by_color
                    .entry(color.to_ascii_lowercase())
                    .or_default()
                    .push(name);
            }
            for names in by_color.values().filter(|n| n.len() > 1) {
                warnings.push(format!(
                    "These {kind} have the same colour, probably from two devices: {}. Change one if that is confusing.",
                    names.join(", ")
                ));
            }
        }
        let repeated = data.reference_clashes();
        if !repeated.is_empty() {
            warnings.push(format!(
                "More than one billing summary uses the same reference: {}. Change a device's prefix and void and reissue one of them.",
                repeated.join(", ")
            ));
        }
        for references in data.billed_twice() {
            warnings.push(format!(
                "More than one billing summary holds the same time: {}. Void one of them, so it isn't billed twice.",
                references.join(", ")
            ));
        }
        let short = data.pause_time_short();
        for id in short.keys() {
            if let Some(b) = data.summaries.get(*id) {
                warnings.push(format!(
                    "Billing summary {} holds pause time that no longer matches what's logged, so it could be billed twice. Void and reissue it.",
                    b.reference
                ));
            }
        }
        let clashes: Vec<String> = data
            .prefix_clashes(own)
            .iter()
            .map(|d| self.device_name_of(d))
            .collect();
        if !clashes.is_empty() {
            warnings.push(format!(
                "Another device uses the same reference prefix: {}. Choose a different prefix, or references can repeat.",
                clashes.join(", ")
            ));
        }

        let targets = self
            .file
            .prefs
            .sync_targets
            .iter()
            .map(|t| TargetView {
                provider: t.provider,
                root: t.root.clone(),
                folder: PathBuf::from(&t.root)
                    .join(APP_FOLDER)
                    .to_string_lossy()
                    .into_owned(),
                status: {
                    let mut status = self.targets.get(&t.root).cloned().unwrap_or_default();
                    // Conflict copies and damage in the lease folder count with the device files'.
                    status.ignored_files += self.lease_io.ignored(&t.root);
                    status
                },
            })
            .collect();

        Snapshot {
            revision: self.revision,
            now: self.now(),
            device_id: self.file.device_id.clone(),
            device_name: self.file.device_name.clone(),
            system_zone: self.system_zone.clone(),
            tzdb_version: chrono_tz::IANA_TZDB_VERSION,
            projects,
            tasks,
            segments,
            shifts: data
                .shifts
                .values()
                .filter(|s| !s.deleted)
                .cloned()
                .collect(),
            rates: data
                .rates
                .values()
                .filter(|r| !r.deleted)
                .cloned()
                .collect(),
            rounding: data
                .rounding
                .values()
                .filter(|r| !r.deleted)
                .cloned()
                .collect(),
            settings: data
                .settings
                .iter()
                .map(|(k, v)| (k.clone(), v.value.clone()))
                .collect(),
            prefs: PrefsView {
                theme: self.file.prefs.theme,
                hide_earnings: self.file.prefs.hide_earnings,
                last_currency: self.file.prefs.last_currency.clone(),
                client_zone_prompt_done: self.file.prefs.client_zone_prompt_done,
            },
            setup: SetupView {
                pending: self.setup_pending(),
                step: self.file.prefs.setup_step,
            },
            pause_requests: data
                .pause_requests
                .values()
                .filter(|q| data.tasks.get(&q.task_id).is_some_and(Task::is_live))
                .cloned()
                .collect(),
            notices: self.notices.clone(),
            warnings,
            clock_behind: self.clock_behind(),
            sync: SyncView {
                targets,
                detected: self.detected.clone(),
                prompt_pending: !self.file.prefs.sync_prompt_done
                    && self.file.prefs.sync_targets.is_empty(),
                pending_copy: self.file.prefs.pending_copy.clone(),
                join_pending: self
                    .file
                    .prefs
                    .join_pending
                    .as_ref()
                    .map(|p| JoinPendingView {
                        root: p.root.clone(),
                        taken_from: p.info.restored_from.clone(),
                        at: p.info.base_taken_at,
                        device_name: if p.info.by == own {
                            self.file.device_name.clone()
                        } else {
                            self.device_name_of(&p.info.by)
                        },
                    }),
            },
            launch: LaunchView {
                warm: self.warm,
                cold_required: cold_required(data),
                copy_source: if self.warm { self.copy_source() } else { None },
            },
            raw_segments,
            gaps: data
                .gaps
                .values()
                .filter(|g| data.tasks.get(&g.task_id).is_some_and(Task::is_live))
                .cloned()
                .collect(),
            pauses: data
                .pauses
                .values()
                .filter(|p| !p.deleted && data.tasks.get(&p.task_id).is_some_and(Task::is_live))
                .cloned()
                .collect(),
            tags: data.tags.values().filter(|t| !t.deleted).cloned().collect(),
            templates: data
                .templates
                .values()
                .filter(|t| !t.deleted)
                .cloned()
                .collect(),
            billing: BillingView {
                prefix: data.prefix_of(own),
                clashes,
                summaries: data.summaries.values().cloned().collect(),
            },
            reminder: self.current_reminder(),
            reminder_settings: ReminderSettings {
                idle_minutes: minutes_setting(data, "idleMinutes", DEFAULT_IDLE_MINUTES),
                long_run_minutes: minutes_setting(
                    data,
                    "longRunMinutes",
                    reminder::DEFAULT_LONG_RUN_MINUTES,
                ),
                remind_past_shift: data
                    .settings
                    .get("remindPastShift")
                    .and_then(|e| e.value.as_bool())
                    .unwrap_or(true),
            },
            backup_keep: self.backup_keep(),
            has_pdf_logo: self.paths.dir.join(PDF_LOGO_FILE).exists(),
            lease: self.lease_view(),
            platform: PlatformView {
                packaged: self.packaged,
            },
        }
    }

    /// A first start still waiting for its data, once a folder has been tried: nothing is written there until it comes.
    fn awaiting_warning(&self) -> Option<String> {
        let tried = self
            .targets
            .values()
            .any(|s| s.last_read_at.is_some() || s.last_error.is_some());
        (self.file.prefs.awaiting_cloud && tried).then(|| {
            format!(
                "This computer's data hasn't arrived from {} yet, so nothing is written there until it does. Check that the folder is synced to this computer, then press Sync now in Settings > Sync between devices.",
                folder_list(&self.file.prefs.sync_targets)
            )
        })
    }

    /// A folder that has been failing a while. The status says so too, but only on the page with the status.
    fn folders_failing(&self) -> Vec<String> {
        let now = self.now();
        let targets = &self.file.prefs.sync_targets;
        let failing = |t: &SyncTarget| {
            self.targets
                .get(&t.root)
                .is_some_and(|s| s.last_error.is_some())
        };
        targets
            .iter()
            .filter_map(|t| {
                let status = self.targets.get(&t.root)?;
                let error = status.last_error.as_deref()?;
                // What a run knows: a folder failing since the start of this run hasn't been written in it.
                let since = status.last_written_at.unwrap_or(self.opened);
                let age = now.secs() - since.secs();
                if age < FOLDER_FAILING_WARNING_SECS {
                    return None;
                }
                let stop = if error.ends_with(['.', '?', '!']) {
                    ""
                } else {
                    "."
                };
                let safe = targets
                    .iter()
                    .find(|other| other.root != t.root && !failing(other))
                    .map_or_else(
                        || "Your data is safe on this computer.".to_owned(),
                        |other| {
                            format!(
                                "{} folder is fine, so your data is safe there.",
                                folder_label(other)
                            )
                        },
                    );
                Some(format!(
                    "{} folder hasn't been written since {} ago: {error}{stop} {safe}",
                    folder_label(t),
                    span_text(age),
                ))
            })
            .collect()
    }
}

/// A timer here that a sync is about to close, and how to say so.
struct TimerLost {
    name: String,
    /// The device that did it.
    by: String,
    how: &'static str,
    then: &'static str,
}

impl TimerLost {
    fn of(task: Option<&Task>) -> Self {
        const KEPT: &str = "The time up to now is kept.";
        let (how, by, then) = match task {
            Some(t) if t.discarded.is_some() => (
                "deleted",
                t.discarded.as_ref().map(|d| d.by.as_str()),
                "The time logged on it is gone from every view.",
            ),
            Some(t) if !t.is_live() => ("deleted", Some(t.deleted_stamp.by.as_str()), KEPT),
            Some(t) if t.state == TaskState::Stopped => {
                ("stopped", Some(t.state_stamp.by.as_str()), KEPT)
            }
            Some(t) if t.state == TaskState::Paused => (
                "paused",
                Some(t.state_stamp.by.as_str()),
                "The time up to now is kept. Resume it if you're still working.",
            ),
            Some(t) => ("changed", Some(t.state_stamp.by.as_str()), KEPT),
            None => ("removed", None, KEPT),
        };
        Self {
            name: task.map_or_else(|| "A task".to_owned(), |t| t.name.clone()),
            by: by.unwrap_or_default().to_owned(),
            how,
            then,
        }
    }
}

/// A folder's path as compared with another's: the same folder can be written with separators at the end and, on
/// Windows, in another letter case or with / for \. A drive's root keeps its separator, as `C:` alone is not that root.
fn folder_key_for(root: &str, windows: bool) -> String {
    let (mut key, sep) = if windows {
        (root.replace('/', "\\").to_lowercase(), '\\')
    } else {
        (root.to_owned(), '/')
    };
    while key.len() > 1 && key.ends_with(sep) && !(windows && key.ends_with(":\\")) {
        key.pop();
    }
    key
}

fn folder_key(root: &str) -> String {
    folder_key_for(root, cfg!(windows))
}

fn name_device(e: CoreError, devices: BTreeMap<String, RemoteDevice>) -> CoreError {
    match e {
        CoreError::RunningElsewhere(id) => CoreError::RunningElsewhere(
            devices
                .get(&id)
                .map_or_else(|| "another device".to_owned(), |d| d.name.clone()),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::crash_image;
    use crate::time::ManualClock;

    const T0: &str = "2026-03-01T09:00:00Z";

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
            description: doc("Details"),
            continued_from: None,
            tags: vec!["tag-concept".to_owned()],
        }
    }
    fn doc(text: &str) -> Value {
        serde_json::json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":text}]}]})
    }
    fn open(dir: &Path, clock: &ManualClock) -> Service {
        Service::open(dir, Arc::new(clock.clone()), "pc").unwrap()
    }

    fn running(dir: &Path, clock: &ManualClock) -> (Service, String) {
        let mut s = open(dir, clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        s.start_task(&t).unwrap();
        (s, t)
    }

    #[test]
    fn cold_launch_needs_every_field_and_the_flag_is_launch_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        assert!(!s.snapshot().launch.warm);
        assert_eq!(s.snapshot().launch.cold_required.len(), 4);
        let p = s.create_project(&pinput("P")).unwrap();
        let blank = TaskInput {
            name: "T".into(),
            project_id: p.clone(),
            description: Value::Null,
            continued_from: None,
            tags: Vec::new(),
        };
        let t = s.create_task(&blank).unwrap();
        assert!(matches!(s.start_task(&t), Err(CoreError::Validation(_))));
        let spaces = TaskInput {
            description: doc("   "),
            ..blank.clone()
        };
        s.update_task(&t, &spaces).unwrap();
        assert!(matches!(s.start_task(&t), Err(CoreError::Validation(_))));
        assert!(!s.is_warm());
        assert!(s.file().data.segments.is_empty());
        let filled = TaskInput {
            description: doc("Real"),
            tags: vec!["tag-concept".to_owned()],
            ..blank.clone()
        };
        s.update_task(&t, &filled).unwrap();
        s.start_task(&t).unwrap();
        assert!(s.is_warm());
        // Warm: a blank description is allowed, name and project are still mandatory.
        let other = s.create_task(&blank).unwrap();
        s.start_task(&other).unwrap();
        assert!(
            s.create_task(&TaskInput {
                name: " ".into(),
                ..blank.clone()
            })
            .is_err()
        );
        // A real relaunch is cold again.
        drop(s);
        let s = open(dir.path(), &clock);
        assert!(!s.is_warm());
        assert!(s.snapshot().launch.copy_source.is_none());
    }

    #[test]
    fn resume_is_refused_while_cold_and_accepted_once_warm() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t) = running(dir.path(), &clock);
        s.pause_task(&t).unwrap();
        assert!(
            s.resume_task(&t).is_ok(),
            "warm: resume continues the same task"
        );
        s.on_exit().unwrap();
        drop(s);
        let mut s = open(dir.path(), &clock);
        let before = s.file().data.tasks[&t].clone();
        let segs = s.file().data.segments.len();
        assert!(matches!(
            s.resume_task(&t),
            Err(CoreError::ColdStartRequired)
        ));
        assert_eq!(
            s.file().data.segments.len(),
            segs,
            "a refused resume logs nothing"
        );
        // The continuation is a new task linked to the old, which stays as it was.
        let p = before.project_id.clone();
        let next = s
            .create_task(&TaskInput {
                name: "Next".into(),
                project_id: p,
                description: doc("More"),
                continued_from: Some(t.clone()),
                tags: vec!["tag-concept".to_owned()],
            })
            .unwrap();
        s.start_task(&next).unwrap();
        assert_eq!(
            s.file().data.tasks[&next].continued_from.as_deref(),
            Some(t.as_str())
        );
        let old = &s.file().data.tasks[&t];
        assert_eq!(
            (&old.name, &old.description, old.state),
            (&before.name, &before.description, TaskState::Paused)
        );
        // Now warm, the old task can be stopped or resumed directly.
        assert!(s.resume_task(&t).is_ok());
    }

    #[test]
    fn crash_recovery_leaves_the_launch_cold_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (s, t) = running(dir.path(), &clock);
        drop(s);
        let mut s = open(dir.path(), &clock);
        assert!(
            s.snapshot().notices[0]
                .message
                .contains("next start asks for every field")
        );
        assert!(matches!(
            s.resume_task(&t),
            Err(CoreError::ColdStartRequired)
        ));
    }

    #[test]
    fn stopping_an_older_task_does_not_change_the_copy_source() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, old) = running(dir.path(), &clock);
        let p = s.file().data.tasks[&old].project_id.clone();
        let new = s.create_task(&tinput("New", &p)).unwrap();
        s.start_task(&new).unwrap();
        s.stop_task(&old).unwrap();
        assert_eq!(s.snapshot().launch.copy_source.unwrap().task_id, new);
    }

    #[test]
    fn a_past_open_segment_from_another_devices_file_is_closed_and_leaves_no_running_task() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t) = running(dir.path(), &clock);
        let mut incoming = s.file().data.clone();
        s.mutate(|f, _| {
            f.data = Shared::default();
            Ok(())
        })
        .unwrap();
        clock.advance(86_400);
        assert!(incoming.tasks.contains_key(&t));
        incoming.settings.clear();
        s.absorb_remote(&incoming, BTreeMap::new(), Vec::new());
        let device = s.file().device_id.clone();
        assert!(s.file().data.own_open_segments(&device).is_empty());
        assert_ne!(s.file().data.tasks[&t].state, TaskState::Running);
    }

    #[test]
    fn warm_copy_source_is_the_most_recently_worked_task() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, a) = running(dir.path(), &clock);
        let src = s.snapshot().launch.copy_source.unwrap();
        assert_eq!(src.task_id, a);
        assert!(!src.project_archived);
        clock.advance(60);
        s.stop_task(&a).unwrap();
        assert_eq!(
            s.snapshot().launch.copy_source.unwrap().task_id,
            a,
            "a stopped task can still be the source"
        );
        let p = s.file().data.tasks[&a].project_id.clone();
        s.set_project_archived(&p, true).unwrap();
        assert!(s.snapshot().launch.copy_source.unwrap().project_archived);
    }

    #[test]
    fn crash_recovery_closes_at_the_last_heartbeat_and_notifies() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t) = running(dir.path(), &clock);
        clock.advance(30);
        s.heartbeat().unwrap();
        clock.advance(30);
        s.heartbeat().unwrap();
        clock.advance(3600);
        drop(s); // the crash: nothing closes the segment

        let s = open(dir.path(), &clock);
        let seg = s.file().data.segments.values().next().unwrap();
        assert_eq!(seg.end.unwrap().iso(), "2026-03-01T09:01:00Z");
        assert_eq!(s.file().data.tasks[&t].state, TaskState::Paused);
        assert_eq!(
            s.file().data.tasks[&t].pause_reason,
            Some(PauseReason::CrashRecovery)
        );
        assert!(
            s.snapshot()
                .notices
                .iter()
                .any(|n| n.message.contains("did not shut down cleanly"))
        );
        // Recovery was saved, so a second launch has nothing to do. The first one ends first: the store is one app's.
        drop(s);
        let again = open(dir.path(), &clock);
        assert!(again.snapshot().notices.is_empty());
    }

    #[test]
    fn a_heartbeat_leaves_the_store_file_alone_and_a_crash_still_stops_at_it() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _) = running(dir.path(), &clock);
        let before = s.stored();
        let changes = s.store.total_changes();
        clock.advance(30);
        s.heartbeat().unwrap();
        // One row, the beat. The records and the rest of the store stay as they were.
        assert_eq!(s.store.total_changes() - changes, 1);
        assert_eq!(s.stored().data, before.data);
        // In memory it counts at once, for what other devices are told.
        assert_eq!(
            s.device_file().last_heartbeat.unwrap().iso(),
            "2026-03-01T09:00:30Z"
        );
        clock.advance(3600);
        drop(s);
        let again = open(dir.path(), &clock);
        let seg = again.file().data.segments.values().next().unwrap();
        assert_eq!(seg.end.unwrap().iso(), "2026-03-01T09:00:30Z");
    }

    // The `after_a_crash_*` tests: a run of work, then the process killed at the end. The next launch reads the database
    // and its WAL as they were on disk (`crash_image`), and must still tell the truth about the run, whichever moment
    // the crash comes.
    fn minutes_of(s: &Service, task: &str) -> i64 {
        s.file()
            .data
            .segments
            .values()
            .filter(|x| x.task_id == task)
            .map(|x| x.end.unwrap_or(x.start).secs() - x.start.secs())
            .sum::<i64>()
            / 60
    }
    /// Time passing with a task running, in two beats: each one waits for a real disk flush.
    fn beats(s: &mut Service, clock: &ManualClock, minutes: i64) {
        clock.advance(minutes * 60 - 30);
        s.heartbeat().unwrap();
        clock.advance(30);
        s.heartbeat().unwrap();
    }

    #[test]
    fn after_a_crash_after_a_switch_hours_stay_on_the_right_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, a) = running(dir.path(), &clock);
        beats(&mut s, &clock, 60);
        let b = s
            .create_task(&tinput("B", &s.file().data.tasks[&a].project_id.clone()))
            .unwrap();
        s.start_task(&b).unwrap();
        beats(&mut s, &clock, 420);
        let image = crash_image(dir.path());
        drop(s);
        let again = open(image.path(), &clock);
        assert_eq!(minutes_of(&again, &a), 60);
        assert_eq!(minutes_of(&again, &b), 420);
    }

    #[test]
    fn after_a_crash_right_after_a_pause_the_hours_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, a) = running(dir.path(), &clock);
        beats(&mut s, &clock, 180);
        clock.advance(10);
        s.pause_task(&a).unwrap();
        let image = crash_image(dir.path());
        drop(s);
        let again = open(image.path(), &clock);
        assert_eq!(minutes_of(&again, &a), 180);
    }

    #[test]
    fn after_a_crash_after_a_clean_quit_the_hours_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, a) = running(dir.path(), &clock);
        beats(&mut s, &clock, 480);
        s.on_exit().unwrap();
        let image = crash_image(dir.path());
        drop(s);
        let again = open(image.path(), &clock);
        assert_eq!(minutes_of(&again, &a), 480);
    }

    #[test]
    fn after_a_crash_an_edit_older_than_one_beat_survives() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, a) = running(dir.path(), &clock);
        beats(&mut s, &clock, 10);
        let project = s.create_project(&pinput("Later")).unwrap();
        beats(&mut s, &clock, 20);
        let image = crash_image(dir.path());
        drop(s);
        let again = open(image.path(), &clock);
        assert!(again.file().data.projects.contains_key(&project));
        assert_eq!(minutes_of(&again, &a), 30);
    }

    #[test]
    fn restoring_a_backup_is_not_overruled_by_a_heartbeat_from_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _) = running(dir.path(), &clock);
        s.run_daily_backup().unwrap();
        let name = s.list_backups().unwrap().remove(0).name;
        clock.advance(86_400);
        s.heartbeat().unwrap();
        s.restore_backup(&name).unwrap();
        // No beat is left in the store: what it holds is the backup's.
        assert_eq!(s.stored().last_heartbeat.unwrap().iso(), T0);
        assert_eq!(s.file().last_heartbeat.unwrap().iso(), T0);
    }

    #[test]
    fn crash_with_heartbeat_older_than_the_segment_gives_zero_not_negative() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _) = running(dir.path(), &clock);
        s.mutate(|f, _| {
            f.last_heartbeat = Ts::parse("2020-01-01T00:00:00Z");
            Ok(())
        })
        .unwrap();
        drop(s);
        let s = open(dir.path(), &clock);
        let seg = s.file().data.segments.values().next().unwrap();
        assert_eq!(seg.end, Some(seg.start));
    }

    #[test]
    fn clean_exit_closes_at_exit_time_and_marks_paused() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t) = running(dir.path(), &clock);
        clock.advance(120);
        s.on_exit().unwrap();
        s.on_exit().unwrap();
        drop(s);
        let s = open(dir.path(), &clock);
        assert_eq!(
            s.file()
                .data
                .segments
                .values()
                .next()
                .unwrap()
                .end
                .unwrap()
                .iso(),
            "2026-03-01T09:02:00Z"
        );
        assert_eq!(
            s.file().data.tasks[&t].pause_reason,
            Some(PauseReason::AppQuit)
        );
        assert!(s.snapshot().notices.is_empty());
    }

    #[test]
    fn data_survives_reopen_and_device_id_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (s, _) = running(dir.path(), &clock);
        let id = s.file().device_id.clone();
        drop(s);
        assert_eq!(open(dir.path(), &clock).file().device_id, id);
    }

    #[test]
    fn a_failed_save_leaves_memory_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let before = s.file().clone();
        // Validation failure inside the transaction.
        assert!(s.create_project(&pinput(" ")).is_err());
        assert_eq!(*s.file(), before);
        assert_eq!(s.revision(), 0);
    }

    #[test]
    fn nothing_syncs_until_the_user_chooses_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.set_detected(vec![Detected {
            provider: SyncProvider::OneDrive,
            root: "/x".into(),
            source: "test".into(),
        }]);
        let snap = s.snapshot();
        assert!(snap.sync.prompt_pending);
        assert!(snap.sync.targets.is_empty());
        assert_eq!(snap.sync.detected.len(), 1);
        s.mark_sync_prompt_done().unwrap();
        assert!(!s.snapshot().sync.prompt_pending);
        assert!(s.snapshot().sync.targets.is_empty());
    }

    #[test]
    fn theme_and_hide_earnings_persist_across_relaunch() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        assert_eq!(s.snapshot().prefs.theme, Theme::Light);
        s.set_theme(Theme::SolarizedDark).unwrap();
        s.set_hide_earnings(true).unwrap();
        drop(s);
        let s = open(dir.path(), &clock);
        assert_eq!(s.snapshot().prefs.theme, Theme::SolarizedDark);
        assert!(s.snapshot().prefs.hide_earnings);
    }

    #[test]
    fn snapshot_timestamps_are_utc_z_strings() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (s, _) = running(dir.path(), &clock);
        let j = serde_json::to_value(s.snapshot()).unwrap();
        assert_eq!(j["segments"][0]["start"], "2026-03-01T09:00:00Z");
        assert_eq!(j["now"], "2026-03-01T09:00:00Z");
        assert!(j["segments"][0]["end"].is_null());
        assert!(j["tzdbVersion"].as_str().unwrap().starts_with("20"));
    }

    // ---- Phase 2 service paths

    fn pending_gaps(s: &Service) -> Vec<crate::model::IdleGap> {
        s.file()
            .data
            .gaps
            .values()
            .filter(|g| g.decision == GapDecision::Pending)
            .cloned()
            .collect()
    }

    #[test]
    fn time_asleep_is_not_billable_until_confirmed_and_the_raw_segment_stays() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t) = running(dir.path(), &clock);
        let raw_start = s.file().data.segments.values().next().unwrap().start;
        assert!(
            !s.observe_activity(Some(0)).unwrap(),
            "first tick only arms"
        );
        clock.advance(5);
        assert!(!s.observe_activity(Some(0)).unwrap());
        // The machine slept for three hours: the next tick arrives late.
        clock.advance(3 * 3600);
        assert!(s.observe_activity(Some(0)).unwrap());
        let gaps = pending_gaps(&s);
        assert_eq!(gaps.len(), 1);
        let task_secs = |s: &Service| {
            s.snapshot()
                .segments
                .iter()
                .filter(|x| x.task_id == t && x.kind == crate::pieces::PieceKind::Work)
                .map(|x| x.end.unwrap_or_else(|| clock.now()).0 - x.start.0)
                .sum::<i64>()
        };
        assert!(task_secs(&s) < 3 * 3600, "unanswered gap counts nowhere");
        let seg = s.file().data.segments.values().next().unwrap();
        assert_eq!(seg.start, raw_start, "raw timestamps are preserved");
        assert!(seg.edit.is_none());
        s.decide_gap(&gaps[0].id, GapDecision::Keep).unwrap();
        assert!(pending_gaps(&s).is_empty());
        assert!(task_secs(&s) >= 3 * 3600, "kept time counts again");
    }

    #[test]
    fn a_restore_keeps_a_safety_copy_and_comes_back_cold() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t) = running(dir.path(), &clock);
        s.pause_task(&t).unwrap();
        assert!(s.run_daily_backup().unwrap());
        let name = s.list_backups().unwrap()[0].name.clone();
        clock.advance(60);
        let late = s
            .create_task(&tinput(
                "Added after",
                &s.file().data.tasks[&t].project_id.clone(),
            ))
            .unwrap();
        let safety = s.restore_backup(&name).unwrap();
        assert!(!s.file().data.tasks.contains_key(&late));
        assert!(s.file().data.tasks.contains_key(&t));
        assert!(!s.is_warm(), "a restore counts as a cold start");
        let listed: Vec<String> = s
            .list_backups()
            .unwrap()
            .into_iter()
            .map(|b| b.name)
            .collect();
        assert!(listed.contains(&safety), "{listed:?}");
        // The safety copy brings the undone work back.
        s.restore_backup(&safety).unwrap();
        assert!(s.file().data.tasks.contains_key(&late));
    }

    #[test]
    fn a_restore_refuses_a_name_outside_the_backup_folder_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t) = running(dir.path(), &clock);
        assert!(s.restore_backup("../z-work-data.json").is_err());
        assert!(s.file().data.tasks.contains_key(&t));
        assert!(s.is_warm());
    }

    #[test]
    fn a_warm_copy_keeps_tags_and_subtask_text_and_clears_ticks_and_time() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let description = serde_json::json!({"type":"doc","content":[
            {"type":"taskList","content":[
                {"type":"taskItem","attrs":{"checked":true},"content":[
                    {"type":"paragraph","content":[{"type":"text","text":"Sketch"}]}]}]}]});
        let t = s
            .create_task(&TaskInput {
                description: description.clone(),
                ..tinput("T", &p)
            })
            .unwrap();
        s.start_task(&t).unwrap();
        let copy = s.snapshot().launch.copy_source.unwrap();
        assert_eq!(copy.tags, vec!["tag-concept".to_owned()]);
        let items = crate::subtasks::items(&copy.description);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "Sketch");
        assert!(!items[0].checked);
        // The copy is a new task with no time of its own.
        let copied = s
            .create_task(&TaskInput {
                description: copy.description,
                tags: copy.tags,
                ..tinput("T again", &p)
            })
            .unwrap();
        assert!(s.file().data.segments.values().all(|x| x.task_id != copied));
    }

    #[test]
    fn a_cold_start_refuses_missing_tags_and_a_warm_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let bare = TaskInput {
            tags: Vec::new(),
            ..tinput("T", &p)
        };
        let t = s.create_task(&bare).unwrap();
        assert!(matches!(s.start_task(&t), Err(CoreError::Validation(_))));
        assert!(s.file().data.segments.is_empty());
        let tagged = s.create_task(&tinput("Tagged", &p)).unwrap();
        s.start_task(&tagged).unwrap();
        s.start_task(&t).unwrap();
    }

    #[test]
    fn colors_are_canonical_on_tags_and_projects_and_anything_else_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let tag = crate::catalog::TagInput {
            id: None,
            name: "Pick".into(),
            color: "#AbCdEf".into(),
        };
        let id = s.save_tag(&tag).unwrap();
        assert_eq!(s.file().data.tags[&id].color, "#abcdef");
        let p = s
            .create_project(&ProjectInput {
                color: "#010203".into(),
                ..pinput("Coloured")
            })
            .unwrap();
        assert_eq!(s.file().data.projects[&p].color, "#010203");
        for bad in [
            "",
            "abcdef",
            "#abc",
            "#abcdefff",
            "#ggggGG",
            "rgb(1,2,3)",
            "#12345\u{e9}",
        ] {
            let refused = s.save_tag(&crate::catalog::TagInput {
                id: None,
                name: format!("Bad {bad}"),
                color: bad.into(),
            });
            assert!(matches!(refused, Err(CoreError::Validation(_))), "{bad}");
            let refused = s.create_project(&ProjectInput {
                color: bad.into(),
                ..pinput(&format!("Bad {bad}"))
            });
            assert!(matches!(refused, Err(CoreError::Validation(_))), "{bad}");
        }
    }

    fn gap_total(s: &Service) -> i64 {
        s.file()
            .data
            .gaps
            .values()
            .map(|g| g.end.secs() - g.start.secs())
            .sum()
    }

    #[test]
    fn leaving_while_away_records_the_time_on_every_path_that_closes_the_segment() {
        for path in ["exit", "pause", "stop"] {
            let dir = tempfile::tempdir().unwrap();
            let clock = ManualClock::at(T0);
            let (mut s, t) = running(dir.path(), &clock);
            s.observe_activity(Some(0)).unwrap();
            for _ in 0..3 {
                clock.advance(5);
                s.observe_activity(Some(3 * 3600)).unwrap();
            }
            assert_eq!(gap_total(&s), 0, "still away, nothing decided yet");
            match path {
                "exit" => s.on_exit().unwrap(),
                "pause" => s.pause_task(&t).unwrap(),
                _ => s.stop_task(&t).unwrap(),
            }
            assert!(gap_total(&s) > 0, "{path}");
            assert_eq!(pending_gaps(&s).len(), 1, "{path}");
        }
    }

    #[test]
    fn a_crash_while_away_still_leaves_the_time_unconfirmed() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        for _ in 0..3 {
            clock.advance(5);
            s.heartbeat().unwrap();
            s.observe_activity(Some(1800)).unwrap();
        }
        assert!(s.file().prefs.away_since.is_some());
        drop(s); // No exit: the process died.
        clock.advance(600);
        let s = open(dir.path(), &clock);
        assert_eq!(pending_gaps(&s).len(), 1);
        assert!(s.file().prefs.away_since.is_none());
    }

    #[test]
    fn found_time_away_survives_a_save_that_fails() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        clock.advance(3600);
        // The save fails inside its transaction, as on a full disk.
        s.set_saves_blocked(true);
        assert!(s.observe_activity(Some(0)).is_err());
        assert!(pending_gaps(&s).is_empty());
        s.set_saves_blocked(false);
        clock.advance(5);
        assert!(s.observe_activity(Some(0)).unwrap());
        assert_eq!(pending_gaps(&s).len(), 1);
        // The save after a failed one rewrote every row, so memory and disk can't have drifted apart.
        assert_eq!(
            s.store.last_save().upserted,
            crate::store::record_count(&s.file().data)
        );
        assert_eq!(s.stored(), *s.file());
    }

    #[test]
    fn a_phase_1_task_list_gets_stable_ids_when_it_opens() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        s.mutate(|f, _| {
            f.data.tasks.get_mut(&t).unwrap().description = serde_json::json!({"type":"doc","content":[
                {"type":"taskList","content":[
                    {"type":"taskItem","attrs":{"checked":false},"content":[{"type":"paragraph","content":[{"type":"text","text":"One"}]}]},
                    {"type":"taskItem","attrs":{"checked":false},"content":[{"type":"paragraph","content":[{"type":"text","text":"Two"}]}]}]}]});
            Ok(())
        })
        .unwrap();
        drop(s);
        let mut s = open(dir.path(), &clock);
        let ids: Vec<String> = crate::subtasks::items(&s.file().data.tasks[&t].description)
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(ids, vec![format!("{t}-0"), format!("{t}-1")]);
        assert!(s.set_active_subtask(&t, Some("")).is_err());
        assert!(s.set_active_subtask(&t, Some(&ids[1])).is_ok());
    }

    #[test]
    fn removing_the_active_subtask_clears_it_and_new_segments_do_not_use_it() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let listed = serde_json::json!({"type":"doc","content":[{"type":"taskList","content":[
            {"type":"taskItem","attrs":{"checked":false},"content":[{"type":"paragraph","content":[{"type":"text","text":"One"}]}]}]}]});
        let input = TaskInput {
            description: listed,
            ..tinput("T", &p)
        };
        let t = s.create_task(&input).unwrap();
        let id = crate::subtasks::items(&s.file().data.tasks[&t].description)[0]
            .id
            .clone();
        s.set_active_subtask(&t, Some(&id)).unwrap();
        s.update_task(
            &t,
            &TaskInput {
                description: doc("No list now"),
                ..input.clone()
            },
        )
        .unwrap();
        assert_eq!(s.file().data.tasks[&t].active_subtask, None);
        s.start_task(&t).unwrap();
        assert!(
            s.file()
                .data
                .segments
                .values()
                .all(|x| x.subtask_id.is_none())
        );
    }

    #[test]
    fn a_lock_opens_one_unconfirmed_gap_whatever_the_idle_limit_says() {
        for (limit, lock_secs) in [(10i64, 9 * 60i64), (0, 3 * 3600)] {
            let dir = tempfile::tempdir().unwrap();
            let clock = ManualClock::at(T0);
            let (mut s, _) = running(dir.path(), &clock);
            s.set_setting("idleMinutes", serde_json::json!(limit))
                .unwrap();
            s.observe_lock(Some(false)).unwrap();
            s.observe_activity(Some(0)).unwrap();
            clock.advance(5);
            assert!(!s.observe_lock(Some(true)).unwrap());
            assert_eq!(s.file().prefs.away_since, Some(clock.now()));
            let mut left = lock_secs;
            while left > 0 {
                clock.advance(5);
                left -= 5;
                assert!(!s.observe_lock(Some(true)).unwrap());
                assert!(
                    !s.observe_activity(Some(0)).unwrap(),
                    "no idle count at all"
                );
            }
            assert!(s.observe_lock(Some(false)).unwrap());
            let gaps = pending_gaps(&s);
            assert_eq!(gaps.len(), 1, "limit {limit}");
            assert_eq!(gaps[0].kind, GapKind::Lock);
            assert_eq!(gaps[0].end.secs() - gaps[0].start.secs(), lock_secs);
            assert_eq!(s.file().prefs.away_since, None);
            s.decide_gap(&gaps[0].id, GapDecision::Thinking).unwrap();
        }
    }

    #[test]
    fn no_lock_signal_changes_nothing_and_a_lock_with_no_task_running_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        assert!(!s.observe_lock(None).unwrap());
        s.observe_lock(Some(true)).unwrap();
        clock.advance(3600);
        assert!(!s.observe_lock(Some(false)).unwrap());
        assert!(s.file().data.gaps.is_empty());
    }

    #[test]
    fn quitting_while_locked_records_the_time() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        s.observe_lock(Some(true)).unwrap();
        clock.advance(100);
        s.on_exit().unwrap();
        assert_eq!(pending_gaps(&s).len(), 1);
    }

    /// One summary, then a backup, then more summaries: the store a restore goes back from.
    fn billed_world(dir: &Path, clock: &ManualClock) -> (Service, String, String) {
        let mut s = open(dir, clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        s.start_task(&t).unwrap();
        clock.advance(3600);
        s.stop_task(&t).unwrap();
        (s, p, t)
    }
    fn this_month(p: &str) -> crate::summary::SummaryRequest {
        crate::summary::SummaryRequest {
            project_id: p.to_owned(),
            from: "2026-03-01".into(),
            to: "2026-03-31".into(),
            zone: "UTC".into(),
        }
    }

    #[test]
    fn restoring_an_older_backup_never_reuses_a_reference_number() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, p, _) = billed_world(dir.path(), &clock);
        s.run_daily_backup().unwrap();
        let name = s.list_backups().unwrap()[0].name.clone();
        let first = s.create_summary(&this_month(&p)).unwrap();
        assert_eq!(s.file().data.summaries[&first].reference, "A-001");
        s.restore_backup(&name).unwrap();
        assert!(
            s.file().data.summaries.is_empty(),
            "the summary was rolled back"
        );
        let again = s.create_summary(&this_month(&p)).unwrap();
        assert_eq!(s.file().data.summaries[&again].reference, "A-002");
        // The mark survives a relaunch too.
        drop(s);
        let s = open(dir.path(), &clock);
        assert_eq!(s.file().prefs.reference_marks["A"], 2);
    }

    #[test]
    fn a_restore_rolls_back_data_and_settings_and_keeps_what_belongs_to_the_device() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, p, _) = billed_world(dir.path(), &clock);
        s.set_setting("idleMinutes", serde_json::json!(25)).unwrap();
        s.run_daily_backup().unwrap();
        let name = s.list_backups().unwrap()[0].name.clone();
        let audits_at_backup = s.file().data.audit_log.len();
        clock.advance(60);
        s.set_setting("idleMinutes", serde_json::json!(5)).unwrap();
        s.set_device_prefix("Q").unwrap();
        s.set_backup_keep(7).unwrap();
        s.set_theme(Theme::SolarizedDark).unwrap();
        let later = s.create_project(&pinput("Later")).unwrap();
        s.create_summary(&this_month(&p)).unwrap();
        let audits_before = s.file().data.audit_log.len();
        assert!(audits_before > audits_at_backup);
        s.restore_backup(&name).unwrap();
        let f = s.file();
        assert!(!f.data.projects.contains_key(&later), "data rolls back");
        assert_eq!(
            f.data.settings["idleMinutes"].value,
            serde_json::json!(25),
            "settings roll back"
        );
        assert_eq!(f.data.prefix_of(&f.device_id), "Q", "the prefix stays");
        assert_eq!(
            f.prefs.backup_keep,
            Some(7),
            "this device's preferences stay"
        );
        assert_eq!(f.prefs.theme, Theme::SolarizedDark);
        assert_eq!(f.prefs.reference_marks["Q"], 1, "the mark stays");
        assert_eq!(
            f.data.audit_log.len(),
            audits_before + 1,
            "nothing leaves the audit trail"
        );
        let entry = f
            .data
            .audit_log
            .values()
            .find(|a| a.action == "restore")
            .unwrap();
        assert_eq!(entry.entity_id, name);
    }

    #[test]
    fn a_task_running_in_a_backup_closes_at_its_heartbeat_with_no_crash_notice() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t) = running(dir.path(), &clock);
        clock.advance(600);
        s.heartbeat().unwrap();
        let beat = clock.now();
        s.run_daily_backup().unwrap();
        let name = s.list_backups().unwrap()[0].name.clone();
        clock.advance(3600);
        s.restore_backup(&name).unwrap();
        let f = s.file();
        assert_eq!(f.data.tasks[&t].state, TaskState::Paused);
        assert_eq!(
            f.data.tasks[&t].pause_reason,
            Some(PauseReason::BackupRestored)
        );
        assert!(f.data.segments.values().all(|x| x.end == Some(beat)));
        let notices = s.snapshot().notices;
        assert!(
            notices
                .iter()
                .all(|n| !n.message.contains("did not shut down cleanly")),
            "{notices:?}"
        );
        assert!(
            notices
                .iter()
                .any(|n| n.message.contains("running when this backup was taken"))
        );
    }

    #[test]
    fn the_list_of_backups_is_newest_first_across_both_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _) = running(dir.path(), &clock);
        s.run_daily_backup().unwrap();
        let name = s.list_backups().unwrap()[0].name.clone();
        clock.advance(86_400 * 2);
        s.run_daily_backup().unwrap();
        clock.advance(60);
        let safety = s.restore_backup(&name).unwrap();
        assert_eq!(s.list_backups().unwrap()[0].name, safety);
    }

    /// One poll round as the app makes it: the lock first, then the activity tick.
    fn poll(s: &mut Service, clock: &ManualClock, secs: i64, locked: Option<bool>) {
        clock.advance(secs);
        s.observe_lock(locked).unwrap();
        s.observe_activity(Some(0)).unwrap();
    }

    #[test]
    fn a_query_that_never_answers_leaves_idle_and_sleep_detection_working() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _) = running(dir.path(), &clock);
        for _ in 0..50 {
            poll(&mut s, &clock, 5, None);
        }
        assert!(pending_gaps(&s).is_empty(), "working is not an absence");
        poll(&mut s, &clock, 3600, None);
        assert_eq!(pending_gaps(&s).len(), 1, "the sleep is still found");
    }

    #[test]
    fn a_query_failing_after_a_lock_closes_it_at_the_last_confirmed_moment() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        poll(&mut s, &clock, 5, Some(true));
        for _ in 0..60 {
            poll(&mut s, &clock, 5, Some(true));
        }
        for _ in 0..720 {
            poll(&mut s, &clock, 5, None);
        }
        assert!(
            s.file().prefs.away_since.is_none(),
            "the lock no longer latches"
        );
        let gaps = pending_gaps(&s);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].kind, GapKind::Lock);
        assert_eq!(
            gaps[0].end.secs() - gaps[0].start.secs(),
            300,
            "not the hour of work after it"
        );
        // The answer coming back later adds nothing.
        s.observe_lock(Some(false)).unwrap();
        assert_eq!(s.file().data.gaps.len(), 1);
    }

    #[test]
    fn a_lock_seen_once_before_the_query_fails_stays_work_and_a_later_sleep_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        poll(&mut s, &clock, 5, Some(true));
        for _ in 0..720 {
            poll(&mut s, &clock, 5, None);
        }
        assert!(pending_gaps(&s).is_empty());
        poll(&mut s, &clock, 3600, None);
        let gaps = pending_gaps(&s);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].kind, GapKind::Sleep);
        s.on_exit().unwrap();
        assert_eq!(pending_gaps(&s).len(), 1);
    }

    #[test]
    fn one_failed_poll_does_not_split_a_lock() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        poll(&mut s, &clock, 5, Some(true));
        poll(&mut s, &clock, 5, None);
        poll(&mut s, &clock, 5, Some(true));
        poll(&mut s, &clock, 5, Some(false));
        let gaps = pending_gaps(&s);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].end.secs() - gaps[0].start.secs(), 15);
    }

    #[test]
    fn a_clock_going_back_during_a_lock_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _) = running(dir.path(), &clock);
        clock.advance(5);
        s.observe_activity(Some(0)).unwrap();
        s.observe_lock(Some(true)).unwrap();
        clock.advance(-300);
        s.observe_lock(Some(false)).unwrap();
        assert!(pending_gaps(&s).is_empty());
    }

    #[test]
    fn a_crash_during_a_lock_is_labelled_a_lock() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        poll(&mut s, &clock, 5, Some(true));
        clock.advance(60);
        s.heartbeat().unwrap();
        assert_eq!(s.file().prefs.away_kind, Some(GapKind::Lock));
        drop(s);
        clock.advance(600);
        let s = open(dir.path(), &clock);
        let gaps = pending_gaps(&s);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].kind, GapKind::Lock);
        assert_eq!(s.file().prefs.away_kind, None);
    }

    // ---- Gap-fill round

    fn fixed_random() -> Box<dyn FnMut() -> u64 + Send> {
        let mut n = 11u64;
        Box::new(move || {
            n = n.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            n >> 16
        })
    }

    #[test]
    fn an_exact_colour_clash_is_replaced_and_reported_and_a_similar_one_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.set_random(fixed_random());
        let a = s
            .create_project(&ProjectInput {
                color: "#2563eb".into(),
                ..pinput("A")
            })
            .unwrap();
        assert_eq!(s.take_color_replaced(), None);
        let b = s
            .create_project(&ProjectInput {
                color: "#2563EB".into(),
                ..pinput("B")
            })
            .unwrap();
        assert_eq!(s.take_color_replaced().as_deref(), Some("#2563EB"));
        assert_ne!(s.file().data.projects[&b].color, "#2563eb");
        let c = s
            .create_project(&ProjectInput {
                color: "#2563ec".into(),
                ..pinput("C")
            })
            .unwrap();
        assert_eq!(s.take_color_replaced(), None);
        assert_eq!(s.file().data.projects[&c].color, "#2563ec");
        // Archived projects don't count.
        s.set_project_archived(&a, true).unwrap();
        let d = s
            .create_project(&ProjectInput {
                color: "#2563eb".into(),
                ..pinput("D")
            })
            .unwrap();
        assert_eq!(s.take_color_replaced(), None);
        assert_eq!(s.file().data.projects[&d].color, "#2563eb");
        // Saving a colour left unchanged never replaces it.
        s.mutate(|f, _| {
            f.data.projects.get_mut(&c).unwrap().color = "#2563eb".into();
            Ok(())
        })
        .unwrap();
        s.update_project(
            &c,
            &ProjectInput {
                color: "#2563eb".into(),
                ..pinput("C renamed")
            },
        )
        .unwrap();
        assert_eq!(s.take_color_replaced(), None);
        assert_eq!(s.file().data.projects[&c].color, "#2563eb");
        assert!(
            s.snapshot()
                .warnings
                .iter()
                .any(|w| w.contains("same colour"))
        );
    }

    #[test]
    fn tags_follow_the_same_rule_within_their_own_kind() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.set_random(fixed_random());
        s.create_project(&ProjectInput {
            color: "#123456".into(),
            ..pinput("A")
        })
        .unwrap();
        let tag = TagInput {
            id: None,
            name: "New".into(),
            color: "#123456".into(),
        };
        let id = s.save_tag(&tag).unwrap();
        assert_eq!(
            s.take_color_replaced(),
            None,
            "a project's colour doesn't count"
        );
        let again = s
            .save_tag(&TagInput {
                name: "Again".into(),
                ..tag
            })
            .unwrap();
        assert_eq!(s.take_color_replaced().as_deref(), Some("#123456"));
        assert_ne!(
            s.file().data.tags[&again].color,
            s.file().data.tags[&id].color
        );
    }

    #[test]
    fn colours_two_devices_picked_offline_stay_as_saved_after_a_merge() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let a = s.create_project(&pinput("A")).unwrap();
        let mut other = s.file().data.clone();
        let mut twin = other.projects[&a].clone();
        twin.id = "twin".into();
        twin.name = "Twin".into();
        other.projects.insert("twin".into(), twin);
        s.absorb_remote(&other, BTreeMap::new(), Vec::new());
        assert_eq!(
            s.file().data.projects["twin"].color,
            s.file().data.projects[&a].color
        );
        assert!(
            s.snapshot()
                .warnings
                .iter()
                .any(|w| w.contains("same colour"))
        );
    }

    #[test]
    fn setup_is_pending_only_on_a_fresh_store_resumes_and_answers_the_old_prompts() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        assert!(s.snapshot().setup.pending);
        s.set_setup_step(3).unwrap();
        drop(s);
        let mut s = open(dir.path(), &clock);
        assert!(s.snapshot().setup.pending);
        assert_eq!(s.snapshot().setup.step, 3, "resumes");
        s.finish_setup().unwrap();
        let snap = s.snapshot();
        assert!(!snap.setup.pending);
        assert!(!snap.sync.prompt_pending);
        assert!(snap.prefs.client_zone_prompt_done);
        drop(s);
        assert!(
            !open(dir.path(), &clock).snapshot().setup.pending,
            "shown once"
        );
    }

    #[test]
    fn connecting_a_folder_during_setup_does_not_end_it_and_a_user_with_data_is_not_asked() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.add_sync_target(SyncTarget {
            provider: SyncProvider::Custom,
            root: "/somewhere".into(),
        })
        .unwrap();
        assert!(
            s.snapshot().setup.pending,
            "the rest of the steps are still owed"
        );
        s.finish_setup().unwrap();
        assert!(!s.snapshot().setup.pending);

        let dir = tempfile::tempdir().unwrap();
        let mut s = open(dir.path(), &clock);
        s.create_project(&pinput("Mine")).unwrap();
        assert!(!s.snapshot().setup.pending, "data from before the wizard");
        s.add_sync_target(SyncTarget {
            provider: SyncProvider::Custom,
            root: "/elsewhere".into(),
        })
        .unwrap();
        assert!(!s.snapshot().setup.pending, "connecting doesn't start it");
    }

    #[test]
    fn an_existing_user_is_not_shown_setup() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.mark_sync_prompt_done().unwrap();
        assert!(!s.snapshot().setup.pending);
    }

    #[test]
    fn a_draft_typed_before_a_shutdown_is_recovered_once_and_a_stale_one_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        let stale = s.create_task(&tinput("Stale", &p)).unwrap();
        clock.advance(5);
        s.stash_draft(&t, &doc("Typed at the last moment"), &Value::Null)
            .unwrap();
        s.stash_draft(&stale, &doc("Older than the save"), &Value::Null)
            .unwrap();
        assert!(
            s.stash_draft(&t, &serde_json::json!("<b>no</b>"), &Value::Null)
                .is_err()
        );
        clock.advance(5);
        s.update_task(
            &stale,
            &TaskInput {
                description: doc("Saved later"),
                ..tinput("Stale", &p)
            },
        )
        .unwrap();
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let mut s = open(dir.path(), &clock);
        assert_eq!(
            s.file().data.tasks[&t].description,
            doc("Typed at the last moment")
        );
        assert_eq!(s.file().data.tasks[&stale].description, doc("Saved later"));
        assert!(
            s.snapshot()
                .notices
                .iter()
                .any(|n| n.message.contains("Recovered"))
        );
        // The stale one replaces nothing, and its words are shown rather than dropped, until dismissed.
        let older = s
            .snapshot()
            .notices
            .into_iter()
            .find(|n| n.message.contains("Older than the save"))
            .unwrap();
        s.dismiss_notice(&older.id);
        s.on_exit().unwrap();
        drop(s);
        let s = open(dir.path(), &clock);
        assert!(s.snapshot().notices.is_empty(), "replayed once");
    }

    /// Two devices, the task shared. Returns them with the task id.
    fn two_devices(here: &Path, there: &Path, clock: &ManualClock) -> (Service, Service, String) {
        let mut s = open(here, clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        let mut other = open(there, clock);
        other.absorb_remote(&s.file().data.clone(), BTreeMap::new(), Vec::new());
        (s, other, t)
    }

    /// The description `other` holds, saved there and merged here.
    fn edited_elsewhere(s: &mut Service, other: &mut Service, t: &str, text: &str) {
        other
            .patch_task(
                t,
                &TaskPatch {
                    description: Some(doc(text)),
                    ..TaskPatch::default()
                },
                &TaskExpected::default(),
            )
            .unwrap();
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
    }

    #[test]
    fn a_draft_never_replaces_text_that_came_from_elsewhere_meanwhile() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        let shown = s.file().data.tasks[&t].description.clone();
        clock.advance(3);
        other
            .patch_task(
                &t,
                &TaskPatch {
                    description: Some(doc("From another device")),
                    ..TaskPatch::default()
                },
                &TaskExpected::default(),
            )
            .unwrap();
        // Typed after the other device saved, so the old rule (the newer stamp wins) would replace it.
        clock.advance(2);
        s.stash_draft(&t, &doc("Typed here"), &shown).unwrap();
        clock.advance(1);
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let mut s = open(here.path(), &clock);
        assert_eq!(
            s.file().data.tasks[&t].description,
            doc("From another device")
        );
        let notices = s.snapshot().notices;
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].level, "warning");
        assert!(notices[0].message.contains("Typed here"));
        assert!(
            notices[0]
                .message
                .contains("its description changed while they waited")
        );
        // The notice is the only place left for them, so they outlast a launch nobody read it in.
        assert_eq!(kept_words(here.path()), ["Typed here"]);
        assert!(drafts_on_disk(here.path()).is_empty());
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let mut s = open(here.path(), &clock);
        let notices = s.snapshot().notices;
        assert_eq!(notices.len(), 1);
        assert!(notices[0].message.contains("Typed here"));
        // Dismissing it ends them.
        s.dismiss_notice(&notices[0].id);
        assert!(kept_words(here.path()).is_empty());
        s.on_exit().unwrap();
        drop(s);
        let s = open(here.path(), &clock);
        assert!(s.snapshot().notices.is_empty());
    }

    #[test]
    fn a_draft_older_than_the_text_from_elsewhere_is_shown_too() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        let shown = s.file().data.tasks[&t].description.clone();
        clock.advance(2);
        s.stash_draft(&t, &doc("Typed here"), &shown).unwrap();
        clock.advance(3);
        edited_elsewhere(&mut s, &mut other, &t, "From another device");
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let s = open(here.path(), &clock);
        assert_eq!(
            s.file().data.tasks[&t].description,
            doc("From another device")
        );
        assert!(
            s.snapshot().notices[0].message.contains("Typed here"),
            "words older than the newer text still may not vanish"
        );
    }

    #[test]
    fn a_draft_over_text_from_elsewhere_the_editor_was_showing_is_recovered() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        clock.advance(3);
        edited_elsewhere(&mut s, &mut other, &t, "From another device");
        clock.advance(2);
        s.stash_draft(&t, &doc("Typed here"), &doc("From another device"))
            .unwrap();
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let s = open(here.path(), &clock);
        assert_eq!(s.file().data.tasks[&t].description, doc("Typed here"));
        assert!(
            s.snapshot()
                .notices
                .iter()
                .any(|n| n.message.contains("Recovered"))
        );
    }

    #[test]
    fn a_draft_typed_after_a_slow_save_of_this_device_is_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        let shown = s.file().data.tasks[&t].description.clone();
        clock.advance(5);
        s.patch_task(
            &t,
            &TaskPatch {
                description: Some(doc("Saved")),
                ..TaskPatch::default()
            },
            &TaskExpected::default(),
        )
        .unwrap();
        // The save's reply hadn't come, so the draft still names the text before it.
        clock.advance(1);
        s.stash_draft(&t, &doc("Saved and more"), &shown).unwrap();
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let s = open(dir.path(), &clock);
        assert_eq!(s.file().data.tasks[&t].description, doc("Saved and more"));
    }

    #[test]
    fn dismissing_a_notice_leaves_words_stashed_since() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        let shown = s.file().data.tasks[&t].description.clone();
        s.stash_draft(&t, &doc("Typed here"), &shown).unwrap();
        clock.advance(3);
        edited_elsewhere(&mut s, &mut other, &t, "From another device");
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let mut s = open(here.path(), &clock);
        clock.advance(5);
        s.stash_draft(&t, &doc("Typed later"), &doc("From another device"))
            .unwrap();
        let id = s.snapshot().notices[0].id.clone();
        s.dismiss_notice(&id);
        assert_eq!(
            drafts_on_disk(here.path())[&t].description,
            doc("Typed later")
        );
    }

    /// Words a launch could not put back: stored, stashed over and saved over on the same task.
    fn words_kept_at_a_launch(here: &Path, there: &Path, clock: &ManualClock) -> (Service, String) {
        let (mut s, mut other, t) = two_devices(here, there, clock);
        let shown = s.file().data.tasks[&t].description.clone();
        clock.advance(3);
        s.stash_draft(&t, &doc("Held words before the restart"), &shown)
            .unwrap();
        clock.advance(1);
        edited_elsewhere(&mut s, &mut other, &t, "Newer from elsewhere");
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let s = open(here, clock);
        assert_eq!(kept_notices(&s).len(), 1);
        (s, t)
    }

    /// The words in their file.
    fn kept_words(dir: &Path) -> Vec<String> {
        crate::drafts::read_kept(dir)
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|k| k.words)
            .collect()
    }

    /// What the files of a kind moved aside hold.
    fn files_aside(dir: &Path, stem: &str) -> Vec<String> {
        let mut aside: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().contains(&format!("{stem}.corrupt-")))
            .map(|p| std::fs::read_to_string(p).unwrap())
            .collect();
        aside.sort();
        aside
    }

    /// What the notices say of the words kept at a launch.
    fn kept_notices(s: &Service) -> Vec<String> {
        s.snapshot()
            .notices
            .into_iter()
            .filter(|n| n.message.contains("Your unsaved words"))
            .map(|n| n.message)
            .collect()
    }

    #[test]
    fn typing_on_a_task_leaves_the_words_its_notice_shows() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t) = words_kept_at_a_launch(here.path(), there.path(), &clock);
        // Without dismissing the notice, the user goes on in that task, as they would to merge the words in.
        let stored = s.file().data.tasks[&t].description.clone();
        clock.advance(1);
        s.stash_draft(&t, &doc("Newer from elsewhere and more"), &stored)
            .unwrap();
        clock.advance(1);
        s.patch_task(
            &t,
            &TaskPatch {
                description: Some(doc("Newer from elsewhere and more")),
                ..TaskPatch::default()
            },
            &TaskExpected {
                description: Some(stored),
                ..TaskExpected::default()
            },
        )
        .unwrap();
        s.clear_draft(&t).unwrap();
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let mut s = open(here.path(), &clock);
        let words = kept_notices(&s);
        assert_eq!(
            words.len(),
            1,
            "the notice comes back until it is dismissed"
        );
        assert!(words[0].contains("Held words before the restart"));
        assert_eq!(
            s.file().data.tasks[&t].description,
            doc("Newer from elsewhere and more")
        );
        // Only dismissing it ends them.
        let id = s.snapshot().notices[0].id.clone();
        s.dismiss_notice(&id);
        s.on_exit().unwrap();
        drop(s);
        let s = open(here.path(), &clock);
        assert!(kept_notices(&s).is_empty());
    }

    #[test]
    fn a_stash_alone_leaves_the_words_a_notice_shows_and_is_put_back_itself() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (s, t) = words_kept_at_a_launch(here.path(), there.path(), &clock);
        // Typed, and the app killed before the save: the stash is replayed, and the notice stays.
        let stored = s.file().data.tasks[&t].description.clone();
        clock.advance(1);
        s.stash_draft(&t, &doc("Typed after the notice"), &stored)
            .unwrap();
        drop(s);
        clock.advance(60);
        let s = open(here.path(), &clock);
        assert_eq!(
            s.file().data.tasks[&t].description,
            doc("Typed after the notice")
        );
        let words = kept_notices(&s);
        assert_eq!(words.len(), 1);
        assert!(words[0].contains("Held words before the restart"));
    }

    #[test]
    fn two_lots_of_words_kept_on_one_task_each_have_their_notice() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        let shown = s.file().data.tasks[&t].description.clone();
        clock.advance(3);
        s.stash_draft(&t, &doc("First words"), &shown).unwrap();
        clock.advance(1);
        edited_elsewhere(&mut s, &mut other, &t, "Newer one");
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let mut s = open(here.path(), &clock);
        assert_eq!(kept_notices(&s).len(), 1);
        clock.advance(1);
        s.stash_draft(&t, &doc("Second words"), &doc("Newer one"))
            .unwrap();
        clock.advance(1);
        edited_elsewhere(&mut s, &mut other, &t, "Newer two");
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let mut s = open(here.path(), &clock);
        let words = kept_notices(&s);
        assert_eq!(words.len(), 2);
        assert!(words.iter().any(|w| w.contains("First words")));
        assert!(words.iter().any(|w| w.contains("Second words")));
        // Dismissing one leaves the other.
        let first = s
            .snapshot()
            .notices
            .into_iter()
            .find(|n| n.message.contains("First words"))
            .unwrap();
        s.dismiss_notice(&first.id);
        s.on_exit().unwrap();
        drop(s);
        let s = open(here.path(), &clock);
        let words = kept_notices(&s);
        assert_eq!(words.len(), 1);
        assert!(words[0].contains("Second words"));
    }

    #[test]
    fn a_draft_on_a_task_deleted_elsewhere_is_shown_until_dismissed() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        let shown = s.file().data.tasks[&t].description.clone();
        s.stash_draft(&t, &doc("Typed here"), &shown).unwrap();
        clock.advance(3);
        other.delete_draft(&t).unwrap();
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let s = open(here.path(), &clock);
        let words = kept_notices(&s);
        assert_eq!(words.len(), 1);
        assert!(words[0].contains("Typed here"));
        assert!(words[0].contains("“T”"));
        assert!(words[0].contains("no longer exists"));
        drop(s);
        let mut s = open(here.path(), &clock);
        let id = s.snapshot().notices[0].id.clone();
        s.dismiss_notice(&id);
        drop(s);
        let s = open(here.path(), &clock);
        assert!(s.snapshot().notices.is_empty());
    }

    #[test]
    fn a_draft_already_saved_or_empty_on_a_task_deleted_elsewhere_is_nothing_to_tell() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        let shown = s.file().data.tasks[&t].description.clone();
        let empty = s
            .create_task(&tinput(
                "Empty",
                &s.file().data.tasks[&t].project_id.clone(),
            ))
            .unwrap();
        s.patch_task(
            &t,
            &TaskPatch {
                description: Some(doc("Saved")),
                ..TaskPatch::default()
            },
            &TaskExpected::default(),
        )
        .unwrap();
        s.stash_draft(&t, &doc("Saved"), &shown).unwrap();
        s.stash_draft(&empty, &Value::Null, &Value::Null).unwrap();
        other.absorb_remote(&s.file().data.clone(), BTreeMap::new(), Vec::new());
        clock.advance(3);
        other.delete_draft(&t).unwrap();
        other.delete_draft(&empty).unwrap();
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        s.on_exit().unwrap();
        drop(s);
        let s = open(here.path(), &clock);
        assert!(s.snapshot().notices.is_empty());
    }

    #[test]
    fn words_put_back_at_launch_whose_task_the_first_sync_deletes_are_shown_not_lost() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        let shown = s.file().data.tasks[&t].description.clone();
        clock.advance(2);
        s.stash_draft(&t, &doc("Typed here"), &shown).unwrap();
        s.on_exit().unwrap();
        drop(s);
        clock.advance(30);
        other.delete_draft(&t).unwrap();
        clock.advance(30);
        let mut s = open(here.path(), &clock);
        assert_eq!(s.file().data.tasks[&t].description, doc("Typed here"));
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        assert!(!s.file().data.tasks[&t].is_live());
        let words = kept_notices(&s);
        assert_eq!(words.len(), 1);
        assert!(words[0].contains("Typed here"));
        assert!(words[0].contains("no longer exists"));
    }

    #[test]
    fn words_the_page_gives_up_on_a_gone_task_are_shown_until_dismissed() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.keep_unsaved_words(
            "gone",
            "Old name",
            Some("New name"),
            Some(&doc("Typed here")),
        );
        let words = kept_notices(&s);
        assert_eq!(words.len(), 1);
        assert!(words[0].contains("“Old name”"));
        assert!(words[0].contains("New name\nTyped here"));
        s.on_exit().unwrap();
        drop(s);
        let mut s = open(dir.path(), &clock);
        assert_eq!(kept_notices(&s), words, "the same again at the next launch");
        let id = s.snapshot().notices[0].id.clone();
        s.dismiss_notice(&id);
        assert!(kept_words(dir.path()).is_empty());
    }

    /// Words put back at launch, and the other device has them too.
    fn words_put_back_and_synced(
        here: &Path,
        there: &Path,
        clock: &ManualClock,
    ) -> (Service, Service, String) {
        let (mut s, mut other, t) = two_devices(here, there, clock);
        let shown = s.file().data.tasks[&t].description.clone();
        clock.advance(2);
        s.stash_draft(&t, &doc("Typed here"), &shown).unwrap();
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let mut s = open(here, clock);
        assert_eq!(s.file().data.tasks[&t].description, doc("Typed here"));
        other.absorb_remote(&s.file().data.clone(), BTreeMap::new(), Vec::new());
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        (s, other, t)
    }

    #[test]
    fn an_edit_before_the_launch_by_a_clock_that_runs_ahead_leaves_the_words_in_a_notice() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let ahead = ManualClock::at("2026-03-01T09:02:00Z");
        let mut s = open(here.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        let mut other = open(there.path(), &ahead);
        other.absorb_remote(&s.file().data.clone(), BTreeMap::new(), Vec::new());
        let shown = s.file().data.tasks[&t].description.clone();
        clock.advance(2);
        s.stash_draft(&t, &doc("Typed here"), &shown).unwrap();
        s.on_exit().unwrap();
        drop(s);
        clock.advance(30);
        ahead.advance(32);
        // 30 seconds before the launch here, which that clock reads as 90 seconds after it.
        other
            .patch_task(
                &t,
                &TaskPatch {
                    description: Some(doc("Typed elsewhere")),
                    ..TaskPatch::default()
                },
                &TaskExpected::default(),
            )
            .unwrap();
        clock.advance(30);
        let mut s = open(here.path(), &clock);
        assert_eq!(s.file().data.tasks[&t].description, doc("Typed here"));
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        assert_eq!(s.file().data.tasks[&t].description, doc("Typed elsewhere"));
        let words = kept_notices(&s);
        assert_eq!(words.len(), 1, "{:?}", s.snapshot().notices);
        assert!(words[0].contains("Typed here"));
        assert_eq!(kept_words(here.path()).len(), 1);
    }

    #[test]
    fn an_edit_just_after_the_launch_by_a_pc_without_the_words_leaves_them_in_a_notice() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        let shown = s.file().data.tasks[&t].description.clone();
        clock.advance(2);
        s.stash_draft(&t, &doc("Typed here"), &shown).unwrap();
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let mut s = open(here.path(), &clock);
        assert_eq!(s.file().data.tasks[&t].description, doc("Typed here"));
        clock.advance(10);
        edited_elsewhere(&mut s, &mut other, &t, "Typed elsewhere");
        assert_eq!(s.file().data.tasks[&t].description, doc("Typed elsewhere"));
        let words = kept_notices(&s);
        assert_eq!(words.len(), 1, "{:?}", s.snapshot().notices);
        assert!(words[0].contains("Typed here"));
    }

    #[test]
    fn an_edit_made_after_the_launch_goes_on_from_words_put_back_and_tells_nothing() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = words_put_back_and_synced(here.path(), there.path(), &clock);
        clock.advance(4 * 3600);
        edited_elsewhere(&mut s, &mut other, &t, "Typed here, and on from it");
        assert!(kept_notices(&s).is_empty());
        assert!(kept_words(here.path()).is_empty());
    }

    #[test]
    fn a_task_deleted_after_the_launch_with_words_put_back_in_it_tells_nothing() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = words_put_back_and_synced(here.path(), there.path(), &clock);
        clock.advance(60);
        s.delete_draft(&t).unwrap();
        clock.advance(60);
        let p = s.file().data.tasks[&t].project_id.clone();
        other.create_task(&tinput("Another", &p)).unwrap();
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        assert!(kept_notices(&s).is_empty());
    }

    #[test]
    fn a_task_deleted_elsewhere_after_the_launch_with_words_put_back_in_it_tells_nothing() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = words_put_back_and_synced(here.path(), there.path(), &clock);
        clock.advance(4 * 3600);
        other.delete_draft(&t).unwrap();
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        assert!(!s.file().data.tasks[&t].is_live());
        assert!(kept_notices(&s).is_empty());
    }

    #[test]
    fn a_file_of_kept_words_that_cannot_be_read_now_is_left_for_the_next_launch() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        drop(open(dir.path(), &clock));
        // A folder in its place can't be read, like a file something else has open.
        let path = dir.path().join("z-work-kept-words.json");
        std::fs::create_dir(&path).unwrap();
        let mut s = open(dir.path(), &clock);
        let notices = s.snapshot().notices;
        assert_eq!(notices.len(), 1);
        assert!(notices[0].message.contains("can't be read"));
        assert!(notices[0].message.contains("next launch"));
        assert!(path.is_dir(), "not moved");
        s.keep_unsaved_words("gone", "Old name", Some("New name"), None);
        s.on_exit().unwrap();
        assert!(path.is_dir(), "not written over");
        drop(s);
        std::fs::remove_dir(&path).unwrap();
        let s = open(dir.path(), &clock);
        assert!(s.snapshot().notices.is_empty());
    }

    #[test]
    fn a_file_of_kept_words_that_can_be_read_again_takes_this_runs_words_and_shows_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.keep_unsaved_words("gone 1", "First", Some("Kept before"), None);
        s.on_exit().unwrap();
        drop(s);
        // A folder in its place can't be read, like a file something else has open for a while.
        let path = dir.path().join("z-work-kept-words.json");
        let aside = dir.path().join("in use");
        std::fs::rename(&path, &aside).unwrap();
        std::fs::create_dir(&path).unwrap();
        clock.advance(1);
        let mut s = open(dir.path(), &clock);
        assert!(kept_notices(&s).is_empty());
        s.keep_unsaved_words("gone 2", "Second", Some("Kept in this run"), None);
        assert_eq!(told_to_copy(&s), 1);
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&aside, &path).unwrap();
        clock.advance(1);
        s.keep_unsaved_words("gone 3", "Third", Some("Kept later"), None);
        let mut words = kept_words(dir.path());
        words.sort();
        assert_eq!(words, ["Kept before", "Kept in this run", "Kept later"]);
        assert_eq!(kept_notices(&s).len(), 3);
        drop(s);
        assert_eq!(kept_notices(&open(dir.path(), &clock)).len(), 3);
    }

    #[test]
    fn a_second_file_of_kept_words_that_cannot_be_read_does_not_replace_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        drop(open(dir.path(), &clock));
        let path = dir.path().join("z-work-kept-words.json");
        for damaged in ["{ first", "{ second"] {
            std::fs::write(&path, damaged).unwrap();
            drop(open(dir.path(), &clock));
        }
        assert_eq!(
            files_aside(dir.path(), "z-work-kept-words"),
            ["{ first", "{ second"]
        );
    }

    #[test]
    fn the_stash_of_words_kept_for_a_gone_task_goes_only_once_their_file_has_them() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.stash_draft("gone", &doc("Typed here"), &Value::Null)
            .unwrap();
        let blocker = dir.path().join("z-work-kept-words.json.tmp");
        std::fs::create_dir(&blocker).unwrap();
        s.keep_unsaved_words("gone", "Old name", None, Some(&doc("Typed here")));
        assert_eq!(
            kept_notices(&s).len(),
            1,
            "the notice shows them all the same"
        );
        assert!(!drafts_on_disk(dir.path()).is_empty());
        std::fs::remove_dir(&blocker).unwrap();
        s.stash_draft("gone 2", &doc("Typed there"), &Value::Null)
            .unwrap();
        s.keep_unsaved_words("gone 2", "Other name", None, Some(&doc("Typed there")));
        assert_eq!(kept_words(dir.path()).len(), 2);
        assert_eq!(
            drafts_on_disk(dir.path()).into_keys().collect::<Vec<_>>(),
            ["gone"],
        );
    }

    #[test]
    fn words_kept_for_a_gone_task_keep_their_lines_and_a_new_name() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let lines = serde_json::json!({"type":"doc","content":[
            {"type":"paragraph","content":[{"type":"text","text":"Line one"}]},
            {"type":"paragraph","content":[{"type":"text","text":"Line two"}]},
        ]});
        s.keep_unsaved_words("gone", "Old name", Some("New name"), Some(&lines));
        assert_eq!(kept_words(dir.path()), ["New name\nLine one\nLine two"]);
    }

    #[test]
    fn words_kept_for_a_gone_task_keep_a_line_break_as_a_space_between_their_words() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        // Shift+Enter makes a `hardBreak` inside the paragraph, and Enter makes the next paragraph.
        let typed = serde_json::json!({"type":"doc","content":[
            {"type":"paragraph","content":[
                {"type":"text","text":"Call Anna"},{"type":"hardBreak"},{"type":"text","text":"about the invoice"}
            ]},
            {"type":"paragraph","content":[{"type":"text","text":"Then send it"}]},
        ]});
        s.keep_unsaved_words("gone", "Old name", None, Some(&typed));
        assert_eq!(
            kept_words(dir.path()),
            ["Call Anna about the invoice\nThen send it"]
        );
        assert!(
            kept_notices(&s)[0].contains("Call Anna about the invoice\nThen send it"),
            "and the notice shows them the same way"
        );
    }

    #[test]
    fn dismissing_any_notice_reads_as_newer_so_no_older_reply_brings_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.notify("warning", "Something to read".to_owned());
        let before = s.revision();
        let id = s.snapshot().notices[0].id.clone();
        s.dismiss_notice(&id);
        assert!(s.revision() > before);
    }

    #[test]
    fn keeping_and_dismissing_words_read_as_newer_so_no_older_reply_undoes_them() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let before = s.snapshot().revision;
        s.keep_unsaved_words("gone", "Old name", Some("New name"), None);
        let kept = s.snapshot();
        assert!(kept.revision > before);
        s.dismiss_notice(&kept.notices[0].id);
        assert!(s.snapshot().revision > kept.revision);
    }

    fn told_to_copy(s: &Service) -> usize {
        s.snapshot()
            .notices
            .iter()
            .filter(|n| n.message.contains("before you close the app"))
            .count()
    }

    #[test]
    fn words_kept_while_their_file_cannot_be_read_are_said_to_be_at_risk() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        drop(open(dir.path(), &clock));
        let path = dir.path().join("z-work-kept-words.json");
        std::fs::create_dir(&path).unwrap();
        let mut s = open(dir.path(), &clock);
        assert_eq!(told_to_copy(&s), 0, "nothing is at risk yet");
        s.keep_unsaved_words("gone", "Old name", Some("New name"), None);
        assert_eq!(told_to_copy(&s), 1);
        s.keep_unsaved_words("gone 2", "Other name", Some("Other new name"), None);
        assert_eq!(told_to_copy(&s), 1, "once is enough");
        assert_eq!(kept_notices(&s).len(), 2);
    }

    /// The warning that kept words may be lost, as it is on screen.
    fn dismiss_at_risk_warning(s: &mut Service) {
        let warning = s
            .snapshot()
            .notices
            .into_iter()
            .find(|n| n.message.contains("before you close the app"))
            .unwrap();
        s.dismiss_notice(&warning.id);
    }

    #[test]
    fn a_warning_dismissed_while_writes_still_fail_is_raised_again_by_the_next_words_kept() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        std::fs::create_dir(dir.path().join("z-work-kept-words.json.tmp")).unwrap();
        s.keep_unsaved_words("gone 1", "First", Some("First words"), None);
        assert_eq!(told_to_copy(&s), 1);
        dismiss_at_risk_warning(&mut s);
        assert_eq!(told_to_copy(&s), 0, "nothing new has gone wrong yet");
        s.keep_unsaved_words("gone 2", "Second", Some("Second words"), None);
        assert_eq!(
            told_to_copy(&s),
            1,
            "these words may be lost, and nothing else on screen says so"
        );
        s.keep_unsaved_words("gone 3", "Third", Some("Third words"), None);
        assert_eq!(told_to_copy(&s), 1, "while it is up, once is enough");
    }

    #[test]
    fn a_warning_still_up_when_writes_work_for_a_while_is_not_raised_a_second_time() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let blocker = dir.path().join("z-work-kept-words.json.tmp");
        std::fs::create_dir(&blocker).unwrap();
        s.keep_unsaved_words("gone 1", "First", Some("First words"), None);
        assert_eq!(told_to_copy(&s), 1);
        std::fs::remove_dir(&blocker).unwrap();
        s.keep_unsaved_words("gone 2", "Second", Some("Second words"), None);
        assert_eq!(kept_words(dir.path()).len(), 2);
        std::fs::create_dir(&blocker).unwrap();
        s.keep_unsaved_words("gone 3", "Third", Some("Third words"), None);
        assert_eq!(told_to_copy(&s), 1);
    }

    #[test]
    fn a_warning_dismissed_when_writes_work_again_stays_gone() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let blocker = dir.path().join("z-work-kept-words.json.tmp");
        std::fs::create_dir(&blocker).unwrap();
        s.keep_unsaved_words("gone 1", "First", Some("First words"), None);
        dismiss_at_risk_warning(&mut s);
        std::fs::remove_dir(&blocker).unwrap();
        s.keep_unsaved_words("gone 2", "Second", Some("Second words"), None);
        assert_eq!(told_to_copy(&s), 0);
        assert_eq!(kept_words(dir.path()).len(), 2);
    }

    #[test]
    fn words_put_back_that_lose_while_their_file_cannot_be_written_are_said_to_be_at_risk() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        let shown = s.file().data.tasks[&t].description.clone();
        s.stash_draft(&t, &doc("Typed here"), &shown).unwrap();
        s.on_exit().unwrap();
        drop(s);
        clock.advance(30);
        let mut s = open(here.path(), &clock);
        let blocker = here.path().join("z-work-kept-words.json.tmp");
        std::fs::create_dir(&blocker).unwrap();
        clock.advance(1);
        edited_elsewhere(&mut s, &mut other, &t, "From another device");
        assert_eq!(kept_notices(&s).len(), 1);
        assert_eq!(told_to_copy(&s), 1);
        assert!(kept_words(here.path()).is_empty());
        std::fs::remove_dir(&blocker).unwrap();
        s.on_exit().unwrap();
        assert_eq!(kept_words(here.path()), ["Typed here"]);
    }

    #[test]
    fn nothing_is_said_of_a_failed_write_when_no_words_are_left_to_lose() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.keep_unsaved_words("gone", "Old name", Some("New name"), None);
        let blocker = dir.path().join("z-work-kept-words.json");
        let id = s.snapshot().notices[0].id.clone();
        // With nothing left the file only has to go, and a folder in its place can't.
        std::fs::remove_file(&blocker).unwrap();
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(blocker.join("in the way"), "x").unwrap();
        s.dismiss_notice(&id);
        assert!(s.snapshot().notices.is_empty());
    }

    #[test]
    fn words_stay_a_draft_until_their_own_file_has_them() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        let shown = s.file().data.tasks[&t].description.clone();
        s.stash_draft(&t, &doc("Typed here"), &shown).unwrap();
        clock.advance(3);
        edited_elsewhere(&mut s, &mut other, &t, "From another device");
        s.on_exit().unwrap();
        drop(s);
        // A folder where the file's temporary copy goes makes the write fail.
        let blocker = here.path().join("z-work-kept-words.json.tmp");
        std::fs::create_dir(&blocker).unwrap();
        let s = open(here.path(), &clock);
        assert_eq!(kept_notices(&s).len(), 1);
        assert_eq!(told_to_copy(&s), 1);
        assert!(!drafts_on_disk(here.path()).is_empty());
        assert!(kept_words(here.path()).is_empty());
        drop(s);
        std::fs::remove_dir(&blocker).unwrap();
        let s = open(here.path(), &clock);
        assert_eq!(kept_notices(&s).len(), 1);
        assert_eq!(told_to_copy(&s), 0);
        assert_eq!(kept_words(here.path()), ["Typed here"]);
        assert!(drafts_on_disk(here.path()).is_empty());
    }

    #[test]
    fn a_launch_cut_short_before_it_cleared_a_draft_tells_of_its_words_once() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        let shown = s.file().data.tasks[&t].description.clone();
        s.stash_draft(&t, &doc("Typed here"), &shown).unwrap();
        let draft = drafts_on_disk(here.path());
        clock.advance(3);
        edited_elsewhere(&mut s, &mut other, &t, "From another device");
        s.on_exit().unwrap();
        drop(s);
        drop(open(here.path(), &clock));
        // As if the launch had stopped between writing the words and clearing the draft.
        crate::drafts::write(here.path(), &draft).unwrap();
        let s = open(here.path(), &clock);
        assert_eq!(kept_notices(&s).len(), 1);
        assert_eq!(kept_words(here.path()), ["Typed here"]);
        assert!(drafts_on_disk(here.path()).is_empty());
    }

    /// Words on a task that changed elsewhere, and a launch that couldn't write them to their file, so they
    /// stay a draft. Returns with what blocks that write.
    fn words_a_launch_left_a_draft(
        here: &Path,
        there: &Path,
        clock: &ManualClock,
    ) -> (Service, String, PathBuf) {
        let (mut s, mut other, t) = two_devices(here, there, clock);
        let shown = s.file().data.tasks[&t].description.clone();
        s.stash_draft(&t, &doc("Typed here"), &shown).unwrap();
        clock.advance(3);
        edited_elsewhere(&mut s, &mut other, &t, "From another device");
        s.on_exit().unwrap();
        drop(s);
        let blocker = here.join("z-work-kept-words.json.tmp");
        std::fs::create_dir(&blocker).unwrap();
        clock.advance(60);
        let s = open(here, clock);
        assert_eq!(kept_notices(&s).len(), 1);
        assert!(!drafts_on_disk(here).is_empty());
        (s, t, blocker)
    }

    /// Dismisses the notice showing kept words.
    fn dismiss_kept_notice(s: &mut Service, words: &str) {
        let notice = s
            .snapshot()
            .notices
            .into_iter()
            .find(|n| n.message.contains("Your unsaved words") && n.message.contains(words))
            .unwrap();
        s.dismiss_notice(&notice.id);
    }

    #[test]
    fn words_dismissed_after_a_launch_left_them_a_draft_do_not_come_back() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _, blocker) = words_a_launch_left_a_draft(here.path(), there.path(), &clock);
        std::fs::remove_dir(&blocker).unwrap();
        dismiss_kept_notice(&mut s, "Typed here");
        assert!(drafts_on_disk(here.path()).is_empty());
        drop(s);
        clock.advance(60);
        let s = open(here.path(), &clock);
        assert!(kept_notices(&s).is_empty(), "dismissed for good");
    }

    #[test]
    fn a_draft_a_launch_left_goes_when_its_words_reach_their_file() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _, blocker) = words_a_launch_left_a_draft(here.path(), there.path(), &clock);
        std::fs::remove_dir(&blocker).unwrap();
        s.on_exit().unwrap();
        assert_eq!(kept_words(here.path()), ["Typed here"]);
        assert!(drafts_on_disk(here.path()).is_empty());
        drop(s);
        clock.advance(60);
        let s = open(here.path(), &clock);
        assert_eq!(kept_notices(&s).len(), 1, "from the one place they are now");
    }

    #[test]
    fn a_draft_a_launch_left_stays_while_their_file_still_cannot_be_written() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _, _blocker) = words_a_launch_left_a_draft(here.path(), there.path(), &clock);
        s.on_exit().unwrap();
        assert!(kept_words(here.path()).is_empty());
        assert!(!drafts_on_disk(here.path()).is_empty());
        drop(s);
        std::fs::remove_dir(here.path().join("z-work-kept-words.json.tmp")).unwrap();
        clock.advance(60);
        let s = open(here.path(), &clock);
        assert_eq!(kept_words(here.path()), ["Typed here"]);
        assert_eq!(kept_notices(&s).len(), 1);
    }

    #[test]
    fn words_typed_since_a_launch_left_a_draft_stay_when_that_draft_goes() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t, blocker) = words_a_launch_left_a_draft(here.path(), there.path(), &clock);
        let stored = s.file().data.tasks[&t].description.clone();
        let p = s.file().data.tasks[&t].project_id.clone();
        let another = s.create_task(&tinput("Another", &p)).unwrap();
        clock.advance(1);
        s.stash_draft(&t, &doc("Typed since"), &stored).unwrap();
        s.stash_draft(&another, &doc("Typed there too"), &Value::Null)
            .unwrap();
        std::fs::remove_dir(&blocker).unwrap();
        dismiss_kept_notice(&mut s, "Typed here");
        let left = drafts_on_disk(here.path());
        assert_eq!(left[&t].description, doc("Typed since"));
        assert_eq!(left[&another].description, doc("Typed there too"));
        drop(s);
        clock.advance(60);
        let s = open(here.path(), &clock);
        assert_eq!(s.file().data.tasks[&t].description, doc("Typed since"));
        assert!(kept_notices(&s).is_empty());
    }

    #[test]
    fn a_draft_a_launch_left_next_to_another_task_goes_alone() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t, blocker) = words_a_launch_left_a_draft(here.path(), there.path(), &clock);
        let p = s.file().data.tasks[&t].project_id.clone();
        let another = s.create_task(&tinput("Another", &p)).unwrap();
        s.stash_draft(&another, &doc("Typed there"), &Value::Null)
            .unwrap();
        std::fs::remove_dir(&blocker).unwrap();
        s.on_exit().unwrap();
        assert_eq!(
            drafts_on_disk(here.path()).into_keys().collect::<Vec<_>>(),
            [another]
        );
    }

    /// Words in their file and still a draft too, as a launch cut short can leave them, with something else
    /// holding that file at this launch. Returns with its copy, to go back in its place.
    fn words_in_a_file_in_use_and_a_draft(
        here: &Path,
        there: &Path,
        clock: &ManualClock,
    ) -> (Service, PathBuf, PathBuf) {
        let (mut s, mut other, t) = two_devices(here, there, clock);
        let shown = s.file().data.tasks[&t].description.clone();
        s.stash_draft(&t, &doc("Typed here"), &shown).unwrap();
        let draft = drafts_on_disk(here);
        clock.advance(3);
        edited_elsewhere(&mut s, &mut other, &t, "From another device");
        s.on_exit().unwrap();
        drop(s);
        drop(open(here, clock));
        crate::drafts::write(here, &draft).unwrap();
        // A folder in its place can't be read, like a file something else has open.
        let path = here.join("z-work-kept-words.json");
        let aside = here.join("in use");
        std::fs::rename(&path, &aside).unwrap();
        std::fs::create_dir(&path).unwrap();
        clock.advance(60);
        let s = open(here, clock);
        assert_eq!(kept_notices(&s).len(), 1);
        (s, path, aside)
    }

    #[test]
    fn words_dismissed_do_not_come_back_from_a_file_read_again_in_that_run() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, path, aside) =
            words_in_a_file_in_use_and_a_draft(here.path(), there.path(), &clock);
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&aside, &path).unwrap();
        dismiss_kept_notice(&mut s, "Typed here");
        assert!(kept_notices(&s).is_empty(), "brought back at once");
        assert!(kept_words(here.path()).is_empty());
        assert!(drafts_on_disk(here.path()).is_empty());
        drop(s);
        clock.advance(60);
        let s = open(here.path(), &clock);
        assert!(kept_notices(&s).is_empty());
    }

    #[test]
    fn words_dismissed_while_their_file_is_in_use_do_not_come_back_when_it_is_read() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, path, aside) =
            words_in_a_file_in_use_and_a_draft(here.path(), there.path(), &clock);
        dismiss_kept_notice(&mut s, "Typed here");
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&aside, &path).unwrap();
        s.keep_unsaved_words("gone", "Old name", Some("Other words"), None);
        let words = kept_notices(&s);
        assert_eq!(words.len(), 1, "{words:?}");
        assert!(words[0].contains("Other words"));
        assert_eq!(kept_words(here.path()), ["Other words"]);
        assert!(drafts_on_disk(here.path()).is_empty());
    }

    #[test]
    fn a_file_read_again_still_shows_the_words_nobody_dismissed() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, path, aside) =
            words_in_a_file_in_use_and_a_draft(here.path(), there.path(), &clock);
        let mut in_it: Vec<Kept> = serde_json::from_slice(&std::fs::read(&aside).unwrap()).unwrap();
        let mut more = in_it[0].clone();
        more.words = "Kept before as well".to_owned();
        in_it.push(more);
        std::fs::write(&aside, serde_json::to_vec(&in_it).unwrap()).unwrap();
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&aside, &path).unwrap();
        dismiss_kept_notice(&mut s, "Typed here");
        let words = kept_notices(&s);
        assert_eq!(words.len(), 1, "{words:?}");
        assert!(words[0].contains("Kept before as well"));
        assert_eq!(kept_words(here.path()), ["Kept before as well"]);
    }

    #[test]
    fn a_file_of_kept_words_that_is_damaged_is_told_of_and_not_written_over() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        drop(open(dir.path(), &clock));
        let path = dir.path().join("z-work-kept-words.json");
        std::fs::write(&path, b"{ not the words").unwrap();
        let s = open(dir.path(), &clock);
        let notices = s.snapshot().notices;
        assert_eq!(notices.len(), 1);
        assert!(notices[0].message.contains("can't be read"));
        assert!(notices[0].message.contains("z-work-kept-words.corrupt-"));
        assert!(!path.exists());
        assert_eq!(
            files_aside(dir.path(), "z-work-kept-words"),
            ["{ not the words"]
        );
        drop(s);
        let s = open(dir.path(), &clock);
        assert!(s.snapshot().notices.is_empty(), "told once");
    }

    /// A task with words stashed on it, and the app closed before they were saved.
    fn words_stashed_and_closed(dir: &Path, clock: &ManualClock) -> String {
        let mut s = open(dir, clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        let shown = s.file().data.tasks[&t].description.clone();
        clock.advance(5);
        s.stash_draft(&t, &doc("Held words"), &shown).unwrap();
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        t
    }

    /// The drafts in their file.
    fn drafts_on_disk(dir: &Path) -> BTreeMap<String, Draft> {
        crate::drafts::read(dir).unwrap().unwrap()
    }

    /// Takes away the rights to read a file, like something else having it open. False for a user who can
    /// read anything regardless, who can't run the test.
    #[cfg(unix)]
    fn make_unreadable(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::read(path).is_err()
    }

    #[cfg(unix)]
    fn make_readable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_drafts_file_that_cannot_be_read_at_a_launch_is_said_and_left_for_the_next_one() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let t = words_stashed_and_closed(dir.path(), &clock);
        let path = dir.path().join("z-work-drafts.json");
        let bytes = std::fs::read(&path).unwrap();
        if !make_unreadable(&path) {
            return;
        }
        let s = open(dir.path(), &clock);
        let notices = s.snapshot().notices;
        assert!(path.exists(), "deleted unread");
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].level, "warning");
        assert!(notices[0].message.contains("can't be read"));
        assert!(notices[0].message.contains("next launch"));
        assert_ne!(s.file().data.tasks[&t].description, doc("Held words"));
        drop(s);
        make_readable(&path);
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "written over");
        clock.advance(60);
        let s = open(dir.path(), &clock);
        assert_eq!(s.file().data.tasks[&t].description, doc("Held words"));
        assert!(!path.exists(), "done with once the words are back");
    }

    #[cfg(unix)]
    #[test]
    fn a_stash_or_a_clear_while_the_drafts_file_cannot_be_read_leaves_what_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let held = s.create_task(&tinput("Held", &p)).unwrap();
        let other = s.create_task(&tinput("Other", &p)).unwrap();
        s.stash_draft(&held, &doc("Held words"), &Value::Null)
            .unwrap();
        let path = dir.path().join("z-work-drafts.json");
        let bytes = std::fs::read(&path).unwrap();
        if !make_unreadable(&path) {
            return;
        }
        assert!(s.stash_draft(&other, &doc("Typed"), &Value::Null).is_err());
        assert!(s.clear_draft(&held).is_err());
        make_readable(&path);
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "written over");
        // That was only for as long as it lasted.
        s.stash_draft(&other, &doc("Typed"), &Value::Null).unwrap();
        let mut both = [held, other];
        both.sort();
        assert_eq!(
            drafts_on_disk(dir.path()).into_keys().collect::<Vec<_>>(),
            both
        );
    }

    #[test]
    fn a_drafts_file_a_launch_could_not_read_is_not_written_over_in_that_run() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let t = words_stashed_and_closed(dir.path(), &clock);
        // A folder in its place can't be read, like a file something else has open.
        let path = dir.path().join("z-work-drafts.json");
        let aside = dir.path().join("in use");
        std::fs::rename(&path, &aside).unwrap();
        std::fs::create_dir(&path).unwrap();
        let s = open(dir.path(), &clock);
        let notices = s.snapshot().notices;
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].message.contains("can't be read"));
        assert!(path.is_dir(), "not moved");
        // Whatever had it lets go, with the app running. What is in it hasn't been put back or shown to anyone, and a
        // draft holds one lot of words for a task, so words typed on that task now must not take its place.
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&aside, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let stored = s.file().data.tasks[&t].description.clone();
        assert!(
            s.stash_draft(&t, &doc("Typed in this run"), &stored)
                .is_err()
        );
        assert!(s.clear_draft(&t).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        drop(s);
        let s = open(dir.path(), &clock);
        assert_eq!(s.file().data.tasks[&t].description, doc("Held words"));
    }

    #[test]
    fn words_that_lose_to_an_edit_made_here_are_not_said_to_have_changed_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let t = words_stashed_and_closed(dir.path(), &clock);
        // A run that can't read the drafts leaves them alone, and the user edits that very task in it.
        let path = dir.path().join("z-work-drafts.json");
        let aside = dir.path().join("in use");
        std::fs::rename(&path, &aside).unwrap();
        std::fs::create_dir(&path).unwrap();
        let mut s = open(dir.path(), &clock);
        clock.advance(5);
        s.patch_task(
            &t,
            &TaskPatch {
                description: Some(doc("Typed in the stuck run")),
                ..TaskPatch::default()
            },
            &TaskExpected::default(),
        )
        .unwrap();
        s.on_exit().unwrap();
        drop(s);
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&aside, &path).unwrap();
        clock.advance(60);
        let s = open(dir.path(), &clock);
        assert_eq!(
            s.file().data.tasks[&t].description,
            doc("Typed in the stuck run")
        );
        let words = kept_notices(&s);
        assert_eq!(words.len(), 1, "{words:?}");
        assert!(words[0].contains("Held words"));
        // True of an edit from another device and of this one alike.
        assert!(
            !words[0].contains("elsewhere"),
            "said of an edit made here: {}",
            words[0]
        );
        assert!(words[0].contains("its description changed while they waited"));
    }

    #[test]
    fn a_drafts_file_that_is_damaged_is_told_of_and_not_written_over() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        drop(open(dir.path(), &clock));
        let path = dir.path().join("z-work-drafts.json");
        std::fs::write(&path, b"{ not the drafts").unwrap();
        let s = open(dir.path(), &clock);
        let notices = s.snapshot().notices;
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].level, "warning");
        assert!(notices[0].message.contains("can't be read"));
        assert!(notices[0].message.contains("z-work-drafts.corrupt-"));
        assert!(!path.exists());
        assert_eq!(
            files_aside(dir.path(), "z-work-drafts"),
            ["{ not the drafts"]
        );
        drop(s);
        let s = open(dir.path(), &clock);
        assert!(s.snapshot().notices.is_empty(), "told once");
    }

    #[test]
    fn a_drafts_file_damaged_during_a_run_is_not_written_over_and_goes_aside_at_the_next_launch() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        let path = dir.path().join("z-work-drafts.json");
        std::fs::write(&path, b"{ not the drafts").unwrap();
        assert!(s.stash_draft(&t, &doc("Typed"), &Value::Null).is_err());
        assert!(s.clear_draft(&t).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{ not the drafts");
        drop(s);
        let s = open(dir.path(), &clock);
        assert_eq!(
            files_aside(dir.path(), "z-work-drafts"),
            ["{ not the drafts"]
        );
        assert_eq!(s.snapshot().notices.len(), 1);
    }

    /// Stamps run ahead of the clock when saves come faster than one a second or another device's clock is
    /// ahead, so a stamp later than a draft does not show the draft is older than the save.
    #[test]
    fn stashing_the_same_words_again_keeps_the_time_they_were_stashed_first() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (s, t) = running(dir.path(), &clock);
        let shown = s.file().data.tasks[&t].description.clone();
        s.stash_draft(&t, &doc("Typed"), &shown).unwrap();
        let first = drafts_on_disk(dir.path())[&t].at;
        clock.advance(5);
        s.stash_draft(&t, &doc("Typed"), &shown).unwrap();
        assert_eq!(drafts_on_disk(dir.path())[&t].at, first);
        clock.advance(5);
        s.stash_draft(&t, &doc("Typed on"), &shown).unwrap();
        assert!(drafts_on_disk(dir.path())[&t].at > first);
    }

    #[test]
    fn held_words_stashed_again_after_this_devices_newer_text_are_shown_not_put_back_over_it() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t) = running(dir.path(), &clock);
        let shown = s.file().data.tasks[&t].description.clone();
        s.stash_draft(&t, &doc("Held words"), &shown).unwrap();
        clock.advance(2);
        s.patch_task(
            &t,
            &TaskPatch {
                description: Some(doc("Newer here")),
                ..TaskPatch::default()
            },
            &TaskExpected::default(),
        )
        .unwrap();
        clock.advance(2);
        // What the page does when the save of those words is refused.
        s.stash_draft(&t, &doc("Held words"), &shown).unwrap();
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let s = open(dir.path(), &clock);
        assert_eq!(s.file().data.tasks[&t].description, doc("Newer here"));
        let words = kept_notices(&s);
        assert_eq!(words.len(), 1, "{:?}", s.snapshot().notices);
        assert!(words[0].contains("Held words"));
    }

    #[test]
    fn a_draft_with_an_earlier_time_than_this_devices_save_is_shown_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        let shown = s.file().data.tasks[&t].description.clone();
        s.stash_draft(&t, &doc("Saved three and more"), &shown)
            .unwrap();
        for words in ["Saved one", "Saved two", "Saved three"] {
            s.patch_task(
                &t,
                &TaskPatch {
                    description: Some(doc(words)),
                    ..TaskPatch::default()
                },
                &TaskExpected::default(),
            )
            .unwrap();
        }
        s.on_exit().unwrap();
        drop(s);
        let s = open(dir.path(), &clock);
        assert_eq!(s.file().data.tasks[&t].description, doc("Saved three"));
        let notices = s.snapshot().notices;
        assert!(notices[0].message.contains("Saved three and more"));
    }

    #[test]
    fn a_draft_that_differs_from_the_stored_text_only_in_subtask_ids_was_saved() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        let shown = s.file().data.tasks[&t].description.clone();
        let list = serde_json::json!({"type":"doc","content":[{"type":"taskList","content":[{"type":"taskItem","attrs":{"checked":false,"subtaskId":null},"content":[{"type":"paragraph","content":[{"type":"text","text":"One"}]}]}]}]});
        s.stash_draft(&t, &list, &shown).unwrap();
        clock.advance(1);
        s.patch_task(
            &t,
            &TaskPatch {
                description: Some(list),
                ..TaskPatch::default()
            },
            &TaskExpected::default(),
        )
        .unwrap();
        s.on_exit().unwrap();
        drop(s);
        let s = open(dir.path(), &clock);
        assert!(s.snapshot().notices.is_empty());
    }

    #[test]
    fn words_put_back_at_launch_that_lose_to_the_first_sync_are_shown_not_lost() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        let shown = s.file().data.tasks[&t].description.clone();
        clock.advance(2);
        s.stash_draft(&t, &doc("Typed here"), &shown).unwrap();
        s.on_exit().unwrap();
        drop(s);
        // The other device edits while this one is off, and later than the words were typed.
        clock.advance(30);
        other
            .patch_task(
                &t,
                &TaskPatch {
                    description: Some(doc("From another device")),
                    ..TaskPatch::default()
                },
                &TaskExpected::default(),
            )
            .unwrap();
        clock.advance(30);
        let mut s = open(here.path(), &clock);
        assert_eq!(s.file().data.tasks[&t].description, doc("Typed here"));
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        assert_eq!(
            s.file().data.tasks[&t].description,
            doc("From another device")
        );
        let notices = s.snapshot().notices;
        assert!(notices.iter().any(|n| n.message.contains("Typed here")));
        // Told once in this run, and kept for the next like any other.
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        assert_eq!(s.snapshot().notices.len(), notices.len());
        assert_eq!(kept_words(here.path()), ["Typed here"]);
        s.on_exit().unwrap();
        drop(s);
        let s = open(here.path(), &clock);
        assert!(
            s.snapshot()
                .notices
                .iter()
                .any(|n| n.message.contains("Typed here"))
        );
    }

    #[test]
    fn a_draft_with_no_text_that_met_newer_text_is_nothing_to_tell() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        let shown = s.file().data.tasks[&t].description.clone();
        s.stash_draft(&t, &Value::Null, &shown).unwrap();
        clock.advance(3);
        edited_elsewhere(&mut s, &mut other, &t, "From another device");
        s.on_exit().unwrap();
        drop(s);
        clock.advance(60);
        let s = open(here.path(), &clock);
        assert_eq!(
            s.file().data.tasks[&t].description,
            doc("From another device")
        );
        assert!(s.snapshot().notices.is_empty());
    }

    /// A known limitation, in the README: a save checks only against what has reached its own PC. If this
    /// behaviour changes, so does that entry.
    #[test]
    fn two_pcs_saving_one_field_before_a_sync_keep_the_later_save_and_say_nothing() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        let base = s.file().data.tasks[&t].description.clone();
        let expected = TaskExpected {
            description: Some(base),
            ..TaskExpected::default()
        };
        let save = |svc: &mut Service, words: &str| {
            let patch = TaskPatch {
                description: Some(doc(words)),
                ..TaskPatch::default()
            };
            svc.patch_task(&t, &patch, &expected).unwrap().conflicts
        };
        clock.advance(5);
        assert!(save(&mut s, "Words on this PC").is_empty());
        clock.advance(5);
        assert!(save(&mut other, "Words on the other PC").is_empty());
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        other.absorb_remote(&s.file().data.clone(), BTreeMap::new(), Vec::new());
        assert_eq!(
            s.file().data.tasks[&t].description,
            doc("Words on the other PC")
        );
        assert_eq!(
            other.file().data.tasks[&t].description,
            doc("Words on the other PC")
        );
        assert!(s.snapshot().notices.is_empty());
        assert!(other.snapshot().notices.is_empty());
    }

    #[test]
    fn a_merge_landing_between_the_read_and_the_save_is_refused_at_the_write() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        // The editor reads what is stored.
        let task = s.snapshot().tasks.into_iter().find(|x| x.id == t).unwrap();
        let revision = s.revision();
        // A sync lands before the save does.
        clock.advance(3);
        edited_elsewhere(&mut s, &mut other, &t, "From another device");
        assert!(s.revision() > revision, "a merge counts as a change");
        clock.advance(2);
        let out = s
            .patch_task(
                &t,
                &TaskPatch {
                    name: Some("Mine".into()),
                    description: Some(doc("My words")),
                    ..TaskPatch::default()
                },
                &TaskExpected {
                    name: Some(task.name),
                    description: Some(task.description),
                    ..TaskExpected::default()
                },
            )
            .unwrap();
        assert_eq!(out.conflicts, vec![crate::machine::TaskField::Description]);
        assert_eq!(out.stored.description, doc("From another device"));
        assert_eq!(out.stored.name, "Mine", "the rest of the patch goes");
        let stored = &s.file().data.tasks[&t];
        assert_eq!(stored.description, doc("From another device"));
        assert_eq!(stored.name, "Mine");
        // And the refusal holds across a restart: what was written is what the reply said.
        s.on_exit().unwrap();
        drop(s);
        let s = open(here.path(), &clock);
        assert_eq!(
            s.file().data.tasks[&t].description,
            doc("From another device")
        );
    }

    #[test]
    fn a_merge_of_another_field_never_conflicts_and_the_snapshot_counts_changes() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        let task = s.snapshot().tasks.into_iter().find(|x| x.id == t).unwrap();
        let before = s.snapshot().revision;
        clock.advance(3);
        other
            .patch_task(
                &t,
                &TaskPatch {
                    name: Some("Renamed elsewhere".into()),
                    ..TaskPatch::default()
                },
                &TaskExpected::default(),
            )
            .unwrap();
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        let out = s
            .patch_task(
                &t,
                &TaskPatch {
                    description: Some(doc("My words")),
                    ..TaskPatch::default()
                },
                &TaskExpected {
                    description: Some(task.description),
                    ..TaskExpected::default()
                },
            )
            .unwrap();
        assert_eq!(out.conflicts, Vec::new());
        assert_eq!(out.stored.name, "Renamed elsewhere");
        assert_eq!(out.stored.description, doc("My words"));
        assert!(s.snapshot().revision > before);
    }

    #[test]
    fn a_failed_save_at_launch_keeps_the_draft() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        clock.advance(5);
        s.stash_draft(&t, &doc("Typed"), &Value::Null).unwrap();
        s.on_exit().unwrap();
        drop(s);
        // The launch's own save fails inside its transaction.
        crate::store::block_new_stores(true);
        clock.advance(5);
        let failed = Service::open(dir.path(), Arc::new(clock.clone()), "pc");
        crate::store::block_new_stores(false);
        assert!(failed.is_err());
        drop(failed);
        assert!(!drafts_on_disk(dir.path()).is_empty());
        let s = open(dir.path(), &clock);
        assert_eq!(s.file().data.tasks[&t].description, doc("Typed"));
    }

    #[test]
    fn two_pause_requests_close_at_the_earliest_with_one_audit_entry() {
        let dir_a = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut owner, t) = running(dir_a.path(), &clock);
        clock.advance(600);
        let mut data = owner.file().data.clone();
        let seg = data.segments.keys().next().unwrap().clone();
        let at = |n: i64| Ts(Ts::parse(T0).unwrap().0 + n);
        for (id, n) in [("late", 500), ("early", 100)] {
            data.pause_requests.insert(
                id.into(),
                crate::model::PauseRequest {
                    id: id.into(),
                    task_id: t.clone(),
                    segment_id: seg.clone(),
                    requested_by: "pc-b".into(),
                    at: at(n),
                    stamp: crate::model::Stamp::default(),
                },
            );
        }
        owner.absorb_remote(&data, BTreeMap::new(), Vec::new());
        assert_eq!(owner.file().data.segments[&seg].end, Some(at(100)));
        let audits = owner
            .file()
            .data
            .audit_log
            .values()
            .filter(|a| a.action == "pausedByRequest")
            .count();
        assert_eq!(audits, 1);
    }

    #[test]
    fn a_saved_edit_clears_its_draft() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        clock.advance(5);
        s.stash_draft(&t, &doc("Draft"), &Value::Null).unwrap();
        s.clear_draft(&t).unwrap();
        drop(s);
        clock.advance(5);
        let s = open(dir.path(), &clock);
        assert_ne!(s.file().data.tasks[&t].description, doc("Draft"));
    }

    #[test]
    fn the_owner_is_told_once_which_task_was_paused_and_by_which_device() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut owner, t) = running(dir_a.path(), &clock);
        owner.set_system_zone("UTC".into());
        let mut asker = open(dir_b.path(), &clock);
        clock.advance(600);
        asker.absorb_remote(&owner.file().data.clone(), BTreeMap::new(), Vec::new());
        asker.request_remote_pause(&t).unwrap();
        let at = asker.file().data.pause_requests.values().next().unwrap().at;
        let by = asker.file().device_id.clone();
        clock.advance(600);
        let seen = BTreeMap::from([(
            by,
            crate::model::RemoteDevice {
                name: "Office PC".into(),
                last_heartbeat: None,
                written_at: clock.now(),
            },
        )]);
        assert!(owner.absorb_remote(&asker.file().data.clone(), seen.clone(), Vec::new()));
        let name = owner.file().data.tasks[&t].name.clone();
        let notices = owner.snapshot().notices;
        assert_eq!(notices.len(), 1, "{notices:?}");
        let text = &notices[0].message;
        assert!(text.contains(&format!("\"{name}\"")), "{text}");
        assert!(text.contains("Office PC"), "{text}");
        assert!(text.contains(&at.iso()[11..16]), "{text}");
        assert_eq!(notices[0].level, "warning");
        // One system notification too, for when the window is hidden, and only one.
        assert_eq!(owner.take_pause_alerts(), std::slice::from_ref(text));
        assert!(owner.take_pause_alerts().is_empty());

        // The next cycles bring the same request again: it has done its work.
        clock.advance(600);
        owner.absorb_remote(&asker.file().data.clone(), seen.clone(), Vec::new());
        assert_eq!(owner.snapshot().notices.len(), 1);
        assert!(owner.take_pause_alerts().is_empty());
        let id = owner.snapshot().notices[0].id.clone();
        owner.dismiss_notice(&id);
        owner.absorb_remote(&asker.file().data.clone(), seen, Vec::new());
        assert!(
            owner.snapshot().notices.is_empty(),
            "dismissed stays dismissed"
        );
    }

    #[test]
    fn the_pause_notice_gives_the_time_in_the_zone_being_viewed() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut owner, t) = running(dir_a.path(), &clock);
        owner.set_system_zone("UTC".into());
        owner
            .set_setting("myZone", serde_json::json!("Asia/Tokyo"))
            .unwrap();
        let mut asker = open(dir_b.path(), &clock);
        asker.absorb_remote(&owner.file().data.clone(), BTreeMap::new(), Vec::new());
        asker.request_remote_pause(&t).unwrap();
        let at = asker.file().data.pause_requests.values().next().unwrap().at;
        owner.absorb_remote(&asker.file().data.clone(), BTreeMap::new(), Vec::new());
        let expected = chrono::DateTime::from_timestamp(at.secs(), 0)
            .unwrap()
            .with_timezone(&chrono_tz::Asia::Tokyo)
            .format("%Y-%m-%d %H:%M")
            .to_string();
        let notices = owner.snapshot().notices;
        assert!(notices[0].message.contains(&expected), "{notices:?}");
    }

    #[test]
    fn a_requesting_device_with_no_known_name_is_still_mentioned() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut owner, t) = running(dir_a.path(), &clock);
        let mut asker = open(dir_b.path(), &clock);
        clock.advance(600);
        asker.absorb_remote(&owner.file().data.clone(), BTreeMap::new(), Vec::new());
        asker.request_remote_pause(&t).unwrap();
        owner.absorb_remote(&asker.file().data.clone(), BTreeMap::new(), Vec::new());
        let notices = owner.snapshot().notices;
        assert!(notices[0].message.contains("another device"), "{notices:?}");
    }

    #[test]
    fn a_pause_request_reaches_the_owner_who_closes_its_own_segment() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut owner, t) = running(dir_a.path(), &clock);
        let mut asker = open(dir_b.path(), &clock);
        clock.advance(600);
        asker.absorb_remote(&owner.file().data.clone(), BTreeMap::new(), Vec::new());
        let seg_before = asker.file().data.segments.values().next().unwrap().clone();
        let id = asker.request_remote_pause(&t).unwrap();
        assert_eq!(
            asker.request_remote_pause(&t).unwrap(),
            id,
            "asking twice is one request"
        );
        assert_eq!(
            asker.file().data.segments.values().next().unwrap(),
            &seg_before,
            "the asker never rewrites the owner's segment"
        );
        assert!(
            asker
                .file()
                .data
                .audit_log
                .values()
                .any(|a| a.action == "requestPause")
        );
        let at = asker.file().data.pause_requests[&id].at;
        clock.advance(600);
        owner.absorb_remote(&asker.file().data.clone(), BTreeMap::new(), Vec::new());
        let task = &owner.file().data.tasks[&t];
        assert_eq!(task.state, TaskState::Paused);
        assert_eq!(task.pause_reason, Some(PauseReason::RemoteRequest));
        assert_eq!(
            owner.file().data.segments.values().next().unwrap().end,
            Some(at)
        );
        assert!(
            owner
                .file()
                .data
                .audit_log
                .values()
                .any(|a| a.action == "pausedByRequest")
        );
        // A request can't be made for a task running here.
        owner.resume_task(&t).unwrap();
        assert!(matches!(
            owner.request_remote_pause(&t),
            Err(CoreError::Conflict(_))
        ));
    }

    #[test]
    fn one_shift_version_can_be_reverted_and_the_last_one_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let input = |from: &str, id: Option<String>| ShiftInput {
            shift_id: id,
            name: "Day".into(),
            color: "#336699".into(),
            start: "09:00".into(),
            end: "17:00".into(),
            weekdays: vec![1],
            zone: "UTC".into(),
            break_minutes: 0,
            effective_from: from.into(),
            retired: false,
        };
        let id = s.save_shift(&input("2026-01-01", None)).unwrap();
        s.save_shift(&input("2026-02-01", Some(id.clone())))
            .unwrap();
        let later = s
            .file()
            .data
            .shifts
            .values()
            .find(|v| v.effective_from == "2026-02-01")
            .unwrap()
            .id
            .clone();
        s.revert_shift_version(&later).unwrap();
        assert!(s.file().data.shifts[&later].deleted);
        let first = s
            .file()
            .data
            .shifts
            .values()
            .find(|v| v.effective_from == "2026-01-01")
            .unwrap()
            .id
            .clone();
        assert!(matches!(
            s.revert_shift_version(&first),
            Err(CoreError::Conflict(_))
        ));
        assert!(matches!(
            s.revert_shift_version(&later),
            Err(CoreError::NotFound(_))
        ));
    }

    #[test]
    fn billable_pause_audit_entries_name_the_task() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t) = running(dir.path(), &clock);
        clock.advance(7200);
        let id = s
            .mark_billable_pause(&crate::adjust::PauseInput {
                task_id: t.clone(),
                start: Ts::parse(T0).unwrap(),
                end: Ts::parse("2026-03-01T09:30:00Z").unwrap(),
                note: String::new(),
            })
            .unwrap();
        s.unmark_billable_pause(&id).unwrap();
        let entries = s.audit_entries(Some(&id));
        assert_eq!(entries.len(), 2);
        for e in entries {
            let named = [&e.before, &e.after]
                .iter()
                .any(|v| v["taskId"] == t.as_str());
            assert!(named, "{e:?}");
        }
    }

    /// Time that counts: the app sees someone at the keyboard throughout, so no time away comes of it.
    fn work(s: &mut Service, clock: &ManualClock, minutes: i64) {
        s.observe_activity(Some(0)).unwrap();
        for _ in 0..minutes {
            clock.advance(60);
            s.observe_activity(Some(0)).unwrap();
        }
    }

    /// A task with everything that can be logged on one: time, time away, a correction, a stretch entered
    /// by hand and a billable pause. Beside it, another task with an hour of its own.
    fn doomed_world(dir: &Path, clock: &ManualClock) -> (Service, String, String, String) {
        let mut s = open(dir, clock);
        s.set_system_zone("UTC".into());
        let p = s
            .create_project(&ProjectInput {
                budget_hours: Some(100.0),
                ..pinput("P")
            })
            .unwrap();
        let t = s.create_task(&tinput("Doomed", &p)).unwrap();
        let t0 = clock.now();
        s.start_task(&t).unwrap();
        work(&mut s, clock, 60);
        s.pause_task(&t).unwrap();
        let first = s.file().data.segments.keys().next().unwrap().clone();
        s.mutate(|f, c| {
            f.data
                .add_gap(c, &first, t0.later(60), t0.later(120), GapKind::Idle);
            Ok(())
        })
        .unwrap();
        clock.advance(600);
        s.resume_task(&t).unwrap();
        work(&mut s, clock, 30);
        s.stop_task(&t).unwrap();
        let second = s
            .file()
            .data
            .segments
            .keys()
            .find(|id| **id != first)
            .unwrap()
            .clone();
        s.correct_segment(&CorrectionInput {
            segment_id: second,
            start: t0.later(4200),
            end: t0.later(5900),
            reason: "A slip".into(),
            allow_overlap: true,
        })
        .unwrap();
        s.mark_billable_pause(&PauseInput {
            task_id: t.clone(),
            start: t0.later(3600),
            end: t0.later(4200),
            note: "A call".into(),
        })
        .unwrap();
        s.add_manual_entry(&ManualEntryInput {
            task_id: t.clone(),
            start: t0.later(-7200),
            end: t0.later(-3600),
            reason: "Forgot".into(),
            subtask_id: None,
            allow_overlap: true,
        })
        .unwrap();
        let kept = s.create_task(&tinput("Kept", &p)).unwrap();
        clock.advance(100);
        s.start_task(&kept).unwrap();
        work(&mut s, clock, 60);
        s.stop_task(&kept).unwrap();
        (s, p, t, kept)
    }

    fn csv_of(s: &Service) -> String {
        crate::csv::build(
            &s.export_source(),
            &crate::report::ExportRequest {
                day_zone: "UTC".into(),
                my_zone: "UTC".into(),
                client_zone: None,
                include_earnings: true,
                segment_shifts: BTreeMap::new(),
                analytics: Vec::new(),
                selection: None,
            },
        )
        .unwrap()
    }

    /// Whether the time of this task reaches anything that reads time.
    fn time_shows(s: &Service, t: &str, name: &str) -> Vec<&'static str> {
        let snap = s.snapshot();
        let src = s.export_source();
        let mut shows = Vec::new();
        let mut note = |seen: bool, what| {
            if seen {
                shows.push(what);
            }
        };
        note(snap.tasks.iter().any(|x| x.id == t), "tasks");
        note(snap.segments.iter().any(|x| x.task_id == t), "segments");
        note(
            snap.raw_segments.iter().any(|x| x.task_id == t),
            "raw segments",
        );
        note(snap.gaps.iter().any(|x| x.task_id == t), "time away");
        note(
            snap.pauses.iter().any(|x| x.task_id == t),
            "billable pauses",
        );
        note(
            src.rows(chrono_tz::UTC).iter().any(|r| r.row.task_id == t),
            "report rows",
        );
        note(
            src.segments().iter().any(|r| r.task_name == name),
            "report segments",
        );
        note(
            src.adjustments().iter().any(|r| r.task_name == name),
            "adjustments",
        );
        note(
            csv_of(s)
                .lines()
                .any(|line| line.split(',').nth(1) == Some(name)),
            "csv",
        );
        shows
    }

    #[test]
    fn a_task_deleted_with_its_time_leaves_every_view_and_stays_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, p, t, kept) = doomed_world(dir.path(), &clock);
        let every_view = [
            "tasks",
            "segments",
            "raw segments",
            "time away",
            "billable pauses",
            "report rows",
            "report segments",
            "adjustments",
            "csv",
        ];
        assert_eq!(time_shows(&s, &t, "Doomed"), every_view);
        let exact = |s: &Service| s.snapshot().projects[0].figures.exact_secs;
        let kept_row = |s: &Service| {
            s.export_source()
                .rows(chrono_tz::UTC)
                .into_iter()
                .find(|r| r.row.task_id == kept)
                .unwrap()
                .row
        };
        let kept_before = kept_row(&s);
        assert!(exact(&s) > kept_before.exact_secs);
        let preview = s.summary_preview(&this_month(&p)).unwrap();
        assert_eq!(preview.rows.len(), 2);
        let segments = s.file().data.segments.len();
        let pauses = s.file().data.pauses.len();
        let gaps = s.file().data.gaps.len();

        s.delete_task(&t).unwrap();

        assert_eq!(time_shows(&s, &t, "Doomed"), Vec::<&str>::new());
        assert_eq!(exact(&s), kept_before.exact_secs, "the project's figures");
        assert_eq!(
            s.snapshot().projects[0].figures.billable_secs,
            kept_before.billable_secs
        );
        assert!(s.snapshot().projects[0].has_tasks, "Kept is still there");
        assert_eq!(kept_row(&s), kept_before, "its neighbour doesn't move");
        let preview = s.summary_preview(&this_month(&p)).unwrap();
        assert_eq!(preview.rows.len(), 1);
        assert_eq!(preview.rows[0].task_id, kept);
        assert!(preview.pause_ids.is_empty());
        assert_eq!(preview.segment_ids.len(), 1);
        assert_eq!(preview.unconfirmed_secs, 0);
        let selected = crate::report::SelectedEntry {
            task_id: t.clone(),
            day: "2026-03-01".into(),
        };
        assert!(
            s.export_source()
                .narrowed(chrono_tz::UTC, &[selected])
                .is_err(),
            "an old selection has nothing to narrow to"
        );

        // Every record is still in the file, and it says why they count for nothing.
        let data = &s.file().data;
        assert_eq!(data.segments.len(), segments);
        assert_eq!(data.pauses.len(), pauses);
        assert_eq!(data.gaps.len(), gaps);
        assert!(data.tasks[&t].discarded.is_some());
        assert!(data.tasks[&t].deleted);

        // And it stays that way.
        drop(s);
        let s = open(dir.path(), &clock);
        assert_eq!(time_shows(&s, &t, "Doomed"), Vec::<&str>::new());
        assert!(s.file().data.tasks[&t].discarded.is_some());
    }

    #[test]
    fn deleting_a_task_is_recorded_in_the_audit_trail() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _, t, _) = doomed_world(dir.path(), &clock);
        s.delete_task(&t).unwrap();
        let entries = s.audit_entries(Some(&t));
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!((e.entity.as_str(), e.action.as_str()), ("task", "delete"));
        assert_eq!(e.by, s.file().device_id);
        assert_eq!(e.before["name"], "Doomed");
        assert_eq!(e.before["project"], "P");
        assert_eq!(e.before["state"], "stopped");
        assert_eq!(e.before["segments"], 3);
        assert_eq!(e.before["pauses"], 1);
        assert_eq!(e.after, Value::Null);
    }

    #[test]
    fn a_task_running_here_cannot_be_deleted_until_its_timer_stops() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        s.start_task(&t).unwrap();
        clock.advance(600);
        let refused = |s: &mut Service| {
            let e = s.delete_task(&t).unwrap_err();
            assert!(matches!(e, CoreError::Conflict(_)), "{e}");
            assert_eq!(e.code(), "conflict");
            assert_eq!(
                e.to_string(),
                "this task is running; pause or stop it before deleting it"
            );
            assert!(s.file().data.tasks[&t].discarded.is_none());
            assert_eq!(s.snapshot().tasks.len(), 1);
        };
        refused(&mut s);
        // Another device timing it as well changes nothing here.
        other.start_task(&t).unwrap();
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        refused(&mut s);

        s.pause_task(&t).unwrap();
        s.delete_task(&t).unwrap();
        assert!(s.snapshot().tasks.is_empty());
    }

    #[test]
    fn a_task_running_on_another_device_can_be_deleted_here_with_or_without_a_pause_request() {
        for asked in [false, true] {
            let here = tempfile::tempdir().unwrap();
            let there = tempfile::tempdir().unwrap();
            let clock = ManualClock::at(T0);
            let (mut owner, t) = running(here.path(), &clock);
            work(&mut owner, &clock, 10);
            let mut other = open(there.path(), &clock);
            other.absorb_remote(&owner.file().data.clone(), BTreeMap::new(), Vec::new());
            assert!(other.snapshot().tasks[0].running_elsewhere.is_some());
            if asked {
                other.request_remote_pause(&t).unwrap();
            }
            other.delete_task(&t).unwrap();
            assert_eq!(time_shows(&other, &t, "T"), Vec::<&str>::new());
            assert!(other.snapshot().warnings.is_empty());
            assert!(other.snapshot().pause_requests.is_empty());
            // Its owner closes that segment, and nobody else ever does.
            let open_there = other.file().data.segments.values().next().unwrap();
            assert_eq!(open_there.end, None);
            assert_eq!(open_there.device_id, owner.file().device_id);
            let audit = other.audit_entries(Some(&t));
            let delete = audit.iter().find(|a| a.action == "delete").unwrap();
            assert_eq!(delete.before["state"], "running");
        }
    }

    #[test]
    fn a_task_stopped_here_while_it_runs_elsewhere_can_be_deleted_here() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut owner, t) = running(here.path(), &clock);
        clock.advance(60);
        let mut other = open(there.path(), &clock);
        other.absorb_remote(&owner.file().data.clone(), BTreeMap::new(), Vec::new());
        other.stop_task(&t).unwrap();
        other.delete_task(&t).unwrap();
        assert!(other.snapshot().tasks.is_empty());
        owner.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        assert!(owner.snapshot().tasks.is_empty());
        let own = owner.file().device_id.clone();
        assert!(owner.file().data.own_open_segments(&own).is_empty());
    }

    #[test]
    fn time_on_a_summary_refuses_the_delete_even_with_a_timer_only_on_another_device() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut owner, t) = running(here.path(), &clock);
        work(&mut owner, &clock, 10);
        owner.pause_task(&t).unwrap();
        let p = owner.file().data.tasks[&t].project_id.clone();
        owner.create_summary(&this_month(&p)).unwrap();
        owner.resume_task(&t).unwrap();
        let mut other = open(there.path(), &clock);
        other.absorb_remote(&owner.file().data.clone(), BTreeMap::new(), Vec::new());
        assert_eq!(
            other.delete_task(&t).unwrap_err().to_string(),
            "this task's time is on billing summary A-001; void that summary first"
        );
    }

    #[test]
    fn a_device_still_timing_a_task_deleted_elsewhere_stops_at_its_next_sync_and_no_time_returns() {
        for asked in [false, true] {
            let owner_dir = tempfile::tempdir().unwrap();
            let deleter_dir = tempfile::tempdir().unwrap();
            let third_dir = tempfile::tempdir().unwrap();
            let clock = ManualClock::at(T0);
            let (mut owner, t) = running(owner_dir.path(), &clock);
            work(&mut owner, &clock, 10);
            let mut deleter = open(deleter_dir.path(), &clock);
            deleter.absorb_remote(&owner.file().data.clone(), BTreeMap::new(), Vec::new());
            if asked {
                deleter.request_remote_pause(&t).unwrap();
            }
            deleter.delete_task(&t).unwrap();

            // Nothing of that reaches the owner yet. It goes on working: a pause, a resume, and more time,
            // so a segment no request could have covered, and a timer still running at its next sync.
            work(&mut owner, &clock, 30);
            owner.pause_task(&t).unwrap();
            clock.advance(60);
            owner.resume_task(&t).unwrap();
            work(&mut owner, &clock, 30);
            let name = owner.snapshot().tasks[0].name.clone();
            let logged = owner.export_source().rows(chrono_tz::UTC)[0].row.exact_secs;
            assert!(logged > 3600, "{logged}");
            assert_eq!(owner.snapshot().tasks.len(), 1, "it hasn't heard");
            let before_it_heard = owner.file().data.clone();

            // That time reaches the device that deleted the task while it is still counting up.
            deleter.absorb_remote(&before_it_heard, BTreeMap::new(), Vec::new());
            assert_eq!(time_shows(&deleter, &t, &name), Vec::<&str>::new());
            assert!(deleter.snapshot().warnings.is_empty());

            // The owner's next sync.
            let by = deleter.file().device_id.clone();
            let seen = BTreeMap::from([(
                by,
                RemoteDevice {
                    name: "Office PC".into(),
                    last_heartbeat: None,
                    written_at: clock.now(),
                },
            )]);
            assert!(owner.absorb_remote(&deleter.file().data.clone(), seen, Vec::new()));
            let own = owner.file().device_id.clone();
            assert!(
                owner.file().data.own_open_segments(&own).is_empty(),
                "the timer has stopped"
            );
            assert!(!crate::tray::tray_state(owner.file(), clock.now(), chrono_tz::UTC).running);
            assert_eq!(owner.current_reminder(), None);
            let notices = owner.snapshot().notices;
            assert_eq!(notices.len(), 1, "{notices:?}");
            let text = &notices[0].message;
            assert!(text.contains(&format!("\"{name}\"")), "{text}");
            assert!(text.contains("Office PC"), "{text}");
            assert!(text.contains("stopped"), "{text}");
            assert_eq!(owner.take_pause_alerts(), std::slice::from_ref(text));
            assert_eq!(time_shows(&owner, &t, &name), Vec::<&str>::new());
            assert!(matches!(owner.resume_task(&t), Err(CoreError::NotFound(_))));

            // All devices meet, one of them new, which first hears from the owner's older copy alone.
            clock.advance(60);
            deleter.absorb_remote(&owner.file().data.clone(), BTreeMap::new(), Vec::new());
            let mut third = open(third_dir.path(), &clock);
            third.absorb_remote(&before_it_heard, BTreeMap::new(), Vec::new());
            assert_eq!(
                third.snapshot().tasks.len(),
                1,
                "as yet, it knows no better"
            );
            third.absorb_remote(&deleter.file().data.clone(), BTreeMap::new(), Vec::new());
            // The owner's older copy comes round again, from a slow cloud client, and changes nothing.
            owner.absorb_remote(&before_it_heard, BTreeMap::new(), Vec::new());
            deleter.absorb_remote(&before_it_heard, BTreeMap::new(), Vec::new());
            third.absorb_remote(&owner.file().data.clone(), BTreeMap::new(), Vec::new());
            for device in [&owner, &deleter, &third] {
                assert_eq!(time_shows(device, &t, &name), Vec::<&str>::new());
                assert!(device.snapshot().warnings.is_empty());
                assert!(!device.snapshot().projects[0].has_tasks);
                assert_eq!(device.file().data.tasks[&t], owner.file().data.tasks[&t]);
                assert_eq!(device.file().data.segments, owner.file().data.segments);
            }
            let held = &owner.file().data.segments;
            assert_eq!(held.len(), 2, "the raw records are all there");
            assert!(held.values().all(|s| s.end.is_some()), "and all closed");
            assert_eq!(owner.snapshot().notices.len(), 1, "told once");
            assert!(owner.take_pause_alerts().is_empty());
        }
    }

    #[test]
    fn a_timer_asked_to_pause_and_then_deleted_closes_at_the_request_and_tells_only_of_the_delete()
    {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut owner, t) = running(here.path(), &clock);
        work(&mut owner, &clock, 10);
        let mut other = open(there.path(), &clock);
        other.absorb_remote(&owner.file().data.clone(), BTreeMap::new(), Vec::new());
        other.request_remote_pause(&t).unwrap();
        let at = other.file().data.pause_requests.values().next().unwrap().at;
        other.delete_task(&t).unwrap();
        work(&mut owner, &clock, 10);
        assert!(owner.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new()));
        let seg = owner.file().data.segments.values().next().unwrap();
        assert_eq!(seg.end, Some(at));
        let notices = owner.snapshot().notices;
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].message.contains("was deleted"), "{notices:?}");
        assert_eq!(owner.take_pause_alerts().len(), 1);
        assert!(owner.snapshot().tasks.is_empty());
    }

    #[test]
    fn a_paused_task_or_a_draft_with_time_can_be_deleted_and_nothing_more_can_be_done_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t) = running(dir.path(), &clock);
        clock.advance(600);
        s.pause_task(&t).unwrap();
        s.delete_task(&t).unwrap();
        assert!(matches!(s.resume_task(&t), Err(CoreError::NotFound(_))));
        assert!(matches!(s.delete_task(&t), Err(CoreError::NotFound(_))));

        let p = s.file().data.tasks[&t].project_id.clone();
        let draft = s.create_task(&tinput("Hand-entered", &p)).unwrap();
        s.add_manual_entry(&ManualEntryInput {
            task_id: draft.clone(),
            start: clock.now().later(-60),
            end: clock.now(),
            reason: String::new(),
            subtask_id: None,
            allow_overlap: true,
        })
        .unwrap();
        assert!(matches!(
            s.delete_draft(&draft),
            Err(CoreError::Conflict(_))
        ));
        s.delete_task(&draft).unwrap();
        assert_eq!(s.file().data.tasks[&draft].state, TaskState::Draft);
        assert_eq!(time_shows(&s, &draft, "Hand-entered"), Vec::<&str>::new());
    }

    #[test]
    fn a_stale_edit_from_another_device_cannot_bring_a_deleted_task_back() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        s.start_task(&t).unwrap();
        clock.advance(3600);
        s.pause_task(&t).unwrap();
        other.absorb_remote(&s.file().data.clone(), BTreeMap::new(), Vec::new());

        s.delete_task(&t).unwrap();
        // Later than the delete by every clock, on a device that hasn't heard of it.
        clock.advance(60);
        other
            .patch_task(
                &t,
                &TaskPatch {
                    name: Some("Renamed elsewhere".into()),
                    description: Some(doc("Edited elsewhere")),
                    ..TaskPatch::default()
                },
                &TaskExpected::default(),
            )
            .unwrap();
        other.stop_task(&t).unwrap();
        assert_eq!(other.snapshot().tasks.len(), 1);

        assert!(s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new()));
        assert!(s.snapshot().tasks.is_empty());
        assert_eq!(s.file().data.tasks[&t].name, "Renamed elsewhere");
        assert!(other.absorb_remote(&s.file().data.clone(), BTreeMap::new(), Vec::new()));
        assert!(other.snapshot().tasks.is_empty());
        assert_eq!(
            time_shows(&other, &t, "Renamed elsewhere"),
            Vec::<&str>::new()
        );
        assert_eq!(s.file().data.tasks[&t], other.file().data.tasks[&t]);
        // No more edits from there either.
        assert!(matches!(
            other.patch_task(
                &t,
                &TaskPatch {
                    name: Some("Again".into()),
                    ..TaskPatch::default()
                },
                &TaskExpected::default(),
            ),
            Err(CoreError::NotFound(_))
        ));
    }

    #[test]
    fn time_that_arrives_after_the_delete_is_hidden() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        s.start_task(&t).unwrap();
        clock.advance(600);
        s.pause_task(&t).unwrap();
        other.absorb_remote(&s.file().data.clone(), BTreeMap::new(), Vec::new());
        // Another device logs an hour it hasn't told anyone of.
        clock.advance(3600);
        let end = clock.now();
        other
            .add_manual_entry(&ManualEntryInput {
                task_id: t.clone(),
                start: end.later(-3600),
                end,
                reason: String::new(),
                subtask_id: None,
                allow_overlap: true,
            })
            .unwrap();
        s.delete_task(&t).unwrap();
        let held = s.file().data.segments.len();
        assert!(s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new()));
        assert_eq!(s.file().data.segments.len(), held + 1, "it did arrive");
        assert_eq!(time_shows(&s, &t, "T"), Vec::<&str>::new());
    }

    #[test]
    fn a_device_timing_a_task_deleted_elsewhere_stops_says_so_and_the_time_stays_hidden() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut timing, t) = two_devices(here.path(), there.path(), &clock);
        let p = s.file().data.tasks[&t].project_id.clone();
        let next = s.create_task(&tinput("Next", &p)).unwrap();
        timing.absorb_remote(&s.file().data.clone(), BTreeMap::new(), Vec::new());
        timing.start_task(&t).unwrap();
        // Here, nothing is known of that yet, so nothing stops the delete.
        s.delete_task(&t).unwrap();
        s.start_task(&next).unwrap();
        clock.advance(600);

        // The segment that was running there reaches this device while still open.
        s.absorb_remote(&timing.file().data.clone(), BTreeMap::new(), Vec::new());
        assert!(
            s.file()
                .data
                .segments
                .values()
                .any(|seg| seg.task_id == t && seg.end.is_none()),
        );
        assert_eq!(time_shows(&s, &t, "T"), Vec::<&str>::new());
        assert!(
            s.snapshot().warnings.is_empty(),
            "only one task is running: {:?}",
            s.snapshot().warnings
        );

        let by = s.file().device_id.clone();
        let seen = BTreeMap::from([(
            by,
            RemoteDevice {
                name: "Office PC".into(),
                last_heartbeat: None,
                written_at: clock.now(),
            },
        )]);
        assert!(timing.absorb_remote(&s.file().data.clone(), seen, Vec::new()));
        let own = timing.file().device_id.clone();
        assert!(
            timing.file().data.own_open_segments(&own).is_empty(),
            "the timer has stopped"
        );
        assert_eq!(timing.file().data.tasks[&t].state, TaskState::Paused);
        assert_eq!(time_shows(&timing, &t, "T"), Vec::<&str>::new());
        let tray = crate::tray::tray_state(timing.file(), clock.now(), chrono_tz::UTC);
        assert!(!tray.running);
        let notices = timing.snapshot().notices;
        assert_eq!(notices.len(), 1, "{notices:?}");
        let text = &notices[0].message;
        assert!(text.contains("\"T\""), "{text}");
        assert!(text.contains("Office PC"), "{text}");
        assert!(text.contains("stopped"), "{text}");
        assert_eq!(timing.take_pause_alerts(), std::slice::from_ref(text));

        // The next cycles bring the same delete again: it has done its work.
        clock.advance(600);
        timing.absorb_remote(&s.file().data.clone(), BTreeMap::new(), Vec::new());
        assert_eq!(timing.snapshot().notices.len(), 1);
        assert!(timing.take_pause_alerts().is_empty());

        // Its closed copy reaches the device that deleted it, hidden as well.
        s.absorb_remote(&timing.file().data.clone(), BTreeMap::new(), Vec::new());
        assert_eq!(time_shows(&s, &t, "T"), Vec::<&str>::new());
        assert_eq!(s.file().data.tasks[&t], timing.file().data.tasks[&t]);
    }

    #[test]
    fn a_pause_asked_for_on_a_task_deleted_meanwhile_tells_only_of_the_delete() {
        let here = tempfile::tempdir().unwrap();
        let asker = tempfile::tempdir().unwrap();
        let deleter = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut owner, mut deleting, t) = two_devices(here.path(), deleter.path(), &clock);
        owner.start_task(&t).unwrap();
        let mut asking = open(asker.path(), &clock);
        asking.absorb_remote(&owner.file().data.clone(), BTreeMap::new(), Vec::new());
        asking.request_remote_pause(&t).unwrap();
        deleting.delete_task(&t).unwrap();
        clock.advance(60);
        let both = merge(&asking.file().data, &deleting.file().data);
        assert!(owner.absorb_remote(&both, BTreeMap::new(), Vec::new()));
        let notices = owner.snapshot().notices;
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].message.contains("was deleted"), "{notices:?}");
        assert_eq!(owner.take_pause_alerts().len(), 1);
        assert!(owner.snapshot().pause_requests.is_empty());
    }

    #[test]
    fn a_draft_deleted_elsewhere_but_started_here_keeps_its_time() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        other.delete_draft(&t).unwrap();
        s.start_task(&t).unwrap();
        work(&mut s, &clock, 60);
        s.stop_task(&t).unwrap();
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        other.absorb_remote(&s.file().data.clone(), BTreeMap::new(), Vec::new());
        for device in [&s, &other] {
            assert!(device.file().data.tasks[&t].deleted);
            assert!(device.file().data.tasks[&t].discarded.is_none());
            assert!(device.snapshot().tasks.iter().any(|x| x.id == t));
            let rows = device.export_source().rows(chrono_tz::UTC);
            assert_eq!(rows[0].row.exact_secs, 3600);
        }
    }

    #[test]
    fn time_on_a_live_summary_refuses_the_delete_and_a_voided_summary_stays_as_issued() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, p, t) = billed_world(dir.path(), &clock);
        let first = s.create_summary(&this_month(&p)).unwrap();
        let e = s.delete_task(&t).unwrap_err();
        assert!(matches!(e, CoreError::Conflict(_)), "{e}");
        assert_eq!(
            e.to_string(),
            "this task's time is on billing summary A-001; void that summary first"
        );
        assert!(s.file().data.tasks[&t].discarded.is_none());
        assert!(time_shows(&s, &t, "T").contains(&"report rows"));

        // A second summary, for time logged later: both are named, and both have to go.
        clock.advance(2 * 86_400);
        s.add_manual_entry(&ManualEntryInput {
            task_id: t.clone(),
            start: Ts::parse("2026-03-02T09:00:00Z").unwrap(),
            end: Ts::parse("2026-03-02T10:00:00Z").unwrap(),
            reason: String::new(),
            subtask_id: None,
            allow_overlap: true,
        })
        .unwrap();
        let second = s.create_summary(&this_month(&p)).unwrap();
        assert_eq!(
            s.delete_task(&t).unwrap_err().to_string(),
            "this task's time is on billing summaries A-001, A-002; void those summaries first"
        );

        s.void_summary(&first).unwrap();
        s.void_summary(&second).unwrap();
        let issued = s.file().data.summaries.clone();
        s.delete_task(&t).unwrap();
        assert_eq!(
            s.file().data.summaries,
            issued,
            "a voided summary keeps every number it had"
        );
        let shown = s.snapshot().billing.summaries;
        assert_eq!(shown.len(), 2);
        assert!(shown.iter().all(|b| b.rows[0].task_name == "T"));

        // Its number isn't reused.
        let later = s.create_task(&tinput("Later", &p)).unwrap();
        s.start_task(&later).unwrap();
        clock.advance(60);
        s.stop_task(&later).unwrap();
        let third = s.create_summary(&this_month(&p)).unwrap();
        assert_eq!(s.file().data.summaries[&third].reference, "A-003");
        assert_eq!(s.file().data.summaries[&third].rows.len(), 1);
    }

    #[test]
    fn a_summary_that_crosses_with_a_delete_keeps_its_numbers() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, p, t) = billed_world(here.path(), &clock);
        let mut other = open(there.path(), &clock);
        other.absorb_remote(&s.file().data.clone(), BTreeMap::new(), Vec::new());
        // Each device hears nothing of the other's step.
        let id = s.create_summary(&this_month(&p)).unwrap();
        other.delete_task(&t).unwrap();
        let issued = s.file().data.summaries[&id].clone();
        assert_eq!(issued.rows.len(), 1);

        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        other.absorb_remote(&s.file().data.clone(), BTreeMap::new(), Vec::new());
        for device in [&s, &other] {
            assert_eq!(device.file().data.summaries[&id], issued);
            assert_eq!(time_shows(device, &t, "T"), Vec::<&str>::new());
            // Nothing returns to unbilled, as there's no time left to bill.
            let preview = device.summary_preview(&this_month(&p)).unwrap();
            assert!(preview.rows.is_empty());
        }
        s.void_summary(&id).unwrap();
        assert!(s.summary_preview(&this_month(&p)).unwrap().rows.is_empty());
    }

    #[test]
    fn a_project_a_summary_names_can_only_be_archived_once_its_tasks_are_gone() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, p, t) = billed_world(dir.path(), &clock);
        let id = s.create_summary(&this_month(&p)).unwrap();
        s.void_summary(&id).unwrap();
        s.delete_task(&t).unwrap();
        let e = s.delete_project(&p).unwrap_err();
        assert!(matches!(e, CoreError::Conflict(_)), "{e}");
        assert_eq!(
            e.to_string(),
            "a project with billing summaries can only be archived"
        );
        let snap = s.snapshot();
        assert_eq!(snap.projects[0].id, p, "the summary can still name it");
        assert!(snap.projects[0].has_tasks, "so the page offers Archive");
        s.set_project_archived(&p, true).unwrap();
        assert!(s.snapshot().projects[0].archived);
    }

    #[test]
    fn a_project_deleted_while_a_summary_of_it_was_made_elsewhere_stays_visible_as_archived() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, p, t) = billed_world(here.path(), &clock);
        let mut other = open(there.path(), &clock);
        other.absorb_remote(&s.file().data.clone(), BTreeMap::new(), Vec::new());
        // Each device hears nothing of the other's steps.
        s.create_summary(&this_month(&p)).unwrap();
        other.delete_task(&t).unwrap();
        other.delete_project(&p).unwrap();
        s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new());
        other.absorb_remote(&s.file().data.clone(), BTreeMap::new(), Vec::new());
        for device in [&s, &other] {
            assert!(device.file().data.projects[&p].deleted);
            let snap = device.snapshot();
            assert_eq!(snap.billing.summaries.len(), 1);
            assert_eq!(snap.projects.len(), 1, "the summary can still name it");
            assert_eq!(snap.projects[0].name, "P");
            assert!(snap.projects[0].archived);
        }
    }

    #[test]
    fn time_on_a_deleted_task_overlaps_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, p, t) = billed_world(dir.path(), &clock);
        let other = s.create_task(&tinput("Other", &p)).unwrap();
        let input = ManualEntryInput {
            task_id: other,
            start: Ts::parse(T0).unwrap().later(60),
            end: Ts::parse(T0).unwrap().later(120),
            reason: String::new(),
            subtask_id: None,
            allow_overlap: false,
        };
        assert!(matches!(
            s.add_manual_entry(&input),
            Err(CoreError::Overlap(_))
        ));
        s.delete_task(&t).unwrap();
        s.add_manual_entry(&input).unwrap();
    }

    #[test]
    fn continuations_stay_as_their_own_tasks_and_lose_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, source) = running(dir.path(), &clock);
        clock.advance(600);
        s.pause_task(&source).unwrap();
        let p = s.file().data.tasks[&source].project_id.clone();
        let next = s
            .create_task(&TaskInput {
                continued_from: Some(source.clone()),
                ..tinput("Next", &p)
            })
            .unwrap();
        s.start_task(&next).unwrap();
        work(&mut s, &clock, 10);
        s.pause_task(&next).unwrap();
        let link = |s: &Service| {
            let snap = s.snapshot();
            snap.tasks
                .iter()
                .find(|x| x.id == next)
                .unwrap()
                .continued_from
                .clone()
        };
        assert_eq!(link(&s), Some(source.clone()));

        s.delete_task(&source).unwrap();
        assert_eq!(link(&s), None, "no \"an earlier task\" left behind");
        let rows = s.export_source().rows(chrono_tz::UTC);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row.exact_secs, 600);
        // The record keeps it: it is set once, at creation, and a merge goes by that.
        assert_eq!(
            s.file().data.tasks[&next].continued_from,
            Some(source.clone())
        );
        assert!(matches!(
            s.create_task(&TaskInput {
                continued_from: Some(source),
                ..tinput("Too late", &p)
            }),
            Err(CoreError::NotFound(_))
        ));
    }

    #[test]
    fn a_warm_copy_goes_back_to_the_task_worked_before_a_deleted_one() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, first) = running(dir.path(), &clock);
        clock.advance(600);
        s.pause_task(&first).unwrap();
        let p = s.file().data.tasks[&first].project_id.clone();
        let last = s.create_task(&tinput("Last", &p)).unwrap();
        s.start_task(&last).unwrap();
        clock.advance(600);
        s.stop_task(&last).unwrap();
        let copy = |s: &Service| s.snapshot().launch.copy_source.map(|c| c.task_id);
        assert_eq!(copy(&s), Some(last.clone()));
        s.delete_task(&last).unwrap();
        assert_eq!(copy(&s), Some(first.clone()));
        s.delete_task(&first).unwrap();
        assert_eq!(copy(&s), None);
        assert!(s.snapshot().launch.warm, "the launch stays warm");
    }

    #[test]
    fn the_tray_stops_offering_a_deleted_task_and_a_project_can_go_once_its_tasks_have() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t) = running(dir.path(), &clock);
        clock.advance(600);
        s.pause_task(&t).unwrap();
        let p = s.file().data.tasks[&t].project_id.clone();
        let tray = |s: &Service| crate::tray::tray_state(s.file(), clock.now(), chrono_tz::UTC);
        assert_eq!(tray(&s).task_id, Some(t.clone()));
        assert!(s.delete_project(&p).is_err());
        s.delete_task(&t).unwrap();
        assert_eq!(tray(&s).task_id, None);
        assert_eq!(tray(&s).status_line, "No task running");
        assert!(!s.snapshot().projects[0].has_tasks);
        s.delete_project(&p).unwrap();
        assert!(s.snapshot().projects.is_empty());
    }

    #[test]
    fn a_backup_from_before_the_delete_brings_the_task_back_until_a_sync_says_otherwise() {
        let here = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _, t) = billed_world(here.path(), &clock);
        s.run_daily_backup().unwrap();
        let name = s.list_backups().unwrap()[0].name.clone();
        clock.advance(60);
        s.delete_task(&t).unwrap();
        let deleted = s.file().data.clone();
        s.restore_backup(&name).unwrap();
        assert_eq!(s.export_source().rows(chrono_tz::UTC).len(), 1);
        s.absorb_remote(&deleted, BTreeMap::new(), Vec::new());
        assert!(s.export_source().rows(chrono_tz::UTC).is_empty());
    }

    /// A checklist under the limit as typed, and over it once each item has its id.
    fn list_that_grows_past_the_limit() -> Value {
        let item = serde_json::json!({"type":"taskItem","attrs":{"checked":false},"content":[{"type":"paragraph","content":[{"type":"text","text":"x"}]}]});
        let count = (500 * 1024) / item.to_string().len();
        let list = serde_json::json!({"type":"doc","content":[{"type":"taskList","content": vec![item; count]}]});
        crate::validate::check_description(&list).unwrap();
        assert!(
            crate::validate::check_description(&crate::subtasks::assign_ids(&list, None)).is_err()
        );
        list
    }

    #[test]
    fn r3_a_draft_that_grows_past_the_limit_lets_the_app_open_and_keeps_the_words() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        let stored = s.file().data.tasks[&t].description.clone();
        // Left on disk by a version that only checked what was typed.
        let mut drafts = BTreeMap::new();
        drafts.insert(
            t.clone(),
            Draft {
                description: list_that_grows_past_the_limit(),
                at: clock.now(),
                base: stored.clone(),
            },
        );
        crate::drafts::write(dir.path(), &drafts).unwrap();
        drop(s);
        clock.advance(60);
        let s = Service::open(dir.path(), Arc::new(clock.clone()), "pc");
        let s = s.expect("the app opens");
        assert_eq!(s.file().data.tasks[&t].description, stored);
        let kept = |s: &Service| {
            s.snapshot()
                .notices
                .into_iter()
                .filter(|n| n.message.contains("too long") && n.message.contains("x\nx"))
                .count()
        };
        assert_eq!(kept(&s), 1, "{:?}", s.snapshot().notices.len());
        assert!(crate::drafts::read(dir.path()).unwrap().unwrap().is_empty());
        drop(s);
        // And at every launch after, with the words still shown until they are dismissed.
        clock.advance(60);
        let s = Service::open(dir.path(), Arc::new(clock), "pc").expect("opens again");
        assert_eq!(kept(&s), 1);
    }

    #[test]
    fn r3_a_stash_ids_would_take_past_the_limit_is_refused_when_it_is_stashed() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        let stored = s.file().data.tasks[&t].description.clone();
        let e = s
            .stash_draft(&t, &list_that_grows_past_the_limit(), &stored)
            .expect_err("the page hears at once");
        assert!(e.to_string().contains("too large"), "{e}");
        assert!(crate::drafts::read(dir.path()).unwrap().unwrap().is_empty());
    }

    #[test]
    fn r3_a_stored_list_ids_would_take_past_the_limit_lets_the_app_open() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        // A list saved before items had ids, or by a device that didn't add them.
        let list = list_that_grows_past_the_limit();
        s.mutate(|f, _| {
            f.data.tasks.get_mut(&t).unwrap().description = list.clone();
            Ok(())
        })
        .unwrap();
        drop(s);
        let s = Service::open(dir.path(), Arc::new(clock), "pc").expect("the app opens");
        assert_eq!(s.file().data.tasks[&t].description, list, "left as it was");
    }

    #[test]
    fn r11_a_zone_change_reaches_the_view_and_the_day_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at("2026-03-03T09:00:00Z");
        let mut s = open(dir.path(), &clock);
        s.set_system_zone("UTC".to_owned());
        let p = s
            .create_project(&ProjectInput {
                budget_hours: Some(100.0),
                ..pinput("P")
            })
            .unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        s.set_rounding(
            &RoundingInput {
                project_id: None,
                effective_from: "2026-01-01".into(),
                increment_minutes: 15,
                method: crate::model::RoundMethod::Up,
                confirm_retroactive: true,
            },
            "2026-03-03",
        )
        .unwrap();
        // Either side of midnight in UTC, and the middle of one morning in Tokyo.
        for (a, b) in [
            ("2026-03-01T23:50:00Z", "2026-03-01T23:55:00Z"),
            ("2026-03-02T00:05:00Z", "2026-03-02T00:10:00Z"),
        ] {
            s.add_manual_entry(&ManualEntryInput {
                task_id: t.clone(),
                start: Ts::parse(a).unwrap(),
                end: Ts::parse(b).unwrap(),
                reason: String::new(),
                subtask_id: None,
                allow_overlap: true,
            })
            .unwrap();
        }
        let billable = |s: &Service| s.snapshot().projects[0].figures.billable_secs;
        assert_eq!(billable(&s), 30 * 60, "a day each, each rounded up");
        let revision = s.revision();

        assert!(s.observe_system_zone(Some("Asia/Tokyo".to_owned())));
        assert_eq!(s.snapshot().system_zone, "Asia/Tokyo");
        assert_eq!(s.viewing_zone(), chrono_tz::Asia::Tokyo);
        assert_eq!(billable(&s), 15 * 60, "one day now, rounded once");
        assert!(s.revision() > revision, "no older reply can undo it");

        // Nothing new the next time round, and a failed read or an unknown name changes nothing.
        let revision = s.revision();
        assert!(!s.observe_system_zone(Some("Asia/Tokyo".to_owned())));
        assert!(!s.observe_system_zone(None));
        assert!(!s.observe_system_zone(Some("Mars/Olympus".to_owned())));
        assert_eq!(s.snapshot().system_zone, "Asia/Tokyo");
        assert_eq!(s.revision(), revision);
    }

    #[test]
    fn r12_opening_waits_while_something_holds_the_store_and_only_then() {
        use std::io::{Error, ErrorKind};
        use std::time::Duration;
        let held = || CoreError::Read(Error::from(ErrorKind::PermissionDenied));
        let mut tries = 0;
        let mut waits = 0;
        let got = retry_while_held(
            || {
                tries += 1;
                if tries < 3 { Err(held()) } else { Ok(tries) }
            },
            |_| waits += 1,
        );
        assert_eq!(got.unwrap(), 3, "free by the third try");
        assert_eq!(waits, 2);

        let mut waited = Duration::ZERO;
        let got: Result<(), _> = retry_while_held(|| Err(held()), |d| waited += d);
        assert!(matches!(got, Err(CoreError::Read(_))), "it gives up");
        assert!(
            waited >= Duration::from_secs(2) && waited <= Duration::from_secs(10),
            "{waited:?}"
        );

        // What waiting can't help opens, or fails, at once.
        let fails: [fn() -> CoreError; 3] = [
            || CoreError::Read(Error::from(ErrorKind::IsADirectory)),
            || CoreError::Io(Error::from(ErrorKind::StorageFull)),
            || CoreError::Validation("no".into()),
        ];
        for fail in fails {
            let mut tries = 0;
            let got: Result<(), _> = retry_while_held(
                || {
                    tries += 1;
                    Err(fail())
                },
                |_| panic!("no wait"),
            );
            assert!(got.is_err());
            assert_eq!(tries, 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn r12_a_store_held_at_the_first_save_opens_once_it_is_free() {
        use std::os::unix::fs::PermissionsExt;
        let clock = ManualClock::at("2026-03-01T09:00:00Z");
        let dir = tempfile::tempdir().unwrap();
        let t = {
            let mut s = Service::open(dir.path(), Arc::new(clock.clone()), "pc").unwrap();
            let p = s.create_project(&pinput("P")).unwrap();
            let t = s.create_task(&tinput("T", &p)).unwrap();
            s.start_task(&t).unwrap();
            t
        };
        // No clean quit, so this launch has a timer to close and the store to write.
        clock.advance(30);
        let holding = |mode| {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        };
        holding(0o500);
        let got = retry_while_held(
            || Service::open(dir.path(), Arc::new(clock.clone()), "pc"),
            |_| holding(0o700),
        );
        holding(0o700);
        let s = got.unwrap();
        assert_eq!(s.file().data.tasks[&t].state, TaskState::Paused);
        assert_eq!(
            s.file().data.own_open_segments(&s.file().device_id).len(),
            0
        );
    }

    #[test]
    fn r12_a_backup_that_cannot_be_read_does_not_stop_the_app_opening() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("Kept")).unwrap();
        drop(s);
        // Something at the old backup's path that can't be read as a file, as a locked one can't, and an old store that
        // can't be read either: neither is looked at while the database is fine.
        std::fs::create_dir(dir.path().join("z-work-data.json.bak")).unwrap();
        std::fs::write(dir.path().join("z-work-data.json"), b"junk").unwrap();
        let s = Service::open(dir.path(), Arc::new(clock), "pc");
        let s = s.expect("the database is fine");
        assert!(s.file().data.projects.contains_key(&p));
    }

    #[test]
    fn r14_raising_a_budget_after_an_alert_lets_it_alert_again() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at("2026-03-03T09:00:00Z");
        let mut s = open(dir.path(), &clock);
        let p = s
            .create_project(&ProjectInput {
                budget_hours: Some(1.0),
                ..pinput("P")
            })
            .unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        let entry = |t: &str, a: &str, b: &str| ManualEntryInput {
            task_id: t.to_owned(),
            start: Ts::parse(a).unwrap(),
            end: Ts::parse(b).unwrap(),
            reason: String::new(),
            subtask_id: None,
            allow_overlap: true,
        };
        s.add_manual_entry(&entry(&t, "2026-03-01T09:00:00Z", "2026-03-01T10:00:00Z"))
            .unwrap();
        assert_eq!(s.take_alerts().len(), 1);
        assert!(s.take_alerts().is_empty(), "each level alerts once");
        s.update_project(
            &p,
            &ProjectInput {
                budget_hours: Some(10.0),
                ..pinput("P")
            },
        )
        .unwrap();
        assert!(s.take_alerts().is_empty(), "the level fell: nothing to say");
        // 10.5 hours: past all of the new budget, a level it had reached under the old one.
        s.add_manual_entry(&entry(&t, "2026-03-02T08:00:00Z", "2026-03-02T17:30:00Z"))
            .unwrap();
        let alerts = s.take_alerts();
        assert_eq!(alerts.len(), 1, "{alerts:?}");
        assert!(alerts[0].contains("105%"), "{alerts:?}");
        assert!(s.take_alerts().is_empty());
    }

    #[test]
    fn r14_taking_a_budget_away_and_setting_one_again_can_alert_again() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at("2026-03-03T09:00:00Z");
        let mut s = open(dir.path(), &clock);
        let budget = |hours: Option<f64>| ProjectInput {
            budget_hours: hours,
            ..pinput("P")
        };
        let p = s.create_project(&budget(Some(1.0))).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        s.add_manual_entry(&ManualEntryInput {
            task_id: t,
            start: Ts::parse("2026-03-01T09:00:00Z").unwrap(),
            end: Ts::parse("2026-03-01T10:00:00Z").unwrap(),
            reason: String::new(),
            subtask_id: None,
            allow_overlap: true,
        })
        .unwrap();
        assert_eq!(s.take_alerts().len(), 1);
        s.update_project(&p, &budget(None)).unwrap();
        assert!(s.take_alerts().is_empty());
        s.update_project(&p, &budget(Some(1.0))).unwrap();
        assert_eq!(s.take_alerts().len(), 1);
    }

    /// The times of this task's segments, earliest start first.
    fn stretches(s: &Service, task: &str) -> Vec<(String, Option<String>)> {
        let mut all: Vec<_> = s
            .file()
            .data
            .segments
            .values()
            .filter(|x| x.task_id == task)
            .map(|x| (x.start, x.end))
            .collect();
        all.sort();
        all.into_iter()
            .map(|(a, b)| (a.iso(), b.map(|e| e.iso())))
            .collect()
    }
    fn unbilled(s: &Service, task: &str) -> i64 {
        s.snapshot()
            .tasks
            .iter()
            .find(|x| x.id == task)
            .unwrap()
            .unbilled_secs
    }
    /// Time passes with someone working, one tick a minute.
    fn work_for(s: &mut Service, clock: &ManualClock, minutes: i64) {
        for _ in 0..minutes {
            clock.advance(60);
            s.observe_activity(Some(0)).unwrap();
        }
    }
    fn away_times(s: &Service) -> Vec<(String, String)> {
        let mut all: Vec<_> = s
            .snapshot()
            .gaps
            .iter()
            .map(|g| (g.start.iso(), g.end.iso()))
            .collect();
        all.sort();
        all
    }
    fn iso(text: &str) -> String {
        text.to_owned()
    }

    #[test]
    fn r4_a_clock_put_back_during_a_run_keeps_the_work_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at("2026-03-02T10:00:00Z");
        let (mut s, t) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        work_for(&mut s, &clock, 5);
        // The clock was an hour ahead when the task started, and now it is corrected.
        clock.set("2026-03-02T09:10:00Z");
        s.observe_activity(Some(0)).unwrap();
        assert_eq!(
            s.file().data.tasks[&t].state,
            TaskState::Running,
            "the timer keeps going"
        );
        work_for(&mut s, &clock, 40);
        s.pause_task(&t).unwrap();
        assert_eq!(
            stretches(&s, &t),
            vec![
                (
                    iso("2026-03-02T09:10:00Z"),
                    Some(iso("2026-03-02T09:50:00Z"))
                ),
                (
                    iso("2026-03-02T10:00:00Z"),
                    Some(iso("2026-03-02T10:05:00Z"))
                ),
            ]
        );
        assert_eq!(unbilled(&s, &t), 45 * 60);
        let snap = s.snapshot();
        assert!(snap.gaps.is_empty(), "no time away: {:?}", snap.gaps);
        assert_eq!(snap.notices.len(), 1, "{:?}", snap.notices);
        assert!(snap.notices[0].message.contains("clock went back"));
        assert!(snap.notices[0].message.contains("\"T\""));
        assert!(
            s.take_alerts()
                .iter()
                .any(|a| a.contains("clock went back")),
            "a window that is hidden hears of it too"
        );
        assert!(
            !s.take_alerts()
                .iter()
                .any(|a| a.contains("clock went back")),
            "once"
        );
        assert!(
            s.file()
                .data
                .audit_log
                .values()
                .any(|a| a.action == "clockPutBack"),
            "on the record of changes"
        );
        // A new stretch starts at the time on the clock, whatever the old one ended at.
        clock.advance(100);
        s.resume_task(&t).unwrap();
        clock.advance(100);
        s.pause_task(&t).unwrap();
        assert_eq!(unbilled(&s, &t), 45 * 60 + 100);
    }

    #[test]
    fn r4_a_clock_put_back_just_before_a_pause_keeps_the_work_too() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at("2026-03-02T10:00:00Z");
        let (mut s, t) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        work_for(&mut s, &clock, 10);
        clock.set("2026-03-02T09:10:00Z");
        s.pause_task(&t).unwrap();
        assert_eq!(unbilled(&s, &t), 600);
        assert_eq!(s.snapshot().notices.len(), 1);
    }

    #[test]
    fn r4_time_away_under_way_when_the_clock_goes_back_stays_with_what_ran_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at("2026-03-02T10:00:00Z");
        let (mut s, t) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        work_for(&mut s, &clock, 5);
        let mut idle = 0;
        for _ in 0..15 {
            clock.advance(60);
            idle += 60;
            s.observe_activity(Some(idle)).unwrap();
        }
        clock.set("2026-03-02T09:10:00Z");
        idle += 5;
        assert!(
            s.observe_activity(Some(idle)).unwrap(),
            "time away was found"
        );
        assert_eq!(
            away_times(&s),
            vec![(iso("2026-03-02T10:05:00Z"), iso("2026-03-02T10:20:00Z"))]
        );
        // Still away on the new clock: that is time away from the new stretch, from its start.
        for _ in 0..20 {
            clock.advance(60);
            idle += 60;
            s.observe_activity(Some(idle)).unwrap();
        }
        clock.advance(5);
        s.observe_activity(Some(1)).unwrap();
        assert_eq!(
            away_times(&s),
            vec![
                (iso("2026-03-02T09:10:00Z"), iso("2026-03-02T09:30:04Z")),
                (iso("2026-03-02T10:05:00Z"), iso("2026-03-02T10:20:00Z")),
            ]
        );
        s.pause_task(&t).unwrap();
        // Five minutes before the first absence and the one second after the second.
        assert_eq!(unbilled(&s, &t), 5 * 60 + 1);
    }

    #[test]
    fn r4_a_lock_under_way_when_the_clock_goes_back_is_an_absence_on_each_clock() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at("2026-03-02T10:00:00Z");
        let (mut s, t) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        work_for(&mut s, &clock, 5);
        let locked_for = |s: &mut Service, minutes: i64| {
            for _ in 0..minutes {
                clock.advance(60);
                // The lock is looked at first, as the app does on every tick.
                s.observe_lock(Some(true)).unwrap();
                s.observe_activity(Some(0)).unwrap();
            }
        };
        s.observe_lock(Some(true)).unwrap();
        locked_for(&mut s, 5);
        clock.set("2026-03-02T09:10:00Z");
        s.observe_lock(Some(true)).unwrap();
        s.observe_activity(Some(0)).unwrap();
        locked_for(&mut s, 30);
        s.observe_lock(Some(false)).unwrap();
        assert_eq!(
            away_times(&s),
            vec![
                (iso("2026-03-02T09:10:00Z"), iso("2026-03-02T09:40:00Z")),
                (iso("2026-03-02T10:05:00Z"), iso("2026-03-02T10:10:00Z")),
            ]
        );
        work_for(&mut s, &clock, 10);
        s.pause_task(&t).unwrap();
        assert_eq!(unbilled(&s, &t), 15 * 60);
    }

    #[test]
    fn r4_a_lock_taken_just_after_the_clock_went_back_is_on_the_new_clock() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at("2026-03-02T10:00:00Z");
        let (mut s, t) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        work_for(&mut s, &clock, 5);
        clock.set("2026-03-02T09:10:00Z");
        s.observe_lock(Some(true)).unwrap();
        s.observe_activity(Some(0)).unwrap();
        clock.advance(60);
        s.observe_lock(Some(false)).unwrap();
        assert_eq!(
            away_times(&s),
            vec![(iso("2026-03-02T09:10:00Z"), iso("2026-03-02T09:11:00Z"))]
        );
        s.pause_task(&t).unwrap();
        assert_eq!(unbilled(&s, &t), 5 * 60);
    }

    #[test]
    fn r4_a_crash_soon_after_the_clock_went_back_ends_the_new_stretch_at_its_own_heartbeat() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at("2026-03-02T10:00:00Z");
        let (mut s, t) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        work_for(&mut s, &clock, 60);
        s.heartbeat().unwrap();
        // 11:00 on a clock an hour ahead, and now corrected.
        clock.set("2026-03-02T10:00:00Z");
        s.observe_activity(Some(0)).unwrap();
        work_for(&mut s, &clock, 5);
        s.heartbeat().unwrap();
        drop(s);
        clock.set("2026-03-02T15:00:00Z");
        let s = open(dir.path(), &clock);
        assert_eq!(
            stretches(&s, &t),
            vec![
                (
                    iso("2026-03-02T10:00:00Z"),
                    Some(iso("2026-03-02T10:05:00Z"))
                ),
                (
                    iso("2026-03-02T10:00:00Z"),
                    Some(iso("2026-03-02T11:00:00Z"))
                ),
            ],
            "the hour on the old clock, and the five minutes on the new"
        );
        assert_eq!(unbilled(&s, &t), 65 * 60);
    }

    #[test]
    fn r4_a_crash_before_the_next_beat_after_the_clock_went_back_adds_nothing_to_the_new_stretch() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at("2026-03-02T10:00:00Z");
        let (mut s, t) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        work_for(&mut s, &clock, 60);
        s.heartbeat().unwrap();
        clock.set("2026-03-02T10:00:00Z");
        s.observe_activity(Some(0)).unwrap();
        drop(s);
        clock.set("2026-03-02T15:00:00Z");
        let s = open(dir.path(), &clock);
        assert_eq!(unbilled(&s, &t), 3600, "{:?}", stretches(&s, &t));
    }

    #[test]
    fn r4_a_crash_then_a_boot_on_a_corrected_clock_keeps_a_run_begun_ahead_of_it() {
        let run = |boot: &str| {
            let dir = tempfile::tempdir().unwrap();
            let clock = ManualClock::at("2026-03-02T10:00:00Z");
            let (mut s, t) = running(dir.path(), &clock);
            s.observe_activity(Some(0)).unwrap();
            work_for(&mut s, &clock, 60);
            s.heartbeat().unwrap();
            drop(s);
            clock.set(boot);
            let s = open(dir.path(), &clock);
            (unbilled(&s, &t), s.snapshot().notices[0].message.clone())
        };
        // The clock now reads before the run began, so its heartbeat is the only time on its own clock.
        let (secs, told) = run("2026-03-02T09:30:00Z");
        assert_eq!(secs, 3600);
        assert!(told.contains("2026-03-02T11:00:00Z"), "{told}");
        // On a clock that wasn't changed, nothing is different.
        assert_eq!(run("2026-03-02T11:30:00Z").0, 3600);
        // Inside the run the clock can't say which it was, so nothing past it is counted, as before.
        assert_eq!(run("2026-03-02T10:30:00Z").0, 1800);
    }

    #[test]
    fn r4_a_small_correction_or_a_clock_put_back_with_nothing_running_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at("2026-03-02T10:00:00Z");
        let (mut s, t) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        work_for(&mut s, &clock, 10);
        clock.advance(-crate::idle::JUMP_SECS);
        s.observe_activity(Some(0)).unwrap();
        assert_eq!(stretches(&s, &t).len(), 1, "a few seconds isn't a new run");
        assert!(s.snapshot().notices.is_empty());
        s.pause_task(&t).unwrap();
        s.observe_activity(Some(0)).unwrap();
        let before = stretches(&s, &t);
        clock.advance(-3600);
        s.observe_activity(Some(0)).unwrap();
        assert_eq!(stretches(&s, &t), before);
        assert!(s.snapshot().notices.is_empty());
        assert!(s.take_alerts().is_empty());
    }

    #[test]
    fn r4_a_clock_put_back_is_kept_for_the_next_save_when_a_save_fails() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at("2026-03-02T10:00:00Z");
        let (mut s, t) = running(dir.path(), &clock);
        s.observe_activity(Some(0)).unwrap();
        work_for(&mut s, &clock, 10);
        clock.set("2026-03-02T09:10:00Z");
        // The save fails inside its transaction, as on a full disk.
        s.set_saves_blocked(true);
        assert!(s.observe_activity(Some(0)).is_err());
        assert_eq!(stretches(&s, &t).len(), 1, "memory stays as it was");
        assert!(s.snapshot().notices.is_empty());
        s.set_saves_blocked(false);
        clock.advance(60);
        s.observe_activity(Some(0)).unwrap();
        assert_eq!(
            stretches(&s, &t),
            vec![
                (iso("2026-03-02T09:10:00Z"), None),
                (
                    iso("2026-03-02T10:00:00Z"),
                    Some(iso("2026-03-02T10:10:00Z"))
                ),
            ],
            "the new stretch starts when the clock was found changed"
        );
        assert_eq!(s.snapshot().notices.len(), 1);
    }

    /// A device with a folder, as `run_cycle` sees it.
    fn on_folder(dir: &Path, root: &Path, clock: &ManualClock) -> std::sync::Mutex<Service> {
        let mut s = open(dir, clock);
        s.add_sync_target(SyncTarget {
            provider: SyncProvider::Custom,
            root: root.to_string_lossy().into_owned(),
        })
        .unwrap();
        std::sync::Mutex::new(s)
    }

    /// A laptop that started a timer, was last heard of an hour in and then died. Returns the desktop on the same
    /// folder, with the project, the task and the segment left open.
    fn laptop_lost(
        dirs: [&Path; 3],
        clock: &ManualClock,
    ) -> (std::sync::Mutex<Service>, String, String, String) {
        use crate::sync::{RealIo, run_cycle};
        let [here, there, root] = dirs;
        let laptop = on_folder(here, root, clock);
        let desktop = on_folder(there, root, clock);
        let (p, t, seg) = {
            let mut s = laptop.lock().unwrap();
            let p = s.create_project(&pinput("P")).unwrap();
            let t = s.create_task(&tinput("T", &p)).unwrap();
            s.start_task(&t).unwrap();
            let seg = s.file().data.segments.keys().next().unwrap().clone();
            (p, t, seg)
        };
        run_cycle(&laptop, &RealIo);
        run_cycle(&desktop, &RealIo);
        clock.advance(3600);
        laptop.lock().unwrap().heartbeat().unwrap();
        run_cycle(&laptop, &RealIo);
        drop(laptop);
        run_cycle(&desktop, &RealIo);
        (desktop, p, t, seg)
    }

    #[test]
    fn r5_a_segment_left_open_by_a_lost_device_can_be_billed_after_settling_and_correcting() {
        let dirs = [(); 3].map(|()| tempfile::tempdir().unwrap());
        let clock = ManualClock::at("2026-03-02T09:00:00Z");
        let (desktop, p, t, seg) = laptop_lost(dirs.each_ref().map(|d| d.path()), &clock);
        clock.advance(86_400 * 30);
        let mut s = desktop.lock().unwrap();
        let fix = CorrectionInput {
            segment_id: seg.clone(),
            start: Ts::parse("2026-03-02T09:00:00Z").unwrap(),
            end: Ts::parse("2026-03-02T09:45:00Z").unwrap(),
            reason: "left at a quarter to".into(),
            allow_overlap: false,
        };
        let req = this_month(&p);
        assert!(s.summary_preview(&req).unwrap().running_excluded);
        assert!(s.pause_task(&t).is_err());
        assert!(s.correct_segment(&fix).is_err(), "still open, so not yet");
        assert!(s.void_segment(&seg, "gone").is_err());

        s.settle_lost_segment(&seg, "the laptop is gone").unwrap();
        assert_eq!(s.file().data.segments[&seg].end, None, "raw stays");
        assert_eq!(
            s.file().data.segments[&seg].effective(),
            Some((
                Ts::parse("2026-03-02T09:00:00Z").unwrap(),
                Some(Ts::parse("2026-03-02T10:00:00Z").unwrap())
            )),
            "closed at the last moment it was heard of"
        );
        assert_eq!(s.file().data.tasks[&t].state, TaskState::Paused);
        assert!(!s.summary_preview(&req).unwrap().running_excluded);
        assert_eq!(s.summary_preview(&req).unwrap().billable_secs, 3600);

        s.correct_segment(&fix).unwrap();
        assert_eq!(s.summary_preview(&req).unwrap().billable_secs, 2700);
        let id = s.create_summary(&req).unwrap();
        let summary = &s.file().data.summaries[&id];
        assert_eq!(summary.segment_ids, vec![seg.clone()]);
        assert_eq!(summary.rows[0].billable_secs, 2700);
        assert!(s.void_segment(&seg, "gone").is_err(), "it is billed now");
    }

    #[test]
    fn r5_a_pause_asked_for_is_enough_to_close_it_with_no_month_of_silence() {
        let dirs = [(); 3].map(|()| tempfile::tempdir().unwrap());
        let clock = ManualClock::at("2026-03-02T09:00:00Z");
        let (desktop, p, t, seg) = laptop_lost(dirs.each_ref().map(|d| d.path()), &clock);
        let mut s = desktop.lock().unwrap();
        // The steps the review found led nowhere: stop leaves the segment, asking changes nothing about it.
        s.stop_task(&t).unwrap();
        s.request_remote_pause(&t).unwrap();
        clock.advance(600);
        s.settle_lost_segment(&seg, "the laptop is gone").unwrap();
        let preview = s.summary_preview(&this_month(&p)).unwrap();
        assert!(!preview.running_excluded);
        assert_eq!(preview.segment_ids, vec![seg.clone()]);
        assert_eq!(preview.billable_secs, 3600);
        s.void_segment(&seg, "gone").unwrap();
        assert_eq!(s.summary_preview(&this_month(&p)).unwrap().billable_secs, 0);
    }

    /// As another computer holding the lease leaves this one.
    fn held_by_laptop(s: &mut Service) {
        s.lease.state = LeaseStateName::HeldByOtherLive;
        s.lease.want_claim = false;
        s.lease.last_change_at = None;
        s.lease.holder = Some(crate::lease::HolderInfo {
            device_id: "laptop-id".to_owned(),
            device_name: "LAPTOP".to_owned(),
            generation: 4,
            beat_seq: 9,
            beat_at: s.now(),
            last_sign_at: s.now(),
            clock_suspect: false,
            open_segments: Vec::new(),
        });
    }

    fn custom_folder(root: &str) -> SyncTarget {
        SyncTarget {
            provider: SyncProvider::Custom,
            root: root.to_owned(),
        }
    }

    fn read_only_error<T: std::fmt::Debug>(r: Result<T, CoreError>) -> String {
        match r {
            Err(e @ CoreError::ReadOnly { .. }) => e.to_string(),
            other => panic!("not refused as read-only: {other:?}"),
        }
    }

    #[test]
    fn read_only_refuses_changes_to_shared_data_and_allows_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.add_sync_target(custom_folder("/not/there")).unwrap();
        let p = s.create_project(&pinput("P")).unwrap();
        let t = s.create_task(&tinput("T", &p)).unwrap();
        s.run_daily_backup().unwrap();
        let backup = s.list_backups().unwrap()[0].name.clone();
        held_by_laptop(&mut s);
        let before = s.store.total_changes();
        let (data, revision) = (s.file().data.clone(), s.revision());

        assert_eq!(
            read_only_error(s.create_project(&pinput("Q"))),
            "LAPTOP is using zWorkTracker, so this computer is read-only. Take over from the banner to work here."
        );
        read_only_error(s.create_task(&tinput("U", &p)));
        read_only_error(s.delete_task(&t));
        read_only_error(s.set_setting("myZone", serde_json::json!("Europe/Lisbon")));
        read_only_error(s.set_device_prefix("B"));
        read_only_error(s.restore_backup(&backup));
        assert_eq!(s.file().data, data);
        assert_eq!(s.revision(), revision, "nothing was committed");
        assert_eq!(s.store.total_changes(), before, "nor written");
        assert!(!s.lease.want_claim, "and nothing asks for the lease");

        // A cold start is checked first, so warm up before it: this is about the lease.
        s.warm = true;
        read_only_error(s.start_task(&t));

        // These are this device's own. The data stays the same.
        s.set_theme(Theme::Dark).unwrap();
        s.set_hide_earnings(true).unwrap();
        s.set_backup_keep(9).unwrap();
        s.mark_client_zone_prompt_done().unwrap();
        s.remove_sync_target("/somewhere/else").unwrap();
        assert_eq!(s.file().prefs.theme, Theme::Dark);
        assert_eq!(s.file().data, data);
        assert!(s.snapshot().lease.read_only);
        // Looking at everything still works.
        assert!(s.summary_preview(&this_month(&p)).is_ok());
    }

    #[test]
    fn read_only_still_takes_what_a_sync_brings_and_can_ask_for_a_pause() {
        let (here, there) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let clock = ManualClock::at(T0);
        let (mut s, mut other, t) = two_devices(here.path(), there.path(), &clock);
        s.add_sync_target(custom_folder("/not/there")).unwrap();
        other.warm = true;
        other.start_task(&t).unwrap();
        s.warm = true;
        held_by_laptop(&mut s);

        clock.advance(60);
        assert!(s.absorb_remote(&other.file().data.clone(), BTreeMap::new(), Vec::new()));
        assert_eq!(s.file().data.tasks[&t].state, TaskState::Running, "merged");
        s.request_remote_pause(&t).unwrap();
        assert_eq!(s.file().data.pause_requests.len(), 1);
        assert!(!s.lease.want_claim, "none of this needs the lease");
        assert_eq!(s.lease.last_change_at, None);
    }

    #[test]
    fn a_timer_that_is_running_can_still_close_when_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t) = running(dir.path(), &clock);
        s.add_sync_target(custom_folder("/not/there")).unwrap();
        held_by_laptop(&mut s);
        read_only_error(s.pause_task(&t));
        // Quitting isn't the user changing anything.
        clock.advance(60);
        s.on_exit().unwrap();
        assert!(
            s.file()
                .data
                .own_open_segments(&s.file().device_id)
                .is_empty()
        );
        assert_eq!(s.file().data.tasks[&t].state, TaskState::Paused);
    }

    #[test]
    fn time_away_found_is_kept_when_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, _) = running(dir.path(), &clock);
        s.add_sync_target(custom_folder("/not/there")).unwrap();
        held_by_laptop(&mut s);
        s.observe_activity(Some(0)).unwrap();
        clock.advance(3 * 3600);
        assert!(s.observe_activity(Some(0)).unwrap());
        assert_eq!(pending_gaps(&s).len(), 1);
    }

    #[test]
    fn choosing_a_zone_to_look_at_is_not_a_change_and_works_when_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.add_sync_target(custom_folder("/not/there")).unwrap();
        s.set_setting("activeZone", serde_json::json!("Asia/Tokyo"))
            .unwrap();
        s.set_setting("showBoth", serde_json::json!(true)).unwrap();
        assert!(!s.lease.want_claim, "looking claims nothing");
        assert_eq!(s.lease.last_change_at, None);
        held_by_laptop(&mut s);
        s.set_setting("activeZone", serde_json::json!("Europe/Lisbon"))
            .unwrap();
        assert_eq!(
            s.file().data.settings["activeZone"].value,
            serde_json::json!("Europe/Lisbon")
        );
        s.set_setting("showBoth", serde_json::json!(false)).unwrap();
        assert!(
            s.set_setting("activeZone", serde_json::json!("Nowhere/Land"))
                .is_err()
        );
        read_only_error(s.set_setting("clientZone", serde_json::json!("Asia/Tokyo")));
    }

    #[test]
    fn a_change_wants_the_lease_only_with_a_folder_and_only_if_it_changes_shared_data() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        assert_eq!(s.lease.state, LeaseStateName::Off);
        s.create_project(&pinput("P")).unwrap();
        assert!(!s.lease.want_claim, "no folder, no lease");
        assert_eq!(s.lease.last_change_at, None);

        s.add_sync_target(custom_folder("/not/there")).unwrap();
        assert_eq!(s.lease.state, LeaseStateName::Checking);
        s.set_theme(Theme::Dark).unwrap();
        assert!(!s.lease.want_claim, "a preference claims nothing");
        clock.advance(60);
        s.create_project(&pinput("Q")).unwrap();
        assert!(s.lease.want_claim);
        assert_eq!(s.lease.last_change_at, Some(s.now()));

        s.remove_sync_target("/not/there").unwrap();
        assert_eq!(s.lease.state, LeaseStateName::Off);
        assert!(!s.lease.want_claim, "forgotten with the last folder");

        // A store with a folder starts by checking, and writable.
        s.add_sync_target(custom_folder("/not/there")).unwrap();
        drop(s);
        let s = open(dir.path(), &clock);
        assert_eq!(s.lease.state, LeaseStateName::Checking);
        assert!(!s.read_only());
    }

    #[test]
    fn a_change_refused_or_not_saved_does_not_count_as_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.add_sync_target(custom_folder("/not/there")).unwrap();
        assert!(s.create_task(&tinput("", "no-project")).is_err());
        assert!(!s.lease.want_claim);
        // The save fails inside its transaction, as on a full disk.
        s.set_saves_blocked(true);
        assert!(s.create_project(&pinput("P")).is_err());
        assert!(!s.lease.want_claim, "nothing happened that needs the lease");
        s.set_saves_blocked(false);
    }

    fn restored_elsewhere(at: Ts) -> EpochInfo {
        EpochInfo {
            epoch: 2,
            by: "laptop-id".to_owned(),
            at,
            restored_from: "20260301T090000Z--a1b2c3d4--auto".to_owned(),
            base_taken_at: at,
        }
    }

    #[test]
    fn a_backup_from_before_a_restore_is_refused_while_folders_are_connected() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let p = s.create_project(&pinput("P")).unwrap();
        s.run_daily_backup().unwrap();
        let before = s.list_backups().unwrap()[0].name.clone();
        let info = restored_elsewhere(s.now());
        s.mutate(|f, _| {
            f.prefs.epoch = Some(info);
            Ok(())
        })
        .unwrap();
        s.add_sync_target(custom_folder("/not/there")).unwrap();
        s.update_project(&p, &pinput("Renamed")).unwrap();
        let kept = s.store.total_changes();
        let refused = s.restore_backup(&before).unwrap_err();
        assert_eq!(refused.code(), "conflict");
        assert_eq!(
            refused.to_string(),
            "This backup is from before the version of 2026-03-01 09:00 was restored. Restoring it would undo that on every computer. Use Versions instead."
        );
        assert_eq!(s.store.total_changes(), kept, "nothing was touched");

        // One taken since is at the same point, so it does no harm.
        clock.advance(86_400);
        s.run_daily_backup().unwrap();
        s.update_project(&p, &pinput("Renamed again")).unwrap();
        let since = s
            .list_backups()
            .unwrap()
            .into_iter()
            .find(|b| b.name != before)
            .unwrap()
            .name;
        s.restore_backup(&since).unwrap();
        assert_eq!(s.file().data.projects[&p].name, "Renamed");
        assert_eq!(s.file().prefs.epoch.as_ref().unwrap().epoch, 2, "kept");

        // With no folder there is nothing to undo on other computers.
        s.remove_sync_target("/not/there").unwrap();
        s.restore_backup(&before).unwrap();
        assert_eq!(s.file().data.projects[&p].name, "P");
    }

    #[test]
    fn a_backup_restored_with_a_folder_counts_as_a_change_that_needs_the_lease() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.create_project(&pinput("P")).unwrap();
        s.run_daily_backup().unwrap();
        let name = s.list_backups().unwrap()[0].name.clone();
        s.add_sync_target(custom_folder("/not/there")).unwrap();
        assert!(!s.lease.want_claim);
        s.restore_backup(&name).unwrap();
        assert!(s.lease.want_claim);
    }

    #[test]
    fn restored_data_and_the_restore_it_follows_are_saved_together() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let (mut s, t) = running(dir.path(), &clock);
        s.mutate(|f, _| {
            f.data
                .set_device_prefix(&Ctx::at(Ts::parse(T0).unwrap(), "other-device"), "Z")
        })
        .unwrap();
        let prefixes = s.file().data.prefixes.clone();
        s.warm = true;
        // What comes back has a task running here, and an earlier state of this store.
        let mut version = s.file().data.clone();
        version.prefixes.clear();
        version.audit_log.clear();
        s.mutate(|f, c| {
            f.data
                .audit(c, "x", "y", "later", "", Value::Null, Value::Null);
            Ok(())
        })
        .unwrap();
        let audit_before = s.file().data.audit_log.len();
        clock.advance(600);
        let at = Ts::parse(T0).unwrap().later(120);
        let info = restored_elsewhere(s.now());
        let names = s
            .install_restored(
                version,
                RestoreNote {
                    entity: "version",
                    name: "20260301T090000Z--a1b2c3d4--auto".to_owned(),
                    after: serde_json::json!({"epoch": 2}),
                    closed_at: at,
                    reason: PauseReason::BackupRestored,
                    last_heartbeat: None,
                    epoch: Some(info.clone()),
                },
            )
            .unwrap();
        assert_eq!(names, vec!["T".to_owned()]);
        assert!(!s.is_warm(), "comes back cold");
        assert_eq!(s.file().prefs.epoch.as_ref(), Some(&info));
        assert_eq!(s.device_file().epoch_info.as_ref(), Some(&info));
        assert_eq!(s.file().data.prefixes, prefixes, "current prefixes stay");
        assert_eq!(s.file().data.audit_log.len(), audit_before + 1);
        let log = s
            .file()
            .data
            .audit_log
            .values()
            .find(|a| a.entity == "version")
            .unwrap();
        assert_eq!(log.action, "restore");
        assert_eq!(log.after, serde_json::json!({"epoch": 2}));
        let seg = s.file().data.segments.values().next().unwrap();
        assert_eq!(seg.end, Some(at));
        assert_eq!(s.file().data.tasks[&t].state, TaskState::Paused);
        assert_eq!(
            s.file().data.tasks[&t].pause_reason,
            Some(PauseReason::BackupRestored)
        );
        // Read back from disk, both are there: one save.
        drop(s);
        let s = open(dir.path(), &clock);
        assert_eq!(s.file().prefs.epoch.as_ref(), Some(&info));
        assert_eq!(
            s.file().data.segments.values().next().unwrap().end,
            Some(at)
        );
    }

    #[test]
    fn a_failed_install_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let before = s.file().clone();
        s.set_saves_blocked(true);
        let result = s.install_restored(
            Shared::default(),
            RestoreNote {
                entity: "version",
                name: "n".to_owned(),
                after: Value::Null,
                closed_at: s.now(),
                reason: PauseReason::BackupRestored,
                last_heartbeat: None,
                epoch: Some(restored_elsewhere(s.now())),
            },
        );
        assert!(result.is_err());
        assert_eq!(s.file(), &before);
        s.set_saves_blocked(false);
        assert_eq!(s.stored(), before, "and the store either");
    }

    #[test]
    fn folders_joined_follow_the_folders_connected() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let joined = |s: &Service| s.file().prefs.sync_joined.clone();
        let mut s = open(dir.path(), &clock);
        assert_eq!(
            joined(&s),
            Some(Vec::new()),
            "a new store has joined nothing"
        );
        s.add_sync_target(custom_folder("/a")).unwrap();
        s.add_sync_target(custom_folder("/b")).unwrap();
        assert_eq!(
            joined(&s),
            Some(Vec::new()),
            "a folder joins once it has been read"
        );

        // A store from before this field has been merging with its folders all along.
        s.mutate(|f, _| {
            f.prefs.sync_joined = None;
            Ok(())
        })
        .unwrap();
        drop(s);
        let mut s = open(dir.path(), &clock);
        assert_eq!(joined(&s), Some(vec!["/a".to_owned(), "/b".to_owned()]));

        // Taken off, it is as new if it comes back, so it merges rather than replaces.
        s.remove_sync_target("/a").unwrap();
        assert_eq!(joined(&s), Some(vec!["/b".to_owned()]));
        s.switch_targets(Some("/b"), custom_folder("/c"), None)
            .unwrap();
        assert_eq!(joined(&s), Some(Vec::new()), "and /c isn't joined yet");
        assert_eq!(s.lease.state, LeaseStateName::Checking);
        s.switch_targets(Some("/c"), custom_folder("/c"), None)
            .unwrap();
        assert_eq!(s.file().prefs.sync_targets, vec![custom_folder("/c")]);

        // One left over from a store changed by hand goes when the store opens.
        s.mutate(|f, _| {
            f.prefs.sync_joined = Some(vec!["/gone".to_owned(), "/c".to_owned()]);
            Ok(())
        })
        .unwrap();
        drop(s);
        let s = open(dir.path(), &clock);
        assert_eq!(joined(&s), Some(vec!["/c".to_owned()]));
    }

    /// D1: what makes two spellings one folder, on each kind of system.
    #[test]
    fn folder_keys_ignore_trailing_separators_and_on_windows_case_and_slashes() {
        let same = |a: &str, b: &str, windows: bool| {
            folder_key_for(a, windows) == folder_key_for(b, windows)
        };
        assert!(same("/cloud/OneDrive", "/cloud/OneDrive/", false));
        assert!(same("/cloud/OneDrive", "/cloud/OneDrive//", false));
        assert!(!same("/cloud/OneDrive", "/cloud/onedrive", false));
        assert!(!same("/cloud/One", "/cloud/OneDrive", false));
        assert!(!same("C:\\Sync", "C:/Sync", false), "\\ is a letter there");
        assert_eq!(folder_key_for("/", false), "/");
        assert_eq!(folder_key_for("//", false), "/");

        assert!(same(
            "C:\\Users\\Me\\OneDrive",
            "c:/users/me/onedrive/",
            true
        ));
        assert!(same(
            "C:\\Users\\Me\\OneDrive",
            "C:\\Users\\Me\\OneDrive\\\\",
            true
        ));
        assert!(same("\\\\nas\\share\\work", "//NAS/share/work/", true));
        assert!(!same("C:\\Users\\Me\\One", "C:\\Users\\Me\\OneDrive", true));
        assert_eq!(folder_key_for("D:\\", true), "d:\\");
        assert_eq!(folder_key_for("D:/", true), "d:\\");
        assert_eq!(folder_key_for("D:\\\\", true), "d:\\");
        assert!(
            !same("D:\\", "D:", true),
            "D: alone is not the drive's root"
        );
    }

    #[test]
    fn a_folder_waiting_for_a_choice_goes_with_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.add_sync_target(custom_folder("/a")).unwrap();
        s.add_sync_target(custom_folder("/b")).unwrap();
        let pending = |s: &mut Service, root: &str| {
            let info = restored_elsewhere(s.now());
            let root = root.to_owned();
            s.mutate(|f, _| {
                f.prefs.join_pending = Some(crate::model::JoinPending { root, info });
                Ok(())
            })
            .unwrap();
        };
        pending(&mut s, "/a");
        // The page's words: restored from the version of {time} on {name}.
        let shown = s.snapshot().sync.join_pending.unwrap();
        assert_eq!(shown.root, "/a");
        assert_eq!(shown.taken_from, "20260301T090000Z--a1b2c3d4--auto");
        assert_eq!(shown.at, Ts::parse(T0).unwrap());
        assert_eq!(shown.device_name, "another device");
        // A folder this device restored once, connected again.
        let own = s.file().device_id.clone();
        s.mutate(|f, _| {
            if let Some(p) = &mut f.prefs.join_pending {
                p.info.by = own;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(s.snapshot().sync.join_pending.unwrap().device_name, "pc");
        s.remove_sync_target("/b").unwrap();
        assert!(
            s.file().prefs.join_pending.is_some(),
            "that was the other one"
        );
        s.remove_sync_target("/a").unwrap();
        assert_eq!(s.file().prefs.join_pending, None);
        assert!(s.snapshot().sync.join_pending.is_none());

        s.add_sync_target(custom_folder("/c")).unwrap();
        pending(&mut s, "/c");
        s.switch_targets(Some("/c"), custom_folder("/d"), None)
            .unwrap();
        assert_eq!(s.file().prefs.join_pending, None);

        // One whose folder went while the app wasn't looking goes when the store opens.
        pending(&mut s, "/gone");
        drop(s);
        let s = open(dir.path(), &clock);
        assert_eq!(s.file().prefs.join_pending, None);
    }

    #[test]
    fn a_snapshot_has_the_lease_and_the_platform_as_the_page_reads_them() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        let v = serde_json::to_value(s.snapshot()).unwrap();
        assert_eq!(
            v["lease"],
            serde_json::json!({
                "state": "off",
                "readOnly": false,
                "holder": null,
                "generation": null,
                "note": null,
                "reachableRoots": [],
                "unreachableRoots": [],
            })
        );
        assert_eq!(v["platform"], serde_json::json!({"packaged": false}));
        assert_eq!(v["sync"]["joinPending"], Value::Null);

        s.add_sync_target(custom_folder("/a")).unwrap();
        s.set_packaged(true);
        assert!(s.packaged());
        held_by_laptop(&mut s);
        clock.advance(90);
        s.lease.reachable_roots = vec!["/a".to_owned()];
        s.lease.unreachable_roots = vec!["/b".to_owned()];
        s.lease.note = Some("A note".to_owned());
        let info = restored_elsewhere(s.now());
        s.mutate(|f, _| {
            f.prefs.join_pending = Some(crate::model::JoinPending {
                root: "/a".to_owned(),
                info,
            });
            Ok(())
        })
        .unwrap();
        let v = serde_json::to_value(s.snapshot()).unwrap();
        assert_eq!(
            v["lease"],
            serde_json::json!({
                "state": "heldByOtherLive",
                "readOnly": true,
                "holder": {
                    "deviceId": "laptop-id",
                    "deviceName": "LAPTOP",
                    "generation": 4,
                    "beatSeq": 9,
                    "lastSignAt": T0,
                    "ageSecs": 90,
                    "clockSuspect": false,
                },
                "generation": 4,
                "note": "A note",
                "reachableRoots": ["/a"],
                "unreachableRoots": ["/b"],
            })
        );
        assert_eq!(v["platform"], serde_json::json!({"packaged": true}));
        assert_eq!(
            v["sync"]["joinPending"],
            serde_json::json!({
                "root": "/a",
                "takenFrom": "20260301T090000Z--a1b2c3d4--auto",
                "at": "2026-03-01T09:01:30Z",
                "deviceName": "another device",
            })
        );
    }

    #[test]
    fn the_view_gives_this_devices_own_generation_while_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.add_sync_target(custom_folder("/a")).unwrap();
        s.lease.state = LeaseStateName::HeldByMe;
        s.lease.claim = Some(crate::lease::Claim {
            generation: 8,
            acquired_at: s.now(),
            beat_seq: 1,
        });
        let view = s.lease_view();
        assert_eq!(view.generation, Some(8));
        assert!(view.holder.is_none() && !view.read_only);
    }

    #[test]
    fn with_no_folder_to_read_the_view_names_the_last_holder() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.add_sync_target(custom_folder("/a")).unwrap();
        s.lease.state = LeaseStateName::Unknown;
        // Nobody known: keep working.
        let view = s.lease_view();
        assert!(!view.read_only && view.holder.is_none());
        let memo = crate::model::LeaseMemo {
            device_id: "laptop-id".to_owned(),
            device_name: "LAPTOP".to_owned(),
            generation: 6,
            at: s.now(),
        };
        s.mutate(|f, _| {
            f.prefs.lease_memo = Some(memo);
            Ok(())
        })
        .unwrap();
        clock.advance(3600);
        let view = s.lease_view();
        assert!(view.read_only);
        let holder = view.holder.unwrap();
        assert_eq!(holder.device_name, "LAPTOP");
        assert_eq!(holder.age_secs, Some(3600));
        assert_eq!(view.generation, Some(6));
        read_only_error(s.create_project(&pinput("P")));
        // Work here anyway.
        s.lease.offline_claim = Some(6);
        assert!(!s.lease_view().read_only);
        s.create_project(&pinput("P")).unwrap();
        s.lease.offline_claim = None;
        // A day later it can't stop anyone.
        clock.advance(86_400);
        assert!(!s.lease_view().read_only);
        s.create_project(&pinput("Q")).unwrap();
        // The last holder being this device says nothing about another.
        let own = crate::model::LeaseMemo {
            device_id: s.file().device_id.clone(),
            device_name: "pc".to_owned(),
            generation: 7,
            at: s.now(),
        };
        s.mutate(|f, _| {
            f.prefs.lease_memo = Some(own);
            Ok(())
        })
        .unwrap();
        let view = s.lease_view();
        assert!(!view.read_only && view.holder.is_none());
    }

    #[test]
    fn a_store_with_no_folder_is_told_of_uninstalling_only_when_packaged() {
        const WORDS: &str = "This copy of zWorkTracker keeps its data in a private Windows folder. Uninstalling the app deletes that folder and your hours with it. Connect a OneDrive or Google Drive folder in Settings > Sync between devices.";
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        assert!(!s.snapshot().warnings.iter().any(|w| w == WORDS));
        s.set_packaged(true);
        assert!(s.snapshot().warnings.iter().any(|w| w == WORDS));
        s.add_sync_target(custom_folder("/a")).unwrap();
        assert!(!s.snapshot().warnings.iter().any(|w| w == WORDS));
    }

    fn folder_warnings(s: &Service) -> Vec<String> {
        s.snapshot()
            .warnings
            .into_iter()
            .filter(|w| w.contains("hasn't been written"))
            .collect()
    }

    #[test]
    fn a_folder_failing_for_half_an_hour_is_mentioned_and_where_the_data_is_safe() {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        for (provider, root) in [
            (SyncProvider::OneDrive, "/od"),
            (SyncProvider::GoogleDrive, "/gd"),
        ] {
            s.add_sync_target(SyncTarget {
                provider,
                root: root.to_owned(),
            })
            .unwrap();
        }
        let written = s.now();
        s.target_status_mut("/od").last_written_at = Some(written);
        s.target_status_mut("/gd").last_written_at = Some(written);
        assert!(folder_warnings(&s).is_empty());

        s.target_status_mut("/od").last_error = Some("The disk is full".to_owned());
        clock.advance(29 * 60);
        assert!(folder_warnings(&s).is_empty(), "not yet");
        s.target_status_mut("/gd").last_written_at = Some(s.now());
        clock.advance(60);
        assert_eq!(
            folder_warnings(&s),
            vec![
                "OneDrive folder hasn't been written since 30 min ago: The disk is full. Google Drive folder is fine, so your data is safe there."
                    .to_owned()
            ]
        );

        // When neither can be written, each is said, and only this computer has it all.
        s.target_status_mut("/gd").last_error = Some(
            "Folder not found. Is the drive connected and the desktop client running?".to_owned(),
        );
        clock.advance(3600);
        assert_eq!(
            folder_warnings(&s),
            vec![
                "OneDrive folder hasn't been written since 1 h 30 min ago: The disk is full. Your data is safe on this computer.".to_owned(),
                "Google Drive folder hasn't been written since 1 h 1 min ago: Folder not found. Is the drive connected and the desktop client running? Your data is safe on this computer.".to_owned(),
            ]
        );

        // A write that works ends it.
        for root in ["/od", "/gd"] {
            let status = s.target_status_mut(root);
            status.last_error = None;
            status.last_written_at = Some(written);
        }
        assert!(folder_warnings(&s).is_empty());
    }

    #[test]
    fn a_folder_that_has_failed_all_this_run_counts_from_the_start_of_it_and_goes_by_its_own_name()
    {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(T0);
        let mut s = open(dir.path(), &clock);
        s.add_sync_target(custom_folder("/home/me/Dropbox"))
            .unwrap();
        s.target_status_mut("/home/me/Dropbox").last_error = Some("It is locked.".to_owned());
        clock.advance(2 * 86_400);
        assert_eq!(
            folder_warnings(&s),
            vec![
                "Dropbox folder hasn't been written since 2 days ago: It is locked. Your data is safe on this computer.".to_owned()
            ]
        );
    }

    #[test]
    fn lengths_of_time_read_as_a_person_says_them() {
        for (secs, want) in [
            (-5, "less than a minute"),
            (59, "less than a minute"),
            (60, "1 min"),
            (3599, "59 min"),
            (3600, "1 h"),
            (5400, "1 h 30 min"),
            (86_399, "23 h 59 min"),
            (86_400, "1 day"),
            (2 * 86_400 - 1, "1 day"),
            (2 * 86_400, "2 days"),
        ] {
            assert_eq!(span_text(secs), want);
        }
    }

    /// Two devices on a folder, called what a person would call them, with a timer running on the laptop.
    fn laptop_and_desktop(
        dirs: [&Path; 3],
        clock: &ManualClock,
    ) -> (
        std::sync::Mutex<Service>,
        std::sync::Mutex<Service>,
        String,
        String,
        String,
    ) {
        let [here, there, root] = dirs;
        let named = |dir: &Path, name: &str| {
            let mut s = Service::open(dir, Arc::new(clock.clone()), name).unwrap();
            s.add_sync_target(custom_folder(&root.to_string_lossy()))
                .unwrap();
            std::sync::Mutex::new(s)
        };
        let (laptop, desktop) = (named(here, "LAPTOP"), named(there, "DESKTOP"));
        let (p, t, seg) = {
            let mut s = laptop.lock().unwrap();
            let p = s.create_project(&pinput("P")).unwrap();
            let t = s.create_task(&tinput("T", &p)).unwrap();
            s.start_task(&t).unwrap();
            let seg = s.file().data.segments.keys().next().unwrap().clone();
            (p, t, seg)
        };
        cycle(&laptop);
        cycle(&desktop);
        cycle(&laptop);
        (laptop, desktop, p, t, seg)
    }

    fn cycle(s: &std::sync::Mutex<Service>) -> bool {
        crate::sync::run_cycle(s, &crate::sync::RealIo)
    }

    fn warning_notices(s: &Service) -> Vec<String> {
        s.snapshot()
            .notices
            .into_iter()
            .filter(|n| n.level == "warning")
            .map(|n| n.message)
            .collect()
    }

    #[test]
    fn r5_a_device_that_comes_back_gets_its_time_back_and_hears_of_it() {
        let dirs = [(); 3].map(|()| tempfile::tempdir().unwrap());
        let clock = ManualClock::at("2026-03-02T09:00:00Z");
        let (laptop, desktop, _, t, seg) =
            laptop_and_desktop(dirs.each_ref().map(|d| d.path()), &clock);
        // Off the network for a day and more, with its timer still going.
        clock.advance(25 * 3600);
        cycle(&desktop);
        desktop
            .lock()
            .unwrap()
            .settle_lost_segment(&seg, "the laptop is gone")
            .unwrap();
        cycle(&desktop);
        {
            let s = desktop.lock().unwrap();
            let snap = s.snapshot();
            assert_eq!(snap.tasks[0].running_elsewhere, None, "not any more");
            let view = snap.raw_segments.iter().find(|r| r.id == seg).unwrap();
            assert!(view.corrected && view.raw_end.is_none());
            let piece = snap.segments.iter().find(|p| p.segment_id == seg).unwrap();
            assert_eq!(piece.provisional_end, None);
            assert!(piece.end.is_some());
        }

        // Back an hour later. Its timer stops at once, and its record replaces the settlement.
        clock.advance(3600);
        assert!(cycle(&laptop));
        {
            let s = laptop.lock().unwrap();
            let closed = &s.file().data.segments[&seg];
            let real = Ts::parse("2026-03-03T11:00:00Z").unwrap();
            assert_eq!(closed.end, Some(real));
            assert_eq!(closed.edit, None, "lifted");
            assert_eq!(s.file().data.tasks[&t].state, TaskState::Paused);
            assert!(
                s.file()
                    .data
                    .own_open_segments(&s.file().device_id)
                    .is_empty()
            );
            let told = warning_notices(&s);
            assert!(
                told.iter().any(|m| m == "\"T\": 1 day logged here after DESKTOP closed it is back. Correct it in task details if that time wasn't work."),
                "{told:?}"
            );
        }
        // The other device takes it from the file, and tells nobody.
        clock.advance(60);
        assert!(cycle(&desktop));
        let s = desktop.lock().unwrap();
        assert_eq!(s.file().data.segments[&seg].edit, None);
        assert_eq!(
            s.file().data.segments[&seg].end,
            Some(Ts::parse("2026-03-03T11:00:00Z").unwrap())
        );
        assert!(warning_notices(&s).is_empty());
        let log = s.audit_entries(Some(&seg));
        assert!(log.iter().any(|a| a.action == "settleLost"));
        assert!(log.iter().any(|a| a.action == "reclaimAfterReturn"));
    }

    #[test]
    fn r5_a_settled_segment_is_not_called_running_on_two_devices_at_once() {
        let dirs = [(); 3].map(|()| tempfile::tempdir().unwrap());
        let clock = ManualClock::at("2026-03-02T09:00:00Z");
        let (_laptop, desktop, p, t, seg) =
            laptop_and_desktop(dirs.each_ref().map(|d| d.path()), &clock);
        clock.advance(25 * 3600);
        cycle(&desktop);
        let mut d = desktop.lock().unwrap();
        let other = d.create_task(&tinput("U", &p)).unwrap();
        d.warm = true;
        d.start_task(&other).unwrap();
        let two = |s: &Service| {
            s.snapshot()
                .warnings
                .iter()
                .any(|w| w.starts_with("Tasks are running on more than one device"))
        };
        let elsewhere = |s: &Service| {
            s.snapshot()
                .tasks
                .iter()
                .find(|x| x.id == t)
                .unwrap()
                .running_elsewhere
                .clone()
        };
        assert!(two(&d));
        assert_eq!(elsewhere(&d), Some("LAPTOP".to_owned()));
        d.settle_lost_segment(&seg, "the laptop is gone").unwrap();
        assert!(!two(&d));
        assert_eq!(elsewhere(&d), None);
        // This device's own goes by the raw end, so a timer here stays shown as running here.
        let snap = d.snapshot();
        assert_eq!(
            snap.tasks.iter().find(|x| x.id == other).unwrap().state,
            TaskState::Running
        );
    }

    #[test]
    fn r5_a_late_file_with_an_earlier_end_lifts_a_settlement_with_nobody_to_tell() {
        let dirs = [(); 3].map(|()| tempfile::tempdir().unwrap());
        let clock = ManualClock::at("2026-03-02T09:00:00Z");
        let (laptop, desktop, _, t, seg) =
            laptop_and_desktop(dirs.each_ref().map(|d| d.path()), &clock);
        // The laptop paused at 09:30 and wrote its file, which then took a while to arrive.
        clock.advance(1800);
        laptop.lock().unwrap().pause_task(&t).unwrap();
        let held_back = laptop.lock().unwrap().device_file();
        drop(laptop);
        clock.advance(3600);
        {
            let mut s = desktop.lock().unwrap();
            s.request_remote_pause(&t).unwrap();
            // Its last sign reached the desktop as later than the pause.
            let seen = BTreeMap::from([(
                held_back.device_id.clone(),
                RemoteDevice {
                    name: "LAPTOP".to_owned(),
                    last_heartbeat: Some(s.now()),
                    written_at: s.now(),
                },
            )]);
            s.absorb_remote(&Shared::default(), seen, Vec::new());
            s.settle_lost_segment(&seg, "the laptop is gone").unwrap();
            assert_eq!(
                s.file().data.segments[&seg].edit.as_ref().unwrap().end,
                s.now(),
                "the asked moment, as far as is known"
            );
            assert!(s.absorb_remote(&held_back.data, BTreeMap::new(), Vec::new()));
            assert_eq!(s.file().data.segments[&seg].edit, None);
            assert_eq!(
                s.file().data.segments[&seg].effective(),
                Some((
                    Ts::parse("2026-03-02T09:00:00Z").unwrap(),
                    Some(Ts::parse("2026-03-02T09:30:00Z").unwrap())
                ))
            );
            assert!(warning_notices(&s).is_empty());
        }
    }

    #[test]
    fn r5_time_already_billed_stays_and_the_device_that_comes_back_hears_once() {
        let dirs = [(); 3].map(|()| tempfile::tempdir().unwrap());
        let clock = ManualClock::at("2026-03-02T09:00:00Z");
        let (laptop, desktop, p, _, seg) =
            laptop_and_desktop(dirs.each_ref().map(|d| d.path()), &clock);
        clock.advance(3600);
        laptop.lock().unwrap().heartbeat().unwrap();
        cycle(&laptop);
        cycle(&desktop);
        clock.advance(25 * 3600);
        let reference = {
            let mut s = desktop.lock().unwrap();
            s.settle_lost_segment(&seg, "the laptop is gone").unwrap();
            let id = s.create_summary(&this_month(&p)).unwrap();
            s.file().data.summaries[&id].reference.clone()
        };
        cycle(&desktop);

        clock.advance(600);
        cycle(&laptop);
        let s = laptop.lock().unwrap();
        let held = &s.file().data.segments[&seg];
        assert!(held.end.is_some(), "its own record is there");
        let edit = held.edit.as_ref().unwrap();
        assert_eq!(
            edit.end,
            Ts::parse("2026-03-02T10:00:00Z").unwrap(),
            "the summary's"
        );
        assert!(!edit.settled);
        let told = warning_notices(&s);
        let wanted = format!(
            "\"T\": the time on {reference} differs from what this computer recorded by 1 day. To change it, void the summary, then restore the segment or add the time by hand."
        );
        assert_eq!(told.iter().filter(|m| **m == wanted).count(), 1, "{told:?}");
        drop(s);
        clock.advance(600);
        cycle(&laptop);
        let told = warning_notices(&laptop.lock().unwrap());
        assert_eq!(told.iter().filter(|m| **m == wanted).count(), 1, "once");
    }

    #[test]
    fn r5_a_settlement_that_waited_on_a_summary_lifts_when_the_summary_is_voided_here() {
        let dirs = [(); 3].map(|()| tempfile::tempdir().unwrap());
        let clock = ManualClock::at("2026-03-02T09:00:00Z");
        let (laptop, desktop, p, t, seg) =
            laptop_and_desktop(dirs.each_ref().map(|d| d.path()), &clock);
        clock.advance(25 * 3600);
        cycle(&desktop);
        let mut d = desktop.lock().unwrap();
        d.settle_lost_segment(&seg, "the laptop is gone").unwrap();
        let summary = d.create_summary(&this_month(&p)).unwrap();
        // The laptop's record arrives, closed.
        laptop.lock().unwrap().pause_task(&t).unwrap();
        let file = laptop.lock().unwrap().device_file();
        d.absorb_remote(&file.data, BTreeMap::new(), Vec::new());
        assert!(d.file().data.segments[&seg].edit.as_ref().unwrap().settled);
        // Nothing arrives now, but something is due.
        d.void_summary(&summary).unwrap();
        assert!(d.absorb_remote(&Shared::default(), BTreeMap::new(), Vec::new()));
        assert_eq!(d.file().data.segments[&seg].edit, None);
        assert!(!d.absorb_remote(&Shared::default(), BTreeMap::new(), Vec::new()));
    }
}
