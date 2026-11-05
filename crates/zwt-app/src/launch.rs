//! The data folder of milestone 1: `--sample <folder>` or `ZWT_DATA_DIR=<folder>`, checked by zwt-bridge's guard so
//! the real data folder is never opened. Without either, the app refuses to start.
//!
//! Also how `main` tells the user that the app did not start or could not go on: one line on stderr, and on Windows
//! the same line in [`RefusalWindow`], because a release build there has no console to print to (review N3).

use std::error::Error;

use zwt_bridge::datadir::{self, DataDir, Launch, Platform};

use crate::RefusalWindow;

/// The exit code of a refused start.
pub const REFUSED: u8 = 2;

/// The exit code of a run that failed after the start: the store could not be opened, the worker panicked, the
/// windowing platform failed, or the running task could not be paused on the way out.
pub const FAILED: u8 = 1;

/// Why the app does not start, or could not go on: the message, printed as it is (and shown on Windows), and the
/// exit code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub message: String,
    pub code: u8,
}

/// The folder to open, or why there is none.
pub fn data_dir(launch: &Launch) -> Result<DataDir, Refused> {
    datadir::resolve(launch).map_err(|e| Refused {
        message: e.to_string(),
        code: REFUSED,
    })
}

/// A run error, for `main` to report as it reports a refusal: [`message`] and [`FAILED`].
#[must_use]
pub fn failure(e: &(dyn Error + 'static)) -> Refused {
    Refused {
        message: message(e),
        code: FAILED,
    }
}

/// `e` and each error below it on one line, after the program's name: "zwt-app: could not open the data in /x:
/// could not read data: Permission denied (os error 13)". A cause whose text the line already ends with (an error
/// that prints its source, as `CoreError::Read` does) is not repeated.
#[must_use]
pub fn message(e: &(dyn Error + 'static)) -> String {
    let mut line = format!("zwt-app: {e}");
    let mut source = e.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !line.ends_with(&text) {
            line.push_str(": ");
            line.push_str(&text);
        }
        source = cause.source();
    }
    line
}

/// Whether `main` also shows a refusal or a run error in [`RefusalWindow`] on `platform`: on Windows, where a
/// release build has no console and a debug build's console closes with the process. Elsewhere milestone 1 runs
/// from a terminal (it needs `--sample` or `ZWT_DATA_DIR`), which shows stderr; an app bundle started from the
/// Finder has no stderr to see, so this grows to macOS once one is shipped.
#[must_use]
pub const fn shows_window(platform: Platform) -> bool {
    matches!(platform, Platform::Windows)
}

/// The window that shows `message`; `run()` it to show it until the user closes it.
pub fn message_window(message: &str) -> Result<RefusalWindow, slint::PlatformError> {
    let window = RefusalWindow::new()?;
    window.set_message(message.into());
    Ok(window)
}
