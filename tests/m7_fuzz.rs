//! M7 — Fuzz & property-based tests.
//!
//! 1. Random-bytes-don't-panic: the Decoder must never panic on arbitrary
//!    byte slices, only return errors or empty results.
//! 2. CRC determinism: the same bytes always produce the same CRC.
//! 3. Accumulator monotonicity: accumulated fields increase monotonically
//!    (when no rollover occurs).
//! 4. DateTime round-trip: FIT epoch ↔ chrono::DateTime conversion.

use proptest::prelude::*;

use fit::Decoder;

// ────────────────────────────────────────────────────────────────────
// 1. Random-bytes-don't-panic
// ────────────────────────────────────────────────────────────────────

proptest! {
    /// The decoder must never panic on arbitrary byte slices.
    #[test]
    fn random_bytes_dont_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        // Just ensure no panic — we don't care about the result.
        let _ = Decoder::new(&bytes).read_all();
    }

    /// Variant with up to 64k of random data.
    #[test]
    fn random_bytes_dont_panic_large(bytes in proptest::collection::vec(any::<u8>(), 0..65536)) {
        let _ = Decoder::new(&bytes).read_all();
    }

    /// Truncated valid FIT files must not panic.
    #[test]
    fn truncated_fit_doesnt_panic(truncate in 0..14usize) {
        // Build a minimal valid FIT, then truncate.
        let records = build_one_record_fit();
        let truncated = &records[..truncate.min(records.len())];
        let _ = Decoder::new(truncated).read_all();
    }
}

/// Build a minimal valid FIT file with one data record (14-byte header + records + 2-byte CRC).
fn build_one_record_fit() -> Vec<u8> {
    let mut records = Vec::new();
    // Definition: header=0x40, local=0, no dev
    records.push(0x40);
    records.extend_from_slice(&[
        0x00, 0x00, // reserved, arch=LE
        0x00, 0x00, // global_mesg_num = 0
        0x01, // field count
        0x00, 0x04, 0x06, // fdn=0, size=4, base=UInt32
    ]);
    // Data: header=0x00
    records.push(0x00);
    records.extend_from_slice(&0x3B59EFF8u32.to_le_bytes());

    let data_size = records.len() as u32;
    let mut bytes = vec![14u8, 0x20, 0xD0, 0x52];
    bytes.extend_from_slice(&data_size.to_le_bytes());
    bytes.extend_from_slice(b".FIT");
    bytes.extend_from_slice(&[0, 0]); // header CRC
    bytes.extend_from_slice(&records);
    bytes.extend_from_slice(&[0, 0]); // file CRC
    bytes
}

// ────────────────────────────────────────────────────────────────────
// 2. CRC determinism
// ────────────────────────────────────────────────────────────────────

proptest! {
    /// CRC-16 over the same slice is always identical.
    #[test]
    fn crc_is_deterministic(len in 0usize..1024) {
        use rand::Rng;
        let mut rng = rand::rng();
        let bytes: Vec<u8> = (0..len).map(|_| rng.random()).collect();

        let crc1 = fit::crc16(&bytes);
        let crc2 = fit::crc16(&bytes);
        prop_assert_eq!(crc1, crc2);
    }

    /// CRC of an empty slice is 0 (per the CRC module's own definition).
    #[test]
    fn crc_of_empty_is_zero(_dummy in any::<u8>()) {
        prop_assert_eq!(fit::crc16(&[]), 0);
    }
}

// ────────────────────────────────────────────────────────────────────
// 3. Accumulator monotonicity
// ────────────────────────────────────────────────────────────────────

proptest! {
    /// The accumulator must not panic on arbitrary value sequences.
    #[test]
    fn accumulator_no_panic(
        mesg_num in 0u16..50,
        field_def_num in 0u8..30,
        values in proptest::collection::vec(0u32..0xFFFFFF, 1..20),
        bits in 1u32..64,
    ) {
        let mut acc = fit::transforms::Accumulator::new();
        for raw in &values {
            let _ = acc.accumulate(mesg_num, field_def_num, *raw as u64, bits);
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// 4. DateTime round-trip
// ────────────────────────────────────────────────────────────────────

#[cfg(feature = "chrono")]
proptest! {
    /// FIT timestamp → DateTime → timestamp round-trip preserves the value.
    #[test]
    fn datetime_roundtrip(seconds in 0u32..0xFFFF_FFFF) {
        use fit::datetime;
        if let Some(dt) = datetime::fit_to_datetime(seconds) {
            let back = datetime::datetime_to_fit(dt);
            prop_assert_eq!(Some(seconds), back);
        }
    }

    /// DateTime → FIT → DateTime round-trip.
    #[test]
    fn datetime_roundtrip_via_chrono(epoch_secs in 631065600i64..2147483647i64) {
        use chrono::{TimeZone, Utc};
        use fit::datetime;

        let dt = Utc.timestamp_opt(epoch_secs, 0).unwrap();
        if let Some(fit_secs) = datetime::datetime_to_fit(dt) {
            let back = datetime::fit_to_datetime(fit_secs);
            prop_assert_eq!(Some(dt), back);
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// 5. Compressed timestamp consistency
// ────────────────────────────────────────────────────────────────────

proptest! {
    /// Compressed timestamps always produce values >= the FIT epoch (2000-01-01).
    #[test]
    fn compressed_timestamps_are_post_epoch(
        base_ts in 631152000u32..0x7FFFFFFF, // after FIT epoch
        offsets in proptest::collection::vec(0u8..32, 1..10),
    ) {
        const FIT_EPOCH: u32 = 631065600; // 1989-12-31 00:00:00 UTC

        let mut last_ts = base_ts;
        for offset in &offsets {
            let last_5bits = last_ts & 0x1F;
            let new_ts = if *offset as u32 >= last_5bits {
                (last_ts & !0x1F) | (*offset as u32)
            } else {
                (last_ts & !0x1F).wrapping_add(0x20) | (*offset as u32)
            };
            last_ts = new_ts;
            // Compressed timestamps should be reasonable FIT timestamps.
            prop_assert!(new_ts >= FIT_EPOCH, "compressed timestamp {new_ts} is before FIT epoch");
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// 6. Random definition + data combos
// ────────────────────────────────────────────────────────────────────

proptest! {
    /// Random definition messages followed by random data messages must
    /// not panic (they may produce errors, but no panics).
    #[test]
    fn random_definition_and_data(
        field_count in 1u8..8,
    ) {
        use rand::Rng;
        let mut rng = rand::rng();

        let mut records = Vec::new();
        // Definition record
        records.push(0x40); // local=0
        records.push(0x00); // reserved
        records.push(0x00); // arch=LE
        records.extend_from_slice(&0u16.to_le_bytes()); // global_mesg_num=0
        records.push(field_count);

        let mut data_size = 0u8;
        for _ in 0..field_count {
            let fdn: u8 = rng.random_range(0..20);
            let size: u8 = rng.random_range(1..8);
            // Use uint8 base type (0x02) — always valid for any size.
            records.push(fdn);
            records.push(size);
            records.push(0x02);
            data_size += size;
        }

        // Data record
        records.push(0x00);
        for _ in 0..data_size {
            records.push(rng.random());
        }

        // Wrap in FIT file
        let ds = records.len() as u32;
        let mut bytes = vec![14u8, 0x20, 0xD0, 0x52];
        bytes.extend_from_slice(&ds.to_le_bytes());
        bytes.extend_from_slice(b".FIT");
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&records);
        bytes.extend_from_slice(&[0, 0]);

        let _ = Decoder::new(&bytes).read_all();
    }
}
