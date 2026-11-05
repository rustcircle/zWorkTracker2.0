//! Generates `src/generated/tzdata.json` for the frontend from the same tz database Rust links,
//! so both sides use identical rules. Every transition from 2000 to 2045 is listed.
//!
//! Run: cargo run -p z-work-core --example gen_tzdata -- ../src/generated/tzdata.json

use std::collections::BTreeMap;

use chrono::{DateTime, Offset, TimeZone, Utc};
use chrono_tz::{OffsetName, TZ_VARIANTS, Tz};

const FROM: i64 = 946_684_800; // 2000-01-01
const TO: i64 = 2_398_291_200; // 2046-01-01
const DAY: i64 = 86_400;

fn at(tz: Tz, secs: i64) -> (i32, String) {
    let dt: DateTime<Utc> = DateTime::from_timestamp(secs, 0).unwrap_or(DateTime::UNIX_EPOCH);
    let off = tz.offset_from_utc_datetime(&dt.naive_utc());
    let abbr = off.abbreviation().map_or_else(String::new, str::to_owned);
    (off.fix().local_minus_utc(), abbr)
}

fn main() -> std::io::Result<()> {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "tzdata.json".to_owned());
    let mut rules: Vec<String> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    let mut zones: BTreeMap<String, usize> = BTreeMap::new();
    for tz in TZ_VARIANTS {
        let mut periods: Vec<(i32, String)> = Vec::new();
        let mut times: Vec<i64> = Vec::new();
        let mut kinds: Vec<usize> = Vec::new();
        let mut cur = at(tz, FROM);
        periods.push(cur.clone());
        let mut t = FROM;
        while t < TO {
            let next = t + DAY;
            if at(tz, next) != cur {
                // Bisect to the second of the change.
                let (mut lo, mut hi) = (t, next);
                while hi - lo > 1 {
                    let mid = lo + (hi - lo) / 2;
                    if at(tz, mid) == cur {
                        lo = mid
                    } else {
                        hi = mid
                    }
                }
                let change = at(tz, hi);
                let idx = periods
                    .iter()
                    .position(|p| *p == change)
                    .unwrap_or_else(|| {
                        periods.push(change.clone());
                        periods.len() - 1
                    });
                times.push(hi);
                kinds.push(idx);
                cur = change;
                t = hi;
            } else {
                t = next;
            }
        }
        let periods_json = serde_json::to_string(&periods).unwrap_or_default();
        // Transition times are stored in minutes as deltas: compact and exact for all modern rules.
        let mut last = FROM / 60;
        let deltas: Vec<i64> = times
            .iter()
            .map(|x| {
                let m = x.div_euclid(60);
                let d = m - last;
                last = m;
                d
            })
            .collect();
        let rule = format!(
            "{{\"p\":{periods_json},\"t\":{},\"k\":{}}}",
            serde_json::to_string(&deltas).unwrap_or_default(),
            serde_json::to_string(&kinds).unwrap_or_default()
        );
        let id = *index.entry(rule.clone()).or_insert_with(|| {
            rules.push(rule);
            rules.len() - 1
        });
        zones.insert(tz.name().to_owned(), id);
    }
    let zones_json = serde_json::to_string(&zones).unwrap_or_default();
    let body = format!(
        "{{\"version\":\"{}\",\"fromMinutes\":{},\"zones\":{zones_json},\"rules\":[{}]}}\n",
        chrono_tz::IANA_TZDB_VERSION,
        FROM / 60,
        rules.join(",")
    );
    std::fs::write(&out, &body)?;
    eprintln!(
        "{} zones, {} distinct rules, {} bytes, tzdb {}",
        zones.len(),
        rules.len(),
        body.len(),
        chrono_tz::IANA_TZDB_VERSION
    );
    Ok(())
}
