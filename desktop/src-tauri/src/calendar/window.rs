//! Local time without a time-zone crate: the 8-day window, ISO 8601 with an offset and the local
//! date. The offset at an instant comes from a `fn` pointer so tests can fake a daylight-saving
//! change.

const DAY: i64 = 86_400;
/// Today plus the 7 following days.
const WINDOW_DAYS: i64 = 8;

/// Minutes east of UTC at a unix time.
pub type OffsetAt = fn(i64) -> i32;

/// From local today 00:00 to the local midnight that ends the 7th following day, as unix seconds.
pub fn window(now: i64, offset_min: OffsetAt) -> (i64, i64) {
    let today = (now + secs(offset_min(now))).div_euclid(DAY);
    (
        utc_of_local(today * DAY, offset_min),
        utc_of_local((today + WINDOW_DAYS) * DAY, offset_min),
    )
}

/// The unix time of a local wall-clock time (seconds since the local epoch). The offset is read
/// at the instant itself, so a daylight-saving change between now and then is respected.
fn utc_of_local(local: i64, offset_min: OffsetAt) -> i64 {
    let guess = local - secs(offset_min(local));
    local - secs(offset_min(guess))
}

fn secs(offset_min: i32) -> i64 {
    i64::from(offset_min) * 60
}

/// `YYYY-MM-DDTHH:MM:SS±HH:MM` in the offset at `unix`.
pub fn iso_local(unix: i64, offset_min: i32) -> String {
    let local = unix + secs(offset_min);
    let s = local.rem_euclid(DAY);
    let sign = if offset_min < 0 { '-' } else { '+' };
    let off = offset_min.unsigned_abs();
    format!(
        "{}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
        date_of(local),
        s / 3600,
        s % 3600 / 60,
        s % 60,
        off / 60,
        off % 60
    )
}

/// `YYYY-MM-DD` in the offset at `unix`.
pub fn local_date(unix: i64, offset_min: i32) -> String {
    date_of(unix + secs(offset_min))
}

fn date_of(local: i64) -> String {
    let (y, m, d) = civil_from_days(local.div_euclid(DAY));
    format!("{y:04}-{m:02}-{d:02}")
}

/// The unix time of `YYYY-MM-DDTHH:MM[:SS[.fff]]` followed by `Z` or `±HH:MM`, the shape
/// `iso_local` writes; `None` for anything else.
pub fn unix_of_iso(s: &str) -> Option<i64> {
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let b = s.as_bytes();
    if b.len() < 17 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' {
        return None;
    }
    let (y, mo, d, h, mi) = (
        num(0..4)?,
        num(5..7)?,
        num(8..10)?,
        num(11..13)?,
        num(14..16)?,
    );
    let mut at = 16;
    let mut sec = 0;
    if b[at] == b':' {
        sec = num(at + 1..at + 3)?;
        at += 3;
        if b.get(at) == Some(&b'.') {
            at += 1;
            while b.get(at).is_some_and(u8::is_ascii_digit) {
                at += 1;
            }
        }
    }
    let offset_s = match (b.get(at), s.len() - at) {
        (Some(b'Z'), 1) => 0,
        (Some(sign @ (b'+' | b'-')), 6) if b[at + 3] == b':' => {
            let off = num(at + 1..at + 3)? * 3600 + num(at + 4..at + 6)? * 60;
            if *sign == b'-' { -off } else { off }
        }
        _ => return None,
    };
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    Some(days_from_civil(y, mo, d) * DAY + h * 3600 + mi * 60 + sec - offset_s)
}

/// Howard Hinnant's `days_from_civil`: (year, month, day) to days since 1970-01-01.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to (year, month, day).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-13T00:00:00Z.
    const NOV_13: i64 = 1_794_528_000;

    fn dhaka(_: i64) -> i32 {
        360
    }

    fn utc(_: i64) -> i32 {
        0
    }

    /// +01:00 until 2026-11-16T01:00:00Z, +02:00 from then: a change inside the window.
    const CHANGE: i64 = NOV_13 + 3 * DAY + 3600;

    fn dst(at: i64) -> i32 {
        if at < CHANGE { 60 } else { 120 }
    }

    #[test]
    fn iso_with_an_offset_reads_back_as_unix_time() {
        assert_eq!(unix_of_iso("2026-11-13T00:00:00Z"), Some(NOV_13));
        assert_eq!(unix_of_iso("2026-11-13T06:00:00+06:00"), Some(NOV_13));
        assert_eq!(unix_of_iso("2026-11-12T19:30-04:30"), Some(NOV_13));
        assert_eq!(unix_of_iso("2026-11-13T00:00:00.250Z"), Some(NOV_13));
        for unix in [0, NOV_13, NOV_13 + 12_345, -86_401] {
            for offset in [0, 360, -300, 345] {
                assert_eq!(unix_of_iso(&iso_local(unix, offset)), Some(unix));
            }
        }
        for bad in [
            "",
            "2026-11-13",
            "2026-11-13T00:00:00",
            "2026-11-13T00:00:00+0600",
            "2026-13-13T00:00:00Z",
            "2026-11-13T24:00:00Z",
            "2026-11-13T00:00:00Zjunk",
            "2026-1a-13T00:00:00Z",
        ] {
            assert_eq!(unix_of_iso(bad), None, "{bad}");
        }
    }

    #[test]
    fn dates_and_times_are_formatted_in_the_offset() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        assert_eq!(iso_local(NOV_13, 0), "2026-11-13T00:00:00+00:00");
        assert_eq!(
            iso_local(NOV_13 + 4 * 3600, 360),
            "2026-11-13T10:00:00+06:00"
        );
        assert_eq!(iso_local(NOV_13, -330), "2026-11-12T18:30:00-05:30");
        assert_eq!(local_date(NOV_13 - 1, 0), "2026-11-12");
        assert_eq!(local_date(NOV_13 - 1, 360), "2026-11-13");
        assert_eq!(local_date(NOV_13 + 18 * 3600, 360), "2026-11-14");
    }

    #[test]
    fn the_window_is_eight_local_days_from_midnight() {
        // 2026-11-13 00:00 +06:00 is 2026-11-12T18:00Z.
        let midnight = NOV_13 - 6 * 3600;
        for now in [midnight, midnight + DAY - 1, midnight + 12 * 3600] {
            assert_eq!(window(now, dhaka), (midnight, midnight + 8 * DAY), "{now}");
        }
        assert_eq!(window(midnight - 1, dhaka).0, midnight - DAY);
        assert_eq!(window(NOV_13 + 5, utc), (NOV_13, NOV_13 + 8 * DAY));
    }

    #[test]
    fn the_window_follows_a_daylight_saving_change() {
        let now = NOV_13 + 12 * 3600;
        let (from, to) = window(now, dst);
        // Local midnight at +01:00 and, 8 days later, at +02:00: one hour shorter.
        assert_eq!(from, NOV_13 - 3600);
        assert_eq!(to, NOV_13 + 8 * DAY - 2 * 3600);
        assert_eq!(iso_local(from, dst(from)), "2026-11-13T00:00:00+01:00");
        assert_eq!(iso_local(to, dst(to)), "2026-11-21T00:00:00+02:00");
    }
}
