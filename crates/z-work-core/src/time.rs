//! UTC timestamps with whole-second precision, stored as `YYYY-MM-DDTHH:MM:SSZ`.
//! Whole seconds keep segment sums exact in every export and in the UI.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Ts(pub i64);

impl Ts {
    /// 0000-01-01T00:00:00Z and 9999-12-31T23:59:59Z: the range `iso` can print and `parse` can read.
    /// 2000-01-01, the earliest moment any record can plausibly hold.
    pub const EARLIEST: Self = Self(946_684_800);
    pub const MIN: Self = Self(-62_167_219_200);
    pub const MAX: Self = Self(253_402_300_799);

    #[must_use]
    pub fn secs(self) -> i64 {
        self.0
    }
    #[must_use]
    pub fn datetime(self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.0, 0).unwrap_or(DateTime::UNIX_EPOCH)
    }
    /// Twenty ASCII bytes for any time in range. Stores hold several per record, so this is the
    /// hot path of saving and of every snapshot; `iso_chrono` is the definition it must match.
    fn iso_bytes(self) -> Option<[u8; 20]> {
        if !(Self::MIN.0..=Self::MAX.0).contains(&self.0) {
            return None;
        }
        let (days, secs) = (self.0.div_euclid(86_400), self.0.rem_euclid(86_400));
        let (y, m, d) = civil_from_days(days);
        let digit = |v: i64| b'0' + (v % 10) as u8;
        Some([
            digit(y / 1000),
            digit(y / 100),
            digit(y / 10),
            digit(y),
            b'-',
            digit(m / 10),
            digit(m),
            b'-',
            digit(d / 10),
            digit(d),
            b'T',
            digit(secs / 36_000),
            digit(secs / 3600),
            b':',
            digit(secs / 600 % 6),
            digit(secs / 60),
            b':',
            digit(secs % 60 / 10),
            digit(secs % 60),
            b'Z',
        ])
    }
    fn iso_chrono(self) -> String {
        self.datetime().format("%Y-%m-%dT%H:%M:%SZ").to_string()
    }
    #[must_use]
    pub fn iso(self) -> String {
        match self.iso_bytes() {
            Some(b) => b.iter().map(|c| char::from(*c)).collect(),
            None => self.iso_chrono(),
        }
    }
    /// The exact form this program writes, without the general parser. None for anything else.
    fn parse_stored(s: &str) -> Option<Self> {
        let &[
            y0,
            y1,
            y2,
            y3,
            b'-',
            m0,
            m1,
            b'-',
            d0,
            d1,
            b'T',
            h0,
            h1,
            b':',
            i0,
            i1,
            b':',
            s0,
            s1,
            b'Z',
        ] = s.as_bytes()
        else {
            return None;
        };
        let y = two(y0, y1)? * 100 + two(y2, y3)?;
        let (m, d) = (two(m0, m1)?, two(d0, d1)?);
        let (h, i, sec) = (two(h0, h1)?, two(i0, i1)?, two(s0, s1)?);
        // A leap second or any other oddity goes to the general parser, which has the last word.
        if !(1..=12).contains(&m)
            || d < 1
            || d > days_in_month(y, m)
            || h > 23
            || i > 59
            || sec > 59
        {
            return None;
        }
        Some(Self(
            days_from_civil(y, m, d) * 86_400 + h * 3600 + i * 60 + sec,
        ))
    }
    fn parse_chrono(s: &str) -> Option<Self> {
        DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| Self(d.timestamp()))
            .filter(|t| (Self::MIN.0..=Self::MAX.0).contains(&t.0))
    }
    /// Accepts any RFC 3339 value, normalises it to UTC and truncates to whole seconds.
    pub fn parse(s: &str) -> Option<Self> {
        Self::parse_stored(s).or_else(|| Self::parse_chrono(s))
    }
    #[must_use]
    pub fn later(self, secs: i64) -> Self {
        Self(self.0.saturating_add(secs).min(Self::MAX.0))
    }
}

/// Two ASCII digits as a number.
fn two(a: u8, b: u8) -> Option<i64> {
    (a.is_ascii_digit() && b.is_ascii_digit())
        .then(|| i64::from(a - b'0') * 10 + i64::from(b - b'0'))
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        4 | 6 | 9 | 11 => 30,
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let (era, yoe) = (y.div_euclid(400), y.rem_euclid(400));
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of `days_from_civil`.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

impl fmt::Display for Ts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.iso())
    }
}

impl Serialize for Ts {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.iso_bytes().as_ref().map(|b| std::str::from_utf8(b)) {
            Some(Ok(text)) => s.serialize_str(text),
            _ => s.serialize_str(&self.iso_chrono()),
        }
    }
}

impl<'de> Deserialize<'de> for Ts {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Text;
        impl serde::de::Visitor<'_> for Text {
            type Value = Ts;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string")
            }
            fn visit_str<E: serde::de::Error>(self, raw: &str) -> Result<Ts, E> {
                Ts::parse(raw).ok_or_else(|| E::custom("invalid RFC 3339 timestamp"))
            }
        }
        d.deserialize_str(Text)
    }
}

/// The only source of time in the application.
pub trait Clock: Send + Sync {
    fn now(&self) -> Ts;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Ts {
        Ts(Utc::now().timestamp())
    }
}

/// Test clock. Clones share one time value.
#[derive(Clone, Debug, Default)]
pub struct ManualClock(Arc<AtomicI64>);

impl ManualClock {
    #[must_use]
    pub fn at(iso: &str) -> Self {
        Self(Arc::new(AtomicI64::new(Ts::parse(iso).map_or(0, Ts::secs))))
    }
    pub fn set(&self, iso: &str) {
        self.0
            .store(Ts::parse(iso).map_or(0, Ts::secs), Ordering::SeqCst);
    }
    pub fn advance(&self, secs: i64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Ts {
        Ts(self.0.load(Ordering::SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_as_utc_z() {
        let t = Ts::parse("2026-03-08T07:00:00.750-04:00").unwrap();
        assert_eq!(t.iso(), "2026-03-08T11:00:00Z");
        let j = serde_json::to_string(&t).unwrap();
        assert_eq!(j, "\"2026-03-08T11:00:00Z\"");
        assert_eq!(serde_json::from_str::<Ts>(&j).unwrap(), t);
    }

    /// The fast paths are only allowed to be faster: chrono is the definition of both directions.
    #[test]
    fn iso_matches_chrono_for_every_day_and_across_the_whole_range() {
        let check = |t: Ts| {
            assert_eq!(t.iso(), t.iso_chrono(), "format of {}", t.0);
            let text = t.iso_chrono();
            assert_eq!(
                Ts::parse_stored(&text),
                Ts::parse_chrono(&text),
                "parse of {text}"
            );
            assert_eq!(serde_json::to_string(&t).unwrap(), format!("\"{text}\""));
        };
        // Every day from 1890 to 2110 at a changing second, then every 97th day of the whole range.
        let mut secs = -2_524_521_600i64;
        while secs < 4_417_977_600 {
            check(Ts(secs + (secs / 86_400).rem_euclid(86_400)));
            secs += 86_400;
        }
        let mut secs = Ts::MIN.0;
        while secs <= Ts::MAX.0 {
            check(Ts(secs));
            secs += 86_400 * 97 + 3599;
        }
        for t in [
            Ts::MIN,
            Ts::MAX,
            Ts::EARLIEST,
            Ts(0),
            Ts(-1),
            Ts(86_399),
            Ts(86_400),
        ] {
            check(t);
        }
    }

    #[test]
    fn out_of_range_times_still_print_as_before() {
        for t in [Ts(Ts::MAX.0 + 86_400), Ts(Ts::MIN.0 - 1), Ts(i64::MIN / 2)] {
            assert_eq!(t.iso(), t.iso_chrono());
            assert_eq!(
                serde_json::to_string(&t).unwrap(),
                format!("\"{}\"", t.iso_chrono())
            );
        }
    }

    proptest::proptest! {
        /// Whatever the text, the fast parser agrees with chrono or steps aside for it.
        #[test]
        fn parse_agrees_with_chrono_on_any_near_valid_text(
            y in 0u32..10_000, m in 0u32..14, d in 0u32..33, h in 0u32..26, i in 0u32..62, s in 0u32..62,
            sep in proptest::sample::select(vec!['T', 't', ' ']),
            end in proptest::sample::select(vec!["Z", "z", "+00:00", ".5Z", "-04:30", ""]),
        ) {
            let text = format!("{y:04}-{m:02}-{d:02}{sep}{h:02}:{i:02}:{s:02}{end}");
            proptest::prop_assert_eq!(Ts::parse(&text), Ts::parse_chrono(&text), "{}", text);
        }

        #[test]
        fn parse_agrees_with_chrono_on_arbitrary_bytes(text in "[0-9TZ:+-]{0,24}") {
            proptest::prop_assert_eq!(Ts::parse(&text), Ts::parse_chrono(&text), "{}", text);
        }

        #[test]
        fn iso_agrees_with_chrono_anywhere(secs in Ts::MIN.0..=Ts::MAX.0) {
            proptest::prop_assert_eq!(Ts(secs).iso(), Ts(secs).iso_chrono());
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(serde_json::from_str::<Ts>("\"yesterday\"").is_err());
        assert!(Ts::parse("2026-13-01T00:00:00Z").is_none());
    }

    #[test]
    fn rejects_values_that_would_print_as_year_10000() {
        assert!(Ts::parse("9999-12-31T23:59:59-01:00").is_none());
        let max = Ts::parse("9999-12-31T23:59:59Z").unwrap();
        assert_eq!(max, Ts::MAX);
        assert_eq!(max.later(5), max);
        assert_eq!(Ts::parse(&max.iso()), Some(max));
    }
}
