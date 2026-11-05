//! UI-independent adapter between `z-work-core` and the Slint UI of zWorkTracker 2.0 (PLAN.md section 5).
//!
//! - [`datadir`]: the data folder of milestone 1 (`--sample <dir>` or `ZWT_DATA_DIR`), never the real one.
//! - [`sample`]: a deterministic sample store, seeded through the core, with one task running once open.
//! - [`worker`]: the thread that owns `Service`; commands in, [`worker::Update`]s out, no polling.
//! - [`topics`]: the topic bitset and the diff of two files that sets it.
//! - [`views`]: the rows and values each topic shows.
//! - [`hub`]: the UI-thread view models that write only changed rows and values to the UI's models.
//!
//! Nothing here depends on Slint: the UI implements [`hub::RowSink`] and [`hub::CellSink`] over its models and
//! forwards each update from the worker thread to the UI thread. See `nodes/B1/api.md` of the milestone-1 run.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod datadir;
pub mod hub;
pub mod sample;
pub mod topics;
pub mod views;
pub mod worker;

pub use z_work_core::{Clock, ManualClock, SystemClock, Ts};
pub use zwt_ui_logic::day::{DayChip, DayPiece, PieceKind, ProjectSecs};
pub use zwt_ui_logic::share::{ShareLevel, ShareRow};
pub use zwt_ui_logic::{format, share};

/// Whole seconds from `start` to the clock's now; 0 when the clock reads earlier than `start`.
#[must_use]
pub fn elapsed_secs(clock: &dyn Clock, start: Ts) -> i64 {
    clock.now().secs().saturating_sub(start.secs()).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_follows_the_clock_and_never_goes_negative() {
        let clock = ManualClock::at("2026-11-04T08:00:00Z");
        let start = clock.now();
        assert_eq!(
            start.secs(),
            1_793_779_200,
            "the clock parsed the start time"
        );
        clock.advance(90);
        assert_eq!(elapsed_secs(&clock, start), 90);
        clock.set("2026-11-04T07:59:00Z");
        assert_eq!(elapsed_secs(&clock, start), 0);
    }
}
