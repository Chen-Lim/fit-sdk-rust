//! Streaming FIT decoder — yields one [`RawMessage`] per `next()` call.
//!
//! M4 scope: decode bytes into [`RawValue`]s with invalid-value detection,
//! but **no** Profile-aware transforms (no scale/offset, no enum-string
//! conversion, no DateTime, no SubField/Component expansion). Those land in
//! M5.
//!
//! Multi-FIT chains are handled transparently: when the iterator reaches the
//! end of one chain's data area + 2-byte CRC, it tries to parse another
//! [`FileHeader`] from the remaining bytes and resets the local definition
//! table. See protocol §"多 FIT 链".

use crate::definition::{LocalDefinitions, MessageDefinition};
use crate::error::FitError;
use crate::header::FileHeader;
use crate::raw_value::{decode_value, RawValue};
use crate::record_header::RecordHeader;
use crate::stream::ByteStream;

/// One decoded FIT message.
#[derive(Debug, Clone, PartialEq)]
pub struct RawMessage {
    /// Profile-level message number — index into the codegen-produced
    /// `MesgNum` enum.
    pub global_mesg_num: u16,
    /// Standard fields, in wire order.
    pub fields: Vec<RawField>,
    /// Developer fields, if any. Without a registered `field_description`
    /// (mesg_num=206), the wire bytes are stored verbatim; resolving them
    /// to typed values lands in M6.
    pub dev_fields: Vec<RawDevField>,
    /// `true` when this message is the first Data record after a chained-FIT
    /// boundary. Upper layers (e.g. [`crate::TypedDecoder`]) use this to
    /// reset per-chain state such as the [`crate::transforms::Accumulator`].
    /// Always `false` for the very first message of the first chain.
    pub starts_new_chain: bool,
}

impl RawMessage {
    /// Look up a standard field by its definition number.
    pub fn field(&self, field_def_num: u8) -> Option<&RawField> {
        self.fields.iter().find(|f| f.field_def_num == field_def_num)
    }
}

/// A standard field's decoded value.
#[derive(Debug, Clone, PartialEq)]
pub struct RawField {
    /// Wire-level field definition number.
    pub field_def_num: u8,
    /// Decoded raw value.
    pub value: RawValue,
}

/// A developer field's wire bytes, awaiting M6 schema resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDevField {
    /// Wire-level field definition number.
    pub field_def_num: u8,
    /// Index into the developer data ID table.
    pub developer_data_index: u8,
    /// Raw bytes exactly as they appeared on the wire.
    pub bytes: Vec<u8>,
}

/// The streaming decoder. Implements [`Iterator`] yielding one
/// `Result<RawMessage, FitError>` per Data record.
///
/// On the first call, the iterator parses the [`FileHeader`] and seeks past
/// it. Definition records are absorbed silently into the 16-slot
/// [`LocalDefinitions`] table; only Data records produce items. After a
/// fatal error, all subsequent `next()` calls return `None`.
pub struct Decoder<'a> {
    stream: ByteStream<'a>,
    local_defs: LocalDefinitions,
    /// File offset of the trailing 2-byte CRC for the current chain. Set
    /// after each `parse_header_at_cursor` call.
    chain_crc_offset: Option<usize>,
    /// Whether to run [`Self::parse_header_at_cursor`] before pulling records.
    needs_header: bool,
    /// Once any error fires, the iterator is "drained" — all subsequent
    /// `next()` calls return `None`.
    terminated: bool,
    /// Most recent full timestamp seen (from field_def_num=253 in Data
    /// messages). Used to reconstruct compressed timestamps.
    last_timestamp: Option<u32>,
    /// Set when a chained-FIT boundary was just crossed; consumed by the
    /// next Data record so its [`RawMessage::starts_new_chain`] reports
    /// `true`. Then cleared.
    chain_just_reset: bool,
}

impl<'a> Decoder<'a> {
    /// Construct a decoder over a byte slice. The slice may contain a
    /// single FIT file or several FIT files concatenated end-to-end.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            stream: ByteStream::new(bytes),
            local_defs: LocalDefinitions::new(),
            chain_crc_offset: None,
            needs_header: true,
            terminated: false,
            last_timestamp: None,
            chain_just_reset: false,
        }
    }

    /// Drain the iterator into two vectors: messages and errors. Mirrors the
    /// JS SDK's `read() -> { messages, errors }` shape. On a structural
    /// error the iterator stops, so `errors` will hold at most one entry.
    pub fn read_all(self) -> (Vec<RawMessage>, Vec<FitError>) {
        let mut messages = Vec::new();
        let mut errors = Vec::new();
        for item in self {
            match item {
                Ok(m) => messages.push(m),
                Err(e) => errors.push(e),
            }
        }
        (messages, errors)
    }

    /// Parse a FileHeader at the cursor and seek past it. On success, sets
    /// `chain_crc_offset` to the offset where the trailing CRC lives.
    fn parse_header_at_cursor(&mut self) -> Result<(), FitError> {
        let pos = self.stream.position();
        let bytes = self.stream.as_slice();
        let header = FileHeader::parse(&bytes[pos..])?;
        let header_size = header.header_size as usize;
        let data_size = header.data_size as usize;
        let total = header_size + data_size + 2;
        if bytes.len() - pos < total {
            return Err(FitError::TooShort {
                expected: total,
                actual: bytes.len() - pos,
            });
        }
        self.stream.seek(pos + header_size)?;
        self.chain_crc_offset = Some(pos + header_size + data_size);
        Ok(())
    }

    /// Reconstruct a full timestamp from a 5-bit compressed offset.
    ///
    /// Algorithm per protocol §2.3:
    /// - If `offset >= last_5bits`: same epoch window.
    /// - Else: rollover — epoch window advances by 0x20.
    fn decode_compressed_timestamp(&mut self, timestamp_offset: u8) -> u32 {
        let last_ts = self.last_timestamp.unwrap_or(0);
        let last_5bits = last_ts & 0x1F;
        let offset = u32::from(timestamp_offset);
        let new_ts = if offset >= last_5bits {
            (last_ts & !0x1F) | offset
        } else {
            (last_ts & !0x1F).wrapping_add(0x20) | offset
        };
        self.last_timestamp = Some(new_ts);
        new_ts
    }

    /// Track the timestamp from a decoded Data message, if it has field 253.
    fn track_timestamp(&mut self, msg: &RawMessage) {
        if let Some(f) = msg.field(253) {
            if let Some(ts) = f.value.as_u32() {
                self.last_timestamp = Some(ts);
            }
        }
    }

    /// Decode the body of a Data message keyed by `local_mesg_num`. The
    /// 1-byte record header has already been consumed.
    fn decode_data(&mut self, local_mesg_num: u8) -> Result<RawMessage, FitError> {
        // Snapshot what we need from the definition. `FieldDefinition` is
        // `Copy`, so this is cheap; cloning the `dev_fields` Vec avoids the
        // borrow checker complaining about &self.local_defs vs &mut self.stream.
        let (endian, global_mesg_num, fields, dev_fields) = {
            let def = self.local_defs.require(local_mesg_num)?;
            (def.endian, def.global_mesg_num, def.fields.clone(), def.dev_fields.clone())
        };

        let mut out_fields = Vec::with_capacity(fields.len());
        for f in &fields {
            let raw = self.stream.read_bytes(f.size as usize)?;
            let value = decode_value(f.base_type, raw, endian, f.field_def_num)?;
            out_fields.push(RawField {
                field_def_num: f.field_def_num,
                value,
            });
        }

        let mut out_dev = Vec::with_capacity(dev_fields.len());
        for d in &dev_fields {
            let raw = self.stream.read_bytes(d.size as usize)?.to_vec();
            out_dev.push(RawDevField {
                field_def_num: d.field_def_num,
                developer_data_index: d.developer_data_index,
                bytes: raw,
            });
        }

        let msg = RawMessage {
            global_mesg_num,
            fields: out_fields,
            dev_fields: out_dev,
            starts_new_chain: false,
        };
        self.track_timestamp(&msg);
        Ok(msg)
    }

    /// Decode a compressed-timestamp data message. The timestamp field
    /// (fdn=253) is reconstructed from the compressed offset instead of being
    /// read from the wire. All other fields are decoded normally.
    fn decode_data_compressed(
        &mut self,
        local_mesg_num: u8,
        timestamp_offset: u8,
    ) -> Result<RawMessage, FitError> {
        let (endian, global_mesg_num, fields, dev_fields) = {
            let def = self.local_defs.require(local_mesg_num)?;
            (def.endian, def.global_mesg_num, def.fields.clone(), def.dev_fields.clone())
        };

        let timestamp = self.decode_compressed_timestamp(timestamp_offset);

        let mut out_fields = Vec::with_capacity(fields.len());
        for f in &fields {
            if f.field_def_num == 253 {
                out_fields.push(RawField {
                    field_def_num: 253,
                    value: RawValue::UInt32(vec![timestamp]),
                });
            } else {
                let raw = self.stream.read_bytes(f.size as usize)?;
                let value = decode_value(f.base_type, raw, endian, f.field_def_num)?;
                out_fields.push(RawField {
                    field_def_num: f.field_def_num,
                    value,
                });
            }
        }

        let mut out_dev = Vec::with_capacity(dev_fields.len());
        for d in &dev_fields {
            let raw = self.stream.read_bytes(d.size as usize)?.to_vec();
            out_dev.push(RawDevField {
                field_def_num: d.field_def_num,
                developer_data_index: d.developer_data_index,
                bytes: raw,
            });
        }

        Ok(RawMessage {
            global_mesg_num,
            fields: out_fields,
            dev_fields: out_dev,
            starts_new_chain: false,
        })
    }
}

impl<'a> Iterator for Decoder<'a> {
    type Item = Result<RawMessage, FitError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.terminated {
            return None;
        }

        // First call (or after a chain boundary): parse the FileHeader.
        if self.needs_header {
            self.needs_header = false;
            if let Err(e) = self.parse_header_at_cursor() {
                self.terminated = true;
                return Some(Err(e));
            }
        }

        loop {
            // End of current chain reached?
            if let Some(crc_off) = self.chain_crc_offset {
                if self.stream.position() >= crc_off {
                    // Skip the 2-byte trailing CRC. We don't verify it here —
                    // callers who care can use `fit::check_integrity` first.
                    if self.stream.read_bytes(2).is_err() {
                        self.terminated = true;
                        return None;
                    }
                    // More bytes? Try to start a new chain. If not, we're done.
                    if self.stream.is_empty() {
                        self.terminated = true;
                        return None;
                    }
                    self.local_defs.clear();
                    self.last_timestamp = None;
                    self.chain_crc_offset = None;
                    self.chain_just_reset = true;
                    if let Err(e) = self.parse_header_at_cursor() {
                        self.terminated = true;
                        return Some(Err(e));
                    }
                }
            }

            // Read the record header byte.
            let hb = match self.stream.read_u8() {
                Ok(b) => b,
                Err(e) => {
                    self.terminated = true;
                    return Some(Err(e));
                }
            };

            match RecordHeader::classify(hb) {
                RecordHeader::Definition {
                    local_mesg_num,
                    has_dev_data,
                } => match MessageDefinition::parse(&mut self.stream, has_dev_data) {
                    Ok(def) => {
                        self.local_defs.set(local_mesg_num, def);
                        continue;
                    }
                    Err(e) => {
                        self.terminated = true;
                        return Some(Err(e));
                    }
                },
                RecordHeader::Data { local_mesg_num } => {
                    let mut result = self.decode_data(local_mesg_num);
                    match &mut result {
                        Ok(msg) => msg.starts_new_chain = std::mem::take(&mut self.chain_just_reset),
                        Err(_) => self.terminated = true,
                    }
                    return Some(result);
                }
                RecordHeader::CompressedTimestamp {
                    local_mesg_num,
                    timestamp_offset,
                } => {
                    let mut result = self.decode_data_compressed(local_mesg_num, timestamp_offset);
                    match &mut result {
                        Ok(msg) => msg.starts_new_chain = std::mem::take(&mut self.chain_just_reset),
                        Err(_) => self.terminated = true,
                    }
                    return Some(result);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal but valid 14-byte header for a single in-memory FIT
    /// file with `data_size` records bytes, plus a placeholder CRC pair.
    fn write_fake_fit(records: &[u8]) -> Vec<u8> {
        let data_size = records.len() as u32;
        let mut bytes = vec![14u8, 0x20, 0xD0, 0x52];
        bytes.extend_from_slice(&data_size.to_le_bytes());
        bytes.extend_from_slice(b".FIT");
        bytes.extend_from_slice(&[0, 0]); // header CRC = 0 → skip
        bytes.extend_from_slice(records);
        bytes.extend_from_slice(&[0, 0]); // file CRC placeholder
        bytes
    }

    /// Build a Definition + a single Data record where the Definition has
    /// one u32 LE field (field_def_num=0).
    fn one_u32_record() -> Vec<u8> {
        let mut records = Vec::new();
        // Definition record: header = 0x40 (Definition, local=0, no dev)
        records.push(0x40);
        records.extend_from_slice(&[
            0x00, 0x00, // reserved, arch=LE
            0x00, 0x00, // global_mesg_num = 0
            0x01, // field count
            0x00, 0x04, 0x06, // fdn=0, size=4, base=UInt32
        ]);
        // Data record: header = 0x00 (Data, local=0)
        records.push(0x00);
        records.extend_from_slice(&[0xF8, 0xEF, 0x59, 0x3B]); // 995749880 LE (= 0x3B59EFF8)
        records
    }

    #[test]
    fn decodes_synthetic_one_record_file() {
        let fit = write_fake_fit(&one_u32_record());
        let (msgs, errs) = Decoder::new(&fit).read_all();
        assert!(errs.is_empty(), "got errors: {errs:?}");
        assert_eq!(msgs.len(), 1);
        let m = &msgs[0];
        assert_eq!(m.global_mesg_num, 0);
        assert_eq!(m.fields.len(), 1);
        assert_eq!(m.fields[0].field_def_num, 0);
        assert_eq!(m.fields[0].value.as_u32(), Some(995749880));
    }

    #[test]
    fn data_without_definition_yields_error() {
        // Records: just a Data byte for local_mesg_num=0 with no preceding Definition.
        let bad = vec![0x00, 0x01, 0x02, 0x03, 0x04]; // header + 4 bytes
        let fit = write_fake_fit(&bad);
        let (_msgs, errs) = Decoder::new(&fit).read_all();
        assert_eq!(errs.len(), 1);
        assert!(matches!(errs[0], FitError::UndefinedLocalMesgNum(0)));
    }

    #[test]
    fn compressed_timestamp_without_definition_yields_error() {
        // A compressed-timestamp byte with no prior definition → undefined local mesg.
        let fit = write_fake_fit(&[0x80]);
        let (_msgs, errs) = Decoder::new(&fit).read_all();
        assert_eq!(errs.len(), 1);
        assert!(matches!(errs[0], FitError::UndefinedLocalMesgNum(_)));
    }

    #[test]
    fn malformed_field_definition_is_caught_at_definition() {
        // Definition with a UInt32 field declared as size=3 (not multiple of 4).
        let mut records = Vec::new();
        records.push(0x40);
        records.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x00, 0x01, // header bytes
            0x00, 0x03, 0x06, // fdn=0, size=3, base=UInt32 — INVALID size
        ]);
        let fit = write_fake_fit(&records);
        let (_, errs) = Decoder::new(&fit).read_all();
        assert_eq!(errs.len(), 1);
        assert!(matches!(
            errs[0],
            FitError::MalformedField {
                field_def_num: 0,
                size: 3,
                ..
            }
        ));
    }

    #[test]
    fn read_all_returns_pair() {
        let fit = write_fake_fit(&one_u32_record());
        let (msgs, errs) = Decoder::new(&fit).read_all();
        assert_eq!(msgs.len(), 1);
        assert_eq!(errs.len(), 0);
    }

    #[test]
    fn endian_round_trip_via_definition() {
        // Same record but architecture=BE: bytes should decode the same.
        let mut records = Vec::new();
        records.push(0x40);
        records.extend_from_slice(&[
            0x00, 0x01, // arch = BE
            0x00, 0x00, // global_mesg_num (BE) = 0
            0x01, 0x00, 0x04, 0x86, // fdn=0, size=4, base=UInt32 with BE flag
        ]);
        records.push(0x00); // Data header
        records.extend_from_slice(&[0x3B, 0x59, 0xEF, 0xF8]); // 995749880 BE
        let fit = write_fake_fit(&records);
        let (msgs, errs) = Decoder::new(&fit).read_all();
        assert!(errs.is_empty());
        assert_eq!(msgs[0].fields[0].value.as_u32(), Some(995749880));
    }

    /// Concat two synthetic FIT files; the decoder must clear `local_defs`
    /// at the chain boundary and successfully decode both.
    #[test]
    fn multi_fit_chain_resets_local_defs() {
        let one = write_fake_fit(&one_u32_record());
        let chained: Vec<u8> = one.iter().chain(one.iter()).copied().collect();
        let (msgs, errs) = Decoder::new(&chained).read_all();
        assert!(errs.is_empty(), "got errors: {errs:?}");
        assert_eq!(msgs.len(), 2, "expected one message per chained file");
        for m in &msgs {
            assert_eq!(m.fields[0].value.as_u32(), Some(995749880));
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // Compressed timestamp tests
    // ────────────────────────────────────────────────────────────────────

    /// Build a Definition for a "record" message (global_mesg_num=20) with
    /// two fields: fdn=0 (uint8, heart_rate) and fdn=253 (uint32, timestamp).
    fn record_definition() -> Vec<u8> {
        let mut r = Vec::new();
        // Definition: header=0x40, local=0
        r.push(0x40);
        r.extend_from_slice(&[
            0x00, 0x00, // reserved, arch=LE
            0x14, 0x00, // global_mesg_num = 20 (record) LE
            0x02,       // field count = 2
            0x00, 0x01, 0x02, // fdn=0, size=1, base=uint8 (heart_rate)
            0xFD, 0x04, 0x86, // fdn=253, size=4, base=uint32 (timestamp)
        ]);
        r
    }

    /// LE bytes for a u32 value.
    fn u32_le(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    #[test]
    fn compressed_timestamp_basic() {
        // Definition (local=0): record with hr + timestamp.
        // Data (local=0): timestamp=1000, hr=120. Sets last_timestamp=1000.
        // CompressedTimestamp (local=0, offset=5): timestamp should be
        // (1000 & ~0x1F) | 5 = 997... wait, 1000 = 0x3E8, 0x3E8 & 0x1F = 0x08.
        // offset=5 < last_5bits=8 → rollover: (1000 & !0x1F) + 0x20 | 5
        //   = (1000 - 8) + 32 + 5 = 1029.
        let mut records = Vec::new();
        records.extend_from_slice(&record_definition());
        // Data record: header=0x00 (local=0)
        records.push(0x00);
        records.push(120); // hr
        records.extend_from_slice(&u32_le(1000)); // timestamp
        // CompressedTimestamp: header = 0x80 | (0 << 5) | 5 = 0x85
        //   local=0 (bits 6:5 = 00), offset=5 (bits 4:0 = 00101)
        records.push(0x85);
        records.push(130); // hr (only non-timestamp field)

        let fit = write_fake_fit(&records);
        let (msgs, errs) = Decoder::new(&fit).read_all();
        assert!(errs.is_empty(), "got errors: {errs:?}");
        assert_eq!(msgs.len(), 2);

        // First message: normal data.
        assert_eq!(msgs[0].fields[0].value.as_u8(), Some(120));
        assert_eq!(msgs[0].fields[1].value.as_u32(), Some(1000));

        // Second message: compressed timestamp.
        assert_eq!(msgs[1].fields[0].value.as_u8(), Some(130));
        // timestamp = (1000 & !0x1F) + 0x20 | 5 = 992 + 32 + 5 = 1029
        assert_eq!(msgs[1].fields[1].value.as_u32(), Some(1029));
    }

    #[test]
    fn compressed_timestamp_no_rollover() {
        // last_timestamp = 1000 (0x3E8), last_5bits = 8.
        // offset=10 >= 8 → no rollover: (1000 & !0x1F) | 10 = 992 | 10 = 1002.
        let mut records = Vec::new();
        records.extend_from_slice(&record_definition());
        records.push(0x00); // Data header
        records.push(120);
        records.extend_from_slice(&u32_le(1000));
        // Compressed: header = 0x80 | (0 << 5) | 10 = 0x8A
        records.push(0x8A);
        records.push(125);

        let fit = write_fake_fit(&records);
        let (msgs, errs) = Decoder::new(&fit).read_all();
        assert!(errs.is_empty());
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1].fields[1].value.as_u32(), Some(1002));
    }

    #[test]
    fn compressed_timestamp_rollover() {
        // last_timestamp = 1000, last_5bits = 8.
        // offset=3 < 8 → rollover: (1000 & !0x1F) + 0x20 | 3 = 992 + 32 + 3 = 1027.
        let mut records = Vec::new();
        records.extend_from_slice(&record_definition());
        records.push(0x00);
        records.push(120);
        records.extend_from_slice(&u32_le(1000));
        // Compressed: header = 0x80 | 3 = 0x83
        records.push(0x83);
        records.push(125);

        let fit = write_fake_fit(&records);
        let (msgs, errs) = Decoder::new(&fit).read_all();
        assert!(errs.is_empty());
        assert_eq!(msgs[1].fields[1].value.as_u32(), Some(1027));
    }

    #[test]
    fn compressed_timestamp_chain_of_records() {
        // Multiple compressed records in a row, all advancing time by 1s.
        // Base timestamp: 995749880 (0x3B59EFF8), last_5bits = 0x18 = 24.
        let base: u32 = 995749880;
        let mut records = Vec::new();
        records.extend_from_slice(&record_definition());

        // Data record at base timestamp.
        records.push(0x00);
        records.push(100);
        records.extend_from_slice(&u32_le(base));

        // 5 compressed records, each with offset = last_5bits + 1 (no rollover).
        // After base: last_5bits=24. offsets: 25, 26, 27, 28, 29
        for (i, offset) in (25..30).enumerate() {
            // Each successive offset is >= last_5bits, so no rollover.
            // ts = (prev & !0x1F) | offset
            let expected_ts = (base & !0x1F) | offset;
            records.push(0x80 | offset as u8);
            records.push(100 + i as u8);

            // Verify inline (we'll check after decode too).
            assert_eq!(expected_ts, base - (base & 0x1F) + offset);
        }

        let fit = write_fake_fit(&records);
        let (msgs, errs) = Decoder::new(&fit).read_all();
        assert!(errs.is_empty(), "got errors: {errs:?}");
        assert_eq!(msgs.len(), 6);

        assert_eq!(msgs[0].fields[1].value.as_u32(), Some(base));
        for (i, offset) in (25..30).enumerate() {
            let expected = (base & !0x1F) | offset;
            assert_eq!(
                msgs[i + 1].fields[1].value.as_u32(),
                Some(expected),
                "compressed record {i} with offset {offset}"
            );
        }
    }

    #[test]
    fn compressed_timestamp_rollover_at_boundary() {
        // last_5bits = 31 (max). offset=0 → rollover.
        let ts: u32 = 1023; // 0x3FF, last_5bits = 31
        let mut records = Vec::new();
        records.extend_from_slice(&record_definition());
        records.push(0x00);
        records.push(100);
        records.extend_from_slice(&u32_le(ts));
        // Compressed: header = 0x80 | 0 = 0x80 (offset=0)
        records.push(0x80);
        records.push(110);

        let fit = write_fake_fit(&records);
        let (msgs, errs) = Decoder::new(&fit).read_all();
        assert!(errs.is_empty());
        // offset=0 < last_5bits=31 → rollover: (1023 & !0x1F) + 0x20 | 0 = 1024
        assert_eq!(msgs[1].fields[1].value.as_u32(), Some(1024));
    }

    #[test]
    fn compressed_timestamp_cold_start_no_prior_timestamp() {
        // No Data message before compressed → last_timestamp is None (= 0).
        // Compressed with offset=5: (0 & !0x1F) | 5 = 5.
        let mut records = Vec::new();
        records.extend_from_slice(&record_definition());
        // Compressed: header = 0x85 (local=0, offset=5)
        records.push(0x85);
        records.push(100);

        let fit = write_fake_fit(&records);
        let (msgs, errs) = Decoder::new(&fit).read_all();
        assert!(errs.is_empty());
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].fields[1].value.as_u32(), Some(5));
    }

    #[test]
    fn multi_chain_resets_compressed_state() {
        // Chain 1: Data at ts=1000, then compressed.
        // Chain 2: Data at ts=2000, then compressed — must NOT use chain 1's state.
        let mut chain1 = Vec::new();
        chain1.extend_from_slice(&record_definition());
        chain1.push(0x00);
        chain1.push(100);
        chain1.extend_from_slice(&u32_le(1000));
        // Compressed offset=10: (1000 & !0x1F) | 10 = 992 | 10 = 1002
        chain1.push(0x8A);
        chain1.push(110);

        let mut chain2 = Vec::new();
        chain2.extend_from_slice(&record_definition());
        chain2.push(0x00);
        chain2.push(100);
        chain2.extend_from_slice(&u32_le(2000));
        // Compressed offset=5: (2000 & !0x1F) | 5 = rollover → 2016+32+5? No:
        //   2000 & 0x1F = 16, offset=5 < 16 → rollover: 1984 + 32 + 5 = 2021
        chain2.push(0x85);
        chain2.push(110);

        let fit1 = write_fake_fit(&chain1);
        let fit2 = write_fake_fit(&chain2);
        let chained: Vec<u8> = fit1.iter().chain(fit2.iter()).copied().collect();

        let (msgs, errs) = Decoder::new(&chained).read_all();
        assert!(errs.is_empty(), "got errors: {errs:?}");
        assert_eq!(msgs.len(), 4);

        // Chain 1: compressed = 1002
        assert_eq!(msgs[0].fields[1].value.as_u32(), Some(1000));
        assert_eq!(msgs[1].fields[1].value.as_u32(), Some(1002));

        // Chain 2: first record resets, compressed based on chain 2's ts
        assert_eq!(msgs[2].fields[1].value.as_u32(), Some(2000));
        // 2000 & 0x1F = 16, offset=5 < 16 → rollover
        assert_eq!(msgs[3].fields[1].value.as_u32(), Some(2021));
    }
}
