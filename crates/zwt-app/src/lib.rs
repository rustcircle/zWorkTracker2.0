//! zWorkTracker 2.0: the Slint components, generated at build time from `ui/*.slint`, and the UI thread's wiring to
//! zwt-bridge.
//!
//! The binary (`src/main.rs`) and the UI tests (`tests/*.rs`) both build the app from here: [`ui::Ui`] owns the models,
//! the hub and the 1 s clock of one [`AppWindow`] and needs no running event loop. [`exit`] makes every way of
//! quitting shut the worker down first, [`launch`] words why the app did not start or could not go on, and
//! [`renderer`] picks what draws the window (FemtoVG on wgpu) and says on stderr what did.

/// The Rust that `slint-build` generates from `ui/app.slint`.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "Slint's generated code unwraps and indexes its own invariants; our lints are for our code"
)]
mod generated {
    slint::include_modules!();
}

pub use generated::*;

pub mod clock;
pub mod convert;
pub mod exit;
pub mod launch;
pub mod measure;
pub mod renderer;
pub mod ui;

/// Shows `app` in the theme with this 1.x id ("light", "solarizedLight", "solarizedDark", "dark"); an unknown id shows
/// Light, as in 1.x.
pub fn set_theme(app: &AppWindow, id: &str) {
    use slint::ComponentHandle as _;
    app.global::<Theme>().set_current(id.into());
}
