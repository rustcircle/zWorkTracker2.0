//! The forgotten-timer reminder: a long uninterrupted run, or running past the end of a shift.

use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use serde::Serialize;

use crate::billing::{day_of, iso_weekday};
use crate::machine::stamp_order;
use crate::model::{Shared, ShiftVersion};
use crate::time::Ts;
use crate::validate::parse_zone;

pub const DEFAULT_LONG_RUN_MINUTES: i64 = 240;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Reminder {
    LongRun {
        #[serde(rename = "taskId")]
        task_id: String,
        since: Ts,
    },
    PastShift {
        #[serde(rename = "taskId")]
        task_id: String,
        shift: String,
        ended: Ts,
    },
}

fn version_on<'a>(versions: &[&'a ShiftVersion], day: &str) -> Option<&'a ShiftVersion> {
    versions
        .iter()
        .copied()
        .filter(|v| !v.deleted && v.effective_from.as_str() <= day)
        .max_by(|a, b| {
            a.effective_from
                .cmp(&b.effective_from)
                .then_with(|| stamp_order(&a.stamp, &a.id, &b.stamp, &b.id))
        })
        .filter(|v| !v.retired)
}

fn minutes(hhmm: &str) -> Option<i64> {
    let (h, m) = hhmm.split_once(':')?;
    Some(h.parse::<i64>().ok()? * 60 + m.parse::<i64>().ok()?)
}

/// The end of a shift that started on `day`, as an instant.
fn shift_end(v: &ShiftVersion, day: NaiveDate) -> Option<Ts> {
    let tz = parse_zone(&v.zone).ok()?;
    let (start, end) = (minutes(&v.start)?, minutes(&v.end)?);
    let end_day = if end <= start {
        day.checked_add_signed(Duration::days(1))?
    } else {
        day
    };
    let time = NaiveTime::from_hms_opt(
        u32::try_from(end / 60).ok()?,
        u32::try_from(end % 60).ok()?,
        0,
    )?;
    let local = NaiveDateTime::new(end_day, time);
    let at = tz.from_local_datetime(&local).earliest()?;
    Some(Ts(at.timestamp()))
}

/// The latest shift this segment has run past, if any.
fn past_shift(shared: &Shared, seg_start: Ts, now: Ts) -> Option<(String, Ts)> {
    let mut groups: std::collections::BTreeMap<&str, Vec<&ShiftVersion>> = Default::default();
    for v in shared.shifts.values() {
        groups.entry(v.shift_id.as_str()).or_default().push(v);
    }
    let mut best: Option<(String, Ts)> = None;
    for versions in groups.values() {
        let zone = versions.first().and_then(|v| parse_zone(&v.zone).ok());
        let Some(zone) = zone else { continue };
        let today = day_of(now, zone);
        for back in 0..=1 {
            let Some(day) = today.checked_sub_signed(Duration::days(back)) else {
                continue;
            };
            let key = day.format("%Y-%m-%d").to_string();
            let Some(v) = version_on(versions, &key) else {
                continue;
            };
            let weekday = u8::try_from(iso_weekday(day)).unwrap_or(0);
            if !v.weekdays.contains(&weekday) {
                continue;
            }
            let Some(end) = shift_end(v, day) else {
                continue;
            };
            if seg_start < end && end < now && best.as_ref().is_none_or(|b| b.1 < end) {
                best = Some((v.name.clone(), end));
            }
        }
    }
    best
}

/// `long_run_minutes` of 0 or less turns the long-run reminder off.
#[must_use]
pub fn due(
    shared: &Shared,
    device: &str,
    now: Ts,
    long_run_minutes: i64,
    remind_past_shift: bool,
) -> Option<Reminder> {
    let seg = shared
        .segments
        .values()
        .find(|s| s.end.is_none() && s.device_id == device)?;
    if remind_past_shift && let Some((shift, ended)) = past_shift(shared, seg.start, now) {
        return Some(Reminder::PastShift {
            task_id: seg.task_id.clone(),
            shift,
            ended,
        });
    }
    if long_run_minutes > 0 && now.secs() - seg.start.secs() >= long_run_minutes * 60 {
        return Some(Reminder::LongRun {
            task_id: seg.task_id.clone(),
            since: seg.start,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::tests::{ctx, project_input, task_input};
    use crate::model::Stamp;

    fn running(at: &str) -> (Shared, String) {
        let mut s = Shared::default();
        let c = ctx(at);
        let p = s.create_project(&c, &project_input("A")).unwrap();
        let t = s.create_task(&c, &task_input("T", &p)).unwrap();
        s.start(&c, &t).unwrap();
        (s, c.device.to_owned())
    }
    fn at(x: &str) -> Ts {
        Ts::parse(x).unwrap()
    }
    fn shift(start: &str, end: &str) -> ShiftVersion {
        ShiftVersion {
            id: "v".into(),
            shift_id: "s".into(),
            effective_from: "2026-01-01".into(),
            name: "Day".into(),
            color: "#123456".into(),
            start: start.into(),
            end: end.into(),
            weekdays: vec![1, 2, 3, 4, 5, 6, 7],
            zone: "UTC".into(),
            break_minutes: 0,
            retired: false,
            deleted: false,
            stamp: Stamp {
                at: at("2026-01-01T00:00:00Z"),
                by: "d".into(),
            },
        }
    }

    #[test]
    fn a_long_run_reminds_at_the_limit_and_zero_is_off() {
        let (s, d) = running("2026-03-02T09:00:00Z");
        assert_eq!(due(&s, &d, at("2026-03-02T12:59:00Z"), 240, true), None);
        assert!(matches!(
            due(&s, &d, at("2026-03-02T13:00:00Z"), 240, true),
            Some(Reminder::LongRun { .. })
        ));
        assert_eq!(due(&s, &d, at("2026-03-03T13:00:00Z"), 0, true), None);
        assert_eq!(
            due(&s, "other-device", at("2026-03-03T13:00:00Z"), 240, true),
            None
        );
    }

    #[test]
    fn running_past_a_shift_end_reminds_including_overnight() {
        let (mut s, d) = running("2026-03-02T22:30:00Z");
        s.shifts.insert("v".into(), shift("22:00", "06:00"));
        assert_eq!(due(&s, &d, at("2026-03-03T05:59:00Z"), 0, true), None);
        let got = due(&s, &d, at("2026-03-03T06:01:00Z"), 0, true);
        assert!(
            matches!(got, Some(Reminder::PastShift { ended, .. }) if ended == at("2026-03-03T06:00:00Z"))
        );
        assert_eq!(due(&s, &d, at("2026-03-03T06:01:00Z"), 0, false), None);
    }

    #[test]
    fn a_segment_started_after_the_shift_does_not_remind() {
        let (mut s, d) = running("2026-03-02T18:00:00Z");
        s.shifts.insert("v".into(), shift("09:00", "17:00"));
        assert_eq!(due(&s, &d, at("2026-03-02T19:00:00Z"), 0, true), None);
    }
}
