//! Regression tests of the data-folder guard for review findings R4 and N1 (2026-11-04 review of milestone 1).
//!
//! - R4: spellings of the real folder that a file system takes for its name (`ſ`, `ﬆ`, `ﬅ`, the Kelvin sign, `ı` on
//!   NTFS, any case; on Windows a short 8.3 name or an NTFS stream suffix) were refused only after the real folder had
//!   been looked up on disk, or not at all. They must be refused as text, before any disk call.
//! - N1: a folder that holds the real one (`$HOME`, `~/Library/Application Support`, `%APPDATA%`) was accepted.
//!
//! The second review of the fix (reviewer F1 and F2, security-auditor V1 to V3) added the HFS+ spellings, the paths
//! that name folders by number (`/.vol`, `/dev/fd`), Windows drive-relative paths and loopback shares, and the
//! firmlinked data volume; and narrowed the 8.3 rule to the place of the real folder's own name.
//!
//! The security audit of the merged fixes (2026-11-05, SA2 F1) found that macOS's path prefixes `/.nofollow` and
//! `/.resolve/<n>` passed the text check: `/.nofollow/.vol/<device>/<inode>/x` made `x` inside the real folder, and
//! `/.nofollow` in front of a folder that holds the real one was accepted (an N1 bypass). The `sa2_f1_*` tests.
//!
//! Every disk test uses a fake home in a temp dir, so its "real" folder is computed from it and the user's own is
//! never named. The tests use only what the guard had before the fix, so the same file ran red before it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::path::{Path, PathBuf};

use zwt_bridge::datadir::{APP_IDENTIFIER, Platform, Refusal, refusal};
#[cfg(unix)]
use zwt_bridge::datadir::{DataDirError, Launch, resolve};

const ID: &str = APP_IDENTIFIER;

/// Spellings of the app identifier that APFS (case-insensitive, the macOS default) takes for the identifier itself:
/// long s (U+017F), the `st` ligature (U+FB06), the long-s `t` ligature (U+FB05), the Kelvin sign (U+212A), and upper
/// case. `scratch/apfs` on the review machine listed the folder under each of them.
const APFS_SPELLINGS: [&str; 5] = [
    "com.ru\u{17F}tcircle.zworktracker",
    "com.ru\u{FB06}circle.zworktracker",
    "com.ru\u{FB05}circle.zworktracker",
    "com.rustcircle.zwor\u{212A}tracker",
    "COM.RUSTCIRCLE.ZWORKTRACKER",
];

/// Spellings NTFS takes for the identifier: it compares upper cases, so dotless `ı` (U+0131) and long `ſ` match `i`
/// and `s` there.
const NTFS_SPELLINGS: [&str; 3] = [
    "com.rustc\u{131}rcle.zworktracker",
    "com.ru\u{17F}tcircle.zworktracker",
    "Com.RustCircle.ZWorkTracker",
];

/// Spellings HFS+ (Mac OS Extended, case-insensitive) takes for the identifier: it skips U+200C-U+200F,
/// U+202A-U+202E, U+206A-U+206F and U+FEFF when it matches names (reviewer F1, checked on a disk image).
const HFS_SPELLINGS: [&str; 4] = [
    "com.rustcircle.zwork\u{200C}tracker",
    "\u{FEFF}com.rustcircle.zworktracker",
    "com.rust\u{202A}circle.zworktracker",
    "com.rustcircle.zworktracke\u{206A}r",
];

/// A refusal of the guard, as opposed to a missing or bad argument or an I/O error (such as the `PermissionDenied`
/// of a sealed folder, which would mean the guard asked the disk about it).
#[cfg(unix)]
fn is_refusal(e: &DataDirError) -> bool {
    !matches!(
        e,
        DataDirError::NotGiven
            | DataDirError::FlagWithoutFolder
            | DataDirError::Empty(_)
            | DataDirError::NoCurrentDir { .. }
            | DataDirError::Io { .. }
            | DataDirError::NotAFolder { .. }
    )
}

#[cfg(unix)]
fn launch(home: &Path, dir: &Path) -> Launch {
    Launch {
        args: vec![OsString::from("--sample"), dir.as_os_str().to_owned()],
        home: Some(home.as_os_str().to_owned()),
        cwd: Some(home.to_path_buf()),
        ..Launch::default()
    }
}

/// The fake home's "real" data folder, as the guard computes it on this host.
#[cfg(unix)]
fn real_of(home: &Path) -> PathBuf {
    match Platform::host() {
        Platform::MacOs => home.join("Library/Application Support").join(ID),
        Platform::Linux => home.join(".local/share").join(ID),
        Platform::Windows => unreachable!("the disk tests are Unix only"),
    }
}

/// Mode 000 on a folder for the life of the guard: looking up any name in it fails with `PermissionDenied`, so a
/// lookup, `lstat` or `realpath` of the real folder inside it cannot succeed.
#[cfg(unix)]
struct Sealed(PathBuf);

#[cfg(unix)]
impl Sealed {
    fn new(dir: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        Self(dir.to_path_buf())
    }

    /// False when the seal does not hold (a test run as root reads anything).
    fn holds(&self) -> bool {
        std::fs::read_dir(&self.0).is_err()
    }
}

#[cfg(unix)]
impl Drop for Sealed {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// The names in `dir`, sorted.
#[cfg(target_os = "macos")]
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A fake home whose "real" folder exists and holds one file, `marker`; with that folder's device and inode.
#[cfg(target_os = "macos")]
fn home_with_marked_real() -> (tempfile::TempDir, PathBuf, String) {
    use std::os::unix::fs::MetadataExt;
    let home = tempfile::tempdir().unwrap();
    let real = real_of(home.path());
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("marker"), b"fake").unwrap();
    let meta = std::fs::metadata(&real).unwrap();
    let vol = format!(".vol/{}/{}", meta.dev(), meta.ino());
    (home, real, vol)
}

#[test]
fn r4_unicode_and_case_spellings_of_the_app_folder_are_refused_as_text() {
    let mac = vec![format!("/Users/pat/Library/Application Support/{ID}")];
    for spelling in APFS_SPELLINGS.into_iter().chain(HFS_SPELLINGS) {
        for dir in [
            format!("/Users/pat/Library/Application Support/{spelling}"),
            format!("/Users/pat/Library/Application Support/{spelling}/sub/dir"),
            format!("/Users/pat/Library/Application Support/x/../{spelling}/"),
            // The app's name in any spelling, wherever it is (the Store build's redirected copy, a backup).
            format!("/Volumes/Backup/{spelling}/data"),
        ] {
            assert!(
                refusal(Platform::MacOs, &dir, &mac).is_some(),
                "macOS took {dir:?}"
            );
        }
    }
    let windows = vec![format!(r"C:\Users\pat\AppData\Roaming\{ID}")];
    for spelling in NTFS_SPELLINGS {
        for dir in [
            format!(r"C:\Users\pat\AppData\Roaming\{spelling}"),
            format!(r"c:/users/pat/appdata/roaming/{spelling}/sub"),
            format!(r"D:\copies\{spelling}"),
        ] {
            assert!(
                refusal(Platform::Windows, &dir, &windows).is_some(),
                "Windows took {dir:?}"
            );
        }
    }
    // Compatibility forms (full-width letters) and a decomposed accent are folded too: the guard refuses more
    // spellings than any one file system matches, never fewer.
    assert!(
        refusal(
            Platform::Linux,
            "/home/pat/x/\u{FF43}om.rustcircle.zworktracker",
            &[format!("/home/pat/.local/share/{ID}")]
        )
        .is_some()
    );
    let accented = vec![format!("/Users/jos\u{E9}/Library/Application Support/{ID}")];
    assert!(
        refusal(
            Platform::MacOs,
            "/Users/jose\u{301}/Library/Application Support/com.ru\u{17F}tcircle.zworktracker/x",
            &accented
        )
        .is_some()
    );
    // Neighbours stay usable.
    for ok in [
        format!("/Users/pat/Library/Application Support/{ID}-sample"),
        "/Users/pat/Library/Application Support/other".to_owned(),
        "/Users/pat/samples/m1".to_owned(),
    ] {
        assert_eq!(refusal(Platform::MacOs, &ok, &mac), None, "{ok}");
    }
}

#[test]
fn r4_windows_short_names_and_stream_suffixes_are_refused_as_text() {
    let real = vec![format!(r"C:\Users\pat\AppData\Roaming\{ID}")];
    for dir in [
        // 8.3 short names NTFS makes for the identifier: the first form, and the hashed form after four clashes.
        r"C:\Users\pat\AppData\Roaming\COMRUS~1.ZWO".to_owned(),
        r"c:\users\pat\appdata\roaming\comrus~2.zwo\sub".to_owned(),
        r"C:\Users\pat\AppData\Roaming\CO1A2B~1.ZWO".to_owned(),
        r"\\?\C:\Users\pat\AppData\Roaming\COMRUS~1.ZWO".to_owned(),
        // The same through this computer's administrative share of drive C:.
        r"\\localhost\C$\Users\pat\AppData\Roaming\COMRUS~1.ZWO".to_owned(),
        // NTFS stream names of the folder itself.
        format!(r"C:\Users\pat\AppData\Roaming\{ID}::$INDEX_ALLOCATION"),
        format!(r"C:\Users\pat\AppData\Roaming\{ID}:$I30:$INDEX_ALLOCATION\sub"),
        r"D:\samples\m1:stream".to_owned(),
        // Relative to the current folder of drive C:, which may be the real folder (security-auditor V2).
        r"C:x".to_owned(),
        r"C:".to_owned(),
        r"c:sample\m1".to_owned(),
    ] {
        assert!(
            refusal(Platform::Windows, &dir, &real).is_some(),
            "Windows took {dir:?}"
        );
    }
    // A tilde without a digit, the colon of a drive, and a short name anywhere but in the real folder's own place
    // are plain names. A short name above it (`PATRIC~1`) is expanded from the disk before anything below it is
    // looked up; `%TEMP%` holds such a name for long user names (reviewer F2).
    for ok in [
        r"D:\samples\m1",
        r"D:\samples\a~b",
        r"D:\samples\~draft",
        r"\\server\share\samples",
        r"\\?\D:\samples\m1",
        r"C:\Users\pat\Documents\backup~2024\sample",
        r"C:\Users\RUNNER~1\AppData\Local\Temp\zwt",
        r"C:\Users\PATRIC~1\AppData\Roaming\x",
    ] {
        assert_eq!(refusal(Platform::Windows, ok, &real), None, "{ok}");
    }
    // On macOS and Linux `~1` and `:` are plain characters of a name.
    assert_eq!(
        refusal(
            Platform::MacOs,
            "/Users/pat/samples/v~1:old",
            &[format!("/Users/pat/Library/Application Support/{ID}")]
        ),
        None
    );
}

#[test]
fn r4_paths_that_name_folders_by_number_are_refused_as_text() {
    let mac = vec![format!("/Users/pat/Library/Application Support/{ID}")];
    for dir in [
        "/.vol/16777234/2/fresh",
        "/.VOL/16777234/2",
        "/x/../.vol/16777234/2",
        "/.file/id=6571367.2773272/x",
        "/dev/fd/3/z",
    ] {
        assert!(
            refusal(Platform::MacOs, dir, &mac).is_some(),
            "macOS took {dir:?}"
        );
    }
    let linux = vec![format!("/home/pat/.local/share/{ID}")];
    for dir in ["/dev/fd/3/z", "/proc/self/cwd/x", "/proc/1234/fd/3"] {
        assert!(
            refusal(Platform::Linux, dir, &linux).is_some(),
            "Linux took {dir:?}"
        );
    }
    for ok in ["/Volumes/x/.vol", "/Users/pat/dev/fd", "/devices/fd"] {
        assert_eq!(refusal(Platform::MacOs, ok, &mac), None, "{ok}");
    }
    assert_eq!(refusal(Platform::Linux, "/home/pat/proc", &linux), None);
}

/// macOS volfs reaches a folder by device and inode, with no name in the path, and `mkdir` works through it: the
/// guard used to create `fresh` inside the real folder before its last check failed (security-auditor V1).
#[cfg(target_os = "macos")]
#[test]
fn r4_a_volfs_path_to_the_real_folder_creates_nothing_in_it() {
    use std::os::unix::fs::MetadataExt;
    let home = tempfile::tempdir().unwrap();
    let real = real_of(home.path());
    std::fs::create_dir_all(&real).unwrap();
    let meta = std::fs::metadata(&real).unwrap();
    for tail in ["fresh", "fresh/deeper", ""] {
        let given = PathBuf::from(format!("/.vol/{}/{}", meta.dev(), meta.ino())).join(tail);
        let got = resolve(&launch(home.path(), &given));
        let inside: Vec<_> = std::fs::read_dir(&real)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(
            inside.is_empty(),
            "{}: created inside the real folder: {inside:?} ({got:?})",
            given.display()
        );
        assert!(
            matches!(&got, Err(e) if is_refusal(e)),
            "{}: {got:?}",
            given.display()
        );
    }
}

/// The fake real folder exists and its parent is sealed. Any lookup of the real folder would fail with
/// `PermissionDenied`, so a refusal that names the folder as it was given proves the guard decided before it asked
/// the disk.
#[cfg(unix)]
#[test]
fn r4_odd_spellings_are_refused_as_given_before_the_disk_is_asked() {
    let home = tempfile::tempdir().unwrap();
    let real = real_of(home.path());
    std::fs::create_dir_all(&real).unwrap();
    let parent = real.parent().unwrap().to_path_buf();
    let seal = Sealed::new(&parent);
    if !seal.holds() {
        eprintln!("skipped: permissions are not enforced for this user (root?)");
        return;
    }
    for spelling in APFS_SPELLINGS.into_iter().chain(HFS_SPELLINGS) {
        for given in [parent.join(spelling), parent.join(spelling).join("sub/dir")] {
            let err = resolve(&launch(home.path(), &given)).unwrap_err();
            assert!(is_refusal(&err), "{}: {err}", given.display());
            assert!(
                err.to_string().contains(&*given.to_string_lossy()),
                "refused as given, before the disk was asked: {err}"
            );
        }
    }
    drop(seal);
    assert_eq!(
        std::fs::read_dir(&parent).unwrap().count(),
        1,
        "nothing was created beside the real folder"
    );
}

/// A new user has no real folder yet. A spelling that the file system folds to its name would create it (APFS), so
/// sample data would land where real data goes next.
#[cfg(unix)]
#[test]
fn r4_an_odd_spelling_never_creates_the_real_folder() {
    let home = tempfile::tempdir().unwrap();
    let real = real_of(home.path());
    let parent = real.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&parent).unwrap();
    for spelling in APFS_SPELLINGS.into_iter().chain(HFS_SPELLINGS) {
        let given = parent.join(spelling).join("m1");
        let got = resolve(&launch(home.path(), &given));
        assert!(
            matches!(&got, Err(e) if is_refusal(e)),
            "{}: {got:?}",
            given.display()
        );
        assert!(!real.exists(), "{spelling}: the real folder was created");
        assert_eq!(
            std::fs::read_dir(&parent).unwrap().count(),
            0,
            "{spelling}: something was created beside the real folder"
        );
    }
}

/// A link whose target spells the real folder oddly is refused before it is followed: the target is checked as text.
#[cfg(unix)]
#[test]
fn r4_a_link_to_an_odd_spelling_is_refused_before_it_is_followed() {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir().unwrap();
    let real = real_of(home.path());
    std::fs::create_dir_all(&real).unwrap();
    let parent = real.parent().unwrap().to_path_buf();
    let links = home.path().join("links");
    std::fs::create_dir(&links).unwrap();
    let spellings: Vec<&str> = APFS_SPELLINGS.into_iter().chain(HFS_SPELLINGS).collect();
    for (n, spelling) in spellings.iter().enumerate() {
        symlink(parent.join(spelling), links.join(format!("l{n}"))).unwrap();
    }
    let seal = Sealed::new(&parent);
    if !seal.holds() {
        eprintln!("skipped: permissions are not enforced for this user (root?)");
        return;
    }
    for n in 0..spellings.len() {
        let given = links.join(format!("l{n}")).join("sample");
        let err = resolve(&launch(home.path(), &given)).unwrap_err();
        assert!(is_refusal(&err), "{}: {err}", given.display());
    }
}

#[test]
fn n1_a_folder_that_holds_the_real_folder_is_refused_as_text() {
    let mac = vec![format!("/Users/pat/Library/Application Support/{ID}")];
    for dir in [
        "/Users/pat/Library/Application Support",
        "/Users/pat/Library/Application Support/",
        "/Users/pat/Library/Application Support/.",
        "/Users/pat/Library/x/../Application Support",
        "/Users/pat/Library",
        "/Users/pat",
        "/USERS/PAT",
        "/Users",
        "/",
        // The data volume, which macOS firmlinks to the root (security-auditor V3).
        "/System/Volumes/Data/Users/pat/Library/Application Support",
        "/System/Volumes/Data/Users/pat",
        "/System/Volumes/Data",
    ] {
        assert!(
            refusal(Platform::MacOs, dir, &mac).is_some(),
            "macOS took {dir:?}"
        );
    }
    let windows = vec![format!(r"C:\Users\pat\AppData\Roaming\{ID}")];
    for dir in [
        r"C:\Users\pat\AppData\Roaming",
        r"c:\users\PAT\appdata\roaming\",
        r"C:/Users/pat/AppData",
        r"\\?\C:\Users\pat",
        r"C:\",
        // This computer's administrative share of drive C: (security-auditor V3).
        r"\\localhost\C$\Users\pat\AppData\Roaming",
        r"\\127.0.0.1\c$\Users\pat",
    ] {
        assert!(
            refusal(Platform::Windows, dir, &windows).is_some(),
            "Windows took {dir:?}"
        );
    }
    let linux = vec![format!("/home/pat/.local/share/{ID}")];
    for dir in ["/home/pat/.local/share", "/home/pat/.local", "/home/pat"] {
        assert!(
            refusal(Platform::Linux, dir, &linux).is_some(),
            "Linux took {dir:?}"
        );
    }
    // The same folders spelled as their file systems spell them: a decomposed accent, another case.
    let accented = vec![format!("/Users/jos\u{E9}/Library/Application Support/{ID}")];
    assert!(refusal(Platform::MacOs, "/Users/jose\u{301}", &accented).is_some());
    assert!(refusal(Platform::MacOs, "/Users/JOS\u{C9}/Library", &accented).is_some());
    // Folders beside it, or below a folder beside it, stay usable.
    for ok in [
        format!("/Users/pat/Library/Application Support/{ID}-sample"),
        "/Users/pat/Library/Application Support/other".to_owned(),
        "/Users/pat/samples".to_owned(),
        "/Users/patricia".to_owned(),
        "/Volumes/Data".to_owned(),
    ] {
        assert_eq!(refusal(Platform::MacOs, &ok, &mac), None, "{ok}");
    }
    for ok in [
        r"C:\Users\pat\AppData\Roaming\zwt-sample",
        r"C:\Users\pat\Documents",
        r"D:\",
    ] {
        assert_eq!(refusal(Platform::Windows, ok, &windows), None, "{ok}");
    }
}

/// `--sample` with the home folder or the folder the real one lives in: refused before anything is created there,
/// also through a link.
#[cfg(unix)]
#[test]
fn n1_resolve_refuses_a_folder_that_holds_the_real_one() {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir().unwrap();
    let real = real_of(home.path());
    let support = real.parent().unwrap().to_path_buf();
    let mut tries = vec![support.clone(), home.path().to_path_buf()];
    if let Some(library) = support.parent().filter(|p| *p != home.path()) {
        tries.push(library.to_path_buf());
    }
    let link = home.path().join("data");
    symlink(&support, &link).unwrap();
    tries.push(link);
    for given in &tries {
        let got = resolve(&launch(home.path(), given));
        assert!(
            matches!(&got, Err(e) if is_refusal(e)),
            "{}: {got:?}",
            given.display()
        );
    }
    assert!(
        !support.exists(),
        "refused before anything was created there"
    );
    // A folder of its own in the home folder is fine.
    let own = home.path().join("samples/m1");
    assert!(resolve(&launch(home.path(), &own)).is_ok());
}

/// SA2 F1: on macOS a path whose first folder's name starts with a dot is refused as text, whatever follows: macOS's
/// `/.nofollow` and `/.resolve/<n>` prefixes, which look the rest of the path up in their own way, `.vol` and `.file`
/// (which keep their own names in the refusal), any such name a later macOS adds, and the spellings a name key folds
/// to a dot. Below the root, and on the other platforms, a dot name is a plain name.
#[test]
fn sa2_f1_on_macos_a_first_folder_named_with_a_leading_dot_is_refused_as_text() {
    let mac = vec![format!("/Users/pat/Library/Application Support/{ID}")];
    for (dir, through) in [
        (
            "/.nofollow/Users/pat/Library/Application Support",
            "/.nofollow",
        ),
        ("/.nofollow/Users/pat", "/.nofollow"),
        ("/.nofollow/.vol/16777234/2/x", "/.nofollow"),
        ("/.NoFoLlOw/Users/pat/x", "/.NoFoLlOw"),
        ("/.resolve/1/Users/pat/samples", "/.resolve"),
        ("/./.nofollow/x", "/.nofollow"),
        ("/x/../.nofollow/x", "/.nofollow"),
        ("/System/Volumes/Data/.nofollow/Users/pat", "/.nofollow"),
        ("/.VOL/16777234/2", "/.vol"),
        ("/.future/x", "/.future"),
        ("/\u{FF0E}nofollow/x", "/\u{FF0E}nofollow"),
        ("/\u{200B}.nofollow/x", "/\u{200B}.nofollow"),
    ] {
        assert_eq!(
            refusal(Platform::MacOs, dir, &mac),
            Some(Refusal::SpecialPath(through.to_owned())),
            "{dir:?}"
        );
    }
    for ok in [
        "/Users/pat/.nofollow/x",
        "/Users/pat/samples/.m1",
        "/Volumes/x/.resolve/1",
    ] {
        assert_eq!(refusal(Platform::MacOs, ok, &mac), None, "{ok}");
    }
    let linux = vec![format!("/home/pat/.local/share/{ID}")];
    assert_eq!(
        refusal(Platform::Linux, "/.nofollow/home/pat/x", &linux),
        None
    );
}

/// SA2 F1, the write (2026-11-05, reproduced by nodes/SA2/nofollow-volfs.sh): `/.nofollow/.vol/<device>/<inode>/x`
/// names the fake real folder by number behind a prefix the text check let through, and `x` was made inside it before
/// the last check failed. Refused now as text, before any disk call, and the folder still holds only its marker.
#[cfg(target_os = "macos")]
#[test]
fn sa2_f1_a_nofollow_or_resolve_volfs_path_makes_nothing_in_the_real_folder() {
    let (home, real, vol) = home_with_marked_real();
    for prefix in [
        "/.nofollow",
        "/.NOFOLLOW",
        "//.nofollow",
        "/.nofollow/.",
        "/.resolve/1",
        "/.resolve/0",
    ] {
        for tail in ["x", "x/y", ""] {
            let given = PathBuf::from(format!("{prefix}/{vol}")).join(tail);
            let got = resolve(&launch(home.path(), &given));
            assert_eq!(
                entries(&real),
                ["marker"],
                "{}: made inside the real folder ({got:?})",
                given.display()
            );
            assert!(
                matches!(&got, Err(DataDirError::SpecialPath { .. })),
                "{}: {got:?}",
                given.display()
            );
        }
    }
}

/// SA2 F1, the N1 bypass: `/.nofollow` in front of the folder that holds the real one, or of the home folder, was
/// accepted, so sample data would have been kept beside the real folder (`/.nofollow<support>/x` made `x` there).
#[cfg(target_os = "macos")]
#[test]
fn sa2_f1_a_nofollow_spelling_of_a_folder_that_holds_the_real_one_is_refused() {
    let (home, real, _) = home_with_marked_real();
    let support = real.parent().unwrap().to_path_buf();
    for held in [
        support.clone(),
        support.join("x"),
        home.path().to_path_buf(),
    ] {
        for prefix in ["/.nofollow", "/.NOFOLLOW", "/.resolve/1"] {
            let given = PathBuf::from(format!("{prefix}{}", held.display()));
            let got = resolve(&launch(home.path(), &given));
            assert!(
                matches!(&got, Err(DataDirError::SpecialPath { .. })),
                "{}: {got:?}",
                given.display()
            );
        }
    }
    assert_eq!(entries(&real), ["marker"]);
    assert_eq!(
        entries(&support),
        [ID],
        "nothing made beside the real folder"
    );
}

/// SA2 F1 for links: a link whose target starts with such a prefix is refused before it is followed, its target being
/// checked as text like the path given.
#[cfg(target_os = "macos")]
#[test]
fn sa2_f1_a_link_to_a_nofollow_path_is_refused_before_it_is_followed() {
    use std::os::unix::fs::symlink;
    let (home, real, vol) = home_with_marked_real();
    let support = real.parent().unwrap().to_path_buf();
    let links = home.path().join("links");
    std::fs::create_dir(&links).unwrap();
    symlink(format!("/.nofollow/{vol}"), links.join("vol")).unwrap();
    symlink(
        format!("/.nofollow{}", support.display()),
        links.join("support"),
    )
    .unwrap();
    for given in [
        links.join("vol/x"),
        links.join("vol"),
        links.join("support"),
        links.join("support/x"),
    ] {
        let got = resolve(&launch(home.path(), &given));
        assert!(
            matches!(&got, Err(DataDirError::SpecialPath { .. })),
            "{}: {got:?}",
            given.display()
        );
    }
    assert_eq!(entries(&real), ["marker"]);
    assert_eq!(
        entries(&support),
        [ID],
        "nothing made beside the real folder"
    );
}
