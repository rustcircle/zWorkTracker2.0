//! Automatic dated copies of the local store, in `<data>/backups/`. Separate from sync: they never
//! leave this computer. Restoring makes a safety copy of the current store first.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::Serialize;

use crate::error::CoreError;
use crate::model::LocalFile;
use crate::persist::{self, Paths};
use crate::time::Ts;

pub const DEFAULT_KEEP: u32 = 30;
const SAFETY_KEEP: usize = 5;
const DAILY_PREFIX: &str = "z-work-data-";
const SAFETY_PREFIX: &str = "pre-restore-";
/// A copy of an older version's store, taken at the first start of this one. Listed and restorable like any backup,
/// and never removed by retention.
const LEGACY_PREFIX: &str = "z-work-data-legacy-";
/// The 1.3.1 store (`z-work-data.json`, else its `.bak`), byte for byte as the first 1.3.2 start found it.
pub const LEGACY_COPY: &str = "z-work-data-legacy-1.3.1.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    pub name: String,
    pub bytes: u64,
    /// A copy made automatically just before a restore.
    pub safety: bool,
}

fn dir(paths: &Paths) -> PathBuf {
    paths.dir.join("backups")
}

/// Names only come from this folder's own pattern, so a request can't reach any other file.
fn is_backup_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".json") else {
        return false;
    };
    if let Some(version) = stem.strip_prefix(LEGACY_PREFIX) {
        return !version.is_empty()
            && version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    }
    let tail = stem
        .strip_prefix(DAILY_PREFIX)
        .or_else(|| stem.strip_prefix(SAFETY_PREFIX));
    tail.is_some_and(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

fn names(paths: &Paths) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    match fs::read_dir(dir(paths)) {
        Ok(entries) => {
            for e in entries {
                let name = e?.file_name().to_string_lossy().into_owned();
                if is_backup_name(&name) {
                    out.push(name);
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    out.sort();
    Ok(out)
}

pub fn list(paths: &Paths) -> io::Result<Vec<BackupInfo>> {
    let d = dir(paths);
    let mut out: Vec<BackupInfo> = names(paths)?
        .into_iter()
        .map(|name| BackupInfo {
            bytes: fs::metadata(d.join(&name)).map_or(0, |m| m.len()),
            safety: name.starts_with(SAFETY_PREFIX),
            name,
        })
        .collect();
    // Newest first by the date in the name, whichever kind of backup it is. The copy of the old store holds the
    // oldest data of all, so it comes last.
    let dated = |b: &BackupInfo| {
        if is_legacy_copy(&b.name) {
            return String::new();
        }
        b.name
            .strip_prefix(DAILY_PREFIX)
            .or_else(|| b.name.strip_prefix(SAFETY_PREFIX))
            .unwrap_or(&b.name)
            .to_owned()
    };
    out.sort_by_key(|b| std::cmp::Reverse(dated(b)));
    Ok(out)
}

fn write(paths: &Paths, name: &str, file: &LocalFile) -> Result<(), CoreError> {
    let d = dir(paths);
    fs::create_dir_all(&d)?;
    let bytes = serde_json::to_vec_pretty(file)?;
    let tmp = d.join(format!(".{name}.tmp"));
    persist::write_atomic(&d.join(name), &tmp, &bytes)?;
    Ok(())
}

/// The copy of an older version's store.
pub(crate) fn is_legacy_copy(name: &str) -> bool {
    name.starts_with(LEGACY_PREFIX)
}

/// The day a backup's name is dated, `YYYY-MM-DD`. `None` for the copy of an older store and for a name not dated.
fn dated_day(name: &str) -> Option<&str> {
    if is_legacy_copy(name) {
        return None;
    }
    let stem = name.strip_suffix(".json")?;
    let tail = stem
        .strip_prefix(DAILY_PREFIX)
        .or_else(|| stem.strip_prefix(SAFETY_PREFIX))?;
    let day = tail.get(..10)?;
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    Some(day)
}

/// Removes the oldest copies past `keep`. Only those this version wrote: a copy dated on or before the day the database
/// was made may have been written by 1.3.1, which wrote its copy of that day before the upgrade, and stays for good, as
/// does the copy of the 1.3.1 store. A name can't tell which version wrote a copy dated that day, so this version's
/// copies of that day stay too.
fn prune(paths: &Paths, prefix: &str, keep: usize) -> io::Result<()> {
    let from = paths
        .backups_from
        .map(|t| t.datetime().format("%Y-%m-%d").to_string());
    let mut mine: Vec<String> = names(paths)?
        .into_iter()
        .filter(|n| n.starts_with(prefix))
        .filter(|n| dated_day(n).is_some_and(|day| from.as_deref().is_none_or(|f| day > f)))
        .collect();
    let excess = mine.len().saturating_sub(keep);
    for name in mine.drain(..excess) {
        fs::remove_file(dir(paths).join(name))?;
    }
    Ok(())
}

/// Writes today's copy if there isn't one yet. Returns true when it wrote one.
pub fn daily(paths: &Paths, file: &LocalFile, now: Ts, keep: u32) -> Result<bool, CoreError> {
    let name = format!("{DAILY_PREFIX}{}.json", now.datetime().format("%Y-%m-%d"));
    if dir(paths).join(&name).exists() {
        return Ok(false);
    }
    write(paths, &name, file)?;
    prune(paths, DAILY_PREFIX, keep.max(1) as usize)?;
    Ok(true)
}

/// Reads and checks a backup without changing anything.
pub fn read(paths: &Paths, name: &str) -> Result<LocalFile, CoreError> {
    if !is_backup_name(name) {
        return Err(CoreError::NotFound("backup"));
    }
    let bytes = fs::read(dir(paths).join(name)).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            CoreError::NotFound("backup")
        } else {
            CoreError::Io(e)
        }
    })?;
    persist::parse_checked(&bytes)
}

/// Copies the current store aside before it is replaced.
pub fn safety_copy(paths: &Paths, file: &LocalFile, now: Ts) -> Result<String, CoreError> {
    let name = format!(
        "{SAFETY_PREFIX}{}.json",
        now.datetime().format("%Y-%m-%dT%H-%M-%S")
    );
    write(paths, &name, file)?;
    prune(paths, SAFETY_PREFIX, SAFETY_KEEP)?;
    Ok(name)
}

/// Keeps the store of an older version as `LEGACY_COPY`, once. The source is only read, by the caller.
pub(crate) fn keep_legacy(paths: &Paths, bytes: &[u8]) -> Result<(), CoreError> {
    let d = dir(paths);
    let dest = d.join(LEGACY_COPY);
    if fs::symlink_metadata(&dest).is_ok() {
        return Ok(());
    }
    fs::create_dir_all(&d)?;
    let tmp = d.join(format!(".{LEGACY_COPY}.tmp"));
    persist::with_local_retry(|| persist::write_atomic(&dest, &tmp, bytes))?;
    Ok(())
}

#[must_use]
pub fn folder(paths: &Paths) -> PathBuf {
    dir(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::new_local_file;

    fn at(day: u32) -> Ts {
        Ts::parse(&format!("2026-03-{day:02}T10:00:00Z")).unwrap()
    }

    #[test]
    fn one_copy_a_day_with_retention() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path());
        let f = new_local_file("pc");
        assert!(daily(&paths, &f, at(1), 3).unwrap());
        assert!(!daily(&paths, &f, at(1), 3).unwrap(), "already done today");
        for day in 2..=5 {
            daily(&paths, &f, at(day), 3).unwrap();
        }
        let got: Vec<String> = list(&paths).unwrap().into_iter().map(|b| b.name).collect();
        assert_eq!(
            got,
            [
                "z-work-data-2026-03-05.json",
                "z-work-data-2026-03-04.json",
                "z-work-data-2026-03-03.json"
            ]
        );
    }

    #[test]
    fn only_this_folders_own_names_can_be_read() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path());
        for bad in [
            "../z-work-data.json",
            "z-work-data-../../x.json",
            "other.json",
            "z-work-data-.json",
            "",
        ] {
            assert!(
                matches!(read(&paths, bad), Err(CoreError::NotFound(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_damaged_backup_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path());
        fs::create_dir_all(dir(&paths)).unwrap();
        fs::write(dir(&paths).join("z-work-data-2026-03-01.json"), "{nope").unwrap();
        assert!(read(&paths, "z-work-data-2026-03-01.json").is_err());
    }

    #[test]
    fn safety_copies_are_kept_apart_from_daily_retention() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path());
        let f = new_local_file("pc");
        safety_copy(&paths, &f, at(1)).unwrap();
        daily(&paths, &f, at(2), 1).unwrap();
        daily(&paths, &f, at(3), 1).unwrap();
        let all = list(&paths).unwrap();
        assert_eq!(all.iter().filter(|b| b.safety).count(), 1);
        assert_eq!(all.iter().filter(|b| !b.safety).count(), 1);
    }
}
