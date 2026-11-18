//! M6 integration: DecodeMode (`skip_header`, `data_only`), option conflict
//! validation, `on_mesg` callback, `decode_memo_glob`, and HR merge.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use fit::{Decoder, FitError, Value};

fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/test_data");
    p.push(name);
    p
}

fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixture(name)).expect("fixture must be readable")
}

const FIT_FILE_SHORT: &[u8] = &[
    0x0E, 0x20, 0x8B, 0x08, 0x24, 0x00, 0x00, 0x00, 0x2E, 0x46, 0x49, 0x54, 0x8E, 0xA3, 0x40, 0x00,
    0x00, 0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x01, 0x02, 0x84, 0x04, 0x04, 0x86, 0x08, 0x0A, 0x07,
    0x00, 0x04, 0x01, 0x00, 0x00, 0xCA, 0x9A, 0x3B, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x00, 0x5D, 0xF2,
];

const FIT_FILE_SHORT_INVALID_HEADER: &[u8] = &[
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00,
    0x00, 0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x01, 0x02, 0x84, 0x04, 0x04, 0x86, 0x08, 0x0A, 0x07,
    0x00, 0x04, 0x01, 0x00, 0x00, 0xCA, 0x9A, 0x3B, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x00, 0x5D, 0xF2,
];

const FIT_FILE_SHORT_INVALID_CRC: &[u8] = &[
    0x0E, 0x20, 0x8B, 0x08, 0x24, 0x00, 0x00, 0x00, 0x2E, 0x46, 0x49, 0x54, 0x8E, 0xA3, 0x40, 0x00,
    0x00, 0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x01, 0x02, 0x84, 0x04, 0x04, 0x86, 0x08, 0x0A, 0x07,
    0x00, 0x04, 0x01, 0x00, 0x00, 0xCA, 0x9A, 0x3B, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x00, 0x00, 0x00,
];

const FIT_FILE_MEMO_GLOB_SIMPLE: &[u8] = &[
    0x0E, 0x20, 0xAB, 0x52, 0x45, 0x00, 0x00, 0x00, 0x2E, 0x46, 0x49, 0x54, 0xCA, 0x8E, 0x40, 0x00,
    0x01, 0x00, 0x1B, 0x02, 0xFE, 0x02, 0x84, 0x08, 0x10, 0x07, 0x00, 0x00, 0x00, 0x77, 0x6F, 0x72,
    0x6B, 0x6F, 0x75, 0x74, 0x53, 0x74, 0x65, 0x70, 0x4D, 0x65, 0x73, 0x67, 0x00, 0x40, 0x00, 0x01,
    0x00, 0x91, 0x05, 0xFA, 0x04, 0x86, 0x01, 0x02, 0x84, 0x02, 0x02, 0x84, 0x03, 0x01, 0x02, 0x04,
    0x07, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1B, 0x00, 0x00, 0x08, 0x73, 0x74, 0x72, 0x69,
    0x6E, 0x67, 0x00, 0xEC, 0x85,
];

// ────────────────────────────────────────────────────────────────────
// skip_header (DecodeMode::SkipHeader)
// ────────────────────────────────────────────────────────────────────

#[test]
fn skip_header_decodes_invalid_header_and_invalid_crc() {
    // Invalid header fails without skip_header, succeeds with skip_header.
    let (_, errs) = Decoder::builder(FIT_FILE_SHORT_INVALID_HEADER)
        .build()
        .read_all();
    assert_eq!(errs.len(), 1);

    let (msgs, errs) = Decoder::builder(FIT_FILE_SHORT_INVALID_HEADER)
        .skip_header(true)
        .build()
        .read_all();
    assert!(errs.is_empty(), "got errors: {errs:?}");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].name, "file_id");

    // Valid header + valid CRC also succeeds with skip_header and preserves file_id.
    let (msgs, errs) = Decoder::builder(FIT_FILE_SHORT)
        .skip_header(true)
        .build()
        .read_all();
    assert!(errs.is_empty(), "got errors: {errs:?}");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].name, "file_id");

    // Invalid CRC fails in Normal mode, succeeds with skip_header.
    let (msgs, errs) = Decoder::builder(FIT_FILE_SHORT_INVALID_CRC)
        .build()
        .read_all();
    assert_eq!(msgs.len(), 1);
    assert!(matches!(
        errs.as_slice(),
        [FitError::FileCrcMismatch { .. }]
    ));

    let (msgs, errs) = Decoder::builder(FIT_FILE_SHORT_INVALID_CRC)
        .skip_header(true)
        .build()
        .read_all();
    assert!(errs.is_empty(), "got errors: {errs:?}");
    assert_eq!(msgs.len(), 1);
}

// ────────────────────────────────────────────────────────────────────
// data_only (DecodeMode::DataOnly)
// ────────────────────────────────────────────────────────────────────

#[test]
fn data_only_decodes_headerless_and_rejects_headed_file() {
    let data_only_slice = &FIT_FILE_SHORT[14..];

    // Headerless slice fails in Normal mode, succeeds with data_only.
    let (_, errs) = Decoder::builder(data_only_slice).build().read_all();
    assert_eq!(errs.len(), 1);

    let (msgs, errs) = Decoder::builder(data_only_slice)
        .data_only(true)
        .build()
        .read_all();
    assert!(errs.is_empty(), "got errors: {errs:?}");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].name, "file_id");

    // Headerless slice with corrupt CRC also succeeds in data_only mode.
    let (msgs, errs) = Decoder::builder(&FIT_FILE_SHORT_INVALID_CRC[14..])
        .data_only(true)
        .build()
        .read_all();
    assert!(errs.is_empty(), "got errors: {errs:?}");
    assert_eq!(msgs.len(), 1);

    // File WITH header fails when decoded as data_only.
    let (_, errs) = Decoder::builder(FIT_FILE_SHORT)
        .data_only(true)
        .build()
        .read_all();
    assert_eq!(errs.len(), 1);
}

// ────────────────────────────────────────────────────────────────────
// Conflicting option validation
// ────────────────────────────────────────────────────────────────────

#[test]
fn conflicting_decode_modes_return_invalid_options() {
    let (msgs, errs) = Decoder::builder(FIT_FILE_SHORT)
        .skip_header(true)
        .data_only(true)
        .build()
        .read_all();
    assert!(msgs.is_empty());
    assert!(matches!(errs.as_slice(), [FitError::InvalidOptions(_)]));
}

#[cfg(feature = "chrono")]
#[test]
fn merge_heart_rates_requires_scale_offset_and_components() {
    let bytes = read_fixture("HrmPluginTestActivity.fit");

    let (msgs, errs) = Decoder::builder(&bytes)
        .merge_heart_rates(true)
        .apply_scale_and_offset(false)
        .build()
        .read_all();
    assert!(msgs.is_empty());
    assert!(matches!(errs.as_slice(), [FitError::InvalidOptions(_)]));

    let (msgs, errs) = Decoder::builder(&bytes)
        .merge_heart_rates(true)
        .expand_components(false)
        .build()
        .read_all();
    assert!(msgs.is_empty());
    assert!(matches!(errs.as_slice(), [FitError::InvalidOptions(_)]));
}

// ────────────────────────────────────────────────────────────────────
// on_mesg callback
// ────────────────────────────────────────────────────────────────────

#[test]
fn on_mesg_callback_fires_for_each_message() {
    let bytes = read_fixture("Activity.fit");
    let count = Arc::new(AtomicUsize::new(0));
    let count_clone = Arc::clone(&count);

    let (msgs, errs) = Decoder::builder(&bytes)
        .on_mesg(move |_| {
            count_clone.fetch_add(1, Ordering::SeqCst);
        })
        .build()
        .read_all();

    assert!(errs.is_empty());
    assert_eq!(
        count.load(Ordering::SeqCst),
        msgs.len(),
        "on_mesg should fire exactly once per message"
    );
}

// ────────────────────────────────────────────────────────────────────
// MemoGlob integration
// ────────────────────────────────────────────────────────────────────

#[test]
fn decode_memo_glob_works_with_default_enum_resolution() {
    let (msgs, errs) = Decoder::builder(FIT_FILE_MEMO_GLOB_SIMPLE)
        .decode_memo_glob(true)
        .build()
        .read_all();
    assert!(errs.is_empty(), "got errors: {errs:?}");
    let step = msgs
        .iter()
        .find(|m| m.global_mesg_num == 27)
        .expect("workout_step");
    assert_eq!(
        step.field("notes").unwrap().value,
        Value::String("string".to_string())
    );
}

// ────────────────────────────────────────────────────────────────────
// HR merge integration
// ────────────────────────────────────────────────────────────────────

#[cfg(feature = "chrono")]
#[test]
fn hr_merge_adds_heart_rate_to_records() {
    let bytes = read_fixture("Activity.fit");
    let (mut messages, errs) = Decoder::builder(&bytes).build().read_all();
    assert!(errs.is_empty());

    let before_hr_count = messages
        .iter()
        .filter(|m| m.global_mesg_num == 20 && m.field("heart_rate").is_some())
        .count();

    fit::merge_heart_rates(&mut messages);

    let after_hr_count = messages
        .iter()
        .filter(|m| m.global_mesg_num == 20 && m.field("heart_rate").is_some())
        .count();

    assert!(
        after_hr_count >= before_hr_count,
        "HR merge should not remove heart_rate fields"
    );
}
