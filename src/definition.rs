//! Definition messages and the 16-slot local-definition table.
//!
//! A Definition message declares the wire schema for subsequent Data messages
//! that share the same local message number. Layout (bytes after the
//! 1-byte record header):
//!
//! ```text
//!   Offset  Size  Field
//!   ─────────────────────────────────────────
//!      0     1    Reserved (always 0x00)
//!      1     1    Architecture (0 = LE, 1 = BE)
//!      2     2    Global Message Number (per architecture)
//!      4     1    Field Count N
//!      5+   3×N   Field definitions: [field_def_num, size, base_type_byte]
//!   [if header bit 5 set: developer fields]
//!     ...    1    Developer Field Count M
//!     ...   3×M   Dev fields: [field_def_num, size, developer_data_index]
//! ```
//!
//! Reference: `guide/fit_binary_learning_notes.md` §2.2 + §2.4.

use std::array;

use crate::base_type::BaseType;
use crate::error::FitError;
use crate::stream::{ByteStream, Endian};

/// Maximum number of simultaneously valid local message definitions.
pub const LOCAL_DEFINITION_SLOTS: usize = 16;

/// One field's wire-level shape inside a Definition message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldDefinition {
    /// The Profile-level field identifier.
    pub field_def_num: u8,
    /// Total byte width on the wire. May exceed the BaseType element size
    /// when the field is an array (e.g. `size = 4` with `BaseType::UInt16`
    /// means a length-2 array of u16).
    pub size: u8,
    /// Decoded base type (after masking off the endian flag).
    pub base_type: BaseType,
    /// Raw base-type byte preserved verbatim — useful when round-tripping
    /// in the encoder (M8) and for diagnostics.
    pub base_type_byte: u8,
}

impl FieldDefinition {
    /// Number of array elements implied by `size` and the base-type stride.
    /// Returns 0 when `size` is not a multiple of the element size (malformed
    /// definition); callers may treat this as an error or skip the field.
    pub fn element_count(&self) -> usize {
        let stride = self.base_type.element_size();
        if stride == 0 || (self.size as usize) % stride != 0 {
            0
        } else {
            (self.size as usize) / stride
        }
    }
}

/// One developer-field reference inside a Definition.
///
/// The actual schema (name, units, base type, scale, …) is registered
/// dynamically via the `developer_data_id` (mesg 207) and `field_description`
/// (mesg 206) messages. A decoder must build an
/// `(developer_data_index, field_def_num) → FieldDescription` lookup before
/// decoding Data messages that reference dev fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeveloperFieldDefinition {
    /// Field definition number within the developer data schema.
    pub field_def_num: u8,
    /// Total byte width on the wire.
    pub size: u8,
    /// Index into the developer data ID table identifying the data source.
    pub developer_data_index: u8,
}

/// A complete Definition message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageDefinition {
    /// Profile-level (global) message number — the index into `MesgNum`.
    pub global_mesg_num: u16,
    /// Endianness for multi-byte fields in subsequent Data messages.
    pub endian: Endian,
    /// Standard fields, in wire order.
    pub fields: Vec<FieldDefinition>,
    /// Developer fields, in wire order. Empty unless the definition's record
    /// header had the dev-data bit set.
    pub dev_fields: Vec<DeveloperFieldDefinition>,
    /// Reserved byte preserved verbatim (always `0x00` for compliant files,
    /// but kept so the encoder can reproduce non-compliant input bit-for-bit).
    pub reserved: u8,
}

impl MessageDefinition {
    /// Parse the body of a Definition message from `stream`.
    ///
    /// Assumes the 1-byte record header has already been consumed and that
    /// `has_dev_data` reflects bit 5 of that header.
    pub fn parse(stream: &mut ByteStream<'_>, has_dev_data: bool) -> Result<Self, FitError> {
        let reserved = stream.read_u8()?;
        let architecture = stream.read_u8()?;
        let endian = if architecture == 0 {
            Endian::Little
        } else {
            Endian::Big
        };

        let global_mesg_num = stream.read_u16(endian)?;
        let field_count = stream.read_u8()? as usize;

        let mut fields = Vec::with_capacity(field_count);
        for _ in 0..field_count {
            let field_def_num = stream.read_u8()?;
            let size = stream.read_u8()?;
            let base_type_byte = stream.read_u8()?;
            let base_type = BaseType::from_byte(base_type_byte)?;
            // Eagerly validate that the wire size is compatible with the
            // base type's element stride. STRING and BYTE are stride-1 by
            // construction so any non-zero size is allowed; numeric types
            // must have `size` be a positive multiple of their element size.
            let stride = base_type.element_size();
            if !base_type.is_string()
                && !base_type.is_byte()
                && (size == 0 || (size as usize) % stride != 0)
            {
                return Err(FitError::MalformedField {
                    field_def_num,
                    size,
                    element_size: stride,
                });
            }
            fields.push(FieldDefinition {
                field_def_num,
                size,
                base_type,
                base_type_byte,
            });
        }

        let mut dev_fields = Vec::new();
        if has_dev_data {
            let dev_count = stream.read_u8()? as usize;
            dev_fields.reserve_exact(dev_count);
            for _ in 0..dev_count {
                let field_def_num = stream.read_u8()?;
                let size = stream.read_u8()?;
                let developer_data_index = stream.read_u8()?;
                dev_fields.push(DeveloperFieldDefinition {
                    field_def_num,
                    size,
                    developer_data_index,
                });
            }
        }

        Ok(Self {
            global_mesg_num,
            endian,
            fields,
            dev_fields,
            reserved,
        })
    }

    /// Total bytes consumed by a single Data message (excluding its 1-byte
    /// header) keyed by this definition.
    pub fn data_size(&self) -> usize {
        let core: usize = self.fields.iter().map(|f| f.size as usize).sum();
        let dev: usize = self.dev_fields.iter().map(|f| f.size as usize).sum();
        core + dev
    }

    /// Look up a standard field by its definition number.
    pub fn field(&self, field_def_num: u8) -> Option<&FieldDefinition> {
        self.fields
            .iter()
            .find(|f| f.field_def_num == field_def_num)
    }
}

/// The 16-slot local-definition table.
///
/// Each Definition message stores itself at index `local_mesg_num` (0..=15),
/// overwriting any previous occupant. Each Data message looks up its slot
/// to learn how to parse its bytes.
///
/// At the start of a new chained FIT file (multi-FIT) the table must be
/// fully cleared via [`Self::clear`].
#[derive(Debug, Default)]
pub struct LocalDefinitions {
    slots: [Option<MessageDefinition>; LOCAL_DEFINITION_SLOTS],
}

impl LocalDefinitions {
    /// Create an empty local-definition table.
    pub fn new() -> Self {
        Self {
            slots: array::from_fn(|_| None),
        }
    }

    /// Borrow the definition stored in `local_mesg_num`'s slot, if any.
    pub fn get(&self, local_mesg_num: u8) -> Option<&MessageDefinition> {
        self.slots
            .get(local_mesg_num as usize)
            .and_then(Option::as_ref)
    }

    /// Borrow or error with [`FitError::UndefinedLocalMesgNum`].
    pub fn require(&self, local_mesg_num: u8) -> Result<&MessageDefinition, FitError> {
        self.get(local_mesg_num)
            .ok_or(FitError::UndefinedLocalMesgNum(local_mesg_num))
    }

    /// Install a definition, overwriting any prior occupant.
    pub fn set(&mut self, local_mesg_num: u8, def: MessageDefinition) {
        if let Some(slot) = self.slots.get_mut(local_mesg_num as usize) {
            *slot = Some(def);
        }
    }

    /// Drop every slot. Required at chained-FIT boundaries.
    pub fn clear(&mut self) {
        for slot in self.slots.iter_mut() {
            *slot = None;
        }
    }

    /// Number of currently-occupied slots (≤ 16).
    pub fn occupied(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-written Definition: file_id (mesg_num=0), 5 fields, little-endian.
    fn sample_le_definition_bytes() -> Vec<u8> {
        // After the record header (not included here):
        //   reserved=0x00, architecture=0x00 (LE), global_mesg_num=0x0000 (LE),
        //   field_count=5, then 5 × {fdn, size, base_type}.
        // Fields are made-up but match valid base-type codes.
        vec![
            0x00, 0x00, 0x00, 0x00, 0x05, // reserved, arch, mesg_num LE, field_count
            0x00, 0x01, 0x00, // type: fdn=0, size=1, base=Enum
            0x01, 0x02, 0x84, // manufacturer: fdn=1, size=2, base=UInt16 + endian flag
            0x02, 0x02, 0x84, // product: fdn=2, size=2, base=UInt16 + endian flag
            0x03, 0x04, 0x86, // time_created: fdn=3, size=4, base=UInt32 + endian flag
            0x04, 0x04, 0x8C, // serial_number: fdn=4, size=4, base=UInt32z + endian flag
        ]
    }

    #[test]
    fn parses_canonical_le_definition() {
        let bytes = sample_le_definition_bytes();
        let mut stream = ByteStream::new(&bytes);
        let def = MessageDefinition::parse(&mut stream, false).unwrap();

        assert_eq!(def.global_mesg_num, 0);
        assert_eq!(def.endian, Endian::Little);
        assert_eq!(def.reserved, 0);
        assert_eq!(def.fields.len(), 5);
        assert!(def.dev_fields.is_empty());

        // Field 0: type
        assert_eq!(def.fields[0].field_def_num, 0);
        assert_eq!(def.fields[0].size, 1);
        assert_eq!(def.fields[0].base_type, BaseType::Enum);

        // Field 1: manufacturer (raw byte 0x84 → endian flag set, code 0x04 → UInt16)
        assert_eq!(def.fields[1].base_type, BaseType::UInt16);
        assert!(BaseType::endian_flag_set(def.fields[1].base_type_byte));

        // Field 4: serial_number (uint32z)
        assert_eq!(def.fields[4].base_type, BaseType::UInt32z);
        assert_eq!(def.fields[4].element_count(), 1);

        assert_eq!(def.data_size(), 1 + 2 + 2 + 4 + 4);
    }

    #[test]
    fn parses_big_endian_definition() {
        // Same as LE but architecture = 0x01 and global_mesg_num bytes flipped.
        let bytes = vec![
            0x00, 0x01, 0x00, 0x14, 0x01, // arch=BE, mesg_num=0x0014 (record), field_count=1
            253, 0x04, 0x86, // timestamp: fdn=253, size=4, base=UInt32 (BE flag)
        ];
        let mut stream = ByteStream::new(&bytes);
        let def = MessageDefinition::parse(&mut stream, false).unwrap();
        assert_eq!(def.endian, Endian::Big);
        assert_eq!(def.global_mesg_num, 20);
        assert_eq!(def.fields.len(), 1);
        assert_eq!(def.fields[0].field_def_num, 253);
        assert_eq!(def.fields[0].base_type, BaseType::UInt32);
    }

    #[test]
    fn parses_definition_with_dev_data() {
        let mut bytes = sample_le_definition_bytes();
        bytes.extend_from_slice(&[
            0x02, // dev_count = 2
            0x00, 0x04, 0x00, // dev field 0: fdn=0, size=4, dev_data_index=0
            0x05, 0x02, 0x01, // dev field 1: fdn=5, size=2, dev_data_index=1
        ]);
        let mut stream = ByteStream::new(&bytes);
        let def = MessageDefinition::parse(&mut stream, /* has_dev_data = */ true).unwrap();

        assert_eq!(def.dev_fields.len(), 2);
        assert_eq!(def.dev_fields[0].field_def_num, 0);
        assert_eq!(def.dev_fields[0].developer_data_index, 0);
        assert_eq!(def.dev_fields[1].field_def_num, 5);
        assert_eq!(def.dev_fields[1].developer_data_index, 1);

        // data_size includes both standard and dev fields.
        assert_eq!(def.data_size(), (1 + 2 + 2 + 4 + 4) + (4 + 2));
    }

    #[test]
    fn rejects_unknown_base_type() {
        let bytes = vec![
            0x00, 0x00, 0x00, 0x00, 0x01, //
            0x00, 0x01, 0x1A, // bad base type code 0x1A
        ];
        let mut stream = ByteStream::new(&bytes);
        let err = MessageDefinition::parse(&mut stream, false).unwrap_err();
        assert!(matches!(err, FitError::UnknownBaseType(_, _)));
    }

    #[test]
    fn local_definitions_set_get_clear() {
        let mut tbl = LocalDefinitions::new();
        assert_eq!(tbl.occupied(), 0);
        assert!(tbl.get(0).is_none());

        let bytes = sample_le_definition_bytes();
        let mut stream = ByteStream::new(&bytes);
        let def = MessageDefinition::parse(&mut stream, false).unwrap();
        tbl.set(3, def.clone());
        assert_eq!(tbl.occupied(), 1);
        assert_eq!(tbl.get(3).unwrap().global_mesg_num, 0);

        // Overwriting same slot is allowed.
        tbl.set(3, def);
        assert_eq!(tbl.occupied(), 1);

        // Out-of-range writes are silently ignored (mask in record_header keeps us safe).
        tbl.set(
            99,
            MessageDefinition {
                global_mesg_num: 0,
                endian: Endian::Little,
                fields: Vec::new(),
                dev_fields: Vec::new(),
                reserved: 0,
            },
        );
        assert_eq!(tbl.occupied(), 1);

        tbl.clear();
        assert_eq!(tbl.occupied(), 0);
    }

    #[test]
    fn require_returns_useful_error() {
        let tbl = LocalDefinitions::new();
        let err = tbl.require(7).unwrap_err();
        assert_eq!(err, FitError::UndefinedLocalMesgNum(7));
    }

    #[test]
    fn element_count_handles_arrays_and_misalignment() {
        let single = FieldDefinition {
            field_def_num: 0,
            size: 4,
            base_type: BaseType::UInt32,
            base_type_byte: 0x06,
        };
        assert_eq!(single.element_count(), 1);

        let array = FieldDefinition {
            field_def_num: 0,
            size: 8,
            base_type: BaseType::UInt16,
            base_type_byte: 0x04,
        };
        assert_eq!(array.element_count(), 4);

        // size not a multiple of element size → 0 (signals malformed definition)
        let bad = FieldDefinition {
            field_def_num: 0,
            size: 3,
            base_type: BaseType::UInt32,
            base_type_byte: 0x06,
        };
        assert_eq!(bad.element_count(), 0);
    }
}
