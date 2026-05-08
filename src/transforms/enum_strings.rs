//! Resolve a numeric raw value to its canonical snake_case enum name.
//!
//! The actual lookup is provided by codegen in
//! `crate::profile::generated::types::enum_str_by_value` (one match arm per
//! enum type). This module is a thin re-export with documentation.

pub use crate::profile::generated::types::base_type_for_type_name;
pub use crate::profile::generated::types::enum_str_by_value;
pub use crate::profile::generated::types::enum_value_by_str;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_known_values() {
        assert_eq!(enum_str_by_value("sport", 1), Some("running"));
        assert_eq!(enum_str_by_value("manufacturer", 1), Some("garmin"));
        assert_eq!(enum_str_by_value("file", 4), Some("activity"));
    }

    #[test]
    fn dispatches_through_mesg_num() {
        assert_eq!(enum_str_by_value("mesg_num", 20), Some("record"));
        assert_eq!(enum_str_by_value("mesg_num", 0), Some("file_id"));
    }

    #[test]
    fn unknown_type_returns_none() {
        assert_eq!(enum_str_by_value("not_a_real_type", 1), None);
    }

    #[test]
    fn unknown_value_within_known_type_returns_none() {
        // sport is u8; 250 is not a defined sport value.
        assert_eq!(enum_str_by_value("sport", 250), None);
    }
}
