//! MemoGlob reassembly — stitch multi-part text onto target messages.
//!
//! `memo_glob` messages (mesg 145) carry chunks of UTF-8 text that must be
//! concatenated (ordered by `part_index`) and written into a field on
//! another message identified by `(mesg_num, parent_index, field_num)`.
//!
//! Reference: JS SDK `utils-memo-glob.js`.

use std::collections::HashMap;

use crate::value::{Field, FieldKind, Message, Value};

/// Grouping key for memo_glob parts.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct MemoKey {
    mesg_num: u16,
    parent_index: u64,
    field_num: u8,
}

/// One collected memo_glob part.
struct MemoPart {
    part_index: u64,
    data: Vec<u8>,
}

/// Extract `data` (fdn 4) or `memo` (fdn 0) bytes from a memo_glob message.
fn extract_data(msg: &Message) -> Vec<u8> {
    // Prefer `data` (fdn 4), fall back to `memo` (fdn 0).
    for name in ["data", "memo"] {
        if let Some(field) = msg.field(name) {
            match &field.value {
                Value::Bytes(b) => return b.clone(),
                Value::Array(items) => {
                    return items
                        .iter()
                        .filter_map(|v| match v {
                            Value::UInt(n) => Some(*n as u8),
                            Value::Bytes(b) => Some(b[0]),
                            _ => None,
                        })
                        .collect();
                }
                _ => {}
            }
        }
    }
    Vec::new()
}

/// Extract a memo_glob's target `mesg_num` as u16.
fn extract_mesg_num(msg: &Message) -> Option<u16> {
    let field = msg.field("mesg_num")?;
    match &field.value {
        Value::UInt(v) => Some(*v as u16),
        // If enum resolution already converted it, we can't easily map back.
        // This shouldn't happen since mesg_num type is "mesg_num" which is an
        // enum whose values are the message numbers themselves.
        _ => None,
    }
}

/// Extract a u64 field value.
fn extract_u64(msg: &Message, name: &str) -> Option<u64> {
    msg.field(name).and_then(|f| f.value.as_u64())
}

/// Decode memo_glob parts and merge into target messages.
///
/// After calling this function, any message referenced by a `memo_glob`
/// will have the reassembled UTF-8 text written into the target field
/// (overwriting existing values).
pub fn decode_memo_glob(messages: &mut [Message]) {
    // Collect memo_glob data first to avoid borrow conflicts.
    let mut groups: HashMap<MemoKey, Vec<MemoPart>> = HashMap::new();

    for msg in messages.iter().filter(|m| m.global_mesg_num == 145) {
        let Some(mesg_num) = extract_mesg_num(msg) else {
            continue;
        };
        let Some(parent_index) = extract_u64(msg, "parent_index") else {
            continue;
        };
        let Some(field_num_raw) = extract_u64(msg, "field_num") else {
            continue;
        };
        let field_num = field_num_raw as u8;
        let part_index = extract_u64(msg, "part_index").unwrap_or(0);
        let data = extract_data(msg);

        let key = MemoKey {
            mesg_num,
            parent_index,
            field_num,
        };
        groups
            .entry(key)
            .or_default()
            .push(MemoPart { part_index, data });
    }

    if groups.is_empty() {
        return;
    }

    // Build a lookup: mesg_num → list of indices in `messages`.
    let mut mesg_indices: HashMap<u16, Vec<usize>> = HashMap::new();
    for (i, msg) in messages.iter().enumerate() {
        mesg_indices
            .entry(msg.global_mesg_num)
            .or_default()
            .push(i);
    }

    for (key, mut parts) in groups {
        // Sort by part_index.
        parts.sort_by_key(|p| p.part_index);

        // Concatenate data bytes.
        let mut combined: Vec<u8> = Vec::new();
        for part in &parts {
            combined.extend_from_slice(&part.data);
        }

        // Decode as UTF-8, trim trailing nulls.
        let text = {
            let end = combined.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
            String::from_utf8_lossy(&combined[..end]).into_owned()
        };

        // Find the target message.
        let Some(indices) = mesg_indices.get(&key.mesg_num) else {
            continue;
        };
        let Some(&msg_idx) = indices.get(key.parent_index as usize) else {
            continue;
        };

        let msg = &mut messages[msg_idx];

        // Look up the target field by field_def_num (from profile).
        // Since we know field_num, find any field with matching Standard kind.
        let target_field_name = crate::profile::mesg_info_by_num(key.mesg_num)
            .and_then(|info| {
                info.fields
                    .iter()
                    .find(|f| f.field_def_num == key.field_num)
                    .map(|f| f.name.to_string())
            })
            .unwrap_or_else(|| key.field_num.to_string());

        // Set or add the field.
        if let Some(existing) = msg.fields.iter_mut().find(|f| f.name == target_field_name) {
            existing.value = Value::String(text);
        } else {
            msg.fields.push(Field {
                name: target_field_name,
                kind: FieldKind::Standard {
                    field_def_num: key.field_num,
                },
                value: Value::String(text),
                units: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FieldKind;

    fn make_memo_glob_message(
        mesg_num: u16,
        parent_index: u64,
        field_num: u8,
        part_index: u64,
        data: &[u8],
    ) -> Message {
        Message {
            global_mesg_num: 145,
            name: "memo_glob",
            fields: vec![
                Field {
                    name: "mesg_num".to_string(),
                    kind: FieldKind::Standard { field_def_num: 1 },
                    value: Value::UInt(mesg_num as u64),
                    units: None,
                },
                Field {
                    name: "parent_index".to_string(),
                    kind: FieldKind::Standard { field_def_num: 2 },
                    value: Value::UInt(parent_index),
                    units: None,
                },
                Field {
                    name: "field_num".to_string(),
                    kind: FieldKind::Standard { field_def_num: 3 },
                    value: Value::UInt(field_num as u64),
                    units: None,
                },
                Field {
                    name: "part_index".to_string(),
                    kind: FieldKind::Standard { field_def_num: 250 },
                    value: Value::UInt(part_index),
                    units: None,
                },
                Field {
                    name: "data".to_string(),
                    kind: FieldKind::Standard { field_def_num: 4 },
                    value: Value::Bytes(data.to_vec()),
                    units: None,
                },
            ],
        }
    }

    fn make_target_message(name: &'static str, global_mesg_num: u16, field_name: &str, field_fdn: u8) -> Message {
        Message {
            global_mesg_num,
            name,
            fields: vec![Field {
                name: field_name.to_string(),
                kind: FieldKind::Standard { field_def_num: field_fdn },
                value: Value::String("to be overwritten".to_string()),
                units: None,
            }],
        }
    }

    #[test]
    fn memo_glob_simple_reassembly() {
        // Target: workout_step (mesg_num=27), parent_index=0, field_num=8 (notes).
        let mut messages = vec![
            make_target_message("workout_step", 27, "notes", 8),
            make_memo_glob_message(27, 0, 8, 0, b"hello!"),
        ];
        decode_memo_glob(&mut messages);
        let notes = messages[0].field("notes").unwrap();
        assert_eq!(notes.value, Value::String("hello!".to_string()));
    }

    #[test]
    fn memo_glob_multi_part_reassembly() {
        // Split "hello!" across 2 parts: "hel" + "lo!"
        let mut messages = vec![
            make_target_message("workout_step", 27, "notes", 8),
            make_memo_glob_message(27, 0, 8, 1, b"lo!"),
            make_memo_glob_message(27, 0, 8, 0, b"hel"),
        ];
        decode_memo_glob(&mut messages);
        let notes = messages[0].field("notes").unwrap();
        assert_eq!(notes.value, Value::String("hello!".to_string()));
    }

    #[test]
    fn memo_glob_trailing_nulls_trimmed() {
        let mut messages = vec![
            make_target_message("workout_step", 27, "notes", 8),
            make_memo_glob_message(27, 0, 8, 0, b"hi\0\0"),
        ];
        decode_memo_glob(&mut messages);
        let notes = messages[0].field("notes").unwrap();
        assert_eq!(notes.value, Value::String("hi".to_string()));
    }

    #[test]
    fn memo_glob_no_memo_messages_is_noop() {
        let mut messages = vec![make_target_message("workout_step", 27, "notes", 8)];
        decode_memo_glob(&mut messages);
        let notes = messages[0].field("notes").unwrap();
        assert_eq!(notes.value, Value::String("to be overwritten".to_string()));
    }

    #[test]
    fn memo_glob_missing_target_is_noop() {
        // memo_glob references mesg_num=999 which doesn't exist.
        let mut messages = vec![
            make_target_message("workout_step", 27, "notes", 8),
            make_memo_glob_message(999, 0, 8, 0, b"hello!"),
        ];
        decode_memo_glob(&mut messages);
        // Should not panic, target message unchanged.
        let notes = messages[0].field("notes").unwrap();
        assert_eq!(notes.value, Value::String("to be overwritten".to_string()));
    }

    #[test]
    fn memo_glob_creates_new_field_when_missing() {
        // memo_glob targets fdn=0 (wkt_step_name) on workout_step.
        // The target message has "notes" but not "wkt_step_name" — should be added.
        let mut messages = vec![
            make_target_message("workout_step", 27, "notes", 8),
            make_memo_glob_message(27, 0, 0, 0, b"name_val"),
        ];
        decode_memo_glob(&mut messages);
        // Profile maps fdn 0 → "wkt_step_name" for workout_step.
        let field = messages[0].field("wkt_step_name");
        assert!(field.is_some(), "new field wkt_step_name should be created");
        if let Some(f) = field {
            assert_eq!(f.value, Value::String("name_val".to_string()));
        }
    }
}
