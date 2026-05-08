//! Regression tests for accumulator state at chained-FIT boundaries.
//!
//! Without per-chain reset, an 8-bit `record.cycles` counter that ends a
//! first segment at total=200 and starts the second at wire value 50 would
//! emit 306 (= 200 + ((50-200) & 0xFF)) instead of the correct 50. This
//! file constructs a synthetic two-chain stream, decodes it through the
//! typed pipeline, and asserts the second segment's accumulated value is
//! independent of the first.

use fit::{crc16, Decoder, Value};

/// Build one synthetic FIT segment containing a Definition for `record`
/// (mesg_num=20) with a single `cycles` field (fdn=18, uint8, LE) followed
/// by `data_records` Data records carrying the supplied cycles wire values.
fn build_segment(cycles_values: &[u8]) -> Vec<u8> {
    let mut records = Vec::new();
    // Definition: header=0x40 (definition, local=0, no dev), arch=LE,
    //   mesg_num=20 (record), 1 field { fdn=18, size=1, base=uint8 }.
    records.push(0x40);
    records.extend_from_slice(&[
        0x00, 0x00, // reserved, arch=LE
        0x14, 0x00, // global_mesg_num = 20 (record), LE
        0x01, // field count
        18, 0x01, 0x02, // fdn=18, size=1, base=uint8
    ]);
    // Data records: header=0x00 (data, local=0), 1 byte payload each.
    for &v in cycles_values {
        records.push(0x00);
        records.push(v);
    }

    // 14-byte header: data_size = records.len(), header_crc backfilled.
    let data_size = records.len() as u32;
    let mut bytes = Vec::with_capacity(14 + records.len() + 2);
    bytes.push(14u8);
    bytes.push(0x20); // protocol version
    bytes.extend_from_slice(&21200u16.to_le_bytes()); // profile version
    bytes.extend_from_slice(&data_size.to_le_bytes());
    bytes.extend_from_slice(b".FIT");
    let header_crc = crc16(&bytes[..12]);
    bytes.extend_from_slice(&header_crc.to_le_bytes());
    bytes.extend_from_slice(&records);
    let file_crc = crc16(&bytes);
    bytes.extend_from_slice(&file_crc.to_le_bytes());
    bytes
}

#[test]
fn accumulator_resets_at_chained_fit_boundary() {
    // Segment 1: cycles 100, 200 → accumulated 100, 200.
    // Segment 2: cycles 50 → expected accumulated 50 (independent of seg 1).
    //
    // Without the reset, segment 2 would compute
    //   delta = (50 - 200) & 0xFF = 106
    //   accumulated = 200 + 106 = 306
    let seg1 = build_segment(&[100, 200]);
    let seg2 = build_segment(&[50]);
    let chained: Vec<u8> = seg1.iter().chain(seg2.iter()).copied().collect();

    let (messages, errors) = Decoder::builder(&chained).build().read_all();
    assert!(errors.is_empty(), "decode errors: {errors:?}");
    assert_eq!(messages.len(), 3, "expected 3 record messages across 2 segments");

    let cycles: Vec<u64> = messages
        .iter()
        .map(|m| match &m.field("cycles").expect("cycles field").value {
            Value::UInt(v) => *v,
            other => panic!("expected UInt, got {other:?}"),
        })
        .collect();

    assert_eq!(cycles[0], 100, "seg1 first record");
    assert_eq!(cycles[1], 200, "seg1 second record");
    assert_eq!(
        cycles[2], 50,
        "seg2 first record must NOT carry seg1's accumulator state \
         (would be 306 if accumulator state leaked)"
    );
}

#[test]
fn raw_decoder_flags_starts_new_chain_only_at_boundary() {
    let seg1 = build_segment(&[100, 200]);
    let seg2 = build_segment(&[50]);
    let chained: Vec<u8> = seg1.iter().chain(seg2.iter()).copied().collect();

    let (messages, errors) = Decoder::new(&chained).read_all();
    assert!(errors.is_empty(), "decode errors: {errors:?}");
    assert_eq!(messages.len(), 3);

    // Only the first message of segment 2 should be flagged.
    assert!(!messages[0].starts_new_chain, "seg1 msg 0");
    assert!(!messages[1].starts_new_chain, "seg1 msg 1");
    assert!(messages[2].starts_new_chain, "seg2 msg 0 must flag boundary");
}
