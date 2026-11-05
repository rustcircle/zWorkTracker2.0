//! `day_pieces` and `day_chips`: a day's pieces cut at the zone's midnights (PARITY 125, 127), running pieces left
//! open, project colours, and the day picker's chips, oldest first with closed time only. `day_increment` and
//! `sub_grid`: the day axis' sub-grid (2.0, owner 2026-11-04).

use chrono::NaiveDate;
use chrono_tz::Tz;
use z_work_core::model::GapDecision;
use zwt_ui_logic::day::{
    DayChip, DayPiece, MIN_LINE_GAP_PX, PieceKind, ProjectSecs, SubGrid, UNKNOWN_PROJECT_COLOR,
    day_chips, day_increment, day_pieces, first_day, sub_grid,
};

use crate::fixtures::{HERE, Store, at};

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

const TOKYO: Tz = chrono_tz::Asia::Tokyo;
const LISBON: Tz = chrono_tz::Europe::Lisbon;
const UTC: Tz = chrono_tz::UTC;

fn spans(pieces: &[DayPiece]) -> Vec<(i64, Option<i64>)> {
    pieces.iter().map(|p| (p.start, p.end)).collect()
}

#[test]
fn cuts_a_piece_at_midnight_in_a_zone_that_is_not_utc() {
    // 14:00 to 16:00 UTC is 23:00 to 01:00 in Tokyo: one hour on the 10th, one on the 11th.
    let s = Store::new().abc().task("a1", "p-a").seg(
        "a1",
        "2026-03-10T14:00:00Z",
        "2026-03-10T16:00:00Z",
    );
    let tenth = day_pieces(&s.file, date("2026-03-10"), TOKYO);
    let eleventh = day_pieces(&s.file, date("2026-03-11"), TOKYO);
    assert_eq!(
        spans(&tenth),
        [(at("2026-03-10T14:00:00Z"), Some(at("2026-03-10T15:00:00Z")))]
    );
    assert_eq!(
        spans(&eleventh),
        [(at("2026-03-10T15:00:00Z"), Some(at("2026-03-10T16:00:00Z")))]
    );
    // The same piece on both days, so each day's keyed list knows it.
    assert_eq!(tenth[0].id, eleventh[0].id);
    // In UTC the whole piece is on the 10th.
    assert_eq!(
        spans(&day_pieces(&s.file, date("2026-03-10"), UTC)),
        [(at("2026-03-10T14:00:00Z"), Some(at("2026-03-10T16:00:00Z")))]
    );
    assert!(day_pieces(&s.file, date("2026-03-11"), UTC).is_empty());
}

#[test]
fn a_day_across_a_clock_change_has_its_real_length() {
    // Lisbon moves to summer time on 29 March 2026: that day runs from 00:00 UTC to 23:00 UTC.
    let s = Store::new().abc().task("a1", "p-a").seg(
        "a1",
        "2026-03-29T22:30:00Z",
        "2026-03-30T00:30:00Z",
    );
    let day = day_pieces(&s.file, date("2026-03-29"), LISBON);
    assert_eq!(
        spans(&day),
        [(at("2026-03-29T22:30:00Z"), Some(at("2026-03-29T23:00:00Z")))]
    );
    let next = day_pieces(&s.file, date("2026-03-30"), LISBON);
    assert_eq!(
        spans(&next),
        [(at("2026-03-29T23:00:00Z"), Some(at("2026-03-30T00:30:00Z")))]
    );
}

#[test]
fn leaves_a_piece_running_on_this_device_open() {
    let s = Store::new()
        .abc()
        .task("a1", "p-a")
        .open("a1", "2026-03-12T09:00:00Z");
    let today = day_pieces(&s.file, date("2026-03-12"), UTC);
    assert_eq!(spans(&today), [(at("2026-03-12T09:00:00Z"), None)]);
    assert!(day_pieces(&s.file, date("2026-03-11"), UTC).is_empty());
}

#[test]
fn a_piece_running_since_yesterday_starts_at_midnight_today_and_stays_open_on_both_days() {
    let s = Store::new()
        .abc()
        .task("a1", "p-a")
        .open("a1", "2026-03-11T22:00:00Z");
    assert_eq!(
        spans(&day_pieces(&s.file, date("2026-03-11"), UTC)),
        [(at("2026-03-11T22:00:00Z"), None)]
    );
    assert_eq!(
        spans(&day_pieces(&s.file, date("2026-03-12"), UTC)),
        [(at("2026-03-12T00:00:00Z"), None)]
    );
}

#[test]
fn a_running_segment_with_time_away_keeps_its_closed_pieces_and_only_the_last_one_open() {
    let s = Store::new()
        .abc()
        .task("a1", "p-a")
        .open("a1", "2026-03-12T09:00:00Z");
    let seg = s.last_segment();
    let s = s.away(
        &seg,
        "2026-03-12T09:30:00Z",
        "2026-03-12T10:00:00Z",
        GapDecision::Drop,
    );
    let today = day_pieces(&s.file, date("2026-03-12"), UTC);
    assert_eq!(
        spans(&today),
        [
            (at("2026-03-12T09:00:00Z"), Some(at("2026-03-12T09:30:00Z"))),
            (at("2026-03-12T10:00:00Z"), None)
        ]
    );
}

#[test]
fn another_devices_running_piece_ends_at_its_last_heartbeat() {
    let s = Store::new().abc().task("a1", "p-a").open_elsewhere(
        "a1",
        "2026-03-12T09:00:00Z",
        "dev-2",
        "2026-03-12T09:40:00Z",
    );
    assert_eq!(
        spans(&day_pieces(&s.file, date("2026-03-12"), UTC)),
        [(at("2026-03-12T09:00:00Z"), Some(at("2026-03-12T09:40:00Z")))]
    );
}

#[test]
fn colours_a_piece_with_its_projects_colour() {
    let s = Store::new()
        .abc()
        .task("a1", "p-a")
        .task("b1", "p-b")
        .task("lost", "p-gone")
        .seg("a1", "2026-03-10T09:00:00Z", "2026-03-10T10:00:00Z")
        .seg("b1", "2026-03-10T10:00:00Z", "2026-03-10T11:00:00Z")
        .seg("lost", "2026-03-10T11:00:00Z", "2026-03-10T12:00:00Z");
    let day = day_pieces(&s.file, date("2026-03-10"), UTC);
    let got: Vec<(&str, &str, &str)> = day
        .iter()
        .map(|p| (p.task_id.as_str(), p.project_id.as_str(), p.color.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("a1", "p-a", "#2563eb"),
            ("b1", "p-b", "#dc2626"),
            ("lost", "p-gone", UNKNOWN_PROJECT_COLOR)
        ]
    );
}

#[test]
fn draws_thinking_and_billable_pauses_with_their_kind_and_leaves_out_dropped_time() {
    let s = Store::new().abc().task("a1", "p-a").seg(
        "a1",
        "2026-03-10T09:00:00Z",
        "2026-03-10T11:00:00Z",
    );
    let seg = s.last_segment();
    let s = s
        .away(
            &seg,
            "2026-03-10T09:30:00Z",
            "2026-03-10T10:00:00Z",
            GapDecision::Thinking,
        )
        .away(
            &seg,
            "2026-03-10T10:00:00Z",
            "2026-03-10T10:30:00Z",
            GapDecision::Drop,
        )
        .pause("a1", "2026-03-10T12:00:00Z", "2026-03-10T12:15:00Z");
    let day = day_pieces(&s.file, date("2026-03-10"), UTC);
    let got: Vec<(PieceKind, i64, Option<i64>)> =
        day.iter().map(|p| (p.kind, p.start, p.end)).collect();
    assert_eq!(
        got,
        [
            (
                PieceKind::Work,
                at("2026-03-10T09:00:00Z"),
                Some(at("2026-03-10T09:30:00Z"))
            ),
            (
                PieceKind::Thinking,
                at("2026-03-10T09:30:00Z"),
                Some(at("2026-03-10T10:00:00Z"))
            ),
            (
                PieceKind::Work,
                at("2026-03-10T10:30:00Z"),
                Some(at("2026-03-10T11:00:00Z"))
            ),
            (
                PieceKind::BillablePause,
                at("2026-03-10T12:00:00Z"),
                Some(at("2026-03-10T12:15:00Z"))
            ),
        ]
    );
}

#[test]
fn stacks_overlapping_pieces_in_lanes() {
    let s = Store::new()
        .abc()
        .task("a1", "p-a")
        .task("b1", "p-b")
        .seg("a1", "2026-03-10T09:00:00Z", "2026-03-10T11:00:00Z")
        .seg("b1", "2026-03-10T10:00:00Z", "2026-03-10T12:00:00Z")
        .seg("a1", "2026-03-10T11:00:00Z", "2026-03-10T11:30:00Z");
    let day = day_pieces(&s.file, date("2026-03-10"), UTC);
    let lanes: Vec<(i64, u32)> = day.iter().map(|p| (p.start, p.lane)).collect();
    assert_eq!(
        lanes,
        [
            (at("2026-03-10T09:00:00Z"), 0),
            (at("2026-03-10T10:00:00Z"), 1),
            (at("2026-03-10T11:00:00Z"), 0)
        ]
    );
}

#[test]
fn leaves_out_pieces_of_no_length_and_of_tasks_deleted_with_their_time() {
    let s = Store::new()
        .abc()
        .task("a1", "p-a")
        .task("gone", "p-a")
        .seg("a1", "2026-03-10T09:00:00Z", "2026-03-10T09:00:00Z")
        .seg("gone", "2026-03-10T10:00:00Z", "2026-03-10T11:00:00Z")
        .discard("gone");
    assert!(day_pieces(&s.file, date("2026-03-10"), UTC).is_empty());
}

// ---- day_chips

#[test]
fn lists_the_days_oldest_first_with_closed_time_only() {
    let s = Store::new()
        .abc()
        .task("a1", "p-a")
        .task("b1", "p-b")
        .seg("a1", "2026-03-10T09:00:00Z", "2026-03-10T10:00:00Z")
        .seg("b1", "2026-03-10T11:00:00Z", "2026-03-10T13:00:00Z")
        .seg("a1", "2026-03-11T09:00:00Z", "2026-03-11T09:30:00Z")
        .seg("b1", "2026-03-12T08:00:00Z", "2026-03-12T08:15:00Z")
        .open("a1", "2026-03-12T09:00:00Z")
        .pause("b1", "2026-03-11T12:00:00Z", "2026-03-11T13:00:00Z");
    let chips = day_chips(&s.file, date("2026-03-12"), 4, UTC);
    let ps = |id: &str, color: &str, secs: i64| ProjectSecs {
        project_id: id.into(),
        color: color.into(),
        secs,
    };
    assert_eq!(
        chips,
        [
            DayChip {
                date: date("2026-03-09"),
                closed_secs: 0,
                by_project: Vec::new()
            },
            DayChip {
                date: date("2026-03-10"),
                closed_secs: 3 * 3600,
                by_project: vec![ps("p-b", "#dc2626", 7200), ps("p-a", "#2563eb", 3600)],
            },
            // The billable pause is not worked time.
            DayChip {
                date: date("2026-03-11"),
                closed_secs: 1800,
                by_project: vec![ps("p-a", "#2563eb", 1800)]
            },
            // The running piece is not in today's chip; the closed one is.
            DayChip {
                date: date("2026-03-12"),
                closed_secs: 900,
                by_project: vec![ps("p-b", "#dc2626", 900)]
            },
        ]
    );
}

#[test]
fn chips_cut_a_piece_at_the_zones_midnight() {
    let s = Store::new().abc().task("a1", "p-a").seg(
        "a1",
        "2026-03-10T14:00:00Z",
        "2026-03-10T16:00:00Z",
    );
    let chips = day_chips(&s.file, date("2026-03-11"), 2, TOKYO);
    let got: Vec<(NaiveDate, i64)> = chips.iter().map(|c| (c.date, c.closed_secs)).collect();
    assert_eq!(
        got,
        [(date("2026-03-10"), 3600), (date("2026-03-11"), 3600)]
    );
}

#[test]
fn chips_count_another_devices_running_piece_up_to_its_heartbeat() {
    let s = Store::new().abc().task("a1", "p-a").open_elsewhere(
        "a1",
        "2026-03-12T09:00:00Z",
        "dev-2",
        "2026-03-12T09:40:00Z",
    );
    assert_eq!(
        day_chips(&s.file, date("2026-03-12"), 1, UTC)[0].closed_secs,
        2400
    );
    assert_ne!(HERE, "dev-2");
}

/// The owner's rule for 2.0 (2026-11-04): the Timeline runs from the past to the future. The picker's chips come
/// oldest first, one per day, ending with the day asked for; a day's pieces come as they sit on its axis, by start.
#[test]
fn the_days_run_from_the_past_to_the_future() {
    let s = Store::new()
        .abc()
        .task("a1", "p-a")
        .task("b1", "p-b")
        .seg("b1", "2026-03-12T14:00:00Z", "2026-03-12T15:00:00Z")
        .seg("a1", "2026-03-12T08:00:00Z", "2026-03-12T09:00:00Z")
        .seg("a1", "2026-03-10T09:00:00Z", "2026-03-10T10:00:00Z")
        .open("a1", "2026-03-12T16:00:00Z");
    let dates: Vec<NaiveDate> = day_chips(&s.file, date("2026-03-12"), 5, UTC)
        .iter()
        .map(|c| c.date)
        .collect();
    assert_eq!(
        dates,
        [
            "2026-03-08",
            "2026-03-09",
            "2026-03-10",
            "2026-03-11",
            "2026-03-12"
        ]
        .map(date),
        "oldest first, the day asked for last"
    );
    let starts: Vec<i64> = day_pieces(&s.file, date("2026-03-12"), UTC)
        .iter()
        .map(|p| p.start)
        .collect();
    assert_eq!(
        starts,
        [
            at("2026-03-12T08:00:00Z"),
            at("2026-03-12T14:00:00Z"),
            at("2026-03-12T16:00:00Z")
        ],
        "by start, the earliest first"
    );
}

// ---- first_day: where the day picker stops paging back

#[test]
fn the_first_day_is_the_day_the_earliest_block_starts_on_in_the_zone() {
    // 16:00 UTC on the 10th is 01:00 on the 11th in Tokyo.
    let s = Store::new()
        .abc()
        .task("a1", "p-a")
        .seg("a1", "2026-03-12T09:00:00Z", "2026-03-12T10:00:00Z")
        .seg("a1", "2026-03-10T16:00:00Z", "2026-03-10T17:00:00Z");
    assert_eq!(first_day(&s.file, UTC), Some(date("2026-03-10")));
    assert_eq!(first_day(&s.file, TOKYO), Some(date("2026-03-11")));
    assert_eq!(
        first_day(&Store::new().abc().file, UTC),
        None,
        "no time, no first day"
    );
}

#[test]
fn the_first_day_counts_what_the_axis_draws_and_nothing_else() {
    let base = || {
        Store::new()
            .abc()
            .task("a1", "p-a")
            .task("gone", "p-b")
            .seg("a1", "2026-03-12T09:00:00Z", "2026-03-12T10:00:00Z")
    };
    // A billable pause is drawn (as an outline), so it counts.
    let paused = base().pause("a1", "2026-03-09T09:00:00Z", "2026-03-09T10:00:00Z");
    assert_eq!(first_day(&paused.file, UTC), Some(date("2026-03-09")));
    // A piece running on this device counts from its start.
    let running = base().open("a1", "2026-03-08T22:00:00Z");
    assert_eq!(first_day(&running.file, UTC), Some(date("2026-03-08")));
    // Not drawn, so not counted: a piece of no length, and the time of a task deleted with its time.
    let undrawn = base()
        .seg("a1", "2026-03-05T09:00:00Z", "2026-03-05T09:00:00Z")
        .seg("gone", "2026-03-06T09:00:00Z", "2026-03-06T10:00:00Z")
        .discard("gone");
    assert_eq!(first_day(&undrawn.file, UTC), Some(date("2026-03-12")));
    assert!(day_pieces(&undrawn.file, date("2026-03-05"), UTC).is_empty());
    assert!(day_pieces(&undrawn.file, date("2026-03-06"), UTC).is_empty());
}

#[test]
fn no_days_is_no_chips() {
    let s = Store::new().abc();
    assert!(day_chips(&s.file, date("2026-03-12"), 0, UTC).is_empty());
}

#[test]
fn chips_are_the_same_whatever_the_clock_says() {
    // No `now` goes in: a running timer cannot change a chip, only data can.
    let s = Store::new()
        .abc()
        .task("a1", "p-a")
        .open("a1", "2026-03-12T09:00:00Z");
    let chips = day_chips(&s.file, date("2026-03-12"), 1, UTC);
    assert_eq!(chips[0].closed_secs, 0);
    assert_eq!(
        chips,
        day_chips(&s.file.clone(), date("2026-03-12"), 1, UTC)
    );
}

/// The day axis' sub-grid splits the hours by the increment of 1.x's Settings > Rounding: the global rule in force on
/// the day (the latest effective date on or before it), read as the Settings page reads it (`ruleFor(rounding, '',
/// day)`); a project's own rule leaves the axis alone; 15 minutes, the core's default, with no rule or rounding off.
#[test]
fn the_axis_increment_is_the_global_rounding_rule_in_force_on_the_day() {
    let none = Store::new().abc();
    assert_eq!(day_increment(&none.file, date("2026-03-12")), 15, "no rule");
    let s = Store::new()
        .abc()
        .rounding("r1", None, "2026-03-01", 30)
        .rounding("r2", None, "2026-03-10", 6)
        .rounding("r3", Some("p-a"), "2026-03-05", 10)
        .rounding("r4", None, "2026-03-20", 0);
    assert_eq!(
        day_increment(&s.file, date("2026-02-28")),
        15,
        "before every rule"
    );
    assert_eq!(day_increment(&s.file, date("2026-03-01")), 30);
    assert_eq!(
        day_increment(&s.file, date("2026-03-09")),
        30,
        "not the project's 10"
    );
    assert_eq!(
        day_increment(&s.file, date("2026-03-10")),
        6,
        "the later rule from its day on"
    );
    assert_eq!(
        day_increment(&s.file, date("2026-03-20")),
        15,
        "rounding off: the default"
    );
    let mut gone = s.clone();
    gone.file.data.rounding.get_mut("r2").unwrap().deleted = true;
    assert_eq!(
        day_increment(&gone.file, date("2026-03-12")),
        30,
        "a deleted rule"
    );
}

fn grid(step: u32, per_hour: u32) -> SubGrid {
    SubGrid { step, per_hour }
}

/// 0.5625 px a minute: the day axis of a 1360 px window (810 px for 24 hours).
const WIDE: f64 = 0.5625;

#[test]
fn the_sub_grid_marks_each_increment_that_splits_the_hour_while_its_lines_stay_apart() {
    // The owner's example: 15 minutes, lines at :15, :30 and :45.
    assert_eq!(sub_grid(15, WIDE), grid(15, 3));
    assert_eq!(sub_grid(30, WIDE), grid(30, 1));
    assert_eq!(sub_grid(20, WIDE), grid(20, 2));
    assert_eq!(sub_grid(10, WIDE), grid(10, 5));
    assert_eq!(sub_grid(6, WIDE), grid(6, 9), "3.4 px apart");
    // Too close at this width: the least multiple that still splits the hour.
    assert_eq!(sub_grid(5, WIDE), grid(10, 5), "2.8 px: every 10 minutes");
    assert_eq!(
        sub_grid(4, WIDE),
        grid(12, 4),
        "8 does not split the hour: 12"
    );
    assert_eq!(sub_grid(1, WIDE), grid(6, 9));
    // With room, every minute.
    assert_eq!(sub_grid(1, 3.0), grid(1, 59));
    assert_eq!(sub_grid(1, 2.99), grid(2, 29));
    // So narrow that even 30 minutes would be closer than 3 px.
    assert_eq!(sub_grid(15, 0.09), SubGrid::default());
    assert_eq!(sub_grid(30, 0.1), grid(30, 1));
}

#[test]
fn an_increment_that_does_not_split_the_hour_marks_its_multiples_into_each_hour() {
    assert_eq!(sub_grid(45, WIDE), grid(45, 1), ":45");
    assert_eq!(sub_grid(25, WIDE), grid(25, 2), ":25 and :50");
    assert_eq!(
        sub_grid(8, WIDE),
        grid(8, 6),
        ":56 is 2.3 px from the hour: :08 to :48"
    );
    assert_eq!(sub_grid(7, WIDE), grid(7, 7), ":07 to :49");
    assert_eq!(
        sub_grid(55, WIDE),
        SubGrid::default(),
        ":55 hugs the next hour"
    );
    // At 960 px (0.2847 px a minute), 7 becomes 14: :14, :28, :42.
    assert_eq!(sub_grid(7, 0.2847), grid(14, 3));
}

#[test]
fn no_sub_grid_for_an_hour_or_more_nor_before_the_axis_has_a_width() {
    for minutes in [0, 60, 90, 120, 1440] {
        assert_eq!(sub_grid(minutes, WIDE), SubGrid::default(), "{minutes}");
    }
    for px in [0.0, -1.0, f64::NAN] {
        assert_eq!(sub_grid(15, px), SubGrid::default(), "{px}");
    }
}

/// Every increment the core allows (1 to 1440 minutes) at widths from a sliver to a wide screen: the lines of an hour,
/// with the hour lines on either side, are at least 3 px apart; each is a multiple of the increment; the step splits
/// the hour whenever the increment does; and an increment with room is drawn as it is.
#[test]
fn the_sub_grid_keeps_its_promises_for_every_increment_and_width() {
    for minutes in 1..=1440_u32 {
        for tenth in 1..=60 {
            let px = f64::from(tenth) / 10.0;
            let g = sub_grid(minutes, px);
            if g == SubGrid::default() {
                assert!(
                    minutes >= 60
                        || f64::from(minutes) * px < MIN_LINE_GAP_PX
                        || !60_u32.is_multiple_of(minutes),
                    "{minutes} min at {px} px: none, though it fits"
                );
                continue;
            }
            assert!(
                minutes < 60 && g.step.is_multiple_of(minutes),
                "{minutes} at {px}: {g:?}"
            );
            let marks: Vec<u32> = (0..=g.per_hour + 1)
                .map(|k| if k > g.per_hour { 60 } else { k * g.step })
                .collect();
            assert!(
                marks
                    .windows(2)
                    .all(|w| f64::from(w[1] - w[0]) * px >= MIN_LINE_GAP_PX),
                "{minutes} at {px}: {g:?}"
            );
            assert!(g.per_hour * g.step < 60, "{minutes} at {px}: {g:?}");
            if 60_u32.is_multiple_of(minutes) {
                assert!(60_u32.is_multiple_of(g.step), "{minutes} at {px}: {g:?}");
                assert_eq!(g.per_hour, 60 / g.step - 1, "{minutes} at {px}: {g:?}");
                if f64::from(minutes) * px >= MIN_LINE_GAP_PX {
                    assert_eq!(
                        g.step, minutes,
                        "{minutes} at {px}: room for every increment"
                    );
                }
            }
        }
    }
}
