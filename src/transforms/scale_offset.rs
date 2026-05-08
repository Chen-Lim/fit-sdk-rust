//! Profile-level Scale/Offset transformation.
//!
//! Per protocol §3.2:
//!
//! ```text
//!   physical = raw_value / scale - offset
//!   raw_value = round((physical + offset) * scale)
//! ```
//!
//! `scale` and `offset` come from the field's Profile metadata. `None`
//! everywhere is treated as the identity.

/// Apply scale/offset to a raw numeric value, producing a physical (`f64`)
/// quantity. `None` for either parameter is the identity.
#[inline]
pub fn apply(raw: f64, scale: Option<f64>, offset: Option<f64>) -> f64 {
    let scale = scale.unwrap_or(1.0);
    let offset = offset.unwrap_or(0.0);
    if scale == 1.0 && offset == 0.0 {
        // Skip the divide entirely so the result preserves any integer-exact
        // value (e.g. `heart_rate` 126 stays exactly 126.0).
        raw
    } else {
        raw / scale - offset
    }
}

/// Inverse of [`apply`] — used by the encoder (M8) to round-trip user-space
/// physical values back to wire-space integers.
#[inline]
pub fn unapply(physical: f64, scale: Option<f64>, offset: Option<f64>) -> f64 {
    let scale = scale.unwrap_or(1.0);
    let offset = offset.unwrap_or(0.0);
    (physical + offset) * scale
}

/// True when scale/offset are *both* the identity (and so applying would be
/// a no-op). Useful for hot-path skip checks.
#[inline]
pub fn is_identity(scale: Option<f64>, offset: Option<f64>) -> bool {
    scale.unwrap_or(1.0) == 1.0 && offset.unwrap_or(0.0) == 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_scale_no_offset_is_identity() {
        assert_eq!(apply(126.0, None, None), 126.0);
        assert_eq!(apply(126.0, Some(1.0), Some(0.0)), 126.0);
    }

    #[test]
    fn speed_scale_1000() {
        // raw = 3000 (mm/s wire), scale = 1000, offset = 0 → 3.0 m/s
        assert_eq!(apply(3000.0, Some(1000.0), None), 3.0);
    }

    #[test]
    fn altitude_scale_5_offset_500() {
        // raw = 10435, scale = 5, offset = 500 → 10435/5 - 500 = 1587.0 m
        assert_eq!(apply(10435.0, Some(5.0), Some(500.0)), 1587.0);
    }

    #[test]
    fn unapply_round_trips_apply() {
        let raw = 10435.0;
        let scale = Some(5.0);
        let offset = Some(500.0);
        let physical = apply(raw, scale, offset);
        assert_eq!(unapply(physical, scale, offset), raw);
    }

    #[test]
    fn is_identity_classifies_correctly() {
        assert!(is_identity(None, None));
        assert!(is_identity(Some(1.0), Some(0.0)));
        assert!(!is_identity(Some(1000.0), None));
        assert!(!is_identity(None, Some(500.0)));
    }
}
