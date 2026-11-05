//! Where milestone 1 keeps its data: a folder named by `--sample <dir>` or `ZWT_DATA_DIR=<dir>`, never the real one.
//!
//! With neither, the app refuses to start, so it can never open real data by accident (PLAN.md M1 step 3). The
//! real data folder (`$HOME/Library/Application Support/com.rustcircle.zworktracker` on macOS,
//! `%APPDATA%\com.rustcircle.zworktracker` on Windows, `$XDG_DATA_HOME/com.rustcircle.zworktracker` or
//! `$HOME/.local/share/com.rustcircle.zworktracker` on Linux) is only ever computed as a string: the guard never
//! opens, reads or stats it (the residuals below are the exceptions).
//!
//! Paths are compared as text, after resolving `.`, `..`, repeated and trailing separators (on Windows also `/`,
//! verbatim prefixes, trailing dots or spaces, and `\\localhost\c$` for `c:`; on macOS a leading
//! `/System/Volumes/Data`, the firmlinked data volume), name by name, and each name as the file systems match names,
//! only wider ([`name_key`]): without the invisible marks some of them skip (HFS+ skips U+200C-U+200F, among
//! others), after NFKC, upper case and full Unicode case folding, so that `ſ`, `ﬆ`, `ﬅ` and the Kelvin sign (which
//! APFS folds), `ı` (which NTFS upper-cases to `I`) and any change of case spell the same name.
//!
//! A folder is refused when it is the real folder, inside it, or holds it (`$HOME`, `~/Library/Application Support`,
//! `%APPDATA%`); when any of its folders is named `com.rustcircle.zworktracker` (which also covers the Store build's
//! redirected copy); when it goes through a path that names folders by number (`/.vol/<device>/<inode>` and
//! `/.file/id=…` on macOS, `/dev/fd/<n>` on macOS and Linux, `/proc` on Linux); on macOS when its first folder's name
//! starts with a dot, as macOS's own path prefixes do (`/.nofollow` and `/.resolve/<n>` look the rest of the path up
//! in their own way, so `/.nofollow/.vol/<device>/<inode>` names a folder by number again; a later macOS may add
//! more); and on Windows when it is relative to a drive's current folder (`C:x`), when any of its names has a `:` (an
//! NTFS stream such as `::$INDEX_ALLOCATION`), or when a short 8.3 name (`~` and a digit, such as `COMRUS~1.ZWO`)
//! stands where the real folder's own name would. Anywhere else a short name is looked up only after its parent has
//! been canonicalised and listed: it is refused when the parent's canonical form is the real folder's parent (a
//! `subst` or mapped drive, a `\\?\Volume{…}` or `GLOBALROOT` path) or the parent holds a folder named like the real
//! one (the real parent through a share, the Store build's copy). One that passes (`C:\Users\PATRIC~1`) is expanded
//! from the disk, a lookup of that folder, and the long name is checked before anything below it is looked up.
//!
//! The given folder is checked as text first, before any disk call. Its links are then resolved one folder at a
//! time: each folder is checked as text before the disk is asked about it, and a link's target before it is
//! followed, so a link into the real folder is refused without a lookup inside it. The lookups stop at the first
//! folder that does not exist. Only the part that exists is then canonicalised (which on Windows also expands short
//! names) and checked again with the missing names after it, and the missing folders are made from there one at a
//! time, each by its own name: a path the OS reads in its own way fails at the first folder it would make, instead
//! of making the last one somewhere else (`/.nofollow/.vol/<device>/<inode>/x` made `x` in the folder with that
//! inode; `<device>` cannot be made in `/.nofollow/.vol`). After the first check the real folder is also compared as
//! the links of the folders above it spell it (only those folders are looked up), so a link to a folder that holds
//! it is refused too.
//!
//! Residuals:
//! - A Windows `subst` or mapped drive pointed at the real folder (`subst Z: %APPDATA%\…`, then `--sample Z:\x`):
//!   the canonical check refuses it, but only after a lookup of `x` inside the real folder and the `canonicalize` of
//!   the drive's root. Resolving the drive without opening it needs `QueryDosDeviceW`, unsafe FFI for the platform
//!   crate (milestone 3). Likewise a short name set by hand (`fsutil file setshortname`, an administrator's tool),
//!   which needs no `~`.
//! - Views of the real folder under another path (security audit SA5): a network share of it (`\\host\share`, also
//!   this computer's own name), a volume mounted on it (`\\?\Volume{…}\`), a bind mount, bindfs or nullfs. Neither
//!   the text nor the canonical path shows the real folder; only a stat of the real folder (device and inode) could
//!   tell.
//! - Races with another process of the same user (security audit SA4): the guard checks names and then uses them,
//!   and the worker opens the folder later by its path. A process that swaps a folder of the path for a link to the
//!   real folder in between (after a check, before a folder below it is looked up, listed or made; or after `resolve`
//!   has returned) has the real folder looked up or listed, an empty folder made inside it, which the last check then
//!   refuses, or the worker open and seed real data. Closing it needs the folder opened once with no link followed at
//!   any level (`openat` with `O_NOFOLLOW`, folder by folder) and that handle, not the path, passed on; a process that
//!   can do this can write the real folder itself anyway.
//! - The real folder as the environment names it: with `HOME` (`APPDATA` on Windows) unset, empty or relative the
//!   guard knows no real folder, and only the name check protects it; with `APPDATA` changed from the folder Windows
//!   reports, the text checks follow the variable.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use unicase::UniCase;
use unicode_normalization::UnicodeNormalization;

/// The command-line flag: `--sample <dir>` or `--sample=<dir>`.
pub const SAMPLE_FLAG: &str = "--sample";
/// The environment variable read when the flag is absent.
pub const DATA_DIR_VAR: &str = "ZWT_DATA_DIR";
/// The app identifier: the name of the real data folder.
pub const APP_IDENTIFIER: &str = "com.rustcircle.zworktracker";

/// Whose path rules apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    MacOs,
    Windows,
    Linux,
}

impl Platform {
    /// The platform this build runs on (any other Unix counts as Linux).
    #[must_use]
    pub const fn host() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// What the process was started with: everything [`resolve`] reads, so tests can pass their own.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    /// The arguments after the program name.
    pub args: Vec<OsString>,
    /// `ZWT_DATA_DIR`.
    pub data_dir_var: Option<OsString>,
    /// `HOME` (macOS, Linux).
    pub home: Option<OsString>,
    /// `APPDATA` (Windows).
    pub appdata: Option<OsString>,
    /// `XDG_DATA_HOME` (Linux).
    pub xdg_data_home: Option<OsString>,
    /// The current folder, for a relative `<dir>`.
    pub cwd: Option<PathBuf>,
}

impl Launch {
    /// This process's arguments, environment and current folder.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            args: std::env::args_os().skip(1).collect(),
            data_dir_var: std::env::var_os(DATA_DIR_VAR),
            home: std::env::var_os("HOME"),
            appdata: std::env::var_os("APPDATA"),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME"),
            cwd: std::env::current_dir().ok(),
        }
    }
}

/// Where the folder came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    SampleFlag,
    DataDirVar,
}

/// A checked data folder: it exists, and `path` is canonical.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataDir {
    pub path: PathBuf,
    pub source: Source,
}

/// Why no data folder could be used. The messages are plain enough to print as they are.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DataDirError {
    #[error(
        "zWorkTracker 2.0 milestone 1 runs on sample data only: start it with --sample <folder> or set ZWT_DATA_DIR=<folder>"
    )]
    NotGiven,
    #[error("--sample needs a folder: --sample <folder>")]
    FlagWithoutFolder,
    #[error("{0} is empty: give it a folder")]
    Empty(&'static str),
    #[error(
        "{} is the real zWorkTracker data folder or inside it ({real}); milestone 1 never opens real data",
        dir.display()
    )]
    RealData { dir: PathBuf, real: String },
    #[error(
        "{} goes through a folder named {APP_IDENTIFIER}, the name of the real data folder; milestone 1 never opens real data",
        dir.display()
    )]
    AppFolderName { dir: PathBuf },
    #[error(
        "{} holds the real zWorkTracker data folder ({real}); give milestone 1 a folder of its own",
        dir.display()
    )]
    HoldsRealData { dir: PathBuf, real: String },
    #[error(
        "{} names a folder by a Windows short name or stream ({part}), which can stand for the real data folder; give the folder's full name",
        dir.display()
    )]
    AliasName { dir: PathBuf, part: String },
    #[error(
        "{} goes through {path}, which names folders by number or reads the rest of the path in its own way, and can stand for the real data folder; give the folder's own path",
        dir.display()
    )]
    SpecialPath { dir: PathBuf, path: String },
    #[error(
        "{} is relative to the current folder of its drive, which cannot be checked; give the full path, such as C:\\zwt-sample",
        dir.display()
    )]
    DriveRelative { dir: PathBuf },
    #[error("{} is relative and the current folder is unknown", dir.display())]
    NoCurrentDir { dir: PathBuf },
    #[error("cannot use {}", dir.display())]
    Io {
        dir: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is not a folder", dir.display())]
    NotAFolder { dir: PathBuf },
}

/// Why a path is refused, by [`refusal`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Equal to or inside this real data folder.
    InsideRealData(String),
    /// One of its folders has the app identifier for a name.
    AppFolderName,
    /// It holds this real data folder: it is one of the folders above it.
    HoldsRealData(String),
    /// On Windows, this name of it can stand for the real folder without its text: a stream (`:`), or a short 8.3
    /// name (`~` and a digit) where the real folder's own name would be.
    AliasName(String),
    /// It goes through this path, which names folders by number (`/.vol`, `/.file`, `/dev/fd`, `/proc`), or on macOS
    /// starts with a folder whose name starts with a dot (`/.nofollow`, `/.resolve`), which macOS may read in its own
    /// way.
    SpecialPath(String),
    /// On Windows, it is relative to the current folder of a drive (`C:x`), which text cannot know.
    DriveRelative,
}

/// The real data folder of `platform` as text, from the environment in `launch`; empty when the variables it
/// needs are unset. Pure: nothing is looked up on disk.
#[must_use]
pub fn real_dirs(platform: Platform, launch: &Launch) -> Vec<String> {
    let text = |v: &Option<OsString>| {
        v.as_ref()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
    };
    match platform {
        Platform::MacOs => text(&launch.home)
            .map(|h| format!("{h}/Library/Application Support/{APP_IDENTIFIER}"))
            .into_iter()
            .collect(),
        Platform::Windows => text(&launch.appdata)
            .map(|a| format!("{a}\\{APP_IDENTIFIER}"))
            .into_iter()
            .collect(),
        Platform::Linux => text(&launch.xdg_data_home)
            .filter(|x| x.starts_with('/'))
            .or_else(|| text(&launch.home).map(|h| format!("{h}/.local/share")))
            .map(|base| format!("{base}/{APP_IDENTIFIER}"))
            .into_iter()
            .collect(),
    }
}

/// Unicode's Default_Ignorable_Code_Point (DerivedCoreProperties, 16.0.0): invisible marks such as U+200B-U+200F,
/// U+202A-U+202E, U+2060-U+206F and U+FEFF. HFS+ skips sixteen of them when it matches names, so
/// `com.rustcircle.zwork\u{200C}tracker` finds the real folder there.
const fn is_default_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{AD}'
            | '\u{34F}'
            | '\u{61C}'
            | '\u{115F}'..='\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFF8}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

/// A folder name as the file systems this guard knows match names, so that every spelling one of them takes for
/// the same name has the same key: without default-ignorable marks (HFS+ skips some), NFKC (`ſ` becomes `s`, `ﬆ` and
/// `ﬅ` become `st`, the Kelvin sign `K`), full upper case (NTFS compares the upper case of each character, which
/// makes `ı` an `I`), full Unicode case folding (as APFS matches names: `ß` and `ẞ` are `ss`), and NFKC again. It
/// merges more names than any one file system does; a guard that refuses a name it need not is on the safe side.
fn name_key(name: &str) -> String {
    let visible: String = name.chars().filter(|c| !is_default_ignorable(*c)).collect();
    let upper = visible.nfkc().collect::<String>().to_uppercase();
    UniCase::new(upper)
        .to_folded_case()
        .nfkc()
        .filter(|c| !is_default_ignorable(*c))
        .collect()
}

/// The [`name_key`] of a folder's name as `platform` reads the name: on Windows without the trailing dots and spaces
/// that Win32 drops.
fn folder_key(platform: Platform, name: &str) -> String {
    let mut key = name_key(name);
    if platform == Platform::Windows {
        // On the key, so that a dot NFKC made (`․`, U+2024) goes as well: wider than Win32, never narrower.
        let kept = key.trim_end_matches(['.', ' ']).len();
        key.truncate(kept);
    }
    key
}

/// One folder of a path: the name as given, for messages, and its [`name_key`], for comparing.
#[derive(Clone, Debug)]
struct Name {
    given: String,
    key: String,
    /// A Windows drive (`c:`) or `\\server\share`, whose `:` is not a stream.
    prefix: bool,
}

impl Name {
    fn new(given: &str, prefix: bool) -> Self {
        Self {
            given: given.to_owned(),
            key: name_key(given),
            prefix,
        }
    }
}

/// `path` begins with the folders of `start`, compared by key.
fn starts_with(path: &[Name], start: &[Name]) -> bool {
    path.len() >= start.len() && path.iter().zip(start).all(|(a, b)| a.key == b.key)
}

/// On Windows, a name that may be a short 8.3 name (`COMRUS~1.ZWO`, `PATRIC~1`): a `~` followed by a digit.
fn is_short_name(key: &str) -> bool {
    key.as_bytes()
        .windows(2)
        .any(|w| matches!(w, [b'~', d] if d.is_ascii_digit()))
}

/// `before` are the folders above a real folder: the name after them stands where the real folder's own name does.
fn is_real_parent(before: &[Name], reals: &[(&String, Vec<Name>)]) -> bool {
    reals.iter().any(|(_, real_parts)| {
        real_parts
            .split_last()
            .is_some_and(|(_, parent)| parent.len() == before.len() && starts_with(before, parent))
    })
}

/// The start of a path that can stand for the real folder without its text. Those that name folders by number:
/// macOS's volfs (`/.vol/<device>/<inode>`) and file references (`/.file/id=…`), file descriptors (`/dev/fd/<n>`),
/// and Linux's `/proc` (`/proc/self/cwd`, `/proc/<pid>/fd/<n>`). And on macOS any first folder whose name starts
/// with a dot (security audit SA2 F1): macOS's own path prefixes are such names, `/.nofollow` and `/.resolve/<n>`,
/// which have the rest of the path looked up in their own way (`/.nofollow/.vol/<device>/<inode>` reaches a folder
/// by number again), and a later macOS may add more. No folder of a user's own lives there.
fn special_path(platform: Platform, parts: &[Name]) -> Option<String> {
    let keys: Vec<&str> = parts.iter().take(2).map(|p| p.key.as_str()).collect();
    match (platform, keys.as_slice()) {
        (Platform::MacOs, [".vol", ..]) => Some("/.vol".to_owned()),
        (Platform::MacOs, [".file", ..]) => Some("/.file".to_owned()),
        (Platform::MacOs, [first, ..]) if first.starts_with('.') => {
            parts.first().map(|p| format!("/{}", p.given))
        }
        (Platform::MacOs | Platform::Linux, ["dev", "fd", ..]) => Some("/dev/fd".to_owned()),
        (Platform::Linux, ["proc", ..]) => Some("/proc".to_owned()),
        _ => None,
    }
}

/// On Windows, `C:x` and `C:` name a folder relative to the current folder of drive C:, which text cannot know.
fn drive_relative(path: &str) -> bool {
    let slashed = path.replace('/', "\\");
    let plain = slashed
        .strip_prefix(r"\\?\")
        .or_else(|| slashed.strip_prefix(r"\\.\"))
        .unwrap_or(&slashed);
    plain.get(1..2) == Some(":") && !plain.get(2..).is_some_and(|rest| rest.starts_with('\\'))
}

/// The folders of a path as `platform` reads it: `.` and empty parts dropped, `..` applied, each name keyed by
/// [`name_key`]; on Windows `/` is a separator, a verbatim prefix (`\\?\`, `\\?\UNC\`, `\\.\`) is dropped, the drive
/// or `\\server\share` is the first part, and trailing dots and spaces of a name are dropped as Win32 drops them.
fn parts(platform: Platform, path: &str) -> Vec<Name> {
    let mut out: Vec<Name> = Vec::new();
    let rest = if platform == Platform::Windows {
        let slashed = path.replace('/', "\\");
        let plain = if let Some(r) = slashed.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{r}")
        } else if let Some(r) = slashed
            .strip_prefix(r"\\?\")
            .or_else(|| slashed.strip_prefix(r"\\.\"))
        {
            r.to_owned()
        } else {
            slashed
        };
        let (prefix, rest) = if let Some(unc) = plain.strip_prefix(r"\\") {
            let mut it = unc.splitn(3, '\\');
            let server = it.next().unwrap_or_default();
            let share = it.next().unwrap_or_default();
            let rest = it.next().unwrap_or_default().to_owned();
            // `\\localhost\c$` is drive C: through this computer's administrative share.
            let loopback = server.eq_ignore_ascii_case("localhost") || server == "127.0.0.1";
            let drive = share
                .strip_suffix('$')
                .filter(|d| d.len() == 1 && d.bytes().all(|b| b.is_ascii_alphabetic()));
            match drive {
                Some(d) if loopback => (format!("{d}:"), rest),
                _ => (format!(r"\\{server}\{share}"), rest),
            }
        } else if plain.get(1..2) == Some(":") {
            (
                plain.get(..2).unwrap_or_default().to_owned(),
                plain.get(2..).unwrap_or_default().to_owned(),
            )
        } else {
            (String::new(), plain)
        };
        if !prefix.is_empty() {
            out.push(Name::new(&prefix, true));
        }
        rest
    } else {
        path.to_owned()
    };
    let base = out.len();
    let separator = if platform == Platform::Windows {
        '\\'
    } else {
        '/'
    };
    for part in rest.split(separator) {
        match part {
            "" | "." => {}
            ".." => {
                if out.len() > base {
                    out.pop();
                }
            }
            name => {
                let part = Name {
                    given: name.to_owned(),
                    key: folder_key(platform, name),
                    prefix: false,
                };
                if !part.key.is_empty() {
                    out.push(part);
                }
            }
        }
    }
    // macOS firmlinks the data volume's folders to the root: `/System/Volumes/Data/Users` is `/Users`.
    if platform == Platform::MacOs
        && let [a, b, c, ..] = out.as_slice()
        && [a, b, c].map(|p| p.key.as_str()) == ["system", "volumes", "data"]
    {
        out.drain(..3);
    }
    out
}

/// How the folder being checked is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// Looked up on the way to another folder: it may hold the real folder (the home folder does).
    Through,
    /// The folder that will hold milestone 1's data.
    Data,
}

/// Why `dir` may not hold milestone 1's data, or `None`. Pure: compares text, touches nothing on disk.
#[must_use]
pub fn refusal(platform: Platform, dir: &str, real_dirs: &[String]) -> Option<Refusal> {
    refusal_as(platform, dir, real_dirs, Role::Data)
}

fn refusal_as(platform: Platform, dir: &str, real_dirs: &[String], role: Role) -> Option<Refusal> {
    if platform == Platform::Windows && drive_relative(dir) {
        return Some(Refusal::DriveRelative);
    }
    let dir_parts = parts(platform, dir);
    if let Some(path) = special_path(platform, &dir_parts) {
        return Some(Refusal::SpecialPath(path));
    }
    let reals: Vec<(&String, Vec<Name>)> = real_dirs
        .iter()
        .map(|real| (real, parts(platform, real)))
        .filter(|(_, real_parts)| !real_parts.is_empty())
        .collect();
    if let Some((real, _)) = reals
        .iter()
        .find(|(_, real_parts)| starts_with(&dir_parts, real_parts))
    {
        return Some(Refusal::InsideRealData((*real).clone()));
    }
    let app = name_key(APP_IDENTIFIER);
    if dir_parts.iter().any(|p| p.key == app) {
        return Some(Refusal::AppFolderName);
    }
    if platform == Platform::Windows {
        for (i, part) in dir_parts.iter().enumerate() {
            let before = dir_parts.get(..i).unwrap_or_default();
            if !part.prefix
                && (part.key.contains(':')
                    || (is_short_name(&part.key) && is_real_parent(before, &reals)))
            {
                return Some(Refusal::AliasName(part.given.clone()));
            }
        }
    }
    if role == Role::Data
        && let Some((real, _)) = reals
            .iter()
            .find(|(_, real_parts)| starts_with(real_parts, &dir_parts))
    {
        return Some(Refusal::HoldsRealData((*real).clone()));
    }
    None
}

fn check(dir: &Path, real: &[String], role: Role) -> Result<(), DataDirError> {
    let dir_buf = || dir.to_path_buf();
    match refusal_as(Platform::host(), &dir.to_string_lossy(), real, role) {
        None => Ok(()),
        Some(Refusal::InsideRealData(real)) => Err(DataDirError::RealData {
            dir: dir_buf(),
            real,
        }),
        Some(Refusal::AppFolderName) => Err(DataDirError::AppFolderName { dir: dir_buf() }),
        Some(Refusal::HoldsRealData(real)) => Err(DataDirError::HoldsRealData {
            dir: dir_buf(),
            real,
        }),
        Some(Refusal::AliasName(part)) => Err(DataDirError::AliasName {
            dir: dir_buf(),
            part,
        }),
        Some(Refusal::SpecialPath(path)) => Err(DataDirError::SpecialPath {
            dir: dir_buf(),
            path,
        }),
        Some(Refusal::DriveRelative) => Err(DataDirError::DriveRelative { dir: dir_buf() }),
    }
}

/// The folder the user named, and where it came from: the flag first, then the environment variable.
fn given(launch: &Launch) -> Result<(PathBuf, Source), DataDirError> {
    let mut args = launch.args.iter();
    while let Some(arg) = args.next() {
        if arg == SAMPLE_FLAG {
            let value = args
                .next()
                .filter(|v| !v.is_empty() && !v.to_string_lossy().starts_with("--"))
                .ok_or(DataDirError::FlagWithoutFolder)?;
            return Ok((PathBuf::from(value), Source::SampleFlag));
        }
        if let Some(value) = arg
            .to_str()
            .and_then(|a| a.strip_prefix(SAMPLE_FLAG))
            .and_then(|a| a.strip_prefix('='))
        {
            if value.is_empty() {
                return Err(DataDirError::Empty(SAMPLE_FLAG));
            }
            return Ok((PathBuf::from(value), Source::SampleFlag));
        }
    }
    match &launch.data_dir_var {
        Some(v) if v.is_empty() => Err(DataDirError::Empty(DATA_DIR_VAR)),
        Some(v) => Ok((PathBuf::from(v), Source::DataDirVar)),
        None => Err(DataDirError::NotGiven),
    }
}

/// Most links followed while resolving one folder, as the OS's own limit (`ELOOP`).
const MAX_LINKS: usize = 40;

/// One part of a path still to resolve.
enum Part {
    /// A drive prefix or the root (pushing it replaces what came before, as `PathBuf::push` does).
    Root(OsString),
    Up,
    Name(OsString),
}

fn parts_of(path: &Path) -> Vec<Part> {
    path.components()
        .filter_map(|c| match c {
            Component::Prefix(_) | Component::RootDir => Some(Part::Root(c.as_os_str().to_owned())),
            Component::CurDir => None,
            Component::ParentDir => Some(Part::Up),
            Component::Normal(n) => Some(Part::Name(n.to_owned())),
        })
        .collect()
}

/// What [`walk`] found: the folders of a path that exist, with every link along them resolved, and below them the
/// names that do not exist yet.
#[derive(Debug)]
struct Walked {
    /// It exists (its last name may be a file), and no folder of it is a link.
    found: PathBuf,
    /// The names below `found`, in order. The first was looked up and was not there; the rest are text only.
    missing: Vec<OsString>,
}

impl Walked {
    /// `found` with the missing names after it.
    fn path(&self) -> PathBuf {
        joined(&self.found, &self.missing)
    }
}

/// `dir` with `names` after it.
fn joined(dir: &Path, names: &[OsString]) -> PathBuf {
    let mut path = dir.to_path_buf();
    path.extend(names);
    path
}

/// `path` with every link along its existing part resolved, one folder at a time. A folder is checked as text
/// before the disk is asked about it, and a link's target before it is followed; so neither a path nor a link
/// into the real data folder makes anything look inside it. The lookups stop at the first folder that does not
/// exist: nothing below it can be a link yet, so the names after it are kept as text (a `..` among them takes one
/// back, and the lookups go on once it is above the missing folder). `platform` is whose rules for short names
/// apply on the disk: the host's, except in tests that run Windows's on another disk.
fn walk(path: &Path, real: &[String], platform: Platform) -> Result<Walked, DataDirError> {
    let io_error = |dir: &Path, source| DataDirError::Io {
        dir: dir.to_path_buf(),
        source,
    };
    // A stack: the next part to resolve is on top.
    let mut todo: Vec<Part> = parts_of(path);
    todo.reverse();
    let mut found = PathBuf::new();
    let mut missing: Vec<OsString> = Vec::new();
    let mut links = 0;
    while let Some(part) = todo.pop() {
        let name = match part {
            // Only the path itself or a link's target starts with one, and a link is only read when nothing is
            // missing.
            Part::Root(root) => {
                missing.clear();
                found.push(root);
                continue;
            }
            // `found` holds no links, so its parent on disk is its parent as text; a missing name is text only.
            Part::Up => {
                if missing.pop().is_none() {
                    found.pop();
                }
                continue;
            }
            Part::Name(name) => name,
        };
        if !missing.is_empty() {
            missing.push(name);
            check(&joined(&found, &missing), real, Role::Through)?;
            continue;
        }
        let next = found.join(&name);
        check(&next, real, Role::Through)?;
        let short =
            platform == Platform::Windows && is_short_name(&name_key(&name.to_string_lossy()));
        if short {
            check_short_name(platform, &found, &name, real)?;
        }
        match fs::symlink_metadata(&next) {
            Ok(meta) if meta.file_type().is_symlink() => {
                links += 1;
                if links > MAX_LINKS {
                    return Err(io_error(&next, io::Error::other("too many links")));
                }
                let target = fs::read_link(&next).map_err(|e| io_error(&next, e))?;
                check(&found.join(&target), real, Role::Through)?;
                let mut target_parts = parts_of(&target);
                target_parts.reverse();
                todo.extend(target_parts);
            }
            // A short name that [`check_short_name`] let pass, above the real folder's own level
            // (`C:\Users\PATRIC~1`): take its long name from the disk, a lookup of this folder only, so that the
            // folders below are compared in their long names before they are looked up.
            Ok(_) if short => {
                let long = fs::canonicalize(&next).map_err(|e| io_error(&next, e))?;
                check(&long, real, Role::Through)?;
                found = long;
            }
            Ok(_) => found = next,
            Err(e) if e.kind() == io::ErrorKind::NotFound => missing.push(name),
            Err(e) => return Err(io_error(&next, e)),
        }
    }
    Ok(Walked { found, missing })
}

/// On Windows, before `name` in `parent` is looked up when it may be a short 8.3 name (`COMRUS~1.ZWO`), which would
/// open the real folder if it is that folder's short name and the text did not show it (security audit SA2 F3): the
/// name is checked again under `parent`'s canonical form, which spells a `subst` or mapped drive, a `\\?\Volume{…}`
/// or a `GLOBALROOT` path as the folder it stands for; then `parent` is listed, and when it holds a folder named like
/// the real one (the real parent through a share, the Store build's copy) the short name is refused, since it may be
/// that folder's. Both read `parent` only, never the folder the short name stands for.
fn check_short_name(
    platform: Platform,
    parent: &Path,
    name: &OsStr,
    real: &[String],
) -> Result<(), DataDirError> {
    let given = parent.join(name);
    let io_error = |source| DataDirError::Io {
        dir: given.clone(),
        source,
    };
    let canonical = fs::canonicalize(parent).map_err(io_error)?;
    check(&canonical.join(name), real, Role::Through)?;
    let app = folder_key(platform, APP_IDENTIFIER);
    for entry in fs::read_dir(parent).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if folder_key(platform, &entry.file_name().to_string_lossy()) == app {
            return Err(DataDirError::AliasName {
                dir: given.clone(),
                part: name.to_string_lossy().into_owned(),
            });
        }
    }
    Ok(())
}

/// Makes the `missing` folders below `found`, which exists and is canonical, one at a time and each by its own name,
/// so that the OS is never handed a path it could read in its own way beyond the folder being made (security audit
/// SA2 F1: `create_dir_all` of `/.nofollow/.vol/<device>/<inode>/x` made `x` in the folder with that inode, whereas
/// making `<device>` in `/.nofollow/.vol` fails). A folder that appeared in the meantime is taken only when it is a
/// folder and not a link.
fn make_missing(found: &Path, missing: &[OsString]) -> Result<PathBuf, DataDirError> {
    let mut dir = found.to_path_buf();
    for name in missing {
        dir.push(name);
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e)
                if e.kind() == io::ErrorKind::AlreadyExists
                    && fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) => {}
            Err(source) => return Err(DataDirError::Io { dir, source }),
        }
    }
    Ok(dir)
}

/// The real folders again, as the links of their parent folders spell them: [`walk`] resolves the parent (macOS's
/// `/var` to `/private/var` for a home in a temp folder, a home folder or `.local/share` that is a link), so the
/// walked and canonical forms of a path can be compared with them. Only the parent and the folders above it are
/// looked up, and those through [`walk`]'s checks, so never the real folder or a folder named like it.
fn linked_real_dirs(real: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in real {
        let path = Path::new(r);
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            continue;
        };
        if !parent.is_absolute() {
            continue;
        }
        if let Ok(walked) = walk(parent, real, Platform::host()) {
            let linked = walked.path().join(name).to_string_lossy().into_owned();
            if !real.contains(&linked) && !out.contains(&linked) {
                out.push(linked);
            }
        }
    }
    out
}

/// The data folder for this launch: named by `--sample` or `ZWT_DATA_DIR`, checked against the real data folder
/// (as given, before any disk call; then as its links resolve; then canonical), made when missing, and returned
/// canonical.
pub fn resolve(launch: &Launch) -> Result<DataDir, DataDirError> {
    let (given, source) = given(launch)?;
    let mut real = real_dirs(Platform::host(), launch);
    let absolute = if given.is_absolute() {
        given
    } else {
        let cwd = launch
            .cwd
            .as_ref()
            .ok_or_else(|| DataDirError::NoCurrentDir { dir: given.clone() })?;
        cwd.join(given)
    };
    check(&absolute, &real, Role::Data)?;
    let linked = linked_real_dirs(&real);
    real.extend(linked);
    let walked = walk(&absolute, &real, Platform::host())?;
    check(&walked.path(), &real, Role::Data)?;
    let io_err = |dir: &Path| {
        let dir = dir.to_path_buf();
        move |source| DataDirError::Io { dir, source }
    };
    // Only what exists is canonicalised; the missing names stay names, made one at a time below it.
    let found = fs::canonicalize(&walked.found).map_err(io_err(&walked.found))?;
    check(&joined(&found, &walked.missing), &real, Role::Data)?;
    let made = make_missing(&found, &walked.missing)?;
    let path = fs::canonicalize(&made).map_err(io_err(&made))?;
    check(&path, &real, Role::Data)?;
    if !path.is_dir() {
        return Err(DataDirError::NotAFolder { dir: path });
    }
    Ok(DataDir { path, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = APP_IDENTIFIER;

    fn launch_with(home: &Path, args: &[&str]) -> Launch {
        Launch {
            args: args.iter().map(OsString::from).collect(),
            home: Some(home.as_os_str().to_owned()),
            cwd: Some(home.to_path_buf()),
            ..Launch::default()
        }
    }

    /// A fake home in a temp dir: its "real" data folder is computed from it, so no test ever names the user's.
    fn fake_home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn real_of(home: &Path) -> PathBuf {
        match Platform::host() {
            Platform::MacOs => home.join("Library/Application Support").join(ID),
            Platform::Linux => home.join(".local/share").join(ID),
            Platform::Windows => unreachable!("APPDATA is used on Windows"),
        }
    }

    fn refused(r: Result<DataDir, DataDirError>) -> bool {
        matches!(
            r,
            Err(DataDirError::RealData { .. } | DataDirError::AppFolderName { .. })
        )
    }

    #[test]
    fn without_flag_or_variable_it_refuses_to_start() {
        let home = fake_home();
        let err = resolve(&launch_with(home.path(), &[])).unwrap_err();
        assert!(matches!(err, DataDirError::NotGiven));
        assert!(err.to_string().contains("--sample <folder>"));
        assert!(err.to_string().contains("ZWT_DATA_DIR=<folder>"));
    }

    #[test]
    fn a_flag_without_a_folder_and_empty_values_are_refused() {
        let home = fake_home();
        for args in [
            &["--sample"][..],
            &["--sample", "--other"],
            &["--sample", ""],
        ] {
            let err = resolve(&launch_with(home.path(), args)).unwrap_err();
            assert!(matches!(err, DataDirError::FlagWithoutFolder), "{args:?}");
        }
        let err = resolve(&launch_with(home.path(), &["--sample="])).unwrap_err();
        assert!(matches!(err, DataDirError::Empty(SAMPLE_FLAG)));
        let mut l = launch_with(home.path(), &[]);
        l.data_dir_var = Some(OsString::new());
        assert!(matches!(
            resolve(&l).unwrap_err(),
            DataDirError::Empty(DATA_DIR_VAR)
        ));
    }

    #[test]
    fn the_real_folder_itself_is_refused_and_left_untouched() {
        let home = fake_home();
        let real = real_of(home.path());
        let arg = real.to_string_lossy().into_owned();
        assert!(refused(resolve(&launch_with(
            home.path(),
            &["--sample", &arg]
        ))));
        let mut l = launch_with(home.path(), &[]);
        l.data_dir_var = Some(OsString::from(&arg));
        assert!(refused(resolve(&l)));
        assert!(!real.exists(), "refused before anything was created");
    }

    #[test]
    fn a_folder_inside_it_is_refused() {
        let home = fake_home();
        let inside = real_of(home.path()).join("sub/dir");
        let arg = inside.to_string_lossy().into_owned();
        assert!(refused(resolve(&launch_with(
            home.path(),
            &["--sample", &arg]
        ))));
        assert!(!real_of(home.path()).exists());
    }

    #[test]
    fn dots_trailing_separators_and_case_do_not_get_past_it() {
        let home = fake_home();
        let real = real_of(home.path()).to_string_lossy().into_owned();
        let parent = real_of(home.path())
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let tries = [
            format!("{real}/"),
            format!("{real}//"),
            format!("{real}/."),
            format!("{real}/./x/.."),
            format!("{parent}/other/../{ID}"),
            format!("{parent}/{}", ID.to_uppercase()),
            format!("{}/sub", real.to_uppercase()),
        ];
        for t in &tries {
            assert!(
                refused(resolve(&launch_with(home.path(), &["--sample", t]))),
                "{t}"
            );
        }
        assert!(!real_of(home.path()).exists());
    }

    #[test]
    fn a_relative_folder_is_resolved_against_the_current_one() {
        let home = fake_home();
        let mut l = launch_with(
            home.path(),
            &[
                "--sample",
                "x/../.local/share/../../Library/Application Support/com.rustcircle.zworktracker/a",
            ],
        );
        if Platform::host() == Platform::Linux {
            l.args = vec!["--sample".into(), format!(".local/share/{ID}/a").into()];
        }
        assert!(refused(resolve(&l)));
        l.cwd = None;
        assert!(matches!(
            resolve(&l).unwrap_err(),
            DataDirError::NoCurrentDir { .. }
        ));
    }

    /// The fake home's "real" folder, made unreadable and unsearchable: any lookup inside it fails with
    /// `PermissionDenied`, so a refusal (not an I/O error) proves the guard never looked inside.
    #[cfg(unix)]
    fn sealed_real(home: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let real = real_of(home);
        fs::create_dir_all(&real).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o000)).unwrap();
        real
    }

    #[cfg(unix)]
    fn unseal(real: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(real, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_link_into_the_real_folder_is_refused_without_looking_inside() {
        use std::os::unix::fs::symlink;
        let home = fake_home();
        let real = sealed_real(home.path());
        let link = home.path().join("innocent");
        symlink(&real, &link).unwrap();
        // A chain of links, and a link whose target walks through `..`.
        let first = home.path().join("first");
        symlink(&link, &first).unwrap();
        let parent = real.parent().unwrap().to_owned();
        let roundabout = home.path().join("roundabout");
        symlink(parent.join("x/..").join(APP_IDENTIFIER), &roundabout).unwrap();
        for given in [
            link.join("sample"),
            link.clone(),
            first.join("deep/er"),
            roundabout.join("sample"),
        ] {
            let got = resolve(&launch_with(
                home.path(),
                &["--sample", &given.to_string_lossy()],
            ));
            assert!(refused(got), "{}", given.display());
        }
        unseal(&real);
        assert_eq!(
            fs::read_dir(&real).unwrap().count(),
            0,
            "nothing created inside"
        );
    }

    #[cfg(unix)]
    #[test]
    fn links_elsewhere_are_followed_and_loops_end() {
        use std::os::unix::fs::symlink;
        let home = fake_home();
        let target = home.path().join("data/samples");
        fs::create_dir_all(&target).unwrap();
        symlink(&target, home.path().join("abs")).unwrap();
        symlink("data/samples", home.path().join("rel")).unwrap();
        for name in ["abs", "rel"] {
            let given = home.path().join(name).join("m1");
            let got = resolve(&launch_with(
                home.path(),
                &["--sample", &given.to_string_lossy()],
            ))
            .unwrap();
            assert_eq!(
                got.path,
                fs::canonicalize(target.join("m1")).unwrap(),
                "{name}"
            );
        }
        symlink(home.path().join("loop-b"), home.path().join("loop-a")).unwrap();
        symlink(home.path().join("loop-a"), home.path().join("loop-b")).unwrap();
        let err = resolve(&launch_with(
            home.path(),
            &["--sample", &home.path().join("loop-a/x").to_string_lossy()],
        ))
        .unwrap_err();
        assert!(matches!(err, DataDirError::Io { .. }), "{err}");
    }

    #[test]
    fn any_folder_named_after_the_app_is_refused() {
        let home = fake_home();
        let arg = home
            .path()
            .join("copies")
            .join(ID)
            .join("x")
            .to_string_lossy()
            .into_owned();
        let err = resolve(&launch_with(home.path(), &["--sample", &arg])).unwrap_err();
        assert!(matches!(err, DataDirError::AppFolderName { .. }), "{err}");
        assert!(!home.path().join("copies").exists());
    }

    #[test]
    fn a_sample_folder_is_created_and_returned_canonical() {
        let home = fake_home();
        let dir = home.path().join("samples/./m1");
        let arg = dir.to_string_lossy().into_owned();
        let got = resolve(&launch_with(home.path(), &["--sample", &arg])).unwrap();
        assert_eq!(got.source, Source::SampleFlag);
        assert!(got.path.is_dir());
        assert_eq!(
            got.path,
            fs::canonicalize(home.path().join("samples/m1")).unwrap()
        );
        // Again: an existing folder is fine, and `--sample=<dir>` reads the same.
        let again = resolve(&launch_with(home.path(), &[&format!("--sample={arg}")])).unwrap();
        assert_eq!(again.path, got.path);
    }

    #[test]
    fn the_flag_wins_over_the_variable_and_the_variable_works_alone() {
        let home = fake_home();
        let (a, b) = (home.path().join("a"), home.path().join("b"));
        let mut l = launch_with(home.path(), &["--sample", &a.to_string_lossy()]);
        l.data_dir_var = Some(b.as_os_str().to_owned());
        let got = resolve(&l).unwrap();
        assert_eq!(got.path, fs::canonicalize(&a).unwrap());
        l.args.clear();
        let got = resolve(&l).unwrap();
        assert_eq!(
            (got.path, got.source),
            (fs::canonicalize(&b).unwrap(), Source::DataDirVar)
        );
    }

    #[test]
    fn a_file_is_not_a_folder() {
        let home = fake_home();
        let file = home.path().join("file");
        fs::write(&file, b"x").unwrap();
        let err = resolve(&launch_with(
            home.path(),
            &["--sample", &file.to_string_lossy()],
        ))
        .unwrap_err();
        assert!(
            matches!(
                err,
                DataDirError::Io { .. } | DataDirError::NotAFolder { .. }
            ),
            "{err}"
        );
    }

    #[test]
    fn the_real_folder_is_computed_per_platform_as_text() {
        let l = Launch {
            home: Some("/Users/pat".into()),
            appdata: Some(r"C:\Users\pat\AppData\Roaming".into()),
            ..Launch::default()
        };
        assert_eq!(
            real_dirs(Platform::MacOs, &l),
            vec![format!("/Users/pat/Library/Application Support/{ID}")]
        );
        assert_eq!(
            real_dirs(Platform::Windows, &l),
            vec![format!(r"C:\Users\pat\AppData\Roaming\{ID}")]
        );
        assert_eq!(
            real_dirs(Platform::Linux, &l),
            vec![format!("/Users/pat/.local/share/{ID}")]
        );
        let xdg = Launch {
            xdg_data_home: Some("/data/x".into()),
            ..l.clone()
        };
        assert_eq!(
            real_dirs(Platform::Linux, &xdg),
            vec![format!("/data/x/{ID}")]
        );
        let relative_xdg = Launch {
            xdg_data_home: Some("rel".into()),
            ..l
        };
        assert_eq!(
            real_dirs(Platform::Linux, &relative_xdg),
            vec![format!("/Users/pat/.local/share/{ID}")],
            "a relative XDG_DATA_HOME is ignored, as the spec says"
        );
        assert!(real_dirs(Platform::MacOs, &Launch::default()).is_empty());
    }

    #[test]
    fn windows_paths_are_compared_as_windows_reads_them() {
        let real = vec![format!(r"C:\Users\pat\AppData\Roaming\{ID}")];
        let refused_paths = [
            format!(r"C:\Users\pat\AppData\Roaming\{ID}"),
            format!(r"c:\users\PAT\appdata\roaming\{}\", ID.to_uppercase()),
            format!(r"C:/Users/pat/AppData/Roaming/{ID}/sub"),
            format!(r"\\?\C:\Users\pat\AppData\Roaming\{ID}"),
            format!(r"C:\Users\pat\AppData\Roaming\x\..\{ID}. \data"),
            format!(r"C:\Users\pat\AppData\Roaming\.\{ID}\\\data"),
        ];
        for p in &refused_paths {
            assert_eq!(
                refusal(Platform::Windows, p, &real),
                Some(Refusal::InsideRealData(real[0].clone())),
                "{p}"
            );
        }
        // The Store build's package-private copy is caught by its folder name.
        let msix = format!(
            r"C:\Users\pat\AppData\Local\Packages\RustCircle.zWorkTracker_x\LocalCache\Roaming\{ID}"
        );
        assert_eq!(
            refusal(Platform::Windows, &msix, &real),
            Some(Refusal::AppFolderName)
        );
        assert_eq!(
            refusal(
                Platform::Windows,
                &format!(r"\\?\UNC\server\share\{ID}"),
                &real
            ),
            Some(Refusal::AppFolderName)
        );
        for ok in [
            r"C:\Users\pat\AppData\Roaming\other",
            r"D:\Users\pat\AppData\Roaming\zwt-sample",
            r"C:\Users\pat\AppData\Roaming\..\..\..\..\..\sample",
        ] {
            assert_eq!(refusal(Platform::Windows, ok, &real), None, "{ok}");
        }
        // %APPDATA% itself holds the real folder (review N1: it was accepted before).
        assert_eq!(
            refusal(Platform::Windows, r"C:\Users\pat\AppData\Roaming", &real),
            Some(Refusal::HoldsRealData(real[0].clone()))
        );
    }

    #[test]
    fn unix_paths_are_compared_by_folder_not_by_prefix() {
        let real = vec![format!("/Users/pat/Library/Application Support/{ID}")];
        assert_eq!(
            refusal(
                Platform::MacOs,
                &format!("/Users/pat/Library/Application Support/{ID}-sample"),
                &real
            ),
            None,
            "a sibling whose name starts the same is another folder"
        );
        // The folder the real one lives in holds it (review N1: it was accepted before).
        assert_eq!(
            refusal(
                Platform::MacOs,
                "/Users/pat/Library/Application Support",
                &real
            ),
            Some(Refusal::HoldsRealData(real[0].clone()))
        );
        assert_eq!(
            refusal(
                Platform::MacOs,
                &format!("/../Users/pat/Library/Application Support/{ID}"),
                &real
            ),
            Some(Refusal::InsideRealData(real[0].clone()))
        );
    }

    #[test]
    fn name_keys_fold_as_apfs_and_ntfs_match_names() {
        for (a, b) in [
            ("\u{17F}", "s"),   // long s: APFS folds it, NTFS upper-cases it to S
            ("\u{FB06}", "st"), // st ligature: APFS
            ("\u{FB05}", "st"), // long s t ligature: APFS
            ("\u{212A}", "k"),  // Kelvin sign: APFS
            ("\u{131}", "i"),   // dotless i: NTFS upper-cases it to I
            ("STRASSE", "stra\u{DF}e"),
            ("\u{1E9E}", "ss"),
            ("e\u{301}", "\u{E9}"), // decomposed and composed accent
            ("\u{FF43}", "c"),      // full-width c
            (ID, &ID.to_uppercase()),
            ("com.rustcircle.zwork\u{200C}tracker", ID), // HFS+ skips U+200C
            ("\u{FEFF}x\u{206A}", "x"),
            ("e\u{34F}\u{301}", "\u{E9}"), // a grapheme joiner does not keep the accent apart
        ] {
            assert_eq!(name_key(a), name_key(b), "{a:?} and {b:?}");
        }
        assert_ne!(name_key("a"), name_key("b"));
        assert_ne!(name_key(ID), name_key(&format!("{ID}-sample")));
    }

    #[test]
    fn odd_spellings_get_the_refusal_their_folder_gets() {
        let mac = vec![format!("/Users/pat/Library/Application Support/{ID}")];
        let inside = |dir: &str| refusal(Platform::MacOs, dir, &mac);
        assert_eq!(
            inside("/Users/pat/Library/Application Support/com.ru\u{17F}tcircle.zworktracker/x"),
            Some(Refusal::InsideRealData(mac[0].clone()))
        );
        assert_eq!(
            inside("/tmp/com.ru\u{FB06}circle.zworktracker"),
            Some(Refusal::AppFolderName)
        );
        assert_eq!(
            inside("/USERS/PAT/library"),
            Some(Refusal::HoldsRealData(mac[0].clone()))
        );
        let win = vec![format!(r"C:\Users\pat\AppData\Roaming\{ID}")];
        assert_eq!(
            refusal(
                Platform::Windows,
                r"C:\Users\pat\AppData\Roaming\COMRUS~1.ZWO\x",
                &win
            ),
            Some(Refusal::AliasName("COMRUS~1.ZWO".to_owned()))
        );
        assert_eq!(
            refusal(
                Platform::Windows,
                &format!(r"C:\Users\pat\AppData\Roaming\{ID}::$INDEX_ALLOCATION"),
                &win
            ),
            Some(Refusal::AliasName(format!("{ID}::$INDEX_ALLOCATION")))
        );
        // The drive's colon is not a stream.
        assert_eq!(refusal(Platform::Windows, r"D:\samples", &win), None);
    }

    #[test]
    fn paths_that_text_cannot_resolve_get_their_own_refusals() {
        let win = vec![format!(r"C:\Users\pat\AppData\Roaming\{ID}")];
        assert_eq!(
            refusal(Platform::Windows, "C:x", &win),
            Some(Refusal::DriveRelative)
        );
        assert_eq!(refusal(Platform::Windows, r"C:\x", &win), None);
        // A short name stands for the real folder only where its own name would be.
        assert_eq!(
            refusal(Platform::Windows, r"C:\Users\pat\AppData\Roaming\X~1", &win),
            Some(Refusal::AliasName("X~1".to_owned()))
        );
        assert_eq!(
            refusal(Platform::Windows, r"C:\Users\pat\AppData\X~1", &win),
            None
        );
        assert_eq!(
            refusal(Platform::Windows, r"\\localhost\c$\Users\pat\AppData", &win),
            Some(Refusal::HoldsRealData(win[0].clone()))
        );
        let mac = vec![format!("/Users/pat/Library/Application Support/{ID}")];
        assert_eq!(
            refusal(Platform::MacOs, "/.vol/1/2/x", &mac),
            Some(Refusal::SpecialPath("/.vol".to_owned()))
        );
        assert_eq!(
            refusal(Platform::Linux, "/dev/fd/3", &mac),
            Some(Refusal::SpecialPath("/dev/fd".to_owned()))
        );
        assert_eq!(
            refusal(
                Platform::MacOs,
                &format!("/System/Volumes/Data/Users/pat/Library/Application Support/{ID}/x"),
                &mac
            ),
            Some(Refusal::InsideRealData(mac[0].clone()))
        );
        let messages = [
            DataDirError::DriveRelative { dir: "C:x".into() }.to_string(),
            DataDirError::SpecialPath {
                dir: "/.vol/1/2".into(),
                path: "/.vol".to_owned(),
            }
            .to_string(),
        ];
        assert!(messages[0].contains(r"C:\zwt-sample"), "{}", messages[0]);
        assert!(
            messages[1].contains("names folders by number"),
            "{}",
            messages[1]
        );
    }

    #[test]
    fn a_folder_above_the_real_one_is_refused_and_the_message_says_why() {
        let home = fake_home();
        let support = real_of(home.path()).parent().unwrap().to_path_buf();
        let err = resolve(&launch_with(
            home.path(),
            &["--sample", &support.to_string_lossy()],
        ))
        .unwrap_err();
        assert!(
            matches!(&err, DataDirError::HoldsRealData { dir, .. } if *dir == support),
            "{err}"
        );
        assert!(
            err.to_string()
                .contains("holds the real zWorkTracker data folder"),
            "{err}"
        );
        assert!(!support.exists(), "refused before anything was created");
    }

    /// The folders above the real one are looked up as their links resolve, so a link to one of them is refused at
    /// the walked check; the real folder itself is never looked up.
    #[cfg(unix)]
    #[test]
    fn the_real_folder_is_also_compared_as_its_parents_links_spell_it() {
        use std::os::unix::fs::symlink;
        let home = fake_home();
        let real = sealed_real(home.path());
        let linked = linked_real_dirs(&real_dirs(Platform::host(), &launch_with(home.path(), &[])));
        unseal(&real);
        let canonical = fs::canonicalize(&real).unwrap();
        assert!(
            linked.contains(&canonical.to_string_lossy().into_owned()) || canonical == real,
            "{linked:?} has {}",
            canonical.display()
        );
        let link = home.path().join("support");
        symlink(real.parent().unwrap(), &link).unwrap();
        let err = resolve(&launch_with(
            home.path(),
            &["--sample", &link.to_string_lossy()],
        ))
        .unwrap_err();
        assert!(matches!(err, DataDirError::HoldsRealData { .. }), "{err}");
    }

    /// The walk looks up each folder until the first that is missing, keeps the names after it as text, and goes
    /// back to looking up once a `..` has taken it above the missing folder (here to a link, which is followed).
    #[cfg(unix)]
    #[test]
    fn the_walk_stops_at_the_first_missing_folder_and_keeps_the_rest_as_text() {
        let home = fake_home();
        let base = walk(home.path(), &[], Platform::host()).unwrap();
        assert!(base.missing.is_empty(), "{base:?}");
        let got = walk(&home.path().join("a/b/../c"), &[], Platform::host()).unwrap();
        assert_eq!(got.found, base.found);
        assert_eq!(got.missing, ["a", "c"].map(OsString::from));
        fs::create_dir(home.path().join("data")).unwrap();
        std::os::unix::fs::symlink(home.path().join("data"), home.path().join("link")).unwrap();
        let got = walk(&home.path().join("a/../link/x/y"), &[], Platform::host()).unwrap();
        assert_eq!(got.found, base.found.join("data"));
        assert_eq!(got.missing, ["x", "y"].map(OsString::from));
    }

    /// Security audit SA2 F1, the second line of defence: the missing folders are made one at a time from the part
    /// that exists, each by its own name, so a path macOS reads in its own way fails at the first folder it would
    /// make (`<device>` cannot be made in `/.nofollow/.vol`), where `create_dir_all` made `x` in the folder with that
    /// inode. The text check refuses such a path first; this runs the making alone, on a fake folder.
    #[cfg(target_os = "macos")]
    #[test]
    fn sa2_f1_missing_folders_are_made_one_at_a_time_so_a_prefix_fails_at_the_first() {
        use std::os::unix::fs::{MetadataExt, symlink};
        let home = fake_home();
        let fake = home.path().join("fake-real");
        fs::create_dir(&fake).unwrap();
        fs::write(fake.join("marker"), b"fake").unwrap();
        let meta = fs::metadata(&fake).unwrap();
        let by_number: Vec<OsString> = [
            meta.dev().to_string(),
            meta.ino().to_string(),
            "x".to_owned(),
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let volfs = Path::new("/.nofollow/.vol");
        let got = make_missing(volfs, &by_number);
        assert!(
            matches!(&got, Err(DataDirError::Io { dir, .. }) if *dir == volfs.join(meta.dev().to_string())),
            "{got:?}"
        );
        assert_eq!(fs::read_dir(&fake).unwrap().count(), 1, "only the marker");
        // Elsewhere the folders are made, and a folder that is there already is taken.
        let canonical = fs::canonicalize(home.path()).unwrap();
        let ab = ["a", "b"].map(OsString::from);
        let made = make_missing(&canonical, &ab).unwrap();
        assert_eq!(made, canonical.join("a/b"));
        assert!(made.is_dir());
        assert_eq!(make_missing(&canonical, &ab).unwrap(), made);
        // A link that appeared where a folder was to be made is not taken.
        symlink(&fake, home.path().join("c")).unwrap();
        let got = make_missing(&canonical, &["c", "d"].map(OsString::from));
        assert!(matches!(&got, Err(DataDirError::Io { .. })), "{got:?}");
        assert_eq!(fs::read_dir(&fake).unwrap().count(), 1, "only the marker");
    }

    /// Security audit SA2 F3 (2026-11-05): on Windows a short name whose parent is not the real one as text (a `subst`
    /// drive, a share, a volume path, the Store build's copy) was looked up, so the real folder was opened by that
    /// name before its long name was checked. Windows's rule runs here on this disk: each parent may be listed (`r`)
    /// but nothing in it looked up (no `x`), so a refusal rather than `PermissionDenied` shows that the parent's
    /// listing decided before any lookup of the short name. What only Windows can show: that NTFS gives the real
    /// folder the short name, and that the canonical form of a `subst` or mapped drive, a volume or `GLOBALROOT` path
    /// is the real parent.
    #[cfg(unix)]
    #[test]
    fn sa2_f3_a_short_name_is_refused_from_its_parents_listing_before_it_is_looked_up() {
        use std::os::unix::fs::PermissionsExt;
        let root = fake_home();
        let copy = root.path().join("LocalCache/Roaming");
        let share = root.path().join("share");
        let plain = root.path().join("plain");
        fs::create_dir_all(copy.join(ID)).unwrap();
        fs::write(copy.join(ID).join("marker"), b"fake").unwrap();
        // As Windows reads names: the trailing dot and space go, case does not count.
        fs::create_dir_all(share.join("Com.RustCircle.ZWorkTracker. ")).unwrap();
        fs::create_dir_all(plain.join("other")).unwrap();
        let listed_only = [&copy, &share, &plain];
        for dir in listed_only {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o444)).unwrap();
        }
        let enforced = fs::symlink_metadata(copy.join(ID)).is_err();
        // The real folder where Windows has it: as text, never one of these parents.
        let real = vec![format!(r"C:\Users\pat\AppData\Roaming\{ID}")];
        let short = |dir: &Path| {
            walk(
                &dir.join("COMRUS~1.ZWO").join("x"),
                &real,
                Platform::Windows,
            )
        };
        let (from_copy, from_share, from_plain) = (short(&copy), short(&share), short(&plain));
        let long_from_plain = walk(&plain.join("m1"), &real, Platform::Windows);
        for dir in listed_only {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
        }
        if !enforced {
            eprintln!("skipped: permissions are not enforced for this user (root?)");
            return;
        }
        for got in [&from_copy, &from_share] {
            assert!(
                matches!(got, Err(DataDirError::AliasName { part, .. }) if part == "COMRUS~1.ZWO"),
                "{got:?}"
            );
        }
        // A parent that holds no such folder: the short name is looked up as any other name is, and here that lookup
        // is what fails.
        for got in [&from_plain, &long_from_plain] {
            assert!(
                matches!(got, Err(DataDirError::Io { source, .. }) if source.kind() == io::ErrorKind::PermissionDenied),
                "{got:?}"
            );
        }
        assert_eq!(
            fs::read_dir(copy.join(ID)).unwrap().count(),
            1,
            "only the marker"
        );
    }
}
