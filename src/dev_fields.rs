//! Developer field schema registry.
//!
//! Collects `developer_data_id` (mesg 207) and `field_description` (mesg 206)
//! messages during decode, then resolves raw dev-field bytes into typed values.

use std::collections::HashMap;

use crate::base_type::BaseType;

/// Runtime description of a single developer field.
#[derive(Debug, Clone)]
pub struct DevFieldInfo {
    /// Human-readable name from `field_description.field_name`.
    pub name: String,
    /// Base type code (raw `fit_base_type_id` byte, masked to `& 0x1F`).
    pub base_type: BaseType,
    /// Scale factor (`None` or `1.0` means identity).
    pub scale: Option<f64>,
    /// Offset (`None` or `0.0` means identity).
    pub offset: Option<f64>,
    /// Display units from `field_description.units`.
    pub units: Option<String>,
}

/// Key for looking up a dev field: `(developer_data_index, field_def_num)`.
type DevFieldKey = (u8, u8);

/// Registry of developer field schemas, populated from `developer_data_id` and
/// `field_description` messages flowing through the typed decoder.
#[derive(Debug, Default, Clone)]
pub struct DevFieldRegistry {
    fields: HashMap<DevFieldKey, DevFieldInfo>,
}

impl DevFieldRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a field description. Called when a `field_description` message
    /// (global_mesg_num=206) flows through the typed decoder.
    #[allow(clippy::too_many_arguments)]
    pub fn register_field(
        &mut self,
        developer_data_index: u8,
        field_definition_number: u8,
        field_name: String,
        fit_base_type_id: u8,
        scale: Option<f64>,
        offset: Option<f64>,
        units: Option<String>,
    ) {
        let base_type = match BaseType::from_byte(fit_base_type_id & BaseType::TYPE_CODE_MASK) {
            Ok(bt) => bt,
            Err(_) => return, // unknown base type — skip silently
        };
        let key = (developer_data_index, field_definition_number);
        self.fields.insert(
            key,
            DevFieldInfo {
                name: field_name,
                base_type,
                scale,
                offset,
                units,
            },
        );
    }

    /// Look up a developer field by `(developer_data_index, field_def_num)`.
    pub fn get(&self, developer_data_index: u8, field_def_num: u8) -> Option<&DevFieldInfo> {
        self.fields.get(&(developer_data_index, field_def_num))
    }

    /// Number of registered fields (for tests).
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Returns `true` if no fields are registered.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

/// Map a [`BaseType`] to the `type_name` string expected by
/// [`crate::transforms::enum_strings::enum_str_by_value`] and
/// `typed_decoder::transform_value`.
pub fn base_type_to_type_name(bt: BaseType) -> &'static str {
    match bt {
        BaseType::Enum => "enum",
        BaseType::SInt8 => "sint8",
        BaseType::UInt8 => "uint8",
        BaseType::SInt16 => "sint16",
        BaseType::UInt16 => "uint16",
        BaseType::SInt32 => "sint32",
        BaseType::UInt32 => "uint32",
        BaseType::String => "string",
        BaseType::Float32 => "float32",
        BaseType::Float64 => "float64",
        BaseType::UInt8z => "uint8z",
        BaseType::UInt16z => "uint16z",
        BaseType::UInt32z => "uint32z",
        BaseType::Byte => "byte",
        BaseType::SInt64 => "sint64",
        BaseType::UInt64 => "uint64",
        BaseType::UInt64z => "uint64z",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_lookup() {
        let mut reg = DevFieldRegistry::new();
        reg.register_field(
            0,
            0,
            "heart_rate".to_string(),
            0x02, // UInt8
            None,
            None,
            Some("bpm".to_string()),
        );
        let info = reg.get(0, 0).unwrap();
        assert_eq!(info.name, "heart_rate");
        assert_eq!(info.base_type, BaseType::UInt8);
        assert_eq!(info.units.as_deref(), Some("bpm"));
    }

    #[test]
    fn unknown_base_type_is_skipped() {
        let mut reg = DevFieldRegistry::new();
        reg.register_field(0, 0, "bad".to_string(), 0xFF, None, None, None);
        assert!(reg.is_empty());
    }

    #[test]
    fn base_type_to_type_name_round_trip() {
        for code in 0x00..=0x10 {
            let bt = BaseType::from_byte(code).unwrap();
            let name = base_type_to_type_name(bt);
            assert!(!name.is_empty());
        }
    }
}
