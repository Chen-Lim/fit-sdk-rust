//! FIT encoder — converts [`Message`]s back to protocol-compliant binary.
//!
//! The encoder reverses the typed-decoder's transforms:
//! - `DateTime` → `datetime_to_fit()` → u32 wire bytes
//! - `Enum(name)` → `enum_value_by_str(type_name, name)` → base-type int bytes
//! - `Float(physical)` → `(v + offset) * scale` → integer bytes
//! - `String` / `Bytes` / `UInt` / `SInt` / `Bool` → direct wire encoding
//!
//! M9 capabilities: per-message Definition re-emission with 16-slot LRU
//! eviction, [`EncoderBuilder`] chaining, developer-field encoding (driven by
//! a [`DevFieldRegistry`] pre-built from `field_description` messages in the
//! input), and multi-segment chains via [`Encoder::encode_chain`].
//!
//! # Limitations
//!
//! - **Compressed-timestamp records are not re-emitted.** The decoder fully
//!   supports them (per protocol §2.3), but the encoder always writes regular
//!   Data records with an explicit `timestamp` field. Round-tripping a file
//!   that contained compressed-timestamp records produces a semantically
//!   identical, slightly larger file — not a byte-for-byte copy.

use std::collections::HashMap;

use crate::base_type::BaseType;
use crate::crc;
use crate::dev_fields::{base_type_to_type_name, DevFieldRegistry};
use crate::error::{FieldTooLargeKind, FitError};
use crate::output_stream::OutputStream;
use crate::profile;
use crate::value::{FieldKind, Message, Value};

// ────────────────────────────────────────────────────────────────────
// Public API: Encoder + EncoderBuilder
// ────────────────────────────────────────────────────────────────────

/// FIT file encoder.
#[derive(Debug, Clone)]
pub struct Encoder {
    protocol_version: u8,
    profile_version: u16,
}

impl Encoder {
    /// Build an encoder with the default protocol (0x20) and profile (21200) versions.
    pub fn new() -> Self {
        Self {
            protocol_version: 0x20,
            profile_version: 21200,
        }
    }

    /// Begin a chainable builder for non-default settings.
    pub fn builder() -> EncoderBuilder {
        EncoderBuilder::default()
    }

    /// Encode a single FIT segment (header + records + CRC).
    pub fn encode(&self, messages: &[Message]) -> Result<Vec<u8>, FitError> {
        let mut out = OutputStream::with_capacity(14 + messages.len() * 32);
        self.write_segment(&mut out, messages)?;
        Ok(out.into_bytes())
    }

    /// Encode multiple FIT segments back-to-back. Each segment gets its own
    /// header, definitions, and trailing CRC; the local-definition table is
    /// reset between segments (matching how chained `.fit` files are decoded).
    pub fn encode_chain(&self, segments: &[&[Message]]) -> Result<Vec<u8>, FitError> {
        if segments.is_empty() {
            return Ok(self.encode_empty());
        }
        let mut out = OutputStream::with_capacity(segments.iter().map(|s| s.len() * 32).sum());
        for seg in segments {
            self.write_segment(&mut out, seg)?;
        }
        Ok(out.into_bytes())
    }

    // ─── internals ─────────────────────────────────────────────────────

    fn write_segment(&self, out: &mut OutputStream, messages: &[Message]) -> Result<(), FitError> {
        if messages.is_empty() {
            self.write_empty_segment(out);
            return Ok(());
        }

        // Pre-pass: harvest developer-field schemas from any field_description
        // (mesg 206) messages in the input so we can size FieldKind::Developer
        // fields when we encounter them.
        let dev_registry = collect_dev_registry(messages);

        let segment_start = out.position();

        // 14-byte header with placeholders for data_size and header_crc.
        out.write_u8(14);
        out.write_u8(self.protocol_version);
        out.write_u16(self.profile_version);
        out.write_u32(0); // data_size placeholder (offset +4..+8)
        out.write_bytes(b".FIT");
        out.write_u16(0); // header CRC placeholder (offset +12..+14)

        let mut registry = LocalDefRegistry::new();
        let mut clock: u64 = 0;

        for msg in messages {
            clock += 1;
            let new_def = build_wire_def(msg, &dev_registry)?;
            let local = registry.acquire(msg.global_mesg_num, &new_def, clock)?;

            if registry.needs_redefinition(msg.global_mesg_num, &new_def) {
                write_definition_record(out, local, &new_def)?;
                registry.commit(msg.global_mesg_num, local, new_def.clone(), clock);
            }

            write_data_record(out, local, msg, &new_def, &dev_registry)?;
        }

        // Backpatch the segment header.
        let header_off = segment_start;
        let payload_len = out.position() - header_off - 14;
        let data_size =
            u32::try_from(payload_len).map_err(|_| FitError::SegmentTooLarge(payload_len))?;
        out.patch(header_off + 4, &data_size.to_le_bytes());

        let header_crc = crc::calculate(&out.as_slice()[header_off..header_off + 12]);
        out.patch(header_off + 12, &header_crc.to_le_bytes());

        let file_crc = crc::calculate(&out.as_slice()[header_off..]);
        out.write_u16(file_crc);
        Ok(())
    }

    fn write_empty_segment(&self, out: &mut OutputStream) {
        let segment_start = out.position();
        out.write_u8(14);
        out.write_u8(self.protocol_version);
        out.write_u16(self.profile_version);
        out.write_u32(0);
        out.write_bytes(b".FIT");
        let header_crc = crc::calculate(&out.as_slice()[segment_start..segment_start + 12]);
        out.write_u16(header_crc);
        let file_crc = crc::calculate(&out.as_slice()[segment_start..]);
        out.write_u16(file_crc);
    }

    fn encode_empty(&self) -> Vec<u8> {
        let mut out = OutputStream::with_capacity(16);
        self.write_empty_segment(&mut out);
        out.into_bytes()
    }
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Chainable builder for [`Encoder`]. Defaults match [`Encoder::new`].
#[derive(Debug, Clone)]
pub struct EncoderBuilder {
    protocol_version: u8,
    profile_version: u16,
}

impl Default for EncoderBuilder {
    fn default() -> Self {
        Self {
            protocol_version: 0x20,
            profile_version: 21200,
        }
    }
}

impl EncoderBuilder {
    /// Set the protocol version byte written at offset 1 of each segment header.
    pub fn protocol_version(mut self, v: u8) -> Self {
        self.protocol_version = v;
        self
    }

    /// Set the profile version word written at offsets 2..4 of each segment header.
    pub fn profile_version(mut self, v: u16) -> Self {
        self.profile_version = v;
        self
    }

    /// Finalise the configuration into an [`Encoder`].
    pub fn build(self) -> Encoder {
        Encoder {
            protocol_version: self.protocol_version,
            profile_version: self.profile_version,
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// Wire-level intermediate representation
// ────────────────────────────────────────────────────────────────────

/// Wire-level field definition used during encoding.
#[derive(Debug, Clone, PartialEq)]
struct WireField {
    field_def_num: u8,
    /// Byte width of this field on the wire (= element_size × count).
    size: u8,
    base_type: BaseType,
    base_type_byte: u8,
    /// Profile type name (e.g. `"manufacturer"`, `"file"`, `"sport"`,
    /// `"date_time"`, or `"uint16"`). Used to reverse `Value::Enum` lookups
    /// and to size enum-typed fields. Empty when no FieldInfo was found.
    type_name: &'static str,
    /// Profile scale factor (`physical = raw / scale - offset`). `None` ⇒ 1.0.
    scale: Option<f64>,
    /// Profile offset. `None` ⇒ 0.0.
    offset: Option<f64>,
}

/// Wire-level developer-field definition.
#[derive(Debug, Clone, PartialEq)]
struct WireDevField {
    field_def_num: u8,
    size: u8,
    developer_data_index: u8,
    base_type: BaseType,
    scale: Option<f64>,
    offset: Option<f64>,
}

/// Wire-level message definition used during encoding.
#[derive(Debug, Clone, PartialEq)]
struct WireDef {
    global_mesg_num: u16,
    fields: Vec<WireField>,
    dev_fields: Vec<WireDevField>,
}

/// Returns `true` if a [`Field`] with the given `(name, field_def_num)` is a
/// real wire field — its name must match the canonical `FieldInfo.name` or one
/// of its `SubField.name`s. Components-synthesised fields (e.g. `enhanced_speed`
/// derived from `speed`'s low 16 bits) carry a different name and must be
/// skipped: they're rebuilt on decode and re-emitting them would duplicate
/// `field_def_num` slots in the Definition record.
///
/// When `mesg_info` is `None` (unknown message), we accept all fields.
fn is_real_wire_field(
    field_name: &str,
    field_def_num: u8,
    mesg_info: Option<&profile::MesgInfo>,
) -> bool {
    let Some(info) = mesg_info else {
        return true;
    };
    let Some(real) = info.field(field_def_num) else {
        return false;
    };
    real.name == field_name || real.sub_fields.iter().any(|sf| sf.name == field_name)
}

/// Reject any `Value::String` (including elements inside `Value::Array`) that
/// contains an embedded NUL (`0x00`) byte, which cannot round-trip cleanly on
/// the FIT wire.
fn validate_no_embedded_nul(value: &Value) -> Result<(), FitError> {
    match value {
        Value::String(s) if s.as_bytes().contains(&0) => Err(FitError::EmbeddedNullInString),
        Value::Array(items) => {
            for item in items {
                validate_no_embedded_nul(item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Build a `WireDef` from a single [`Message`]'s fields, consulting both the
/// generated Profile metadata (for Standard fields) and the supplied developer
/// registry (for Developer fields).
fn build_wire_def(msg: &Message, dev_registry: &DevFieldRegistry) -> Result<WireDef, FitError> {
    let mesg_info = profile::mesg_info_by_num(msg.global_mesg_num);

    let mut fields = Vec::with_capacity(msg.fields.len());
    let mut dev_fields = Vec::new();
    let mut seen_fdns = std::collections::HashSet::<u8>::new();
    let mut seen_dev_keys = std::collections::HashSet::<(u8, u8)>::new();

    for field in &msg.fields {
        match field.kind {
            FieldKind::Standard { field_def_num } => {
                let fi = mesg_info.and_then(|m| m.field(field_def_num));

                if !is_real_wire_field(&field.name, field_def_num, mesg_info) {
                    continue;
                }
                if !seen_fdns.insert(field_def_num) {
                    continue;
                }

                validate_no_embedded_nul(&field.value)?;

                let (type_name, scale, offset, base_type) = match fi {
                    Some(f) => {
                        let (type_name, raw_scale, raw_offset, active_components) =
                            if let Some(sub) = f.sub_fields.iter().find(|sf| sf.name == field.name)
                            {
                                (sub.type_name, sub.scale, sub.offset, sub.components)
                            } else {
                                (f.type_name, f.scale, f.offset, f.components)
                            };
                        // Multi-component packed containers hold heterogeneous bit-fields;
                        // suppress parent-level scale/offset just like TypedDecoder does.
                        let (scale, offset) = if active_components.len() > 1 {
                            (None, None)
                        } else {
                            (raw_scale, raw_offset)
                        };
                        let base_type = BaseType::from_profile_type(type_name)
                            .or_else(|| BaseType::from_profile_type(f.type_name))
                            .unwrap_or_else(|| infer_base_type(&field.value));
                        (type_name, scale, offset, base_type)
                    }
                    None => ("", None, None, infer_base_type(&field.value)),
                };

                let base_type_byte = base_type.wire_byte();
                let size = compute_wire_size(base_type, &field.value)?;

                fields.push(WireField {
                    field_def_num,
                    size,
                    base_type,
                    base_type_byte,
                    type_name,
                    scale,
                    offset,
                });
            }
            FieldKind::Developer {
                field_def_num,
                developer_data_index,
            } => {
                // Resolve schema from the registry. Without it we cannot size
                // the field, so it has to be skipped.
                let Some(info) = dev_registry.get(developer_data_index, field_def_num) else {
                    continue;
                };
                if !seen_dev_keys.insert((developer_data_index, field_def_num)) {
                    continue;
                }
                validate_no_embedded_nul(&field.value)?;
                let size = compute_wire_size(info.base_type, &field.value)?;
                dev_fields.push(WireDevField {
                    field_def_num,
                    size,
                    developer_data_index,
                    base_type: info.base_type,
                    scale: info.scale,
                    offset: info.offset,
                });
            }
        }
    }

    Ok(WireDef {
        global_mesg_num: msg.global_mesg_num,
        fields,
        dev_fields,
    })
}

fn infer_base_type(value: &Value) -> BaseType {
    match value {
        Value::UInt(_) => BaseType::UInt32,
        Value::SInt(_) => BaseType::SInt32,
        Value::Float(_) => BaseType::Float64,
        Value::String(_) => BaseType::String,
        Value::Bytes(_) => BaseType::Byte,
        Value::Bool(_) => BaseType::UInt8,
        Value::Enum(_) => BaseType::Enum,
        Value::DateTime(_) => BaseType::UInt32,
        Value::Invalid => BaseType::UInt32,
        Value::Array(items) if !items.is_empty() => infer_base_type(&items[0]),
        Value::Array(_) => BaseType::UInt32,
    }
}

fn compute_wire_size(base_type: BaseType, value: &Value) -> Result<u8, FitError> {
    match base_type {
        BaseType::String => match value {
            Value::String(s) => {
                if s.as_bytes().contains(&0) {
                    return Err(FitError::EmbeddedNullInString);
                }
                let len = s.len().checked_add(1).ok_or(FitError::FieldTooLarge {
                    kind: FieldTooLargeKind::String,
                    size: s.len(),
                })?;
                u8::try_from(len).map_err(|_| FitError::FieldTooLarge {
                    kind: FieldTooLargeKind::String,
                    size: s.len(),
                })
            }
            Value::Array(items) => {
                if items.is_empty() {
                    return Ok(1);
                }
                let mut total = 0usize;
                for item in items {
                    let item_len = match item {
                        Value::String(s) => {
                            if s.as_bytes().contains(&0) {
                                return Err(FitError::EmbeddedNullInString);
                            }
                            s.len().checked_add(1).ok_or(FitError::FieldTooLarge {
                                kind: FieldTooLargeKind::String,
                                size: usize::MAX,
                            })?
                        }
                        _ => 1,
                    };
                    total = total.checked_add(item_len).ok_or(FitError::FieldTooLarge {
                        kind: FieldTooLargeKind::String,
                        size: usize::MAX,
                    })?;
                }
                u8::try_from(total).map_err(|_| FitError::FieldTooLarge {
                    kind: FieldTooLargeKind::String,
                    size: total,
                })
            }
            _ => Ok(1),
        },
        BaseType::Byte => match value {
            Value::Bytes(b) => {
                let size = b.len().max(1);
                u8::try_from(size).map_err(|_| FitError::FieldTooLarge {
                    kind: FieldTooLargeKind::ByteArray,
                    size: b.len(),
                })
            }
            Value::Array(items) => {
                let mut total = 0usize;
                for item in items {
                    let item_len = match item {
                        Value::Bytes(b) => b.len(),
                        _ => 1,
                    };
                    total = total.checked_add(item_len).ok_or(FitError::FieldTooLarge {
                        kind: FieldTooLargeKind::ByteArray,
                        size: usize::MAX,
                    })?;
                }
                let size = total.max(1);
                u8::try_from(size).map_err(|_| FitError::FieldTooLarge {
                    kind: FieldTooLargeKind::ByteArray,
                    size: total,
                })
            }
            _ => Ok(1),
        },
        _ => {
            let element_size = base_type.element_size();
            let count = match value {
                Value::Array(a) => a.len().max(1),
                _ => 1,
            };
            let total_bytes = element_size.checked_mul(count).ok_or(FitError::FieldTooLarge {
                kind: FieldTooLargeKind::Array,
                size: usize::MAX,
            })?;
            u8::try_from(total_bytes).map_err(|_| FitError::FieldTooLarge {
                kind: FieldTooLargeKind::Array,
                size: total_bytes,
            })
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// Local-definition registry with 16-slot LRU eviction
// ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct RegisteredDef {
    mesg_num: u16,
    wire_def: WireDef,
    last_used: u64,
}

/// Tracks the protocol-mandated 4-bit local-message-number table. Up to 16
/// distinct definitions can be live at once; when a 17th unique mesg_num
/// arrives, the least-recently-used slot is evicted and reused.
#[derive(Debug, Default)]
struct LocalDefRegistry {
    /// Slot table: indexed by local_mesg_num (0..=15). `None` means free.
    slots: [Option<RegisteredDef>; 16],
    /// Mesg-num → local-num index for fast lookup.
    by_mesg_num: HashMap<u16, u8>,
    /// `true` after `acquire` if the matching slot already holds an equivalent
    /// WireDef; `false` if the caller should re-emit a Definition record.
    last_acquire_was_match: bool,
}

impl LocalDefRegistry {
    fn new() -> Self {
        Self::default()
    }

    /// Reserve a local_mesg_num for `(mesg_num, wire_def)`, allocating a fresh
    /// slot or evicting the LRU entry as needed. Caller must follow with
    /// [`Self::commit`] *only* when [`Self::needs_redefinition`] reports `true`.
    fn acquire(&mut self, mesg_num: u16, wire_def: &WireDef, clock: u64) -> Result<u8, FitError> {
        if let Some(&local) = self.by_mesg_num.get(&mesg_num) {
            let slot = self.slots[local as usize]
                .as_mut()
                .expect("by_mesg_num invariant");
            self.last_acquire_was_match = slot.wire_def == *wire_def;
            slot.last_used = clock;
            return Ok(local);
        }
        // Fresh registration — find a free slot first.
        if let Some(free) = self.slots.iter().position(Option::is_none) {
            self.last_acquire_was_match = false;
            return Ok(free as u8);
        }
        // All 16 slots in use — evict LRU.
        let (lru_local, lru_mesg) = self
            .slots
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| s.as_ref().map(|d| d.last_used).unwrap_or(u64::MAX))
            .map(|(i, s)| (i as u8, s.as_ref().expect("all slots occupied").mesg_num))
            .ok_or(FitError::TooManyLocalDefinitions(17))?;
        self.by_mesg_num.remove(&lru_mesg);
        self.slots[lru_local as usize] = None;
        self.last_acquire_was_match = false;
        Ok(lru_local)
    }

    /// `true` iff the caller must emit a fresh Definition record before the
    /// upcoming Data record (either the slot is fresh, or the WireDef changed).
    fn needs_redefinition(&self, mesg_num: u16, wire_def: &WireDef) -> bool {
        match self.by_mesg_num.get(&mesg_num) {
            Some(&local) => self.slots[local as usize]
                .as_ref()
                .map(|d| d.wire_def != *wire_def)
                .unwrap_or(true),
            None => true,
        }
    }

    /// Record that a Definition record has been emitted for `(mesg_num, local)`
    /// with the supplied `wire_def`. Future identical messages will reuse the
    /// slot without re-emission.
    fn commit(&mut self, mesg_num: u16, local: u8, wire_def: WireDef, clock: u64) {
        self.slots[local as usize] = Some(RegisteredDef {
            mesg_num,
            wire_def,
            last_used: clock,
        });
        self.by_mesg_num.insert(mesg_num, local);
    }
}

// ────────────────────────────────────────────────────────────────────
// Developer-field registry harvesting
// ────────────────────────────────────────────────────────────────────

/// Walk `messages` once and harvest a [`DevFieldRegistry`] from any
/// `field_description` (mesg 206) messages. Mirrors what the typed decoder
/// does inline; needed up-front by the encoder so that subsequent messages
/// with `FieldKind::Developer` fields can be sized.
fn collect_dev_registry(messages: &[Message]) -> DevFieldRegistry {
    let mut reg = DevFieldRegistry::new();
    for m in messages {
        if m.global_mesg_num != 206 {
            continue;
        }
        let dev_idx = m.field("developer_data_index").and_then(value_as_u8);
        let fdn = m.field("field_definition_number").and_then(value_as_u8);
        // `fit_base_type_id` is enum-typed in Profile; the typed decoder may
        // surface it as either `Value::UInt(_)` or `Value::Enum("uint8")` /
        // `Value::Enum("float32")` etc. depending on toggles.
        let bt_id = m.field("fit_base_type_id").and_then(|f| match &f.value {
            Value::UInt(v) => u8::try_from(*v).ok(),
            Value::SInt(v) => u8::try_from(*v).ok(),
            Value::Enum(name) => {
                crate::transforms::enum_strings::enum_value_by_str("fit_base_type", name)
                    .and_then(|v| u8::try_from(v).ok())
            }
            _ => None,
        });
        let name = m
            .field("field_name")
            .and_then(|f| match &f.value {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let scale = m.field("scale").and_then(|f| match &f.value {
            Value::Float(v) => Some(*v),
            Value::UInt(v) => Some(*v as f64),
            _ => None,
        });
        let offset = m.field("offset").and_then(|f| match &f.value {
            Value::Float(v) => Some(*v),
            Value::SInt(v) => Some(*v as f64),
            _ => None,
        });
        let units = m.field("units").and_then(|f| match &f.value {
            Value::String(s) => Some(s.clone()),
            _ => None,
        });
        if let (Some(dev_idx), Some(fdn), Some(bt_id)) = (dev_idx, fdn, bt_id) {
            reg.register_field(dev_idx, fdn, name, bt_id, scale, offset, units);
        }
    }
    reg
}

fn value_as_u8(f: &crate::value::Field) -> Option<u8> {
    match &f.value {
        Value::UInt(v) => u8::try_from(*v).ok(),
        Value::SInt(v) => u8::try_from(*v).ok(),
        _ => None,
    }
}

// ────────────────────────────────────────────────────────────────────
// Wire emission
// ────────────────────────────────────────────────────────────────────

fn write_definition_record(
    out: &mut OutputStream,
    local_mesg_num: u8,
    def: &WireDef,
) -> Result<(), FitError> {
    let header = if def.dev_fields.is_empty() {
        0x40 | (local_mesg_num & 0x0F)
    } else {
        // Bit 5 signals developer-field definitions follow.
        0x40 | 0x20 | (local_mesg_num & 0x0F)
    };
    out.write_u8(header);
    out.write_u8(0x00); // reserved
    out.write_u8(0x00); // architecture: little-endian
    out.write_u16(def.global_mesg_num);
    let field_count = u8::try_from(def.fields.len()).map_err(|_| FitError::FieldTooLarge {
        kind: FieldTooLargeKind::FieldList,
        size: def.fields.len(),
    })?;
    out.write_u8(field_count);
    for f in &def.fields {
        out.write_u8(f.field_def_num);
        out.write_u8(f.size);
        out.write_u8(f.base_type_byte);
    }
    if !def.dev_fields.is_empty() {
        let dev_count =
            u8::try_from(def.dev_fields.len()).map_err(|_| FitError::FieldTooLarge {
                kind: FieldTooLargeKind::DevFieldList,
                size: def.dev_fields.len(),
            })?;
        out.write_u8(dev_count);
        for d in &def.dev_fields {
            out.write_u8(d.field_def_num);
            out.write_u8(d.size);
            out.write_u8(d.developer_data_index);
        }
    }
    Ok(())
}

fn write_data_record(
    out: &mut OutputStream,
    local_mesg_num: u8,
    msg: &Message,
    def: &WireDef,
    dev_registry: &DevFieldRegistry,
) -> Result<(), FitError> {
    out.write_u8(local_mesg_num & 0x0F); // data record header
    let mesg_info = profile::mesg_info_by_num(msg.global_mesg_num);

    // Standard fields, in Definition order.
    for wf in &def.fields {
        let value = msg
            .fields
            .iter()
            .find(|f| {
                let FieldKind::Standard { field_def_num } = f.kind else {
                    return false;
                };
                field_def_num == wf.field_def_num
                    && is_real_wire_field(&f.name, field_def_num, mesg_info)
            })
            .map(|f| &f.value);
        encode_field_value(out, value, wf)?;
    }

    // Developer fields, in Definition order.
    for wd in &def.dev_fields {
        let value = msg
            .fields
            .iter()
            .find(|f| {
                matches!(f.kind, FieldKind::Developer { field_def_num, developer_data_index }
                if field_def_num == wd.field_def_num
                    && developer_data_index == wd.developer_data_index)
            })
            .map(|f| &f.value);
        encode_dev_field_value(out, value, wd, dev_registry)?;
    }
    Ok(())
}

fn encode_field_value(
    out: &mut OutputStream,
    value: Option<&Value>,
    wf: &WireField,
) -> Result<(), FitError> {
    let Some(value) = value else {
        write_invalid(out, wf.base_type, wf.size);
        return Ok(());
    };
    encode_value_inner(
        out,
        value,
        wf.base_type,
        wf.size,
        wf.type_name,
        wf.scale,
        wf.offset,
    )
}

fn encode_dev_field_value(
    out: &mut OutputStream,
    value: Option<&Value>,
    wd: &WireDevField,
    _registry: &DevFieldRegistry,
) -> Result<(), FitError> {
    let Some(value) = value else {
        write_invalid(out, wd.base_type, wd.size);
        return Ok(());
    };
    encode_value_inner(
        out,
        value,
        wd.base_type,
        wd.size,
        base_type_to_type_name(wd.base_type),
        wd.scale,
        wd.offset,
    )
}

/// Write trailing invalid elements if a field's `size` exceeds the bytes written.
fn pad_trailing_invalid(out: &mut OutputStream, base_type: BaseType, start: usize, size: u8) {
    let written = out.position() - start;
    if written < size as usize {
        write_invalid(out, base_type, (size as usize - written) as u8);
    }
}

/// Common encoder for both standard and developer fields. `type_name` is used
/// only for `Value::Enum` reverse lookups; `scale`/`offset` flip the M5 forward
/// transform for `Value::Float` on integer base types.
fn encode_value_inner(
    out: &mut OutputStream,
    value: &Value,
    base_type: BaseType,
    size: u8,
    type_name: &str,
    scale: Option<f64>,
    offset: Option<f64>,
) -> Result<(), FitError> {
    let start = out.position();
    match value {
        Value::Invalid => write_invalid(out, base_type, size),
        Value::Bool(b) => {
            if base_type == BaseType::String {
                write_invalid(out, base_type, size);
            } else {
                encode_int(out, i128::from(*b), base_type);
                pad_trailing_invalid(out, base_type, start, size);
            }
        }
        Value::UInt(v) => {
            if base_type == BaseType::String {
                write_invalid(out, base_type, size);
            } else {
                encode_int(out, i128::from(*v), base_type);
                pad_trailing_invalid(out, base_type, start, size);
            }
        }
        Value::SInt(v) => {
            if base_type == BaseType::String {
                write_invalid(out, base_type, size);
            } else {
                encode_int(out, i128::from(*v), base_type);
                pad_trailing_invalid(out, base_type, start, size);
            }
        }
        Value::Float(v) => {
            if base_type == BaseType::String {
                write_invalid(out, base_type, size);
            } else {
                encode_float(out, *v, base_type, scale, offset);
                pad_trailing_invalid(out, base_type, start, size);
            }
        }
        Value::String(s) => {
            if s.as_bytes().contains(&0) {
                return Err(FitError::EmbeddedNullInString);
            }
            if base_type == BaseType::String {
                let max = size as usize;
                let take = s.len().min(max.saturating_sub(1));
                out.write_bytes(&s.as_bytes()[..take]);
                if max > 0 {
                    out.write_u8(0x00);
                }
                for _ in (out.position() - start)..max {
                    out.write_u8(0x00);
                }
            } else {
                write_invalid(out, base_type, size);
            }
        }
        Value::Bytes(b) => {
            if base_type == BaseType::Byte {
                let take = b.len().min(size as usize);
                out.write_bytes(&b[..take]);
                for _ in take..size as usize {
                    out.write_u8(0xFF);
                }
            } else {
                write_invalid(out, base_type, size);
            }
        }
        Value::Enum(name) => {
            if base_type == BaseType::String {
                write_invalid(out, base_type, size);
            } else {
                let lookup_name = if type_name.is_empty() {
                    "enum"
                } else {
                    type_name
                };
                if let Some(v) =
                    crate::transforms::enum_strings::enum_value_by_str(lookup_name, name)
                {
                    encode_int(out, i128::from(v), base_type);
                    pad_trailing_invalid(out, base_type, start, size);
                } else {
                    write_invalid(out, base_type, size);
                }
            }
        }
        Value::DateTime(dt) => {
            if base_type == BaseType::String {
                write_invalid(out, base_type, size);
            } else {
                #[cfg(feature = "chrono")]
                let secs = crate::datetime::datetime_to_fit(*dt);
                #[cfg(not(feature = "chrono"))]
                let secs = Some(*dt);
                if let Some(secs) = secs {
                    encode_int(out, i128::from(secs), base_type);
                    pad_trailing_invalid(out, base_type, start, size);
                } else {
                    write_invalid(out, base_type, size);
                }
            }
        }
        Value::Array(items) => {
            if base_type == BaseType::String {
                for item in items {
                    let written = out.position() - start;
                    if written >= size as usize {
                        break;
                    }
                    let rem = size as usize - written;
                    match item {
                        Value::String(s) => {
                            if s.as_bytes().contains(&0) {
                                return Err(FitError::EmbeddedNullInString);
                            }
                            let take = s.len().min(rem - 1);
                            out.write_bytes(&s.as_bytes()[..take]);
                            out.write_u8(0x00);
                        }
                        _ => out.write_u8(0x00),
                    }
                }
                pad_trailing_invalid(out, base_type, start, size);
            } else if base_type == BaseType::Byte {
                for item in items {
                    let written = out.position() - start;
                    if written >= size as usize {
                        break;
                    }
                    let rem = size as usize - written;
                    match item {
                        Value::Bytes(b) => out.write_bytes(&b[..b.len().min(rem)]),
                        Value::UInt(v) => out.write_u8(u8::try_from(*v).unwrap_or(0xFF)),
                        Value::SInt(v) => out.write_u8(u8::try_from(*v).unwrap_or(0xFF)),
                        _ => out.write_u8(0xFF),
                    }
                }
                pad_trailing_invalid(out, base_type, start, size);
            } else {
                let elem_sz = base_type.element_size();
                let element_size = elem_sz as u8;
                let max_items = size as usize / elem_sz;
                for item in items.iter().take(max_items) {
                    if matches!(item, Value::Array(_)) {
                        write_invalid(out, base_type, element_size);
                    } else {
                        encode_value_inner(
                            out,
                            item,
                            base_type,
                            element_size,
                            type_name,
                            scale,
                            offset,
                        )?;
                    }
                }
                pad_trailing_invalid(out, base_type, start, size);
            }
        }
    }
    Ok(())
}

fn encode_float(
    out: &mut OutputStream,
    v: f64,
    base_type: BaseType,
    scale: Option<f64>,
    offset: Option<f64>,
) {
    match base_type {
        BaseType::Float32 => out.write_u32((v as f32).to_bits()),
        BaseType::Float64 => out.write_u64(v.to_bits()),
        _ => {
            let offset = offset.unwrap_or(0.0);
            let scale = scale.unwrap_or(1.0);
            let scaled = (v + offset) * scale;
            let size = base_type.element_size() as u8;
            // NaN/Infinity → invalid sentinel (saturating `as i128` would
            // map NaN → 0, silently corrupting output).
            if !scaled.is_finite() {
                write_invalid(out, base_type, size);
                return;
            }
            let as_i128 = scaled.round() as i128;
            let (lo, hi) = base_type_int_range(base_type);
            if as_i128 < lo || as_i128 > hi {
                write_invalid(out, base_type, size);
                return;
            }
            encode_int(out, as_i128, base_type);
        }
    }
}

/// Inclusive `[min, max]` range of an integer base type, in `i128`. Used by
/// [`encode_int`] and [`encode_float`] to reject out-of-range values that would
/// silently wrap on cast. Float / String bases are not integer-shaped; we
/// return a degenerate range (they are routed through other paths).
fn base_type_int_range(base_type: BaseType) -> (i128, i128) {
    match base_type {
        BaseType::Enum | BaseType::UInt8 | BaseType::UInt8z | BaseType::Byte => {
            (0, u8::MAX as i128)
        }
        BaseType::UInt16 | BaseType::UInt16z => (0, u16::MAX as i128),
        BaseType::UInt32 | BaseType::UInt32z => (0, u32::MAX as i128),
        BaseType::UInt64 | BaseType::UInt64z => (0, u64::MAX as i128),
        BaseType::SInt8 => (i8::MIN as i128, i8::MAX as i128),
        BaseType::SInt16 => (i16::MIN as i128, i16::MAX as i128),
        BaseType::SInt32 => (i32::MIN as i128, i32::MAX as i128),
        BaseType::SInt64 => (i64::MIN as i128, i64::MAX as i128),
        BaseType::Float32 | BaseType::Float64 | BaseType::String => (0, 0),
    }
}

fn encode_int(out: &mut OutputStream, v: i128, base_type: BaseType) {
    match base_type {
        BaseType::Float32 => out.write_u32((v as f32).to_bits()),
        BaseType::Float64 => out.write_u64((v as f64).to_bits()),
        BaseType::String => {} // strings handled separately
        _ => {
            let (lo, hi) = base_type_int_range(base_type);
            if v < lo || v > hi {
                write_invalid(out, base_type, base_type.element_size() as u8);
                return;
            }
            match base_type {
                BaseType::Enum | BaseType::UInt8 | BaseType::UInt8z | BaseType::Byte => {
                    out.write_u8(v as u8);
                }
                BaseType::UInt16 | BaseType::UInt16z => out.write_u16(v as u16),
                BaseType::UInt32 | BaseType::UInt32z => out.write_u32(v as u32),
                BaseType::UInt64 | BaseType::UInt64z => out.write_u64(v as u64),
                BaseType::SInt8 => out.write_u8(v as i8 as u8),
                BaseType::SInt16 => out.write_i16(v as i16),
                BaseType::SInt32 => out.write_i32(v as i32),
                BaseType::SInt64 => out.write_i64(v as i64),
                BaseType::Float32 | BaseType::Float64 | BaseType::String => unreachable!(),
            }
        }
    }
}

fn write_invalid(out: &mut OutputStream, base_type: BaseType, size: u8) {
    let element_size = base_type.element_size().max(1);
    let count = (size as usize / element_size).max(1);
    match base_type {
        BaseType::Enum | BaseType::UInt8 | BaseType::Byte => {
            for _ in 0..count {
                out.write_u8(0xFF);
            }
        }
        BaseType::UInt8z => {
            for _ in 0..count {
                out.write_u8(0x00);
            }
        }
        BaseType::SInt8 => {
            for _ in 0..count {
                out.write_u8(0x7F);
            }
        }
        BaseType::UInt16 => {
            for _ in 0..count {
                out.write_u16(0xFFFF);
            }
        }
        BaseType::UInt16z => {
            for _ in 0..count {
                out.write_u16(0x0000);
            }
        }
        BaseType::SInt16 => {
            for _ in 0..count {
                out.write_i16(i16::MAX);
            }
        }
        BaseType::UInt32 | BaseType::Float32 => {
            for _ in 0..count {
                out.write_u32(0xFFFF_FFFF);
            }
        }
        BaseType::UInt32z => {
            for _ in 0..count {
                out.write_u32(0);
            }
        }
        BaseType::SInt32 => {
            for _ in 0..count {
                out.write_i32(i32::MAX);
            }
        }
        BaseType::UInt64 | BaseType::Float64 => {
            for _ in 0..count {
                out.write_u64(0xFFFF_FFFF_FFFF_FFFF);
            }
        }
        BaseType::UInt64z => {
            for _ in 0..count {
                out.write_u64(0);
            }
        }
        BaseType::SInt64 => {
            for _ in 0..count {
                out.write_i64(i64::MAX);
            }
        }
        BaseType::String => {
            for _ in 0..count {
                out.write_u8(0x00);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_empty_produces_valid_header() {
        let bytes = Encoder::new().encode(&[]).unwrap();
        assert_eq!(bytes.len(), 16);
        assert_eq!(&bytes[8..12], b".FIT");
        assert_eq!(bytes[0], 14);
        crate::check_integrity(&bytes).unwrap();
    }

    #[test]
    fn encoder_builder_overrides_versions() {
        let enc = Encoder::builder()
            .protocol_version(0x21)
            .profile_version(21300)
            .build();
        let bytes = enc.encode(&[]).unwrap();
        assert_eq!(bytes[1], 0x21);
        assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), 21300);
        crate::check_integrity(&bytes).unwrap();
    }

    #[test]
    fn lru_registry_evicts_least_recently_used() {
        let mut reg = LocalDefRegistry::new();
        // Fill all 16 slots.
        for i in 0..16u16 {
            let def = WireDef {
                global_mesg_num: i,
                fields: vec![],
                dev_fields: vec![],
            };
            let local = reg.acquire(i, &def, i as u64 + 1).unwrap();
            reg.commit(i, local, def, i as u64 + 1);
        }
        // Touch mesg_nums 1..16 with later clock values; mesg_num 0 stays the
        // oldest.
        for i in 1..16u16 {
            let def = WireDef {
                global_mesg_num: i,
                fields: vec![],
                dev_fields: vec![],
            };
            reg.acquire(i, &def, 100 + i as u64).unwrap();
        }
        // Adding a 17th must evict mesg_num 0 and re-use slot 0.
        let new_def = WireDef {
            global_mesg_num: 999,
            fields: vec![],
            dev_fields: vec![],
        };
        let local = reg.acquire(999, &new_def, 200).unwrap();
        assert_eq!(local, 0);
        reg.commit(999, local, new_def, 200);
        assert!(reg.by_mesg_num.contains_key(&999));
        assert!(!reg.by_mesg_num.contains_key(&0));
    }

    #[test]
    fn compute_wire_size_rejects_array_byte_overflow_without_wrapping() {
        let u64_32 = Value::Array(vec![Value::UInt(1); 32]);
        assert!(matches!(
            compute_wire_size(BaseType::UInt64, &u64_32),
            Err(FitError::FieldTooLarge {
                kind: FieldTooLargeKind::Array,
                size: 256,
            })
        ));

        let u32_64 = Value::Array(vec![Value::UInt(1); 64]);
        assert!(matches!(
            compute_wire_size(BaseType::UInt32, &u32_64),
            Err(FitError::FieldTooLarge {
                kind: FieldTooLargeKind::Array,
                size: 256,
            })
        ));

        let u32_63 = Value::Array(vec![Value::UInt(1); 63]);
        assert_eq!(compute_wire_size(BaseType::UInt32, &u32_63).unwrap(), 252);
    }

    #[test]
    fn encode_int_writes_invalid_sentinel_when_out_of_range() {
        let cases = [
            (256i128, BaseType::UInt8, vec![0xFF]),
            (-1i128, BaseType::UInt8, vec![0xFF]),
            (256i128, BaseType::UInt8z, vec![0x00]),
            (-129i128, BaseType::SInt8, vec![0x7F]),
            (128i128, BaseType::SInt8, vec![0x7F]),
            (65536i128, BaseType::UInt16, vec![0xFF, 0xFF]),
            (-1i128, BaseType::UInt16z, vec![0x00, 0x00]),
            (32768i128, BaseType::SInt16, vec![0xFF, 0x7F]),
        ];
        for (val, bt, expected) in cases {
            let mut out = OutputStream::new();
            encode_int(&mut out, val, bt);
            assert_eq!(
                out.as_slice(),
                expected.as_slice(),
                "val={val} bt={bt:?}"
            );
        }
    }

    #[test]
    fn build_wire_def_resolves_subfield_and_suppresses_multi_component_scale() {
        // workout_step (27): fdn 2 SubField "duration_distance" has scale=100.
        let step = Message {
            global_mesg_num: 27,
            name: "workout_step",
            fields: vec![crate::Field {
                name: "duration_distance".to_string(),
                kind: FieldKind::Standard { field_def_num: 2 },
                value: Value::Float(800.0),
                units: Some("m".to_string()),
            }],
        };
        let def = build_wire_def(&step, &DevFieldRegistry::new()).unwrap();
        assert_eq!(def.fields[0].scale, Some(100.0));
        assert_eq!(def.fields[0].base_type, BaseType::UInt32);
        assert_eq!(def.fields[0].base_type_byte, 0x86);

        // record (20): fdn 8 "compressed_speed_distance" has 2 components -> suppress scale.
        let rec = Message {
            global_mesg_num: 20,
            name: "record",
            fields: vec![crate::Field {
                name: "compressed_speed_distance".to_string(),
                kind: FieldKind::Standard { field_def_num: 8 },
                value: Value::Bytes(vec![0x8B, 0x10, 0x00]),
                units: None,
            }],
        };
        let rec_def = build_wire_def(&rec, &DevFieldRegistry::new()).unwrap();
        assert_eq!(rec_def.fields[0].scale, None);
        assert_eq!(rec_def.fields[0].offset, None);
        assert_eq!(rec_def.fields[0].base_type, BaseType::Byte);
    }

    #[test]
    fn write_invalid_pads_full_string_width() {
        let mut out = OutputStream::new();
        write_invalid(&mut out, BaseType::String, 4);
        assert_eq!(out.as_slice(), &[0, 0, 0, 0]);
    }
}
