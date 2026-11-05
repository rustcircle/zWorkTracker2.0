//! Persisted and synced data model. Every replicated record carries the clock that merging needs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::time::Ts;

pub const SCHEMA_VERSION: u64 = 2;

fn yes() -> bool {
    true
}

/// Version-stamped writer info for last-writer-wins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub at: Ts,
    pub by: String,
}

/// The stamp of a field a schema 1 file didn't have: older than any real edit, so any edit wins.
impl Default for Stamp {
    fn default() -> Self {
        Self {
            at: Ts::EARLIEST,
            by: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BillingType {
    #[default]
    Hourly,
    FixedFee,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub color: String,
    #[serde(default)]
    pub archived: bool,
    /// Tombstone so a deleted project never comes back through sync.
    #[serde(default)]
    pub deleted: bool,
    /// When false the project earns nothing, whatever its rate history says.
    #[serde(default = "yes")]
    pub billable: bool,
    #[serde(default)]
    pub budget_hours: Option<f64>,
    #[serde(default)]
    pub client_zone: Option<String>,
    #[serde(default)]
    pub billing_type: BillingType,
    /// The agreed price of a fixed-fee project, in cents of `fee_currency`.
    #[serde(default)]
    pub fixed_fee_cents: Option<i64>,
    #[serde(default)]
    pub fee_currency: Option<String>,
    pub created_at: Ts,
    /// The clock of the record as a whole, moved by every edit. It is all that a build older than the group
    /// clocks below compares, and what they fall back to in a copy from such a build.
    pub stamp: Stamp,
    /// The clock of the name, colour and client zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_stamp: Option<Stamp>,
    /// The clock of billable, billing type, fee and fee currency, which only mean something together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_stamp: Option<Stamp>,
    /// The clock of the budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_stamp: Option<Stamp>,
    /// The clock of the archive state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_stamp: Option<Stamp>,
}

impl Project {
    #[must_use]
    pub fn identity_clock(&self) -> &Stamp {
        self.identity_stamp.as_ref().unwrap_or(&self.stamp)
    }
    #[must_use]
    pub fn billing_clock(&self) -> &Stamp {
        self.billing_stamp.as_ref().unwrap_or(&self.stamp)
    }
    #[must_use]
    pub fn budget_clock(&self) -> &Stamp {
        self.budget_stamp.as_ref().unwrap_or(&self.stamp)
    }
    #[must_use]
    pub fn archive_clock(&self) -> &Stamp {
        self.archive_stamp.as_ref().unwrap_or(&self.stamp)
    }

    /// Writes out the clock of each group that has none. An edit does this before it moves `stamp`, or fields
    /// it leaves alone would look as new as the edit.
    pub fn pin_clocks(&mut self) {
        for clock in [
            &mut self.identity_stamp,
            &mut self.billing_stamp,
            &mut self.budget_stamp,
            &mut self.archive_stamp,
        ] {
            clock.get_or_insert_with(|| self.stamp.clone());
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Draft,
    Running,
    Paused,
    Stopped,
}

impl TaskState {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
        }
    }
    /// Join rank: stopped is final and beats every other state in a merge.
    #[must_use]
    pub fn rank(self) -> u8 {
        u8::from(self == Self::Stopped)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PauseReason {
    User,
    AutoPaused {
        #[serde(rename = "byTaskId")]
        by_task_id: String,
    },
    AppQuit,
    CrashRecovery,
    SyncReconciled,
    /// A restored backup held this task running.
    BackupRestored,
    /// Another device asked this one to pause it.
    RemoteRequest,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub name: String,
    pub name_stamp: Stamp,
    pub project_id: String,
    pub project_stamp: Stamp,
    /// TipTap document JSON or null. Rendered only through the editor.
    #[serde(default)]
    pub description: Value,
    pub description_stamp: Stamp,
    #[serde(default)]
    pub deleted: bool,
    pub deleted_stamp: Stamp,
    /// Deleted on purpose, with all its logged time. Where `deleted` gives way to logged time, this
    /// outranks everything and never clears. The segments stay in the file, for backups and merges,
    /// and `is_live` keeps them from everything that reads them, even ones that arrive afterwards.
    #[serde(default)]
    pub discarded: Option<Stamp>,
    /// The task this one continues. Set once at creation, for traceability.
    #[serde(default)]
    pub continued_from: Option<String>,
    pub created_at: Ts,
    pub state: TaskState,
    #[serde(default)]
    pub pause_reason: Option<PauseReason>,
    /// Clock for state and pause reason, independent of edits to metadata.
    pub state_stamp: Stamp,
    #[serde(default)]
    pub stopped_at: Option<Ts>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub tags_stamp: Stamp,
    /// The subtask new segments of this task are attributed to.
    #[serde(default)]
    pub active_subtask: Option<String>,
    #[serde(default)]
    pub active_subtask_stamp: Stamp,
}

impl Task {
    /// A deleted draft is gone. If another device logged time on it meanwhile, the time wins. A task
    /// deleted with its time is gone whatever happens to it, and that time with it.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.discarded.is_none() && (!self.deleted || self.state != TaskState::Draft)
    }
}

/// One unit of work. Only the owning device ever writes its raw end. Another can close it for everyone else
/// with an edit, when its owner is gone, see `settle_foreign_open_segment`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub id: String,
    pub task_id: String,
    pub device_id: String,
    /// Raw times. Corrections go in `edit`, so these stay exactly as recorded.
    pub start: Ts,
    pub end: Option<Ts>,
    /// Entered by hand rather than timed.
    #[serde(default)]
    pub manual: bool,
    #[serde(default)]
    pub subtask_id: Option<String>,
    /// A correction laid over the raw times.
    #[serde(default)]
    pub edit: Option<SegmentEdit>,
    #[serde(default)]
    pub edit_stamp: Stamp,
}

impl Segment {
    /// The times that count, or `None` when voided. The end is `None` while running.
    #[must_use]
    pub fn effective(&self) -> Option<(Ts, Option<Ts>)> {
        match &self.edit {
            Some(e) if e.voided => None,
            Some(e) => Some((e.start, Some(e.end))),
            None => Some((self.start, self.end)),
        }
    }

    /// Still being timed, as anyone but its owner can tell. A raw-open segment with any edit is closed. The
    /// owner's own bookkeeping goes by the raw end instead, so that it still closes it.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.end.is_none() && self.edit.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentEdit {
    pub start: Ts,
    pub end: Ts,
    #[serde(default)]
    pub voided: bool,
    #[serde(default)]
    pub reason: String,
    /// Written by another device, closing a segment at its last known moment because its owner was gone. The
    /// owner's own record replaces this once it exists. A correction made on top is a new edit, and stays.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub settled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GapKind {
    /// The machine slept, or the clock jumped between heartbeats.
    Sleep,
    /// No keyboard or mouse input.
    Idle,
    /// The session was locked, whatever the idle count said.
    Lock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GapDecision {
    /// Unanswered. Counts in nothing until confirmed.
    Pending,
    Keep,
    Drop,
    Thinking,
}

/// Time away detected inside a running segment. The segment's raw times don't change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdleGap {
    pub id: String,
    pub segment_id: String,
    pub task_id: String,
    pub device_id: String,
    pub start: Ts,
    pub end: Ts,
    pub kind: GapKind,
    pub decision: GapDecision,
    pub decision_stamp: Stamp,
}

/// A pause the user marked as still working. It counts in Billable and never in Exact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BillablePause {
    pub id: String,
    pub task_id: String,
    pub start: Ts,
    pub end: Ts,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub deleted: bool,
    pub stamp: Stamp,
}

/// A request from another device to pause a task that seems to have been left running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PauseRequest {
    pub id: String,
    pub task_id: String,
    pub segment_id: String,
    pub requested_by: String,
    pub at: Ts,
    pub stamp: Stamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    pub id: String,
    pub name: String,
    pub color: String,
    #[serde(default)]
    pub deleted: bool,
    pub stamp: Stamp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub description: Value,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub deleted: bool,
    pub stamp: Stamp,
}

/// One line of the record of changes to logged time. Entries are only ever added.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub id: String,
    pub at: Ts,
    pub by: String,
    pub entity: String,
    pub entity_id: String,
    pub action: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub before: Value,
    #[serde(default)]
    pub after: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryRow {
    pub task_id: String,
    pub task_name: String,
    pub day: String,
    pub exact_secs: i64,
    #[serde(default)]
    pub pause_secs: i64,
    pub billable_secs: i64,
    pub cents: BTreeMap<String, i64>,
}

/// A billing summary for the user's own guidance. Not an invoice, and not a tax document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingSummary {
    pub id: String,
    /// `<prefix>-<number>`, such as `A-001`. Unique per device, not one global sequence.
    pub reference: String,
    pub prefix: String,
    pub number: u32,
    pub device_id: String,
    pub project_id: String,
    pub from: String,
    pub to: String,
    pub zone: String,
    pub created_at: Ts,
    pub segment_ids: Vec<String>,
    #[serde(default)]
    pub pause_ids: Vec<String>,
    pub rows: Vec<SummaryRow>,
    pub totals: BTreeMap<String, i64>,
    #[serde(default)]
    pub voided: bool,
    pub stamp: Stamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevicePrefix {
    pub prefix: String,
    pub stamp: Stamp,
}

/// One date-effective version of a work shift. A shift's history is a set of versions sharing
/// `shift_id`; for a given date the version with the latest `effective_from` on or before it applies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShiftVersion {
    pub id: String,
    pub shift_id: String,
    /// `YYYY-MM-DD`, in the shift's own zone.
    pub effective_from: String,
    pub name: String,
    pub color: String,
    /// Wall-clock `HH:MM` in `zone`. `end <= start` means the shift crosses midnight.
    pub start: String,
    pub end: String,
    /// ISO weekdays, 1 = Monday .. 7 = Sunday, of the date the shift starts on.
    pub weekdays: Vec<u8>,
    pub zone: String,
    #[serde(default)]
    pub break_minutes: u32,
    /// From `effective_from` on, this shift no longer exists. Earlier dates keep their versions.
    #[serde(default)]
    pub retired: bool,
    #[serde(default)]
    pub deleted: bool,
    pub stamp: Stamp,
}

/// A project's price per hour from `effective_from` on. Past earnings never change when a new
/// version is added. `rate_cents: None` means "no rate" from that moment, which is not the same as 0.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateVersion {
    pub id: String,
    pub project_id: String,
    pub effective_from: Ts,
    pub rate_cents: Option<i64>,
    /// `EUR` or `USD`.
    pub currency: String,
    #[serde(default)]
    pub deleted: bool,
    pub stamp: Stamp,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoundMethod {
    Nearest,
    #[default]
    Up,
    Down,
}

/// How time is rounded for days on or after `effective_from`. `increment_minutes: 0` means exact.
/// With `project_id` set it overrides the global setting for that project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoundingVersion {
    pub id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    /// `YYYY-MM-DD`, compared with the row's day in the zone being reported.
    pub effective_from: String,
    pub increment_minutes: u32,
    pub method: RoundMethod,
    #[serde(default)]
    pub deleted: bool,
    pub stamp: Stamp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingEntry {
    pub value: Value,
    pub stamp: Stamp,
}

/// The replicated state: a join-semilattice, see `merge`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shared {
    #[serde(default)]
    pub projects: BTreeMap<String, Project>,
    #[serde(default)]
    pub tasks: BTreeMap<String, Task>,
    #[serde(default)]
    pub segments: BTreeMap<String, Segment>,
    #[serde(default)]
    pub settings: BTreeMap<String, SettingEntry>,
    #[serde(default)]
    pub shifts: BTreeMap<String, ShiftVersion>,
    #[serde(default)]
    pub rates: BTreeMap<String, RateVersion>,
    #[serde(default)]
    pub rounding: BTreeMap<String, RoundingVersion>,
    #[serde(default)]
    pub gaps: BTreeMap<String, IdleGap>,
    #[serde(default)]
    pub pauses: BTreeMap<String, BillablePause>,
    #[serde(default)]
    pub tags: BTreeMap<String, Tag>,
    #[serde(default)]
    pub templates: BTreeMap<String, Template>,
    #[serde(default)]
    pub summaries: BTreeMap<String, BillingSummary>,
    /// Summary reference prefix per device id.
    #[serde(default)]
    pub prefixes: BTreeMap<String, DevicePrefix>,
    #[serde(default)]
    pub audit_log: BTreeMap<String, AuditEntry>,
    /// Asks made from other devices to pause a running task. Only the owner writes its segment's raw end.
    #[serde(default)]
    pub pause_requests: BTreeMap<String, PauseRequest>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncProvider {
    OneDrive,
    GoogleDrive,
    Custom,
}

/// A local folder kept in the cloud by a desktop client. Data lives in `<root>/zWorkTracker/`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncTarget {
    pub provider: SyncProvider,
    pub root: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Theme {
    #[default]
    Light,
    SolarizedLight,
    SolarizedDark,
    Dark,
}

/// Device-local preferences, never replicated.
///
/// Compat note: a key this version doesn't know is skipped, not refused. Up to 1.3.2 these also held `mica` (Windows
/// 11's backdrop, removed in 1.3.3), so a store or backup of theirs still loads, and the next write leaves it out.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPrefs {
    /// Every enabled folder. Empty means sync is off. Never filled without the user's choice.
    #[serde(default)]
    pub sync_targets: Vec<SyncTarget>,
    #[serde(default)]
    pub sync_prompt_done: bool,
    #[serde(default)]
    pub client_zone_prompt_done: bool,
    #[serde(default)]
    pub hide_notice_shown: bool,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub hide_earnings: bool,
    #[serde(default)]
    pub last_currency: Option<String>,
    /// How many daily backups to keep. `None` means the default.
    #[serde(default)]
    pub backup_keep: Option<u32>,
    /// The highest budget alert already shown per project, so each threshold notifies once.
    #[serde(default)]
    pub budget_alerts_seen: BTreeMap<String, u8>,
    /// When the current absence began, so a crash during it still records it as time away.
    #[serde(default)]
    pub away_since: Option<Ts>,
    /// What that absence is, so a crash labels it correctly.
    #[serde(default)]
    pub away_kind: Option<GapKind>,
    /// The highest reference number ever seen per prefix. Kept across a restore, so a number that
    /// has been handed out is never handed out again.
    #[serde(default)]
    pub reference_marks: BTreeMap<String, u32>,
    /// The first-run setup was finished or skipped.
    #[serde(default)]
    pub setup_done: bool,
    /// The step to resume the first-run setup at.
    #[serde(default)]
    pub setup_step: u32,
    /// A sync folder move whose previous folder couldn't be read, to retry later.
    #[serde(default)]
    pub pending_copy: Option<PendingCopy>,
    /// The restore this device's data follows. `None` is epoch 0, a store that never saw one.
    #[serde(default)]
    pub epoch: Option<EpochInfo>,
    /// The folders whose restores this device follows. A root joins after its first successful read, so one
    /// connected later can't silently bring data from past a restore. `None` is a store from before this field.
    #[serde(default)]
    pub sync_joined: Option<Vec<String>>,
    /// The last known holder of the lease, for a folder that can't be reached and for generations. Written only
    /// when the holder or the generation changes, as every save of the preferences rewrites the whole store.
    #[serde(default)]
    pub lease_memo: Option<LeaseMemo>,
    /// The last version written to each folder, by root.
    #[serde(default)]
    pub version_marks: BTreeMap<String, VersionMark>,
    /// A folder waiting for the user to choose between its restored data and this device's.
    #[serde(default)]
    pub join_pending: Option<JoinPending>,
    /// Set at a first start or after a damaged store, when the data is to come from the sync folders: nothing is
    /// written to them until data from one of them has arrived, so an empty folder that isn't synced yet can't get
    /// an empty file of this device. Cleared by the first read that finds device files, by a restore, or by the user
    /// changing the folders.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub awaiting_cloud: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingCopy {
    pub from: String,
    pub to: String,
}

/// A restore every device follows. Files from below it are fenced, so nothing about merging changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpochInfo {
    pub epoch: u64,
    /// The device id of the restorer.
    pub by: String,
    pub at: Ts,
    /// The version file's name.
    pub restored_from: String,
    /// When that version was taken.
    pub base_taken_at: Ts,
}

impl EpochInfo {
    /// How restores are ordered, with no epoch as `(0, "")`.
    #[must_use]
    pub fn key(info: Option<&Self>) -> (u64, &str) {
        info.map_or((0, ""), |e| (e.epoch, e.by.as_str()))
    }
}

/// The last known holder of the lease.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseMemo {
    pub device_id: String,
    pub device_name: String,
    pub generation: u64,
    pub at: Ts,
}

/// The last version written to one folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionMark {
    pub name: String,
    pub at: Ts,
    pub checksum: String,
}

/// A folder with a restore in its past, and a device with its own data: only the user can say which goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinPending {
    pub root: String,
    pub info: EpochInfo,
}

/// Why a version is written. The word is part of the file's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionReason {
    Auto,
    Quit,
    Manual,
    TakeOver,
    PreRestore,
    PreAdopt,
    Restored,
}

impl VersionReason {
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Quit => "quit",
            Self::Manual => "manual",
            Self::TakeOver => "takeover",
            Self::PreRestore => "pre-restore",
            Self::PreAdopt => "pre-adopt",
            Self::Restored => "restored",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDevice {
    pub name: String,
    pub last_heartbeat: Option<Ts>,
    pub written_at: Ts,
}

/// The local store: `z-work-data.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalFile {
    pub version: u64,
    pub device_id: String,
    pub device_name: String,
    #[serde(default)]
    pub last_heartbeat: Option<Ts>,
    #[serde(default)]
    pub prefs: LocalPrefs,
    #[serde(default)]
    pub remote_devices: BTreeMap<String, RemoteDevice>,
    /// The newest stamp this device has seen, its own or another's. New stamps follow it (see `hlc`). Local only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest_stamp: Option<Stamp>,
    /// The latest time, by this computer's clock, at which a change to the data was saved here. Never goes down. The
    /// clock-behind warning compares the clock with it, and never with this device's own stamps, which the hybrid
    /// logical clock may have put ahead of the clock (see `hlc`). Local only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_change_here: Option<Ts>,
    #[serde(default)]
    pub data: Shared,
}

/// What each device writes to `<root>/zWorkTracker/devices/<device-uuid>.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceFile {
    pub schema_version: u64,
    pub device_id: String,
    pub device_name: String,
    pub written_at: Ts,
    #[serde(default)]
    pub last_heartbeat: Option<Ts>,
    /// The restore this file's data follows. An older app writes none, and is fenced after a restore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch_info: Option<EpochInfo>,
    pub data: Shared,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::check_device_file;
    use serde_json::json;

    const NOW: &str = "2026-11-02T12:00:00Z";

    fn now() -> Ts {
        Ts::parse(NOW).unwrap()
    }

    fn file(epoch_info: Option<EpochInfo>) -> DeviceFile {
        DeviceFile {
            schema_version: SCHEMA_VERSION,
            device_id: "d".into(),
            device_name: "pc".into(),
            written_at: now(),
            last_heartbeat: None,
            epoch_info,
            data: Shared::default(),
        }
    }

    fn epoch(n: u64, by: &str) -> EpochInfo {
        EpochInfo {
            epoch: n,
            by: by.into(),
            at: now(),
            restored_from: "20261102T120000Z--a1b2c3d4--auto".into(),
            base_taken_at: now(),
        }
    }

    #[test]
    fn a_plain_edit_is_written_as_it_always_was_and_a_settled_one_says_so() {
        let edit = SegmentEdit {
            start: now(),
            end: now(),
            voided: false,
            reason: String::new(),
            settled: false,
        };
        let plain = serde_json::to_value(&edit).unwrap();
        assert_eq!(plain.get("settled"), None);
        let back: SegmentEdit = serde_json::from_value(plain).unwrap();
        assert!(!back.settled, "a copy from an older app is not settled");
        let settled = SegmentEdit {
            settled: true,
            ..edit
        };
        let v = serde_json::to_value(&settled).unwrap();
        assert_eq!(v["settled"], true);
        assert_eq!(serde_json::from_value::<SegmentEdit>(v).unwrap(), settled);
    }

    #[test]
    fn open_means_no_raw_end_and_no_edit() {
        let edit = SegmentEdit {
            start: now(),
            end: now(),
            voided: false,
            reason: String::new(),
            settled: true,
        };
        let running = Segment::default();
        assert!(running.is_open());
        let closed = Segment {
            end: Some(now()),
            ..Segment::default()
        };
        assert!(!closed.is_open());
        let settled = Segment {
            edit: Some(edit.clone()),
            ..Segment::default()
        };
        assert!(!settled.is_open());
        assert!(settled.end.is_none(), "its owner still closes its raw end");
        let voided = Segment {
            edit: Some(SegmentEdit {
                voided: true,
                ..edit
            }),
            ..Segment::default()
        };
        assert!(!voided.is_open());
    }

    #[test]
    fn a_device_file_from_an_older_app_reads_with_no_epoch_and_an_older_app_reads_a_new_one() {
        let old = json!({
            "schemaVersion": SCHEMA_VERSION,
            "deviceId": "d",
            "deviceName": "pc",
            "writtenAt": NOW,
            "lastHeartbeat": null,
            "data": {},
        });
        let read: DeviceFile = serde_json::from_value(old).unwrap();
        assert_eq!(read.epoch_info, None);
        check_device_file(&read, now()).unwrap();
        assert_eq!(
            serde_json::to_value(&read).unwrap().get("epochInfo"),
            None,
            "and it is written back the same"
        );

        // The shape an older app reads.
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        #[expect(dead_code, reason = "these only have to parse")]
        struct OldDeviceFile {
            schema_version: u64,
            device_id: String,
            device_name: String,
            written_at: Ts,
            #[serde(default)]
            last_heartbeat: Option<Ts>,
            data: Shared,
        }
        let new = file(Some(epoch(3, "d")));
        let bytes = serde_json::to_vec(&new).unwrap();
        serde_json::from_slice::<OldDeviceFile>(&bytes).unwrap();
        check_device_file(&new, now()).unwrap();
        let back: DeviceFile = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, new);
        assert_eq!(
            serde_json::to_value(&new).unwrap()["epochInfo"]["restoredFrom"],
            "20261102T120000Z--a1b2c3d4--auto"
        );
    }

    #[test]
    fn preferences_from_before_the_lease_read_as_empty() {
        let prefs: LocalPrefs = serde_json::from_value(json!({})).unwrap();
        assert_eq!(prefs.epoch, None);
        assert_eq!(prefs.sync_joined, None);
        assert_eq!(prefs.lease_memo, None);
        assert!(prefs.version_marks.is_empty());
        assert_eq!(prefs.join_pending, None);
        let full = LocalPrefs {
            epoch: Some(epoch(2, "d")),
            sync_joined: Some(vec!["/a".into()]),
            lease_memo: Some(LeaseMemo {
                device_id: "d".into(),
                device_name: "pc".into(),
                generation: 7,
                at: now(),
            }),
            version_marks: BTreeMap::from([(
                "/a".to_owned(),
                VersionMark {
                    name: "n".into(),
                    at: now(),
                    checksum: "c".into(),
                },
            )]),
            join_pending: Some(JoinPending {
                root: "/b".into(),
                info: epoch(3, "e"),
            }),
            ..LocalPrefs::default()
        };
        let back: LocalPrefs = serde_json::from_slice(&serde_json::to_vec(&full).unwrap()).unwrap();
        assert_eq!(back, full);
    }

    #[test]
    fn restores_are_ordered_by_epoch_then_device_and_none_comes_first() {
        let (a2, b2, a3) = (epoch(2, "a"), epoch(2, "b"), epoch(3, "a"));
        assert_eq!(EpochInfo::key(None), (0, ""));
        assert!(EpochInfo::key(None) < EpochInfo::key(Some(&a2)));
        assert!(EpochInfo::key(Some(&a2)) < EpochInfo::key(Some(&b2)));
        assert!(EpochInfo::key(Some(&b2)) < EpochInfo::key(Some(&a3)));
    }

    #[test]
    fn a_restore_that_cant_be_right_makes_a_file_no_use_to_anyone() {
        check_device_file(&file(Some(epoch(1, "d"))), now()).unwrap();
        let bad = [
            EpochInfo {
                epoch: (1 << 40) + 1,
                ..epoch(1, "d")
            },
            EpochInfo {
                by: String::new(),
                ..epoch(1, "d")
            },
            EpochInfo {
                by: "x".repeat(65),
                ..epoch(1, "d")
            },
            EpochInfo {
                restored_from: "a\nb".into(),
                ..epoch(1, "d")
            },
            EpochInfo {
                at: Ts::parse("2026-11-05T12:00:00Z").unwrap(),
                ..epoch(1, "d")
            },
            EpochInfo {
                base_taken_at: Ts::parse("2026-11-05T12:00:00Z").unwrap(),
                ..epoch(1, "d")
            },
            EpochInfo {
                at: Ts::parse("1999-01-01T00:00:00Z").unwrap(),
                ..epoch(1, "d")
            },
        ];
        for info in bad {
            assert!(
                check_device_file(&file(Some(info.clone())), now()).is_err(),
                "{info:?}"
            );
            let mut f = file(Some(info.clone()));
            assert!(
                crate::validate::keep_valid_records(&mut f, now()).is_err(),
                "{info:?}"
            );
        }
    }

    #[test]
    fn each_reason_has_the_word_a_versions_file_name_uses() {
        for (reason, word) in [
            (VersionReason::Auto, "auto"),
            (VersionReason::Quit, "quit"),
            (VersionReason::Manual, "manual"),
            (VersionReason::TakeOver, "takeover"),
            (VersionReason::PreRestore, "pre-restore"),
            (VersionReason::PreAdopt, "pre-adopt"),
            (VersionReason::Restored, "restored"),
        ] {
            assert_eq!(reason.word(), word);
        }
    }
}
