//! Whether the WebView2 runtime is there, and what to tell the user when it is not.
//!
//! The window is a WebView2 page, so when the runtime is missing the shell can only say so in a native message. Both
//! the installer build and the Microsoft Store build run the same program, so both show these words. Everything here
//! is pure, so it is tested on every system.

/// Microsoft's page for the Evergreen WebView2 Runtime.
pub const DOWNLOAD_URL: &str = "https://developer.microsoft.com/en-us/microsoft-edge/webview2/";

/// The button that opens [`DOWNLOAD_URL`].
pub const OPEN_DOWNLOAD: &str = "Open download page";

/// The button that only closes the message.
pub const CLOSE: &str = "Close";

/// What Tauri panics with when the app can't finish starting, for example when no webview can be created.
const SETUP_PANIC: &str = "Failed to setup app";

/// How Tauri words an error returned by the app's own setup step, which runs after the window was created.
const SETUP_HOOK: &str = "error encountered during setup hook";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebviewCheck {
    Ready,
    Missing,
}

/// What asking Windows for the runtime's version means. An error is missing, and so is an answer with no version,
/// which some computers give when no runtime is installed.
#[must_use]
pub fn decide(version: Result<&str, &str>) -> WebviewCheck {
    match version {
        Ok(v) if !v.trim().is_empty() => WebviewCheck::Ready,
        _ => WebviewCheck::Missing,
    }
}

/// The message for a missing runtime.
#[must_use]
pub fn missing_text() -> String {
    format!(
        "zWorkTracker needs the Microsoft Edge WebView2 Runtime, and it wasn't found or is turned off on this \
         computer. Windows 11 includes it. Many Windows 10 computers have it, but not all.\n\n\
         Get the Evergreen runtime from Microsoft:\n{DOWNLOAD_URL}\n\n\
         Install it, then open zWorkTracker again. Your data is safe."
    )
}

/// A failure to start, as the user is told of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupFailure {
    pub text: String,
    /// The webview could not be created, so the message offers the runtime's download page.
    pub offer_download: bool,
}

/// For a panic while the app starts: `Some` only for Tauri's "Failed to setup app", which a release build would
/// otherwise end with nothing on screen. A webview that couldn't be created gets the runtime's message, anything else a
/// plain one, with the panic's own words beneath for whoever looks into it.
#[must_use]
pub fn startup_failure(panic_message: &str) -> Option<StartupFailure> {
    if !panic_message.contains(SETUP_PANIC) {
        return None;
    }
    let details = panic_message.trim();
    let webview =
        !panic_message.contains(SETUP_HOOK) && panic_message.to_lowercase().contains("webview");
    Some(if webview {
        StartupFailure {
            text: format!("{}\n\nDetails: {details}", missing_text()),
            offer_download: true,
        }
    } else {
        build_failure(details)
    })
}

/// For an error that stops the app before it has a window.
#[must_use]
pub fn build_failure(error: &str) -> StartupFailure {
    StartupFailure {
        text: format!(
            "zWorkTracker could not start. Your data is safe.\n\nDetails: {}",
            error.trim()
        ),
        offer_download: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test 60.
    #[test]
    fn decide_treats_an_error_or_a_blank_version_as_missing() {
        let cases: [(Result<&str, &str>, WebviewCheck); 6] = [
            (Err("0x80070002"), WebviewCheck::Missing),
            (Err(""), WebviewCheck::Missing),
            (Ok(""), WebviewCheck::Missing),
            (Ok("   "), WebviewCheck::Missing),
            (Ok("\t\n"), WebviewCheck::Missing),
            (Ok("130.0.2849.80"), WebviewCheck::Ready),
        ];
        for (version, want) in cases {
            assert_eq!(decide(version), want, "{version:?}");
        }
    }

    #[test]
    fn missing_text_names_both_windows_versions_and_gives_the_link() {
        let text = missing_text();
        assert!(text.contains(DOWNLOAD_URL));
        assert!(text.contains("Windows 11 includes it"));
        assert!(text.contains("Windows 10"));
        assert!(text.contains("Your data is safe."));
        assert!(
            !text.contains('\u{2014}'),
            "no em dashes in what users read"
        );
    }

    /// Test 61.
    #[test]
    fn startup_failure_text_only_for_setup_panics() {
        assert_eq!(startup_failure(""), None);
        assert_eq!(
            startup_failure("called `Option::unwrap()` on a `None` value"),
            None
        );
        assert_eq!(startup_failure("index out of bounds"), None);

        let no_runtime = "Failed to setup app: runtime error: Could not find the webview runtime, make sure it is installed";
        let failure = startup_failure(no_runtime).expect("a setup panic");
        assert!(failure.offer_download);
        assert!(failure.text.starts_with(&missing_text()));
        assert!(failure.text.ends_with(&format!("Details: {no_runtime}")));

        let creation = "Failed to setup app: runtime error: failed to create webview: WebView2 error: 0x80004005";
        assert!(
            startup_failure(creation)
                .expect("a setup panic")
                .offer_download
        );

        // The app's own setup runs after the window exists, so the runtime is there: no download offered.
        let hook =
            "Failed to setup app: error encountered during setup hook: could not build the tray";
        let failure = startup_failure(hook).expect("a setup panic");
        assert!(!failure.offer_download);
        assert!(failure.text.starts_with("zWorkTracker could not start."));
        assert!(failure.text.contains(hook));
        assert!(!failure.text.contains(DOWNLOAD_URL));

        let window = "Failed to setup app: runtime error: failed to create window";
        assert!(
            !startup_failure(window)
                .expect("a setup panic")
                .offer_download
        );
    }

    #[test]
    fn build_failure_says_what_went_wrong_without_the_runtime_link() {
        let failure = build_failure(" failed to initialize plugin `x`: no ");
        assert!(!failure.offer_download);
        assert_eq!(
            failure.text,
            "zWorkTracker could not start. Your data is safe.\n\nDetails: failed to initialize plugin `x`: no"
        );
    }
}
