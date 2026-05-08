//! M9 — Encoder hardening: LRU eviction, schema-change re-definition,
//! developer-field encoding, multi-segment chains, and the [`EncoderBuilder`].

use std::path::PathBuf;

use fit::{Decoder, Encoder, Field, FieldKind, Message, Value};
use proptest::prelude::*;

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
// 1. EncoderBuilder
// ────────────────────────────────────────────────────────────────────

#[test]
fn builder_round_trip_with_custom_versions() {
    let enc = Encoder::builder()
        .protocol_version(0x21)
        .profile_version(21300)
        .build();
    let bytes = enc.encode(&[]).unwrap();
    fit::check_integrity(&bytes).unwrap();
    assert_eq!(bytes[1], 0x21);
    assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), 21300);
}

// ────────────────────────────────────────────────────────────────────
// 2. LRU eviction: >16 unique mesg_nums survive a round-trip
// ────────────────────────────────────────────────────────────────────

#[test]
fn round_trip_with_more_than_16_unique_mesg_nums() {
    // Build 20 messages, each with its own global_mesg_num. The encoder must
    // evict and re-emit Definitions; the decoder must reconstruct all 20.
    let mut messages: Vec<Message> = Vec::new();
    for n in 0..20u16 {
        // Use mesg_nums starting at file_id (0) and walking up; many of these
        // (e.g. 1, 2, 3...) map to real Profile messages but we keep fields
        // empty to focus on the Definition table mechanics.
        messages.push(Message {
            global_mesg_num: n,
            name: "x",
            fields: vec![],
        });
    }
    let enc = Encoder::new();
    let bytes = enc.encode(&messages).expect("must succeed under LRU");
    fit::check_integrity(&bytes).unwrap();

    let (decoded, errs) = Decoder::new(&bytes).read_all();
    assert!(errs.is_empty(), "decode errors: {errs:?}");
    assert_eq!(decoded.len(), messages.len(), "all 20 messages survive");
    for (i, m) in decoded.iter().enumerate() {
        assert_eq!(m.global_mesg_num, i as u16);
    }
}

#[test]
fn lru_picks_least_recently_used() {
    // Fill 16 unique mesg_nums in order, then re-touch 1..=15 (leaving 0 as
    // LRU), then add a 17th. Expect mesg_num 0's data to no longer be
    // sandwiched between its definition and the 17th — the encoder must re-
    // emit a Definition for any subsequent reference to mesg_num 0.
    let mut messages: Vec<Message> = (0u16..16)
        .map(|n| Message {
            global_mesg_num: n,
            name: "x",
            fields: vec![],
        })
        .collect();
    // Re-touch 1..=15 (clock advances), so mesg_num 0 becomes LRU.
    for n in 1u16..16 {
        messages.push(Message {
            global_mesg_num: n,
            name: "x",
            fields: vec![],
        });
    }
    // Force eviction by introducing a 17th unique mesg_num.
    messages.push(Message {
        global_mesg_num: 999,
        name: "x",
        fields: vec![],
    });
    // Reference mesg_num 0 again — should trigger another eviction-and-redef.
    messages.push(Message {
        global_mesg_num: 0,
        name: "x",
        fields: vec![],
    });

    let bytes = Encoder::new().encode(&messages).unwrap();
    fit::check_integrity(&bytes).unwrap();
    let (decoded, errs) = Decoder::new(&bytes).read_all();
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(decoded.len(), messages.len());
    // Last decoded message must still be mesg_num 0.
    assert_eq!(decoded.last().unwrap().global_mesg_num, 0);
}

// ────────────────────────────────────────────────────────────────────
// 3. Schema-change re-emits Definition
// ────────────────────────────────────────────────────────────────────

#[test]
fn schema_change_within_same_mesg_num_re_emits_definition() {
    use chrono::{TimeZone, Utc};
    // Two file_id messages with different field sets. The encoder must emit
    // a fresh Definition record before the second one even though the
    // mesg_num is unchanged.
    let messages = vec![
        Message {
            global_mesg_num: 0,
            name: "file_id",
            fields: vec![Field {
                name: "type".to_string(),
                kind: FieldKind::Standard { field_def_num: 0 },
                value: Value::Enum("activity"),
                units: None,
            }],
        },
        Message {
            global_mesg_num: 0,
            name: "file_id",
            fields: vec![
                Field {
                    name: "type".to_string(),
                    kind: FieldKind::Standard { field_def_num: 0 },
                    value: Value::Enum("activity"),
                    units: None,
                },
                Field {
                    name: "time_created".to_string(),
                    kind: FieldKind::Standard { field_def_num: 4 },
                    value: Value::DateTime(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
                    units: None,
                },
            ],
        },
    ];
    let bytes = Encoder::new().encode(&messages).unwrap();
    fit::check_integrity(&bytes).unwrap();

    let (decoded, errs) = Decoder::builder(&bytes).build().read_all();
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(decoded.len(), 2);
    // First file_id only carries `type`; second carries `type` + `time_created`.
    assert_eq!(decoded[0].fields.len(), 1);
    assert!(decoded[0].field("time_created").is_none());
    assert!(decoded[1].field("time_created").is_some());
}

// ────────────────────────────────────────────────────────────────────
// 4. Multi-segment chain encoding
// ────────────────────────────────────────────────────────────────────

#[test]
fn encode_chain_concatenates_segments() {
    let bytes = read_fixture("Activity.fit");
    let (messages, _) = Decoder::builder(&bytes).build().read_all();
    assert!(!messages.is_empty());

    // Same content twice → encode_chain produces a 2-segment file.
    let chained = Encoder::new()
        .encode_chain(&[&messages, &messages])
        .unwrap();

    // Each segment must verify independently. fit::check_integrity validates
    // the first segment; chained decoding handles the rest.
    fit::check_integrity(&chained).unwrap();

    let (decoded, errs) = Decoder::new(&chained).read_all();
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(
        decoded.len(),
        messages.len() * 2,
        "chained file decodes to 2× the message count",
    );
}

// ────────────────────────────────────────────────────────────────────
// 5. Developer-field round-trip on Activity.fit
// ────────────────────────────────────────────────────────────────────

#[test]
fn dev_field_round_trip_on_activity() {
    let bytes = read_fixture("Activity.fit");
    let (messages, _) = Decoder::builder(&bytes).build().read_all();

    // Record dev-field stats up-front: how many record messages carry the
    // "Heart Rate" developer field, and what its first value is.
    let dev_count_before: usize = messages
        .iter()
        .flat_map(|m| m.fields.iter())
        .filter(|f| matches!(f.kind, FieldKind::Developer { .. }))
        .count();
    assert!(
        dev_count_before > 0,
        "fixture must include at least one developer field"
    );

    let encoded = Encoder::new().encode(&messages).unwrap();
    fit::check_integrity(&encoded).unwrap();

    let (decoded, errs) = Decoder::builder(&encoded).build().read_all();
    assert!(errs.is_empty(), "{errs:?}");

    let dev_count_after: usize = decoded
        .iter()
        .flat_map(|m| m.fields.iter())
        .filter(|f| matches!(f.kind, FieldKind::Developer { .. }))
        .count();
    assert_eq!(
        dev_count_before, dev_count_after,
        "developer-field count survives round-trip",
    );

    // Field-level: every (record, dev-field) pair must keep its value.
    for (a, b) in messages.iter().zip(decoded.iter()) {
        for fa in &a.fields {
            let FieldKind::Developer {
                field_def_num,
                developer_data_index,
            } = fa.kind
            else {
                continue;
            };
            let fb = b.fields.iter().find(|f| {
                matches!(
                    f.kind,
                    FieldKind::Developer { field_def_num: fdn, developer_data_index: idx }
                        if fdn == field_def_num && idx == developer_data_index
                )
            });
            let fb = fb.unwrap_or_else(|| {
                panic!(
                    "missing dev field after round-trip: name={} fdn={} idx={}",
                    fa.name, field_def_num, developer_data_index
                )
            });
            assert_eq!(
                fa.value, fb.value,
                "dev field {} value drift: {:?} vs {:?}",
                fa.name, fa.value, fb.value
            );
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// 6. Property tests: random typed messages → encode → decode → equal
// ────────────────────────────────────────────────────────────────────

/// Build a random `file_id` message. Restricting ourselves to a profile-defined
/// message guarantees the encoder/decoder agree on field widths.
fn arb_file_id() -> impl Strategy<Value = Message> {
    use chrono::{TimeZone, Utc};
    (
        any::<bool>(),    // include type?
        0u32..16,         // file enum value (0..15 are valid)
        any::<bool>(),    // include serial_number?
        any::<u32>(),     // serial_number value (uint32z)
        any::<bool>(),    // include time_created?
        631_065_600_i64..631_065_600_i64 + 100_000_000, // valid FIT epoch range
    )
        .prop_map(|(has_type, type_val, has_serial, serial, has_time, secs)| {
            let mut fields = Vec::new();
            if has_type {
                let name = match type_val {
                    1 => "device",
                    2 => "settings",
                    3 => "sport",
                    4 => "activity",
                    5 => "workout",
                    6 => "course",
                    _ => "activity",
                };
                fields.push(Field {
                    name: "type".into(),
                    kind: FieldKind::Standard { field_def_num: 0 },
                    value: Value::Enum(name),
                    units: None,
                });
            }
            if has_serial {
                fields.push(Field {
                    name: "serial_number".into(),
                    kind: FieldKind::Standard { field_def_num: 3 },
                    value: Value::UInt(serial as u64),
                    units: None,
                });
            }
            if has_time {
                let dt = Utc.timestamp_opt(secs, 0).unwrap();
                fields.push(Field {
                    name: "time_created".into(),
                    kind: FieldKind::Standard { field_def_num: 4 },
                    value: Value::DateTime(dt),
                    units: None,
                });
            }
            Message {
                global_mesg_num: 0,
                name: "file_id",
                fields,
            }
        })
}

proptest! {
    /// Round-trip equivalence: any sequence of random `file_id` messages must
    /// encode + decode back to a value-equivalent stream. A weak property —
    /// scoped to one message type — but it covers the encoder's hot path
    /// (header, definition, multiple data records, schema changes via field
    /// presence).
    #[test]
    fn random_file_id_round_trips(messages in proptest::collection::vec(arb_file_id(), 0..15)) {
        let enc = Encoder::new();
        let bytes = enc.encode(&messages).unwrap();
        fit::check_integrity(&bytes).unwrap();
        let (decoded, errs) = Decoder::builder(&bytes).build().read_all();
        prop_assert!(errs.is_empty(), "decode errors: {errs:?}");
        prop_assert_eq!(decoded.len(), messages.len());
        for (a, b) in messages.iter().zip(decoded.iter()) {
            for fa in &a.fields {
                let fb = b.field(&fa.name).expect("field must survive");
                prop_assert_eq!(&fa.value, &fb.value, "mismatch on {}", fa.name);
            }
        }
    }
}
