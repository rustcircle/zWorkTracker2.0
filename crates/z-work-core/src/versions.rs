//! Saved copies of the data in the sync folders, and restoring from them.
//!
//! `<root>/zWorkTracker/versions/<YYYY-MM>/<stamp>--<device8>--<reason>.json.gz` is gzip of the exact device file JSON
//! a sync writes, so any unzip tool opens it, with `<name>.changes.txt` beside it saying what changed. The computer
//! holding the lease writes one after its data changes, at most once an hour. Any computer that may change data writes
//! one on quitting after changes, by hand, and around a restore or a take over, and one following a restore keeps a copy
//! of what it replaced. Only the holder thins them out, by name alone, so two of them can never disagree.
//!
//! Each folder is written on its own thread with a time limit, and one that fails costs only its own copy. Restoring
//! raises the data epoch every device file carries, so no merge can undo it: see `epoch`.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::backup;
use crate::changes::{self, DeviceNames, Header};
use crate::epoch::epoch_above;
use crate::error::CoreError;
use crate::gz;
use crate::lease::LeaseStateName;
use crate::model::{
    DeviceFile, EpochInfo, PauseReason, SCHEMA_VERSION, SyncProvider, SyncTarget, VersionMark,
    VersionReason,
};
use crate::persist;
use crate::service::{RestoreNote, Service};
use crate::sync::{
    APP_FOLDER, Counts, HungTargets, MAX_REMOTE_FILE_BYTES, SyncIo, bounded, checksum, counts,
    is_transient, read_at_most, read_target,
};
use crate::time::Ts;
use crate::validate::{keep_valid_records, prepare_own_file};

pub const VERSIONS_FOLDER: &str = "versions";
/// How long one folder's part of a version may take before the rest goes ahead without it.
pub const VERSION_IO_LIMIT: Duration = Duration::from_secs(60);
/// Saved versions past this size in all are worth a word on the page. Nothing is deleted because of it: choosing a cap
/// would delete history, and that is the owner's call.
pub const BUDGET_BYTES: u64 = 1 << 30;

const DAY_SECS: i64 = 86_400;
/// At most one automatic version an hour, as each is a full copy.
const HOURLY_SECS: i64 = 3600;
/// Quitting soon after a version adds nothing worth a copy.
const QUIT_SECS: i64 = 15 * 60;
const KEEP_ALL_SECS: i64 = 7 * DAY_SECS;
const KEEP_DAILY_SECS: i64 = 60 * DAY_SECS;
/// Versions made around a restore or a take over are what a problem is traced through.
const PINNED_SECS: i64 = 365 * DAY_SECS;
/// The most of a stretch in which this computer saved no version in a folder that counts toward any version's age when
/// it thins that folder out. A clock that jumps ahead leaves such a stretch, and so does time away from the app: either
/// way versions there age only while this computer goes on saving them, so a clock it can't vouch for deletes nothing
/// at once. As far ahead as a file's date may be (`validate::MAX_SKEW_SECS`).
const MAX_UNSEEN_SECS: i64 = 2 * DAY_SECS;
/// A temporary file this old belongs to a write that died.
const TMP_SECS: i64 = DAY_SECS;
/// The change record is plain text of at most `changes::AUDIT_LINES` lines.
const MAX_CHANGES_BYTES: u64 = 8 * 1024 * 1024;
/// The last version this device took, in its data folder, which the next one is compared with.
pub const BASE_FILE: &str = "versions-base.json.gz";
const GZ: &str = ".json.gz";
const TXT: &str = ".changes.txt";
/// `bounded` keys: a hung folder's sync worker doesn't stop its versions, and one version worker per folder at most.
const KEY: &str = "version";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const RETRY_BASE_MS: u64 = 100;
const RETRY_ATTEMPTS: u32 = 5;
/// The most records a restore preview names one by one.
const WARNINGS_NAMED: usize = 20;

/// "Save a version now" while a first start waits for its data.
const AWAITING_CLOUD: &str = "This computer's data hasn't arrived from your sync folder yet, so no version can be saved there yet.";
/// A restore while a first start waits for its data. Its own file in the folders, all there is of its data, would fall
/// below the restore and be written over, with no copy of today's data saved first.
const AWAITING_RESTORE: &str = "This computer's data hasn't arrived from your sync folder yet, so no version can be restored yet. Nothing was changed. Check that the folder is synced to this computer, then press Sync now in Settings > Sync between devices.";
const FOLDER_NOT_FOUND: &str =
    "Folder not found. Is the drive connected and the desktop client running?";
const NO_FOLDER: &str = "Versions are saved in your sync folders, and none is connected.";
const WAITING_FOR_CHOICE: &str = "Your sync folder is waiting for your choice in Settings, under Sync between devices, so no version can be saved there yet.";
const NO_PRE_RESTORE: &str =
    "Couldn't save a copy of today's data to any folder first, so nothing was changed.";
const TAKE_OVER_FIRST: &str = "Take over first to restore.";
const VERSION_TOO_LARGE: &str = "The file is unreasonably large.";
const CHANGES_TOO_LARGE: &str = "The record of changes is unreasonably large.";
const FROM_A_NEWER_APP: &str =
    "This version was saved by a newer version of the app. Update this computer to restore it.";
const UNKNOWN_DEVICE: &str = "another computer";

const REASONS: [VersionReason; 7] = [
    VersionReason::Auto,
    VersionReason::Quit,
    VersionReason::Manual,
    VersionReason::TakeOver,
    VersionReason::PreRestore,
    VersionReason::PreAdopt,
    VersionReason::Restored,
];

#[must_use]
pub fn versions_dir(root: &Path) -> PathBuf {
    root.join(APP_FOLDER).join(VERSIONS_FOLDER)
}

/// What a version's name says: `20261102T142530Z--a1b2c3d4--auto`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionName {
    /// By the writer's clock, in UTC.
    pub taken_at: Ts,
    /// The first 8 hex digits of the writer's device id.
    pub device8: String,
    pub reason: VersionReason,
}

fn stamp_of(at: Ts) -> String {
    at.datetime().format("%Y%m%dT%H%M%SZ").to_string()
}

fn reason_of(word: &str) -> Option<VersionReason> {
    REASONS.into_iter().find(|r| r.word() == word)
}

/// What a version asked for is worth when another is asked for before it is written, as the request is one slot: one
/// kept a year outranks one by hand. A pre-adopt copy waits in a slot of its own, so the request only wakes it.
fn rank(reason: VersionReason) -> u8 {
    match reason {
        VersionReason::Restored | VersionReason::TakeOver | VersionReason::PreRestore => 2,
        VersionReason::Manual => 1,
        VersionReason::Auto | VersionReason::Quit | VersionReason::PreAdopt => 0,
    }
}

/// Kept whole for a year, whatever the retention rule says.
fn protected(reason: VersionReason) -> bool {
    matches!(
        reason,
        VersionReason::PreRestore
            | VersionReason::PreAdopt
            | VersionReason::TakeOver
            | VersionReason::Restored
    )
}

impl VersionName {
    /// Only the exact form this app writes, with a reason it knows. Anything else, a conflict copy such as
    /// `<name> (1).json.gz` or `<name>-PC2.json.gz` included, is never read, listed or deleted.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let (stamp, rest) = name.split_once("--")?;
        let (device8, word) = rest.split_once("--")?;
        let part = |from: usize, to: usize| {
            stamp
                .get(from..to)
                .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
        };
        if stamp.len() != 16 || stamp.get(8..9) != Some("T") || stamp.get(15..) != Some("Z") {
            return None;
        }
        let iso = format!(
            "{}-{}-{}T{}:{}:{}Z",
            part(0, 4)?,
            part(4, 6)?,
            part(6, 8)?,
            part(9, 11)?,
            part(11, 13)?,
            part(13, 15)?
        );
        let taken_at = Ts::parse(&iso).filter(|t| stamp_of(*t) == stamp)?;
        let hex = device8.len() == 8
            && device8
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        let reason = reason_of(word)?;
        hex.then(|| Self {
            taken_at,
            device8: device8.to_owned(),
            reason,
        })
    }

    /// `2026-11`: the folder it lives in.
    #[must_use]
    pub fn month(&self) -> String {
        self.taken_at.datetime().format("%Y-%m").to_string()
    }

    fn pinned(&self, now: Ts) -> bool {
        protected(self.reason) && now.secs() - self.taken_at.secs() < PINNED_SECS
    }
}

/// The 8 hex digits of a device's id that its versions' names carry.
fn device8_of(device_id: &str) -> String {
    let hex: String = device_id
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    format!("{hex:0<8}")
}

/// The name of a version taken now by a device.
#[must_use]
pub fn version_name(at: Ts, device_id: &str, reason: VersionReason) -> String {
    format!(
        "{}--{}--{}",
        stamp_of(at),
        device8_of(device_id),
        reason.word()
    )
}

/// The versions the retention rule keeps out of `names`: every one under 7 days old, the newest of each UTC day under
/// 60 days, and the newest of each UTC month after that, forever. Those made around a restore or a take over are kept
/// whole for their first year. A name dated ahead of `now` counts as new. A name the rule doesn't know is kept: nothing
/// is deleted that can't be accounted for. Pure, so every device that prunes agrees, and keeping again keeps the same.
#[must_use]
pub fn keep(names: &[String], now: Ts) -> BTreeSet<String> {
    keep_aged(names, |v| (now.secs() - v.taken_at.secs()).max(0))
}

/// `keep`, with each version as old as `age_of` says, in seconds.
fn keep_aged(names: &[String], age_of: impl Fn(&VersionName) -> i64) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut known = Vec::with_capacity(names.len());
    for name in names {
        match VersionName::parse(name) {
            Some(v) => known.push((name.as_str(), v)),
            None => {
                out.insert(name.clone());
            }
        }
    }
    let mut newest_of_day: BTreeMap<i64, (Ts, &str)> = BTreeMap::new();
    let mut newest_of_month: BTreeMap<String, (Ts, &str)> = BTreeMap::new();
    for (name, v) in &known {
        let this = (v.taken_at, *name);
        let day = newest_of_day
            .entry(v.taken_at.secs().div_euclid(DAY_SECS))
            .or_insert(this);
        *day = (*day).max(this);
        let month = newest_of_month.entry(v.month()).or_insert(this);
        *month = (*month).max(this);
    }
    for (name, v) in &known {
        let age = age_of(v);
        let top_of_day = newest_of_day
            .get(&v.taken_at.secs().div_euclid(DAY_SECS))
            .is_some_and(|(_, n)| n == name);
        let top_of_month = newest_of_month
            .get(&v.month())
            .is_some_and(|(_, n)| n == name);
        if (protected(v.reason) && age < PINNED_SECS)
            || age < KEEP_ALL_SECS
            || (age < KEEP_DAILY_SECS && top_of_day)
            || top_of_month
        {
            out.insert((*name).to_owned());
        }
    }
    out
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

fn lock(svc: &Mutex<Service>) -> MutexGuard<'_, Service> {
    svc.lock().unwrap_or_else(PoisonError::into_inner)
}

fn max_bytes() -> usize {
    usize::try_from(MAX_REMOTE_FILE_BYTES).unwrap_or(usize::MAX)
}

#[derive(Clone, Copy, Debug, Default)]
struct Sizes {
    gz: u64,
    txt: u64,
}

/// What one folder holds of saved versions.
#[derive(Debug, Default)]
struct Scanned {
    versions: BTreeMap<String, Sizes>,
    /// This app's temporary files, with when the version they were for was taken.
    tmp: Vec<(PathBuf, Ts)>,
}

impl Scanned {
    fn bytes(&self) -> u64 {
        self.versions.values().map(|s| s.gz + s.txt).sum()
    }
}

/// `2026-11`.
fn is_month(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 7
        && b.get(4) == Some(&b'-')
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || c.is_ascii_digit())
        && matches!(name.get(5..), Some(m) if ("01"..="12").contains(&m))
}

fn in_month(stem: &str, month: &str) -> Option<VersionName> {
    VersionName::parse(stem).filter(|v| v.month() == month)
}

fn scan(io: &dyn SyncIo, root: &Path) -> Result<Scanned, String> {
    if !io.is_dir(root) {
        return Err(FOLDER_NOT_FOUND.to_owned());
    }
    let dir = versions_dir(root);
    let mut out = Scanned::default();
    if !io.is_dir(&dir) {
        return Ok(out);
    }
    let months = with_retry(io, || io.list(&dir))
        .map_err(|e| format!("Could not list the versions folder: {e}"))?;
    for month in months.into_iter().filter(|m| is_month(m)) {
        let here = dir.join(&month);
        if !io.is_dir(&here) {
            continue;
        }
        let files = with_retry(io, || io.list(&here))
            .map_err(|e| format!("Could not list the versions folder: {e}"))?;
        for file in &files {
            if let Some(stem) = file.strip_suffix(GZ)
                && in_month(stem, &month).is_some()
            {
                // One that can't be measured yet, such as a cloud placeholder, is still there.
                let gz = io.file_size(&here.join(file)).unwrap_or(0);
                out.versions.entry(stem.to_owned()).or_default().gz = gz;
            }
        }
        for file in &files {
            if let Some(stem) = file.strip_suffix(TXT)
                && let Some(sizes) = out.versions.get_mut(stem)
            {
                sizes.txt = io.file_size(&here.join(file)).unwrap_or(0);
            }
            let tmp = file
                .strip_prefix('.')
                .and_then(|f| f.strip_suffix(".tmp"))
                .and_then(|f| f.strip_suffix(GZ).or_else(|| f.strip_suffix(TXT)))
                .and_then(|stem| in_month(stem, &month));
            if let Some(v) = tmp {
                out.tmp.push((here.join(file), v.taken_at));
            }
        }
    }
    Ok(out)
}

/// How old something dated `taken` counts as when this computer, whose versions in a folder are dated `own`, thins that
/// folder out: its age by `now`, less what is past `MAX_UNSEEN_SECS` of each stretch since then in which this computer
/// saved no version there.
fn counted_age(own: &[Ts], now: Ts) -> impl Fn(Ts) -> i64 + use<> {
    let mut points: Vec<i64> = own
        .iter()
        .filter(|t| **t <= now)
        .map(|t| t.secs())
        .collect();
    points.push(now.secs());
    points.sort_unstable();
    // What doesn't count of each stretch, from its first `MAX_UNSEEN_SECS` to its end.
    let unseen: Vec<(i64, i64)> = points
        .windows(2)
        .filter_map(|pair| match pair {
            [from, to] if to - from > MAX_UNSEEN_SECS => Some((from + MAX_UNSEEN_SECS, *to)),
            _ => None,
        })
        .collect();
    let now = now.secs();
    move |taken: Ts| {
        let taken = taken.secs();
        let skipped: i64 = unseen
            .iter()
            .map(|(from, to)| (to - (*from).max(taken)).max(0))
            .sum();
        (now - taken - skipped).max(0)
    }
}

/// Thins out one folder by the retention rule, and returns the size of what stays. Only exact names go: never a conflict
/// copy, never a name the rule doesn't know. Ages are counted as `counted_age` says, for this computer, whose versions'
/// names carry `device8`.
fn prune(io: &dyn SyncIo, root: &Path, now: Ts, device8: &str) -> Result<u64, String> {
    let scanned = scan(io, root)?;
    let names: Vec<String> = scanned.versions.keys().cloned().collect();
    let own: Vec<Ts> = names
        .iter()
        .filter_map(|n| VersionName::parse(n))
        .filter(|v| v.device8 == device8)
        .map(|v| v.taken_at)
        .collect();
    let age = counted_age(&own, now);
    let kept = keep_aged(&names, |v| age(v.taken_at));
    let mut left = 0;
    for (name, sizes) in &scanned.versions {
        let Some(v) = VersionName::parse(name).filter(|_| !kept.contains(name)) else {
            left += sizes.gz + sizes.txt;
            continue;
        };
        let dir = versions_dir(root).join(v.month());
        if io.remove_file(&dir.join(format!("{name}{GZ}"))).is_ok() {
            let _ = io.remove_file(&dir.join(format!("{name}{TXT}")));
        } else {
            left += sizes.gz + sizes.txt;
        }
    }
    for (path, at) in &scanned.tmp {
        if age(*at) > TMP_SECS {
            let _ = io.remove_file(path);
        }
    }
    Ok(left)
}

/// Writes one version and its record to one folder, the record first, so a version that is there has its record. Then
/// reads back that the version is listed at the size written.
fn write_one(
    io: &dyn SyncIo,
    root: &Path,
    name: &str,
    gz: &[u8],
    text: &[u8],
) -> Result<(), String> {
    let v = VersionName::parse(name).ok_or_else(|| "Not a version's name.".to_owned())?;
    if !io.is_dir(root) {
        return Err(FOLDER_NOT_FOUND.to_owned());
    }
    let dir = versions_dir(root).join(v.month());
    with_retry(io, || io.create_dir_all(&dir))
        .map_err(|e| format!("Could not create the versions folder: {e}"))?;
    let txt = format!("{name}{TXT}");
    let file = format!("{name}{GZ}");
    // Dot-prefixed temporaries never match a version's name, so readers ignore them.
    with_retry(io, || {
        io.write_atomic(&dir.join(&txt), &dir.join(format!(".{txt}.tmp")), text)
    })
    .map_err(|e| format!("Could not write the record of changes: {e}"))?;
    let dest = dir.join(&file);
    if let Err(e) = with_retry(io, || {
        io.write_atomic(&dest, &dir.join(format!(".{file}.tmp")), gz)
    }) {
        let _ = io.remove_file(&dir.join(&txt));
        return Err(format!("Could not write the version: {e}"));
    }
    let listed = with_retry(io, || io.list(&dir)).is_ok_and(|names| names.contains(&file));
    let size = io.file_size(&dest).ok();
    if !listed || size != Some(gz.len() as u64) {
        return Err(
            "The version was written but didn't read back whole. Will try again.".to_owned(),
        );
    }
    Ok(())
}

/// Why a version couldn't be taken back.
enum ReadFail {
    /// This copy, now: it may still be syncing, or be damaged. Another copy may do.
    Unreadable(String),
    /// Written by a newer app.
    Future(u64),
    /// Read whole, but as a whole it can't be right.
    Invalid(String),
}

struct Loaded {
    file: DeviceFile,
    /// The records left out, one by one.
    warnings: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchemaProbe {
    schema_version: Option<u64>,
}

/// Reads and checks one copy of a version as sync reads a device file, records that can't be trusted left out.
fn read_version(io: &dyn SyncIo, root: &Path, name: &str, now: Ts) -> Result<Loaded, ReadFail> {
    let unreadable = |why: String| ReadFail::Unreadable(why);
    let v = VersionName::parse(name).ok_or_else(|| unreadable("Not a version's name.".into()))?;
    if !io.is_dir(root) {
        return Err(unreadable(FOLDER_NOT_FOUND.to_owned()));
    }
    let path = versions_dir(root)
        .join(v.month())
        .join(format!("{name}{GZ}"));
    let size = io
        .file_size(&path)
        .map_err(|e| unreadable(format!("Could not read it: {e}")))?;
    if size > MAX_REMOTE_FILE_BYTES {
        return Err(unreadable(VERSION_TOO_LARGE.into()));
    }
    let bytes = read_at_most(io, &path, MAX_REMOTE_FILE_BYTES).map_err(|e| {
        if e.kind() == io::ErrorKind::FileTooLarge {
            unreadable(VERSION_TOO_LARGE.into())
        } else {
            unreadable(format!("Could not read it: {e}"))
        }
    })?;
    let json = gz::decompress(&bytes, max_bytes())
        .map_err(|e| unreadable(format!("It couldn't be unpacked: {e}.")))?;
    match serde_json::from_slice::<SchemaProbe>(&json)
        .ok()
        .and_then(|p| p.schema_version)
    {
        Some(found) if found > SCHEMA_VERSION => return Err(ReadFail::Future(found)),
        Some(found) if found > 0 => {}
        _ => return Err(unreadable("The file is damaged.".into())),
    }
    let mut file: DeviceFile = serde_json::from_slice(&json)
        .map_err(|e| unreadable(format!("The file is damaged: {e}")))?;
    let left_out =
        keep_valid_records(&mut file, now).map_err(|e| ReadFail::Invalid(e.to_string()))?;
    let mut warnings: Vec<String> = left_out
        .iter()
        .take(WARNINGS_NAMED)
        .map(|r| format!("{} ({})", r.name, r.why))
        .collect();
    if left_out.len() > WARNINGS_NAMED {
        warnings.push(format!("and {} more", left_out.len() - WARNINGS_NAMED));
    }
    Ok(Loaded { file, warnings })
}

fn read_bounded<I: SyncIo + Clone + 'static>(
    io: &I,
    hung: &HungTargets,
    root: &str,
    name: &str,
    now: Ts,
    limit: Duration,
) -> Result<Loaded, ReadFail> {
    let (io, folder, name) = (io.clone(), root.to_owned(), name.to_owned());
    bounded(hung, &format!("{KEY}:{root}"), limit, move || {
        read_version(&io, Path::new(&folder), &name, now)
    })
    .map_err(ReadFail::Unreadable)
    .and_then(|r| r)
}

/// The previous snapshot, from this device's data folder.
fn read_base(dir: &Path) -> Option<DeviceFile> {
    let bytes = std::fs::read(dir.join(BASE_FILE)).ok()?;
    let json = gz::decompress(&bytes, max_bytes()).ok()?;
    serde_json::from_slice(&json).ok()
}

fn write_base(dir: &Path, gz: &[u8]) {
    let _ = persist::write_atomic(&dir.join(BASE_FILE), &dir.join(".versions-base.tmp"), gz);
}

/// One version to write in a run.
struct Job {
    reason: VersionReason,
    /// When the data was taken, which the name says: now, or for a copy of what a restore replaced, when that was kept.
    taken_at: Ts,
    file: DeviceFile,
    roots: Vec<String>,
    /// The data as it is now, which moves each folder's mark when it lands.
    current: bool,
}

struct Plan {
    jobs: Vec<Job>,
    now: Ts,
    hung: HungTargets,
    data_dir: PathBuf,
    names: DeviceNames,
    prune: bool,
    /// What this computer's versions' names carry of its id.
    device8: String,
    /// Of the data as it is now.
    checksum: String,
}

/// What one version came to.
struct Done {
    reason: VersionReason,
    name: String,
    file: DeviceFile,
    current: bool,
    landed: Vec<String>,
    failed: Vec<(String, String)>,
}

/// What a run wrote, for the caller that asked for it.
#[derive(Debug, Default)]
pub(crate) struct Ran {
    /// By reason: the version's name and the folders it landed in.
    pub written: Vec<(VersionReason, String, Vec<String>)>,
    /// Folders that failed, with why.
    pub failed: Vec<(String, String)>,
}

impl Ran {
    fn landed(&self, reason: VersionReason) -> Option<&str> {
        self.written
            .iter()
            .find(|(r, _, roots)| *r == reason && !roots.is_empty())
            .map(|(_, name, _)| name.as_str())
    }

    fn why(&self) -> String {
        self.failed
            .first()
            .map_or_else(|| NO_FOLDER.to_owned(), |(_, e)| e.clone())
    }
}

/// What a run is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    /// After a sync: whatever is due.
    Due { quitting: bool },
    /// This version of the data as it is now, at once, in every folder.
    Now(VersionReason),
}

impl Service {
    /// The folders versions go to: all but one waiting for the user's choice, where nothing is written until then.
    fn version_targets(&self) -> Vec<SyncTarget> {
        let waiting = self
            .file
            .prefs
            .join_pending
            .as_ref()
            .map(|p| p.root.as_str());
        self.file
            .prefs
            .sync_targets
            .iter()
            .filter(|t| Some(t.root.as_str()) != waiting)
            .cloned()
            .collect()
    }

    /// There are folders, and none can take a version until the user chooses.
    fn all_folders_waiting(&self) -> bool {
        !self.file.prefs.sync_targets.is_empty() && self.version_targets().is_empty()
    }

    /// Asks again for a version that wasn't written, unless one asked for since is worth as much.
    fn ask_again(&mut self, reason: VersionReason) {
        let keep = match self.take_version_due() {
            Some(due) if rank(due) >= rank(reason) => due,
            _ => reason,
        };
        self.request_version(keep);
    }

    fn device_names(&self) -> DeviceNames {
        DeviceNames::new(
            std::iter::once((self.file.device_id.clone(), self.file.device_name.clone())).chain(
                self.file
                    .remote_devices
                    .iter()
                    .map(|(id, d)| (id.clone(), d.name.clone())),
            ),
        )
    }

    /// A device's name by the 8 hex digits a version's name keeps of its id.
    fn name_of8(&self, device8: &str) -> String {
        if self.file.device_id.starts_with(device8) {
            return self.file.device_name.clone();
        }
        self.file
            .remote_devices
            .iter()
            .find(|(id, _)| id.starts_with(device8))
            .map_or_else(|| UNKNOWN_DEVICE.to_owned(), |(_, d)| d.name.clone())
    }

    /// Decides what to write, taking what was asked for. Nothing is written to no folder. The data is copied and its
    /// checksum taken only once a version may be due, as a sync runs this every time.
    fn plan_versions(&mut self, ask: Ask) -> Option<Plan> {
        let targets = self.version_targets();
        if targets.is_empty() {
            return None;
        }
        let now = self.now();
        let roots: Vec<String> = targets.iter().map(|t| t.root.clone()).collect();
        let read_only = self.read_only();
        let holder = self.lease.state == LeaseStateName::HeldByMe;
        let mut jobs = Vec::new();
        // The reason for a copy of the data as it is now, the folders it may go to, and whether only a change since
        // each folder's last version is worth one.
        let (reason, mut chosen, if_changed) = match ask {
            Ask::Now(reason) => (reason, roots, false),
            Ask::Due { quitting } => {
                // A copy of what following a restore replaced is written whatever the lease says, each one waiting,
                // and named by when it was kept, so that two never share a name.
                let mut last: Option<Ts> = None;
                while let Some(file) = self.take_pre_adopt() {
                    let taken_at =
                        last.map_or(file.written_at, |l| file.written_at.max(l.later(1)));
                    last = Some(taken_at);
                    jobs.push(Job {
                        reason: VersionReason::PreAdopt,
                        taken_at,
                        file,
                        roots: roots.clone(),
                        current: false,
                    });
                }
                let due = self.take_version_due();
                // What a take over or a restore here asked for waits until this device may change data again, as
                // one that may not writes none. One asked for by hand was told it failed.
                if let Some(reason @ (VersionReason::TakeOver | VersionReason::Restored)) = due
                    && read_only
                {
                    self.request_version(reason);
                }
                let quitting = quitting || self.lease.closing || due == Some(VersionReason::Quit);
                let marks = &self.file.prefs.version_marks;
                let old_enough = |gap: i64| -> Vec<String> {
                    roots
                        .iter()
                        .filter(|r| {
                            marks
                                .get(*r)
                                .is_none_or(|m| now.secs() - m.at.secs() >= gap)
                        })
                        .cloned()
                        .collect()
                };
                match due {
                    // Asked for by name: by hand, a take over, a restore. A device that may not change data writes none.
                    Some(
                        reason @ (VersionReason::Manual
                        | VersionReason::TakeOver
                        | VersionReason::Restored
                        | VersionReason::PreRestore),
                    ) if !read_only => (reason, roots, false),
                    _ if quitting
                        && !read_only
                        && (holder || self.lease.last_change_at.is_some()) =>
                    {
                        (VersionReason::Quit, old_enough(QUIT_SECS), true)
                    }
                    _ if holder => (VersionReason::Auto, old_enough(HOURLY_SECS), true),
                    _ => (VersionReason::Auto, Vec::new(), true),
                }
            }
        };
        if jobs.is_empty() && chosen.is_empty() {
            return None;
        }
        let mut current = self.device_file();
        prepare_own_file(&mut current, now);
        let sum = checksum(&current.data);
        if if_changed {
            let marks = &self.file.prefs.version_marks;
            chosen.retain(|r| marks.get(r).is_none_or(|m| m.checksum != sum));
        }
        if !chosen.is_empty() {
            jobs.push(Job {
                reason,
                taken_at: now,
                file: current,
                roots: chosen,
                current: true,
            });
        }
        if jobs.is_empty() {
            return None;
        }
        Some(Plan {
            jobs,
            now,
            hung: self.hung_targets(),
            data_dir: self.paths.dir.clone(),
            names: self.device_names(),
            prune: holder && matches!(ask, Ask::Due { .. }),
            device8: device8_of(&self.file.device_id),
            checksum: sum,
        })
    }

    /// Takes in what a run did: each folder's status, the marks of the folders the current data landed in, and a
    /// version a computer's own step asked for that landed nowhere, which is asked for again. One asked for by hand or
    /// before a restore isn't: whoever asked was told it failed, and a restore refused changed nothing.
    fn versions_done(&mut self, plan: &Plan, done: Vec<Done>, sizes: Vec<(String, u64)>) -> Ran {
        let mut ran = Ran::default();
        let mut marks = Vec::new();
        for d in &done {
            for root in &d.landed {
                let status = self.target_status_mut(root);
                status.last_version_at = Some(plan.now);
                status.last_version_error = None;
                if d.current {
                    marks.push((
                        root.clone(),
                        VersionMark {
                            name: d.name.clone(),
                            at: plan.now,
                            checksum: plan.checksum.clone(),
                        },
                    ));
                }
            }
            for (root, why) in &d.failed {
                self.target_status_mut(root).last_version_error = Some(why.clone());
            }
        }
        for (root, bytes) in sizes {
            self.target_status_mut(&root).versions_bytes = Some(bytes);
        }
        if !marks.is_empty() {
            // A mark that isn't saved only means another version next time.
            let _ = self.mutate_system(|f, _| {
                f.prefs.version_marks.extend(marks);
                Ok(())
            });
        }
        let mut again = None;
        for d in done {
            if d.landed.is_empty() {
                match d.reason {
                    VersionReason::PreAdopt => self.keep_pre_adopt(d.file),
                    reason @ (VersionReason::TakeOver | VersionReason::Restored) => {
                        again = Some(reason);
                    }
                    VersionReason::Auto
                    | VersionReason::Quit
                    | VersionReason::Manual
                    | VersionReason::PreRestore => {}
                }
            }
            ran.failed.extend(d.failed);
            ran.written.push((d.reason, d.name, d.landed));
        }
        // After `keep_pre_adopt`, which asks for its own: the copy it keeps waits beside this.
        if let Some(reason) = again {
            self.ask_again(reason);
        }
        self.revision += 1;
        ran
    }
}

/// The version to compare the first job with: this device's last, else the newest that reads in the first folder.
fn first_base<I: SyncIo + Clone + 'static>(
    io: &I,
    plan: &Plan,
    limit: Duration,
) -> Option<DeviceFile> {
    if let Some(base) = read_base(&plan.data_dir) {
        return Some(base);
    }
    let root = plan.jobs.first()?.roots.first()?.clone();
    let (scan_io, folder) = (io.clone(), root.clone());
    let found = bounded(&plan.hung, &format!("{KEY}:{root}"), limit, move || {
        scan(&scan_io, Path::new(&folder))
    })
    .ok()?
    .ok()?;
    let newest = found
        .versions
        .keys()
        .filter_map(|n| VersionName::parse(n).map(|v| (v.taken_at, n.clone())))
        .max()?;
    read_bounded(io, &plan.hung, &root, &newest.1, plan.now, limit)
        .ok()
        .map(|l| l.file)
}

/// Writes every job to its folders, each folder bounded and on its own. Outside the service lock.
fn carry_out<I: SyncIo + Clone + 'static>(io: &I, plan: &Plan, limit: Duration) -> Vec<Done> {
    let mut base = first_base(io, plan, limit);
    let mut done = Vec::with_capacity(plan.jobs.len());
    for job in &plan.jobs {
        let name = version_name(job.taken_at, &job.file.device_id, job.reason);
        let json = match serde_json::to_vec(&job.file) {
            Ok(json) => json,
            Err(e) => {
                let why = format!("Could not prepare the version: {e}");
                done.push(Done {
                    reason: job.reason,
                    name,
                    file: job.file.clone(),
                    current: job.current,
                    landed: Vec::new(),
                    failed: job.roots.iter().map(|r| (r.clone(), why.clone())).collect(),
                });
                continue;
            }
        };
        let packed = Arc::new(gz::compress(&json));
        let record = changes::diff(base.as_ref().map(|b| &b.data), &job.file.data, &plan.names);
        let text = record.text(&Header {
            taken_at: job.taken_at,
            device_name: &job.file.device_name,
            app_version: APP_VERSION,
            reason: job.reason.word(),
            compared_with: base.as_ref().map(|b| b.written_at),
            epoch: EpochInfo::key(job.file.epoch_info.as_ref()).0,
            schema: job.file.schema_version,
            checksum: &checksum(&job.file.data),
        });
        let text = Arc::new(text.into_bytes());
        let (mut landed, mut failed) = (Vec::new(), Vec::new());
        for root in &job.roots {
            let (io, folder, file) = (io.clone(), root.clone(), name.clone());
            let (g, t) = (Arc::clone(&packed), Arc::clone(&text));
            let wrote = bounded(&plan.hung, &format!("{KEY}:{root}"), limit, move || {
                write_one(&io, Path::new(&folder), &file, &g, &t)
            })
            .and_then(|r| r);
            match wrote {
                Ok(()) => landed.push(root.clone()),
                Err(e) => failed.push((root.clone(), e)),
            }
        }
        if !landed.is_empty() {
            write_base(&plan.data_dir, &packed);
            base = Some(job.file.clone());
        }
        done.push(Done {
            reason: job.reason,
            name,
            file: job.file.clone(),
            current: job.current,
            landed,
            failed,
        });
    }
    done
}

/// The size of the versions in each folder something landed in, thinned out first when this device prunes.
fn measure<I: SyncIo + Clone + 'static>(
    io: &I,
    plan: &Plan,
    done: &[Done],
    limit: Duration,
) -> Vec<(String, u64)> {
    let roots: BTreeSet<&String> = done.iter().flat_map(|d| &d.landed).collect();
    roots
        .into_iter()
        .filter_map(|root| {
            let (io, folder, now, thin) = (io.clone(), root.clone(), plan.now, plan.prune);
            let device8 = plan.device8.clone();
            bounded(&plan.hung, &format!("{KEY}:{root}"), limit, move || {
                let folder = Path::new(&folder);
                if thin {
                    prune(&io, folder, now, &device8)
                } else {
                    scan(&io, folder).map(|s| s.bytes())
                }
            })
            .ok()?
            .ok()
            .map(|bytes| (root.clone(), bytes))
        })
        .collect()
}

fn run<I: SyncIo + Clone + 'static>(
    svc: &Mutex<Service>,
    io: &I,
    limit: Duration,
    ask: Ask,
) -> Ran {
    let plan = {
        let mut s = lock(svc);
        // A first start waiting for its data has nothing of its own to keep in the folders yet.
        if s.awaiting_cloud() {
            return Ran::default();
        }
        s.plan_versions(ask)
    };
    let Some(plan) = plan else {
        return Ran::default();
    };
    let done = carry_out(io, &plan, limit);
    let sizes = measure(io, &plan, &done, limit);
    lock(svc).versions_done(&plan, done, sizes)
}

/// After each sync, under the cycle lock: writes the version that is due, if any, and thins out the folders when this
/// device holds the lease. The service lock is never held while a folder is touched.
pub fn run_due<I: SyncIo + Clone + 'static>(svc: &Mutex<Service>, io: &I, limit: Duration) {
    run(svc, io, limit, Ask::Due { quitting: false });
}

/// As `run_due`, on quitting: a version is written when data changed here since the last one, and that one is at least
/// 15 minutes old. Whether the lease was released first makes no difference.
pub fn run_at_quit<I: SyncIo + Clone + 'static>(svc: &Mutex<Service>, io: &I, limit: Duration) {
    run(svc, io, limit, Ask::Due { quitting: true });
}

/// "Save a version now": refused while another computer holds the lease, and when no folder could take it. One that
/// failed isn't tried again later, as the page said it failed. A version a take over or a restore asked for, and not yet
/// written, is still written at the next sync.
pub fn save_now<I: SyncIo + Clone + 'static>(
    svc: &Mutex<Service>,
    io: &I,
    limit: Duration,
) -> Result<(), CoreError> {
    let pending = {
        let mut s = lock(svc);
        s.lease_gate()?;
        if s.awaiting_cloud() {
            return Err(CoreError::Conflict(AWAITING_CLOUD.to_owned()));
        }
        if s.version_targets().is_empty() {
            let why = if s.all_folders_waiting() {
                WAITING_FOR_CHOICE
            } else {
                NO_FOLDER
            };
            return Err(CoreError::Conflict(why.to_owned()));
        }
        let pending = s.take_version_due();
        s.request_version(VersionReason::Manual);
        pending
    };
    let ran = run(svc, io, limit, Ask::Due { quitting: false });
    if let Some(reason) = pending.filter(|r| rank(*r) > rank(VersionReason::Manual)) {
        lock(svc).ask_again(reason);
    }
    match ran.landed(VersionReason::Manual) {
        Some(_) => Ok(()),
        None => Err(CoreError::Conflict(format!(
            "No folder could take the version: {}",
            ran.why()
        ))),
    }
}

/// Every folder that holds a copy of a version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionLocation {
    pub root: String,
    pub provider: SyncProvider,
    /// The size of the version file in that folder.
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    /// The file name without its extension. The same name in two folders is one version.
    pub name: String,
    pub taken_at: Ts,
    pub device_name: String,
    /// The reason code from the name.
    pub reason: String,
    /// Kept for a year whatever the retention rule says.
    pub pinned: bool,
    pub locations: Vec<VersionLocation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderError {
    pub root: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionList {
    /// Newest first.
    pub versions: Vec<VersionInfo>,
    /// Folders that couldn't be looked in. The versions of the others are still listed.
    pub errors: Vec<FolderError>,
    /// Of every copy, record included, in every folder looked in.
    pub total_bytes: u64,
    pub over_budget: bool,
}

/// The versions in every sync folder, and in `extra_root` too when given, such as a folder no longer connected. Each
/// folder is looked in on its own with a time limit, and one that can't be costs only its own copies.
pub fn list<I: SyncIo + Clone + 'static>(
    svc: &Mutex<Service>,
    io: &I,
    extra_root: Option<&str>,
    limit: Duration,
) -> VersionList {
    let (targets, hung, now) = {
        let s = lock(svc);
        (
            s.file().prefs.sync_targets.clone(),
            s.hung_targets(),
            s.now(),
        )
    };
    let mut folders = targets.clone();
    if let Some(extra) = extra_root.map(str::trim).filter(|r| !r.is_empty())
        && !folders.iter().any(|t| t.root == extra)
    {
        folders.push(SyncTarget {
            provider: SyncProvider::Custom,
            root: extra.to_owned(),
        });
    }
    let mut found: Vec<(SyncTarget, Scanned)> = Vec::new();
    let mut out = VersionList::default();
    for t in folders {
        let (scan_io, root) = (io.clone(), t.root.clone());
        let scanned = bounded(&hung, &format!("{KEY}:{}", t.root), limit, move || {
            scan(&scan_io, Path::new(&root))
        })
        .and_then(|r| r);
        match scanned {
            Ok(s) => found.push((t, s)),
            Err(message) => out.errors.push(FolderError {
                root: t.root,
                message,
            }),
        }
    }
    let mut s = lock(svc);
    let mut by_name: BTreeMap<String, VersionInfo> = BTreeMap::new();
    for (t, scanned) in &found {
        let bytes = scanned.bytes();
        out.total_bytes += bytes;
        if targets.iter().any(|x| x.root == t.root) {
            s.target_status_mut(&t.root).versions_bytes = Some(bytes);
        }
        for (name, sizes) in &scanned.versions {
            let Some(v) = VersionName::parse(name) else {
                continue;
            };
            by_name
                .entry(name.clone())
                .or_insert_with(|| VersionInfo {
                    name: name.clone(),
                    taken_at: v.taken_at,
                    device_name: s.name_of8(&v.device8),
                    reason: v.reason.word().to_owned(),
                    pinned: v.pinned(now),
                    locations: Vec::new(),
                })
                .locations
                .push(VersionLocation {
                    root: t.root.clone(),
                    provider: t.provider,
                    bytes: sizes.gz,
                });
        }
    }
    drop(s);
    out.versions = by_name.into_values().collect();
    out.versions
        .sort_by(|a, b| (b.taken_at, &b.name).cmp(&(a.taken_at, &a.name)));
    out.over_budget = out.total_bytes > BUDGET_BYTES;
    out
}

/// The plain-text record of what changed in a version, from one folder. Only a version's exact name is read, so no path
/// can lead anywhere else.
pub fn read_changes(io: &dyn SyncIo, root: &Path, name: &str) -> Result<String, CoreError> {
    let v = VersionName::parse(name).ok_or(CoreError::NotFound("version"))?;
    if !io.is_dir(root) {
        return Err(CoreError::Conflict(FOLDER_NOT_FOUND.to_owned()));
    }
    let path = versions_dir(root)
        .join(v.month())
        .join(format!("{name}{TXT}"));
    let size = io
        .file_size(&path)
        .map_err(|e| CoreError::Conflict(format!("Could not read it: {e}")))?;
    if size > MAX_CHANGES_BYTES {
        return Err(CoreError::Conflict(CHANGES_TOO_LARGE.to_owned()));
    }
    let bytes = read_at_most(io, &path, MAX_CHANGES_BYTES).map_err(|e| {
        if e.kind() == io::ErrorKind::FileTooLarge {
            CoreError::Conflict(CHANGES_TOO_LARGE.to_owned())
        } else {
            CoreError::Conflict(format!("Could not read it: {e}"))
        }
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `read_changes` with a time limit, for a folder that may hang.
pub fn read_changes_within<I: SyncIo + Clone + 'static>(
    svc: &Mutex<Service>,
    io: &I,
    root: &str,
    name: &str,
    limit: Duration,
) -> Result<String, CoreError> {
    let hung = lock(svc).hung_targets();
    let (io, folder, name) = (io.clone(), root.to_owned(), name.to_owned());
    bounded(&hung, &format!("{KEY}:{root}"), limit, move || {
        read_changes(&io, Path::new(&folder), &name)
    })
    .map_err(CoreError::Conflict)
    .and_then(|r| r)
}

/// What a restore of a version would bring, against what is here now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePreview {
    pub name: String,
    pub taken_at: Ts,
    pub reason: String,
    pub version: Counts,
    pub current: Counts,
    /// Records that couldn't be read, which a restore leaves out.
    pub warnings: Vec<String>,
    /// Why a restore can't go ahead, or `None` when it can.
    pub blocked_reason: Option<String>,
}

/// Reads one copy of a version and says what restoring it would do. A copy that can't be read is an error, so the page
/// can offer another; one read whole that can't be restored says why instead.
pub fn preview_restore<I: SyncIo + Clone + 'static>(
    svc: &Mutex<Service>,
    io: &I,
    root: &str,
    name: &str,
    limit: Duration,
) -> Result<RestorePreview, CoreError> {
    let v = VersionName::parse(name).ok_or(CoreError::NotFound("version"))?;
    let (hung, now) = {
        let s = lock(svc);
        (s.hung_targets(), s.now())
    };
    let read = read_bounded(io, &hung, root, name, now, limit);
    let s = lock(svc);
    let mut preview = RestorePreview {
        name: name.to_owned(),
        taken_at: v.taken_at,
        reason: v.reason.word().to_owned(),
        version: Counts::default(),
        current: counts(&s.file().data),
        warnings: Vec::new(),
        blocked_reason: None,
    };
    match read {
        Ok(loaded) => {
            preview.version = counts(&loaded.file.data);
            preview.warnings = loaded.warnings;
        }
        Err(ReadFail::Unreadable(why)) => return Err(CoreError::Conflict(why)),
        Err(ReadFail::Future(_)) => preview.blocked_reason = Some(FROM_A_NEWER_APP.to_owned()),
        Err(ReadFail::Invalid(why)) => {
            preview.blocked_reason = Some(format!("This version can't be restored: {why}"));
        }
    }
    if preview.blocked_reason.is_none() {
        if s.read_only() {
            preview.blocked_reason = Some(TAKE_OVER_FIRST.to_owned());
        } else if s.all_folders_waiting() {
            preview.blocked_reason = Some(WAITING_FOR_CHOICE.to_owned());
        }
    }
    Ok(preview)
}

/// The highest restore any folder's device files follow, so that this one goes above it.
fn highest_epoch<I: SyncIo + Clone + 'static>(
    io: &I,
    hung: &HungTargets,
    targets: &[SyncTarget],
    own_id: &str,
    now: Ts,
    limit: Duration,
) -> u64 {
    targets
        .iter()
        .filter_map(|t| {
            let (io, root, own) = (io.clone(), t.root.clone(), own_id.to_owned());
            bounded(hung, &format!("{KEY}:{}", t.root), limit, move || {
                read_target(&io, Path::new(&root), &own, now)
            })
            .ok()
        })
        .flat_map(|read| {
            read.own
                .into_iter()
                .chain(read.files)
                .filter_map(|f| f.epoch_info.map(|e| e.epoch))
                .collect::<Vec<_>>()
        })
        .max()
        .unwrap_or(0)
}

impl Service {
    /// The highest restore a restore of `file` has to go above: any in the folders, this device's, and the version's.
    fn restore_top(&self, above: u64, file: &DeviceFile) -> u64 {
        above
            .max(EpochInfo::key(self.file.prefs.epoch.as_ref()).0)
            .max(EpochInfo::key(file.epoch_info.as_ref()).0)
    }

    /// Installs a version read whole, as a restore every device follows: one save holds the data and the new epoch, so a
    /// crash can't leave either without the other. A timer running here stops first, and a local safety copy keeps what
    /// was here, with any edit made while the folders were written.
    pub(crate) fn restore_version(
        &mut self,
        file: DeviceFile,
        name: &str,
        taken_at: Ts,
        above: u64,
        pre_restore: Option<String>,
    ) -> Result<(), CoreError> {
        self.lease_gate()?;
        let number = epoch_above(self.restore_top(above, &file))?;
        self.on_exit()?;
        let now = self.now();
        let safety = backup::safety_copy(&self.paths, &self.file, now)?;
        let epoch = EpochInfo {
            epoch: number,
            by: self.file.device_id.clone(),
            at: now,
            restored_from: name.to_owned(),
            base_taken_at: taken_at,
        };
        let last_heartbeat = self.file.last_heartbeat;
        let stopped = self.install_restored(
            file.data,
            RestoreNote {
                entity: "version",
                name: name.to_owned(),
                after: json!({"safetyCopy": safety, "preRestore": pre_restore, "epoch": number}),
                // A task the version held running here stopped being timed when it was taken.
                closed_at: taken_at,
                reason: PauseReason::BackupRestored,
                last_heartbeat,
                epoch: Some(epoch),
            },
        )?;
        if self.lease.active() {
            self.lease.note_change(now);
        }
        self.request_version(VersionReason::Restored);
        let when = self.when_text(taken_at);
        let kept = match &pre_restore {
            Some(pre) => {
                let at = VersionName::parse(pre).map_or(now, |v| v.taken_at);
                format!(
                    "The data from just before is kept as {safety}, and as the version of {} in your sync folders, so this can be undone by restoring that.",
                    self.when_text(at)
                )
            }
            None => format!("The data from just before is kept as {safety}."),
        };
        let others = if self.file.prefs.sync_targets.is_empty() {
            ""
        } else {
            " Other computers that sync with these folders follow it at their next sync. Changes from a computer on an older app version are skipped until it is updated."
        };
        self.notify(
            "info",
            format!("Restored the version from {when}.{others} {kept}"),
        );
        if !stopped.is_empty() {
            self.notify(
                "info",
                format!(
                    "{} was running when this version was taken. It is paused at the moment the version was taken ({when}).",
                    stopped.join(", ")
                ),
            );
        }
        Ok(())
    }
}

/// Restores a version for every computer that syncs with these folders. Run under the cycle lock for the whole of it,
/// and then sync at once. Reads the copy in `root`, else another folder's; saves a copy of the data as it is now in every
/// folder that takes it, and refuses when folders exist and none does; then installs it at an epoch above any found.
pub fn restore<I: SyncIo + Clone + 'static>(
    svc: &Mutex<Service>,
    io: &I,
    root: &str,
    name: &str,
    limit: Duration,
) -> Result<(), CoreError> {
    let v = VersionName::parse(name).ok_or(CoreError::NotFound("version"))?;
    let (targets, hung, now, own_id) = {
        let s = lock(svc);
        s.lease_gate()?;
        // A first start has no data of its own here yet, so none is copied first (`run`), and what it has is in the
        // folders, below the restore it would make: it waits for its data first.
        if s.awaiting_cloud() {
            return Err(CoreError::Conflict(AWAITING_RESTORE.to_owned()));
        }
        // The copy of today's data that comes first could go nowhere.
        if s.all_folders_waiting() {
            return Err(CoreError::Conflict(format!(
                "{WAITING_FOR_CHOICE} Nothing was changed."
            )));
        }
        (
            s.file().prefs.sync_targets.clone(),
            s.hung_targets(),
            s.now(),
            s.file().device_id.clone(),
        )
    };
    let mut sources = vec![root.to_owned()];
    sources.extend(targets.iter().map(|t| t.root.clone()).filter(|r| r != root));
    let mut loaded = None;
    let mut first_failure = None;
    for source in &sources {
        match read_bounded(io, &hung, source, name, now, limit) {
            Ok(l) => {
                loaded = Some(l);
                break;
            }
            Err(ReadFail::Future(found)) => {
                return Err(CoreError::FutureVersion {
                    found,
                    supported: SCHEMA_VERSION,
                });
            }
            Err(ReadFail::Invalid(why)) => {
                return Err(CoreError::Conflict(format!(
                    "This version can't be restored: {why}"
                )));
            }
            Err(ReadFail::Unreadable(why)) => {
                first_failure.get_or_insert(why);
            }
        }
    }
    let Some(loaded) = loaded else {
        return Err(CoreError::Conflict(format!(
            "No copy of this version could be read: {}",
            first_failure.unwrap_or_default()
        )));
    };
    let above = highest_epoch(io, &hung, &targets, &own_id, now, limit);
    // A restore no reader would take is refused before the copy of today's data is written.
    epoch_above(lock(svc).restore_top(above, &loaded.file))?;
    let ran = run(svc, io, limit, Ask::Now(VersionReason::PreRestore));
    let pre_restore = ran.landed(VersionReason::PreRestore).map(str::to_owned);
    if !targets.is_empty() && pre_restore.is_none() {
        return Err(CoreError::Conflict(NO_PRE_RESTORE.to_owned()));
    }
    lock(svc).restore_version(loaded.file, name, v.taken_at, above, pre_restore)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const DEVICE: &str = "a1b2c3d4-0000-4000-8000-000000000000";

    fn at(iso: &str) -> Ts {
        Ts::parse(iso).unwrap()
    }

    fn name(taken: Ts, reason: VersionReason) -> String {
        version_name(taken, DEVICE, reason)
    }

    #[test]
    fn a_name_is_written_and_read_in_exactly_one_form() {
        let t = at("2026-11-02T14:25:30Z");
        let n = name(t, VersionReason::PreRestore);
        assert_eq!(n, "20261102T142530Z--a1b2c3d4--pre-restore");
        let v = VersionName::parse(&n).unwrap();
        assert_eq!(
            (v.taken_at, v.device8.as_str(), v.reason, v.month().as_str()),
            (t, "a1b2c3d4", VersionReason::PreRestore, "2026-11")
        );
        for reason in REASONS {
            let n = name(t, reason);
            assert_eq!(VersionName::parse(&n).unwrap().reason, reason, "{n}");
        }
        for bad in [
            "20261102T142530Z--a1b2c3d4--auto (1)",
            "20261102T142530Z--a1b2c3d4--auto-PC2",
            "20261102T142530Z--a1b2c3d4--sometime",
            "20261102T142530Z--A1B2C3D4--auto",
            "20261102T142530Z--a1b2c3d--auto",
            "20261302T142530Z--a1b2c3d4--auto",
            "20261102T246030Z--a1b2c3d4--auto",
            "20261102 142530Z--a1b2c3d4--auto",
            "2026110T2142530Z--a1b2c3d4--auto",
            "../../20261102T142530Z--a1b2c3d4--auto",
            "20261102T142530Z--a1b2c3d4--auto/../x",
            "",
        ] {
            assert_eq!(VersionName::parse(bad), None, "{bad}");
        }
        assert!(is_month("2026-11") && !is_month("2026-13") && !is_month("2026-1x"));
    }

    fn day(n: i64) -> i64 {
        n * DAY_SECS
    }

    /// One automatic version each hour for 400 days.
    fn hourly(now: Ts, days: i64) -> Vec<String> {
        (0..days * 24)
            .map(|h| name(now.later(-h * 3600), VersionReason::Auto))
            .collect()
    }

    /// Test 42, the tiers.
    #[test]
    fn keep_retains_tiers_and_is_idempotent() {
        let now = at("2026-11-02T12:30:00Z");
        let names = hourly(now, 400);
        let kept = keep(&names, now);
        let ages: Vec<i64> = kept
            .iter()
            .map(|n| now.secs() - VersionName::parse(n).unwrap().taken_at.secs())
            .collect();
        let under = |secs: i64| ages.iter().filter(|a| **a < secs).count();
        assert_eq!(under(day(7)), 7 * 24, "all of the last week");
        // From 7 to 60 days, one a day: each UTC day's last hour.
        let daily = under(day(60)) - under(day(7));
        assert!((53..=54).contains(&daily), "{daily} a day");
        // Then one a month, the newest of each UTC month: 2025-09 to 2026-08 here. September 2026's is its last day,
        // still kept as a daily one.
        let monthly: BTreeSet<String> = kept
            .iter()
            .filter(|n| now.secs() - VersionName::parse(n).unwrap().taken_at.secs() >= day(60))
            .map(|n| VersionName::parse(n).unwrap().month())
            .collect();
        assert_eq!(monthly.len(), kept.len() - under(day(60)), "one per month");
        assert!(monthly.contains("2025-10") && monthly.contains("2026-08"));
        assert_eq!(keep(&kept.iter().cloned().collect::<Vec<_>>(), now), kept);
        // Each month's survivor is its newest: 2025-10's is its last hour.
        assert!(kept.contains(&name(at("2025-10-31T23:30:00Z"), VersionReason::Auto)));
    }

    /// Test 43.
    #[test]
    fn pinned_reasons_kept_a_year() {
        let now = at("2026-11-02T12:00:00Z");
        let old = |days: i64, hour: i64, reason| name(now.later(-day(days) - hour * 3600), reason);
        let names = vec![
            // Two months ago, a restore and what was around it, then an ordinary copy later that day.
            old(70, 3, VersionReason::PreRestore),
            old(70, 2, VersionReason::Restored),
            old(70, 1, VersionReason::TakeOver),
            old(70, 0, VersionReason::Auto),
            // A year and a day ago, in the middle of its month: a pinned one is now ordinary.
            old(366, 3, VersionReason::PreAdopt),
            old(366, 2, VersionReason::Manual),
            // The newest of that month.
            name(at("2025-11-20T20:00:00Z"), VersionReason::Quit),
        ];
        let kept = keep(&names, now);
        for n in &names[..4] {
            assert!(kept.contains(n), "{n}");
        }
        assert!(!kept.contains(&names[4]), "a year on, treated as ordinary");
        assert!(!kept.contains(&names[5]));
        assert!(kept.contains(&names[6]));
        // At 364 days the same one is still kept whole.
        let earlier = now.later(-day(2));
        assert!(keep(&names, earlier).contains(&names[4]));
    }

    #[test]
    fn a_name_dated_ahead_counts_as_new_and_one_the_rule_does_not_know_is_kept() {
        let now = at("2026-11-02T12:00:00Z");
        let ahead = name(now.later(day(3)), VersionReason::Auto);
        let strange = "20200101T000000Z--a1b2c3d4--auto (1)".to_owned();
        let old_twin = name(at("2020-01-01T00:00:00Z"), VersionReason::Auto);
        let older = name(
            at("2019-12-31T23:00:00Z").later(-day(40)),
            VersionReason::Auto,
        );
        let names = vec![ahead.clone(), strange.clone(), old_twin.clone(), older];
        let kept = keep(&names, now);
        assert!(kept.contains(&ahead) && kept.contains(&strange) && kept.contains(&old_twin));
        assert_eq!(kept.len(), 4, "each is the newest of its month");
    }

    /// Audit SEC-3: a record of changes that grew after its size was taken is refused as too large, whatever its size
    /// said, rather than read whole.
    #[test]
    fn a_record_of_changes_longer_than_its_size_said_is_refused() {
        struct Grew;
        impl SyncIo for Grew {
            fn is_dir(&self, _: &Path) -> bool {
                true
            }
            fn create_dir_all(&self, _: &Path) -> io::Result<()> {
                Ok(())
            }
            fn list(&self, _: &Path) -> io::Result<Vec<String>> {
                Ok(Vec::new())
            }
            fn file_size(&self, _: &Path) -> io::Result<u64> {
                Ok(10)
            }
            fn read(&self, _: &Path, _: u64) -> io::Result<Vec<u8>> {
                Ok(vec![b'x'; usize::try_from(MAX_CHANGES_BYTES).unwrap() + 1])
            }
            fn write_atomic(&self, _: &Path, _: &Path, _: &[u8]) -> io::Result<()> {
                Ok(())
            }
            fn sleep(&self, _: Duration) {}
        }
        let n = name(at("2026-11-02T14:25:30Z"), VersionReason::Auto);
        // Only its length, if it is read: the text itself would fill the test's output.
        let refused = read_changes(&Grew, Path::new("/cloud"), &n)
            .map(|text| text.len())
            .unwrap_err();
        assert_eq!(
            refused.to_string(),
            "The record of changes is unreasonably large."
        );
    }

    fn reason() -> impl Strategy<Value = VersionReason> {
        proptest::sample::select(REASONS.to_vec())
    }

    proptest! {
        /// Test 42: keeping again keeps the same, the newest always stays, nothing under 7 days or pinned goes, and the
        /// order names come in changes nothing.
        #[test]
        fn keep_is_idempotent_and_never_drops_the_newest(
            versions in proptest::collection::vec((-3 * DAY_SECS..800 * DAY_SECS, reason()), 1..80),
            seed in any::<u64>(),
        ) {
            let now = at("2026-11-02T12:00:00Z");
            let names: Vec<String> = versions
                .iter()
                .map(|(age, r)| name(now.later(-age), *r))
                .collect();
            let kept = keep(&names, now);
            prop_assert!(kept.iter().all(|n| names.contains(n)));
            let again: Vec<String> = kept.iter().cloned().collect();
            prop_assert_eq!(&keep(&again, now), &kept);
            let newest = names
                .iter()
                .max_by_key(|n| (VersionName::parse(n).unwrap().taken_at, (*n).clone()))
                .unwrap();
            prop_assert!(kept.contains(newest));
            for n in &names {
                let v = VersionName::parse(n).unwrap();
                let age = now.secs() - v.taken_at.secs();
                if age < KEEP_ALL_SECS || v.pinned(now) {
                    prop_assert!(kept.contains(n), "{} dropped", n);
                }
            }
            let mut shuffled = names.clone();
            let len = shuffled.len();
            shuffled.rotate_left(usize::try_from(seed % len as u64).unwrap());
            shuffled.reverse();
            prop_assert_eq!(&keep(&shuffled, now), &kept);
            // Every month that had a version still has one.
            let months = |set: &mut dyn Iterator<Item = &String>| -> BTreeSet<String> {
                set.map(|n| VersionName::parse(n).unwrap().month()).collect()
            };
            prop_assert_eq!(months(&mut names.iter()), months(&mut kept.iter()));
        }
    }
}
