//! Edits as they are typed, kept in a small file of their own, so a shutdown that gives the page no
//! chance to save can't lose them. Replayed once at the next launch. Never synced.
//!
//! Words the store won't take go to a second file. They are the only copy left, and what is typed
//! next replaces a draft, so only dismissing their notice ends them. Never synced either.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::CoreError;
use crate::persist;
use crate::time::Ts;

const FILE: &str = "z-work-drafts.json";
const KEPT_FILE: &str = "z-work-kept-words.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub description: Value,
    pub at: Ts,
    /// The stored description the words were typed over, as the editor showed it. Null for no
    /// description, and in a draft written before this was kept.
    #[serde(default)]
    pub base: Value,
}

/// A file of words: empty if there is none, or `None` if it is damaged. An error says it couldn't be read at all,
/// which may only mean something else has it open.
fn read_file<T: DeserializeOwned + Default>(path: &Path) -> std::io::Result<Option<T>> {
    let mut bytes = Vec::new();
    let read = persist::with_local_retry(|| {
        bytes = fs::read(path)?;
        Ok(())
    });
    match read {
        Ok(()) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Some(T::default())),
        Err(e) => Err(e),
    }
}

/// The drafts, or `None` for a file that is damaged. For words held back or failing to save a draft is the only
/// copy, so whatever can't read this file must leave it alone.
pub fn read(dir: &Path) -> std::io::Result<Option<BTreeMap<String, Draft>>> {
    read_file(&dir.join(FILE))
}

pub fn write(dir: &Path, drafts: &BTreeMap<String, Draft>) -> Result<(), CoreError> {
    let path = dir.join(FILE);
    if drafts.is_empty() {
        match fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    }
    let bytes = serde_json::to_vec(drafts)?;
    let tmp = dir.join(format!("{FILE}.tmp"));
    persist::with_local_retry(|| persist::write_atomic(&path, &tmp, &bytes))?;
    Ok(())
}

/// Why the store won't take some words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeptBecause {
    /// The description they were typed over changed while they waited, on another device or here.
    ChangedElsewhere,
    /// Their task no longer exists.
    TaskGone,
    /// With an id on each list item they are longer than a description can be.
    TooLong,
}

/// Words shown in a notice until it is dismissed, as plain text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Kept {
    pub task_id: String,
    /// The task's name when they were kept, for a task that may be gone by the time they are read.
    pub name: String,
    pub words: String,
    pub at: Ts,
    pub because: KeptBecause,
}

/// The words kept, or `None` for a file that is damaged.
pub fn read_kept(dir: &Path) -> std::io::Result<Option<Vec<Kept>>> {
    read_file(&dir.join(KEPT_FILE))
}

pub fn write_kept(dir: &Path, kept: &[&Kept]) -> Result<(), CoreError> {
    let path = dir.join(KEPT_FILE);
    if kept.is_empty() {
        match fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    }
    let bytes = serde_json::to_vec(kept)?;
    let tmp = dir.join(format!("{KEPT_FILE}.tmp"));
    persist::with_local_retry(|| persist::write_atomic(&path, &tmp, &bytes))?;
    Ok(())
}

/// Moves a damaged file of drafts out of the way of the next write. Returns its new name.
pub fn set_aside_drafts(dir: &Path, now: Ts) -> std::io::Result<String> {
    set_aside(dir, "z-work-drafts", FILE, now)
}

/// Moves a damaged file of kept words out of the way of the next write. Returns its new name.
pub fn set_aside_kept(dir: &Path, now: Ts) -> std::io::Result<String> {
    set_aside(dir, "z-work-kept-words", KEPT_FILE, now)
}

fn set_aside(dir: &Path, stem: &str, file: &str, now: Ts) -> std::io::Result<String> {
    let moved = persist::set_aside(dir, stem, &dir.join(file), now)?;
    Ok(moved.file_name().map_or_else(
        || file.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    ))
}
