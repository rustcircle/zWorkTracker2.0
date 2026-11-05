//! One computer at a time, as far as plain cloud folders allow. This is the pure part: the record each device
//! writes to `lease/<device-uuid>.json`, how a reader takes one, and what a set of them means. Reading and writing
//! the files is `lease_io`. Every file has one writer, so conflict copies don't arise, and the order of claims is
//! decided by the data rather than by whichever upload landed last. This only decides who stops writing: device
//! files stay mergeable whatever the generation.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::model::LeaseMemo;
use crate::time::Ts;
use crate::validate::{MAX_NAME, check_plain};

pub const LEASE_FOLDER: &str = "lease";
pub const LEASE_VERSION: u32 = 1;
pub const MAX_LEASE_FILE_BYTES: usize = 64 * 1024;
pub const MAX_GENERATION: u64 = 1 << 40;
pub const MAX_OPEN_SEGMENTS: usize = 64;

/// How often the holder rewrites its record. The file is small, but every write is an upload: this is 240 writes
/// per 8-hour day per folder, where the local heartbeat's 30 s would be 960.
pub const LEASE_BEAT_SECS: i64 = 120;
/// How often the records are read, which is a local read of tiny files. It bounds how long the old holder takes to
/// find out to this plus the download.
pub const LEASE_READ_SECS: i64 = 60;
/// How long with no change seen before a holder looks inactive, in minutes: one beat 2 + upload 5 + one missed beat
/// 2 + download 5 + margin 1. The 5 minutes of propagation are an assumption. This only unlocks wording, and nothing
/// happens automatically.
pub const LEASE_STALE_SECS: i64 = 900;
/// A holder isn't called inactive from its timestamp alone until this device has watched long enough to see at
/// least one beat. This covers a skewed clock at launch.
pub const LEASE_MIN_WATCH_SECS: i64 = 180;
/// No timer running and no data change for this long: the lease is released.
pub const LEASE_IDLE_RELEASE_SECS: i64 = 900;
/// The limit per folder in `bounded`, under the key `lease:<root>`, so a stuck sync never reads as a stuck lease.
pub const LEASE_IO_LIMIT: Duration = Duration::from_secs(10);
/// How long a device has to say nothing before the time it left running can be closed from another with no pause
/// asked for first.
pub const LOST_DEVICE_SILENCE_SECS: i64 = 86_400;
/// Past this a last known holder no longer makes a folder that can't be reached read-only.
const MEMO_TRUSTED_SECS: i64 = 86_400;
/// A beat this far ahead of this computer's clock can only be that other clock.
pub(crate) const CLOCK_AHEAD_SECS: i64 = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LeaseRecordState {
    Held,
    Released,
}

/// What one device says about itself. Compact JSON, written only by that device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRecord {
    pub lease_version: u32,
    pub device_id: String,
    pub device_name: String,
    #[serde(default)]
    pub app_version: String,
    pub generation: u64,
    pub state: LeaseRecordState,
    pub acquired_at: Ts,
    pub beat_at: Ts,
    pub beat_seq: u64,
    /// This device's open segments at `beat_at`. Says whether the beat is evidence that a timer ran, not just the app.
    #[serde(default)]
    pub open_segments: Vec<String>,
}

/// A record read from a file called `file_name`, or `None` for one a reader ignores, and counts as ignored. A
/// conflict copy has another name, so it is one of those. Never a sign of damage: it may still be syncing.
#[must_use]
pub fn parse_record(file_name: &str, bytes: &[u8]) -> Option<LeaseRecord> {
    if bytes.len() > MAX_LEASE_FILE_BYTES {
        return None;
    }
    let id = crate::sync::device_id_from_name(file_name)?;
    let record: LeaseRecord = serde_json::from_slice(bytes).ok()?;
    let takes = record.device_id == id
        && record.lease_version <= LEASE_VERSION
        && record.generation <= MAX_GENERATION
        && record.open_segments.len() <= MAX_OPEN_SEGMENTS
        && check_plain(&record.device_name, MAX_NAME, "a device name").is_ok();
    takes.then_some(record)
}

/// The record that counts for each device, as one can be stale in one folder and fresh in another. What sorts after
/// the highest `(generation, beat_seq)` only keeps this the same whichever order the folders were read in.
#[must_use]
pub fn freshest(records: &[LeaseRecord]) -> Vec<LeaseRecord> {
    let key = |r: &LeaseRecord| {
        (
            r.generation,
            r.beat_seq,
            r.state == LeaseRecordState::Released,
            r.beat_at,
            r.acquired_at,
        )
    };
    let tail = |r: &LeaseRecord| (r.device_name.clone(), r.app_version.clone());
    let mut best: BTreeMap<&str, &LeaseRecord> = BTreeMap::new();
    for r in records {
        best.entry(r.device_id.as_str())
            .and_modify(|b| {
                if key(r)
                    .cmp(&key(b))
                    .then_with(|| tail(r).cmp(&tail(b)))
                    .then_with(|| r.open_segments.cmp(&b.open_segments))
                    == Ordering::Greater
                {
                    *b = r;
                }
            })
            .or_insert(r);
    }
    best.into_values().cloned().collect()
}

/// The order of claims: highest generation, then Held before Released, then lowest device id.
fn outranks(a: &LeaseRecord, b: &LeaseRecord) -> Ordering {
    a.generation
        .cmp(&b.generation)
        .then_with(|| (a.state == LeaseRecordState::Held).cmp(&(b.state == LeaseRecordState::Held)))
        .then_with(|| b.device_id.cmp(&a.device_id))
}

/// The record that decides who holds, among those that count.
fn top(records: &[LeaseRecord]) -> Option<&LeaseRecord> {
    records.iter().max_by(|a, b| outranks(a, b))
}

/// This device's own claim, in memory. It overrides its own file, which a cloud client can put back to an old copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub generation: u64,
    pub acquired_at: Ts,
    pub beat_seq: u64,
}

impl Claim {
    fn as_record(&self, me: &str, beat_at: Ts) -> LeaseRecord {
        LeaseRecord {
            lease_version: LEASE_VERSION,
            device_id: me.to_owned(),
            device_name: String::new(),
            app_version: String::new(),
            generation: self.generation,
            state: LeaseRecordState::Held,
            acquired_at: self.acquired_at,
            beat_at,
            beat_seq: self.beat_seq,
            open_segments: Vec::new(),
        }
    }
}

/// The generation just above `generation`, while a reader still takes a record with it. A record no reader takes would
/// leave this device the only one that knows of its claim.
#[must_use]
pub fn above(generation: u64) -> Option<u64> {
    generation
        .checked_add(1)
        .filter(|next| *next <= MAX_GENERATION)
}

/// A new claim outranks everything known, including what has since vanished from the folders. It never comes from a
/// clock. `None` when a record is already at `MAX_GENERATION`, which only a forged one can be. A memo that high is
/// left out, so that once such a record is deleted, claims work again.
#[must_use]
pub fn next_generation(
    records: &[LeaseRecord],
    memo: Option<&LeaseMemo>,
    claim: Option<&Claim>,
) -> Option<u64> {
    let top = records
        .iter()
        .map(|r| r.generation)
        .chain(memo.map(|m| m.generation).filter(|g| *g < MAX_GENERATION))
        .chain(claim.map(|c| c.generation))
        .max()
        .unwrap_or(0);
    above(top)
}

/// The device whose record is at `MAX_GENERATION`, which no claim can go above, for saying which file is in the way.
#[must_use]
pub fn at_ceiling(records: &[LeaseRecord]) -> Option<&str> {
    records
        .iter()
        .filter(|r| r.generation >= MAX_GENERATION)
        .map(|r| r.device_id.as_str())
        .min()
}

/// What this computer has seen of another device's record. In memory only, and on this computer's clock, so that
/// a skewed clock over there can't make the device look alive or dead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    pub seq: u64,
    pub generation: u64,
    pub beat_at: Ts,
    pub first_seen_at: Ts,
    pub changed_at: Ts,
    /// It changed during this run, so it was certainly alive then.
    pub moved: bool,
}

impl Observed {
    fn first(r: &LeaseRecord, now: Ts) -> Self {
        Self {
            seq: r.beat_seq,
            generation: r.generation,
            beat_at: r.beat_at,
            first_seen_at: now,
            changed_at: now,
            moved: false,
        }
    }

    fn saw(&mut self, r: &LeaseRecord, now: Ts) {
        if (self.generation, self.seq, self.beat_at) != (r.generation, r.beat_seq, r.beat_at) {
            self.generation = r.generation;
            self.seq = r.beat_seq;
            self.beat_at = r.beat_at;
            self.changed_at = now;
            self.moved = true;
        }
    }

    /// Whether the device looks in use. Immune to clock skew: the stamp on a beat is only believed while this
    /// computer hasn't seen a change for itself, and not for long.
    #[must_use]
    pub fn is_live(&self, now: Ts) -> bool {
        let since = |t: Ts| now.secs() - t.secs();
        if self.moved {
            since(self.changed_at) < LEASE_STALE_SECS
        } else {
            since(self.first_seen_at) < LEASE_MIN_WATCH_SECS
                || (since(self.beat_at) < LEASE_STALE_SECS
                    && since(self.first_seen_at) < LEASE_STALE_SECS)
        }
    }

    /// Its clock seems to differ from this one. Once it has moved, its stamp is set against the moment this computer
    /// saw the change, so a device that only went quiet afterwards isn't called skewed.
    #[must_use]
    pub fn clock_suspect(&self, now: Ts) -> bool {
        let at = if self.moved { self.changed_at } else { now };
        let behind = at.secs() - self.beat_at.secs();
        (self.moved && behind.abs() > LEASE_STALE_SECS) || -behind > CLOCK_AHEAD_SECS
    }
}

/// Notes what a read found. Every change of `(generation, beat_seq, beat_at)` counts, so that a holder resuming from
/// a lower copy in another folder still reads as alive.
pub fn observe(seen: &mut BTreeMap<String, Observed>, records: &[LeaseRecord], now: Ts) {
    for r in freshest(records) {
        seen.entry(r.device_id.clone())
            .and_modify(|o| o.saw(&r, now))
            .or_insert_with(|| Observed::first(&r, now));
    }
}

/// Who another device is, as far as the records say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HolderInfo {
    pub device_id: String,
    pub device_name: String,
    pub generation: u64,
    pub beat_seq: u64,
    /// The stamp on its last beat, on its own clock.
    pub beat_at: Ts,
    /// The last moment this computer can vouch for. Its own clock's, when the other one's can't be believed.
    pub last_sign_at: Ts,
    pub clock_suspect: bool,
    /// The segments it said it was timing at its last beat.
    pub open_segments: Vec<String>,
}

impl HolderInfo {
    fn of(r: &LeaseRecord, seen: &Observed, now: Ts) -> Self {
        let clock_suspect = seen.clock_suspect(now);
        Self {
            device_id: r.device_id.clone(),
            device_name: r.device_name.clone(),
            generation: r.generation,
            beat_seq: r.beat_seq,
            beat_at: r.beat_at,
            last_sign_at: if clock_suspect {
                seen.changed_at
            } else {
                r.beat_at
            },
            clock_suspect,
            open_segments: r.open_segments.clone(),
        }
    }
}

/// Why this device has to stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LostWhy {
    /// Another claim has a higher generation than this device's.
    Overtaken,
    /// Two claims have the same generation, and the other device has the lower id. This device writes `released`.
    LostTie,
    /// It started a timer or made a change before it saw the holder, and holds no claim.
    NoClaim,
}

/// What a set of records comes to for this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Evaluation {
    /// No folder configured.
    Off,
    /// No folder reachable.
    Unknown,
    /// Nobody holds. With `drop_claim`, this device's claim is over.
    Free { drop_claim: bool },
    /// Another device released at or above this device's claim while it is still working: it claims this.
    Reclaim { generation: u64 },
    /// With `contended`, another record has the same generation and loses the tie. The next write bumps past it.
    HeldByMe { contended: bool },
    /// Another device holds, and this device has to stop, see `Service::on_lease_lost`.
    Lost {
        by: HolderInfo,
        why: LostWhy,
        live: bool,
    },
    /// Another device holds, and this device has nothing under way.
    HeldByOther { holder: HolderInfo, live: bool },
}

#[derive(Clone, Copy, Debug)]
pub struct Inputs<'a> {
    pub me: &'a str,
    /// Any folder is configured.
    pub configured: bool,
    /// Any folder could be read.
    pub reachable: bool,
    /// All readable records across the folders that could be read.
    pub records: &'a [LeaseRecord],
    pub claim: Option<&'a Claim>,
    pub want_claim: bool,
    /// This device has a timer running.
    pub open_own_segment: bool,
    pub last_change_at: Option<Ts>,
    pub observed: &'a BTreeMap<String, Observed>,
    pub memo: Option<&'a LeaseMemo>,
    pub now: Ts,
}

/// Decides what the records mean for this device. Pure, and independent of the order of the records.
#[must_use]
pub fn evaluate(i: &Inputs<'_>) -> Evaluation {
    if !i.configured {
        return Evaluation::Off;
    }
    if !i.reachable {
        return Evaluation::Unknown;
    }
    let mut records = freshest(i.records);
    if let Some(claim) = i.claim {
        records.retain(|r| r.device_id != i.me);
        records.push(claim.as_record(i.me, i.now));
    }
    let Some(top) = top(&records) else {
        return Evaluation::Free { drop_claim: false };
    };
    let mine = top.device_id == i.me;
    match (mine, top.state) {
        (true, LeaseRecordState::Released) => Evaluation::Free { drop_claim: false },
        (true, LeaseRecordState::Held) => Evaluation::HeldByMe {
            contended: records.iter().any(|r| {
                r.device_id != i.me
                    && r.generation == top.generation
                    && r.state == LeaseRecordState::Held
            }),
        },
        (false, LeaseRecordState::Released) => {
            if i.claim.is_none() {
                return Evaluation::Free { drop_claim: false };
            }
            let working = i.open_own_segment
                || i.last_change_at
                    .is_some_and(|at| i.now.secs() - at.secs() < LEASE_IDLE_RELEASE_SECS);
            match next_generation(&records, i.memo, i.claim) {
                Some(generation) if working => Evaluation::Reclaim { generation },
                // Not working, or nothing can go above what was released: this device's claim is over.
                _ => Evaluation::Free { drop_claim: true },
            }
        }
        (false, LeaseRecordState::Held) => {
            let seen = i
                .observed
                .get(&top.device_id)
                .cloned()
                .unwrap_or_else(|| Observed::first(top, i.now));
            let holder = HolderInfo::of(top, &seen, i.now);
            let live = seen.is_live(i.now);
            match i.claim {
                Some(c) if c.generation == top.generation => Evaluation::Lost {
                    by: holder,
                    why: LostWhy::LostTie,
                    live,
                },
                Some(_) => Evaluation::Lost {
                    by: holder,
                    why: LostWhy::Overtaken,
                    live,
                },
                None if i.want_claim || i.open_own_segment => Evaluation::Lost {
                    by: holder,
                    why: LostWhy::NoClaim,
                    live,
                },
                None => Evaluation::HeldByOther { holder, live },
            }
        }
    }
}

/// The states the page sees. Two records at one generation stay internal: the winner shows as `HeldByMe`, and the
/// loser demotes at once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LeaseStateName {
    /// No folder. No lease, no files.
    #[default]
    Off,
    /// From opening until the first read comes back. Never blocks a launch.
    Checking,
    Free,
    HeldByMe,
    HeldByOtherLive,
    HeldByOtherStale,
    /// No folder could be read. Writable or not by what is known of the last holder, see `LeaseState::read_only`.
    Unknown,
}

/// The lease as this run knows it. Never saved, apart from `LocalPrefs::lease_memo`.
#[derive(Clone, Debug, Default)]
pub struct LeaseState {
    pub state: LeaseStateName,
    pub claim: Option<Claim>,
    /// A data change happened with no claim yet, so the next read claims or finds out it can't.
    pub want_claim: bool,
    /// The other device that holds, while one does.
    pub holder: Option<HolderInfo>,
    pub observed: BTreeMap<String, Observed>,
    /// The last change to replicated data made on this computer in this run. Keeps a claim from going idle.
    pub last_change_at: Option<Ts>,
    /// "Work here anyway": the generation it was chosen against, until a folder comes back.
    pub offline_claim: Option<u64>,
    /// Quitting: nothing may write a Held record any more.
    pub closing: bool,
    /// Extra words for the page, such as why a take over was dropped.
    pub note: Option<String>,
    /// When the records were last read, and this device's own last written, for the cadence of each.
    pub last_read_at: Option<Ts>,
    pub last_beat_at: Option<Ts>,
    pub reachable_roots: Vec<String>,
    pub unreachable_roots: Vec<String>,
}

impl LeaseState {
    #[must_use]
    pub fn new(has_targets: bool) -> Self {
        Self {
            state: if has_targets {
                LeaseStateName::Checking
            } else {
                LeaseStateName::Off
            },
            ..Self::default()
        }
    }

    /// False with no folder, so a save that can't be refused costs no comparison of the whole data.
    #[must_use]
    pub fn active(&self) -> bool {
        self.state != LeaseStateName::Off
    }

    /// The folders changed. With none left the claim and all that was seen are forgotten. With the first, reading
    /// starts.
    pub fn targets_changed(&mut self, has_targets: bool) {
        if !has_targets {
            *self = Self::new(false);
        } else if !self.active() {
            self.state = LeaseStateName::Checking;
        }
    }

    /// Changes to replicated data are refused. With no folder to read, that goes by the last holder known: another
    /// device within a day means read-only until the user says otherwise. Sync's own contract is that nothing blocks
    /// time logging, so anything else stays writable.
    #[must_use]
    pub fn read_only(&self, memo: Option<&LeaseMemo>, me: &str, now: Ts) -> bool {
        match self.state {
            LeaseStateName::HeldByOtherLive | LeaseStateName::HeldByOtherStale => true,
            LeaseStateName::Unknown => {
                self.offline_claim.is_none()
                    && memo.is_some_and(|m| {
                        m.device_id != me && now.secs() - m.at.secs() < MEMO_TRUSTED_SECS
                    })
            }
            _ => false,
        }
    }

    /// The other device to name: the one seen holding, or else the one last known to.
    #[must_use]
    pub fn holder_name(&self, memo: Option<&LeaseMemo>) -> String {
        self.holder
            .as_ref()
            .map(|h| h.device_name.clone())
            .or_else(|| memo.map(|m| m.device_name.clone()))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Another computer".to_owned())
    }

    /// `Err(ReadOnly)` while changes to replicated data are refused.
    pub fn gate(&self, memo: Option<&LeaseMemo>, me: &str, now: Ts) -> Result<(), CoreError> {
        if self.read_only(memo, me, now) {
            return Err(CoreError::ReadOnly {
                holder: self.holder_name(memo),
            });
        }
        Ok(())
    }

    /// A change to replicated data was made here: the idle clock starts again, and it needs a claim if it has none.
    pub fn note_change(&mut self, now: Ts) {
        self.last_change_at = Some(now);
        if self.claim.is_none() {
            self.want_claim = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const ME: &str = "m0000000-0000-4000-8000-000000000000";
    const LAPTOP: &str = "b0000000-0000-4000-8000-000000000000";
    const T: i64 = 1_800_000_000;

    fn rec(id: &str, generation: u64, state: LeaseRecordState, seq: u64) -> LeaseRecord {
        LeaseRecord {
            lease_version: 1,
            device_id: id.to_owned(),
            device_name: format!("pc-{}", &id[..1]),
            app_version: "1.3.1".to_owned(),
            generation,
            state,
            acquired_at: Ts(T),
            beat_at: Ts(T),
            beat_seq: seq,
            open_segments: Vec::new(),
        }
    }
    fn held(id: &str, generation: u64) -> LeaseRecord {
        rec(id, generation, LeaseRecordState::Held, 1)
    }
    fn released(id: &str, generation: u64) -> LeaseRecord {
        rec(id, generation, LeaseRecordState::Released, 1)
    }
    fn claim(generation: u64) -> Claim {
        Claim {
            generation,
            acquired_at: Ts(T),
            beat_seq: 1,
        }
    }

    /// What a device with nothing under way sees.
    fn at_rest<'a>(
        records: &'a [LeaseRecord],
        observed: &'a BTreeMap<String, Observed>,
    ) -> Inputs<'a> {
        Inputs {
            me: ME,
            configured: true,
            reachable: true,
            records,
            claim: None,
            want_claim: false,
            open_own_segment: false,
            last_change_at: None,
            observed,
            memo: None,
            now: Ts(T),
        }
    }
    fn none() -> BTreeMap<String, Observed> {
        BTreeMap::new()
    }

    /// A record as watched from `now`: first seen when this computer started looking, changed at `changed`.
    fn watched(
        r: &LeaseRecord,
        first_seen: i64,
        changed: Option<i64>,
    ) -> BTreeMap<String, Observed> {
        let mut seen = BTreeMap::new();
        observe(&mut seen, std::slice::from_ref(r), Ts(first_seen));
        if let Some(at) = changed {
            let o = seen.get_mut(&r.device_id).unwrap();
            o.changed_at = Ts(at);
            o.moved = true;
        }
        seen
    }

    fn device_id() -> impl Strategy<Value = String> {
        (0u8..4).prop_map(|n| {
            if n == 0 {
                ME.to_owned()
            } else {
                format!("{n}0000000-0000-4000-8000-000000000000")
            }
        })
    }

    fn record() -> impl Strategy<Value = LeaseRecord> {
        (
            device_id(),
            0u64..4,
            any::<bool>(),
            0u64..4,
            0i64..3,
            any::<bool>(),
        )
            .prop_map(|(id, generation, is_held, seq, beat, timer)| LeaseRecord {
                state: if is_held {
                    LeaseRecordState::Held
                } else {
                    LeaseRecordState::Released
                },
                beat_at: Ts(T + beat),
                open_segments: if timer {
                    vec!["s".to_owned()]
                } else {
                    Vec::new()
                },
                ..rec(&id, generation, LeaseRecordState::Held, seq)
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn evaluate_is_independent_of_record_order(
            (records, shuffled) in proptest::collection::vec(record(), 0..10)
                .prop_flat_map(|v| (Just(v.clone()), Just(v).prop_shuffle())),
            my_claim in proptest::option::of(0u64..4),
            want_claim in any::<bool>(),
            timer in any::<bool>(),
        ) {
            let claim = my_claim.map(claim);
            let ask = |records: &[LeaseRecord]| {
                let mut seen = BTreeMap::new();
                observe(&mut seen, records, Ts(T));
                evaluate(&Inputs {
                    claim: claim.as_ref(),
                    want_claim,
                    open_own_segment: timer,
                    ..at_rest(records, &seen)
                })
            };
            prop_assert_eq!(ask(&records), ask(&shuffled));
        }

        /// Whoever asks, the same set of records names the same holder, or nobody would agree on one.
        #[test]
        fn every_device_finds_the_same_top_record(records in proptest::collection::vec(record(), 1..10)) {
            let counted = freshest(&records);
            let mut reversed = counted.clone();
            reversed.reverse();
            prop_assert_eq!(top(&counted), top(&reversed));
        }
    }

    #[test]
    fn tie_goes_to_lowest_device_id() {
        let seen = none();
        let (lower, higher) = (
            "a0000000-0000-4000-8000-000000000000",
            "c0000000-0000-4000-8000-000000000000",
        );
        let records = [held(higher, 3), held(lower, 3)];
        let with = |me: &'static str, generation: u64| {
            let c = claim(generation);
            let i = Inputs {
                me,
                claim: Some(&c),
                ..at_rest(&records, &seen)
            };
            evaluate(&i)
        };
        assert_eq!(with(lower, 3), Evaluation::HeldByMe { contended: true });
        match with(higher, 3) {
            Evaluation::Lost { by, why, .. } => {
                assert_eq!(by.device_id, lower);
                assert_eq!(why, LostWhy::LostTie);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn released_top_is_free() {
        let seen = none();
        let records = [released(LAPTOP, 5), held(ME, 4)];
        assert_eq!(
            evaluate(&at_rest(&records, &seen)),
            Evaluation::Free { drop_claim: false },
            "nothing under way here, so it is just free"
        );
        let mine = [released(ME, 5), held(LAPTOP, 4)];
        assert_eq!(
            evaluate(&at_rest(&mine, &seen)),
            Evaluation::Free { drop_claim: false },
            "my own release counts like anyone's"
        );
        // A claim that the release has passed: kept only by a device still at work.
        let records = [released(LAPTOP, 5)];
        let c = claim(4);
        let claimed = Inputs {
            claim: Some(&c),
            ..at_rest(&records, &seen)
        };
        assert_eq!(evaluate(&claimed), Evaluation::Free { drop_claim: true });
        let timing = Inputs {
            open_own_segment: true,
            ..claimed
        };
        assert_eq!(evaluate(&timing), Evaluation::Reclaim { generation: 6 });
        let changed = Inputs {
            last_change_at: Some(Ts(T - LEASE_IDLE_RELEASE_SECS + 1)),
            ..claimed
        };
        assert_eq!(evaluate(&changed), Evaluation::Reclaim { generation: 6 });
        let long_ago = Inputs {
            last_change_at: Some(Ts(T - LEASE_IDLE_RELEASE_SECS)),
            ..claimed
        };
        assert_eq!(evaluate(&long_ago), Evaluation::Free { drop_claim: true });
    }

    #[test]
    fn held_outranks_released_at_one_generation() {
        let seen = none();
        let records = [released(LAPTOP, 4)];
        let c = claim(4);
        let i = Inputs {
            claim: Some(&c),
            ..at_rest(&records, &seen)
        };
        assert_eq!(evaluate(&i), Evaluation::HeldByMe { contended: false });
    }

    #[test]
    fn lower_generation_held_is_ignored() {
        let seen = none();
        let records = [held(LAPTOP, 2), held(ME, 3)];
        assert_eq!(
            evaluate(&at_rest(&records, &seen)),
            Evaluation::HeldByMe { contended: false },
            "a crash and a restart resume from the file"
        );
        let released_above = [held(LAPTOP, 2), released(ME, 3)];
        assert_eq!(
            evaluate(&at_rest(&released_above, &seen)),
            Evaluation::Free { drop_claim: false },
        );
    }

    #[test]
    fn a_claim_overrides_an_old_copy_of_my_own_file() {
        let seen = none();
        let records = [released(ME, 2), held(LAPTOP, 1)];
        let c = claim(3);
        let i = Inputs {
            claim: Some(&c),
            ..at_rest(&records, &seen)
        };
        assert_eq!(evaluate(&i), Evaluation::HeldByMe { contended: false });

        // A cloud client can bring back a copy from before the records vanished, numbered above the claim.
        let records = [released(ME, 5), held(LAPTOP, 1)];
        let i = Inputs {
            claim: Some(&c),
            ..at_rest(&records, &seen)
        };
        assert_eq!(evaluate(&i), Evaluation::HeldByMe { contended: false });
    }

    #[test]
    fn nothing_configured_or_nothing_reachable_says_so_and_no_records_is_free() {
        let seen = none();
        let no_records: [LeaseRecord; 0] = [];
        assert_eq!(
            evaluate(&Inputs {
                configured: false,
                ..at_rest(&no_records, &seen)
            }),
            Evaluation::Off
        );
        assert_eq!(
            evaluate(&Inputs {
                reachable: false,
                ..at_rest(&no_records, &seen)
            }),
            Evaluation::Unknown
        );
        assert_eq!(
            evaluate(&at_rest(&no_records, &seen)),
            Evaluation::Free { drop_claim: false }
        );
    }

    #[test]
    fn a_timer_or_a_change_before_the_holder_was_seen_has_to_stop() {
        let seen = none();
        let records = [held(LAPTOP, 1)];
        let at_rest = at_rest(&records, &seen);
        assert!(matches!(
            evaluate(&at_rest),
            Evaluation::HeldByOther { live: true, .. }
        ));
        for i in [
            Inputs {
                want_claim: true,
                ..at_rest
            },
            Inputs {
                open_own_segment: true,
                ..at_rest
            },
        ] {
            match evaluate(&i) {
                Evaluation::Lost { by, why, .. } => {
                    assert_eq!(by.device_id, LAPTOP);
                    assert_eq!(why, LostWhy::NoClaim);
                }
                other => panic!("{other:?}"),
            }
        }
        let c = claim(0);
        let overtaken = Inputs {
            claim: Some(&c),
            ..at_rest
        };
        assert!(matches!(
            evaluate(&overtaken),
            Evaluation::Lost {
                why: LostWhy::Overtaken,
                ..
            }
        ));
    }

    #[test]
    fn liveness_uses_change_seen_not_the_stamp() {
        let r = LeaseRecord {
            beat_at: Ts(T),
            ..held(LAPTOP, 1)
        };
        // The stamp says just now, but nothing has changed on this computer's clock for 15 minutes.
        let seen = watched(&r, T - 3600, Some(T - LEASE_STALE_SECS));
        let o = &seen[LAPTOP];
        assert!(!o.is_live(Ts(T)));
        // The stamp says three hours ago, and it has just changed.
        let r = LeaseRecord {
            beat_at: Ts(T - 3 * 3600),
            ..held(LAPTOP, 1)
        };
        let seen = watched(&r, T - 3600, Some(T - 10));
        assert!(seen[LAPTOP].is_live(Ts(T)));
        // A later read with the same three values changes nothing.
        let mut seen = watched(&r, T - 3600, None);
        observe(&mut seen, std::slice::from_ref(&r), Ts(T));
        assert!(!seen[LAPTOP].moved);
        assert_eq!(seen[LAPTOP].changed_at, Ts(T - 3600));
        // Each of the three counts as a change, including a lower copy from another folder.
        for changed in [
            LeaseRecord {
                generation: 0,
                ..r.clone()
            },
            LeaseRecord {
                beat_seq: 0,
                ..r.clone()
            },
            LeaseRecord {
                beat_at: Ts(r.beat_at.secs() - 1),
                ..r.clone()
            },
        ] {
            let mut seen = watched(&r, T - 3600, None);
            observe(&mut seen, std::slice::from_ref(&changed), Ts(T));
            assert!(seen[LAPTOP].moved, "{changed:?}");
            assert_eq!(seen[LAPTOP].changed_at, Ts(T));
        }
    }

    #[test]
    fn skewed_clock_behind_is_live_once_it_moves() {
        let behind = |at: i64| LeaseRecord {
            beat_at: Ts(at - 3 * 3600),
            ..held(LAPTOP, 1)
        };
        let mut seen = BTreeMap::new();
        observe(&mut seen, &[behind(T)], Ts(T));
        assert!(seen[LAPTOP].is_live(Ts(T + 60)), "still watching");
        assert!(
            !seen[LAPTOP].is_live(Ts(T + LEASE_MIN_WATCH_SECS)),
            "three hours old and no beat seen"
        );
        assert!(!seen[LAPTOP].clock_suspect(Ts(T + 60)), "may just be old");
        observe(&mut seen, &[behind(T + 600)], Ts(T + 600));
        assert!(seen[LAPTOP].is_live(Ts(T + 600)), "it is beating");
        assert!(seen[LAPTOP].clock_suspect(Ts(T + 600)));
        assert!(
            !seen[LAPTOP].is_live(Ts(T + 600 + LEASE_STALE_SECS)),
            "until it stops"
        );
        let holder = HolderInfo::of(&behind(T + 600), &seen[LAPTOP], Ts(T + 660));
        assert_eq!(
            holder.last_sign_at,
            Ts(T + 600),
            "a stamp that can't be believed isn't the last sign of it"
        );
    }

    #[test]
    fn skewed_clock_ahead_goes_stale_after_watching() {
        let r = LeaseRecord {
            beat_at: Ts(T + 3 * 3600),
            ..held(LAPTOP, 1)
        };
        let seen = watched(&r, T, None);
        let o = &seen[LAPTOP];
        assert!(
            o.is_live(Ts(T + LEASE_MIN_WATCH_SECS)),
            "the stamp is young"
        );
        assert!(o.clock_suspect(Ts(T)));
        assert!(o.is_live(Ts(T + LEASE_STALE_SECS - 1)));
        assert!(!o.is_live(Ts(T + LEASE_STALE_SECS)), "but nothing moved");
        // Up to two minutes ahead is nothing to mention.
        let close = LeaseRecord {
            beat_at: Ts(T + 120),
            ..held(LAPTOP, 1)
        };
        assert!(!watched(&close, T, None)[LAPTOP].clock_suspect(Ts(T)));
    }

    #[test]
    fn not_called_stale_before_min_watch() {
        let r = LeaseRecord {
            beat_at: Ts(T - 2 * 86_400),
            ..held(LAPTOP, 1)
        };
        let seen = watched(&r, T, None);
        let o = &seen[LAPTOP];
        assert!(o.is_live(Ts(T)));
        assert!(o.is_live(Ts(T + LEASE_MIN_WATCH_SECS - 1)));
        assert!(!o.is_live(Ts(T + LEASE_MIN_WATCH_SECS)));
        let records = [r];
        let i = Inputs {
            now: Ts(T + LEASE_MIN_WATCH_SECS),
            ..at_rest(&records, &seen)
        };
        assert!(matches!(
            evaluate(&i),
            Evaluation::HeldByOther { live: false, .. }
        ));
    }

    fn file_name(id: &str) -> String {
        format!("{id}.json")
    }
    fn parse_of(id: &str, r: &LeaseRecord) -> Option<LeaseRecord> {
        parse_record(&file_name(id), &serde_json::to_vec(r).unwrap())
    }

    #[test]
    fn oversized_or_misnamed_or_future_version_record_is_ignored() {
        let id = "8f0c1a2b-0000-4000-8000-000000000001";
        let good = held(id, 7);
        assert_eq!(parse_of(id, &good), Some(good.clone()));
        // Named for another device, or a conflict copy of the right one.
        assert_eq!(
            parse_of("8f0c1a2b-0000-4000-8000-000000000002", &good),
            None
        );
        let bytes = serde_json::to_vec(&good).unwrap();
        assert_eq!(parse_record(&format!("{id} (1).json"), &bytes), None);
        assert_eq!(parse_record(&format!("{id}-PC2.json"), &bytes), None);
        // From a newer version.
        let future = LeaseRecord {
            lease_version: 2,
            ..good.clone()
        };
        assert_eq!(parse_of(id, &future), None);
        // Over the size limit, with the fields all in order: exactly at it is fine.
        let mut padded = serde_json::to_value(&good).unwrap();
        padded["pad"] = serde_json::Value::String(String::new());
        let base = serde_json::to_vec(&padded).unwrap().len();
        padded["pad"] = serde_json::Value::String("x".repeat(MAX_LEASE_FILE_BYTES - base));
        let at_limit = serde_json::to_vec(&padded).unwrap();
        assert_eq!(at_limit.len(), MAX_LEASE_FILE_BYTES);
        assert_eq!(parse_record(&file_name(id), &at_limit), Some(good.clone()));
        padded["pad"] = serde_json::Value::String("x".repeat(MAX_LEASE_FILE_BYTES - base + 1));
        let over = serde_json::to_vec(&padded).unwrap();
        assert_eq!(parse_record(&file_name(id), &over), None);
        // A name no message can show, a generation past any real count, too many segments.
        let bad_name = LeaseRecord {
            device_name: "pc\nname".to_owned(),
            ..good.clone()
        };
        assert_eq!(parse_of(id, &bad_name), None);
        let far = LeaseRecord {
            generation: MAX_GENERATION + 1,
            ..good.clone()
        };
        assert_eq!(parse_of(id, &far), None);
        let limit = LeaseRecord {
            generation: MAX_GENERATION,
            ..good.clone()
        };
        assert_eq!(parse_of(id, &limit), Some(limit.clone()));
        let many = LeaseRecord {
            open_segments: vec!["s".to_owned(); MAX_OPEN_SEGMENTS + 1],
            ..good.clone()
        };
        assert_eq!(parse_of(id, &many), None);
        // Half a file is only one that has not finished syncing.
        assert_eq!(
            parse_record(&file_name(id), &bytes[..bytes.len() / 2]),
            None
        );
        // An unknown field is no reason to ignore it, and what an older writer left out has a default.
        let mut more = serde_json::to_value(&good).unwrap();
        more["fromTheFuture"] = serde_json::json!(1);
        more.as_object_mut().unwrap().remove("openSegments");
        more.as_object_mut().unwrap().remove("appVersion");
        let read = parse_record(&file_name(id), &serde_json::to_vec(&more).unwrap()).unwrap();
        assert!(read.open_segments.is_empty());
        assert_eq!(read.generation, 7);
    }

    #[test]
    fn a_record_is_written_in_the_shape_the_folder_expects() {
        let r = LeaseRecord {
            open_segments: vec!["s1".to_owned()],
            ..held(LAPTOP, 7)
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["leaseVersion"], 1);
        assert_eq!(v["deviceId"], LAPTOP);
        assert_eq!(v["deviceName"], "pc-b");
        assert_eq!(v["appVersion"], "1.3.1");
        assert_eq!(v["generation"], 7);
        assert_eq!(v["state"], "held");
        assert_eq!(v["beatSeq"], 1);
        assert_eq!(v["openSegments"], serde_json::json!(["s1"]));
        assert_eq!(v["acquiredAt"], Ts(T).iso());
        assert_eq!(v["beatAt"], Ts(T).iso());
        assert_eq!(
            serde_json::to_value(released(LAPTOP, 1)).unwrap()["state"],
            "released"
        );
    }

    #[test]
    fn generation_uses_memo_when_records_vanish() {
        let memo = LeaseMemo {
            device_id: LAPTOP.to_owned(),
            device_name: "LAPTOP".to_owned(),
            generation: 9,
            at: Ts(T),
        };
        assert_eq!(next_generation(&[], None, None), Some(1));
        assert_eq!(next_generation(&[], Some(&memo), None), Some(10));
        assert_eq!(
            next_generation(&[held(LAPTOP, 12)], Some(&memo), None),
            Some(13)
        );
        let c = claim(20);
        assert_eq!(
            next_generation(&[held(LAPTOP, 12)], Some(&memo), Some(&c)),
            Some(21)
        );
        // The same through a reclaim, when what the release said is lower than what was once known.
        let seen = none();
        let records = [released(LAPTOP, 3)];
        let c = claim(1);
        let i = Inputs {
            claim: Some(&c),
            open_own_segment: true,
            memo: Some(&memo),
            ..at_rest(&records, &seen)
        };
        assert_eq!(evaluate(&i), Evaluation::Reclaim { generation: 10 });
    }

    /// Audit SEC-1: no claim is numbered past what a reader takes. A record at the top stops claims and is named, and a
    /// memo that high, which only such a record leaves, counts for nothing once that record is gone.
    #[test]
    fn no_claim_goes_past_the_highest_generation_a_reader_takes() {
        assert_eq!(
            next_generation(&[held(LAPTOP, MAX_GENERATION - 1)], None, None),
            Some(MAX_GENERATION)
        );
        let id = "a0000000-0000-4000-8000-000000000000";
        let topmost = LeaseRecord {
            device_id: id.to_owned(),
            ..held(LAPTOP, MAX_GENERATION)
        };
        let bytes = serde_json::to_vec(&topmost).unwrap();
        assert_eq!(
            parse_record(&format!("{id}.json"), &bytes),
            Some(topmost.clone()),
            "the last claim there can be is still read"
        );
        assert_eq!(
            next_generation(std::slice::from_ref(&topmost), None, None),
            None
        );
        assert_eq!(
            next_generation(&[], None, Some(&claim(MAX_GENERATION))),
            None
        );
        assert_eq!(at_ceiling(&[held(LAPTOP, 3), topmost]), Some(id));
        assert_eq!(at_ceiling(&[held(LAPTOP, 3)]), None);
        for generation in [MAX_GENERATION, MAX_GENERATION + 1] {
            let memo = LeaseMemo {
                device_id: LAPTOP.to_owned(),
                device_name: "LAPTOP".to_owned(),
                generation,
                at: Ts(T),
            };
            assert_eq!(
                next_generation(&[held(LAPTOP, 4)], Some(&memo), None),
                Some(5)
            );
        }
        // A release at the top can't be claimed above: the claim here is over, rather than numbered where no reader goes.
        let seen = none();
        let records = [released(LAPTOP, MAX_GENERATION)];
        let c = claim(1);
        let i = Inputs {
            claim: Some(&c),
            open_own_segment: true,
            ..at_rest(&records, &seen)
        };
        assert_eq!(evaluate(&i), Evaluation::Free { drop_claim: true });
    }

    #[test]
    fn the_copy_with_the_highest_generation_and_beat_counts_for_a_device() {
        let copies = [
            rec(LAPTOP, 3, LeaseRecordState::Held, 9),
            rec(LAPTOP, 4, LeaseRecordState::Held, 1),
            rec(LAPTOP, 4, LeaseRecordState::Held, 2),
            rec(LAPTOP, 2, LeaseRecordState::Held, 50),
        ];
        let counted = freshest(&copies);
        assert_eq!(counted.len(), 1);
        assert_eq!((counted[0].generation, counted[0].beat_seq), (4, 2));
        // A stale Held copy in a folder that missed the release doesn't bring a hold back.
        let release = [
            rec(LAPTOP, 4, LeaseRecordState::Released, 3),
            rec(LAPTOP, 4, LeaseRecordState::Held, 2),
        ];
        assert_eq!(freshest(&release)[0].state, LeaseRecordState::Released);
        // At one beat, a release is what came after the hold, whichever folder was read first.
        let held = rec(LAPTOP, 4, LeaseRecordState::Held, 2);
        let release = rec(LAPTOP, 4, LeaseRecordState::Released, 2);
        for copies in [[held.clone(), release.clone()], [release, held]] {
            assert_eq!(freshest(&copies)[0].state, LeaseRecordState::Released);
        }
    }

    fn memo(device: &str, at: i64) -> LeaseMemo {
        LeaseMemo {
            device_id: device.to_owned(),
            device_name: "LAPTOP".to_owned(),
            generation: 3,
            at: Ts(at),
        }
    }

    #[test]
    fn only_another_holder_makes_this_computer_read_only() {
        let now = Ts(T);
        for (state, want) in [
            (LeaseStateName::Off, false),
            (LeaseStateName::Checking, false),
            (LeaseStateName::Free, false),
            (LeaseStateName::HeldByMe, false),
            (LeaseStateName::HeldByOtherLive, true),
            (LeaseStateName::HeldByOtherStale, true),
            (LeaseStateName::Unknown, false),
        ] {
            let s = LeaseState {
                state,
                ..LeaseState::default()
            };
            assert_eq!(s.read_only(None, ME, now), want, "{state:?}");
        }
    }

    #[test]
    fn with_no_folder_to_read_the_last_holder_decides_for_a_day() {
        let now = Ts(T);
        let s = LeaseState {
            state: LeaseStateName::Unknown,
            ..LeaseState::default()
        };
        assert!(!s.read_only(Some(&memo(ME, T)), ME, now), "that was me");
        assert!(s.read_only(Some(&memo(LAPTOP, T - 3600)), ME, now));
        assert!(s.read_only(Some(&memo(LAPTOP, T - 86_399)), ME, now));
        assert!(
            !s.read_only(Some(&memo(LAPTOP, T - 86_400)), ME, now),
            "too long ago to stop anyone"
        );
        match s.gate(Some(&memo(LAPTOP, T - 3600)), ME, now) {
            Err(CoreError::ReadOnly { holder }) => assert_eq!(holder, "LAPTOP"),
            other => panic!("{other:?}"),
        }
        let chose = LeaseState {
            offline_claim: Some(3),
            ..s
        };
        assert!(
            !chose.read_only(Some(&memo(LAPTOP, T - 3600)), ME, now),
            "work here anyway"
        );
        assert!(chose.gate(Some(&memo(LAPTOP, T - 3600)), ME, now).is_ok());
    }

    #[test]
    fn the_refusal_names_the_holder_and_has_its_own_code() {
        let s = LeaseState {
            state: LeaseStateName::HeldByOtherLive,
            holder: Some(HolderInfo::of(
                &LeaseRecord {
                    device_name: "LAPTOP".to_owned(),
                    ..held(LAPTOP, 1)
                },
                &Observed::first(&held(LAPTOP, 1), Ts(T)),
                Ts(T),
            )),
            ..LeaseState::default()
        };
        let e = s.gate(None, ME, Ts(T)).unwrap_err();
        assert_eq!(e.code(), "read_only");
        assert_eq!(
            e.to_string(),
            "LAPTOP is using zWorkTracker, so this computer is read-only. Take over from the banner to work here."
        );
        let nameless = LeaseState { holder: None, ..s };
        assert!(
            nameless
                .gate(None, ME, Ts(T))
                .unwrap_err()
                .to_string()
                .starts_with("Another computer is using")
        );
    }

    #[test]
    fn a_change_with_no_claim_wants_one_and_a_change_with_one_only_restarts_the_idle_clock() {
        let mut s = LeaseState::new(true);
        assert_eq!(s.state, LeaseStateName::Checking);
        s.note_change(Ts(T));
        assert!(s.want_claim);
        assert_eq!(s.last_change_at, Some(Ts(T)));
        let mut held = LeaseState {
            claim: Some(claim(1)),
            ..LeaseState::new(true)
        };
        held.note_change(Ts(T));
        assert!(!held.want_claim);
        assert_eq!(held.last_change_at, Some(Ts(T)));
    }

    #[test]
    fn with_no_folder_there_is_no_lease_and_nothing_is_remembered() {
        let mut s = LeaseState::new(false);
        assert!(!s.active());
        assert_eq!(s.state, LeaseStateName::Off);
        s.targets_changed(true);
        assert_eq!(s.state, LeaseStateName::Checking);
        assert!(s.active());
        s.state = LeaseStateName::HeldByMe;
        s.claim = Some(claim(4));
        s.want_claim = true;
        observe(&mut s.observed, &[held(LAPTOP, 1)], Ts(T));
        s.targets_changed(true);
        assert_eq!(
            s.state,
            LeaseStateName::HeldByMe,
            "a second folder changes nothing"
        );
        s.targets_changed(false);
        assert_eq!(s.state, LeaseStateName::Off);
        assert!(s.claim.is_none() && s.observed.is_empty() && !s.want_claim);
    }

    #[test]
    fn the_page_reads_each_state_by_this_name() {
        for (state, name) in [
            (LeaseStateName::Off, "off"),
            (LeaseStateName::Checking, "checking"),
            (LeaseStateName::Free, "free"),
            (LeaseStateName::HeldByMe, "heldByMe"),
            (LeaseStateName::HeldByOtherLive, "heldByOtherLive"),
            (LeaseStateName::HeldByOtherStale, "heldByOtherStale"),
            (LeaseStateName::Unknown, "unknown"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), name);
        }
    }
}
