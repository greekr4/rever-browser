//! JavaScript `Date` formatting shared by the ports.

/// `new Date(ms).toISOString()`; None where JavaScript throws RangeError.
pub fn to_iso_string(ms: u64) -> Option<String> {
    if ms > 8_640_000_000_000_000 {
        return None;
    }
    let (days, rem) = (ms / 86_400_000, ms % 86_400_000);
    // Days since the epoch to a proleptic Gregorian date (Hinnant's civil_from_days).
    let z = days + 719_468;
    let (era, doe) = (z / 146_097, z % 146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    let year = if year <= 9999 {
        format!("{year:04}")
    } else {
        format!("+{year:06}")
    };
    Some(format!(
        "{year}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000
    ))
}
