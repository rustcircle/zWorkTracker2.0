//! The local store: one SQLite database, `z-work-data.db`, in the data folder.
//!
//! Every replicated record is kept as its exact serde JSON in `records(kind, id, json)`, and the rest of the
//! `LocalFile` as one JSON row in `meta`, so merging and the device files see the values 1.3.1 had. A save writes only
//! the rows that changed, in one transaction. A damaged database is set aside under a new name, never overwritten.
//!
//! The 1.3.1 files (`z-work-data.json`, its `.bak` and the rest) are only ever read, at the first start, for this
//! device's identity and sync folders. The data then comes from the sync folders (see `bootstrap`).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::config::DbConfig;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backup;
use crate::error::CoreError;
use crate::hlc::{self, Stamped};
use crate::model::{
    EpochInfo, LocalFile, LocalPrefs, RemoteDevice, SCHEMA_VERSION, Shared, Stamp, SyncTarget,
};
use crate::persist::{self, MAIN_FILE, Paths};
use crate::service::folder_list;
use crate::time::Ts;
use crate::validate::{check_core, check_epoch};

pub const DB_FILE: &str = "z-work-data.db";
/// "zWT1" (0x7A575431): marks the file as this app's, so a stray SQLite file is never taken for the store.
pub const APPLICATION_ID: i32 = 2_052_543_537;
/// The newest database schema this build knows. Raised by one per migration in `MIGRATIONS`.
pub const DB_SCHEMA_VERSION: u32 = 1;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// What the WAL is cut back to after a checkpoint.
const JOURNAL_SIZE_LIMIT: i64 = 4_194_304;
/// Where a database is built before it is renamed into place, so `DB_FILE` only ever exists complete.
const NEW_FILE: &str = "z-work-data.db.new";
/// The files SQLite may keep beside a database. They belong to it and go wherever it goes.
const SIDE_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];
/// The 1.3.1 store's backup, read at the first start when the main file can't be.
const LEGACY_BAK: &str = "z-work-data.json.bak";

/// The tables of schema 1. The application id goes in with them, so a file is only ever this app's once it has them.
const MIGRATION_1: &str = "\
PRAGMA application_id = 2052543537;
CREATE TABLE schema_version (
    version    INTEGER PRIMARY KEY,
    applied_at TEXT    NOT NULL
) STRICT;
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT, WITHOUT ROWID;
CREATE TABLE records (
    kind TEXT NOT NULL,
    id   TEXT NOT NULL,
    json TEXT NOT NULL,
    PRIMARY KEY (kind, id)
) STRICT;
";

/// (version reached, SQL). Applied in order, each in its own IMMEDIATE transaction with its `schema_version` row.
pub(crate) const MIGRATIONS: &[(u32, &str)] = &[(1, MIGRATION_1)];

const UPSERT_RECORD: &str = "INSERT INTO records(kind, id, json) VALUES (?1, ?2, ?3) \
     ON CONFLICT(kind, id) DO UPDATE SET json = excluded.json";
const DELETE_RECORD: &str = "DELETE FROM records WHERE kind = ?1 AND id = ?2";
const UPSERT_META: &str = "INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value";

/// One row of `records` to write.
enum Op<'a> {
    Put {
        kind: &'static str,
        id: &'a str,
        json: String,
    },
    Del {
        kind: &'static str,
        id: &'a str,
    },
}

/// Every map of `Shared`, by the serde key that is its `records.kind`. The kinds, the diff, the full write and the
/// load all come from this one list, so they can't disagree.
macro_rules! record_kinds {
    ($($kind:literal => $field:ident),* $(,)?) => {
        /// `records.kind`: exactly the serde key of each map of `Shared`.
        #[cfg_attr(not(test), expect(dead_code, reason = "the list the tests hold against `Shared`"))]
        pub(crate) const KINDS: [&str; 15] = [$($kind),*];

        /// What differs between two sets of data, as row writes, and the newest stamp among the records written.
        fn diff_data<'a>(
            prev: &'a Shared,
            next: &'a Shared,
            ops: &mut Vec<Op<'a>>,
            newest: &mut Option<&'a Stamp>,
        ) -> Result<(), CoreError> {
            $(diff_kind($kind, &prev.$field, &next.$field, ops, newest)?;)*
            Ok(())
        }

        /// Every record, as row writes, and the newest stamp among them.
        fn put_data<'a>(
            next: &'a Shared,
            ops: &mut Vec<Op<'a>>,
            newest: &mut Option<&'a Stamp>,
        ) -> Result<(), CoreError> {
            $(for (id, record) in &next.$field {
                ops.push(put($kind, id, record)?);
                hlc::raise(newest, record);
            })*
            Ok(())
        }

        /// Test only: how many records `data` holds, of every kind.
        #[cfg(test)]
        pub(crate) fn record_count(data: &Shared) -> usize {
            0 $(+ data.$field.len())*
        }

        /// Takes one row into `data`. An unknown kind or a row that doesn't parse is damage.
        fn load_row(data: &mut Shared, kind: &str, id: &str, json: &str) -> Result<(), String> {
            match kind {
                $($kind => load_into(&mut data.$field, kind, id, json),)*
                other => Err(format!("it holds a record of an unknown kind ({other})")),
            }
        }
    };
}

record_kinds! {
    "projects" => projects,
    "tasks" => tasks,
    "segments" => segments,
    "settings" => settings,
    "shifts" => shifts,
    "rates" => rates,
    "rounding" => rounding,
    "gaps" => gaps,
    "pauses" => pauses,
    "tags" => tags,
    "templates" => templates,
    "summaries" => summaries,
    "prefixes" => prefixes,
    "auditLog" => audit_log,
    "pauseRequests" => pause_requests,
}

/// Test only: the rows a full write of `data` puts in `records` (kind, id, JSON text), in the order of `KINDS` and ids.
#[cfg(test)]
pub(crate) fn record_rows(data: &Shared) -> Result<Vec<(String, String, String)>, CoreError> {
    let mut ops = Vec::new();
    put_data(data, &mut ops, &mut None)?;
    Ok(ops
        .into_iter()
        .filter_map(|op| match op {
            Op::Put { kind, id, json } => Some((kind.to_owned(), id.to_owned(), json)),
            Op::Del { .. } => None,
        })
        .collect())
}

/// A record about to be written, read back first: what can't be read back must never replace what can.
fn put<'a, T: Serialize + DeserializeOwned + PartialEq>(
    kind: &'static str,
    id: &'a str,
    record: &T,
) -> Result<Op<'a>, CoreError> {
    let json = serde_json::to_string(record)?;
    match serde_json::from_str::<T>(&json) {
        Ok(back) if back == *record => Ok(Op::Put { kind, id, json }),
        _ => Err(CoreError::Validation(format!(
            "refusing to save data that cannot be read back: {kind} {id}"
        ))),
    }
}

/// A merge-join over the two sorted maps: a key only in `prev` is deleted, one only in `next` is written, and one in
/// both is written only when it changed. `newest` is raised to the newest stamp of each record written.
fn diff_kind<'a, T: Serialize + DeserializeOwned + PartialEq + Stamped>(
    kind: &'static str,
    prev: &'a BTreeMap<String, T>,
    next: &'a BTreeMap<String, T>,
    ops: &mut Vec<Op<'a>>,
    newest: &mut Option<&'a Stamp>,
) -> Result<(), CoreError> {
    let mut old = prev.iter().peekable();
    let mut new = next.iter().peekable();
    loop {
        match (old.peek().copied(), new.peek().copied()) {
            (None, None) => return Ok(()),
            (Some((id, _)), None) => {
                ops.push(Op::Del { kind, id });
                old.next();
            }
            (None, Some((id, record))) => {
                ops.push(put(kind, id, record)?);
                hlc::raise(newest, record);
                new.next();
            }
            (Some((a, before)), Some((b, after))) => match a.cmp(b) {
                std::cmp::Ordering::Less => {
                    ops.push(Op::Del { kind, id: a });
                    old.next();
                }
                std::cmp::Ordering::Greater => {
                    ops.push(put(kind, b, after)?);
                    hlc::raise(newest, after);
                    new.next();
                }
                std::cmp::Ordering::Equal => {
                    if before != after {
                        ops.push(put(kind, b, after)?);
                        hlc::raise(newest, after);
                    }
                    old.next();
                    new.next();
                }
            },
        }
    }
}

/// What one save writes, worked out before anything is written.
pub(crate) struct Planned<'a> {
    ops: Vec<Op<'a>>,
    /// Every row is written again, after a save that failed.
    full: bool,
    /// The newest stamp among the records written. A record not written has no stamp that wasn't in the store already,
    /// so this is all a save adds to what this device has seen (addendum P11, see `hlc`).
    newest: Option<&'a Stamp>,
}

impl<'a> Planned<'a> {
    #[must_use]
    pub(crate) fn newest(&self) -> Option<&'a Stamp> {
        self.newest
    }

    /// Some record is written or deleted.
    #[must_use]
    pub(crate) fn writes_records(&self) -> bool {
        self.full || !self.ops.is_empty()
    }
}

/// The record rows a save of `next` over `prev` writes: what differs, or with `full` every record.
pub(crate) fn plan_rows<'a>(
    prev: &'a Shared,
    next: &'a Shared,
    full: bool,
) -> Result<Planned<'a>, CoreError> {
    let mut planned = Planned {
        ops: Vec::new(),
        full,
        newest: None,
    };
    if full {
        put_data(next, &mut planned.ops, &mut planned.newest)?;
    } else {
        diff_data(prev, next, &mut planned.ops, &mut planned.newest)?;
    }
    Ok(planned)
}

fn load_into<T: DeserializeOwned>(
    map: &mut BTreeMap<String, T>,
    kind: &str,
    id: &str,
    json: &str,
) -> Result<(), String> {
    let record = serde_json::from_str(json)
        .map_err(|e| format!("its {kind} record {id} can't be read: {e}"))?;
    map.insert(id.to_owned(), record);
    Ok(())
}

/// What `meta.local` holds: the `LocalFile` without its data. Borrowed, so a save never serialises the data to compare it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalHead<'a> {
    version: u64,
    device_id: &'a str,
    device_name: &'a str,
    last_heartbeat: Option<Ts>,
    prefs: &'a LocalPrefs,
    remote_devices: &'a BTreeMap<String, RemoteDevice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    newest_stamp: Option<&'a Stamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_change_here: Option<Ts>,
}

impl<'a> LocalHead<'a> {
    fn of(file: &'a LocalFile) -> Self {
        Self {
            version: file.version,
            device_id: &file.device_id,
            device_name: &file.device_name,
            last_heartbeat: file.last_heartbeat,
            prefs: &file.prefs,
            remote_devices: &file.remote_devices,
            newest_stamp: file.newest_stamp.as_ref(),
            latest_change_here: file.latest_change_here,
        }
    }
}

fn head_json(file: &LocalFile) -> Result<String, CoreError> {
    Ok(serde_json::to_string(&LocalHead::of(file))?)
}

/// An open database.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    dir: PathBuf,
    /// The heartbeat `meta.beat` holds, so a beat that is not later writes nothing.
    beat: Option<Ts>,
    /// A save failed after it may have reached the disk. The next save rewrites every row, so what is in memory and on
    /// disk can't drift apart.
    full_write_due: bool,
    /// `meta.local` as last loaded or written. The head is compared with this rather than with the caller's previous
    /// file, so a change made to it in memory alone, such as a device name learned during a sync, is still written.
    head: String,
    /// When the database was first made: retention never removes a backup dated on or before that day, as 1.3.1 may
    /// have written it.
    created_at: Option<Ts>,
    #[cfg(any(test, feature = "test-hooks"))]
    saves_blocked: bool,
    /// Test only: the transaction fails after this many record ops, so a save that dies halfway can be checked.
    #[cfg(test)]
    fail_after_ops: Option<usize>,
    /// Test only: what the last save that worked wrote.
    #[cfg(test)]
    last_save: SaveStats,
}

#[derive(Debug)]
pub struct Opened {
    pub store: Store,
    /// `last_heartbeat` is the later of `meta.local`'s and `meta.beat`, cut to `now` when there is a beat, as 1.3.1's
    /// `apply_heartbeat` did with its heartbeat file.
    pub file: LocalFile,
    /// `meta.beat` as stored, not cut to now: only crash recovery of a run that began after now reads it.
    pub raw_beat: Option<Ts>,
    pub origin: Origin,
    /// Each with its level, for `Service::notify`.
    pub notices: Vec<(&'static str, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Existing,
    Created {
        legacy: Legacy,
    },
    Replaced {
        kept_at: PathBuf,
        why: String,
        legacy: Legacy,
        from_backup: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Legacy {
    /// No 1.3.1 store (`z-work-data.json` and `.bak` absent).
    None,
    /// One was found, but neither file parsed. `why` is the parse error of the first one found.
    Unreadable { why: String },
    /// Identity, setup flags and `folders` sync folders were taken from it (or, when replacing, from the damaged database).
    Identity { folders: usize },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SaveStats {
    pub upserted: usize,
    pub deleted: usize,
    pub head_written: bool,
}

/// Why opening didn't give a store.
enum Fault {
    /// The file can't be trusted: it is set aside and replaced.
    Damage(String),
    /// Anything else, such as another program holding it: the open fails and nothing changes.
    Fail(CoreError),
}

/// SQLite's error as the `io::Error` the rest of the app knows how to judge.
fn sql_io(e: &rusqlite::Error) -> io::Error {
    use rusqlite::ErrorCode as C;
    let kind = match e.sqlite_error_code() {
        // Held: `is_transient`, so the open is retried.
        Some(C::DatabaseBusy | C::DatabaseLocked) => io::ErrorKind::WouldBlock,
        // Antivirus: `is_transient` too.
        Some(C::PermissionDenied | C::ReadOnly) => io::ErrorKind::PermissionDenied,
        Some(C::DiskFull) => io::ErrorKind::StorageFull,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, e.to_string())
}

fn is_damage(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt)
    )
}

/// A read that failed: damage when SQLite says the file is bad, else a read error. A held database is never damage.
fn read_fault(e: rusqlite::Error) -> Fault {
    if is_damage(&e) {
        Fault::Damage(e.to_string())
    } else {
        Fault::Fail(CoreError::Read(sql_io(&e)))
    }
}

fn write_error(e: rusqlite::Error) -> CoreError {
    CoreError::Io(sql_io(&e))
}

/// The path SQLite is given. With `SQLITE_OPEN_NOFOLLOW` SQLite refuses a symbolic link anywhere in the path, and
/// folders such as macOS's `/var` are links, so the folder is resolved first. The file's own name is then the only
/// part that may not be a link.
fn sqlite_path(dir: &Path, name: &str) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        Ok(fs::canonicalize(dir)?.join(name))
    }
    #[cfg(not(unix))]
    {
        Ok(dir.join(name))
    }
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn side_file(dir: &Path, stem: &str, suffix: &str) -> PathBuf {
    dir.join(format!("{stem}{suffix}"))
}

/// Removes a file left from an earlier run, never following a link. A folder in its place is an error.
fn remove_leftover(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            Err(io::Error::other(format!("{} is a folder", path.display())))
        }
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Only creation makes `z-work-data.db.new` and its journal, and they hold nothing that isn't derivable.
fn clear_new(dir: &Path) -> io::Result<()> {
    remove_leftover(&dir.join(NEW_FILE))?;
    for suffix in SIDE_SUFFIXES {
        remove_leftover(&side_file(dir, NEW_FILE, suffix))?;
    }
    Ok(())
}

/// Moves the database and whatever SQLite keeps beside it aside under one new stem, so a damaged file stays for hand
/// recovery and its WAL, which "is part of the persistent state of the database", stays with it. If any rename fails,
/// those already made are undone and nothing is created: everything stays as found for the next launch.
pub(crate) fn set_aside_db(dir: &Path, now: Ts) -> io::Result<PathBuf> {
    let stamp = now.datetime().format("%Y%m%dT%H%M%SZ");
    let db = dir.join(DB_FILE);
    let present: Vec<&str> = SIDE_SUFFIXES
        .into_iter()
        .filter(|s| exists(&side_file(dir, DB_FILE, s)))
        .collect();
    for n in 0..1000 {
        let stem = if n == 0 {
            format!("z-work-data.corrupt-{stamp}.db")
        } else {
            format!("z-work-data.corrupt-{stamp}-{n}.db")
        };
        let target = dir.join(&stem);
        if exists(&target)
            || SIDE_SUFFIXES
                .iter()
                .any(|s| exists(&side_file(dir, &stem, s)))
        {
            continue;
        }
        // The database first, so it is never found under its name without the WAL that goes with it, and read as if
        // whole. A crash before the WAL follows leaves no database here, and the next open sends the WAL after the one
        // set aside (`finish_set_aside`).
        let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();
        if exists(&db) {
            moves.push((db.clone(), target.clone()));
        }
        moves.extend(
            present
                .iter()
                .map(|s| (side_file(dir, DB_FILE, s), side_file(dir, &stem, s))),
        );
        for (done, (from, to)) in moves.iter().enumerate() {
            #[cfg(test)]
            if SET_ASIDE_STOPS_AFTER.with(std::cell::Cell::get) == Some(done) {
                return Err(io::Error::other("the app stopped here, for a test"));
            }
            if let Err(e) = fs::rename(from, to) {
                for (back_from, back_to) in moves.iter().take(done).rev() {
                    let _ = fs::rename(back_to, back_from);
                }
                return Err(e);
            }
        }
        return Ok(target);
    }
    Err(io::Error::other(
        "too many damaged data files are set aside",
    ))
}

/// A WAL or journal with no database beside it would be taken into the next new database as if it were its own.
fn move_orphans(dir: &Path, now: Ts) -> io::Result<()> {
    let orphans = SIDE_SUFFIXES
        .iter()
        .any(|s| exists(&side_file(dir, DB_FILE, s)));
    if orphans && !exists(&dir.join(DB_FILE)) {
        set_aside_db(dir, now)?;
    }
    Ok(())
}

/// Reads the version of the schema: `None` for a file without the table or without a row in it.
fn current_version(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    let has_table: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'schema_version')",
        [],
        |r| r.get(0),
    )?;
    if !has_table {
        return Ok(None);
    }
    conn.query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
}

fn newest_of(migrations: &[(u32, &str)]) -> u32 {
    migrations.last().map_or(0, |(v, _)| *v)
}

/// Brings `conn` to the newest schema in `migrations`. Never goes down: a newer database is refused untouched. A
/// database at an older schema is first copied to `z-work-data.v<n>.db`, once.
pub(crate) fn migrate(
    conn: &mut Connection,
    dir: &Path,
    now: Ts,
    migrations: &[(u32, &str)],
) -> Result<(), CoreError> {
    let current = current_version(conn).map_err(|e| CoreError::Read(sql_io(&e)))?;
    let current = u32::try_from(current.unwrap_or(0)).map_err(|_| {
        CoreError::Read(io::Error::other(
            "the schema version of the data file is not valid",
        ))
    })?;
    let newest = newest_of(migrations);
    if current > newest {
        return Err(CoreError::FutureVersion {
            found: current.into(),
            supported: newest.into(),
        });
    }
    if (1..newest).contains(&current) {
        let copy = dir.join(format!("z-work-data.v{current}.db"));
        if !exists(&copy) {
            let target = sqlite_path(dir, &format!("z-work-data.v{current}.db"))?;
            conn.execute("VACUUM INTO ?1", [target.to_string_lossy().into_owned()])
                .map_err(write_error)?;
        }
    }
    for (version, sql) in migrations.iter().filter(|(v, _)| *v > current) {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(write_error)?;
        tx.execute_batch(sql).map_err(write_error)?;
        tx.execute(
            "INSERT INTO schema_version(version, applied_at) VALUES (?1, ?2)",
            (version, now.iso()),
        )
        .map_err(write_error)?;
        tx.commit().map_err(write_error)?;
    }
    Ok(())
}

fn read_meta(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()
}

fn read_meta_ts(conn: &Connection, key: &str) -> Result<Option<Ts>, Fault> {
    match read_meta(conn, key).map_err(read_fault)? {
        None => Ok(None),
        Some(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| Fault::Damage(format!("its {key} row can't be read: {e}"))),
    }
}

/// The `meta.local` row, as JSON. Its absence is damage.
fn read_head(conn: &Connection) -> Result<Value, Fault> {
    let text = read_meta(conn, "local")
        .map_err(read_fault)?
        .ok_or_else(|| Fault::Damage("it holds no device row".to_owned()))?;
    serde_json::from_str(&text)
        .map_err(|e| Fault::Damage(format!("its device row can't be read: {e}")))
}

/// The model version of a head: a newer one is refused untouched, as a newer 1.3.1 file was.
fn check_head_version(head: &Value) -> Result<(), Fault> {
    match head.get("version").and_then(Value::as_u64) {
        None | Some(0) => Err(Fault::Damage("it has no model version".to_owned())),
        Some(found) if found > SCHEMA_VERSION => Err(Fault::Fail(CoreError::FutureVersion {
            found,
            supported: SCHEMA_VERSION,
        })),
        Some(_) => Ok(()),
    }
}

fn text<'r>(row: &'r rusqlite::Row<'_>, i: usize) -> Result<&'r str, Fault> {
    row.get_ref(i)
        .map_err(read_fault)?
        .as_str()
        .map_err(|e| Fault::Damage(format!("a record can't be read: {e}")))
}

/// Every row read into a `LocalFile`, with the heartbeat `meta.beat` holds, as stored.
fn load_file(conn: &Connection, mut head: Value) -> Result<(LocalFile, Option<Ts>), Fault> {
    let mut data = Shared::default();
    {
        let mut stmt = conn
            .prepare("SELECT kind, id, json FROM records")
            .map_err(read_fault)?;
        let mut rows = stmt.query([]).map_err(read_fault)?;
        while let Some(row) = rows.next().map_err(read_fault)? {
            load_row(&mut data, text(row, 0)?, text(row, 1)?, text(row, 2)?)
                .map_err(Fault::Damage)?;
        }
    }
    let Some(fields) = head.as_object_mut() else {
        return Err(Fault::Damage("its device row is not an object".to_owned()));
    };
    fields.insert("data".to_owned(), Value::Object(serde_json::Map::new()));
    let mut file: LocalFile = serde_json::from_value(head)
        .map_err(|e| Fault::Damage(format!("its device row can't be read: {e}")))?;
    // Schema 2 only added optional fields, so an older head just takes the new number, as a 1.3.1 file did.
    file.version = SCHEMA_VERSION;
    file.data = data;
    check_core(&file.data).map_err(|e| Fault::Damage(e.to_string()))?;
    let beat = read_meta_ts(conn, "beat")?;
    Ok((file, beat))
}

/// A connection to `path` with nothing read yet. Until the checks pass nothing may be written, not even the checkpoint
/// SQLite makes when the last connection closes, so a damaged or refused file stays exactly as it was found.
fn connect(path: &Path) -> Result<Connection, Fault> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let conn = Connection::open_with_flags(path, flags).map_err(read_fault)?;
    conn.busy_timeout(BUSY_TIMEOUT).map_err(read_fault)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .map_err(read_fault)?;
    // Before the first access, so a WAL is never given shared memory (no `-shm`) and no other process can open it.
    let mode: String = conn
        .pragma_update_and_check(None, "locking_mode", "EXCLUSIVE", |r| r.get(0))
        .map_err(read_fault)?;
    if !mode.eq_ignore_ascii_case("exclusive") {
        return Err(Fault::Fail(CoreError::Read(io::Error::other(format!(
            "the data file can't be locked for this app alone ({mode})"
        )))));
    }
    conn.pragma_update(None, "trusted_schema", false)
        .map_err(read_fault)?;
    // Reads only, until the caller says otherwise.
    conn.pragma_update(None, "query_only", true)
        .map_err(read_fault)?;
    Ok(conn)
}

/// The settings every write relies on: WAL with a full sync after each commit, and F_FULLFSYNC on macOS as 1.3.1 had.
fn make_durable(conn: &Connection) -> Result<(), Fault> {
    let journal: String = conn
        .pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))
        .map_err(read_fault)?;
    if !journal.eq_ignore_ascii_case("wal") {
        // The rollback journal with a full sync is still durable, only slower.
        eprintln!("the data file stays in {journal} journal mode, as WAL is not available here");
    }
    let synced = synced();
    conn.pragma_update(None, "synchronous", if synced { "FULL" } else { "OFF" })
        .map_err(read_fault)?;
    conn.pragma_update(None, "fullfsync", synced)
        .map_err(read_fault)?;
    conn.pragma_update(None, "checkpoint_fullfsync", synced)
        .map_err(read_fault)?;
    conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT)
        .map_err(read_fault)?;
    Ok(())
}

/// Whether a store opened now syncs every commit to disk: always, but in a test that said otherwise (`NoSyncStores`).
fn synced() -> bool {
    #[cfg(test)]
    if NO_SYNC_STORES.with(std::cell::Cell::get) {
        return false;
    }
    true
}

fn quick_check(conn: &Connection) -> Result<(), Fault> {
    let mut found: Vec<String> = Vec::new();
    conn.pragma_query(None, "quick_check", |r| {
        found.push(r.get(0)?);
        Ok(())
    })
    .map_err(read_fault)?;
    if found == ["ok"] {
        Ok(())
    } else {
        Err(Fault::Damage(
            found
                .into_iter()
                .next()
                .unwrap_or_else(|| "its pages can't be checked".to_owned()),
        ))
    }
}

/// What an open of an existing database read.
struct Loaded {
    conn: Connection,
    file: LocalFile,
    beat: Option<Ts>,
    created_at: Option<Ts>,
}

/// Opens `dir`'s database, checking it before anything is written: that it is this app's, its pages, its schema and
/// model versions, and every row. Only then is it switched to WAL and migrated. A database with a migration to run is
/// copied first, migrated, and then read.
fn open_existing(dir: &Path, now: Ts, migrations: &[(u32, &str)]) -> Result<Loaded, Fault> {
    let path = sqlite_path(dir, DB_FILE).map_err(|e| Fault::Fail(CoreError::Read(e)))?;
    let mut conn = connect(&path)?;
    // The first read takes the lock. This also covers a zero-length file, which SQLite opens as an empty database.
    let id: i32 = conn
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .map_err(read_fault)?;
    if id != APPLICATION_ID {
        return Err(Fault::Damage(
            "it is not a zWorkTracker database".to_owned(),
        ));
    }
    quick_check(&conn)?;
    let current = current_version(&conn).map_err(read_fault)?;
    let Some(current) = current else {
        return Err(Fault::Damage("it has no schema version".to_owned()));
    };
    let newest = newest_of(migrations);
    if current > i64::from(newest) {
        return Err(Fault::Fail(CoreError::FutureVersion {
            found: u64::try_from(current).unwrap_or(u64::MAX),
            supported: newest.into(),
        }));
    }
    if current < 1 {
        return Err(Fault::Damage("its schema version is not valid".to_owned()));
    }
    let head = read_head(&conn)?;
    check_head_version(&head)?;
    // Every row is read before anything is written, so a database that turns out damaged is set aside as it was.
    let read_now = current == i64::from(newest);
    let early = if read_now {
        Some(load_file(&conn, head)?)
    } else {
        None
    };
    conn.pragma_update(None, "query_only", false)
        .map_err(read_fault)?;
    make_durable(&conn)?;
    migrate(&mut conn, dir, now, migrations).map_err(Fault::Fail)?;
    let (file, beat) = match early {
        Some(loaded) => loaded,
        None => load_file(&conn, read_head(&conn)?)?,
    };
    let created_at = read_meta_ts(&conn, "created_at")?;
    // Trusted now: a clean close may checkpoint the WAL into the database again.
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, false)
        .map_err(read_fault)?;
    Ok(Loaded {
        conn,
        file,
        beat,
        created_at,
    })
}

impl Loaded {
    fn opened(
        self,
        dir: &Path,
        now: Ts,
        origin: Origin,
        notices: Vec<(&'static str, String)>,
    ) -> Result<Opened, CoreError> {
        let mut file = self.file;
        // Never past now: a clock put back after a beat must not give a recovered segment time that hasn't happened.
        if let Some(beat) = self.beat {
            file.last_heartbeat = file.last_heartbeat.max(Some(beat)).map(|t| t.min(now));
        }
        let store = Store {
            conn: self.conn,
            dir: dir.to_path_buf(),
            beat: self.beat,
            full_write_due: false,
            head: head_json(&file)?,
            created_at: self.created_at,
            #[cfg(any(test, feature = "test-hooks"))]
            saves_blocked: new_stores_blocked(),
            #[cfg(test)]
            fail_after_ops: None,
            #[cfg(test)]
            last_save: SaveStats::default(),
        };
        Ok(Opened {
            store,
            file,
            raw_beat: self.beat,
            origin,
            notices,
        })
    }
}

/// Opens a database this run has just made. Damage now can only be the disk's.
fn reopen(
    dir: &Path,
    now: Ts,
    origin: Origin,
    notices: Vec<(&'static str, String)>,
) -> Result<Opened, CoreError> {
    match open_existing(dir, now, MIGRATIONS) {
        Ok(loaded) => loaded.opened(dir, now, origin, notices),
        Err(Fault::Damage(why)) => Err(CoreError::Read(io::Error::other(format!(
            "the new data file can't be read back ({why})"
        )))),
        Err(Fault::Fail(e)) => Err(e),
    }
}

/// Writes a complete database for `file` at `dir/z-work-data.db.new`, then renames it into place: `z-work-data.db`
/// exists only once complete, and a crash at any step leaves none, so the next launch starts this again.
fn create(
    dir: &Path,
    now: Ts,
    file: &LocalFile,
    created_from: &str,
    created_at: Option<Ts>,
) -> Result<(), CoreError> {
    check_core(&file.data).map_err(|e| {
        CoreError::Validation(format!(
            "refusing to save data that cannot be read back: {e}"
        ))
    })?;
    let mut ops = Vec::new();
    put_data(&file.data, &mut ops, &mut None)?;
    let head = head_json(file)?;
    clear_new(dir)?;
    move_orphans(dir, now)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let mut conn =
        Connection::open_with_flags(sqlite_path(dir, NEW_FILE)?, flags).map_err(write_error)?;
    conn.busy_timeout(BUSY_TIMEOUT).map_err(write_error)?;
    conn.pragma_update(None, "journal_mode", "DELETE")
        .map_err(write_error)?;
    let synced = synced();
    conn.pragma_update(None, "synchronous", if synced { "FULL" } else { "OFF" })
        .map_err(write_error)?;
    conn.pragma_update(None, "fullfsync", synced)
        .map_err(write_error)?;
    migrate(&mut conn, dir, now, MIGRATIONS)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(write_error)?;
    {
        let mut upsert = tx.prepare_cached(UPSERT_RECORD).map_err(write_error)?;
        for op in &ops {
            if let Op::Put { kind, id, json } = op {
                upsert.execute((kind, id, json)).map_err(write_error)?;
            }
        }
        let mut meta = tx.prepare_cached(UPSERT_META).map_err(write_error)?;
        let created = serde_json::to_string(&created_at.unwrap_or(now))?;
        let app = serde_json::to_string(env!("CARGO_PKG_VERSION"))?;
        let from = serde_json::to_string(created_from)?;
        for (key, value) in [
            ("local", head.as_str()),
            ("created_at", created.as_str()),
            ("created_from", from.as_str()),
            ("created_by_app", app.as_str()),
        ] {
            meta.execute((key, value)).map_err(write_error)?;
        }
    }
    tx.commit().map_err(write_error)?;
    // Closing also gets rid of the journal.
    conn.close().map_err(|(_, e)| write_error(e))?;
    let dest = dir.join(DB_FILE);
    persist::with_local_retry(|| fs::rename(dir.join(NEW_FILE), &dest))?;
    persist::sync_parent(&dest);
    Ok(())
}

/// The identity of a 1.3.1 store, the only thing ever taken from it. Also read from a damaged database's `meta.local`.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyIdentity {
    device_id: String,
    device_name: String,
    #[serde(default)]
    prefs: LegacyPrefs,
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LegacyPrefs {
    sync_targets: Vec<SyncTarget>,
    setup_done: bool,
    setup_step: u32,
    sync_prompt_done: bool,
    client_zone_prompt_done: bool,
    /// How many daily backups to keep, carried so a longer retention set in 1.3.1 still holds.
    backup_keep: Option<u32>,
    /// The restore the old store followed, and the folders whose restores it followed: with them a first start
    /// follows what 1.3.1 would have (see `take_identity`).
    #[serde(deserialize_with = "lenient")]
    epoch: Option<EpochInfo>,
    #[serde(deserialize_with = "lenient")]
    sync_joined: Option<Vec<String>>,
}

/// A field of an old store read for what it is worth: a value that doesn't parse is left out, so a damaged one never
/// makes this computer a new device.
fn lenient<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

/// The heartbeat file 1.3.1 kept beside its store: the latest moment a run of it with a timer going was alive. Only
/// ever read, as the rest of 1.3.1's files.
const LEGACY_HEARTBEAT: &str = "z-work-heartbeat.json";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyBeat {
    device_id: String,
    at: Ts,
}

/// Brings `file` up to date with the heartbeat 1.3.1 recorded for it after its store was last saved, as 1.3.1 did with
/// a store just read (its `apply_heartbeat`): never past now, and a file that can't be read or is another device's says
/// nothing. The file is never written or removed.
pub(crate) fn take_legacy_beat(dir: &Path, file: &mut LocalFile, now: Ts) {
    let beat = fs::read(dir.join(LEGACY_HEARTBEAT))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<LegacyBeat>(&bytes).ok())
        .filter(|beat| beat.device_id == file.device_id);
    if let Some(beat) = beat {
        file.last_heartbeat = file.last_heartbeat.max(Some(beat.at)).map(|t| t.min(now));
    }
}

enum LegacyRead {
    None,
    /// `bytes` are those of the first file found, `path`, kept as a backup all the same.
    Unreadable {
        why: String,
        path: PathBuf,
        bytes: Vec<u8>,
    },
    /// `bytes` are those of the file the identity came from.
    Found {
        identity: LegacyIdentity,
        bytes: Vec<u8>,
    },
}

fn parse_identity(bytes: &[u8]) -> Result<LegacyIdentity, String> {
    let identity: LegacyIdentity = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if uuid::Uuid::parse_str(&identity.device_id).is_err() {
        return Err("its device id is not valid".to_owned());
    }
    if identity.device_name.trim().is_empty() {
        return Err("it has no device name".to_owned());
    }
    Ok(identity)
}

/// Reads `z-work-data.json`, else its `.bak`, never writing either. A file that is held or can't be read at all fails
/// the open, so it is retried and no database is made: a lock for a moment must never make this a new device.
fn read_legacy(dir: &Path) -> Result<LegacyRead, CoreError> {
    let mut first: Option<(String, PathBuf, Vec<u8>)> = None;
    for name in [MAIN_FILE, LEGACY_BAK] {
        let path = dir.join(name);
        let Some(bytes) = persist::read_opt(&path).map_err(CoreError::Read)? else {
            continue;
        };
        match parse_identity(&bytes) {
            Ok(identity) => return Ok(LegacyRead::Found { identity, bytes }),
            Err(why) => {
                if first.is_none() {
                    first = Some((why, path, bytes));
                }
            }
        }
    }
    Ok(match first {
        Some((why, path, bytes)) => LegacyRead::Unreadable { why, path, bytes },
        None => LegacyRead::None,
    })
}

/// Takes an identity into a new file. Returns how many sync folders came with it. With any, the data is to come from
/// them, and nothing is written to them until it has. What the old store followed of their restores comes with them:
/// its `epoch`, if valid, and `syncJoined` for the folders kept (none when it has no list). A restore the old store
/// would have followed is then followed, keeping what this device timed since, and one it followed already is not
/// news (`Service::take_own_files`).
fn take_identity(file: &mut LocalFile, identity: LegacyIdentity, now: Ts) -> usize {
    file.device_id = identity.device_id;
    file.device_name = identity.device_name;
    let from = identity.prefs;
    let prefs = &mut file.prefs;
    prefs.setup_done = from.setup_done;
    prefs.setup_step = from.setup_step;
    prefs.sync_prompt_done = from.sync_prompt_done;
    prefs.client_zone_prompt_done = from.client_zone_prompt_done;
    prefs.backup_keep = from.backup_keep.filter(|k| (1..=365).contains(k));
    let mut targets: Vec<SyncTarget> = Vec::new();
    for target in from.sync_targets {
        if !target.root.trim().is_empty() && !targets.iter().any(|t| t.root == target.root) {
            targets.push(target);
        }
    }
    let folders = targets.len();
    if folders > 0 {
        let mut joined: Vec<String> = Vec::new();
        for root in from.sync_joined.unwrap_or_default() {
            if targets.iter().any(|t| t.root == root) && !joined.contains(&root) {
                joined.push(root);
            }
        }
        prefs.epoch = from.epoch.filter(|e| check_epoch(e, now).is_ok());
        prefs.sync_targets = targets;
        prefs.sync_joined = Some(joined);
        prefs.awaiting_cloud = true;
    }
    folders
}

/// The notice a first start with an old store and sync folders shows.
fn arriving_notice(targets: &[SyncTarget]) -> String {
    format!(
        "zWorkTracker now keeps its data in a database on this computer. Your projects and hours are being brought back from {}, and appear after the first sync. The data file of the previous version was left as it was, and a copy of it is in Settings > Backups as {}.",
        folder_list(targets),
        backup::LEGACY_COPY,
    )
}

/// The first start of this version in `dir`: nothing is taken from an old store but its identity, and the data comes
/// from the sync folders (`awaiting_cloud`). The old store's bytes are copied to the backups, read-only on the source.
fn bootstrap(dir: &Path, device_name: &str, now: Ts) -> Result<Opened, CoreError> {
    let mut file = persist::new_local_file(device_name);
    let mut notices = Vec::new();
    let (legacy, created_from, copy) = match read_legacy(dir)? {
        LegacyRead::None => (Legacy::None, "fresh", None),
        LegacyRead::Unreadable { why, path, bytes } => {
            notices.push(("warning", format!(
                "zWorkTracker now keeps its data in a database on this computer. The data file of the previous version couldn't be read ({why}), so this computer starts as a new device. That file was left as it was: {}.",
                path.display()
            )));
            (Legacy::Unreadable { why }, "legacy", Some(bytes))
        }
        LegacyRead::Found { identity, bytes } => {
            let folders = take_identity(&mut file, identity, now);
            // The last moment 1.3.1 was known alive is this device's, as its identity is.
            take_legacy_beat(dir, &mut file, now);
            if folders == 0 {
                notices.push(("warning", format!(
                    "zWorkTracker now keeps its data in a database on this computer, and starts empty because no sync folder was connected. Your earlier data is still on this computer: restore the backup {} in Settings > Backups to bring it back.",
                    backup::LEGACY_COPY
                )));
            } else {
                notices.push(("info", arriving_notice(&file.prefs.sync_targets)));
            }
            (Legacy::Identity { folders }, "legacy", Some(bytes))
        }
    };
    if let Some(bytes) = copy {
        backup::keep_legacy(&Paths::new(dir), &bytes)?;
    }
    create(dir, now, &file, created_from, None)?;
    reopen(dir, now, Origin::Created { legacy }, notices)
}

/// What can still be read from a damaged database, set aside or about to be, best effort, without changing it.
struct SetAside {
    identity: Option<LegacyIdentity>,
    created_at: Option<Ts>,
    /// Its `meta.local`, for anything else worth carrying over.
    head: Value,
}

impl SetAside {
    /// What the set-aside database had seen still counts (addendum A1.4 P15): its newest stamp, so this device's next
    /// stamps are still later than any it wrote or took before, and when it last saved a change by this clock, for the
    /// clock-behind warning. Each is taken only if it can be read, and never lowers what `file` has.
    fn carry_clock(&self, file: &mut LocalFile) {
        if let Some(seen) = self
            .head
            .get("newestStamp")
            .and_then(|v| Stamp::deserialize(v).ok())
        {
            file.newest_stamp = hlc::later(file.newest_stamp.take(), Some(seen));
        }
        if let Some(at) = self
            .head
            .get("latestChangeHere")
            .and_then(|v| Ts::deserialize(v).ok())
        {
            file.latest_change_here = file.latest_change_here.max(Some(at));
        }
    }
}

fn read_set_aside(path: &Path) -> Option<SetAside> {
    let dir = path.parent()?;
    let name = path.file_name()?.to_str()?;
    // `connect` reads only and never checkpoints. A read-only connection can't open a WAL without shared memory.
    let conn = connect(&sqlite_path(dir, name).ok()?).ok()?;
    conn.busy_timeout(Duration::ZERO).ok()?;
    let head: Value = serde_json::from_str(&read_meta(&conn, "local").ok()??).ok()?;
    let identity = LegacyIdentity::deserialize(&head).ok().filter(|i| {
        uuid::Uuid::parse_str(&i.device_id).is_ok() && !i.device_name.trim().is_empty()
    });
    let created_at = read_meta(&conn, "created_at")
        .ok()
        .flatten()
        .and_then(|t| serde_json::from_str(&t).ok());
    Some(SetAside {
        identity,
        created_at,
        head,
    })
}

/// The newest daily backup that parses, for a damaged database with no folder to bring the data back from.
fn newest_backup(dir: &Path) -> Option<(String, LocalFile)> {
    let paths = Paths::new(dir);
    backup::list(&paths)
        .ok()?
        .into_iter()
        .filter(|b| !b.safety && !backup::is_legacy_copy(&b.name))
        .find_map(|b| backup::read(&paths, &b.name).ok().map(|f| (b.name, f)))
}

/// Why a database that an earlier open set aside, and couldn't replace, is replaced now.
const UNFINISHED: &str = "it was set aside by an earlier start, which couldn't finish";

/// A damaged database: set aside, then replaced (`recover_from`). When its own identity can't be read, the old store
/// gives it, so that is read, and copied to the backups, before anything moves: one held for a moment then fails this
/// open with every file where it was, for the retry.
fn replace(dir: &Path, device_name: &str, now: Ts, why: String) -> Result<Opened, CoreError> {
    let own_identity = read_set_aside(&dir.join(DB_FILE)).is_some_and(|old| old.identity.is_some());
    if !own_identity {
        match read_legacy(dir)? {
            LegacyRead::None => {}
            LegacyRead::Unreadable { bytes, .. } | LegacyRead::Found { bytes, .. } => {
                backup::keep_legacy(&Paths::new(dir), &bytes)?;
            }
        }
    }
    let kept_at = set_aside_db(dir, now).map_err(CoreError::Read)?;
    recover_from(dir, device_name, now, &kept_at, why)
}

/// The database set aside most recently, when there is one: `z-work-data.corrupt-<stamp>[-<n>].db`.
fn newest_set_aside(dir: &Path) -> io::Result<Option<PathBuf>> {
    let mut newest: Option<((String, u32), PathBuf)> = None;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(tail) = name
            .to_str()
            .and_then(|n| n.strip_prefix("z-work-data.corrupt-"))
            .and_then(|n| n.strip_suffix(".db"))
        else {
            continue;
        };
        let (stamp, n) = match tail.split_once('-') {
            Some((stamp, n)) => match n.parse::<u32>() {
                Ok(n) => (stamp, n),
                Err(_) => continue,
            },
            None => (tail, 0),
        };
        let key = (stamp.to_owned(), n);
        if newest.as_ref().is_none_or(|(k, _)| key > *k) {
            newest = Some((key, entry.path()));
        }
    }
    Ok(newest.map(|(_, path)| path))
}

/// What a crash between `set_aside_db`'s renames left behind: the files SQLite keeps beside a database, still under the
/// old name, go after the database already set aside, unless it has its own.
fn finish_set_aside(dir: &Path, kept_at: &Path) -> io::Result<()> {
    if exists(&dir.join(DB_FILE)) {
        return Ok(());
    }
    let Some(stem) = kept_at.file_name().and_then(|n| n.to_str()) else {
        return Ok(());
    };
    for suffix in SIDE_SUFFIXES {
        let (from, to) = (
            side_file(dir, DB_FILE, suffix),
            side_file(dir, stem, suffix),
        );
        if exists(&from) && !exists(&to) {
            fs::rename(&from, &to)?;
        }
    }
    Ok(())
}

/// A new database for the one set aside at `kept_at`, with the identity it or the old store gives, and the data from the
/// folders, else from the newest daily backup, else none. Each step reads, or makes only what isn't there yet, so a
/// failure or a crash anywhere in it leaves nothing the next open can't take up again: `Store::open` comes back here
/// for as long as a set-aside database is all there is.
fn recover_from(
    dir: &Path,
    device_name: &str,
    now: Ts,
    kept_at: &Path,
    why: String,
) -> Result<Opened, CoreError> {
    finish_set_aside(dir, kept_at).map_err(CoreError::Read)?;
    let old = read_set_aside(kept_at);
    let mut file = persist::new_local_file(device_name);
    let mut legacy = Legacy::None;
    let mut found = false;
    if let Some(identity) = old.as_ref().and_then(|o| o.identity.clone()) {
        let folders = take_identity(&mut file, identity, now);
        legacy = Legacy::Identity { folders };
        found = true;
    } else {
        match read_legacy(dir)? {
            LegacyRead::None => {}
            LegacyRead::Unreadable { why, bytes, .. } => {
                backup::keep_legacy(&Paths::new(dir), &bytes)?;
                legacy = Legacy::Unreadable { why };
            }
            LegacyRead::Found { identity, bytes } => {
                backup::keep_legacy(&Paths::new(dir), &bytes)?;
                let folders = take_identity(&mut file, identity, now);
                legacy = Legacy::Identity { folders };
                found = true;
            }
        }
    }
    let kept = kept_at.display();
    let mut from_backup = None;
    let notice = if file.prefs.sync_targets.is_empty() {
        if let Some((name, mut restored)) = newest_backup(dir) {
            if found {
                restored.device_id.clone_from(&file.device_id);
                restored.device_name.clone_from(&file.device_name);
            }
            restored.prefs.awaiting_cloud = false;
            file = restored;
            let text = format!(
                "The data file was damaged ({why}). A copy was kept at {kept}. Your data was restored from the backup {name}. Changes made after that backup are missing."
            );
            from_backup = Some(name);
            text
        } else {
            format!(
                "The data file was damaged ({why}). A copy was kept at {kept}. The app started with empty data."
            )
        }
    } else {
        format!(
            "The data file was damaged ({why}). A copy was kept at {kept}. Your data is being brought back from {}, and appears after the first sync.",
            folder_list(&file.prefs.sync_targets)
        )
    };
    if let Some(old) = &old {
        old.carry_clock(&mut file);
    }
    create(
        dir,
        now,
        &file,
        "replaced",
        old.as_ref().and_then(|o| o.created_at),
    )?;
    reopen(
        dir,
        now,
        Origin::Replaced {
            kept_at: kept_at.to_path_buf(),
            why,
            legacy,
            from_backup,
        },
        vec![("warning", notice)],
    )
}

#[cfg(test)]
thread_local! {
    /// Test only: `set_aside_db` on this thread stops after this many renames, as a crash between them would, undoing
    /// nothing.
    static SET_ASIDE_STOPS_AFTER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    /// Test only: stores opened on this thread begin with their saves blocked, so a launch's own save can fail.
    static BLOCK_NEW_STORES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Test only: stores opened on this thread skip their syncs to disk (see `NoSyncStores`).
    static NO_SYNC_STORES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test only: while this lives, stores opened on this thread skip their syncs to disk (`synchronous = OFF`, no
/// F_FULLFSYNC), which is nearly all a save costs. For properties about something else that open hundreds of stores,
/// such as `scenario_hlc_tests`'. Every other test, and every build, keeps the full sync.
#[cfg(test)]
pub(crate) struct NoSyncStores(());

#[cfg(test)]
impl NoSyncStores {
    pub(crate) fn on() -> Self {
        NO_SYNC_STORES.with(|f| f.set(true));
        Self(())
    }
}

#[cfg(test)]
impl Drop for NoSyncStores {
    fn drop(&mut self) {
        NO_SYNC_STORES.with(|f| f.set(false));
    }
}

/// Test only: makes the saves of every store opened on this thread from now on fail, or not.
#[cfg(test)]
pub(crate) fn block_new_stores(blocked: bool) {
    BLOCK_NEW_STORES.with(|b| b.set(blocked));
}

#[cfg(any(test, feature = "test-hooks"))]
fn new_stores_blocked() -> bool {
    #[cfg(test)]
    {
        BLOCK_NEW_STORES.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// The test hooks a save consults, copied out before its transaction begins.
#[derive(Clone, Copy, Default)]
struct Hooks {
    #[cfg(any(test, feature = "test-hooks"))]
    blocked: bool,
    #[cfg(test)]
    fail_after: Option<usize>,
}

impl Hooks {
    /// Inside the transaction, after a statement: a blocked save fails through the same path as a real failure.
    fn check(self, records: usize) -> io::Result<()> {
        #[cfg(any(test, feature = "test-hooks"))]
        if self.blocked {
            return Err(io::Error::other("saves are blocked for a test"));
        }
        #[cfg(test)]
        if self.fail_after.is_some_and(|n| records >= n) {
            return Err(io::Error::other("a save failed halfway for a test"));
        }
        let _ = records;
        Ok(())
    }
}

impl Store {
    /// Opens the store in `dir`, creating it when absent (`bootstrap`) and setting a damaged one aside (`replace`).
    pub fn open(dir: &Path, device_name: &str, now: Ts) -> Result<Opened, CoreError> {
        fs::create_dir_all(dir)?;
        clear_new(dir)?;
        let found = match fs::symlink_metadata(dir.join(DB_FILE)) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // A database set aside by an open that never made the one to replace it: that goes on from there.
                if let Some(kept_at) = newest_set_aside(dir).map_err(CoreError::Read)? {
                    return recover_from(dir, device_name, now, &kept_at, UNFINISHED.to_owned());
                }
                return bootstrap(dir, device_name, now);
            }
            Err(e) => return Err(CoreError::Read(e)),
        };
        if found.file_type().is_symlink() {
            // Renaming the link leaves whatever it points at untouched.
            return replace(dir, device_name, now, "the data file is a link".to_owned());
        }
        if found.is_dir() {
            return Err(CoreError::Read(io::Error::other(
                "the data file is a folder",
            )));
        }
        match open_existing(dir, now, MIGRATIONS) {
            Ok(loaded) => loaded.opened(dir, now, Origin::Existing, Vec::new()),
            Err(Fault::Damage(why)) => replace(dir, device_name, now, why),
            Err(Fault::Fail(e)) => Err(e),
        }
    }

    /// Writes what differs from `prev` to `next` in one transaction. `prev` must be what was last saved or loaded.
    /// Writes nothing, and returns `SaveStats::default()`, when nothing differs.
    pub fn save(&mut self, prev: &LocalFile, next: &LocalFile) -> Result<SaveStats, CoreError> {
        self.write(prev, next, false)
    }

    /// As `save`, for a restore: every meta row is written and `meta.beat` is deleted in the same transaction.
    pub fn save_restored(
        &mut self,
        prev: &LocalFile,
        next: &LocalFile,
    ) -> Result<SaveStats, CoreError> {
        self.write(prev, next, true)
    }

    fn write(
        &mut self,
        prev: &LocalFile,
        next: &LocalFile,
        restored: bool,
    ) -> Result<SaveStats, CoreError> {
        let planned = self.plan(&prev.data, &next.data)?;
        self.save_planned(planned, next, restored)
    }

    /// The first half of a save: the rows it writes, with the newest stamp among them, before anything is written.
    /// `prev` must be what was last saved or loaded. `save_planned` then writes them for that same `next`.
    pub(crate) fn plan<'a>(
        &self,
        prev: &'a Shared,
        next: &'a Shared,
    ) -> Result<Planned<'a>, CoreError> {
        // What cannot be read back must never replace what can.
        check_core(next).map_err(|e| {
            CoreError::Validation(format!(
                "refusing to save data that cannot be read back: {e}"
            ))
        })?;
        plan_rows(prev, next, self.full_write_due)
    }

    /// The second half of a save: writes what `plan` found for `next.data`, and the rest of `next`, in one transaction.
    /// With `restored`, every meta row is written and `meta.beat` is deleted, as `save_restored` does.
    pub(crate) fn save_planned(
        &mut self,
        planned: Planned<'_>,
        next: &LocalFile,
        restored: bool,
    ) -> Result<SaveStats, CoreError> {
        let Planned { ops, full, .. } = planned;
        let head = head_json(next)?;
        let head_changed = full || restored || head != self.head;
        if ops.is_empty() && !head_changed {
            return Ok(SaveStats::default());
        }
        let hooks = self.hooks();
        match self.apply(
            &ops,
            full,
            head_changed.then_some(head.as_str()),
            restored,
            hooks,
        ) {
            Ok(stats) => {
                self.full_write_due = false;
                if head_changed {
                    self.head = head;
                }
                if restored {
                    self.beat = None;
                }
                #[cfg(test)]
                {
                    self.last_save = stats;
                }
                Ok(stats)
            }
            Err(e) => {
                self.full_write_due = true;
                Err(CoreError::Io(e))
            }
        }
    }

    fn hooks(&self) -> Hooks {
        Hooks {
            #[cfg(any(test, feature = "test-hooks"))]
            blocked: self.saves_blocked,
            #[cfg(test)]
            fail_after: self.fail_after_ops,
        }
    }

    fn apply(
        &mut self,
        ops: &[Op<'_>],
        full: bool,
        head: Option<&str>,
        restored: bool,
        hooks: Hooks,
    ) -> io::Result<SaveStats> {
        let sql = |e: rusqlite::Error| sql_io(&e);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        let mut stats = SaveStats::default();
        let mut records = 0;
        {
            if full {
                tx.execute("DELETE FROM records", []).map_err(sql)?;
                hooks.check(records)?;
            }
            let mut upsert = tx.prepare_cached(UPSERT_RECORD).map_err(sql)?;
            let mut delete = tx.prepare_cached(DELETE_RECORD).map_err(sql)?;
            for op in ops {
                match op {
                    Op::Put { kind, id, json } => {
                        upsert.execute((kind, id, json)).map_err(sql)?;
                        stats.upserted += 1;
                    }
                    Op::Del { kind, id } => {
                        delete.execute((kind, id)).map_err(sql)?;
                        stats.deleted += 1;
                    }
                }
                records += 1;
                hooks.check(records)?;
            }
            if let Some(head) = head {
                tx.execute(UPSERT_META, ("local", head)).map_err(sql)?;
                stats.head_written = true;
                hooks.check(records)?;
            }
            if restored {
                tx.execute("DELETE FROM meta WHERE key = 'beat'", [])
                    .map_err(sql)?;
            }
        }
        tx.commit().map_err(sql)?;
        Ok(stats)
    }

    /// A heartbeat in its own small transaction. Never moves `meta.beat` back: an `at` not later than it writes nothing.
    pub fn save_heartbeat(&mut self, at: Ts) -> Result<(), CoreError> {
        if self.beat >= Some(at) {
            return Ok(());
        }
        self.write_beat(at)
    }

    /// Puts `meta.beat` back to `at`, for a clock put back under a running timer: a beat on the old clock would otherwise
    /// outlast every beat on the new one, and run a recovered segment on to it after a crash.
    pub fn rewind_heartbeat(&mut self, at: Ts) -> Result<(), CoreError> {
        if self.beat == Some(at) {
            return Ok(());
        }
        self.write_beat(at)
    }

    fn write_beat(&mut self, at: Ts) -> Result<(), CoreError> {
        let text = serde_json::to_string(&at)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(write_error)?;
        tx.execute(UPSERT_META, ("beat", text))
            .map_err(write_error)?;
        tx.commit().map_err(write_error)?;
        self.beat = Some(at);
        Ok(())
    }

    /// Every row read again through this store's own connection, `last_heartbeat` not cut. For tests and the bench.
    pub fn reload(&self) -> Result<LocalFile, CoreError> {
        let read = || -> Result<LocalFile, Fault> {
            let (mut file, beat) = load_file(&self.conn, read_head(&self.conn)?)?;
            file.last_heartbeat = file.last_heartbeat.max(beat);
            Ok(file)
        };
        read().map_err(|f| match f {
            Fault::Damage(why) => CoreError::Read(io::Error::other(why)),
            Fault::Fail(e) => e,
        })
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// When the database was first made. Backups dated on or before that day may be 1.3.1's, and retention leaves them.
    #[must_use]
    pub fn created_at(&self) -> Option<Ts> {
        self.created_at
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn block_saves(&mut self, blocked: bool) {
        self.saves_blocked = blocked;
    }

    #[cfg(test)]
    pub(crate) fn total_changes(&self) -> u64 {
        self.conn.total_changes()
    }

    #[cfg(test)]
    pub(crate) fn last_save(&self) -> SaveStats {
        self.last_save
    }

    /// Test only: every row of `records` as it is stored (kind, id, JSON text), by kind and id.
    #[cfg(test)]
    pub(crate) fn rows(&self) -> Vec<(String, String, String)> {
        let mut stmt = self
            .conn
            .prepare("SELECT kind, id, json FROM records ORDER BY kind, id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
}

/// Writes a complete database for `file` into `dir`, as this app would have saved it, with `created_from = "seed"`.
/// For tests, the bench and `perf_bench write-store`. Refuses (`CoreError::Conflict`) a `dir` that already holds one.
pub fn seed_store(dir: &Path, file: &LocalFile, now: Ts) -> Result<(), CoreError> {
    fs::create_dir_all(dir)?;
    if exists(&dir.join(DB_FILE)) {
        return Err(CoreError::Conflict(format!(
            "{} already holds a data file",
            dir.display()
        )));
    }
    create(dir, now, file, "seed", None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::tests::{ctx, project_input, task_input};
    use crate::model::{Segment, SyncProvider};

    const NOW: Ts = Ts(1_800_000_000);

    fn with_project(name: &str) -> LocalFile {
        let mut f = persist::new_local_file("pc");
        f.data
            .create_project(&ctx("2026-01-01T00:00:00Z"), &project_input(name))
            .unwrap();
        f
    }

    fn open(dir: &Path) -> Opened {
        Store::open(dir, "pc", NOW).unwrap()
    }

    fn seeded(f: &LocalFile) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        seed_store(dir.path(), f, NOW).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    fn corrupt_files(dir: &Path) -> Vec<String> {
        names(dir)
            .into_iter()
            .filter(|n| n.contains(".corrupt-"))
            .collect()
    }

    /// A plain connection, for a test to change a closed database behind the store's back.
    fn raw(dir: &Path) -> Connection {
        Connection::open(dir.join(DB_FILE)).unwrap()
    }

    fn db_bytes(dir: &Path) -> Vec<u8> {
        fs::read(dir.join(DB_FILE)).unwrap()
    }

    /// The database and its WAL as a crash would leave them, copied while the store is open.
    fn crash_image(dir: &Path) -> tempfile::TempDir {
        let image = tempfile::tempdir().unwrap();
        for suffix in ["", "-wal"] {
            let from = dir.join(format!("{DB_FILE}{suffix}"));
            if from.exists() {
                fs::copy(&from, image.path().join(format!("{DB_FILE}{suffix}"))).unwrap();
            }
        }
        image
    }

    fn folder(root: &str) -> SyncTarget {
        SyncTarget {
            provider: SyncProvider::OneDrive,
            root: root.to_owned(),
        }
    }

    fn notice(opened: &Opened) -> &str {
        assert_eq!(opened.notices.len(), 1, "{:?}", opened.notices);
        opened.notices[0].1.as_str()
    }

    fn replaced(opened: &Opened) -> (PathBuf, String) {
        match &opened.origin {
            Origin::Replaced { kept_at, why, .. } => (kept_at.clone(), why.clone()),
            other => panic!("not replaced: {other:?}"),
        }
    }

    fn segment(id: &str, task: &str, start: i64) -> Segment {
        serde_json::from_value(serde_json::json!({
            "id": id, "taskId": task, "deviceId": "device-a",
            "start": Ts(start).iso(), "end": Ts(start + 1800).iso(),
        }))
        .unwrap()
    }

    /// 1: was `a_phase_1_store_loads_keeps_its_data_and_a_copy_of_the_original` (the JSON half stays in persist.rs).
    #[test]
    fn a_schema_migration_keeps_a_copy_of_the_database_before_it_runs() {
        let f = with_project("One");
        let dir = seeded(&f);
        let rows = |conn: &Connection| -> Vec<(String, String, String)> {
            let mut stmt = conn
                .prepare("SELECT kind, id, json FROM records ORDER BY kind, id")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        let before = rows(&raw(dir.path()));
        assert!(!before.is_empty());
        // A STRICT table needs a type for every column.
        let test_migrations: &[(u32, &str)] =
            &[(1, MIGRATION_1), (2, "CREATE TABLE t2(x TEXT) STRICT;")];
        let loaded = open_existing(dir.path(), NOW, test_migrations)
            .ok()
            .unwrap();
        assert_eq!(loaded.file.data, f.data);
        let current: i64 = loaded
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(current, 2);
        drop(loaded);
        let copy = dir.path().join("z-work-data.v1.db");
        let kept = fs::read(&copy).unwrap();
        let copied = Connection::open(&copy).unwrap();
        assert_eq!(rows(&copied), before);
        let version: i64 = copied
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1, "the copy is from before the migration");
        drop(copied);
        // The second open has nothing to migrate, so it makes no new copy.
        let again = open_existing(dir.path(), NOW, test_migrations)
            .ok()
            .unwrap();
        drop(again);
        assert_eq!(fs::read(&copy).unwrap(), kept);
        assert!(!dir.path().join("z-work-data.v2.db").exists());
    }

    /// 3: was `fresh_then_round_trip_keeps_one_backup`.
    #[test]
    fn fresh_then_round_trip_returns_the_same_file() {
        let f = with_project("One");
        let dir = seeded(&f);
        let opened = open(dir.path());
        assert_eq!(opened.origin, Origin::Existing);
        assert!(opened.notices.is_empty());
        assert_eq!(opened.file, f);
        assert_eq!(opened.store.reload().unwrap(), f);
        drop(opened);
        assert_eq!(
            names(dir.path()),
            [DB_FILE],
            "no .bak, temporary or JSON file"
        );
    }

    /// 4: was `corrupt_main_recovers_from_backup_and_keeps_the_damaged_file`.
    #[test]
    fn a_damaged_database_is_set_aside_with_its_wal_and_recovers_from_the_newest_daily_backup() {
        let f = with_project("One");
        let dir = seeded(&f);
        let mut opened = open(dir.path());
        let mut next = f.clone();
        next.data
            .create_project(&ctx("2026-01-02T00:00:00Z"), &project_input("Two"))
            .unwrap();
        opened.store.save(&f, &next).unwrap();
        let image = crash_image(dir.path());
        drop(opened);
        let paths = Paths::new(image.path());
        let older = with_project("Older backup");
        let newer = with_project("Newer backup");
        backup::daily(
            &paths,
            &older,
            Ts::parse("2027-01-01T10:00:00Z").unwrap(),
            30,
        )
        .unwrap();
        backup::daily(
            &paths,
            &newer,
            Ts::parse("2027-01-02T10:00:00Z").unwrap(),
            30,
        )
        .unwrap();
        // Neither of these is a daily backup.
        backup::safety_copy(
            &paths,
            &with_project("Safety"),
            Ts::parse("2027-01-03T10:00:00Z").unwrap(),
        )
        .unwrap();
        backup::keep_legacy(
            &paths,
            &serde_json::to_vec(&with_project("Legacy")).unwrap(),
        )
        .unwrap();
        // Page 2 garbled, as a bad sector would leave it. The WAL holds the last save, and goes with the file.
        let mut damaged = db_bytes(image.path());
        damaged[4096..8192].fill(0xA5);
        fs::write(image.path().join(DB_FILE), &damaged).unwrap();
        let wal = fs::read(image.path().join(format!("{DB_FILE}-wal"))).unwrap();
        assert!(!wal.is_empty());

        let opened = open(image.path());
        let (kept_at, why) = replaced(&opened);
        assert_eq!(fs::read(&kept_at).unwrap(), damaged);
        assert_eq!(
            fs::read(format!("{}-wal", kept_at.display())).unwrap(),
            wal,
            "the WAL is set aside with it, unchanged"
        );
        assert_eq!(opened.file.data, newer.data);
        assert_eq!(
            opened.file.device_id, f.device_id,
            "the identity is the damaged store's"
        );
        assert_eq!(
            notice(&opened),
            format!(
                "The data file was damaged ({why}). A copy was kept at {}. Your data was restored from the backup z-work-data-2027-01-02.json. Changes made after that backup are missing.",
                kept_at.display()
            )
        );
        assert_eq!(opened.notices[0].0, "warning");
        drop(opened);
        assert_eq!(open(image.path()).origin, Origin::Existing);
    }

    /// 5: was `truncated_main_and_empty_main_recover`.
    #[test]
    fn a_truncated_or_empty_database_file_is_set_aside_and_replaced() {
        for len in [100, 0] {
            let dir = seeded(&with_project("One"));
            let bytes = db_bytes(dir.path());
            fs::write(dir.path().join(DB_FILE), &bytes[..len]).unwrap();
            let opened = open(dir.path());
            let (kept_at, _) = replaced(&opened);
            assert_eq!(fs::read(&kept_at).unwrap(), &bytes[..len]);
            assert_eq!(corrupt_files(dir.path()).len(), 1, "{len}");
            drop(opened);
            assert_eq!(open(dir.path()).origin, Origin::Existing, "{len}");
        }
    }

    /// 6: was `both_corrupt_keeps_both_and_says_so`.
    #[test]
    fn a_damaged_database_with_no_backup_and_no_folder_starts_empty_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(DB_FILE), b"junk that is no database at all").unwrap();
        let opened = open(dir.path());
        let (kept_at, why) = replaced(&opened);
        assert!(opened.file.data.projects.is_empty());
        assert_eq!(
            notice(&opened),
            format!(
                "The data file was damaged ({why}). A copy was kept at {}. The app started with empty data.",
                kept_at.display()
            )
        );
        assert_eq!(
            fs::read(&kept_at).unwrap(),
            b"junk that is no database at all"
        );
        assert_eq!(corrupt_files(dir.path()).len(), 1);
    }

    /// 7: was `missing_main_uses_backup`.
    #[test]
    fn a_missing_database_is_created_from_the_legacy_identity_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut legacy = with_project("One");
        legacy.device_name = "Studio Mac".to_owned();
        legacy.prefs.sync_targets = vec![folder("/cloud/OneDrive")];
        legacy.prefs.setup_done = true;
        legacy.prefs.sync_prompt_done = true;
        legacy.prefs.theme = crate::model::Theme::Dark;
        let bytes = serde_json::to_vec(&legacy).unwrap();
        fs::write(dir.path().join(MAIN_FILE), &bytes).unwrap();
        let opened = open(dir.path());
        assert_eq!(
            opened.origin,
            Origin::Created {
                legacy: Legacy::Identity { folders: 1 }
            }
        );
        let file = &opened.file;
        assert_eq!(file.device_id, legacy.device_id);
        assert_eq!(file.device_name, "Studio Mac");
        assert_eq!(file.prefs.sync_targets, legacy.prefs.sync_targets);
        assert!(file.prefs.setup_done && file.prefs.sync_prompt_done);
        assert_eq!(file.prefs.theme, crate::model::Theme::Light, "nothing else");
        assert!(file.prefs.awaiting_cloud);
        assert_eq!(file.prefs.sync_joined, Some(Vec::new()));
        assert_eq!(
            file.data,
            Shared::default(),
            "no data: it comes from the folder"
        );
        assert_eq!(
            opened.notices,
            [(
                "info",
                "zWorkTracker now keeps its data in a database on this computer. Your projects and hours are being brought back from OneDrive, and appear after the first sync. The data file of the previous version was left as it was, and a copy of it is in Settings > Backups as z-work-data-legacy-1.3.1.json.".to_owned()
            )]
        );
        assert_eq!(fs::read(dir.path().join(MAIN_FILE)).unwrap(), bytes);
    }

    /// 8: was `newer_version_is_refused_and_untouched`.
    #[test]
    fn a_newer_database_schema_or_model_version_is_refused_and_left_untouched() {
        let schema = seeded(&with_project("One"));
        raw(schema.path())
            .execute(
                "INSERT INTO schema_version(version, applied_at) VALUES (99, 'later')",
                [],
            )
            .unwrap();
        let model = seeded(&with_project("One"));
        raw(model.path())
            .execute(
                "UPDATE meta SET value = json_set(value, '$.version', 3) WHERE key = 'local'",
                [],
            )
            .unwrap();
        for (dir, found, supported) in [(&schema, 99, 1), (&model, 3, SCHEMA_VERSION)] {
            let before = names(dir.path());
            let bytes = db_bytes(dir.path());
            let refused = Store::open(dir.path(), "pc", NOW).unwrap_err();
            assert_eq!(refused.code(), "future_version");
            assert!(
                matches!(refused, CoreError::FutureVersion { found: f, supported: s } if f == found && s == supported),
                "{refused:?}"
            );
            assert_eq!(db_bytes(dir.path()), bytes);
            assert_eq!(names(dir.path()), before, "nothing set aside or added");
        }
    }

    /// Compat: up to 1.3.2 the device row also held `prefs.mica`, the Windows 11 backdrop 1.3.3 removed. Such a
    /// database opens as it is with nothing lost, the key is ignored, and the next write of the row leaves it out.
    #[test]
    fn a_device_row_from_1_3_2_with_the_mica_preference_opens_and_loses_nothing() {
        let device_row = |dir: &Path| -> Value {
            let text: String = raw(dir)
                .query_row("SELECT value FROM meta WHERE key = 'local'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            serde_json::from_str(&text).unwrap()
        };
        let mut f = with_project("One");
        f.prefs.theme = crate::model::Theme::Dark;
        f.prefs.hide_earnings = true;
        f.prefs.setup_done = true;
        let dir = seeded(&f);
        raw(dir.path())
            .execute(
                "UPDATE meta SET value = json_set(value, '$.prefs.mica', json('true')) WHERE key = 'local'",
                [],
            )
            .unwrap();
        assert_eq!(device_row(dir.path())["prefs"]["mica"], Value::Bool(true));

        let mut opened = open(dir.path());
        assert_eq!(opened.origin, Origin::Existing);
        assert!(opened.notices.is_empty(), "{:?}", opened.notices);
        assert_eq!(opened.file.prefs, f.prefs);
        assert_eq!(opened.file.data, f.data);
        assert_eq!(
            serde_json::to_value(&opened.file.prefs)
                .unwrap()
                .get("mica"),
            None
        );

        let mut next = opened.file.clone();
        next.prefs.hide_earnings = false;
        opened.store.save(&opened.file, &next).unwrap();
        drop(opened);
        let written = device_row(dir.path());
        assert_eq!(written["prefs"].get("mica"), None, "{written}");
        assert_eq!(written["prefs"]["theme"], "dark");
        assert_eq!(open(dir.path()).file.prefs, next.prefs);
    }

    /// 9: was `a_heartbeat_touches_only_its_own_file_and_is_read_back_on_load`.
    #[test]
    fn a_heartbeat_writes_one_meta_row_and_is_read_back_on_load() {
        let mut f = with_project("One");
        f.last_heartbeat = Some(Ts(NOW.0 - 60));
        let dir = seeded(&f);
        let mut opened = open(dir.path());
        let changes = opened.store.total_changes();
        opened.store.save_heartbeat(NOW).unwrap();
        assert_eq!(opened.store.total_changes() - changes, 1);
        assert_eq!(opened.store.reload().unwrap().data, f.data);
        drop(opened);
        assert_eq!(open(dir.path()).file.last_heartbeat, Some(NOW));
    }

    /// 10: was `a_heartbeat_never_moves_the_store_backwards_or_crosses_stores`. One database per folder: no crossing.
    #[test]
    fn a_heartbeat_never_moves_the_stored_one_backwards() {
        let dir = seeded(&with_project("One"));
        let mut opened = open(dir.path());
        opened.store.save_heartbeat(NOW).unwrap();
        let changes = opened.store.total_changes();
        opened.store.save_heartbeat(Ts(NOW.0 - 600)).unwrap();
        opened.store.save_heartbeat(NOW).unwrap();
        assert_eq!(opened.store.total_changes(), changes, "nothing written");
        drop(opened);
        assert_eq!(open(dir.path()).file.last_heartbeat, Some(NOW));
    }

    /// 11: was `a_real_save_keeps_the_heartbeat_for_the_backup_and_a_rollback_can_clear_it`.
    #[test]
    fn a_save_keeps_a_later_beat_and_a_restore_save_clears_it() {
        let f = with_project("One");
        let dir = seeded(&f);
        let mut opened = open(dir.path());
        opened.store.save_heartbeat(NOW).unwrap();
        let mut older = f.clone();
        older.last_heartbeat = Some(Ts(NOW.0 - 3600));
        opened.store.save(&f, &older).unwrap();
        assert_eq!(opened.store.reload().unwrap().last_heartbeat, Some(NOW));
        opened.store.save_restored(&older, &older).unwrap();
        assert_eq!(
            opened.store.reload().unwrap().last_heartbeat,
            Some(Ts(NOW.0 - 3600))
        );
        drop(opened);
        assert_eq!(open(dir.path()).file.last_heartbeat, Some(Ts(NOW.0 - 3600)));
    }

    /// 12: was `the_first_beat_after_a_save_brings_the_backup_up_to_date_and_the_next_leaves_it`.
    #[test]
    fn a_committed_save_survives_a_crash_without_a_clean_close() {
        let f = with_project("One");
        let dir = seeded(&f);
        let mut opened = open(dir.path());
        let mut next = f.clone();
        let two = next
            .data
            .create_project(&ctx("2026-01-02T00:00:00Z"), &project_input("Two"))
            .unwrap();
        opened.store.save(&f, &next).unwrap();
        let image = crash_image(dir.path());
        assert!(image.path().join(format!("{DB_FILE}-wal")).exists());
        let recovered = open(image.path());
        assert_eq!(recovered.origin, Origin::Existing);
        assert!(recovered.file.data.projects.contains_key(&two));
        assert_eq!(recovered.file, next);
        drop(opened);
    }

    /// 13: was `a_beat_never_copies_a_main_file_damaged_behind_the_app_over_the_backup`.
    #[test]
    fn a_failed_save_leaves_the_database_as_it_was() {
        let f = with_project("One");
        let dir = seeded(&f);
        let mut opened = open(dir.path());
        let mut next = f.clone();
        for name in ["Two", "Three"] {
            next.data
                .create_project(&ctx("2026-01-02T00:00:00Z"), &project_input(name))
                .unwrap();
        }
        opened.store.fail_after_ops = Some(1);
        let failed = opened.store.save(&f, &next).unwrap_err();
        assert_eq!(failed.code(), "io");
        assert_eq!(opened.store.reload().unwrap(), f);
        assert!(opened.store.full_write_due);
    }

    /// 14: same name as 1.3.1's.
    #[test]
    fn a_heartbeat_past_now_is_not_believed() {
        let dir = seeded(&with_project("One"));
        let mut opened = open(dir.path());
        opened.store.save_heartbeat(Ts(NOW.0 + 7200)).unwrap();
        drop(opened);
        let opened = open(dir.path());
        assert_eq!(opened.file.last_heartbeat, Some(NOW));
        assert_eq!(opened.raw_beat, Some(Ts(NOW.0 + 7200)));
    }

    /// 15: was `a_store_recovered_from_the_backup_still_knows_the_latest_heartbeat`.
    #[test]
    fn a_store_reopened_after_a_crash_knows_the_latest_heartbeat() {
        let f = with_project("One");
        let dir = seeded(&f);
        let mut opened = open(dir.path());
        opened.store.save_heartbeat(NOW).unwrap();
        let image = crash_image(dir.path());
        let recovered = open(image.path());
        assert_eq!(recovered.file.device_id, f.device_id);
        assert_eq!(recovered.file.last_heartbeat, Some(NOW));
        drop(opened);
    }

    /// 17: was `a_store_that_did_not_change_is_not_written_again`.
    #[test]
    fn an_unchanged_file_writes_nothing() {
        let f = with_project("One");
        let dir = seeded(&f);
        let mut opened = open(dir.path());
        let changes = opened.store.total_changes();
        assert_eq!(opened.store.save(&f, &f).unwrap(), SaveStats::default());
        assert_eq!(opened.store.total_changes(), changes);
    }

    /// 18: was `a_file_changed_behind_the_app_is_parsed_again_even_when_it_still_parses`. Nothing can change the file
    /// behind the app: the database is locked for it alone.
    #[test]
    fn a_second_store_cannot_open_a_database_in_use() {
        let f = with_project("One");
        let dir = seeded(&f);
        let mut first = open(dir.path());
        let started = std::time::Instant::now();
        let second = Store::open(dir.path(), "pc", NOW).unwrap_err();
        assert!(second.is_held(), "{second:?}");
        assert!(started.elapsed() < Duration::from_secs(15));
        assert!(corrupt_files(dir.path()).is_empty());
        let next = with_project("Two");
        first.store.save(&f, &next).unwrap();
        assert_eq!(first.store.reload().unwrap().data, next.data);
    }

    /// 19: was `a_file_that_already_held_these_bytes_is_still_checked_before_it_is_trusted`.
    #[test]
    fn every_open_checks_the_pages_and_parses_every_row() {
        let dir = seeded(&with_project("One"));
        raw(dir.path())
            .execute(
                "INSERT INTO records(kind, id, json) VALUES ('projects', 'x', '{not json')",
                [],
            )
            .unwrap();
        let opened = open(dir.path());
        let (_, why) = replaced(&opened);
        assert!(why.contains("projects record x"), "{why}");
        assert!(!opened.file.data.projects.contains_key("x"));
        assert_eq!(corrupt_files(dir.path()).len(), 1);
    }

    /// 20: was `rotating_leaves_the_backup_and_the_main_file_independent`.
    #[test]
    fn only_changed_rows_are_written() {
        let mut f = persist::new_local_file("pc");
        let c = ctx("2026-01-01T00:00:00Z");
        let p = f.data.create_project(&c, &project_input("P")).unwrap();
        let t = f.data.create_task(&c, &task_input("T", &p)).unwrap();
        for i in 0..1000 {
            let s = segment(&format!("s{i:04}"), &t, 1_700_000_000 + i * 3600);
            f.data.segments.insert(s.id.clone(), s);
        }
        let dir = seeded(&f);
        let mut opened = open(dir.path());
        let mut next = f.clone();
        if let Some(s) = next.data.segments.get_mut("s0500") {
            s.end = s.end.map(|e| e.later(60));
        }
        next.data.segments.remove("s0700");
        let added = segment("s1000", &t, 1_700_000_000 + 1000 * 3600);
        next.data.segments.insert(added.id.clone(), added);
        assert_eq!(
            opened.store.save(&f, &next).unwrap(),
            SaveStats {
                upserted: 2,
                deleted: 1,
                head_written: false
            }
        );
        assert_eq!(opened.store.reload().unwrap(), next);
    }

    /// 21: was `a_main_file_that_is_a_link_is_copied_to_the_backup_and_its_target_is_untouched`.
    #[cfg(unix)]
    #[test]
    fn a_database_path_that_is_a_link_is_set_aside_and_its_target_untouched() {
        let elsewhere = seeded(&with_project("Elsewhere"));
        let target = elsewhere.path().join(DB_FILE);
        let bytes = fs::read(&target).unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(DB_FILE)).unwrap();
        let opened = open(dir.path());
        let (kept_at, why) = replaced(&opened);
        assert_eq!(why, "the data file is a link");
        assert!(
            fs::symlink_metadata(&kept_at)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link itself was moved"
        );
        assert_eq!(fs::read(&target).unwrap(), bytes);
        assert!(opened.file.data.projects.is_empty());
        assert!(
            fs::symlink_metadata(dir.path().join(DB_FILE))
                .unwrap()
                .file_type()
                .is_file()
        );
    }

    /// 22: was `a_blocked_backup_does_not_stop_the_save`.
    #[test]
    fn a_save_after_a_failed_one_rewrites_every_row() {
        let f = with_project("One");
        let dir = seeded(&f);
        let mut opened = open(dir.path());
        let mut next = f.clone();
        next.data
            .create_project(&ctx("2026-01-02T00:00:00Z"), &project_input("Two"))
            .unwrap();
        opened.store.fail_after_ops = Some(1);
        assert!(opened.store.save(&f, &next).is_err());
        opened.store.fail_after_ops = None;
        let stats = opened.store.save(&f, &next).unwrap();
        assert_eq!(stats.upserted, record_count(&next.data));
        assert!(stats.head_written);
        assert!(!opened.store.full_write_due);
        assert_eq!(opened.store.reload().unwrap(), next);
    }

    /// 23: was `a_planted_link_at_the_temporary_path_is_not_followed`. The store writes no temporary file, and a
    /// leftover `.new` is removed, never followed (see `persist::tests` for `write_atomic`'s own case).
    #[test]
    fn creation_replaces_a_leftover_temporary_database() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(NEW_FILE), b"junk").unwrap();
        fs::write(dir.path().join(format!("{NEW_FILE}-journal")), b"junk").unwrap();
        let opened = open(dir.path());
        assert_eq!(
            opened.origin,
            Origin::Created {
                legacy: Legacy::None
            }
        );
        assert!(!dir.path().join(NEW_FILE).exists());
        assert!(!dir.path().join(format!("{NEW_FILE}-journal")).exists());
        drop(opened);
        assert_eq!(open(dir.path()).origin, Origin::Existing);
    }

    /// 24: same name as 1.3.1's.
    #[test]
    fn data_that_cannot_be_read_back_is_refused_and_the_store_survives() {
        let good = with_project("One");
        let dir = seeded(&good);
        let mut opened = open(dir.path());
        let mut bad = with_project("Two");
        let (id, project) = bad.data.projects.pop_first().unwrap();
        bad.data.projects.insert(format!("{id}-other"), project);
        let refused = opened.store.save(&good, &bad).unwrap_err();
        assert_eq!(refused.code(), "validation");
        assert!(
            refused
                .to_string()
                .starts_with("refusing to save data that cannot be read back"),
            "{refused}"
        );
        assert_eq!(opened.store.reload().unwrap(), good);
        assert!(!opened.store.full_write_due, "nothing reached the disk");
        assert!(corrupt_files(dir.path()).is_empty());
    }

    /// 25: was `damaged_main_never_replaces_a_good_backup`.
    #[test]
    fn a_damaged_database_is_moved_aside_never_overwritten() {
        let dir = seeded(&with_project("One"));
        let mut damaged = db_bytes(dir.path());
        damaged[100..200].fill(0);
        damaged[4096..4196].fill(0xFF);
        fs::write(dir.path().join(DB_FILE), &damaged).unwrap();
        let opened = open(dir.path());
        let (kept_at, _) = replaced(&opened);
        assert_eq!(fs::read(&kept_at).unwrap(), damaged);
        drop(opened);
        assert_ne!(
            db_bytes(dir.path()),
            damaged,
            "a new file, not the damaged one"
        );
        assert_eq!(corrupt_files(dir.path()).len(), 1);
    }

    /// 26: was `r12_a_backup_that_cannot_be_read_does_not_stop_a_good_main_file_loading`.
    #[test]
    fn r12_a_good_database_opens_without_reading_backups_or_the_legacy_file() {
        let f = with_project("One");
        let dir = seeded(&f);
        fs::write(dir.path().join("backups"), b"a file where the folder goes").unwrap();
        fs::write(dir.path().join(MAIN_FILE), b"junk").unwrap();
        fs::create_dir(dir.path().join(LEGACY_BAK)).unwrap();
        let opened = open(dir.path());
        assert_eq!(opened.origin, Origin::Existing);
        assert!(opened.notices.is_empty());
        assert_eq!(opened.file, f);
    }

    /// 27: was `r12_a_read_error_says_read_and_not_save`.
    #[test]
    fn r12_a_database_that_cannot_be_opened_says_read_and_not_save() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(DB_FILE)).unwrap();
        let e = Store::open(dir.path(), "pc", NOW).unwrap_err();
        let text = e.to_string();
        assert!(text.contains("read"), "{text}");
        assert!(!text.contains("save"), "{text}");
        assert_eq!(e.code(), "io");
    }

    /// 28: was `r12_a_held_backup_leaves_a_damaged_main_file_where_it_is_for_the_next_launch`.
    #[cfg(unix)]
    #[test]
    fn r12_a_damaged_database_that_cannot_be_moved_stays_for_the_next_launch() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(DB_FILE), b"junk").unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let e = Store::open(dir.path(), "pc", NOW).unwrap_err();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(e, CoreError::Read(_)), "{e:?}");
        assert!(e.to_string().contains("read"), "{e}");
        assert_eq!(db_bytes(dir.path()), b"junk");
        assert!(corrupt_files(dir.path()).is_empty());
        assert_eq!(names(dir.path()), [DB_FILE], "nothing new");
        // With the folder writable again, the next launch recovers as it always did.
        let opened = open(dir.path());
        replaced(&opened);
        assert_eq!(corrupt_files(dir.path()).len(), 1);
    }

    /// 29: was `r12_a_missing_main_file_with_a_held_backup_does_not_start_empty`.
    #[cfg(unix)]
    #[test]
    fn r12_a_held_legacy_file_creates_no_database() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let legacy = with_project("One");
        let main = dir.path().join(MAIN_FILE);
        fs::write(&main, serde_json::to_vec(&legacy).unwrap()).unwrap();
        fs::set_permissions(&main, fs::Permissions::from_mode(0o000)).unwrap();
        let e = Store::open(dir.path(), "pc", NOW).unwrap_err();
        fs::set_permissions(&main, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(e.is_held(), "{e:?}");
        assert!(!dir.path().join(DB_FILE).exists());
        let opened = open(dir.path());
        assert_eq!(opened.file.device_id, legacy.device_id);
    }

    /// 31: was `r15_save_flushes_the_folder_after_its_final_rename`.
    #[test]
    fn r15_the_database_runs_in_wal_with_full_sync_exclusive_locking_and_fullfsync() {
        let dir = tempfile::tempdir().unwrap();
        persist::PARENT_SYNCS.with(|s| s.borrow_mut().clear());
        let opened = open(dir.path());
        assert_eq!(
            persist::PARENT_SYNCS.with(|s| s.borrow().clone()),
            vec![dir.path().join(DB_FILE)],
            "creation flushes the folder after its rename"
        );
        let conn = &opened.store.conn;
        let text =
            |name: &str| -> String { conn.pragma_query_value(None, name, |r| r.get(0)).unwrap() };
        let number =
            |name: &str| -> i64 { conn.pragma_query_value(None, name, |r| r.get(0)).unwrap() };
        assert_eq!(text("journal_mode"), "wal");
        assert_eq!(text("locking_mode"), "exclusive");
        assert_eq!(number("synchronous"), 2);
        assert_eq!(number("fullfsync"), 1);
        assert_eq!(number("checkpoint_fullfsync"), 1);
        assert_eq!(number("journal_size_limit"), JOURNAL_SIZE_LIMIT);
        assert_eq!(number("trusted_schema"), 0);
        assert_eq!(number("application_id"), i64::from(APPLICATION_ID));
        assert!(!dir.path().join(format!("{DB_FILE}-shm")).exists());
    }

    /// The test hook that skips the syncs holds only for stores opened while its guard lives.
    #[test]
    fn a_store_skips_its_syncs_only_while_a_test_asks_and_never_after() {
        let number = |o: &Opened, name: &str| -> i64 {
            o.store
                .conn
                .pragma_query_value(None, name, |r| r.get(0))
                .unwrap()
        };
        let dir = seeded(&with_project("One"));
        let quick = {
            let _fast = NoSyncStores::on();
            open(dir.path())
        };
        assert_eq!(number(&quick, "synchronous"), 0);
        assert_eq!(number(&quick, "fullfsync"), 0);
        assert_eq!(number(&quick, "checkpoint_fullfsync"), 0);
        drop(quick);
        let full = open(dir.path());
        assert_eq!(number(&full, "synchronous"), 2);
        assert_eq!(number(&full, "fullfsync"), 1);
        assert_eq!(number(&full, "checkpoint_fullfsync"), 1);
    }

    #[test]
    fn kinds_match_the_maps_of_shared() {
        let value = serde_json::to_value(Shared::default()).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        let mut kinds = KINDS.to_vec();
        kinds.sort_unstable();
        assert_eq!(kinds, keys);
    }

    #[test]
    fn local_head_covers_every_field_of_local_file() {
        let file = with_project("One");
        let without_data = LocalFile {
            data: Shared::default(),
            ..file.clone()
        };
        let mut whole: Vec<String> = serde_json::to_value(&without_data)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| *k != "data")
            .cloned()
            .collect();
        let mut head: Vec<String> = serde_json::to_value(LocalHead::of(&file))
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        whole.sort();
        head.sort();
        assert_eq!(head, whole);
    }

    #[test]
    fn a_new_record_kind_in_the_database_is_damage() {
        let dir = seeded(&with_project("One"));
        raw(dir.path())
            .execute(
                "INSERT INTO records(kind, id, json) VALUES ('widgets', 'w', '{}')",
                [],
            )
            .unwrap();
        let opened = open(dir.path());
        let (_, why) = replaced(&opened);
        assert!(why.contains("unknown kind (widgets)"), "{why}");
    }

    #[test]
    fn seed_store_refuses_a_folder_with_a_database() {
        let f = with_project("One");
        let dir = seeded(&f);
        let bytes = db_bytes(dir.path());
        let refused = seed_store(dir.path(), &with_project("Two"), NOW).unwrap_err();
        assert_eq!(refused.code(), "conflict");
        assert_eq!(db_bytes(dir.path()), bytes);
    }

    #[test]
    fn the_schema_version_is_the_last_migration() {
        assert_eq!(newest_of(MIGRATIONS), DB_SCHEMA_VERSION);
        let dir = seeded(&with_project("One"));
        let version: i64 = raw(dir.path())
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, i64::from(DB_SCHEMA_VERSION));
    }

    /// A1.4 P4: the hook fails inside the transaction, through the same path as a real failure.
    #[test]
    fn a_blocked_save_fails_inside_its_transaction_and_the_next_one_rewrites_every_row() {
        let f = with_project("One");
        let dir = seeded(&f);
        let mut opened = open(dir.path());
        let mut next = f.clone();
        next.device_name = "renamed".to_owned();
        opened.store.block_saves(true);
        let changes = opened.store.total_changes();
        let blocked = opened.store.save(&f, &next).unwrap_err();
        assert_eq!(blocked.code(), "io");
        assert!(!blocked.is_held());
        assert!(opened.store.full_write_due);
        assert_eq!(opened.store.reload().unwrap(), f);
        // The head was written before the hook failed it: the transaction was under way, and rolled back.
        assert!(opened.store.total_changes() > changes);
        opened.store.block_saves(false);
        let stats = opened.store.save(&f, &next).unwrap();
        assert_eq!(stats.upserted, record_count(&next.data));
        assert_eq!(opened.store.reload().unwrap(), next);
    }

    /// A clock put back under a running timer: the beat goes back with it, so beats on the new clock count again.
    #[test]
    fn a_clock_put_back_rewinds_the_beat() {
        let mut f = with_project("One");
        f.last_heartbeat = Some(Ts(NOW.0 - 7200));
        let dir = seeded(&f);
        let mut opened = open(dir.path());
        opened.store.save_heartbeat(NOW).unwrap();
        opened.store.rewind_heartbeat(Ts(NOW.0 - 3600)).unwrap();
        opened.store.save_heartbeat(Ts(NOW.0 - 3570)).unwrap();
        assert_eq!(
            opened.store.reload().unwrap().last_heartbeat,
            Some(Ts(NOW.0 - 3570))
        );
    }

    /// A1.1: the old store's bytes go to the backups once, read-only on the source.
    #[test]
    fn a_new_database_copies_the_legacy_store_to_the_backups_byte_for_byte() {
        for name in [MAIN_FILE, LEGACY_BAK] {
            let dir = tempfile::tempdir().unwrap();
            let legacy = with_project("One");
            let path = dir.path().join(name);
            let bytes = serde_json::to_vec_pretty(&legacy).unwrap();
            fs::write(&path, &bytes).unwrap();
            let mtime = fs::metadata(&path).unwrap().modified().unwrap();
            std::thread::sleep(Duration::from_millis(20));
            let opened = open(dir.path());
            assert_eq!(
                opened.origin,
                Origin::Created {
                    legacy: Legacy::Identity { folders: 0 }
                }
            );
            let copy = dir.path().join("backups").join(backup::LEGACY_COPY);
            assert_eq!(fs::read(&copy).unwrap(), bytes, "{name}");
            assert_eq!(fs::read(&path).unwrap(), bytes, "{name}");
            assert_eq!(
                fs::metadata(&path).unwrap().modified().unwrap(),
                mtime,
                "{name}"
            );
            let paths = Paths::new(dir.path());
            let listed = backup::list(&paths).unwrap();
            assert!(
                listed
                    .iter()
                    .any(|b| b.name == backup::LEGACY_COPY && !b.safety)
            );
            assert_eq!(backup::read(&paths, backup::LEGACY_COPY).unwrap(), legacy);
            assert_eq!(
                opened.notices,
                [(
                    "warning",
                    "zWorkTracker now keeps its data in a database on this computer, and starts empty because no sync folder was connected. Your earlier data is still on this computer: restore the backup z-work-data-legacy-1.3.1.json in Settings > Backups to bring it back.".to_owned()
                )]
            );
            // A copy already there is left as it is.
            drop(opened);
            fs::remove_file(dir.path().join(DB_FILE)).unwrap();
            fs::write(&copy, b"kept").unwrap();
            drop(open(dir.path()));
            assert_eq!(fs::read(&copy).unwrap(), b"kept");
        }
    }

    /// A1.1: a longer retention set in 1.3.1 still holds.
    #[test]
    fn a_legacy_backup_retention_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut legacy = with_project("One");
        legacy.prefs.backup_keep = Some(90);
        fs::write(
            dir.path().join(MAIN_FILE),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        assert_eq!(open(dir.path()).file.prefs.backup_keep, Some(90));
    }

    /// A1.1: retention never removes what 1.3.1 wrote: a backup dated before the database was made, or the legacy copy.
    /// Since review F4, nor this version's copy of the day the database was made, which a name can't tell from 1.3.1's
    /// copy of that day (`a_1_3_1_backup_dated_on_the_day_the_database_was_made_survives_retention`).
    #[test]
    fn a_1_3_1_backup_dated_before_the_database_survives_retention() {
        use crate::time::ManualClock;
        let dir = tempfile::tempdir().unwrap();
        let legacy = with_project("One");
        fs::write(
            dir.path().join(MAIN_FILE),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        let paths = Paths::new(dir.path());
        let old = "z-work-data-2026-10-01.json";
        backup::daily(
            &paths,
            &legacy,
            Ts::parse("2026-10-01T10:00:00Z").unwrap(),
            30,
        )
        .unwrap();
        let old_bytes = fs::read(dir.path().join("backups").join(old)).unwrap();
        let clock = ManualClock::at("2026-11-03T10:00:00Z");
        let mut s =
            crate::service::Service::open(dir.path(), std::sync::Arc::new(clock.clone()), "pc")
                .unwrap();
        s.set_backup_keep(1).unwrap();
        for _ in 0..31 {
            assert!(s.run_daily_backup().unwrap());
            clock.advance(86_400);
        }
        let mut left: Vec<String> = s
            .list_backups()
            .unwrap()
            .into_iter()
            .map(|b| b.name)
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "z-work-data-2026-10-01.json",
                "z-work-data-2026-11-03.json",
                "z-work-data-2026-12-03.json",
                "z-work-data-legacy-1.3.1.json"
            ]
        );
        assert_eq!(
            fs::read(dir.path().join("backups").join(old)).unwrap(),
            old_bytes
        );
    }

    /// Review F4: 1.3.1 may have written the daily backup of the day the database was made, before the update. Retention
    /// leaves it, as it leaves those dated before.
    #[test]
    fn a_1_3_1_backup_dated_on_the_day_the_database_was_made_survives_retention() {
        use crate::time::ManualClock;
        let dir = tempfile::tempdir().unwrap();
        let legacy = with_project("One");
        fs::write(
            dir.path().join(MAIN_FILE),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        let paths = Paths::new(dir.path());
        let same_day = "z-work-data-2026-11-03.json";
        // 1.3.1's backup of that morning.
        backup::daily(
            &paths,
            &legacy,
            Ts::parse("2026-11-03T08:00:00Z").unwrap(),
            30,
        )
        .unwrap();
        let bytes = fs::read(dir.path().join("backups").join(same_day)).unwrap();
        let clock = ManualClock::at("2026-11-03T10:00:00Z");
        let mut s =
            crate::service::Service::open(dir.path(), std::sync::Arc::new(clock.clone()), "pc")
                .unwrap();
        s.set_backup_keep(1).unwrap();
        for _ in 0..31 {
            s.run_daily_backup().unwrap();
            clock.advance(86_400);
        }
        let mut left: Vec<String> = s
            .list_backups()
            .unwrap()
            .into_iter()
            .map(|b| b.name)
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                same_day,
                "z-work-data-2026-12-03.json",
                "z-work-data-legacy-1.3.1.json"
            ]
        );
        assert_eq!(
            fs::read(dir.path().join("backups").join(same_day)).unwrap(),
            bytes,
            "as 1.3.1 wrote it"
        );
    }

    /// A database with a save in its WAL, then page 2 garbled as a bad sector would leave it: damaged, with its identity
    /// still readable. Returns the store's file, the damaged bytes and the WAL.
    fn damaged_with_wal(dir: &Path) -> (LocalFile, Vec<u8>, Vec<u8>) {
        let f = with_project("One");
        seed_store(dir, &f, NOW).unwrap();
        let image = {
            let mut opened = open(dir);
            let mut next = f.clone();
            next.data
                .create_project(&ctx("2026-01-02T00:00:00Z"), &project_input("Two"))
                .unwrap();
            opened.store.save(&f, &next).unwrap();
            crash_image(dir)
        };
        for suffix in ["", "-wal"] {
            let name = format!("{DB_FILE}{suffix}");
            fs::copy(image.path().join(&name), dir.join(&name)).unwrap();
        }
        let mut damaged = db_bytes(dir);
        damaged[4096..8192].fill(0xA5);
        fs::write(dir.join(DB_FILE), &damaged).unwrap();
        let wal = fs::read(dir.join(format!("{DB_FILE}-wal"))).unwrap();
        assert!(!wal.is_empty());
        (f, damaged, wal)
    }

    /// Review F1: an open that set the damaged database aside and got no further, for a failure or a crash, is finished
    /// by the next one: the same identity, the newest daily backup and the notice, not a first start.
    #[test]
    fn a_replace_that_stopped_after_the_set_aside_is_finished_by_the_next_open() {
        let dir = tempfile::tempdir().unwrap();
        let (f, damaged, wal) = damaged_with_wal(dir.path());
        let newer = with_project("Newer backup");
        backup::daily(
            &Paths::new(dir.path()),
            &newer,
            Ts::parse("2027-01-02T10:00:00Z").unwrap(),
            30,
        )
        .unwrap();
        // All an open had done when it stopped.
        let kept = set_aside_db(dir.path(), NOW).unwrap();
        assert!(!dir.path().join(DB_FILE).exists());
        let opened = open(dir.path());
        let (kept_at, why) = replaced(&opened);
        assert_eq!(kept_at, kept, "the copy set aside before");
        assert_eq!(why, UNFINISHED);
        assert_eq!(
            opened.file.device_id, f.device_id,
            "the damaged store's identity"
        );
        assert_eq!(opened.file.data, newer.data);
        assert_eq!(
            notice(&opened),
            format!(
                "The data file was damaged ({why}). A copy was kept at {}. Your data was restored from the backup z-work-data-2027-01-02.json. Changes made after that backup are missing.",
                kept_at.display()
            )
        );
        assert_eq!(fs::read(&kept_at).unwrap(), damaged);
        assert_eq!(fs::read(format!("{}-wal", kept_at.display())).unwrap(), wal);
        assert_eq!(
            corrupt_files(dir.path()).len(),
            2,
            "set aside once, with its WAL"
        );
        drop(opened);
        assert_eq!(open(dir.path()).origin, Origin::Existing);
    }

    /// Review F1: a damaged database whose identity can't be read, and a 1.3.1 store held for a moment, as antivirus can
    /// hold it at login. The open fails with every file where it was, and the retry (`open_when_free`) replaces the
    /// database as if nothing had held the store: its identity, the newest daily backup, the notice.
    #[cfg(unix)]
    #[test]
    fn a_held_legacy_store_stops_a_replace_before_anything_moves() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;

        use crate::service::Service;
        use crate::time::ManualClock;

        let dir = seeded(&with_project("One"));
        let newer = with_project("Newer backup");
        backup::daily(
            &Paths::new(dir.path()),
            &newer,
            Ts::parse("2027-01-02T10:00:00Z").unwrap(),
            30,
        )
        .unwrap();
        let legacy = with_project("Legacy");
        let path = dir.path().join(MAIN_FILE);
        fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        // Nothing in the database can be read, its identity included.
        let len = db_bytes(dir.path()).len();
        fs::write(dir.path().join(DB_FILE), vec![0xA5; len]).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::File::open(&path).is_ok() {
            // Mode 000 doesn't stop the superuser, so there is nothing here to hold the file.
            eprintln!("skipped: running as a user whom mode 000 doesn't stop");
            return;
        }
        let before = names(dir.path());
        let clock = ManualClock::at(&NOW.iso());
        let Err(err) = Service::open(dir.path(), Arc::new(clock.clone()), "pc") else {
            panic!("a store that can't be read stops the open");
        };
        assert!(err.is_held(), "{err:?}");
        assert_eq!(names(dir.path()), before, "nothing moved");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let s = Service::open_when_free(dir.path(), Arc::new(clock), "pc").unwrap();
        assert_eq!(
            s.file().device_id,
            legacy.device_id,
            "the old store's identity"
        );
        let projects = |data: &Shared| -> Vec<String> {
            data.projects.values().map(|p| p.name.clone()).collect()
        };
        assert_eq!(
            projects(&s.file().data),
            ["Newer backup"],
            "the newest daily backup"
        );
        let messages: Vec<String> = s
            .snapshot()
            .notices
            .into_iter()
            .map(|n| n.message)
            .collect();
        assert!(
            messages
                .iter()
                .any(|n| n.starts_with("The data file was damaged (")
                    && n.contains(
                        "Your data was restored from the backup z-work-data-2027-01-02.json."
                    )),
            "{messages:?}"
        );
        assert_eq!(corrupt_files(dir.path()).len(), 1);
    }

    /// Review F6: an open that stops between the renames of a set-aside, as a crash would, never leaves the database under
    /// its name without its WAL, to be read as if whole. The next open sends the WAL after the database set aside, and
    /// replaces that.
    #[test]
    fn a_set_aside_cut_short_keeps_the_wal_with_its_database() {
        let dir = tempfile::tempdir().unwrap();
        let (f, damaged, wal) = damaged_with_wal(dir.path());
        SET_ASIDE_STOPS_AFTER.with(|s| s.set(Some(1)));
        let stopped = Store::open(dir.path(), "pc", NOW);
        SET_ASIDE_STOPS_AFTER.with(|s| s.set(None));
        assert!(stopped.is_err(), "stopped after one rename");
        let opened = open(dir.path());
        let (kept_at, _) = replaced(&opened);
        assert_eq!(fs::read(&kept_at).unwrap(), damaged);
        assert_eq!(
            fs::read(format!("{}-wal", kept_at.display())).unwrap(),
            wal,
            "the WAL went after its database"
        );
        assert_eq!(corrupt_files(dir.path()).len(), 2);
        assert_eq!(
            opened.file.device_id, f.device_id,
            "the identity is still read"
        );
        drop(opened);
        assert_eq!(open(dir.path()).origin, Origin::Existing);
    }
}
