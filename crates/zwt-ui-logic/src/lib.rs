//! The rules zWorkTracker 1.x keeps only in TypeScript, ported to Rust for the Slint UI of 2.0 (PLAN risk R1).
//!
//! - [`format`](mod@format): duration and day texts, character for character as 1.x writes them.
//! - [`counted`]: the core's pieces of counted time as every screen ends them, and which piece is timed here.
//! - [`day`]: one day's pieces of time for the day axis, and the chips of the day picker.
//! - [`share`]: the Share of time donut: which level it shows, its rows, their colours, minutes and percentages.
//! - [`stripes`]: the diagonal stripes of a combination of tags, cut to its slice of the donut and to its swatch.
//!
//! Everything is a pure function of a [`z_work_core::model::LocalFile`] and its arguments: no clock, no I/O, no
//! global state. Each port names the TypeScript it follows (paths relative to `z-work-tracker/src`).
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod collate;
pub mod color;
pub mod counted;
pub mod day;
pub mod format;
pub mod share;
pub mod stripes;

mod js;
