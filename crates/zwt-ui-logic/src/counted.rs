//! The core's pieces of counted time, with open segments ended as 1.x ends them.

use z_work_core::Ts;
use z_work_core::model::{LocalFile, Segment, Shared};
use z_work_core::pieces::{Piece, pieces};
use z_work_core::report::ExportSource;

/// Every piece of counted time of the live tasks (work, thinking, billable pauses), from `z_work_core::pieces`.
///
/// An open segment of this device runs to `now`; another device's open segment runs only to that device's last
/// heartbeat (or a pause asked of it), since nothing newer is known about it. That is the core's
/// `ExportSource::end_of`, the rule the 1.x snapshot uses for every screen.
#[must_use]
pub fn pieces_at(file: &LocalFile, now: Ts) -> Vec<Piece> {
    let source = ExportSource {
        data: Shared {
            pause_requests: file.data.pause_requests.clone(),
            ..Shared::default()
        },
        device_id: file.device_id.clone(),
        remote_devices: file.remote_devices.clone(),
        now,
        only: None,
    };
    pieces(&file.data, &|s: &Segment| source.end_of(s))
}

/// True for the piece a running timer of this device is still adding to: the open last piece of an open segment
/// recorded here. Another device's open piece ends at its last heartbeat and is drawn like a closed one, as in 1.x.
#[must_use]
pub fn is_live(piece: &Piece, file: &LocalFile) -> bool {
    piece.open && piece.device_id.as_deref() == Some(file.device_id.as_str())
}
