//! M8 — Encoder integration tests.
//!
//! 1. Round-trip: decode Activity.fit → encode → re-decode → same message count.
//! 2. Field-level round-trip: specific field values survive encode/decode.
//! 3. Synthetic minimal FIT: encode one message, decode back.
//! 4. Integrity: encoded files pass CRC checks.

use std::path::PathBuf;

use fit::{Decoder, Encoder, Field, FieldKind, Message, Value};

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
// 1. Round-trip on Activity.fit
// ────────────────────────────────────────────────────────────────────

#[test]
fn roundtrip_activity_message_count() {
    let bytes = read_fixture("Activity.fit");

    // Decode with all transforms enabled (default).
    let (messages, errors) = Decoder::builder(&bytes).build().read_all();
    assert!(errors.is_empty(), "decode errors: {errors:?}");

    // Encode back to FIT binary.
    let enc = Encoder::new();
    let encoded = enc.encode(&messages).expect("encode must succeed");

    // Verify encoded file passes integrity check.
    fit::check_integrity(&encoded).expect("encoded file must pass CRC check");

    // Re-decode and compare message count.
    let (messages2, errors2) = Decoder::builder(&encoded).build().read_all();
    assert!(errors2.is_empty(), "re-decode errors: {errors2:?}");
    assert_eq!(
        messages.len(),
        messages2.len(),
        "message count must survive round-trip"
    );
}

#[test]
fn roundtrip_activity_message_names() {
    let bytes = read_fixture("Activity.fit");
    let (messages, _errors) = Decoder::builder(&bytes).build().read_all();

    let enc = Encoder::new();
    let encoded = enc.encode(&messages).unwrap();
    let (messages2, _errors2) = Decoder::builder(&encoded).build().read_all();

    // Each message's name must survive the round-trip.
    for (a, b) in messages.iter().zip(messages2.iter()) {
        assert_eq!(a.global_mesg_num, b.global_mesg_num, "mesg_num mismatch");
        assert_eq!(
            a.name, b.name,
            "name mismatch for mesg_num {}",
            a.global_mesg_num
        );
    }
}

#[test]
fn roundtrip_activity_timestamps() {
    let bytes = read_fixture("Activity.fit");
    let (messages, _errors) = Decoder::builder(&bytes).build().read_all();

    let enc = Encoder::new();
    let encoded = enc.encode(&messages).unwrap();
    let (messages2, _errors2) = Decoder::builder(&encoded).build().read_all();

    // Check that all DateTime fields survive.
    for (a, b) in messages.iter().zip(messages2.iter()) {
        for (fa, fb) in a.fields.iter().zip(b.fields.iter()) {
            if let (Value::DateTime(da), Value::DateTime(db)) = (&fa.value, &fb.value) {
                assert_eq!(da, db, "DateTime mismatch in {}", a.name);
            }
        }
    }
}

#[test]
fn roundtrip_activity_is_valid_fit() {
    let bytes = read_fixture("Activity.fit");
    let (messages, _errors) = Decoder::builder(&bytes).build().read_all();

    let enc = Encoder::new();
    let encoded = enc.encode(&messages).unwrap();

    // Basic FIT validation.
    assert!(
        fit::is_fit(&encoded),
        "encoded bytes must be a valid FIT file"
    );
    fit::check_integrity(&encoded).expect("CRC must be valid");
}

// ────────────────────────────────────────────────────────────────────
// 2. Field-level round-trip
// ────────────────────────────────────────────────────────────────────

#[test]
fn roundtrip_specific_field_values() {
    let bytes = read_fixture("Activity.fit");
    let (messages, _errors) = Decoder::builder(&bytes).build().read_all();

    let enc = Encoder::new();
    let encoded = enc.encode(&messages).unwrap();
    let (messages2, _errors2) = Decoder::builder(&encoded).build().read_all();

    // Check first file_id message's type field.
    let file_id_1 = messages.iter().find(|m| m.global_mesg_num == 0);
    let file_id_2 = messages2.iter().find(|m| m.global_mesg_num == 0);
    if let (Some(a), Some(b)) = (file_id_1, file_id_2) {
        assert_eq!(
            a.field("type").map(|f| &f.value),
            b.field("type").map(|f| &f.value),
            "file_id.type must survive round-trip"
        );
    }

    // Check first session's sport.
    let session_1 = messages.iter().find(|m| m.name == "session");
    let session_2 = messages2.iter().find(|m| m.name == "session");
    if let (Some(a), Some(b)) = (session_1, session_2) {
        assert_eq!(
            a.field("sport").map(|f| &f.value),
            b.field("sport").map(|f| &f.value),
            "session.sport must survive round-trip"
        );
    }
}

// ────────────────────────────────────────────────────────────────────
// 3. Synthetic minimal FIT
// ────────────────────────────────────────────────────────────────────

#[cfg(feature = "chrono")]
#[test]
fn encode_single_record_message() {
    use chrono::{TimeZone, Utc};

    let messages = vec![Message {
        global_mesg_num: 0, // file_id
        name: "file_id",
        fields: vec![
            Field {
                name: "type".to_string(),
                kind: FieldKind::Standard { field_def_num: 0 },
                value: Value::Enum("activity".into()),
                units: None,
            },
            Field {
                name: "time_created".to_string(),
                kind: FieldKind::Standard { field_def_num: 3 },
                value: Value::DateTime(Utc.with_ymd_and_hms(2021, 7, 19, 21, 11, 20).unwrap()),
                units: None,
            },
        ],
    }];

    let enc = Encoder::new();
    let encoded = enc.encode(&messages).unwrap();

    assert!(fit::is_fit(&encoded));
    fit::check_integrity(&encoded).expect("CRC must be valid");

    // Re-decode and verify.
    let (decoded, errors) = Decoder::builder(&encoded).build().read_all();
    assert!(errors.is_empty(), "errors: {errors:?}");
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0].name, "file_id");

    let type_field = decoded[0].field("type").unwrap();
    assert_eq!(type_field.value, Value::Enum("activity".into()));
}

#[test]
fn encode_with_uint_fields() {
    let messages = vec![Message {
        global_mesg_num: 0, // file_id
        name: "file_id",
        fields: vec![
            Field {
                name: "type".to_string(),
                kind: FieldKind::Standard { field_def_num: 0 },
                value: Value::Enum("activity".into()),
                units: None,
            },
            Field {
                name: "manufacturer".to_string(),
                kind: FieldKind::Standard { field_def_num: 1 },
                value: Value::UInt(1), // garmin
                units: None,
            },
            Field {
                name: "product".to_string(),
                kind: FieldKind::Standard { field_def_num: 2 },
                value: Value::UInt(3415),
                units: None,
            },
        ],
    }];

    let enc = Encoder::new();
    let encoded = enc.encode(&messages).unwrap();

    fit::check_integrity(&encoded).expect("CRC must be valid");

    let (decoded, errors) = Decoder::builder(&encoded).build().read_all();
    assert!(errors.is_empty(), "errors: {errors:?}");
    assert_eq!(decoded.len(), 1);

    let msg = &decoded[0];
    // manufacturer is an enum-typed field (uint16); the default typed-decoder
    // converts UInt(1) → Enum("garmin"). The encoder's job is to preserve
    // the wire bytes — which it does, as evidenced by the round-trip naming.
    assert_eq!(
        msg.field("manufacturer").unwrap().value,
        Value::Enum("garmin".into())
    );
    // product activates the `garmin_product` SubField (because manufacturer ==
    // garmin); 3415 is not a named garmin_product value, so it stays a UInt.
    assert_eq!(
        msg.field("garmin_product").unwrap().value,
        Value::UInt(3415)
    );
}

#[test]
fn encode_multiple_messages() {
    let messages = vec![
        Message {
            global_mesg_num: 0, // file_id
            name: "file_id",
            fields: vec![Field {
                name: "type".to_string(),
                kind: FieldKind::Standard { field_def_num: 0 },
                value: Value::Enum("activity".into()),
                units: None,
            }],
        },
        Message {
            global_mesg_num: 49, // file_creator
            name: "file_creator",
            fields: vec![Field {
                name: "software_version".to_string(),
                kind: FieldKind::Standard { field_def_num: 0 },
                value: Value::UInt(1),
                units: None,
            }],
        },
    ];

    let enc = Encoder::new();
    let encoded = enc.encode(&messages).unwrap();

    fit::check_integrity(&encoded).expect("CRC must be valid");

    let (decoded, errors) = Decoder::builder(&encoded).build().read_all();
    assert!(errors.is_empty(), "errors: {errors:?}");
    assert_eq!(decoded.len(), 2);
    assert_eq!(decoded[0].name, "file_id");
    assert_eq!(decoded[1].name, "file_creator");
}

// ────────────────────────────────────────────────────────────────────
// 4. Raw decode round-trip (using raw Decoder, not TypedDecoder)
// ────────────────────────────────────────────────────────────────────

#[test]
fn raw_roundtrip_activity() {
    let bytes = read_fixture("Activity.fit");
    let (raw_msgs, errors) = Decoder::new(&bytes).read_all();
    assert!(errors.is_empty(), "raw decode errors: {errors:?}");

    // Encode via typed pipeline (need Messages).
    let (typed_msgs, t_errors) = Decoder::builder(&bytes).build().read_all();
    assert!(t_errors.is_empty());

    let enc = Encoder::new();
    let encoded = enc.encode(&typed_msgs).unwrap();

    // Re-decode raw and compare count.
    let (raw_msgs2, errors2) = Decoder::new(&encoded).read_all();
    assert!(errors2.is_empty(), "raw re-decode errors: {errors2:?}");
    assert_eq!(
        raw_msgs.len(),
        raw_msgs2.len(),
        "raw message count must survive round-trip"
    );
}

// ────────────────────────────────────────────────────────────────────
// 5. Field-level round-trip on Activity.fit
// ────────────────────────────────────────────────────────────────────

/// Compare two Values, allowing a small absolute tolerance for `Float`
/// (which round-trips through scale/offset and integer truncation).
fn values_roughly_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => (x - y).abs() < 1e-6,
        (Value::Array(xs), Value::Array(ys)) if xs.len() == ys.len() => xs
            .iter()
            .zip(ys.iter())
            .all(|(x, y)| values_roughly_equal(x, y)),
        _ => a == b,
    }
}

#[test]
fn roundtrip_activity_field_values_full() {
    let bytes = read_fixture("Activity.fit");
    let (messages, _) = Decoder::builder(&bytes).build().read_all();

    let enc = Encoder::new();
    let encoded = enc.encode(&messages).unwrap();
    let (messages2, errors2) = Decoder::builder(&encoded).build().read_all();
    assert!(errors2.is_empty(), "re-decode errors: {errors2:?}");

    assert_eq!(messages.len(), messages2.len(), "msg count");

    // For every (message, field) pair on the original side, the same field
    // (looked up by name) must decode to a roughly-equal Value after the
    // round-trip. We tolerate fields the encoder cannot represent yet (dev
    // fields, components synthesised at decode time) by skipping them on the
    // original side when they are absent from the round-trip.
    let mut compared = 0usize;
    for (a, b) in messages.iter().zip(messages2.iter()) {
        assert_eq!(a.global_mesg_num, b.global_mesg_num);
        for fa in &a.fields {
            // Skip developer fields (encoder M8 doesn't emit them yet).
            if !matches!(fa.kind, FieldKind::Standard { .. }) {
                continue;
            }
            let Some(fb) = b.field(&fa.name) else {
                continue;
            };
            assert!(
                values_roughly_equal(&fa.value, &fb.value),
                "{}.{}: {:?} vs {:?}",
                a.name,
                fa.name,
                fa.value,
                fb.value,
            );
            compared += 1;
        }
    }
    // Sanity: we must have actually compared a lot of fields.
    assert!(compared > 1000, "only compared {compared} fields");
}

#[test]
fn roundtrip_preserves_record_speed_with_scale() {
    // record.speed has scale=1000 (raw u16 → m/s f64). Verify that a Float
    // value survives encode → decode through the reverse-scale path.
    let bytes = read_fixture("Activity.fit");
    let (messages, _) = Decoder::builder(&bytes).build().read_all();

    let speed_before: Vec<f64> = messages
        .iter()
        .filter(|m| m.name == "record")
        .filter_map(|m| m.field("speed"))
        .filter_map(|f| match &f.value {
            Value::Float(v) => Some(*v),
            _ => None,
        })
        .collect();
    assert!(
        !speed_before.is_empty(),
        "fixture must have at least one record.speed Float"
    );

    let enc = Encoder::new();
    let encoded = enc.encode(&messages).unwrap();
    let (messages2, _) = Decoder::builder(&encoded).build().read_all();

    let speed_after: Vec<f64> = messages2
        .iter()
        .filter(|m| m.name == "record")
        .filter_map(|m| m.field("speed"))
        .filter_map(|f| match &f.value {
            Value::Float(v) => Some(*v),
            _ => None,
        })
        .collect();

    assert_eq!(speed_before.len(), speed_after.len(), "speed count");
    for (a, b) in speed_before.iter().zip(speed_after.iter()) {
        // 1/scale = 1e-3 m/s — round-trip should keep us well within that.
        assert!((a - b).abs() < 1e-3, "speed drift: {a} vs {b}");
    }
}
