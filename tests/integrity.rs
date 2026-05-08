//! Integration tests for header parsing and CRC verification against the
//! three real FIT fixtures from Garmin's JS SDK test data.
//!
//! Fixtures live under `tests/fixtures/test_data/`.

use std::path::{Path, PathBuf};

fn fixture_path(name: &str) -> PathBuf {
    // CARGO_MANIFEST_DIR points at the project root.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/test_data");
    p.push(name);
    p
}

fn read_fixture(name: &str) -> Vec<u8> {
    let path = fixture_path(name);
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "failed to read fixture {} from {}: {e}",
            name,
            path.display()
        )
    })
}

#[test]
fn fixture_paths_resolve() {
    for name in ["Activity.fit", "HrmPluginTestActivity.fit", "WithGearChangeData.fit"] {
        let path = fixture_path(name);
        assert!(
            Path::new(&path).exists(),
            "fixture file does not exist: {}",
            path.display()
        );
    }
}

#[test]
fn activity_is_recognized_as_fit() {
    let bytes = read_fixture("Activity.fit");
    assert!(fit::is_fit(&bytes));
}

#[test]
fn activity_passes_integrity() {
    let bytes = read_fixture("Activity.fit");
    fit::check_integrity(&bytes).expect("Activity.fit should have valid CRCs");
}

#[test]
fn hrm_plugin_passes_integrity() {
    let bytes = read_fixture("HrmPluginTestActivity.fit");
    fit::check_integrity(&bytes).expect("HrmPluginTestActivity.fit should have valid CRCs");
}

#[test]
fn gear_change_passes_integrity() {
    let bytes = read_fixture("WithGearChangeData.fit");
    fit::check_integrity(&bytes).expect("WithGearChangeData.fit should have valid CRCs");
}

#[test]
fn header_fields_make_sense_for_activity() {
    let bytes = read_fixture("Activity.fit");
    let header = fit::FileHeader::parse(&bytes).unwrap();

    // The Garmin JS SDK test data uses 14-byte headers.
    assert_eq!(header.header_size, 14);
    assert!(header.header_crc.is_some());

    // Total file size implied by the header should exactly match the byte slice
    // (no chained files in this fixture).
    assert_eq!(header.total_file_size(), bytes.len());

    // ".FIT" signature already validated by parse(), but spot-check:
    assert_eq!(&bytes[8..12], b".FIT");
}

#[test]
fn corrupted_file_crc_is_detected() {
    let mut bytes = read_fixture("Activity.fit");
    let len = bytes.len();
    // Flip a bit in the trailing CRC.
    bytes[len - 1] ^= 0x01;
    let err = fit::check_integrity(&bytes).expect_err("corruption must be detected");
    assert!(
        matches!(err, fit::FitError::FileCrcMismatch { .. }),
        "expected FileCrcMismatch, got {err:?}"
    );
}

#[test]
fn corrupted_header_crc_is_detected() {
    let mut bytes = read_fixture("Activity.fit");
    // Flip a bit in the header CRC bytes (offset 12..14).
    bytes[12] ^= 0x01;
    let err = fit::check_integrity(&bytes).expect_err("header corruption must be detected");
    assert!(
        matches!(err, fit::FitError::HeaderCrcMismatch { .. }),
        "expected HeaderCrcMismatch, got {err:?}"
    );
}

#[test]
fn truncated_file_is_rejected() {
    let bytes = read_fixture("Activity.fit");
    // Drop the trailing CRC bytes — total length now < total_file_size().
    let truncated = &bytes[..bytes.len() - 2];
    assert!(matches!(
        fit::check_integrity(truncated),
        Err(fit::FitError::TooShort { .. })
    ));
}
