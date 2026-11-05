//! The few JavaScript number rules the ports depend on, so a port gives the same answer as 1.x.

/// `Math.round`: the nearest integer, an exact half going up (towards positive infinity, so -2.5 gives -2).
pub(crate) fn round(x: f64) -> f64 {
    let floor = x.floor();
    // `x - floor` is exact for every finite double, so the half is tested without a rounding error.
    if x - floor >= 0.5 { floor + 1.0 } else { floor }
}

/// `Math.round(secs / 60)` for whole seconds: the nearest whole minute, an exact half minute going up.
pub(crate) fn round_minutes(secs: i64) -> i64 {
    secs.saturating_add(30).div_euclid(60)
}

/// `String(n).padStart(2, '0')`: two digits at least, a minus sign counting as one ("-5" stays "-5").
pub(crate) fn pad2(n: i64) -> String {
    format!("{n:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_goes_up_on_an_exact_half_like_math_round() {
        assert_eq!(round(2.5), 3.0);
        assert_eq!(round(-2.5), -2.0);
        assert_eq!(round(0.499_999_999_999_999_94), 0.0);
        assert_eq!(round(-0.1), 0.0);
        assert_eq!(round(1.4), 1.0);
    }

    #[test]
    fn whole_minutes_match_math_round_of_the_division() {
        for secs in -200_i64..=7_300 {
            let want = round(secs as f64 / 60.0) as i64;
            assert_eq!(round_minutes(secs), want, "{secs} s");
        }
    }

    #[test]
    fn pad2_is_pad_start() {
        assert_eq!(pad2(5), "05");
        assert_eq!(pad2(0), "00");
        assert_eq!(pad2(45), "45");
        assert_eq!(pad2(123), "123");
        assert_eq!(pad2(-5), "-5");
    }
}
