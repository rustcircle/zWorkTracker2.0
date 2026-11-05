//! What draws the app's window (owner decision 2026-11-04, `src/renderer.rs`): FemtoVG on wgpu, picked in code, on
//! Metal on macOS and on Direct3D 12 or Vulkan on Windows, never OpenGL; `SLINT_BACKEND` overrides it for diagnosis.
//! The app tests run the built binary on a fresh sample folder and read the line it says on stderr.
//!
//! On macOS a window that is covered draws no frame with wgpu until it is uncovered, so the first test, which waits
//! for a frame, needs the window in front (as `tests/lifecycle.rs` does); the others do not wait for a frame.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use zwt_app::renderer::{self, Attempt, FEMTOVG_WGPU, SOFTWARE};
use zwt_bridge::datadir::Platform;

/// The variables that would change the choice, removed so that the test sees the app's own.
const CHOICE_VARIABLES: [&str; 5] = [
    "SLINT_BACKEND",
    "WGPU_BACKEND",
    "WGPU_ADAPTER_NAME",
    "WGPU_POWER_PREF",
    "SLINT_WGPU_CPU",
];

/// False on Linux without X11 or Wayland: the app cannot open its window there.
fn display_available() -> bool {
    !cfg!(target_os = "linux")
        || std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// The built app on the sample folder `dir`, with the choice variables removed and the log records of wgpu and
/// winit on from warnings up (`ZWT_LOG=warn`, which also has the line name the adapter).
fn app(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_zwt-app"));
    c.arg("--sample").arg(dir);
    for var in CHOICE_VARIABLES {
        c.env_remove(var);
    }
    c.env_remove("ZWT_DATA_DIR")
        .env_remove("ZWT_MEASURE")
        .env_remove("ZWT_MEASURE_EXIT")
        .env_remove("ZWT_MEASURE_LATENCY")
        .env("ZWT_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    c
}

/// A started app, killed when dropped unless it has ended: a failed test must not leave a window behind.
struct Running(Child);

impl Running {
    fn start(command: &mut Command) -> Self {
        Self(command.spawn().unwrap())
    }

    /// The app's exit, within 30 s.
    fn exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "the app did not end within 30 s");
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// A running app's stderr, line by line, keeping every line seen.
struct Stderr {
    lines: Receiver<String>,
    seen: Vec<String>,
}

impl Stderr {
    fn of(app: &mut Running) -> Self {
        let stderr = app.0.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    /// The first line, within 60 s, that starts with `prefix`.
    fn wait_for(&mut self, prefix: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self.lines.recv_timeout(left).unwrap_or_else(|e| {
                panic!(
                    "no {prefix:?} line within 60 s ({e:?}); a covered window draws nothing on macOS: {:?}",
                    self.seen
                )
            });
            self.seen.push(line.clone());
            if line.starts_with(prefix) {
                return line;
            }
        }
    }

    /// The rest, up to the end of the stream.
    fn read_rest(&mut self) {
        while let Ok(line) = self.lines.recv_timeout(Duration::from_secs(30)) {
            self.seen.push(line);
        }
    }
}

/// The value of `key=` in a line of `key=value` fields (a quoted value keeps its quotes).
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let at = line.find(&format!(" {key}="))? + key.len() + 2;
    let rest = line.get(at..)?;
    if rest.starts_with('"') {
        let end = rest.get(1..)?.find('"')? + 2;
        rest.get(..end)
    } else {
        rest.split(' ').next()
    }
}

/// The wgpu backends the app may draw with here, as the line names them.
fn platform_gpus() -> Vec<String> {
    renderer::attempts(Platform::host())
        .iter()
        .filter_map(|a| match a {
            Attempt::Wgpu(gpu) => Some(format!("{gpu:?}")),
            Attempt::Software => None,
        })
        .collect()
}

#[test]
fn the_window_is_drawn_by_femtovg_on_the_platforms_wgpu_backend() {
    if !display_available() {
        eprintln!("skipped: no display to open the app's window on");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // The first-frame hook quits after the first frame; with a hook on, the line names the adapter.
    let mut running = Running::start(
        app(dir.path())
            .env("ZWT_MEASURE", "1")
            .env("ZWT_MEASURE_EXIT", "1"),
    );
    let mut stderr = Stderr::of(&mut running);
    let line = stderr.wait_for("zwt-app renderer=");
    let gpus = platform_gpus();
    match field(&line, "renderer") {
        Some(FEMTOVG_WGPU) => {
            let backend = field(&line, "backend").unwrap();
            assert!(
                gpus.iter().any(|g| g == backend),
                "{backend} is one of {gpus:?}: {line}"
            );
            assert_ne!(backend, "Gl", "never OpenGL");
            assert!(
                field(&line, "adapter").is_some_and(|a| a.len() > 2 && a.starts_with('"')),
                "the adapter is named: {line}"
            );
            if cfg!(target_os = "macos") {
                assert_eq!(backend, "Metal", "{line}");
            }
            let first_frame = stderr.wait_for("zwt-measure first-frame ");
            for key in ["renderer", "adapter", "backend", "type"] {
                assert_eq!(
                    field(&first_frame, key),
                    field(&line, key),
                    "the measurement hooks see the same {key}: {first_frame}"
                );
            }
            let status = running.exit();
            stderr.read_rest();
            assert!(status.success(), "{status:?}: {:?}", stderr.seen);
        }
        // Where the platform's wgpu backends have no GPU adapter (a VM), the software renderer draws, and no
        // hook can quit the app after its first frame: dropping `running` stops it.
        Some(SOFTWARE) => {
            if cfg!(target_os = "macos") {
                panic!("a Mac has Metal: {line}");
            }
        }
        other => panic!("renderer {other:?}: {line}"),
    }
    let renderer_lines = stderr
        .seen
        .iter()
        .filter(|l| l.starts_with("zwt-app renderer="))
        .count();
    assert_eq!(renderer_lines, 1, "said once: {:?}", stderr.seen);
    // wgpu reports a validation error by panicking, and warns through `log`, which ZWT_LOG=warn prints.
    for bad in ["zwt-log ", "wgpu error", "Validation Error", "panicked"] {
        assert!(
            !stderr.seen.iter().any(|l| l.contains(bad)),
            "{bad:?} in stderr: {:?}",
            stderr.seen
        );
    }
}

/// Without a measurement hook or `ZWT_LOG` no rendering notifier is set (it costs FemtoVG an extra pass per frame), so
/// the line comes from the choice as the window is made, before any frame.
#[test]
fn without_a_hook_or_zwt_log_the_line_comes_from_the_choice_at_once() {
    if !display_available() {
        eprintln!("skipped: no display to open the app's window on");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut running = Running::start(app(dir.path()).env_remove("ZWT_LOG"));
    let mut stderr = Stderr::of(&mut running);
    let line = stderr.wait_for("zwt-app renderer=");
    if cfg!(target_os = "macos") {
        assert_eq!(
            line,
            "zwt-app renderer=femtovg-wgpu adapter=unknown backend=Metal"
        );
    } else if field(&line, "renderer") == Some(FEMTOVG_WGPU) {
        let backend = field(&line, "backend").unwrap();
        assert!(platform_gpus().iter().any(|g| g == backend), "{line}");
        assert_eq!(field(&line, "adapter"), Some("unknown"), "{line}");
    } else {
        assert!(
            line.starts_with("zwt-app renderer=software adapter=none backend=none"),
            "{line}"
        );
    }
}

#[test]
fn slint_backend_overrides_the_choice_for_diagnosis() {
    if !display_available() {
        eprintln!("skipped: no display to open the app's window on");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut running = Running::start(app(dir.path()).env("SLINT_BACKEND", "winit-software"));
    let mut stderr = Stderr::of(&mut running);
    // The software renderer has no rendering notifier, so the line comes before the event loop runs; and with no
    // notifier no hook could quit the app after a frame, so dropping `running` stops it.
    let line = stderr.wait_for("zwt-app renderer=");
    assert_eq!(
        line,
        r#"zwt-app renderer=software adapter=none backend=none SLINT_BACKEND="winit-software""#
    );
}

/// The fallback from one wgpu backend to the next relies on Slint refusing a backend without a GPU adapter inside
/// `BackendSelector::select()`, before it makes an event loop (which winit allows once per process, and on macOS
/// only on the main thread: a test thread that made one would panic), and on the words of that refusal. A Mac has
/// neither Direct3D 12 nor Vulkan (wgpu leaves both out of a macOS build), so Slint refuses both here as it refuses
/// them on a Windows machine without that GPU support.
#[cfg(target_os = "macos")]
#[test]
fn slint_refuses_a_wgpu_backend_without_a_gpu_before_making_an_event_loop() {
    use zwt_app::exit;
    use zwt_app::renderer::Gpu;

    if std::env::var_os("SLINT_WGPU_CPU").is_some() {
        eprintln!("skipped: SLINT_WGPU_CPU makes Slint skip the adapter check");
        return;
    }
    for gpu in [Gpu::Dx12, Gpu::Vulkan] {
        let refused = renderer::try_attempt(Attempt::Wgpu(gpu), exit::Handler).unwrap_err();
        assert!(
            refused.to_string().contains(renderer::NO_GPU_ADAPTER),
            "{gpu:?}: {refused}"
        );
    }
}
