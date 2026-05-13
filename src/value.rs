//! Typed (post-transform) field values and message types — the public
//! surface produced by [`crate::TypedDecoder`].
//!
//! [`Value`] is the union of every shape a fully-transformed field can
//! take: a typed scalar, a string, a resolved enum name, a converted
//! datetime, or an array. Compared to [`crate::RawValue`] it is consumer-
//! friendly: scale/offset already applied, enums already named, datetimes
//! already wall-clock.

#[cfg(feature = "chrono")]
use chrono::{DateTime, Utc};

/// A fully-transformed field value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Invalid or unsupported field value.
    Invalid,
    /// Untransformed signed integer (no scale/offset applied).
    SInt(i64),
    /// Untransformed unsigned integer.
    UInt(u64),
    /// Either a `Float32`/`Float64` base value, or any numeric value that
    /// has had non-identity scale/offset applied.
    Float(f64),
    /// UTF-8 string.
    String(String),
    /// Opaque byte array.
    Bytes(Vec<u8>),
    /// Boolean value.
    Bool(bool),
    /// Resolved enum value (e.g. `"running"` for `Sport::Running`).
    Enum(String),
    /// FIT timestamp converted to wall-clock UTC. With the `chrono` feature
    /// disabled this carries the raw FIT epoch seconds (u32) instead.
    #[cfg(feature = "chrono")]
    DateTime(DateTime<Utc>),
    /// FIT timestamp as raw seconds since the FIT epoch (1989-12-31 UTC).
    /// Only present when the `chrono` feature is **disabled**.
    #[cfg(not(feature = "chrono"))]
    DateTime(u32),
    /// Multi-element field. Each entry is a [`Value`] of homogeneous type.
    Array(Vec<Value>),
}

impl Value {
    /// Returns `true` if this is [`Value::Invalid`].
    pub fn is_invalid(&self) -> bool {
        matches!(self, Value::Invalid)
    }

    /// Extract an `f64` from `Float`, `UInt`, or `SInt` variants.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(v) => Some(*v),
            Value::UInt(v) => Some(*v as f64),
            Value::SInt(v) => Some(*v as f64),
            _ => None,
        }
    }

    /// Extract an `i64` from `SInt` or non-negative `UInt` variants.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::SInt(v) => Some(*v),
            Value::UInt(v) => Some(*v as i64),
            _ => None,
        }
    }

    /// Extract a `u64` from `UInt` or non-negative `SInt` variants.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::UInt(v) => Some(*v),
            Value::SInt(v) if *v >= 0 => Some(*v as u64),
            _ => None,
        }
    }

    /// Extract a string slice from `String` or `Enum` variants.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s.as_str()),
            Value::Enum(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Extract a `DateTime<Utc>` from the `DateTime` variant.
    #[cfg(feature = "chrono")]
    pub fn as_datetime(&self) -> Option<DateTime<Utc>> {
        match self {
            Value::DateTime(d) => Some(*d),
            _ => None,
        }
    }

    /// Extract the raw FIT epoch seconds from the `DateTime` variant.
    /// Available only when the `chrono` feature is disabled.
    #[cfg(not(feature = "chrono"))]
    pub fn as_datetime(&self) -> Option<u32> {
        match self {
            Value::DateTime(s) => Some(*s),
            _ => None,
        }
    }
}

/// One field of a fully-transformed message.
#[derive(Debug, Clone)]
pub struct Field {
    /// Snake-case canonical name from Profile.xlsx (or, when SubField
    /// resolution kicks in, the SubField's name). Developer fields carry
    /// the name from `field_description`, which is an owned `String`.
    pub name: String,
    /// Standard or developer field — see [`FieldKind`].
    pub kind: FieldKind,
    /// The decoded field value.
    pub value: Value,
    /// Display unit if Profile defines one (e.g. `"m/s"`, `"bpm"`).
    pub units: Option<String>,
}

/// Provenance of a field — distinguishes Profile-declared standard fields
/// from runtime-registered developer fields.
#[derive(Debug, Clone, Copy)]
pub enum FieldKind {
    /// A standard field defined in Profile.xlsx.
    Standard {
        /// Wire-level field definition number.
        field_def_num: u8,
    },
    /// A developer field; without M6's `field_description` registry, the
    /// value will be `Value::Bytes` and `name` will be a synthetic placeholder.
    Developer {
        /// Wire-level field definition number.
        field_def_num: u8,
        /// Index into the developer data ID table.
        developer_data_index: u8,
    },
}

/// A fully-transformed FIT message.
#[derive(Debug, Clone)]
pub struct Message {
    /// Profile-level message number.
    pub global_mesg_num: u16,
    /// Snake-case canonical message name from Profile.xlsx.
    pub name: &'static str,
    /// Fully-transformed fields.
    pub fields: Vec<Field>,
}

impl Message {
    /// Look up a standard field by its snake-case name. (Returns the first
    /// match — Profile guarantees field names are unique within a message.)
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }
}
