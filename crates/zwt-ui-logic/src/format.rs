//! Duration and day texts, character for character as 1.x writes them.
//!
//! Ports of `features/analysis/shareSlices.ts` (`hoursMinutes`, `percentText`; a legend row's figure is
//! [`crate::share::ShareRow::text`]), `features/analysis/format.ts` (`fmtHM`, `fmtHMS`) and `components/format.ts`
//! (`formatHMS`, `formatDuration`, `dayLabel`, `shortDay`, `relativeDay`).

use chrono::{Datelike, Days, NaiveDate};

use crate::js::{pad2, round_minutes};

/// `hoursMinutes`: hours always written, minutes always two digits: "16h 50m", "0h 45m", "123h 05m".
#[must_use]
pub fn hours_minutes(minutes: i64) -> String {
    format!("{}h {}m", minutes.div_euclid(60), pad2(minutes % 60))
}

/// The running elapsed clock as 1.x's `LiveTicker` shows it (`formatHMS` of the session's seconds): "1:02:05",
/// and "0:00:00" for nothing or less.
#[must_use]
pub fn ticker(secs: i64) -> String {
    let s = secs.max(0);
    format!("{}:{}:{}", s / 3600, pad2((s % 3600) / 60), pad2(s % 60))
}

/// The percentage of a row with `secs` of time: "47%", or "<1%" for a row with time whose share rounds to 0
/// (`percentText` without its brackets, which [`crate::share::ShareRow::pct`] adds).
#[must_use]
pub fn percent_label(percent: i64, secs: i64) -> String {
    if secs > 0 && percent < 1 {
        "<1%".to_owned()
    } else {
        format!("{percent}%")
    }
}

/// `formatDuration`: "2h 05m", "45m 10s", "10m", "30s". Hours drop the seconds.
#[must_use]
pub fn duration(secs: i64) -> String {
    let s = secs.max(0);
    let (h, m, r) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}h {}m", pad2(m))
    } else if m > 0 {
        if r > 0 {
            format!("{m}m {}s", pad2(r))
        } else {
            format!("{m}m")
        }
    } else {
        format!("{r}s")
    }
}

/// `fmtHM` of the Analysis tab: "2h 05m", "45m", "0m". Rounded to the nearest minute.
#[must_use]
pub fn fmt_hm(secs: i64) -> String {
    let m = round_minutes(secs);
    let h = m.div_euclid(60);
    if h > 0 {
        format!("{h}h {}m", pad2(m % 60))
    } else {
        format!("{m}m")
    }
}

/// `fmtHMS` of the Analysis tab, the exact value of a tooltip ("To the second: 2:05:13").
#[must_use]
pub fn fmt_hms(secs: i64) -> String {
    format!(
        "{}:{}:{}",
        secs.div_euclid(3600),
        pad2((secs % 3600).div_euclid(60)),
        pad2(secs % 60)
    )
}

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The weekday `dayLabel` starts with: "Thu".
#[must_use]
pub fn weekday_short(day: NaiveDate) -> &'static str {
    let i = day.weekday().num_days_from_monday() as usize;
    WEEKDAYS.get(i).copied().unwrap_or_default()
}

fn month_short(day: NaiveDate) -> &'static str {
    MONTHS
        .get(day.month0() as usize)
        .copied()
        .unwrap_or_default()
}

/// `dayLabel`: "Thu 12 Mar 2026".
#[must_use]
pub fn day_label(day: NaiveDate) -> String {
    format!(
        "{} {} {} {}",
        weekday_short(day),
        day.day(),
        month_short(day),
        day.year()
    )
}

/// `shortDay`: "12 Mar".
#[must_use]
pub fn short_day(day: NaiveDate) -> String {
    format!("{} {}", day.day(), month_short(day))
}

/// `relativeDay`: "Today, Thu 12 Mar 2026", "Yesterday, Wed 11 Mar 2026", else the plain [`day_label`].
#[must_use]
pub fn relative_day(day: NaiveDate, today: NaiveDate) -> String {
    if day == today {
        format!("Today, {}", day_label(day))
    } else if today.checked_sub_days(Days::new(1)) == Some(day) {
        format!("Yesterday, {}", day_label(day))
    } else {
        day_label(day)
    }
}
