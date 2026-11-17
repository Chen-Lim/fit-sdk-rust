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

use std::borrow::Cow;
use std::collections::VecDeque;

use crate::base_type::BaseType;
#[cfg(feature = "chrono")]
use crate::datetime;
use crate::decoder::Decoder;
use crate::dev_fields::{self, DevFieldRegistry};
use crate::error::FitError;
use crate::profile::{self, Component, FieldInfo};
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
    /// Skip the 14-byte header without validating it (uses [`crate::DecodeMode::SkipHeader`])
    /// and ignore trailing file CRC. Mutually exclusive with `data_only`.
    pub skip_header: bool,
    /// Treat the byte stream as headerless FIT records (uses [`crate::DecodeMode::DataOnly`])
    /// and ignore trailing file CRC. Mutually exclusive with `skip_header`.
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

fn validate_options(options: TransformOptions) -> Result<(), FitError> {
    if options.data_only && options.skip_header {
        return Err(FitError::InvalidOptions(
            "data_only and skip_header cannot both be enabled",
        ));
    }
    #[cfg(feature = "chrono")]
    if options.merge_heart_rates
        && (!options.apply_scale_and_offset || !options.expand_components)
    {
        return Err(FitError::InvalidOptions(
            "merge_heart_rates requires apply_scale_and_offset and expand_components",
        ));
    }
    Ok(())
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
        let mode = if self.options.skip_header {
            crate::decoder::DecodeMode::SkipHeader
        } else if self.options.data_only {
            crate::decoder::DecodeMode::DataOnly
        } else {
            crate::decoder::DecodeMode::Normal
        };
        let pending_option_error = validate_options(self.options).err();
        TypedDecoder {
            inner: Decoder::with_mode(self.bytes, mode),
            options: self.options,
            accumulator: Accumulator::new(),
            dev_registry: DevFieldRegistry::new(),
            on_mesg: self.on_mesg,
            pending_option_error,
            terminated: false,
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
    pending_option_error: Option<FitError>,
    terminated: bool,
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

        // Post-processing: MemoGlob reassembly
        if self.options.decode_memo_glob && !messages.is_empty() {
            crate::transforms::memo_glob::decode_memo_glob(&mut messages);
        }

        // Post-processing: HR merge (chrono-only).
        #[cfg(feature = "chrono")]
        if self.options.merge_heart_rates && !messages.is_empty() {
            crate::transforms::merge_heart_rates(&mut messages);
        }

        (messages, errors)
    }
}

impl<'a> Iterator for TypedDecoder<'a> {
    type Item = Result<Message, FitError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.terminated {
            return None;
        }
        if let Some(err) = self.pending_option_error.take() {
            self.terminated = true;
            return Some(Err(err));
        }

        match self.inner.next()? {
            Ok(raw) => {
                // Reset per-chain accumulator state at chained-FIT boundaries.
                // The raw decoder flags the first Data record after a boundary;
                // accumulated counter totals must not leak between chained files.
                if raw.starts_new_chain {
                    self.accumulator.clear();
                }
                let endian = self.inner.last_endian();
                collect_dev_meta(&raw, &mut self.dev_registry);
                let msg = transform_message(
                    &raw,
                    endian,
                    self.options,
                    &mut self.accumulator,
                    &self.dev_registry,
                );
                if let Some(ref cb) = self.on_mesg {
                    cb(&msg);
                }
                Some(Ok(msg))
            }
            Err(e) => {
                self.terminated = true;
                Some(Err(e))
            }
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
        207 => {}
        // field_description — register the field schema.
        206 => {
            let Some(dev_idx) = raw.field(0).and_then(|f| f.value.as_u8()) else {
                return;
            };
            let Some(fdn) = raw.field(1).and_then(|f| f.value.as_u8()) else {
                return;
            };
            let fit_base_type_id = raw.field(2).and_then(|f| f.value.as_u8()).unwrap_or(0xFF);
            let field_name = raw
                .field(3)
                .and_then(|f| f.value.as_str())
                .unwrap_or("unknown")
                .to_string();
            let scale_raw = raw.field(6).and_then(|f| f.value.as_u8());
            let offset_raw = raw.field(7).and_then(|f| f.value.as_i8());
            let units_str = raw
                .field(8)
                .and_then(|f| f.value.as_str())
                .map(|s| s.to_string());

            let scale = scale_raw
                .map(|v| v as f64)
                .filter(|&s| s != 1.0 && s != 0.0);
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

/// One queued component-expansion unit (parent component slice + wire bytes).
struct PendingExpansion {
    components: &'static [Component],
    raw: RawValue,
}

/// Accumulated expanded values sharing the same target field name within a message.
struct ExpandedGroup {
    name: &'static str,
    field_def_num: u8,
    units: Option<&'static str>,
    values: Vec<Value>,
}

const MAX_COMPONENT_EXPANSIONS: usize = 64;

fn transform_message(
    raw: &RawMessage<'_>,
    endian: Endian,
    options: TransformOptions,
    acc: &mut Accumulator,
    dev_registry: &DevFieldRegistry,
) -> Message {
    let mesg_info = profile::mesg_info_by_num(raw.global_mesg_num);
    let name = mesg_info.map(|m| m.name).unwrap_or("unknown");

    let mut fields = Vec::with_capacity(raw.fields.len() + 4);
    let mut pending_expansions: VecDeque<PendingExpansion> = VecDeque::new();
    let mut accumulated_seeds: Vec<(&'static FieldInfo, RawValue)> = Vec::new();

    // Pass 1: emit wire fields (with SubField resolution), enqueue component
    // containers, and record authoritative full-width accumulator seeds.
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
        let selected_subfield = if options.expand_subfields {
            subfields::select(fi, mesg, raw)
        } else {
            None
        };
        let (effective_name, type_name, mut scale, mut offset, units, active_components) =
            match selected_subfield {
                Some(sub) => (
                    sub.name,
                    sub.type_name,
                    sub.scale,
                    sub.offset,
                    sub.units,
                    sub.components,
                ),
                None => (
                    fi.name,
                    fi.type_name,
                    fi.scale,
                    fi.offset,
                    fi.units,
                    fi.components,
                ),
            };

        // Multi-component packed containers hold heterogeneous bit-fields;
        // parent-level scale/offset (e.g. `compressed_speed_distance` 100,
        // `hr.event_timestamp_12` 1024) must not be applied to the raw container.
        if active_components.len() > 1 {
            scale = None;
            offset = None;
        }

        if fi.accumulate && !rf.value.is_invalid() {
            accumulated_seeds.push((fi, rf.value.clone()));
        }

        let value = transform_value(&rf.value, type_name, scale, offset, options);
        fields.push(Field {
            name: effective_name.to_string(),
            kind: FieldKind::Standard {
                field_def_num: rf.field_def_num,
            },
            value,
            units: units.map(str::to_string),
        });

        if options.expand_components && !active_components.is_empty() && !rf.value.is_invalid() {
            pending_expansions.push_back(PendingExpansion {
                components: active_components,
                raw: rf.value.clone(),
            });
        }
    }

    // Seed the accumulator from full-width wire fields before expanding any
    // component deltas, matching JS `#setAccumulatedField`.
    for (fi, raw_val) in accumulated_seeds {
        let mut matched_component: Option<&Component> = None;
        'outer: for item in &pending_expansions {
            for c in item.components {
                if c.name == fi.name && (fi.accumulate || c.accumulate) {
                    matched_component = Some(c);
                    break 'outer;
                }
            }
        }
        for v in raw_u64_elements(&raw_val) {
            let seed = match matched_component {
                Some(c) => {
                    let phys = scale_offset::apply(v as f64, fi.scale, fi.offset);
                    scale_offset::unapply(phys, c.scale, c.offset)
                        .round()
                        .max(0.0) as u64
                }
                None => v,
            };
            acc.set(raw.global_mesg_num, fi.field_def_num, seed);
        }
    }

    // Pass 2: cascading BFS component expansion with target invalid checks
    // and deterministic array grouping by target field name.
    if options.expand_components {
        if let Some(mesg) = mesg_info {
            let mut groups: Vec<ExpandedGroup> = Vec::new();
            let mut expansions = 0usize;

            while let Some(item) = pending_expansions.pop_front() {
                if expansions >= MAX_COMPONENT_EXPANSIONS {
                    break;
                }
                expansions += 1;

                for comp in components::unpack_raw(item.components, &item.raw) {
                    let target_fi = mesg.field_by_name(comp.target_name);
                    let target_fdn = target_fi.map_or(0, |tf| tf.field_def_num);
                    let target_type = target_fi.map_or("uint64", |tf| tf.type_name);
                    let target_bt = target_fi.and_then(|tf| BaseType::from_profile_type(tf.type_name));
                    let target_scale = target_fi.and_then(|tf| tf.scale);
                    let target_offset = target_fi.and_then(|tf| tf.offset);
                    let target_units = target_fi.and_then(|tf| tf.units).or(comp.units);

                    let is_acc = target_fi.map_or(comp.accumulate, |tf| {
                        tf.accumulate || comp.accumulate
                    });
                    let comp_acc = if is_acc {
                        acc.accumulate(
                            raw.global_mesg_num,
                            target_fdn,
                            comp.raw,
                            comp.bits as u32,
                        )
                    } else {
                        comp.raw
                    };

                    let physical = scale_offset::apply(comp_acc as f64, comp.scale, comp.offset);
                    let target_raw_f64 = scale_offset::unapply(physical, target_scale, target_offset);

                    let cv = if is_target_invalid(target_raw_f64, target_bt) {
                        Value::Invalid
                    } else {
                        if let Some(tf) = target_fi {
                            if !tf.components.is_empty()
                                && expansions + pending_expansions.len() < MAX_COMPONENT_EXPANSIONS
                            {
                                let cascade_raw = raw_from_f64(
                                    target_raw_f64,
                                    target_bt.unwrap_or(BaseType::UInt32),
                                );
                                pending_expansions.push_back(PendingExpansion {
                                    components: tf.components,
                                    raw: cascade_raw,
                                });
                            }
                        }

                        expanded_component_value(
                            comp_acc,
                            physical,
                            target_raw_f64,
                            target_type,
                            target_bt,
                            comp.scale,
                            comp.offset,
                            target_scale,
                            target_offset,
                            options,
                        )
                    };

                    if let Some(group) = groups.iter_mut().find(|g| g.name == comp.target_name) {
                        if group.units.is_none() {
                            group.units = target_units;
                        }
                        group.values.push(cv);
                    } else {
                        groups.push(ExpandedGroup {
                            name: comp.target_name,
                            field_def_num: target_fdn,
                            units: target_units,
                            values: vec![cv],
                        });
                    }
                }
            }

            for group in groups {
                let value = if group.values.len() == 1 {
                    group.values.into_iter().next().unwrap()
                } else if group.values.iter().all(|v| matches!(v, Value::Invalid)) {
                    Value::Invalid
                } else {
                    Value::Array(group.values)
                };
                upsert_standard_field(
                    &mut fields,
                    Field {
                        name: group.name.to_string(),
                        kind: FieldKind::Standard {
                            field_def_num: group.field_def_num,
                        },
                        value,
                        units: group.units.map(str::to_string),
                    },
                );
            }
        }
    }

    // Developer fields: resolve via registry if available, else pass through bytes.
    for dev in &raw.dev_fields {
        if let Some(info) = dev_registry.get(dev.developer_data_index, dev.field_def_num) {
            let value = resolve_dev_field(dev, info, endian, options);
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

fn upsert_standard_field(fields: &mut Vec<Field>, expanded: Field) {
    if let Some(existing) = fields
        .iter_mut()
        .find(|f| matches!(f.kind, FieldKind::Standard { .. }) && f.name == expanded.name)
    {
        *existing = expanded;
    } else {
        fields.push(expanded);
    }
}

fn raw_u64_elements(raw: &RawValue) -> Vec<u64> {
    use RawValue::*;
    match raw {
        Invalid | String(_) => Vec::new(),
        Byte(b) => b.iter().map(|&v| v as u64).collect(),
        EnumScalar(v) | U8Scalar(v) | U8zScalar(v) => vec![*v as u64],
        I8Scalar(v) => vec![*v as u8 as u64],
        U16Scalar(v) | U16zScalar(v) => vec![*v as u64],
        I16Scalar(v) => vec![*v as u16 as u64],
        U32Scalar(v) | U32zScalar(v) => vec![*v as u64],
        I32Scalar(v) => vec![*v as u32 as u64],
        U64Scalar(v) | U64zScalar(v) => vec![*v],
        I64Scalar(v) => vec![*v as u64],
        F32Scalar(v) => vec![*v as u64],
        F64Scalar(v) => vec![*v as u64],
        EnumArray(a) | U8Array(a) | U8zArray(a) => a.iter().map(|&v| v as u64).collect(),
        I8Array(a) => a.iter().map(|&v| v as u8 as u64).collect(),
        U16Array(a) | U16zArray(a) => a.iter().map(|&v| v as u64).collect(),
        I16Array(a) => a.iter().map(|&v| v as u16 as u64).collect(),
        U32Array(a) | U32zArray(a) => a.iter().map(|&v| v as u64).collect(),
        I32Array(a) => a.iter().map(|&v| v as u32 as u64).collect(),
        U64Array(a) | U64zArray(a) => a.to_vec(),
        I64Array(a) => a.iter().map(|&v| v as u64).collect(),
        F32Array(a) => a.iter().map(|&v| v as u64).collect(),
        F64Array(a) => a.iter().map(|&v| v as u64).collect(),
    }
}

fn is_target_invalid(target_raw_f64: f64, target_bt: Option<BaseType>) -> bool {
    if !target_raw_f64.is_finite() {
        return true;
    }
    let Some(bt) = target_bt else {
        return false;
    };
    let rounded = target_raw_f64.round();
    match bt {
        BaseType::Enum | BaseType::UInt8 | BaseType::Byte => rounded == 0xFF as f64,
        BaseType::SInt8 => rounded == i8::MAX as f64,
        BaseType::UInt16 => rounded == 0xFFFF as f64,
        BaseType::SInt16 => rounded == i16::MAX as f64,
        BaseType::UInt32 => rounded == u32::MAX as f64,
        BaseType::SInt32 => rounded == i32::MAX as f64,
        BaseType::UInt8z | BaseType::UInt16z | BaseType::UInt32z | BaseType::UInt64z => {
            rounded == 0.0
        }
        BaseType::SInt64 => rounded == i64::MAX as f64,
        BaseType::UInt64 | BaseType::Float32 | BaseType::Float64 | BaseType::String => false,
    }
}

fn raw_from_f64(value: f64, bt: BaseType) -> RawValue {
    let rounded = value.round();
    match bt {
        BaseType::Enum => RawValue::EnumScalar(rounded.clamp(0.0, u8::MAX as f64) as u8),
        BaseType::UInt8 => RawValue::U8Scalar(rounded.clamp(0.0, u8::MAX as f64) as u8),
        BaseType::UInt8z => RawValue::U8zScalar(rounded.clamp(0.0, u8::MAX as f64) as u8),
        BaseType::SInt8 => RawValue::I8Scalar(rounded.clamp(i8::MIN as f64, i8::MAX as f64) as i8),
        BaseType::UInt16 => RawValue::U16Scalar(rounded.clamp(0.0, u16::MAX as f64) as u16),
        BaseType::UInt16z => RawValue::U16zScalar(rounded.clamp(0.0, u16::MAX as f64) as u16),
        BaseType::SInt16 => {
            RawValue::I16Scalar(rounded.clamp(i16::MIN as f64, i16::MAX as f64) as i16)
        }
        BaseType::UInt32 => RawValue::U32Scalar(rounded.clamp(0.0, u32::MAX as f64) as u32),
        BaseType::UInt32z => RawValue::U32zScalar(rounded.clamp(0.0, u32::MAX as f64) as u32),
        BaseType::SInt32 => {
            RawValue::I32Scalar(rounded.clamp(i32::MIN as f64, i32::MAX as f64) as i32)
        }
        BaseType::UInt64 => RawValue::U64Scalar(if rounded <= 0.0 {
            0
        } else if rounded >= u64::MAX as f64 {
            u64::MAX
        } else {
            rounded as u64
        }),
        BaseType::UInt64z => RawValue::U64zScalar(if rounded <= 0.0 {
            0
        } else if rounded >= u64::MAX as f64 {
            u64::MAX
        } else {
            rounded as u64
        }),
        BaseType::SInt64 => {
            RawValue::I64Scalar(rounded.clamp(i64::MIN as f64, i64::MAX as f64) as i64)
        }
        BaseType::Float32 => RawValue::F32Scalar(value as f32),
        BaseType::Float64 => RawValue::F64Scalar(value),
        BaseType::Byte => {
            RawValue::Byte(vec![rounded.clamp(0.0, u8::MAX as f64) as u8].into_boxed_slice())
        }
        BaseType::String => RawValue::Invalid,
    }
}

#[allow(clippy::too_many_arguments)]
fn expanded_component_value(
    comp_raw: u64,
    physical: f64,
    target_raw_f64: f64,
    target_type: &str,
    target_bt: Option<BaseType>,
    comp_scale: Option<f64>,
    comp_offset: Option<f64>,
    target_scale: Option<f64>,
    target_offset: Option<f64>,
    options: TransformOptions,
) -> Value {
    if options.convert_datetime && (target_type == "date_time" || target_type == "local_date_time") {
        let secs_f = target_raw_f64.round();
        if (0.0..=u32::MAX as f64).contains(&secs_f) {
            let secs = secs_f as u32;
            #[cfg(feature = "chrono")]
            if let Some(dt) = datetime::fit_to_datetime(secs) {
                return Value::DateTime(dt);
            }
            #[cfg(not(feature = "chrono"))]
            return Value::DateTime(secs);
        }
    }

    if options.convert_types_to_strings && physical >= 0.0 && physical.fract() == 0.0 {
        if let Some(s) = enum_strings::enum_str_by_value(target_type, physical as u64) {
            return Value::Enum(Cow::Borrowed(s));
        }
    }

    if !options.apply_scale_and_offset {
        if scale_offset::is_identity(comp_scale, comp_offset)
            && scale_offset::is_identity(target_scale, target_offset)
            && !matches!(
                target_bt,
                Some(
                    BaseType::SInt8
                        | BaseType::SInt16
                        | BaseType::SInt32
                        | BaseType::SInt64
                        | BaseType::Float32
                        | BaseType::Float64
                )
            )
        {
            return Value::UInt(comp_raw);
        }
        let target_raw = raw_from_f64(target_raw_f64, target_bt.unwrap_or(BaseType::UInt32));
        return raw_to_value_passthrough(&target_raw);
    }

    if !scale_offset::is_identity(comp_scale, comp_offset) {
        return Value::Float(physical);
    }

    if matches!(target_bt, Some(BaseType::Float32 | BaseType::Float64)) {
        Value::Float(physical)
    } else if matches!(
        target_bt,
        Some(BaseType::SInt8 | BaseType::SInt16 | BaseType::SInt32 | BaseType::SInt64)
    ) {
        Value::SInt(comp_raw as i64)
    } else {
        Value::UInt(comp_raw)
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

/// Resolve a developer field's raw bytes into a typed [`Value`] using the
/// registry entry's base type, scale, and offset.
fn resolve_dev_field(
    dev: &RawDevField<'_>,
    info: &dev_fields::DevFieldInfo,
    endian: Endian,
    options: TransformOptions,
) -> Value {
    let raw = match crate::raw_value::decode_value(
        info.base_type,
        &dev.bytes,
        endian,
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
                // `s` is `&'static str` from the codegen dispatcher — store
                // as `Cow::Borrowed` so the hot path stays alloc-free.
                return Value::Enum(Cow::Borrowed(s));
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
        U64Array(a) | U64zArray(a) => Value::Array(a.iter().map(|x| Value::UInt(*x)).collect()),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn build_fit_segments(records: &[u8]) -> Vec<u8> {
        let data_size = records.len() as u32;
        let mut bytes = vec![14u8, 0x20, 0xD0, 0x52];
        bytes.extend_from_slice(&data_size.to_le_bytes());
        bytes.extend_from_slice(b".FIT");
        let hcrc = crate::crc::calculate(&bytes[..12]);
        bytes.extend_from_slice(&hcrc.to_le_bytes());
        bytes.extend_from_slice(records);
        let fcrc = crate::crc::calculate(&bytes);
        bytes.extend_from_slice(&fcrc.to_le_bytes());
        bytes
    }

    #[test]
    fn collect_dev_meta_handles_signed_offset_zero_scale_and_missing_keys() {
        let mut reg = DevFieldRegistry::new();

        // Missing fdn 0 (developer_data_index) must be skipped rather than defaulting to 0.
        let missing_dev_idx = RawMessage {
            global_mesg_num: 206,
            fields: vec![
                RawField {
                    field_def_num: 1,
                    value: RawValue::U8Scalar(2),
                },
                RawField {
                    field_def_num: 2,
                    value: RawValue::U8Scalar(2), // uint8
                },
            ],
            dev_fields: vec![],
            starts_new_chain: false,
        };
        collect_dev_meta(&missing_dev_idx, &mut reg);
        assert!(reg.is_empty());

        // Missing fdn 1 (field_definition_number) must also be skipped.
        let missing_fdn = RawMessage {
            global_mesg_num: 206,
            fields: vec![RawField {
                field_def_num: 0,
                value: RawValue::U8Scalar(0),
            }],
            dev_fields: vec![],
            starts_new_chain: false,
        };
        collect_dev_meta(&missing_fdn, &mut reg);
        assert!(reg.is_empty());

        // scale = 0 must be filtered out, negative sint8 offset (-10) must be preserved.
        let signed_offset = RawMessage {
            global_mesg_num: 206,
            fields: vec![
                RawField {
                    field_def_num: 0,
                    value: RawValue::U8Scalar(1),
                },
                RawField {
                    field_def_num: 1,
                    value: RawValue::U8Scalar(3),
                },
                RawField {
                    field_def_num: 2,
                    value: RawValue::U8Scalar(0x02), // uint8
                },
                RawField {
                    field_def_num: 3,
                    value: RawValue::String("temp_delta".into()),
                },
                RawField {
                    field_def_num: 6,
                    value: RawValue::U8Scalar(0), // scale = 0 -> treated as None
                },
                RawField {
                    field_def_num: 7,
                    value: RawValue::I8Scalar(-10),
                },
            ],
            dev_fields: vec![],
            starts_new_chain: false,
        };
        collect_dev_meta(&signed_offset, &mut reg);
        let info = reg.get(1, 3).expect("field should be registered");
        assert_eq!(info.scale, None);
        assert_eq!(info.offset, Some(-10.0));

        // Ensure negative offset transforms raw 100 -> 100 / 1 - (-10) = 110.
        let val = resolve_dev_field(
            &RawDevField {
                field_def_num: 3,
                developer_data_index: 1,
                bytes: std::borrow::Cow::Borrowed(&[100u8]),
            },
            info,
            Endian::Little,
            TransformOptions::default(),
        );
        assert_eq!(val, Value::Float(110.0));
    }

    #[test]
    fn big_endian_dev_field_uses_message_endianness() {
        // BE definition for field_description (206), dev_idx=0, fdn=0, UInt16 (0x84), name="pts"
        // followed by BE definition+data for session (18) carrying dev field value 1234 (0x04D2).
        let mut recs = Vec::new();
        // Def 0: mesg 206 (BE)
        recs.extend_from_slice(&[
            0x40, 0x00, 0x01, 0x00, 0xCE, 0x04, // arch=BE, mesg=206, 4 fields
            0x00, 0x01, 0x02, // dev_idx
            0x01, 0x01, 0x02, // fdn
            0x02, 0x01, 0x02, // fit_base_type_id
            0x03, 0x04, 0x07, // field_name (4 bytes)
        ]);
        recs.extend_from_slice(&[0x00, 0, 0, 0x84, b'p', b't', b's', 0]);

        // Def 0 (with dev data): mesg 18 (BE), 0 std fields, 1 dev field (fdn=0, size=2, idx=0)
        recs.extend_from_slice(&[
            0x60, 0x00, 0x01, 0x00, 0x12, 0x00, // dev flag, arch=BE, mesg=18, 0 std fields
            0x01, // 1 dev field
            0x00, 0x02, 0x00, // fdn=0, size=2, dev_idx=0
        ]);
        recs.extend_from_slice(&[0x00, 0x04, 0xD2]); // 1234 in BE

        let fit = build_fit_segments(&recs);
        let (msgs, errs) = Decoder::builder(&fit).build().read_all();
        assert!(errs.is_empty(), "errors: {errs:?}");
        assert_eq!(msgs.len(), 2);
        let pts = msgs[1].field("pts").expect("pts dev field");
        assert_eq!(pts.value, Value::UInt(1234));
    }

    #[test]
    fn compressed_speed_distance_cascades_and_suppresses_parent_scale() {
        // Record (20) with fdn=8 compressed_speed_distance: [0x8B, 0x00, 0x08]
        // -> speed=139/100=1.39, distance=128/16=8.0, enhanced_speed=1.39
        // and compressed_speed_distance container keeps raw Value::Bytes (not divided by 100).
        let recs: Vec<u8> = vec![
            0x40, 0x00, 0x00, 0x14, 0x00, 0x01, 0x08, 0x03, 0x0D, // def record fdn=8 (3 byte)
            0x00, 0x8B, 0x00, 0x08,
        ];
        let fit = build_fit_segments(&recs);
        let (msgs, errs) = Decoder::builder(&fit).build().read_all();
        assert!(errs.is_empty(), "errors: {errs:?}");
        let rec = &msgs[0];
        assert_eq!(
            rec.field("compressed_speed_distance").unwrap().value,
            Value::Bytes(vec![0x8B, 0x00, 0x08])
        );
        match rec.field("speed").unwrap().value {
            Value::Float(v) => assert!((v - 1.39).abs() < 1e-9, "got speed={v}"),
            ref other => panic!("expected Float speed, got {other:?}"),
        }
        match rec.field("enhanced_speed").unwrap().value {
            Value::Float(v) => assert!((v - 1.39).abs() < 1e-9, "got enhanced_speed={v}"),
            ref other => panic!("expected Float enhanced_speed, got {other:?}"),
        }
        match rec.field("distance").unwrap().value {
            Value::Float(v) => assert!((v - 8.0).abs() < 1e-9, "got distance={v}"),
            ref other => panic!("expected Float distance, got {other:?}"),
        }
    }

    #[test]
    fn component_expansion_handles_uint8z_invalid_and_in_place_upsert() {
        // Record (20) with both altitude (fdn 2 = 3000 -> 100.0m) and
        // enhanced_altitude (fdn 78 = 3000 -> 100.0m). Expanding altitude into
        // enhanced_altitude must upsert cleanly rather than duplicating fields.
        let mut recs = vec![
            0x40, 0x00, 0x00, 0x14, 0x00, 0x02, // def record, 2 fields
            0x02, 0x02, 0x84, // altitude (u16)
            0x4E, 0x04, 0x86, // enhanced_altitude (u32, fdn=78)
            0x00,
        ];
        recs.extend_from_slice(&3000u16.to_le_bytes());
        recs.extend_from_slice(&3000u32.to_le_bytes());

        // Event (21) with event=0x2A (front_gear_change) and data=0x16_00_00_08
        // -> gear_change_data components (LSB-first): rear_gear_num=8, rear_gear=0 (uint8z invalid!),
        // front_gear_num=0 (uint8z invalid!), front_gear=0x16 (22).
        recs.extend_from_slice(&[
            0x41, 0x00, 0x00, 0x15, 0x00, 0x02, // def event (21), 2 fields
            0x00, 0x01, 0x00, // fdn=0 event (enum)
            0x03, 0x04, 0x86, // fdn=3 data (u32)
            0x01, 0x2A, // event = front_gear_change (42)
        ]);
        recs.extend_from_slice(&0x16_00_00_08u32.to_le_bytes());

        let fit = build_fit_segments(&recs);
        let (msgs, errs) = Decoder::builder(&fit).build().read_all();
        assert!(errs.is_empty(), "errors: {errs:?}");

        let ea_count = msgs[0]
            .fields
            .iter()
            .filter(|f| f.name == "enhanced_altitude")
            .count();
        assert_eq!(ea_count, 1, "enhanced_altitude must not be duplicated");

        let ev = &msgs[1];
        assert!(ev.field("gear_change_data").is_some());
        assert_eq!(ev.field("rear_gear_num").unwrap().value, Value::UInt(8));
        assert_eq!(ev.field("rear_gear").unwrap().value, Value::Invalid);
        assert_eq!(ev.field("front_gear_num").unwrap().value, Value::Invalid);
        assert_eq!(ev.field("front_gear").unwrap().value, Value::UInt(0x16));
    }

    #[test]
    fn hr_event_timestamp_seeds_and_groups_expanded_samples() {
        // Message 0: hr (132) anchor with fdn 9 event_timestamp = 10240 (10.0s)
        // Message 1: hr (132) delta with fdn 10 event_timestamp_12 (3 bytes = 2x 12-bit samples)
        //            sample 0 = 2048 (delta = 2048 - (10240 & 0xFFF) = 2048 - 2048 = 0 -> 10.0s)
        //            sample 1 = 3072 (delta = 1024 -> 11264 = 11.0s)
        let mut recs = vec![
            0x40, 0x00, 0x00, 0x84, 0x00, 0x01, 0x09, 0x04, 0x86, // hr fdn 9
            0x00,
        ];
        recs.extend_from_slice(&10240u32.to_le_bytes());

        recs.extend_from_slice(&[
            0x41, 0x00, 0x00, 0x84, 0x00, 0x01, 0x0A, 0x03, 0x0D, // hr fdn 10 (3 bytes)
            0x01,
        ]);
        // Two 12-bit values LSB-first: v0=0x800 (2048), v1=0xC00 (3072)
        // byte0 = 0x00, byte1 = (0x0 << 4) | 0x8 = 0x08, byte2 = 0xC0
        recs.extend_from_slice(&[0x00, 0x08, 0xC0]);

        let fit = build_fit_segments(&recs);
        let (msgs, errs) = Decoder::builder(&fit).build().read_all();
        assert!(errs.is_empty(), "errors: {errs:?}");
        let ts_field = msgs[1].field("event_timestamp").expect("event_timestamp");
        assert_eq!(
            ts_field.value,
            Value::Array(vec![Value::Float(10.0), Value::Float(11.0)])
        );
    }

    #[test]
    fn subfield_uses_own_scale_offset_and_units() {
        // workout_step (27): duration_type (fdn 1) = 1 (distance),
        // duration_value (fdn 2) = 400_000 -> SubField duration_distance (scale=100, units="m") = 4000.0
        let mut recs = vec![
            0x40, 0x00, 0x00, 0x1B, 0x00, 0x02, // def workout_step (27), 2 fields
            0x01, 0x01, 0x00, // duration_type (enum)
            0x02, 0x04, 0x86, // duration_value (uint32)
            0x00, 0x01, // duration_type = distance (1)
        ];
        recs.extend_from_slice(&400_000u32.to_le_bytes());

        let fit = build_fit_segments(&recs);
        let (msgs, errs) = Decoder::builder(&fit).build().read_all();
        assert!(errs.is_empty(), "errors: {errs:?}");
        let df = msgs[0]
            .field("duration_distance")
            .expect("duration_distance subfield");
        match df.value {
            Value::Float(v) => assert!((v - 4000.0).abs() < 1e-9, "got {v}"),
            ref other => panic!("expected Float duration_distance, got {other:?}"),
        }
        assert_eq!(df.units.as_deref(), Some("m"));
    }

    #[test]
    fn all_invalid_multi_element_expanded_group_collapses_to_invalid() {
        // ant_rx (80): mesg_data (fdn 2, byte[9]) unpacks channel_number (8b) and 8x data (8b).
        // When channel_number=1 and all 8 data bytes are 0xFF, mesg_data is NOT wire-invalid
        // (because byte 0 != 0xFF), so Pass 2 unpacks 8 elements of `data`, all invalid (0xFF),
        // and the group must collapse to Value::Invalid.
        let mut recs = vec![
            0x40, 0x00, 0x00, 0x50, 0x00, 0x01, // def ant_rx (80), 1 field
            0x02, 0x09, 0x0D, // mesg_data: 9 bytes
            0x00, // data record
            0x01, // channel_number = 1
        ];
        recs.extend_from_slice(&[0xFF; 8]); // all 8 data lanes invalid

        let fit = build_fit_segments(&recs);
        let (msgs, errs) = Decoder::builder(&fit).build().read_all();
        assert!(errs.is_empty(), "errors: {errs:?}");
        assert_eq!(msgs[0].field("channel_number").unwrap().value, Value::UInt(1));
        assert_eq!(msgs[0].field("data").unwrap().value, Value::Invalid);
    }
}
