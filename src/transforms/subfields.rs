//! SubField selection — pick the correct semantic interpretation of a
//! field based on the value of *another* field in the same message.
//!
//! Reference: `guide/fit_binary_learning_notes.md` §"SubField 选择算法（重要）".
//!
//! Worked example: the `event` message's `data` field (u32) takes on
//! different meanings depending on `event_type`:
//!
//! ```text
//!   event_type = 0x10 (rear_gear_change)  →  data is gear_change_data
//!   event_type = 0x2A (rider_position)    →  data is rider_position
//!   ...                                   →  data stays as a generic u32
//! ```

use crate::profile::{FieldInfo, MesgInfo, SubField};
use crate::transforms::components::scalar_as_u64;
use crate::transforms::enum_strings::enum_str_by_value;
use crate::RawMessage;

/// Inspect a parent field's `sub_fields` and return the first one whose
/// condition matches the parent message. Returns `None` when no SubField
/// applies (in which case the parent field keeps its default semantics).
pub fn select<'a>(
    parent_field: &'a FieldInfo,
    parent_mesg: &MesgInfo,
    raw_message: &RawMessage<'_>,
) -> Option<&'a SubField> {
    if parent_field.sub_fields.is_empty() {
        return None;
    }
    for sub in parent_field.sub_fields {
        for (ref_name, expected) in sub.conditions {
            if condition_matches(parent_mesg, raw_message, ref_name, expected) {
                return Some(sub);
            }
        }
    }
    None
}

fn condition_matches(
    parent_mesg: &MesgInfo,
    raw_message: &RawMessage<'_>,
    ref_field_name: &str,
    expected: &str,
) -> bool {
    // Find the ref field's metadata in the parent message.
    let Some(ref_field) = parent_mesg.field_by_name(ref_field_name) else {
        return false;
    };
    // Find the ref field's decoded value in the raw message.
    let Some(actual) = raw_message.field(ref_field.field_def_num) else {
        return false;
    };
    let Some(actual_u64) = scalar_as_u64(&actual.value) else {
        return false;
    };

    // Two acceptable forms for the SubField's expected condition value:
    // (1) a numeric literal ("16", "0x10"), or
    // (2) the snake_case name of a value in the ref field's enum type.
    if let Some(expected_int) = parse_int_literal(expected) {
        return actual_u64 == expected_int;
    }
    if let Some(actual_str) = enum_str_by_value(ref_field.type_name, actual_u64) {
        return actual_str == expected;
    }
    false
}

fn parse_int_literal(s: &str) -> Option<u64> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok();
    }
    s.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::generated::messages;
    use crate::raw_value::RawValue;

    #[test]
    fn parse_int_literal_accepts_hex_and_decimal() {
        assert_eq!(parse_int_literal("16"), Some(16));
        assert_eq!(parse_int_literal("0x10"), Some(16));
        assert_eq!(parse_int_literal("0X10"), Some(16));
        assert_eq!(parse_int_literal("running"), None);
    }

    #[test]
    fn file_id_product_subfield_resolves_for_garmin_manufacturer() {
        let file_id = messages::ALL_MESSAGES
            .iter()
            .find(|m| m.name == "file_id")
            .expect("file_id must exist");

        let product_field = file_id
            .field_by_name("product")
            .expect("file_id.product must exist");
        assert!(!product_field.sub_fields.is_empty());

        // Synthesize a RawMessage with manufacturer = 1 (garmin).
        let raw = RawMessage {
            global_mesg_num: 0,
            fields: vec![
                crate::RawField {
                    field_def_num: 1, // manufacturer
                    value: RawValue::U16Scalar(1),
                },
                crate::RawField {
                    field_def_num: 2, // product (the parent of the SubField)
                    value: RawValue::U16Scalar(0),
                },
            ],
            dev_fields: vec![],
            starts_new_chain: false,
        };

        let sub = select(product_field, file_id, &raw)
            .expect("garmin_product subfield must match for manufacturer=1");
        assert_eq!(sub.name, "garmin_product");
    }

    #[test]
    fn returns_none_when_no_condition_matches() {
        let file_id = messages::ALL_MESSAGES
            .iter()
            .find(|m| m.name == "file_id")
            .unwrap();
        let product = file_id.field_by_name("product").unwrap();

        // manufacturer = 999 (not a known value, no subfield matches).
        let raw = RawMessage {
            global_mesg_num: 0,
            fields: vec![crate::RawField {
                field_def_num: 1,
                value: RawValue::U16Scalar(999),
            }],
            dev_fields: vec![],
            starts_new_chain: false,
        };
        assert!(select(product, file_id, &raw).is_none());
    }
}
