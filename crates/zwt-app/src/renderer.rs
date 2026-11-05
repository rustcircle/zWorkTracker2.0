//! What draws the app's windows: Slint's FemtoVG renderer on wgpu, chosen here in code, not by `SLINT_BACKEND` or
//! Slint's default (owner decision, 2026-11-04: OpenGL is deprecated on macOS and weak in Windows VMs).
//!
//! - wgpu draws on Metal on macOS; on Windows on Direct3D 12, or on Vulkan where Direct3D 12 has no GPU adapter; on
//!   Linux (not shipped) on Vulkan. Never on OpenGL: wgpu's OpenGL backend is compiled in, because Slint's FemtoVG
//!   asks for wgpu's default features, but [`select`] never asks for it. Slint's own default would take Vulkan
//!   before Direct3D 12 on Windows (wgpu lists Vulkan adapters first), and leaves OpenGL out unless `WGPU_BACKEND`
//!   asks for it alone.
//! - Slint refuses a wgpu backend without a GPU adapter (a CPU one such as WARP does not count; `SLINT_WGPU_CPU`
//!   skips that check, and the first backend is then taken as it is); [`select`] then tries the next. Where none has
//!   one, Slint's software renderer draws. This covers a missing adapter only: a failure later, when the window's
//!   surface or device is made, ends the run with an error, because the platform and its event loop can be made
//!   once per process.
//! - `SLINT_BACKEND`, when set, replaces all of this, for diagnosis: Slint then picks as it says, with its own wgpu
//!   defaults (`SLINT_BACKEND=winit-software` draws in software; on Windows a wgpu renderer named there loads
//!   `dxcompiler.dll` by name again, which [`settings`] prevents).
//!
//! What draws is said once on stderr ([`report`]). The adapter is looked up through the window's rendering notifier,
//! which costs FemtoVG an extra pass per frame, so only while a measurement hook runs, `ZWT_LOG` is set or
//! `SLINT_BACKEND` chose: `zwt-app renderer=femtovg-wgpu adapter="Apple M3 Ultra" backend=Metal type=IntegratedGpu`,
//! when the first frame is set up. Otherwise the line comes from the choice, as the window is made:
//! `zwt-app renderer=femtovg-wgpu adapter=unknown backend=Metal`. The software renderer's line is
//! `zwt-app renderer=software adapter=none backend=none`. [`picked`] keeps what was said for the measurement hooks.

use std::cell::RefCell;
use std::fmt;

use slint::wgpu_30::{WGPUConfiguration, WGPUSettings, wgpu};
use slint::winit_030::CustomApplicationHandler;
use slint::{
    BackendSelector, GraphicsAPI, PlatformError, RenderingState, SetRenderingNotifierError,
};
use zwt_bridge::datadir::Platform;

/// Slint's winit backend, the only one built.
pub const BACKEND: &str = "winit";

/// Slint's FemtoVG renderer on wgpu (Cargo feature `renderer-femtovg-wgpu`): what the app draws with.
pub const FEMTOVG_WGPU: &str = "femtovg-wgpu";

/// Slint's software renderer (Cargo feature `renderer-software`): what draws when no GPU adapter is found.
pub const SOFTWARE: &str = "software";

/// What Slint 1.18.1 says when `BackendSelector::select` refuses a wgpu backend without a GPU adapter, before it
/// makes the event loop. Only this refusal lets [`select`] go on to the next attempt; tests/renderer.rs pins it.
pub const NO_GPU_ADAPTER: &str = "no GPU-backed WGPU adapter is available";

/// A wgpu backend the app draws with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gpu {
    Metal,
    Dx12,
    Vulkan,
}

impl Gpu {
    /// This backend alone, as wgpu names backends.
    #[must_use]
    pub const fn backends(self) -> wgpu::Backends {
        match self {
            Self::Metal => wgpu::Backends::METAL,
            Self::Dx12 => wgpu::Backends::DX12,
            Self::Vulkan => wgpu::Backends::VULKAN,
        }
    }
}

/// One way to draw, as [`select`] asks Slint for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attempt {
    /// FemtoVG on wgpu with this backend only.
    Wgpu(Gpu),
    /// The software renderer.
    Software,
}

impl fmt::Display for Attempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wgpu(gpu) => write!(f, "{FEMTOVG_WGPU} on {gpu:?}"),
            Self::Software => f.write_str(SOFTWARE),
        }
    }
}

/// The ways to draw on `platform`, best first.
#[must_use]
pub const fn attempts(platform: Platform) -> &'static [Attempt] {
    match platform {
        Platform::MacOs => &[Attempt::Wgpu(Gpu::Metal), Attempt::Software],
        Platform::Windows => &[
            Attempt::Wgpu(Gpu::Dx12),
            Attempt::Wgpu(Gpu::Vulkan),
            Attempt::Software,
        ],
        Platform::Linux => &[Attempt::Wgpu(Gpu::Vulkan), Attempt::Software],
    }
}

/// Slint's wgpu settings with `gpu`'s backend alone; the rest as Slint makes them (power preference from
/// `WGPU_POWER_PREF`, instance flags from the build and `WGPU_*` variables), except Direct3D 12's shader compiler and
/// runtime, which no variable changes. `WGPU_BACKEND` does not count here.
///
/// Direct3D 12 compiles shaders with FXC and runs on the D3D12 runtime of Windows, with no Agility SDK (security
/// audit SA2 F2, CWE-427). wgpu's default, `Dx12Compiler::Auto`, loads `dxcompiler.dll` by its bare name
/// (`LoadLibraryExW("dxcompiler.dll", NULL, 0)`, wgpu-hal 30.0.1 `dx12/instance.rs`): where no copy sits beside the
/// app or in the system folders, as on most machines, the search goes on to the current folder and every folder on
/// `PATH`, and a `dxcompiler.dll` planted there would run inside the app. FXC is `d3dcompiler_47.dll`, which Windows
/// 10 and 11 ship in System32, where the search finds it before the current folder and `PATH`; `Auto` falls back to
/// it whenever no DXC is found, so it is what most machines drew with already. `WGPU_DX12_COMPILER` and
/// `WGPU_DX12_AGILITY_SDK_PATH`, which Slint's defaults read, would bring such a load back.
///
/// Milestone 3, in the platform crate (unsafe FFI): `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)` at
/// the start of `main`, so that no DLL any crate loads by name is looked up in the current folder or on `PATH`.
#[must_use]
pub fn settings(gpu: Gpu) -> WGPUSettings {
    let mut settings = WGPUSettings::default();
    settings.backends = gpu.backends();
    settings.backend_options.dx12.shader_compiler = wgpu::Dx12Compiler::Fxc;
    settings.backend_options.dx12.agility_sdk = None;
    settings
}

/// How the renderer is picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// In code: the first of [`attempts`] for this platform that Slint accepts.
    InCode(Platform),
    /// As `SLINT_BACKEND` says (`winit-software`, `winit-femtovg-wgpu`, ...), for diagnosis.
    SlintBackend(String),
}

impl Request {
    /// From this process's environment.
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(Platform::host(), std::env::var("SLINT_BACKEND").ok())
    }

    /// From `SLINT_BACKEND`'s value, trimmed; an empty one does not count.
    #[must_use]
    pub fn new(platform: Platform, slint_backend: Option<String>) -> Self {
        match slint_backend
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            Some(value) => Self::SlintBackend(value.to_owned()),
            None => Self::InCode(platform),
        }
    }
}

/// What [`select`] made the platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selected {
    /// The attempt Slint accepted, and the ones it refused before it (a wgpu backend without a GPU adapter).
    InCode {
        attempt: Attempt,
        refused: Vec<Attempt>,
    },
    /// What `SLINT_BACKEND` said.
    SlintBackend(String),
}

/// Makes Slint's winit backend the platform, drawing as `request` says, with the handler `handler` makes for each
/// attempt (Slint drops it with an attempt it refuses). Call it before the first window is made. Slint's refusal of a
/// wgpu backend without a GPU adapter ([`NO_GPU_ADAPTER`]) leads to the next attempt; any other error ends the
/// choice. The error then names each attempt made, with Slint's reason.
pub fn select<H: CustomApplicationHandler + 'static>(
    request: &Request,
    handler: impl Fn() -> H,
) -> Result<Selected, PlatformError> {
    match request {
        Request::SlintBackend(value) => {
            BackendSelector::new()
                .backend_name(value.clone())
                .with_winit_custom_application_handler(handler())
                .select()?;
            Ok(Selected::SlintBackend(value.clone()))
        }
        Request::InCode(platform) => {
            let tried = first_accepted(
                attempts(*platform),
                |attempt| try_attempt(attempt, handler()),
                |e| e.to_string().contains(NO_GPU_ADAPTER),
            );
            match tried.accepted {
                Some(attempt) => Ok(Selected::InCode {
                    attempt,
                    refused: tried.refused.into_iter().map(|(a, _)| a).collect(),
                }),
                None => Err(PlatformError::Other(no_renderer(&tried.refused))),
            }
        }
    }
}

/// Asks Slint for `attempt` with `handler` installed, and makes it the platform when Slint accepts it. Slint checks a
/// wgpu attempt's adapters first, in `select()` before it makes the event loop, so a refused attempt leaves nothing
/// behind (tests/renderer.rs pins this).
pub fn try_attempt<H: CustomApplicationHandler + 'static>(
    attempt: Attempt,
    handler: H,
) -> Result<(), PlatformError> {
    let selector = BackendSelector::new().backend_name(BACKEND.into());
    let selector = match attempt {
        Attempt::Wgpu(gpu) => selector
            .renderer_name(FEMTOVG_WGPU.into())
            .require_wgpu_30(WGPUConfiguration::Automatic(settings(gpu))),
        Attempt::Software => selector.renderer_name(SOFTWARE.into()),
    };
    selector
        .with_winit_custom_application_handler(handler)
        .select()
}

/// What [`first_accepted`] found: the attempt accepted, if one was, and the ones refused before it, each with why.
#[derive(Debug, PartialEq, Eq)]
struct Tried<A, E> {
    accepted: Option<A>,
    refused: Vec<(A, E)>,
}

/// Tries `attempts` in order until `try_one` accepts one, going on after a refusal only when `next_may_follow` says
/// so; none is tried after the accepted one.
fn first_accepted<A: Copy, E>(
    attempts: &[A],
    mut try_one: impl FnMut(A) -> Result<(), E>,
    next_may_follow: impl Fn(&E) -> bool,
) -> Tried<A, E> {
    let mut refused = Vec::new();
    for &attempt in attempts {
        match try_one(attempt) {
            Ok(()) => {
                return Tried {
                    accepted: Some(attempt),
                    refused,
                };
            }
            Err(e) => {
                let go_on = next_may_follow(&e);
                refused.push((attempt, e));
                if !go_on {
                    break;
                }
            }
        }
    }
    Tried {
        accepted: None,
        refused,
    }
}

fn no_renderer(all: &[(Attempt, PlatformError)]) -> String {
    let each: Vec<String> = all.iter().map(|(a, e)| format!("{a}: {e}")).collect();
    format!("no renderer could be set up ({})", each.join("; "))
}

/// The wgpu adapter a window draws with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Adapter {
    /// As the driver names it: "Apple M3 Ultra", "NVIDIA GeForce RTX 4070".
    pub name: String,
    /// wgpu's device type: "IntegratedGpu", "DiscreteGpu", "VirtualGpu", "Cpu", "Other".
    pub device_type: String,
    /// The driver's name and version, when the backend gives them (Vulkan does; Metal does not).
    pub driver: String,
}

/// What draws a window, as [`report`] said it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picked {
    /// [`FEMTOVG_WGPU`], [`SOFTWARE`], or Slint's name for another graphics API.
    pub renderer: String,
    /// wgpu's backend ("Metal", "Dx12", "Vulkan"), as the adapter names it, or as the choice asked for it when the
    /// adapter was not looked up.
    pub backend: Option<String>,
    /// The adapter, when it was looked up through the rendering notifier.
    pub adapter: Option<Adapter>,
}

impl Picked {
    /// From the graphics API Slint hands the rendering notifier when a window's rendering is set up.
    #[must_use]
    pub fn from_api(api: &GraphicsAPI<'_>) -> Self {
        match api {
            GraphicsAPI::WGPU30 { device, .. } => Self::from_info(&device.adapter_info()),
            other => Self {
                renderer: format!("{other:?}"),
                backend: None,
                adapter: None,
            },
        }
    }

    /// FemtoVG on wgpu with the adapter `info` describes.
    #[must_use]
    pub fn from_info(info: &wgpu::AdapterInfo) -> Self {
        let driver = [info.driver.trim(), info.driver_info.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            renderer: FEMTOVG_WGPU.to_owned(),
            backend: Some(format!("{:?}", info.backend)),
            adapter: Some(Adapter {
                name: info.name.clone(),
                device_type: format!("{:?}", info.device_type),
                driver,
            }),
        }
    }

    /// From the choice alone, without looking the adapter up. `SLINT_BACKEND`'s choice is not known here.
    #[must_use]
    pub fn chosen(selected: &Selected) -> Self {
        match selected {
            Selected::InCode {
                attempt: Attempt::Wgpu(gpu),
                ..
            } => Self {
                renderer: FEMTOVG_WGPU.to_owned(),
                backend: Some(format!("{gpu:?}")),
                adapter: None,
            },
            Selected::InCode {
                attempt: Attempt::Software,
                ..
            } => Self::software(),
            Selected::SlintBackend(_) => Self {
                renderer: "unknown".to_owned(),
                backend: None,
                adapter: None,
            },
        }
    }

    /// The software renderer, which takes no rendering notifier.
    #[must_use]
    pub fn software() -> Self {
        Self {
            renderer: SOFTWARE.to_owned(),
            backend: None,
            adapter: None,
        }
    }

    /// `renderer=femtovg-wgpu adapter="Apple M3 Ultra" backend=Metal type=IntegratedGpu`, and `driver="..."` when
    /// there is one; `renderer=femtovg-wgpu adapter=unknown backend=Metal` when the adapter was not looked up;
    /// `renderer=software adapter=none backend=none` without a GPU. The measurement hooks add this to their
    /// first-frame line.
    #[must_use]
    pub fn fields(&self) -> String {
        let backend = self.backend.as_deref().unwrap_or("unknown");
        match (&self.adapter, self.renderer.as_str()) {
            (Some(a), _) => {
                let mut s = format!(
                    "renderer={} adapter={:?} backend={backend} type={}",
                    self.renderer, a.name, a.device_type
                );
                if !a.driver.is_empty() {
                    s.push_str(&format!(" driver={:?}", a.driver));
                }
                s
            }
            (None, SOFTWARE) => format!("renderer={SOFTWARE} adapter=none backend=none"),
            (None, renderer) => format!("renderer={renderer} adapter=unknown backend={backend}"),
        }
    }

    /// The line said on stderr: [`Self::fields`] after `zwt-app`, then how it was chosen when that was not the first
    /// attempt (`refused=Dx12`: Slint refused Direct3D 12, which had no GPU adapter) or not in code
    /// (`SLINT_BACKEND="winit-software"`).
    #[must_use]
    pub fn line(&self, selected: &Selected) -> String {
        let mut line = format!("zwt-app {}", self.fields());
        match selected {
            Selected::InCode { refused, .. } if !refused.is_empty() => {
                let names: Vec<String> = refused
                    .iter()
                    .map(|a| match a {
                        Attempt::Wgpu(gpu) => format!("{gpu:?}"),
                        Attempt::Software => SOFTWARE.to_owned(),
                    })
                    .collect();
                line.push_str(&format!(" refused={}", names.join(",")));
            }
            Selected::InCode { .. } => {}
            Selected::SlintBackend(value) => line.push_str(&format!(" SLINT_BACKEND={value:?}")),
        }
        line
    }
}

thread_local! {
    /// What [`record`] said first (the UI thread's windows).
    static PICKED: RefCell<Option<Picked>> = const { RefCell::new(None) };
}

/// What draws the app's window, once [`report`] has said it; `None` before.
#[must_use]
pub fn picked() -> Option<Picked> {
    PICKED.with_borrow(Clone::clone)
}

/// Says `picked`'s line and keeps it, the first time only: another window, or one that sets its rendering up again
/// (after a suspend, say), is not said again.
fn record(picked: Picked, selected: &Selected) {
    if PICKED.with_borrow(Option::is_some) {
        return;
    }
    eprintln!("{}", picked.line(selected));
    PICKED.set(Some(picked));
}

/// Hooks run on each rendering state after [`report`] has looked at it (the measurement hooks).
pub type Frames = Box<dyn FnMut(&RenderingState)>;

/// Says on stderr, once, what draws `window`, and runs `frames` on each of its rendering states.
///
/// The adapter is looked up through the window's rendering notifier (a window takes one, so this is the app's only
/// one) when `frames` are given, when `look_up` is set (a diagnosis switch such as `ZWT_LOG`) or when `SLINT_BACKEND`
/// chose; the line then comes when the first frame is set up. FemtoVG draws an extra pass per frame while a notifier
/// is set, so otherwise none is set and the line comes now, from the choice. The software renderer takes no notifier:
/// its line comes now too, and the error is returned (no frame hook can run).
pub fn report(
    window: &slint::Window,
    selected: Selected,
    frames: Option<Frames>,
    look_up: bool,
) -> Result<(), SetRenderingNotifierError> {
    let watch = look_up || frames.is_some() || matches!(selected, Selected::SlintBackend(_));
    if !watch {
        record(Picked::chosen(&selected), &selected);
        return Ok(());
    }
    let seen = selected.clone();
    let mut frames = frames;
    let set = window.set_rendering_notifier(move |state, api| {
        if matches!(state, RenderingState::RenderingSetup) {
            record(Picked::from_api(api), &seen);
        }
        if let Some(frames) = frames.as_mut() {
            frames(&state);
        }
    });
    if let Err(SetRenderingNotifierError::Unsupported) = set {
        record(Picked::software(), &selected);
    }
    set
}

/// Prints the `log` records of the crates below Slint (wgpu, naga, winit) on stderr from the level `ZWT_LOG` names
/// (`error`, `warn`, `info`, `debug`, `trace`), as `zwt-log WARN wgpu_hal::metal: ...`. True when it did install
/// the logger: `ZWT_LOG` names a level, `off` included. No logger is installed otherwise; a second call changes
/// nothing.
pub fn log_from_env() -> bool {
    let Some(level) = std::env::var("ZWT_LOG")
        .ok()
        .and_then(|v| v.trim().parse::<log::LevelFilter>().ok())
    else {
        return false;
    };
    let installed = log::set_logger(&STDERR_LOG).is_ok();
    if installed {
        log::set_max_level(level);
    }
    installed
}

struct StderrLog;

static STDERR_LOG: StderrLog = StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!(
                "zwt-log {} {}: {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test code: a failed lookup is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn the_app_asks_for_femtovg_on_wgpu_then_software() {
        assert_eq!(FEMTOVG_WGPU, "femtovg-wgpu");
        assert_eq!(SOFTWARE, "software");
        assert_eq!(BACKEND, "winit");
        for platform in [Platform::MacOs, Platform::Windows, Platform::Linux] {
            let all = attempts(platform);
            assert_eq!(all.last(), Some(&Attempt::Software), "{platform:?}");
            assert!(
                all.iter()
                    .filter(|a| **a != Attempt::Software)
                    .all(|a| matches!(a, Attempt::Wgpu(_))),
                "{platform:?}"
            );
        }
    }

    #[test]
    fn metal_on_macos_direct3d_12_then_vulkan_on_windows_never_opengl() {
        assert_eq!(
            attempts(Platform::MacOs),
            &[Attempt::Wgpu(Gpu::Metal), Attempt::Software]
        );
        assert_eq!(
            attempts(Platform::Windows),
            &[
                Attempt::Wgpu(Gpu::Dx12),
                Attempt::Wgpu(Gpu::Vulkan),
                Attempt::Software
            ]
        );
        assert_eq!(
            attempts(Platform::Linux),
            &[Attempt::Wgpu(Gpu::Vulkan), Attempt::Software]
        );
        for gpu in [Gpu::Metal, Gpu::Dx12, Gpu::Vulkan] {
            let backends = settings(gpu).backends;
            assert_eq!(backends, gpu.backends(), "{gpu:?} alone");
            assert!(!backends.contains(wgpu::Backends::GL), "{gpu:?}");
            assert_eq!(backends.iter().count(), 1, "{gpu:?}");
        }
    }

    /// Security audit SA2 F2: Direct3D 12 compiles with FXC, never `dxcompiler.dll` found by name, and loads no
    /// Agility SDK, whatever `WGPU_DX12_COMPILER` and `WGPU_DX12_AGILITY_SDK_*` say. A run of this test with them set
    /// (a child, since setting variables here would race the other tests) sees Slint's defaults follow them and the
    /// app's settings not.
    #[test]
    fn direct3d_12_compiles_with_fxc_and_no_agility_sdk_whatever_the_environment_says() {
        const CHILD: &str = "ZWT_TEST_DX12_CHILD";
        let child = std::env::var_os(CHILD).is_some();
        for gpu in [Gpu::Metal, Gpu::Dx12, Gpu::Vulkan] {
            let dx12 = settings(gpu).backend_options.dx12;
            assert!(
                matches!(dx12.shader_compiler, wgpu::Dx12Compiler::Fxc),
                "{gpu:?}: {:?}",
                dx12.shader_compiler
            );
            assert!(
                dx12.agility_sdk.is_none(),
                "{gpu:?}: {:?}",
                dx12.agility_sdk
            );
        }
        let defaults = WGPUSettings::default().backend_options.dx12;
        if child {
            assert!(
                matches!(
                    defaults.shader_compiler,
                    wgpu::Dx12Compiler::DynamicDxc { .. }
                ),
                "the variable reached Slint's defaults: {:?}",
                defaults.shader_compiler
            );
            assert!(defaults.agility_sdk.is_some(), "{:?}", defaults.agility_sdk);
            return;
        }
        let name = "renderer::tests::direct3d_12_compiles_with_fxc_and_no_agility_sdk_whatever_the_environment_says";
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([name, "--exact", "--test-threads=1"])
            .env(CHILD, "1")
            .env("WGPU_DX12_COMPILER", "dxc")
            .env("WGPU_DX12_AGILITY_SDK_PATH", r".\agility")
            .env("WGPU_DX12_AGILITY_SDK_VERSION", "614")
            .output()
            .unwrap();
        let report = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && report.contains("1 passed"),
            "{report}{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn slint_backend_replaces_the_choice_only_when_it_says_something() {
        assert_eq!(
            Request::new(Platform::Windows, None),
            Request::InCode(Platform::Windows)
        );
        assert_eq!(
            Request::new(Platform::MacOs, Some("  ".to_owned())),
            Request::InCode(Platform::MacOs)
        );
        assert_eq!(
            Request::new(Platform::MacOs, Some(" winit-software\n".to_owned())),
            Request::SlintBackend("winit-software".to_owned())
        );
    }

    /// Slint's refusal of a backend without a GPU adapter, and another error.
    fn no_gpu() -> String {
        format!("WGPU 30.x rendering was required but {NO_GPU_ADAPTER} for the requested backends.")
    }

    #[test]
    fn a_backend_without_a_gpu_leads_to_the_next_attempt() {
        let windows = attempts(Platform::Windows);
        let mut tried = Vec::new();
        let picked = first_accepted(
            windows,
            |a| {
                tried.push(a);
                if a == Attempt::Wgpu(Gpu::Dx12) {
                    Err(no_gpu())
                } else {
                    Ok(())
                }
            },
            |e| e.contains(NO_GPU_ADAPTER),
        );
        assert_eq!(
            picked,
            Tried {
                accepted: Some(Attempt::Wgpu(Gpu::Vulkan)),
                refused: vec![(Attempt::Wgpu(Gpu::Dx12), no_gpu())]
            }
        );
        assert_eq!(
            tried,
            [Attempt::Wgpu(Gpu::Dx12), Attempt::Wgpu(Gpu::Vulkan)],
            "nothing after the accepted one"
        );
        let none = first_accepted(windows, |_| Err(no_gpu()), |e| e.contains(NO_GPU_ADAPTER));
        assert_eq!(none.accepted, None);
        assert_eq!(none.refused.len(), 3, "each attempt is reported");
    }

    #[test]
    fn any_other_error_ends_the_choice() {
        // A winit event loop that could not be made: winit allows one per process, so a next attempt could only
        // fail with "recreation attempt" and bury the cause.
        let mut tried = Vec::new();
        let none = first_accepted(
            attempts(Platform::Windows),
            |a| {
                tried.push(a);
                if a == Attempt::Wgpu(Gpu::Dx12) {
                    Err(no_gpu())
                } else {
                    Err("the event loop could not be made".to_owned())
                }
            },
            |e| e.contains(NO_GPU_ADAPTER),
        );
        assert_eq!(none.accepted, None);
        assert_eq!(
            tried,
            [Attempt::Wgpu(Gpu::Dx12), Attempt::Wgpu(Gpu::Vulkan)],
            "not the software renderer"
        );
        assert_eq!(none.refused.len(), 2);
    }

    #[test]
    fn when_nothing_draws_the_error_names_each_attempt() {
        let all = [
            (
                Attempt::Wgpu(Gpu::Dx12),
                PlatformError::Other("no GPU-backed adapter".to_owned()),
            ),
            (
                Attempt::Software,
                PlatformError::Other("no display".to_owned()),
            ),
        ];
        assert_eq!(
            no_renderer(&all),
            "no renderer could be set up (femtovg-wgpu on Dx12: no GPU-backed adapter; software: no display)"
        );
    }

    fn metal() -> Picked {
        Picked {
            renderer: FEMTOVG_WGPU.to_owned(),
            backend: Some("Metal".to_owned()),
            adapter: Some(Adapter {
                name: "Apple M3 Ultra".to_owned(),
                device_type: "IntegratedGpu".to_owned(),
                driver: String::new(),
            }),
        }
    }

    #[test]
    fn the_line_names_renderer_adapter_and_backend() {
        let first = Selected::InCode {
            attempt: Attempt::Wgpu(Gpu::Metal),
            refused: vec![],
        };
        assert_eq!(
            metal().line(&first),
            r#"zwt-app renderer=femtovg-wgpu adapter="Apple M3 Ultra" backend=Metal type=IntegratedGpu"#
        );
        let vulkan = Picked {
            renderer: FEMTOVG_WGPU.to_owned(),
            backend: Some("Vulkan".to_owned()),
            adapter: Some(Adapter {
                name: "NVIDIA GeForce RTX 4070".to_owned(),
                device_type: "DiscreteGpu".to_owned(),
                driver: "NVIDIA 566.36".to_owned(),
            }),
        };
        let after_dx12 = Selected::InCode {
            attempt: Attempt::Wgpu(Gpu::Vulkan),
            refused: vec![Attempt::Wgpu(Gpu::Dx12)],
        };
        assert_eq!(
            vulkan.line(&after_dx12),
            r#"zwt-app renderer=femtovg-wgpu adapter="NVIDIA GeForce RTX 4070" backend=Vulkan type=DiscreteGpu driver="NVIDIA 566.36" refused=Dx12"#
        );
        let software = Selected::InCode {
            attempt: Attempt::Software,
            refused: vec![Attempt::Wgpu(Gpu::Dx12), Attempt::Wgpu(Gpu::Vulkan)],
        };
        assert_eq!(
            Picked::software().line(&software),
            "zwt-app renderer=software adapter=none backend=none refused=Dx12,Vulkan"
        );
        assert_eq!(
            Picked::software().line(&Selected::SlintBackend("winit-software".to_owned())),
            r#"zwt-app renderer=software adapter=none backend=none SLINT_BACKEND="winit-software""#
        );
    }

    #[test]
    fn without_a_look_up_the_line_comes_from_the_choice() {
        let dx12 = Selected::InCode {
            attempt: Attempt::Wgpu(Gpu::Dx12),
            refused: vec![],
        };
        assert_eq!(
            Picked::chosen(&dx12).line(&dx12),
            "zwt-app renderer=femtovg-wgpu adapter=unknown backend=Dx12"
        );
        let software = Selected::InCode {
            attempt: Attempt::Software,
            refused: vec![Attempt::Wgpu(Gpu::Metal)],
        };
        assert_eq!(
            Picked::chosen(&software).line(&software),
            "zwt-app renderer=software adapter=none backend=none refused=Metal"
        );
    }

    #[test]
    fn a_name_with_quotes_stays_one_field() {
        let mut odd = metal();
        if let Some(a) = odd.adapter.as_mut() {
            a.name = r#"GPU "X" 1"#.to_owned();
        }
        assert!(
            odd.fields()
                .starts_with(r#"renderer=femtovg-wgpu adapter="GPU \"X\" 1" backend=Metal"#),
            "{}",
            odd.fields()
        );
    }

    #[test]
    fn the_line_is_said_once() {
        let selected = Selected::InCode {
            attempt: Attempt::Wgpu(Gpu::Metal),
            refused: vec![],
        };
        assert_eq!(picked(), None);
        record(metal(), &selected);
        record(Picked::software(), &selected);
        assert_eq!(picked(), Some(metal()), "the first one stays");
    }
}
