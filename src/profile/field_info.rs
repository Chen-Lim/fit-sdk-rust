//! Runtime types describing a FIT message's schema.
//!
//! Every value here is `&'static str` / `&'static [...]` so the entire profile
//! lives in `.rodata` with zero allocation. The generated code in
//! [`super::generated`] populates these structs with const initialisers.

/// Static metadata for a single field of a single message.
#[derive(Debug)]
pub struct FieldInfo {
    /// Field definition number used on the wire (0..=255).
    pub field_def_num: u8,
    /// Snake-case canonical name from Profile.xlsx.
    pub name: &'static str,
    /// Type reference: either a base type ("uint16") or a Types-sheet name ("sport").
    /// Resolution to a runtime base type happens in M3.
    pub type_name: &'static str,
    /// Raw array spec (e.g. `"[5]"`, `"[N]"`, `"[5x10]"`) or `None` for scalars.
    pub array: Option<&'static str>,
    /// Scale factor (`physical = raw / scale - offset`). First element only when
    /// scale is per-component; component-specific scales live on `Component`.
    pub scale: Option<f64>,
    /// Offset.
    pub offset: Option<f64>,
    /// Display unit (e.g. `"m/s"`, `"bpm"`).
    pub units: Option<&'static str>,
    /// Components — non-empty when the wire field unpacks LSB-first into
    /// multiple sub-values (see protocol §"Components").
    pub components: &'static [Component],
    /// SubFields — alternative semantic interpretations selected at runtime
    /// based on the value of another field in the same message.
    pub sub_fields: &'static [SubField],
    /// Whether this field accumulates across messages (for rollover compensation).
    pub accumulate: bool,
}

/// One LSB-first slot inside a Components field.
#[derive(Debug)]
pub struct Component {
    /// Name of the synthesised sub-value (refers to another field in the same message).
    pub name: &'static str,
    /// Width in bits.
    pub bits: u8,
    pub scale: Option<f64>,
    pub offset: Option<f64>,
    pub units: Option<&'static str>,
    pub accumulate: bool,
}

/// One alternative interpretation of a parent field.
#[derive(Debug)]
pub struct SubField {
    /// Snake-case canonical name from Profile.xlsx.
    pub name: &'static str,
    /// Type reference for this SubField.
    pub type_name: &'static str,
    /// Conditions: parallel `(ref_field_name, ref_value)` pairs. The SubField
    /// applies when **any** condition matches (i.e. ref-field decoded value
    /// equals one of the listed values, after Profile-level type resolution).
    pub conditions: &'static [(&'static str, &'static str)],
    /// Components to unpack when this SubField is selected (empty for most SubFields).
    pub components: &'static [Component],
    /// Scale factor.
    pub scale: Option<f64>,
    /// Offset.
    pub offset: Option<f64>,
    /// Display unit.
    pub units: Option<&'static str>,
}

/// Static metadata for a single message.
#[derive(Debug)]
pub struct MesgInfo {
    /// Snake-case canonical name from Profile.xlsx.
    pub name: &'static str,
    /// Fields in spreadsheet order.
    pub fields: &'static [FieldInfo],
}

impl MesgInfo {
    /// Find a field by its definition number. O(n); messages have ≤ 50 fields
    /// in practice, so linear scan beats a hashmap for both code size and cache.
    pub fn field(&self, field_def_num: u8) -> Option<&FieldInfo> {
        self.fields
            .iter()
            .find(|f| f.field_def_num == field_def_num)
    }

    /// Find a field by its snake-case name.
    pub fn field_by_name(&self, name: &str) -> Option<&FieldInfo> {
        self.fields.iter().find(|f| f.name == name)
    }
}
