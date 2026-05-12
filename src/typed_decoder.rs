//! Typed decoder — composes the M5 transform pipeline on top of [`Decoder`].
//!
//! Builder API:
//!
//! ```ignore
//! let (messages, errors) = fit::Decoder::builder(&bytes)
//!     .apply_scale_and_offset(true)
//!     .convert_datetime(true)
//!     .build()
//!     .read_all();
//! ```
//!
//! The pipeline is:
//!   1. Look up `MesgInfo` by `global_mesg_num`.
//!   2. For each raw field, find its `FieldInfo`.
//!   3. Apply SubField selection (alters effective type/scale/offset).
//!   4. Convert the raw value through DateTime / enum-string / scale/offset.
//!   5. If the field declares Components, additionally emit one synthetic
//!      [`Field`] per component (LSB-first unpacking).
//!   6. Resolve developer fields via [`DevFieldRegistry`] (populated from
//!      `developer_data_id` / `field_description` messages).

use crate::base_type::BaseType;
#[cfg(feature = "chrono")]
use crate::datetime;
use crate::decoder::Decoder;
use crate::dev_fields::{self, DevFieldRegistry};
use crate::error::FitError;
use crate::profile;
use crate::raw_value::RawValue;
use crate::stream::Endian;
use crate::transforms::{components, enum_strings, scale_offset, subfields, Accumulator};
use crate::value::{Field, FieldKind, Message, Value};
use crate::{RawDevField, RawField, RawMessage};

/// Callback type for per-message notifications.
type MesgCallback<'a> = Box<dyn Fn(&Message) + 'a>;

/// Toggleable transforms. All default to `true` (matches the JS SDK's
/// "decode richly" preset). Disable individually to get cheaper / less
/// opinionated output.
#[derive(Debug, Clone, Copy)]
pub struct TransformOptions {
    pub apply_scale_and_offset: bool,
    pub expand_components: bool,
    pub expand_subfields: bool,
    pub convert_types_to_strings: bool,
    pub convert_datetime: bool,
    /// Run MemoGlob reassembly after `read_all()` collects messages.
    pub decode_memo_glob: bool,
    /// Run heart-rate merge (HR samples → averaged `record.heart_rate`) after
    /// `read_all()` collects messages. Requires `expand_components` and
    /// `apply_scale_and_offset` to be enabled to compute fractional sample
    /// timestamps correctly. Only available with the `chrono` feature.
    #[cfg(feature = "chrono")]
    pub merge_heart_rates: bool,
    /// Skip `file_id` messages from the output.
    pub skip_header: bool,
    /// Only keep messages whose `global_mesg_num` is in the Profile
    /// (filters out unknown / metadata-only messages).
    pub data_only: bool,
}

impl Default for TransformOptions {
    fn default() -> Self {
        Self {
            apply_scale_and_offset: true,
            expand_components: true,
            expand_subfields: true,
            convert_types_to_strings: true,
            convert_datetime: true,
            decode_memo_glob: false,
            #[cfg(feature = "chrono")]
            merge_heart_rates: false,
            skip_header: false,
            data_only: false,
        }
    }
}

/// Builder for [`TypedDecoder`]. Created by [`Decoder::builder`].
pub struct DecoderBuilder<'a> {
    bytes: &'a [u8],
    options: TransformOptions,
    on_mesg: Option<MesgCallback<'a>>,
}

impl<'a> DecoderBuilder<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            options: TransformOptions::default(),
            on_mesg: None,
        }
    }

    pub fn apply_scale_and_offset(mut self, v: bool) -> Self {
        self.options.apply_scale_and_offset = v;
        self
    }
    pub fn expand_components(mut self, v: bool) -> Self {
        self.options.expand_components = v;
        self
    }
    pub fn expand_subfields(mut self, v: bool) -> Self {
        self.options.expand_subfields = v;
        self
    }
    pub fn convert_types_to_strings(mut self, v: bool) -> Self {
        self.options.convert_types_to_strings = v;
        self
    }
    pub fn convert_datetime(mut self, v: bool) -> Self {
        self.options.convert_datetime = v;
        self
    }
    pub fn decode_memo_glob(mut self, v: bool) -> Self {
        self.options.decode_memo_glob = v;
        self
    }
    #[cfg(feature = "chrono")]
    pub fn merge_heart_rates(mut self, v: bool) -> Self {
        self.options.merge_heart_rates = v;
        self
    }
    pub fn skip_header(mut self, v: bool) -> Self {
        self.options.skip_header = v;
        self
    }
    pub fn data_only(mut self, v: bool) -> Self {
        self.options.data_only = v;
        self
    }

    /// Register a callback invoked for each decoded data message.
    pub fn on_mesg<F: Fn(&Message) + 'a>(mut self, f: F) -> Self {
        self.on_mesg = Some(Box::new(f));
        self
    }

    pub fn build(self) -> TypedDecoder<'a> {
        TypedDecoder {
            inner: Decoder::new(self.bytes),
            options: self.options,
            accumulator: Accumulator::new(),
            dev_registry: DevFieldRegistry::new(),
            on_mesg: self.on_mesg,
        }
    }
}

impl<'a> Decoder<'a> {
    /// Entry point for the typed pipeline. The caller chains transform-
    /// option toggles on the returned [`DecoderBuilder`] and finishes with
    /// `.build()`.
    pub fn builder(bytes: &'a [u8]) -> DecoderBuilder<'a> {
        DecoderBuilder::new(bytes)
    }
}

/// Typed-message iterator. Yields one [`Message`] per Data record from the
/// underlying [`Decoder`].
pub struct TypedDecoder<'a> {
    inner: Decoder<'a>,
    options: TransformOptions,
    pub(crate) accumulator: Accumulator,
    dev_registry: DevFieldRegistry,
    on_mesg: Option<MesgCallback<'a>>,
}

impl<'a> TypedDecoder<'a> {
    /// Drain into vectors, mirroring [`Decoder::read_all`].
    pub fn read_all(mut self) -> (Vec<Message>, Vec<FitError>) {
        let mut messages = Vec::new();
        let mut errors = Vec::new();
        for item in self.by_ref() {
            match item {
                Ok(m) => messages.push(m),
                Err(e) => errors.push(e),
            }
        }

        // Post-processing: skip_header
        if self.options.skip_header {
            messages.retain(|m| m.global_mesg_num != 0); // file_id = 0
        }

        // Post-processing: data_only — filter to known Profile messages.
        if self.options.data_only {
            messages.retain(|m| crate::profile::mesg_info_by_num(m.global_mesg_num).is_some());
        }

        // Post-processing: MemoGlob reassembly
        if self.options.decode_memo_glob {
            crate::transforms::memo_glob::decode_memo_glob(&mut messages);
        }

        // Post-processing: HR merge (chrono-only).
        #[cfg(feature = "chrono")]
        if self.options.merge_heart_rates {
            crate::transforms::merge_heart_rates(&mut messages);
        }

        (messages, errors)
    }
}

impl<'a> Iterator for TypedDecoder<'a> {
    type Item = Result<Message, FitError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.inner.next()? {
            Ok(raw) => {
                // Reset per-chain accumulator state at chained-FIT boundaries.
                // The raw decoder flags the first Data record after a boundary;
                // accumulated counter totals must not leak between chained files.
                if raw.starts_new_chain {
                    self.accumulator.clear();
                }
                collect_dev_meta(&raw, &mut self.dev_registry);
                let msg = transform_message(
                    &raw,
                    self.options,
                    &mut self.accumulator,
                    &self.dev_registry,
                );
                if let Some(ref cb) = self.on_mesg {
                    cb(&msg);
                }
                Some(Ok(msg))
            }
            Err(e) => Some(Err(e)),
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// Per-message and per-field transformation
// ────────────────────────────────────────────────────────────────────

/// Intercept `developer_data_id` (207) and `field_description` (206) messages
/// to populate the developer field registry.
fn collect_dev_meta(raw: &RawMessage<'_>, registry: &mut DevFieldRegistry) {
    match raw.global_mesg_num {
        // developer_data_id — we just need to know the index exists.
        // The registry doesn't store the id itself, but we could extend it.
        207 => {}
        // field_description — register the field schema.
        206 => {
            let dev_idx = raw.field(0).and_then(|f| f.value.as_u8()).unwrap_or(0);
            let fdn = raw.field(1).and_then(|f| f.value.as_u8()).unwrap_or(0);
            let fit_base_type_id = raw.field(2).and_then(|f| f.value.as_u8()).unwrap_or(0xFF);
            let field_name = raw
                .field(3)
                .and_then(|f| f.value.as_str())
                .unwrap_or("unknown")
                .to_string();
            let scale_raw = raw.field(6).and_then(|f| f.value.as_u8());
            let offset_raw = raw.field(7).and_then(|f| f.value.as_u8());
            let units_str = raw
                .field(8)
                .and_then(|f| f.value.as_str())
                .map(|s| s.to_string());

            let scale = scale_raw.map(|v| v as f64).filter(|&s| s != 1.0);
            let offset = offset_raw.map(|v| v as f64).filter(|&o| o != 0.0);

            registry.register_field(
                dev_idx,
                fdn,
                field_name,
                fit_base_type_id,
                scale,
                offset,
                units_str,
            );
        }
        _ => {}
    }
}

fn transform_message(
    raw: &RawMessage<'_>,
    options: TransformOptions,
    acc: &mut Accumulator,
    dev_registry: &DevFieldRegistry,
) -> Message {
    let mesg_info = profile::mesg_info_by_num(raw.global_mesg_num);
    let name = mesg_info.map(|m| m.name).unwrap_or("unknown");

    let mut fields = Vec::with_capacity(raw.fields.len() + 4);

    for rf in &raw.fields {
        let Some(mesg) = mesg_info else {
            fields.push(unknown_field(rf));
            continue;
        };
        let Some(fi) = mesg.field(rf.field_def_num) else {
            fields.push(unknown_field(rf));
            continue;
        };

        // Step 1: SubField resolution — picks effective semantics for this field.
        let selected_subfield: Option<&crate::profile::SubField> = if options.expand_subfields {
            subfields::select(fi, mesg, raw)
        } else {
            None
        };
        let (effective_name, type_name, scale, offset, units) = match selected_subfield {
            Some(sub) => (sub.name, sub.type_name, fi.scale, fi.offset, fi.units),
            None => (fi.name, fi.type_name, fi.scale, fi.offset, fi.units),
        };

        // Step 2: Accumulator — rollover compensation for counter fields.
        let raw_for_transform = if fi.accumulate {
            if let Some(scalar) = components::scalar_as_u64(&rf.value) {
                let bits = resolve_accumulator_bits(fi.type_name);
                let accumulated =
                    acc.accumulate(raw.global_mesg_num, rf.field_def_num, scalar, bits);
                RawValue::U64Scalar(accumulated)
            } else {
                rf.value.clone()
            }
        } else {
            rf.value.clone()
        };

        // Step 3: convert the value through the option-gated transforms.
        let value = transform_value(&raw_for_transform, type_name, scale, offset, options);
        fields.push(Field {
            name: effective_name.to_string(),
            kind: FieldKind::Standard {
                field_def_num: rf.field_def_num,
            },
            value,
            units: units.map(str::to_string),
        });

        // Step 4: Components expansion (after the parent field has been emitted).
        if options.expand_components {
            // Parent field components.
            if !fi.components.is_empty() {
                if let Some(scalar) = components::scalar_as_u64(&rf.value) {
                    for comp in components::unpack_scalar(fi.components, scalar) {
                        let comp_raw = if comp.accumulate {
                            acc.accumulate(
                                raw.global_mesg_num,
                                rf.field_def_num,
                                comp.raw,
                                comp.bits as u32,
                            )
                        } else {
                            comp.raw
                        };
                        let raw_v = RawValue::U64Scalar(comp_raw);
                        let cv =
                            transform_value(&raw_v, "uint64", comp.scale, comp.offset, options);
                        fields.push(Field {
                            name: comp.target_name.to_string(),
                            kind: FieldKind::Standard { field_def_num: 0 },
                            value: cv,
                            units: comp.units.map(str::to_string),
                        });
                    }
                }
            }
            // SubField components (if the selected SubField has its own components).
            if let Some(sub) = selected_subfield {
                if !sub.components.is_empty() {
                    if let Some(scalar) = components::scalar_as_u64(&rf.value) {
                        for comp in components::unpack_scalar(sub.components, scalar) {
                            let comp_raw = if comp.accumulate {
                                acc.accumulate(
                                    raw.global_mesg_num,
                                    rf.field_def_num,
                                    comp.raw,
                                    comp.bits as u32,
                                )
                            } else {
                                comp.raw
                            };
                            let raw_v = RawValue::U64Scalar(comp_raw);
                            let cv =
                                transform_value(&raw_v, "uint64", comp.scale, comp.offset, options);
                            fields.push(Field {
                                name: comp.target_name.to_string(),
                                kind: FieldKind::Standard { field_def_num: 0 },
                                value: cv,
                                units: comp.units.map(str::to_string),
                            });
                        }
                    }
                }
            }
        }
    }

    // Developer fields: resolve via registry if available, else pass through bytes.
    for dev in &raw.dev_fields {
        if let Some(info) = dev_registry.get(dev.developer_data_index, dev.field_def_num) {
            let value = resolve_dev_field(dev, info, options);
            fields.push(Field {
                name: info.name.clone(),
                kind: FieldKind::Developer {
                    field_def_num: dev.field_def_num,
                    developer_data_index: dev.developer_data_index,
                },
                value,
                units: info.units.clone(),
            });
        } else {
            fields.push(Field {
                name: "developer_field".to_string(),
                kind: FieldKind::Developer {
                    field_def_num: dev.field_def_num,
                    developer_data_index: dev.developer_data_index,
                },
                value: Value::Bytes(dev.bytes.clone().into_owned()),
                units: None,
            });
        }
    }

    Message {
        global_mesg_num: raw.global_mesg_num,
        name,
        fields,
    }
}

fn unknown_field(rf: &RawField) -> Field {
    Field {
        name: "unknown".to_string(),
        kind: FieldKind::Standard {
            field_def_num: rf.field_def_num,
        },
        value: raw_to_value_passthrough(&rf.value),
        units: None,
    }
}

/// Resolve a Profile type_name to the accumulator bit width.
///
/// For counter fields, the wire type's element size determines the wraparound
/// width (e.g. `uint8` → 8 bits, `uint16` → 16 bits, `uint32` → 32 bits).
/// Falls back to 32 bits if the type cannot be resolved.
fn resolve_accumulator_bits(type_name: &str) -> u32 {
    // Try Profile enum types first (e.g. "sport", "date_time").
    if let Some(bt) = crate::transforms::enum_strings::base_type_for_type_name(type_name) {
        return (bt.element_size() * 8) as u32;
    }
    // Try raw base type names (e.g. "uint8", "uint16", "uint32").
    let bt = match type_name {
        "enum" | "sint8" | "uint8" | "uint8z" | "byte" | "string" | "bool" => BaseType::UInt8,
        "sint16" | "uint16" | "uint16z" => BaseType::UInt16,
        "sint32" | "uint32" | "uint32z" | "float32" | "date_time" | "local_date_time" => {
            BaseType::UInt32
        }
        "sint64" | "uint64" | "uint64z" | "float64" => BaseType::UInt64,
        _ => BaseType::UInt32,
    };
    (bt.element_size() * 8) as u32
}

/// Resolve a developer field's raw bytes into a typed [`Value`] using the
/// registry entry's base type, scale, and offset.
fn resolve_dev_field(
    dev: &RawDevField<'_>,
    info: &dev_fields::DevFieldInfo,
    options: TransformOptions,
) -> Value {
    // Decode the raw bytes as the registered base type.
    // Developer fields are always LE per the FIT protocol.
    let raw = match crate::raw_value::decode_value(
        info.base_type,
        &dev.bytes,
        Endian::Little,
        dev.field_def_num,
    ) {
        Ok(v) => v,
        Err(_) => return Value::Bytes(dev.bytes.clone().into_owned()),
    };

    let type_name = dev_fields::base_type_to_type_name(info.base_type);
    transform_value(&raw, type_name, info.scale, info.offset, options)
}

fn transform_value(
    raw: &RawValue,
    type_name: &str,
    scale: Option<f64>,
    offset: Option<f64>,
    options: TransformOptions,
) -> Value {
    if matches!(raw, RawValue::Invalid) {
        return Value::Invalid;
    }

    // DateTime conversion (date_time / local_date_time).
    // With chrono: produces `Value::DateTime(DateTime<Utc>)`.
    // Without chrono: produces `Value::DateTime(u32)` carrying raw FIT seconds.
    if options.convert_datetime && (type_name == "date_time" || type_name == "local_date_time") {
        if let Some(secs) = components::scalar_as_u64(raw) {
            if secs <= u32::MAX as u64 {
                #[cfg(feature = "chrono")]
                {
                    if let Some(dt) = datetime::fit_to_datetime(secs as u32) {
                        return Value::DateTime(dt);
                    }
                }
                #[cfg(not(feature = "chrono"))]
                {
                    return Value::DateTime(secs as u32);
                }
            }
        }
    }

    // Enum int → snake_case string.
    if options.convert_types_to_strings {
        if let Some(v) = components::scalar_as_u64(raw) {
            if let Some(s) = enum_strings::enum_str_by_value(type_name, v) {
                return Value::Enum(s);
            }
        }
    }

    // Scale / Offset.
    if options.apply_scale_and_offset && !scale_offset::is_identity(scale, offset) {
        if let Some(v) = scalar_as_f64(raw) {
            return Value::Float(scale_offset::apply(v, scale, offset));
        }
        if let Some(arr) = array_as_f64s(raw) {
            return Value::Array(
                arr.into_iter()
                    .map(|v| Value::Float(scale_offset::apply(v, scale, offset)))
                    .collect(),
            );
        }
    }

    raw_to_value_passthrough(raw)
}

fn raw_to_value_passthrough(raw: &RawValue) -> Value {
    use RawValue::*;
    match raw {
        Invalid => Value::Invalid,
        String(s) => Value::String(s.to_string()),
        Byte(v) => Value::Bytes(v.to_vec()),

        // Scalars — single element on the stack.
        EnumScalar(v) | U8Scalar(v) | U8zScalar(v) => Value::UInt(*v as u64),
        U16Scalar(v) | U16zScalar(v) => Value::UInt(*v as u64),
        U32Scalar(v) | U32zScalar(v) => Value::UInt(*v as u64),
        U64Scalar(v) | U64zScalar(v) => Value::UInt(*v),
        I8Scalar(v) => Value::SInt(*v as i64),
        I16Scalar(v) => Value::SInt(*v as i64),
        I32Scalar(v) => Value::SInt(*v as i64),
        I64Scalar(v) => Value::SInt(*v),
        F32Scalar(v) => Value::Float(*v as f64),
        F64Scalar(v) => Value::Float(*v),

        // Arrays — multiple elements. (length ≥ 2 by construction)
        EnumArray(a) | U8Array(a) | U8zArray(a) => {
            Value::Array(a.iter().map(|x| Value::UInt(*x as u64)).collect())
        }
        U16Array(a) | U16zArray(a) => {
            Value::Array(a.iter().map(|x| Value::UInt(*x as u64)).collect())
        }
        U32Array(a) | U32zArray(a) => {
            Value::Array(a.iter().map(|x| Value::UInt(*x as u64)).collect())
        }
        U64Array(a) | U64zArray(a) => {
            Value::Array(a.iter().map(|x| Value::UInt(*x)).collect())
        }
        I8Array(a) => Value::Array(a.iter().map(|x| Value::SInt(*x as i64)).collect()),
        I16Array(a) => Value::Array(a.iter().map(|x| Value::SInt(*x as i64)).collect()),
        I32Array(a) => Value::Array(a.iter().map(|x| Value::SInt(*x as i64)).collect()),
        I64Array(a) => Value::Array(a.iter().map(|x| Value::SInt(*x)).collect()),
        F32Array(a) => Value::Array(a.iter().map(|x| Value::Float(*x as f64)).collect()),
        F64Array(a) => Value::Array(a.iter().map(|x| Value::Float(*x)).collect()),
    }
}

#[inline]
fn scalar_as_f64(raw: &RawValue) -> Option<f64> {
    raw.scalar_f64()
}

#[inline]
fn array_as_f64s(raw: &RawValue) -> Option<Vec<f64>> {
    // For `to_f64s` we only want to take the array path here; callers already
    // tried the scalar route first, but `to_f64s` accepts both and returns a
    // single-element vec for scalars, which is also valid output.
    raw.to_f64s()
}
