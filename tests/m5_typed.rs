//! M5 integration: end-to-end typed decoding (transforms applied) on real
//! fixtures + targeted tests for components and SubField resolution.

use std::path::PathBuf;

use fit::{Decoder, Field, FieldKind, Value};

fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/test_data");
    p.push(name);
    p
}

fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixture(name)).expect("fixture must be readable")
}

// ────────────────────────────────────────────────────────────────────
// Activity.fit — transform pipeline integration
// ────────────────────────────────────────────────────────────────────

#[test]
fn activity_typed_decode_total_count_matches_raw() {
    // Typed decoder must produce exactly as many messages as the raw decoder.
    let bytes = read_fixture("Activity.fit");
    let raw_count = Decoder::new(&bytes).read_all().0.len();
    let typed_count = Decoder::builder(&bytes).build().read_all().0.len();
    assert_eq!(
        typed_count, raw_count,
        "typed pipeline must not drop messages"
    );
    assert_eq!(typed_count, 3611);
}

#[cfg(feature = "chrono")]
#[test]
fn activity_first_record_has_datetime_and_known_fields() {
    let bytes = read_fixture("Activity.fit");
    let (msgs, errs) = Decoder::builder(&bytes).build().read_all();
    assert!(errs.is_empty());

    let first_record = msgs
        .iter()
        .find(|m| m.global_mesg_num == 20)
        .expect("must have a record message");
    assert_eq!(first_record.name, "record");

    // timestamp → DateTime (after FIT epoch conversion)
    let ts = first_record.field("timestamp").expect("record.timestamp");
    let dt = ts.value.as_datetime().expect("timestamp must be DateTime");
    assert_eq!(
        dt.timestamp(),
        995_749_880 + fit::datetime::FIT_EPOCH_OFFSET_SECS
    );

    // heart_rate is a uint8 with no scale, no enum: stays as UInt(126)
    let hr = first_record.field("heart_rate").expect("record.heart_rate");
    assert_eq!(hr.value, Value::UInt(126), "heart_rate raw passthrough");

    // speed has scale=1000 → 1000 raw / 1000 = 1.0 m/s
    let speed = first_record.field("speed").expect("record.speed");
    assert!(matches!(speed.value, Value::Float(v) if (v - 1.0).abs() < 1e-9));

    // distance has scale=100 → 0/100 = 0.0
    let dist = first_record.field("distance").expect("record.distance");
    assert!(matches!(dist.value, Value::Float(v) if v.abs() < 1e-9));
}

#[test]
fn activity_file_id_type_is_enum_string() {
    let bytes = read_fixture("Activity.fit");
    let (msgs, errs) = Decoder::builder(&bytes).build().read_all();
    assert!(errs.is_empty());

    let file_id = &msgs[0];
    assert_eq!(file_id.name, "file_id");

    // type field is `file` enum, value 4 → "activity"
    let ty = file_id.field("type").expect("file_id.type");
    assert_eq!(ty.value, Value::Enum("activity"));
}

#[test]
fn activity_session_sport_resolves_to_string() {
    let bytes = read_fixture("Activity.fit");
    let (msgs, errs) = Decoder::builder(&bytes).build().read_all();
    assert!(errs.is_empty());

    let session = msgs
        .iter()
        .find(|m| m.global_mesg_num == 18)
        .expect("must have a session message");

    // sport field exists and resolves to a known sport string.
    let sport = session.field("sport").expect("session.sport");
    assert!(
        matches!(sport.value, Value::Enum(s) if !s.is_empty()),
        "session.sport should be Enum(...), got {:?}",
        sport.value,
    );
}

// ────────────────────────────────────────────────────────────────────
// Toggle: disabling each transform produces the expected raw shape
// ────────────────────────────────────────────────────────────────────

#[test]
fn disabling_convert_datetime_keeps_raw_uint() {
    let bytes = read_fixture("Activity.fit");
    let (msgs, errs) = Decoder::builder(&bytes)
        .convert_datetime(false)
        .build()
        .read_all();
    assert!(errs.is_empty());

    let first_record = msgs.iter().find(|m| m.global_mesg_num == 20).unwrap();
    let ts = first_record.field("timestamp").unwrap();
    // Without datetime conversion, timestamp is a plain u32-equivalent UInt.
    assert_eq!(ts.value, Value::UInt(995_749_880));
}

#[test]
fn disabling_convert_types_keeps_enums_as_uint() {
    let bytes = read_fixture("Activity.fit");
    let (msgs, errs) = Decoder::builder(&bytes)
        .convert_types_to_strings(false)
        .build()
        .read_all();
    assert!(errs.is_empty());

    let file_id = &msgs[0];
    let ty = file_id.field("type").unwrap();
    assert_eq!(ty.value, Value::UInt(4));
}

#[test]
fn disabling_scale_offset_keeps_raw_integers() {
    let bytes = read_fixture("Activity.fit");
    let (msgs, errs) = Decoder::builder(&bytes)
        .apply_scale_and_offset(false)
        .build()
        .read_all();
    assert!(errs.is_empty());

    let first_record = msgs.iter().find(|m| m.global_mesg_num == 20).unwrap();
    let speed = first_record.field("speed").unwrap();
    // With scale disabled, speed is raw u16.
    assert_eq!(speed.value, Value::UInt(1000));
}

// ────────────────────────────────────────────────────────────────────
// Components — verify the unit-level transform correctness directly.
//
// NOTE: end-to-end Components expansion through `event.data → gear_change_data`
// requires Profile-level metadata that lives on **subfield rows** in
// Profile.xlsx (the components/bits/scale columns of subfield rows are
// currently dropped by `fit-codegen`). Wiring SubField-level components
// through codegen + the runtime is scheduled for M6 alongside the
// developer-field schema work.
//
// Until then we exercise the algorithm itself via the unit tests in
// `transforms::components` and verify that SubField *naming* fires here.
// ────────────────────────────────────────────────────────────────────

#[test]
fn gear_change_fixture_renames_event_data_via_subfield() {
    let bytes = read_fixture("WithGearChangeData.fit");
    let (msgs, errs) = Decoder::builder(&bytes).build().read_all();
    assert!(errs.is_empty(), "got errors: {errs:?}");

    // Find at least one event message whose `data` field has been renamed
    // to `gear_change_data` by SubField resolution (event_type == 0x10/0x11).
    let renamed = msgs
        .iter()
        .any(|m| m.name == "event" && m.fields.iter().any(|f| f.name == "gear_change_data"));
    assert!(
        renamed,
        "event.data must be renamed to `gear_change_data` when event_type is a gear-change kind"
    );
}

// ────────────────────────────────────────────────────────────────────
// SubField — file_id.product is renamed to garmin_product when
// manufacturer == garmin
// ────────────────────────────────────────────────────────────────────

#[test]
fn file_id_product_resolves_to_garmin_subfield_when_applicable() {
    let bytes = read_fixture("WithGearChangeData.fit");
    let (msgs, errs) = Decoder::builder(&bytes).build().read_all();
    assert!(errs.is_empty());

    let file_id = msgs.iter().find(|m| m.name == "file_id").expect("file_id");
    let mfr = file_id.field("manufacturer").expect("manufacturer");

    // If the binary's manufacturer resolves to "garmin", then the product
    // field should be renamed `garmin_product`. (For Activity.fit it's
    // "development" which doesn't match any subfield, so we use this
    // gear-change fixture which is a Garmin file.)
    if mfr.value == Value::Enum("garmin") {
        assert!(
            file_id.field("garmin_product").is_some(),
            "expected garmin_product subfield when manufacturer is garmin"
        );
    }
}

// ────────────────────────────────────────────────────────────────────
// M6: Developer fields are resolved via the registry
// ────────────────────────────────────────────────────────────────────

#[test]
fn dev_fields_are_resolved_not_raw_bytes() {
    let bytes = read_fixture("Activity.fit");
    let (msgs, errs) = Decoder::builder(&bytes).build().read_all();
    assert!(errs.is_empty());

    // With M6, dev fields that have matching field_description messages
    // should be resolved to typed values, not opaque Bytes.
    let dev_fields: Vec<&Field> = msgs
        .iter()
        .flat_map(|m| m.fields.iter())
        .filter(|f| matches!(f.kind, FieldKind::Developer { .. }))
        .collect();

    // Activity.fit has developer fields (from field_description registrations).
    // Verify at least some are resolved (not Bytes).
    let resolved_count = dev_fields
        .iter()
        .filter(|f| !matches!(f.value, Value::Bytes(_)))
        .count();
    assert!(
        resolved_count > 0,
        "expected at least some dev fields to be resolved, got 0 out of {}",
        dev_fields.len()
    );

    // Resolved dev fields should have a non-generic name.
    for f in &dev_fields {
        if !matches!(f.value, Value::Bytes(_)) {
            assert_ne!(
                f.name, "developer_field",
                "resolved dev field should have its real name"
            );
        }
    }
}
