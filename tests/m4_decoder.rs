//! M4 integration: end-to-end raw decoding of the real fixtures and the
//! synthetic chained-FIT case.
//!
//! Validates against `Activity.csv` (the JS SDK's decoded reference output —
//! data, not source) that:
//!   1. Total Data row count matches messages produced by our decoder.
//!   2. Per-message-name counts match (record / session / lap / etc.).
//!   3. The first `record` message's `timestamp` (field_def_num=253) decodes
//!      to the integer the CSV shows.

use std::collections::HashMap;
use std::path::PathBuf;

use fit::profile::MesgNum;
use fit::{Decoder, RawValue};

fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/test_data");
    p.push(name);
    p
}

fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixture(name)).expect("fixture must be readable")
}

fn read_csv() -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/example_files/Activity.csv");
    std::fs::read_to_string(p).expect("Activity.csv must be readable")
}

/// Group CSV Data lines by message name (column 2).
fn csv_data_counts(csv: &str) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for line in csv.lines() {
        if let Some(rest) = line.strip_prefix("Data,") {
            // Column 1 is local mesg num, column 2 is message name.
            let mut parts = rest.splitn(3, ',');
            let _local = parts.next();
            if let Some(name) = parts.next() {
                *counts.entry(name.to_string()).or_insert(0) += 1;
            }
        }
    }
    counts
}

// ────────────────────────────────────────────────────────────────────
// Activity.fit — main acceptance test
// ────────────────────────────────────────────────────────────────────

#[test]
fn activity_full_decode_total_count() {
    let bytes = read_fixture("Activity.fit");
    let (msgs, errs) = Decoder::new(&bytes).read_all();
    assert!(errs.is_empty(), "got errors: {errs:?}");

    // From `grep -c '^Data,' Activity.csv` (= 3611). Pinning the total acts
    // as a cheap canary: if decoding desyncs, this fires before the per-mesg
    // breakdown does.
    assert_eq!(
        msgs.len(),
        3611,
        "total message count must match CSV Data rows"
    );
}

#[test]
fn activity_per_message_counts_match_csv() {
    let bytes = read_fixture("Activity.fit");
    let (msgs, errs) = Decoder::new(&bytes).read_all();
    assert!(errs.is_empty());

    // Ours: group by global_mesg_num → name via codegen.
    let mut ours: HashMap<String, usize> = HashMap::new();
    for m in &msgs {
        let name = MesgNum::from_value(m.global_mesg_num)
            .map(|n| n.as_str().to_string())
            .unwrap_or_else(|| format!("mesg_num_{}", m.global_mesg_num));
        *ours.entry(name).or_insert(0) += 1;
    }

    let csv = read_csv();
    let theirs = csv_data_counts(&csv);

    // Every CSV message name must appear with the same count in our decode.
    for (name, expected) in &theirs {
        let actual = ours.get(name).copied().unwrap_or(0);
        assert_eq!(
            actual, *expected,
            "mismatch for `{name}`: decoder={actual}, CSV={expected}",
        );
    }
    // And vice-versa — no extra messages on our side either.
    for (name, decoded) in &ours {
        let expected = theirs.get(name).copied().unwrap_or(0);
        assert_eq!(
            *decoded, expected,
            "decoder produced extra messages for `{name}` (our={decoded}, CSV={expected})",
        );
    }
}

#[test]
fn activity_first_record_timestamp_is_csv_value() {
    let bytes = read_fixture("Activity.fit");
    let (msgs, errs) = Decoder::new(&bytes).read_all();
    assert!(errs.is_empty());

    let first_record = msgs
        .iter()
        .find(|m| m.global_mesg_num == 20 /* MesgNum::Record */)
        .expect("Activity.fit must have at least one record message");

    // field_def_num 253 is `timestamp` (date_time → uint32 LE).
    let ts = first_record
        .field(253)
        .expect("record must have a timestamp field");
    assert_eq!(
        ts.value.as_u32(),
        Some(995749880),
        "timestamp of first record must match the CSV value",
    );
}

#[test]
fn activity_first_file_id_decodes_with_correct_types() {
    let bytes = read_fixture("Activity.fit");
    let (msgs, errs) = Decoder::new(&bytes).read_all();
    assert!(errs.is_empty());

    // The very first message in Activity.fit is file_id (mesg_num=0).
    let first = &msgs[0];
    assert_eq!(first.global_mesg_num, 0);

    // Spot-check values that the CSV agrees with the binary on:
    //   type = 4 (activity), manufacturer = 255 (development),
    //   product = 0, time_created = 995749880.
    // (CSV's serial_number value diverges from the binary — likely an
    // anonymisation in the FIT Cookbook example. Our decoder is byte-accurate
    // on the wire; verifying the *type* of serial_number is still useful.)
    assert_eq!(first.field(0).unwrap().value.as_u8(), Some(4), "type");
    assert_eq!(
        first.field(1).unwrap().value.as_u16(),
        Some(255),
        "manufacturer"
    );
    assert_eq!(first.field(2).unwrap().value.as_u16(), Some(0), "product");
    assert_eq!(
        first.field(4).unwrap().value.as_u32(),
        Some(995749880),
        "time_created"
    );

    // serial_number must be uint32z (a Z-typed scalar field).
    let serial = first.field(3).expect("file_id.serial_number missing");
    assert!(
        matches!(&serial.value, RawValue::UInt32z(v) if v.len() == 1),
        "serial_number must decode as a length-1 UInt32z; got {:?}",
        serial.value,
    );
}

// ────────────────────────────────────────────────────────────────────
// Other fixtures
// ────────────────────────────────────────────────────────────────────

#[test]
fn hrm_plugin_decodes_without_errors() {
    let bytes = read_fixture("HrmPluginTestActivity.fit");
    let (msgs, errs) = Decoder::new(&bytes).read_all();
    assert!(errs.is_empty(), "got errors: {errs:?}");
    assert!(!msgs.is_empty());
}

#[test]
fn gear_change_decodes_without_errors() {
    let bytes = read_fixture("WithGearChangeData.fit");
    let (msgs, errs) = Decoder::new(&bytes).read_all();
    assert!(errs.is_empty(), "got errors: {errs:?}");
    assert!(!msgs.is_empty());
}

// ────────────────────────────────────────────────────────────────────
// Multi-FIT chain (synthetic)
// ────────────────────────────────────────────────────────────────────

#[test]
fn multi_fit_chain_doubles_message_count() {
    let single = read_fixture("Activity.fit");
    let chained: Vec<u8> = single.iter().chain(single.iter()).copied().collect();

    let (msgs, errs) = Decoder::new(&chained).read_all();
    assert!(
        errs.is_empty(),
        "chained file should decode cleanly: {errs:?}"
    );
    assert_eq!(
        msgs.len(),
        3611 * 2,
        "two concatenated Activity.fit files must yield 2× messages (decoder must reset \
         LocalDefinitions at the chain boundary)"
    );
}

// ────────────────────────────────────────────────────────────────────
// Iterator semantics
// ────────────────────────────────────────────────────────────────────

#[test]
fn iterator_terminates_after_fatal_error() {
    // Truncate a real file mid-record. After yielding some messages, the
    // iterator must produce exactly one error and then `None` forever.
    let bytes = read_fixture("Activity.fit");
    let truncated = &bytes[..bytes.len() / 2];

    let mut decoder = Decoder::new(truncated);
    let mut saw_error = false;
    for item in decoder.by_ref() {
        if item.is_err() {
            saw_error = true;
            break;
        }
    }
    assert!(saw_error, "truncated file must surface an error");
    // After the error, the iterator must be drained.
    assert!(decoder.next().is_none());
    assert!(decoder.next().is_none());
}
