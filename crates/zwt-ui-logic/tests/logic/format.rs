//! `components/format.test.ts` ("durations", "dates"), the legend-text cases of
//! `features/analysis/shareSlices.test.ts` ("the text of a legend row"), and the figures the other 1.x tests expect.

use chrono::NaiveDate;
use zwt_ui_logic::format::{
    day_label, duration, fmt_hm, fmt_hms, hours_minutes, percent_label, relative_day, short_day,
    ticker,
};

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

// ---- components/format.test.ts: durations

#[test]
fn formats_hours_minutes_and_seconds() {
    assert_eq!(duration(0), "0s");
    assert_eq!(duration(45), "45s");
    assert_eq!(duration(600), "10m");
    assert_eq!(duration(605), "10m 05s");
    assert_eq!(duration(3600), "1h 00m");
    assert_eq!(duration(3 * 3600 + 25 * 60 + 59), "3h 25m");
    // formatHMS, the clock of LiveTicker.
    assert_eq!(ticker(3725), "1:02:05");
    assert_eq!(ticker(-5), "0:00:00");
}

#[test]
fn ticker_is_the_session_clock_of_live_ticker() {
    // "0:00:00" before anything is timed, then h:mm:ss with unpadded hours.
    assert_eq!(ticker(0), "0:00:00");
    assert_eq!(ticker(59), "0:00:59");
    assert_eq!(ticker(60), "0:01:00");
    assert_eq!(ticker(3599), "0:59:59");
    assert_eq!(ticker(36_000), "10:00:00");
    assert_eq!(ticker(100 * 3600 + 7), "100:00:07");
}

// ---- components/format.test.ts: dates

#[test]
fn labels_days_without_intl() {
    assert_eq!(day_label(date("2026-03-12")), "Thu 12 Mar 2026");
    assert_eq!(
        relative_day(date("2026-03-12"), date("2026-03-12")),
        "Today, Thu 12 Mar 2026"
    );
    assert_eq!(
        relative_day(date("2026-03-11"), date("2026-03-12")),
        "Yesterday, Wed 11 Mar 2026"
    );
    assert_eq!(
        relative_day(date("2026-03-01"), date("2026-03-12")),
        "Sun 1 Mar 2026"
    );
    assert_eq!(short_day(date("2026-03-12")), "12 Mar");
}

// ---- features/analysis/shareSlices.test.ts: the text of a legend row

#[test]
fn writes_hours_always_and_minutes_with_two_digits() {
    assert_eq!(hours_minutes(45), "0h 45m");
    assert_eq!(hours_minutes(5), "0h 05m");
    assert_eq!(hours_minutes(60), "1h 00m");
    assert_eq!(hours_minutes(1010), "16h 50m");
    assert_eq!(hours_minutes(123 * 60 + 5), "123h 05m");
}

#[test]
fn writes_less_than_one_percent_for_a_row_with_time_whose_share_rounds_to_nothing() {
    // `percentText` without its brackets (the legend's row adds them: ShareRow::pct).
    assert_eq!(percent_label(0, 0), "0%");
    assert_eq!(percent_label(1, 1), "1%");
    assert_eq!(percent_label(0, 20), "<1%");
    assert_eq!(percent_label(0, 1), "<1%");
    assert_eq!(percent_label(47, 60_600), "47%");
}

// ---- the contract's figures, with the values of the 1.x tests

#[test]
fn analysis_figures_of_tooltips() {
    assert_eq!(fmt_hm(7500), "2h 05m");
    assert_eq!(fmt_hm(2700), "45m");
    assert_eq!(fmt_hm(0), "0m");
    assert_eq!(fmt_hms(7513), "2:05:13");
    assert_eq!(fmt_hms(3600), "1:00:00");
}
