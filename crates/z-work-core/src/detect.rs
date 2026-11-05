//! Finds sync folders of cloud desktop clients. Pure: environment and filesystem are injected, so
//! the Windows rules are unit-tested on any host. Real Windows detection needs a real Windows check.

use serde::Serialize;

use crate::model::SyncProvider;

pub trait DetectEnv {
    fn var(&self, name: &str) -> Option<String>;
    fn is_dir(&self, path: &str) -> bool;
    fn list_dir(&self, path: &str) -> Vec<String>;
    /// Where Google Drive's own settings say it mounts, on Windows. A drive letter or a path.
    fn drive_mount_points(&self) -> Vec<String> {
        Vec::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detected {
    pub provider: SyncProvider,
    pub root: String,
    pub source: String,
}

/// "My Drive" as the Google Drive client names it in common display languages.
const MY_DRIVE_NAMES: &[&str] = &[
    "My Drive",
    "Meu Drive",
    "O meu disco",
    "Mi unidad",
    "Mon Drive",
    "Mein Drive",
    "Il mio Drive",
    "Mijn Drive",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOs {
    Windows,
    Mac,
    Other,
}

#[must_use]
pub fn detect(env: &dyn DetectEnv, os: HostOs) -> Vec<Detected> {
    let mut found: Vec<Detected> = Vec::new();
    let mut add = |provider, root: String, source: String, env: &dyn DetectEnv| {
        if env.is_dir(&root) && !found.iter().any(|d| d.root == root) {
            found.push(Detected {
                provider,
                root,
                source,
            });
        }
    };
    match os {
        HostOs::Windows => {
            for var in ["OneDriveCommercial", "OneDriveConsumer", "OneDrive"] {
                if let Some(path) = env.var(var).filter(|p| !p.trim().is_empty()) {
                    add(
                        SyncProvider::OneDrive,
                        path.trim_end_matches('\\').to_owned(),
                        format!("%{var}%"),
                        env,
                    );
                }
            }
            // Google Drive's own settings first, then the scan as a fallback.
            for mount in env.drive_mount_points() {
                let mount = mount.trim().trim_end_matches(['\\', '/']);
                if mount.is_empty() {
                    continue;
                }
                let base = if mount.len() == 1 {
                    format!("{mount}:")
                } else {
                    mount.to_owned()
                };
                add(
                    SyncProvider::GoogleDrive,
                    base.clone(),
                    "Google Drive settings".to_owned(),
                    env,
                );
                for name in MY_DRIVE_NAMES {
                    add(
                        SyncProvider::GoogleDrive,
                        format!("{base}\\{name}"),
                        "Google Drive settings".to_owned(),
                        env,
                    );
                }
            }
            for letter in b'D'..=b'Z' {
                let letter = char::from(letter);
                for name in MY_DRIVE_NAMES {
                    let path = format!("{letter}:\\{name}");
                    add(
                        SyncProvider::GoogleDrive,
                        path,
                        format!("drive {letter}:"),
                        env,
                    );
                }
            }
        }
        HostOs::Mac => {
            if let Some(home) = env.var("HOME") {
                let cloud = format!("{home}/Library/CloudStorage");
                for entry in env.list_dir(&cloud) {
                    let path = format!("{cloud}/{entry}");
                    if entry.starts_with("OneDrive") {
                        add(SyncProvider::OneDrive, path, "CloudStorage".to_owned(), env);
                    } else if entry.starts_with("GoogleDrive") {
                        for name in MY_DRIVE_NAMES {
                            add(
                                SyncProvider::GoogleDrive,
                                format!("{path}/{name}"),
                                "CloudStorage".to_owned(),
                                env,
                            );
                        }
                    }
                }
            }
        }
        HostOs::Other => {}
    }
    found
}

/// The real environment.
pub struct RealEnv;

impl DetectEnv for RealEnv {
    #[cfg(windows)]
    fn drive_mount_points(&self) -> Vec<String> {
        use winreg::RegKey;
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        let mut out = Vec::new();
        let read = |root, path: &str, value: &str| -> Option<String> {
            RegKey::predef(root)
                .open_subkey(path)
                .ok()?
                .get_value(value)
                .ok()
        };
        out.extend(read(
            HKEY_CURRENT_USER,
            "Software\\Google\\DriveFS\\Share",
            "MountPointPath",
        ));
        out.extend(read(
            HKEY_CURRENT_USER,
            "Software\\Google\\DriveFS",
            "DefaultMountPoint",
        ));
        out.extend(read(
            HKEY_LOCAL_MACHINE,
            "SOFTWARE\\Google\\DriveFS",
            "DefaultMountPoint",
        ));
        out
    }
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
    fn is_dir(&self, path: &str) -> bool {
        std::path::Path::new(path).is_dir()
    }
    fn list_dir(&self, path: &str) -> Vec<String> {
        std::fs::read_dir(path)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[must_use]
pub fn host_os() -> HostOs {
    if cfg!(windows) {
        HostOs::Windows
    } else if cfg!(target_os = "macos") {
        HostOs::Mac
    } else {
        HostOs::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Default)]
    struct Fake {
        vars: BTreeMap<String, String>,
        dirs: BTreeSet<String>,
        lists: BTreeMap<String, Vec<String>>,
    }
    impl DetectEnv for Fake {
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).cloned()
        }
        fn is_dir(&self, path: &str) -> bool {
            self.dirs.contains(path)
        }
        fn list_dir(&self, path: &str) -> Vec<String> {
            self.lists.get(path).cloned().unwrap_or_default()
        }
    }

    #[test]
    fn windows_env_vars_and_drive_scan() {
        let mut f = Fake::default();
        f.vars
            .insert("OneDrive".into(), r"C:\Users\p\OneDrive\".into());
        f.vars
            .insert("OneDriveConsumer".into(), r"C:\Users\p\OneDrive".into());
        f.vars.insert(
            "OneDriveCommercial".into(),
            r"C:\Users\p\OneDrive - Acme".into(),
        );
        f.dirs.insert(r"C:\Users\p\OneDrive".into());
        f.dirs.insert(r"C:\Users\p\OneDrive - Acme".into());
        f.dirs.insert(r"G:\My Drive".into());
        let d = detect(&f, HostOs::Windows);
        let got: Vec<_> = d.iter().map(|x| (x.provider, x.root.as_str())).collect();
        assert_eq!(
            got,
            [
                (SyncProvider::OneDrive, r"C:\Users\p\OneDrive - Acme"),
                (SyncProvider::OneDrive, r"C:\Users\p\OneDrive"),
                (SyncProvider::GoogleDrive, r"G:\My Drive"),
            ]
        );
    }

    #[test]
    fn localized_my_drive_and_nothing_found() {
        let mut f = Fake::default();
        assert!(detect(&f, HostOs::Windows).is_empty());
        f.vars.insert("OneDrive".into(), r"C:\gone".into());
        assert!(
            detect(&f, HostOs::Windows).is_empty(),
            "a variable pointing nowhere is ignored"
        );
        f.dirs.insert(r"H:\Meu Drive".into());
        assert_eq!(
            detect(&f, HostOs::Windows)[0].provider,
            SyncProvider::GoogleDrive
        );
    }

    #[test]
    fn mac_cloud_storage() {
        let mut f = Fake::default();
        f.vars.insert("HOME".into(), "/Users/someone".into());
        f.lists.insert(
            "/Users/someone/Library/CloudStorage".into(),
            vec![
                "OneDrive-Personal".into(),
                "GoogleDrive-x@y.z".into(),
                "Dropbox".into(),
            ],
        );
        f.dirs
            .insert("/Users/someone/Library/CloudStorage/OneDrive-Personal".into());
        f.dirs
            .insert("/Users/someone/Library/CloudStorage/GoogleDrive-x@y.z/My Drive".into());
        let d = detect(&f, HostOs::Mac);
        assert_eq!(d.len(), 2);
        assert_eq!(
            d[1].root,
            "/Users/someone/Library/CloudStorage/GoogleDrive-x@y.z/My Drive"
        );
    }

    #[test]
    fn google_drives_own_settings_come_before_the_scan() {
        struct Mounted;
        impl DetectEnv for Mounted {
            fn var(&self, _: &str) -> Option<String> {
                None
            }
            fn is_dir(&self, path: &str) -> bool {
                path == "G:\\My Drive" || path == "C:\\Drive" || path == "D:\\My Drive"
            }
            fn list_dir(&self, _: &str) -> Vec<String> {
                Vec::new()
            }
            fn drive_mount_points(&self) -> Vec<String> {
                vec!["G".into(), "C:\\Drive\\".into(), "  ".into()]
            }
        }
        let found = detect(&Mounted, HostOs::Windows);
        let roots: Vec<&str> = found.iter().map(|d| d.root.as_str()).collect();
        assert_eq!(roots, ["G:\\My Drive", "C:\\Drive", "D:\\My Drive"]);
        assert_eq!(found[0].source, "Google Drive settings");
        assert_eq!(
            found[2].source, "drive D:",
            "the scan is still the fallback"
        );
    }
}
