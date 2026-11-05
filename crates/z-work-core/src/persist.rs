//! What is left of the 1.3.1 JSON store: atomic file writes (temp, fsync, rename) for the side files and backups, and
//! reading a `LocalFile` from JSON, which backups still are. The store itself is `store`, a SQLite database.
//! Damaged files are moved aside and kept. Nothing is ever silently wiped.

use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;
use uuid::Uuid;

use crate::error::CoreError;
use crate::model::{LocalFile, SCHEMA_VERSION, Shared};
use crate::time::Ts;
use crate::validate::check_core;

/// The 1.3.1 store. Never written by this version: only read, at the first start, for this device's identity.
pub const MAIN_FILE: &str = "z-work-data.json";

#[derive(Clone, Debug)]
pub struct Paths {
    pub dir: PathBuf,
    /// When the database here was first made. Backups dated on or before that day may have been written by 1.3.1, and
    /// retention leaves them. `None` keeps every dated backup subject to retention.
    pub(crate) backups_from: Option<Ts>,
}

impl Paths {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            backups_from: None,
        }
    }

    #[must_use]
    pub(crate) fn with_backups_from(mut self, from: Option<Ts>) -> Self {
        self.backups_from = from;
        self
    }
}

/// Creates `tmp` afresh and fills it. A leftover file or planted link is removed, never followed.
fn write_new(tmp: &Path, bytes: &[u8]) -> io::Result<()> {
    match fs::symlink_metadata(tmp) {
        Ok(meta) if meta.is_dir() => return Err(io::Error::other("temporary path is a directory")),
        Ok(_) => fs::remove_file(tmp)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Antivirus or backup tools can hold a file for a moment. Retried for about half a second.
pub(crate) fn with_local_retry(mut op: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    let mut wait = 50;
    for _ in 0..3 {
        match op() {
            Err(e) if crate::sync::is_transient(&e) => {
                std::thread::sleep(std::time::Duration::from_millis(wait))
            }
            other => return other,
        }
        wait *= 2;
    }
    op()
}

#[cfg(test)]
thread_local! {
    /// The renames whose folder was flushed, in order, for tests to read.
    pub(crate) static PARENT_SYNCS: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Flushes the folder a file was just renamed into. Until that is on disk, a power cut can undo the rename.
pub(crate) fn sync_parent(dest: &Path) {
    #[cfg(test)]
    PARENT_SYNCS.with(|s| s.borrow_mut().push(dest.to_path_buf()));
    #[cfg(unix)]
    if let Some(parent) = dest.parent()
        && let Ok(d) = File::open(parent)
    {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dest;
}

/// Writes `bytes` to `tmp`, flushes it to disk and renames it over `dest`.
pub fn write_atomic(dest: &Path, tmp: &Path, bytes: &[u8]) -> io::Result<()> {
    write_new(tmp, bytes)?;
    fs::rename(tmp, dest)?;
    sync_parent(dest);
    Ok(())
}

/// Upgrades an older store step by step before it is parsed. New fields that are optional need no
/// step here, since every replicated record defaults missing fields. Add `1 => ...` for a schema 2.
fn migrate(mut value: Value) -> Value {
    // Schema 2 adds optional records and fields only, all of which default when missing. Starter
    // tags are added when the store opens, so the step just records the new version.
    if value.get("version").and_then(Value::as_u64) == Some(1)
        && let Some(obj) = value.as_object_mut()
    {
        obj.insert("version".to_owned(), Value::from(2u64));
    }
    value
}

/// Reads a store file from elsewhere, such as a backup, without touching the disk.
pub fn parse_checked(bytes: &[u8]) -> Result<LocalFile, CoreError> {
    match parse_local(bytes) {
        Parsed::Good(f, _) => Ok(*f),
        Parsed::Newer(found) => Err(CoreError::FutureVersion {
            found,
            supported: SCHEMA_VERSION,
        }),
        Parsed::Corrupt(why) => Err(CoreError::Validation(format!(
            "that file can't be read: {why}"
        ))),
    }
}

#[derive(Debug)]
enum Parsed {
    /// The file and the schema version it was written in.
    Good(
        Box<LocalFile>,
        #[cfg_attr(
            not(test),
            expect(dead_code, reason = "the tests check which way a file was read")
        )]
        u64,
    ),
    Corrupt(String),
    Newer(u64),
}

fn parse_local(bytes: &[u8]) -> Parsed {
    // A store in the current schema needs no migration, and so no generic JSON tree, whose peak is
    // many times the size of the file. Anything this doesn't take goes the long way and gets its verdict.
    if let Ok(file) = serde_json::from_slice::<LocalFile>(bytes)
        && file.version == SCHEMA_VERSION
    {
        return match check_core(&file.data) {
            Ok(()) => Parsed::Good(Box::new(file), SCHEMA_VERSION),
            Err(e) => Parsed::Corrupt(e.to_string()),
        };
    }
    let value: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => return Parsed::Corrupt(e.to_string()),
    };
    let version = value.get("version").and_then(Value::as_u64);
    let found = match version {
        None | Some(0) => return Parsed::Corrupt("no schema version".into()),
        Some(v) if v > SCHEMA_VERSION => return Parsed::Newer(v),
        Some(v) => v,
    };
    let value = migrate(value);
    let file: LocalFile = match serde_json::from_value(value) {
        Ok(f) => f,
        Err(e) => return Parsed::Corrupt(e.to_string()),
    };
    if let Err(e) = check_core(&file.data) {
        return Parsed::Corrupt(e.to_string());
    }
    Parsed::Good(Box::new(file), found)
}

pub(crate) fn read_opt(path: &Path) -> io::Result<Option<Vec<u8>>> {
    read_held(|| fs::read(path))
}

/// A read given the same time as a write, as something else may have the file open for a moment.
fn read_held(mut read: impl FnMut() -> io::Result<Vec<u8>>) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = None;
    let got = with_local_retry(|| {
        bytes = Some(read()?);
        Ok(())
    });
    match got {
        Ok(()) => Ok(bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Moves a damaged file aside under a unique name so it stays available for hand recovery.
pub(crate) fn set_aside(dir: &Path, stem: &str, from: &Path, now: Ts) -> io::Result<PathBuf> {
    let stamp = now.datetime().format("%Y%m%dT%H%M%SZ");
    for n in 0..1000 {
        let name = if n == 0 {
            format!("{stem}.corrupt-{stamp}.json")
        } else {
            format!("{stem}.corrupt-{stamp}-{n}.json")
        };
        let target = dir.join(name);
        if !target.exists() {
            fs::rename(from, &target)?;
            return Ok(target);
        }
    }
    Err(io::Error::other("too many quarantined files"))
}

#[must_use]
pub fn new_local_file(device_name: &str) -> LocalFile {
    LocalFile {
        version: SCHEMA_VERSION,
        device_id: Uuid::new_v4().to_string(),
        device_name: device_name.to_owned(),
        last_heartbeat: None,
        prefs: crate::model::LocalPrefs::default(),
        remote_devices: std::collections::BTreeMap::new(),
        newest_stamp: None,
        latest_change_here: None,
        data: Shared::default(),
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    /// A store written by the phase 1 build, as it was on disk.
    const V1: &str = r##"{"version":1,"deviceId":"11111111-1111-4111-8111-111111111111","deviceName":"pc",
      "lastHeartbeat":"2026-03-01T09:10:00Z","prefs":{"theme":"dark","hideEarnings":true},
      "remoteDevices":{},"data":{
        "projects":{"p":{"id":"p","name":"Old","color":"#112233","billable":true,"createdAt":"2026-01-01T00:00:00Z",
          "stamp":{"at":"2026-01-01T00:00:00Z","by":"d"}}},
        "tasks":{"t":{"id":"t","name":"Old task","nameStamp":{"at":"2026-01-01T00:00:00Z","by":"d"},"projectId":"p",
          "projectStamp":{"at":"2026-01-01T00:00:00Z","by":"d"},"description":null,
          "descriptionStamp":{"at":"2026-01-01T00:00:00Z","by":"d"},"deletedStamp":{"at":"2026-01-01T00:00:00Z","by":"d"},
          "createdAt":"2026-01-01T00:00:00Z","state":"paused","pauseReason":{"kind":"user"},
          "stateStamp":{"at":"2026-01-01T00:00:00Z","by":"d"}}},
        "segments":{"s":{"id":"s","taskId":"t","deviceId":"d","start":"2026-03-01T09:00:00Z","end":"2026-03-01T09:10:00Z"}}
      }}"##;

    /// Was `a_phase_1_store_loads_keeps_its_data_and_a_copy_of_the_original`: the store is SQLite now (see
    /// `store::tests::a_schema_migration_keeps_a_copy_of_the_database_before_it_runs`), and a phase 1 file can still
    /// come back as a backup, which is read with the same JSON migration.
    #[test]
    fn a_phase_1_backup_parses_with_its_data() {
        let file = parse_checked(V1.as_bytes()).unwrap();
        assert_eq!(file.version, SCHEMA_VERSION);
        assert_eq!(file.data.tasks["t"].name, "Old task");
        assert!(file.data.tasks["t"].tags.is_empty());
        assert_eq!(
            file.data.segments["s"].effective().unwrap().0,
            Ts::parse("2026-03-01T09:00:00Z").unwrap()
        );
        assert_eq!(
            file.data.projects["p"].billing_type,
            crate::model::BillingType::Hourly
        );
        // What it says once written in the current schema reads back the same.
        let again = parse_checked(&serde_json::to_vec(&file).unwrap()).unwrap();
        assert_eq!(again, file);
    }

    /// Compat: a backup 1.3.2 wrote holds `prefs.mica`, the Windows 11 backdrop 1.3.3 removed. It parses with nothing
    /// lost, the key is ignored, and a file written from it no longer has it.
    #[test]
    fn a_1_3_2_backup_with_the_mica_preference_parses_and_loses_nothing() {
        let mut file = new_local_file("pc");
        file.prefs.theme = crate::model::Theme::SolarizedDark;
        file.prefs.setup_done = true;
        let mut old = serde_json::to_value(&file).unwrap();
        old["prefs"]["mica"] = Value::Bool(true);
        let read = parse_checked(&serde_json::to_vec_pretty(&old).unwrap()).unwrap();
        assert_eq!(read, file);
        assert_eq!(
            serde_json::to_value(&read).unwrap()["prefs"].get("mica"),
            None
        );
    }

    #[test]
    fn a_phase_1_device_file_is_readable_and_passes_the_sync_checks() {
        let v: Value = serde_json::from_str(V1).unwrap();
        let text = serde_json::json!({
            "schemaVersion": 1, "deviceId": "d", "deviceName": "old pc",
            "writtenAt": "2026-03-01T09:10:00Z", "data": v["data"],
        })
        .to_string();
        let f: crate::model::DeviceFile = serde_json::from_str(&text).unwrap();
        let now = Ts::parse("2026-04-01T00:00:00Z").unwrap();
        crate::validate::check_device_file(&f, now).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::tests::{ctx, project_input};

    fn with_project(name: &str) -> LocalFile {
        let mut f = new_local_file("pc");
        f.data
            .create_project(&ctx("2026-01-01T00:00:00Z"), &project_input(name))
            .unwrap();
        f
    }

    #[test]
    fn current_schema_files_read_the_same_with_and_without_the_json_tree() {
        let long_way = |bytes: &[u8]| -> Option<LocalFile> {
            let value = migrate(serde_json::from_slice(bytes).ok()?);
            let file: LocalFile = serde_json::from_value(value).ok()?;
            check_core(&file.data).ok()?;
            Some(file)
        };
        let one = with_project("One");
        let good = serde_json::to_vec(&one).unwrap();
        let Parsed::Good(short, 2) = parse_local(&good) else {
            panic!("a good file must parse");
        };
        assert_eq!(*short, long_way(&good).unwrap());
        // Duplicate keys are refused by the short way, and resolved by the long way as they always were.
        let text = String::from_utf8(good).unwrap();
        let dup = text.replacen(
            "\"deviceName\":\"pc\"",
            "\"deviceName\":\"a\",\"deviceName\":\"pc\"",
            1,
        );
        assert_ne!(dup, text);
        let Parsed::Good(via_dup, 2) = parse_local(dup.as_bytes()) else {
            panic!("the long way still takes a duplicate key");
        };
        assert_eq!(*via_dup, one);
        // An id that doesn't match its key is damage on both ways.
        let wrong = serde_json::to_value(&one).unwrap();
        let mut wrong = wrong;
        let projects = wrong["data"]["projects"].as_object_mut().unwrap();
        let (id, project) = projects
            .iter()
            .next()
            .map(|(k, v)| (k.clone(), v.clone()))
            .unwrap();
        projects.remove(&id);
        projects.insert("other".to_owned(), project);
        let bytes = serde_json::to_vec(&wrong).unwrap();
        assert!(matches!(parse_local(&bytes), Parsed::Corrupt(_)));
        assert!(long_way(&bytes).is_none());
    }

    #[test]
    fn r12_a_read_is_retried_while_the_file_is_held_and_no_file_is_not_an_error() {
        let mut calls = 0;
        let got = read_held(|| {
            calls += 1;
            if calls < 3 {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                Ok(b"data".to_vec())
            }
        });
        assert_eq!(got.unwrap().as_deref(), Some(&b"data"[..]));
        assert_eq!(calls, 3);

        let mut calls = 0;
        let got = read_held(|| {
            calls += 1;
            Err(io::Error::from(io::ErrorKind::NotFound))
        });
        assert_eq!(got.unwrap(), None);
        assert_eq!(calls, 1, "nothing to wait for");

        let got = read_held(|| Err(io::Error::from(io::ErrorKind::PermissionDenied)));
        assert_eq!(got.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    /// The half of 1.3.1's `a_planted_link_at_the_temporary_path_is_not_followed` that is still about this file: backups,
    /// drafts and the device files are written through `write_atomic`.
    #[cfg(unix)]
    #[test]
    fn write_atomic_never_follows_a_link_planted_at_its_temporary_path() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        fs::write(&victim, b"precious").unwrap();
        let dest = dir.path().join("d");
        let tmp = dir.path().join("t");
        std::os::unix::fs::symlink(&victim, &tmp).unwrap();
        write_atomic(&dest, &tmp, b"data").unwrap();
        assert_eq!(fs::read(&victim).unwrap(), b"precious");
        assert_eq!(fs::read(&dest).unwrap(), b"data");
    }
}
