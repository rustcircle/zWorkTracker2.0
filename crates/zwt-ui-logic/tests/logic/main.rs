//! The 1.x TypeScript tests of the ported rules, ported to Rust, plus tests of the store-reading parts.
//!
//! One module per TypeScript file (`z-work-tracker/src/...`); each test keeps the name of the `it` it ports. Tests
//! that read a store build it in memory with [`fixtures::Store`]: the core's own test kit is `cfg(test)` only.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod day;
mod fixtures;
mod format;
mod golden;
mod share_card;
mod share_levels;
mod share_rows;
mod share_slices;
mod stripes;
