//! The 1-byte Record Header that prefixes every record in a FIT file.
//!
//! Layout:
//!
//! ```text
//!   Bit 7  Bit 6  Bit 5  Bits [4:0]
//!   ─────────────────────────────────────────────────────────
//!     1     T      T     Timestamp[2:0]   → CompressedTimestamp data
//!     0     1      D     LocalMsgNum      → Definition (D=1: contains dev fields)
//!     0     0      —     LocalMsgNum      → Data
//! ```
//!
//! In the Definition / Data forms, `LocalMsgNum` is bits `[3:0]` (mask
//! `0x0F`, range 0..=15 — the 16-slot table).
//!
//! In the CompressedTimestamp form, `LocalMsgNum` is only 2 bits at `[6:5]`
//! (range 0..=3, so compressed records can only reference the first four
//! local definitions).
//!
//! Reference: `guide/fit_binary_learning_notes.md` §2.1.

/// Classification of a single record-header byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordHeader {
    /// Definition message — declares the schema for one of the 16 local slots.
    Definition {
        /// Local message number (0..=15).
        local_mesg_num: u8,
        /// True when bit 5 is set: Definition is followed by developer-field
        /// definitions in addition to the standard fields.
        has_dev_data: bool,
    },
    /// Data message — payload follows, parsed against the Definition stored
    /// in `local_mesg_num`'s slot.
    Data {
        /// Local message number (0..=15).
        local_mesg_num: u8,
    },
    /// Compressed-timestamp data message. Bit 7 = 1; only 2 bits available
    /// for `local_mesg_num`, 5 bits for the timestamp delta against the most
    /// recent full timestamp seen.
    CompressedTimestamp {
        /// Local message number (0..=3, 2-bit field).
        local_mesg_num: u8,
        /// Timestamp delta in seconds against the last full timestamp (0..=31).
        timestamp_offset: u8,
    },
}

impl RecordHeader {
    /// Bit 7. When set, this is a CompressedTimestamp data record.
    pub const COMPRESSED_TIMESTAMP_MASK: u8 = 0x80;
    /// Bit 6 (only meaningful when bit 7 = 0). When set, this is a Definition.
    pub const DEFINITION_MASK: u8 = 0x40;
    /// Bit 5 (only meaningful when bit 6 = 1). When set, the Definition
    /// contains developer-field declarations.
    pub const DEV_DATA_MASK: u8 = 0x20;
    /// Bits `[3:0]`. Local message number for Definition / Data records.
    pub const LOCAL_MESG_NUM_MASK: u8 = 0x0F;
    /// Bits `[6:5]`. Local message number when the compressed-timestamp bit is set.
    pub const COMPRESSED_LOCAL_MASK: u8 = 0x60;
    /// Bits `[4:0]`. Timestamp delta for compressed-timestamp records (0..=31 seconds).
    pub const TIMESTAMP_OFFSET_MASK: u8 = 0x1F;

    /// Decode a single record-header byte.
    pub fn classify(byte: u8) -> Self {
        if byte & Self::COMPRESSED_TIMESTAMP_MASK != 0 {
            Self::CompressedTimestamp {
                local_mesg_num: (byte & Self::COMPRESSED_LOCAL_MASK) >> 5,
                timestamp_offset: byte & Self::TIMESTAMP_OFFSET_MASK,
            }
        } else if byte & Self::DEFINITION_MASK != 0 {
            Self::Definition {
                local_mesg_num: byte & Self::LOCAL_MESG_NUM_MASK,
                has_dev_data: byte & Self::DEV_DATA_MASK != 0,
            }
        } else {
            Self::Data {
                local_mesg_num: byte & Self::LOCAL_MESG_NUM_MASK,
            }
        }
    }

    /// Local message number, common to all three record types.
    pub fn local_mesg_num(&self) -> u8 {
        match self {
            Self::Definition { local_mesg_num, .. }
            | Self::Data { local_mesg_num }
            | Self::CompressedTimestamp { local_mesg_num, .. } => *local_mesg_num,
        }
    }

    /// True iff this is a Definition with the developer-data flag set.
    pub fn has_dev_data(&self) -> bool {
        matches!(self, Self::Definition { has_dev_data: true, .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_data_record() {
        // 0b0000_0000 — bit 7 = 0, bit 6 = 0 → Data, local_mesg_num = 0
        let h = RecordHeader::classify(0x00);
        assert_eq!(h, RecordHeader::Data { local_mesg_num: 0 });

        // 0b0000_0101 — Data with local_mesg_num = 5
        let h = RecordHeader::classify(0x05);
        assert_eq!(h, RecordHeader::Data { local_mesg_num: 5 });

        // 0b0000_1111 — Data with local_mesg_num = 15
        let h = RecordHeader::classify(0x0F);
        assert_eq!(h, RecordHeader::Data { local_mesg_num: 15 });
    }

    #[test]
    fn classifies_definition_record() {
        // 0b0100_0000 — Definition, no dev data, local_mesg_num = 0
        let h = RecordHeader::classify(0x40);
        assert_eq!(
            h,
            RecordHeader::Definition {
                local_mesg_num: 0,
                has_dev_data: false
            }
        );

        // 0b0100_0011 — Definition, no dev, local = 3
        let h = RecordHeader::classify(0x43);
        assert_eq!(
            h,
            RecordHeader::Definition {
                local_mesg_num: 3,
                has_dev_data: false
            }
        );
    }

    #[test]
    fn classifies_definition_with_dev_data() {
        // 0b0110_0000 — Definition + dev data flag, local = 0
        let h = RecordHeader::classify(0x60);
        assert_eq!(
            h,
            RecordHeader::Definition {
                local_mesg_num: 0,
                has_dev_data: true
            }
        );
        assert!(h.has_dev_data());

        // 0b0110_1010 — Definition + dev, local = 10
        let h = RecordHeader::classify(0x6A);
        assert_eq!(
            h,
            RecordHeader::Definition {
                local_mesg_num: 10,
                has_dev_data: true
            }
        );
    }

    #[test]
    fn classifies_compressed_timestamp() {
        // 0b1000_0000 — compressed, local = 0, offset = 0
        let h = RecordHeader::classify(0x80);
        assert_eq!(
            h,
            RecordHeader::CompressedTimestamp {
                local_mesg_num: 0,
                timestamp_offset: 0
            }
        );

        // 0b1110_1010 — compressed, local = 3 (bits 6:5 = 11), offset = 10 (bits 4:0 = 01010)
        let h = RecordHeader::classify(0xEA);
        assert_eq!(
            h,
            RecordHeader::CompressedTimestamp {
                local_mesg_num: 3,
                timestamp_offset: 10
            }
        );

        // 0b1011_1111 — compressed, local = 1 (bits 6:5 = 01), offset = 31 (max)
        let h = RecordHeader::classify(0xBF);
        assert_eq!(
            h,
            RecordHeader::CompressedTimestamp {
                local_mesg_num: 1,
                timestamp_offset: 31
            }
        );
    }

    #[test]
    fn local_mesg_num_accessor() {
        assert_eq!(
            RecordHeader::Definition {
                local_mesg_num: 7,
                has_dev_data: false
            }
            .local_mesg_num(),
            7
        );
        assert_eq!(RecordHeader::Data { local_mesg_num: 12 }.local_mesg_num(), 12);
        assert_eq!(
            RecordHeader::CompressedTimestamp {
                local_mesg_num: 2,
                timestamp_offset: 5
            }
            .local_mesg_num(),
            2
        );
    }

    #[test]
    fn dev_data_only_set_for_definition() {
        assert!(!RecordHeader::Data { local_mesg_num: 0 }.has_dev_data());
        assert!(!RecordHeader::CompressedTimestamp {
            local_mesg_num: 0,
            timestamp_offset: 0
        }
        .has_dev_data());
    }
}
