//! FIT timestamp conversions.
//!
//! FIT stores absolute time as a `u32` count of seconds since
//! **1989-12-31 00:00:00 UTC** — 631,065,600 seconds *after* the Unix epoch.
//! The `chrono`-based helpers ([`fit_to_datetime`] / [`datetime_to_fit`]) are
//! only compiled when the `chrono` feature is enabled.

#[cfg(feature = "chrono")]
use chrono::{DateTime, Utc};

/// Seconds between Unix epoch (`1970-01-01`) and FIT epoch (`1989-12-31`).
pub const FIT_EPOCH_OFFSET_SECS: i64 = 631_065_600;

/// Convert a FIT timestamp (seconds since FIT epoch) into a UTC datetime.
///
/// Returns `None` only for the unrepresentable case where adding the offset
/// overflows `i64::MAX` — practically impossible for `u32` inputs.
#[cfg(feature = "chrono")]
pub fn fit_to_datetime(fit_seconds: u32) -> Option<DateTime<Utc>> {
    let unix = i64::from(fit_seconds).checked_add(FIT_EPOCH_OFFSET_SECS)?;
    DateTime::from_timestamp(unix, 0)
}

/// Inverse of [`fit_to_datetime`]. Returns `None` if the datetime is before
/// the FIT epoch or beyond `u32::MAX` seconds after it (≈ year 2125).
#[cfg(feature = "chrono")]
pub fn datetime_to_fit(dt: DateTime<Utc>) -> Option<u32> {
    let secs = dt.timestamp().checked_sub(FIT_EPOCH_OFFSET_SECS)?;
    if (0..=i64::from(u32::MAX)).contains(&secs) {
        Some(secs as u32)
    } else {
        None
    }
}

#[cfg(all(test, feature = "chrono"))]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn fit_epoch_is_1989_12_31_utc() {
        let dt = fit_to_datetime(0).unwrap();
        assert_eq!(dt, Utc.with_ymd_and_hms(1989, 12, 31, 0, 0, 0).unwrap());
    }

    #[test]
    fn known_value_round_trips() {
        let dt = fit_to_datetime(995_749_880).unwrap();
        assert_eq!(dt.timestamp(), 995_749_880 + FIT_EPOCH_OFFSET_SECS);
        let back = datetime_to_fit(dt).unwrap();
        assert_eq!(back, 995_749_880);
    }

    #[test]
    fn datetime_before_fit_epoch_is_none() {
        let dt = Utc.with_ymd_and_hms(1970, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(datetime_to_fit(dt), None);
    }

    #[test]
    fn unix_offset_is_correct() {
        let unix_dt = DateTime::from_timestamp(FIT_EPOCH_OFFSET_SECS, 0).unwrap();
        assert_eq!(
            unix_dt,
            Utc.with_ymd_and_hms(1989, 12, 31, 0, 0, 0).unwrap()
        );
    }
}
