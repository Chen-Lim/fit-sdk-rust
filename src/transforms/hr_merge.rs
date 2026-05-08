//! Heart-rate merge — interpolate HR samples onto record messages.
//!
//! Implements the algorithm from the FIT protocol: expand `hr` messages
//! (mesg 132) into a flat time-series of `{timestamp, heart_rate}` samples,
//! then average samples that fall within each record message's time range.
//!
//! The gap-fill step carries the previous HR value forward in 250 ms
//! increments for gaps up to 5 seconds.
//!
//! Reference: JS SDK `utils-hr-mesg.js`.

use chrono::{DateTime, Utc};

use crate::datetime;
use crate::value::{Message, Value};

/// Gap-fill parameters (matching JS SDK).
const GAP_INCREMENT_MS: f64 = 250.0;
const GAP_MAX_MS: f64 = 5000.0;
const GAP_MAX_STEPS: usize = (GAP_MAX_MS / GAP_INCREMENT_MS) as usize; // 20

/// Rollover threshold for 22-bit event timestamps (0x400000 = 4194304).
const EVENT_TS_ROLLOVER: f64 = 4194304.0;

/// One expanded HR sample (timestamp in seconds since FIT epoch).
#[derive(Debug, Clone)]
struct HrSample {
    timestamp: f64,
    heart_rate: u8,
}

/// Convert a `DateTime<Utc>` to seconds since the FIT epoch
/// (1989-12-31T00:00:00Z).
fn datetime_to_fit_secs(dt: &DateTime<Utc>) -> f64 {
    (dt.timestamp() - datetime::FIT_EPOCH_OFFSET_SECS) as f64
}

/// Extract seconds since FIT epoch from a message field that is a DateTime.
fn field_datetime_secs(msg: &Message, name: &str) -> Option<f64> {
    msg.field(name)
        .and_then(|f| f.value.as_datetime())
        .map(|dt| datetime_to_fit_secs(&dt))
}

/// Get a field's value as f64 (covers Float, UInt, SInt).
fn field_as_f64(msg: &Message, name: &str) -> Option<f64> {
    msg.field(name).and_then(|f| f.value.as_f64())
}

/// Get a field as a vector of f64 values. Handles Array(UInt/Float) and
/// single-element scalars.
fn field_as_f64_vec(msg: &Message, name: &str) -> Option<Vec<f64>> {
    let val = msg.field(name)?.value.clone();
    match val {
        Value::Array(items) => Some(items.iter().filter_map(|v| v.as_f64()).collect()),
        v => v.as_f64().map(|x| vec![x]),
    }
}

/// Expand HR messages into a flat time-series.
///
/// Each `hr` message contributes its `event_timestamp` values (with
/// corresponding `filtered_bpm` values) anchored to the message's
/// `timestamp` + `fractional_timestamp`. Gaps > 250 ms are filled with
/// up to 20 intermediate samples carrying the previous HR forward.
fn expand_heart_rates(hr_messages: &[&Message]) -> Vec<HrSample> {
    if hr_messages.is_empty() {
        return Vec::new();
    }

    let mut anchor_timestamp: Option<f64> = None;
    let mut anchor_event_ts: Option<f64> = None;
    let mut samples: Vec<HrSample> = Vec::new();

    for hr_msg in hr_messages {
        // Update anchor if this message has a timestamp.
        if field_datetime_secs(hr_msg, "timestamp").is_some() {
            let ts_secs = field_datetime_secs(hr_msg, "timestamp").unwrap();
            let frac = field_as_f64(hr_msg, "fractional_timestamp").unwrap_or(0.0);
            anchor_timestamp = Some(ts_secs + frac);

            // Anchor event timestamp: only when exactly 1 event_timestamp is present.
            // Per JS SDK: anchor mesg must have 1 event_timestamp to be valid.
            if let Some(evts) = field_as_f64_vec(hr_msg, "event_timestamp") {
                if evts.len() == 1 {
                    anchor_event_ts = Some(evts[0]);
                }
            }
        }

        let (Some(anchor_ts), Some(anchor_evt)) = (anchor_timestamp, anchor_event_ts) else {
            continue; // no anchor yet — skip
        };

        // Gather event timestamps and BPMs from both fdn 9 (event_timestamp)
        // and fdn 10 (event_timestamp_12 / expanded components).
        let mut all_event_ts: Vec<f64> = Vec::new();
        let mut all_bpm: Vec<f64> = Vec::new();

        if let Some(evts) = field_as_f64_vec(hr_msg, "event_timestamp") {
            all_event_ts = evts;
        }
        if let Some(bpms) = field_as_f64_vec(hr_msg, "filtered_bpm") {
            all_bpm = bpms;
        }

        // Also check for component-expanded event_timestamp fields
        // (from event_timestamp_12's 12-bit components).
        // These appear as additional "event_timestamp" fields; collect
        // any fields named "event_timestamp" that weren't in fdn 9's array.
        // Actually, the pipeline creates synthetic fields with the component's
        // target_name — "event_timestamp". Since fdn 9 also has that name,
        // we might get duplicates. The safest approach: collect ALL fields
        // named "event_timestamp" from the message.
        let comp_event_ts: Vec<f64> = hr_msg
            .fields
            .iter()
            .filter(|f| {
                f.name == "event_timestamp"
                    && !matches!(f.kind, crate::FieldKind::Standard { field_def_num: 9 })
            })
            .filter_map(|f| f.value.as_f64())
            .collect();

        // If we found component-expanded timestamps, use those as primary
        // if fdn 9 had no data.
        if all_event_ts.is_empty() && !comp_event_ts.is_empty() {
            all_event_ts = comp_event_ts;
        }

        if all_event_ts.is_empty() || all_bpm.is_empty() {
            continue;
        }

        for (i, &event_ts) in all_event_ts.iter().enumerate() {
            let bpm = if i < all_bpm.len() {
                all_bpm[i] as u8
            } else {
                continue;
            };

            let mut adj_event_ts = event_ts;

            // Handle rollover: if event_ts < anchor, add 0x400000.
            if adj_event_ts < anchor_evt {
                if anchor_evt - adj_event_ts > EVENT_TS_ROLLOVER {
                    adj_event_ts += EVENT_TS_ROLLOVER;
                } else {
                    continue; // can't compute delta — skip
                }
            }

            let current_ts = anchor_ts + (adj_event_ts - anchor_evt);

            // Gap fill: carry previous HR forward in 250ms steps.
            if let Some(prev) = samples.last() {
                let prev_ts = prev.timestamp;
                let prev_hr = prev.heart_rate;
                let gap_ms = (current_ts - prev_ts).abs() * 1000.0;
                let mut step = 1usize;
                let mut remaining_gap = gap_ms;
                while remaining_gap > GAP_INCREMENT_MS && step <= GAP_MAX_STEPS {
                    samples.push(HrSample {
                        timestamp: prev_ts + GAP_INCREMENT_MS / 1000.0 * step as f64,
                        heart_rate: prev_hr,
                    });
                    remaining_gap -= GAP_INCREMENT_MS;
                    step += 1;
                }
            }

            samples.push(HrSample {
                timestamp: current_ts,
                heart_rate: bpm,
            });
        }
    }

    samples
}

/// Merge heart rates from `hr` messages into `record` messages.
///
/// HR samples are averaged over each record's time range
/// (`recordRangeStartTime` to `recordRangeEndTime` exclusive).
/// The merged HR is stored as a new `heart_rate` field on the record
/// message (overwriting any existing standard heart_rate field).
///
/// Requirements:
/// - `expand_components` must have been enabled during decode
/// - `apply_scale_and_offset` must have been enabled (for fractional timestamps)
pub fn merge_heart_rates(messages: &mut [Message]) {
    // Partition messages: collect hr messages (by reference) and record indices.
    let hr_messages: Vec<&Message> = messages
        .iter()
        .filter(|m| m.global_mesg_num == 132)
        .collect();

    if hr_messages.is_empty() {
        return;
    }

    let samples = expand_heart_rates(&hr_messages);
    if samples.is_empty() {
        return;
    }

    // Walk record messages and assign averaged HR.
    let mut sample_idx = 0usize;
    let mut range_start: Option<f64> = None;

    // Collect record message indices so we can mutably borrow distinct records.
    let record_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.global_mesg_num == 20)
        .map(|(i, _)| i)
        .collect();

    for &rec_idx in &record_indices {
        let range_end = {
            let rec = &messages[rec_idx];
            match field_datetime_secs(rec, "timestamp") {
                Some(secs) => secs,
                None => continue,
            }
        };

        if range_start.is_none() {
            range_start = Some(range_end);
        }
        let mut start = range_start.unwrap();

        // JS SDK quirk: when start == end, nudge start back by 1s.
        if start == range_end {
            start -= 1.0;
            if sample_idx > 0 {
                sample_idx = sample_idx.saturating_sub(1);
            }
        }

        let mut hr_sum: f64 = 0.0;
        let mut hr_count: u32 = 0;

        // Advance through samples within the range.
        while sample_idx < samples.len() {
            let ts = samples[sample_idx].timestamp;
            if ts > start && ts <= range_end {
                hr_sum += samples[sample_idx].heart_rate as f64;
                hr_count += 1;
                sample_idx += 1;
            } else if ts > range_end {
                break;
            } else {
                sample_idx += 1;
            }
        }

        if hr_count > 0 {
            let avg = (hr_sum / hr_count as f64).round() as u64;
            let rec = &mut messages[rec_idx];
            // Replace existing heart_rate or add a new one.
            if let Some(existing) = rec.fields.iter_mut().find(|f| f.name == "heart_rate") {
                existing.value = Value::UInt(avg);
            } else {
                rec.fields.push(crate::Field {
                    name: "heart_rate".to_string(),
                    kind: crate::FieldKind::Standard { field_def_num: 3 },
                    value: Value::UInt(avg),
                    units: Some("bpm".to_string()),
                });
            }
        }

        range_start = Some(range_end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hr_message(
        timestamp_secs: Option<u32>,
        fractional: f64,
        event_timestamps: Vec<f64>,
        filtered_bpm: Vec<f64>,
    ) -> Message {
        let mut fields = Vec::new();
        if let Some(ts) = timestamp_secs {
            let dt = datetime::fit_to_datetime(ts).unwrap();
            fields.push(crate::Field {
                name: "timestamp".to_string(),
                kind: crate::FieldKind::Standard { field_def_num: 253 },
                value: Value::DateTime(dt),
                units: None,
            });
            if fractional != 0.0 {
                fields.push(crate::Field {
                    name: "fractional_timestamp".to_string(),
                    kind: crate::FieldKind::Standard { field_def_num: 0 },
                    value: Value::Float(fractional),
                    units: Some("s".to_string()),
                });
            }
        }
        fields.push(crate::Field {
            name: "event_timestamp".to_string(),
            kind: crate::FieldKind::Standard { field_def_num: 9 },
            value: Value::Array(event_timestamps.into_iter().map(Value::Float).collect()),
            units: Some("s".to_string()),
        });
        fields.push(crate::Field {
            name: "filtered_bpm".to_string(),
            kind: crate::FieldKind::Standard { field_def_num: 6 },
            value: Value::Array(
                filtered_bpm
                    .into_iter()
                    .map(|v| Value::UInt(v as u64))
                    .collect(),
            ),
            units: Some("bpm".to_string()),
        });
        Message {
            global_mesg_num: 132,
            name: "hr",
            fields,
        }
    }

    fn make_record_message(timestamp_secs: u32) -> Message {
        let dt = datetime::fit_to_datetime(timestamp_secs).unwrap();
        Message {
            global_mesg_num: 20,
            name: "record",
            fields: vec![crate::Field {
                name: "timestamp".to_string(),
                kind: crate::FieldKind::Standard { field_def_num: 253 },
                value: Value::DateTime(dt),
                units: None,
            }],
        }
    }

    #[test]
    fn basic_hr_merge() {
        // Anchor message: timestamp=995749800, event_timestamp=995749800 (sets anchor).
        // Delta message: no timestamp, event_timestamp=995749800.2 (delta=0.2).
        // Sample at anchor_ts + delta = 995749800 + 0.2 = 995749800.2.
        // Record at 995749801. Sample at 800.2 falls in (800..801].
        // Gap is 200ms (< 250ms increment), so no gap-fill samples are added.
        let hr1 = make_hr_message(Some(995749800), 0.0, vec![995749800.0], vec![120.0]);
        let hr2 = make_hr_message(None, 0.0, vec![995749800.2], vec![140.0]);
        let mut messages = vec![hr1, hr2, make_record_message(995749801)];
        merge_heart_rates(&mut messages);
        let rec = &messages[2];
        let hr_field = rec.field("heart_rate").unwrap();
        assert_eq!(hr_field.value, Value::UInt(140));
    }

    #[test]
    fn hr_merge_averages_across_range() {
        // Anchor: timestamp=995749800, event=995749800 (anchor ref).
        // Delta: events at 995749800.3 and 995749800.7.
        // First record at 995749800 (creates baseline), second at 995749801.
        // Second record range: (800..801].
        // Samples in range: 800.3 (130) and 800.7 (150). No gap-fill
        // because the gap from anchor (800.0) to first delta (800.3)
        // is 300ms → 1 gap-fill sample at 800.25 (120).
        // All three in range: (120 + 130 + 150) / 3 = 133.3 → round = 133.
        let mut messages = vec![
            make_hr_message(Some(995749800), 0.0, vec![995749800.0], vec![120.0]),
            make_hr_message(
                None,
                0.0,
                vec![995749800.3, 995749800.7],
                vec![130.0, 150.0],
            ),
            make_record_message(995749800), // baseline record
            make_record_message(995749801),
        ];
        merge_heart_rates(&mut messages);
        let hr_field = messages[3].field("heart_rate").unwrap();
        // Gap-fill sample (120 at 800.25) + delta samples (130 at 800.3, 150 at 800.7)
        // all fall in range. Average = 133.
        assert_eq!(hr_field.value, Value::UInt(133));
    }

    #[test]
    fn hr_merge_no_hr_messages_is_noop() {
        let mut messages = vec![make_record_message(995749800)];
        merge_heart_rates(&mut messages);
        assert!(messages[0].field("heart_rate").is_none());
    }

    #[test]
    fn expand_heart_rates_gap_fill() {
        // First HR: anchor message with timestamp and single event_timestamp.
        // Second HR: delta message (no timestamp) with event at 800.6 (600ms gap).
        // Should fill 2 intermediate samples at 800.25 and 800.5.
        let hr1 = make_hr_message(Some(995749800), 0.0, vec![995749800.0], vec![120.0]);
        let hr2 = make_hr_message(None, 0.0, vec![995749800.6], vec![130.0]);
        let hr_msgs = vec![&hr1, &hr2];
        let samples = expand_heart_rates(&hr_msgs);
        // Expected: 800.0(120), 800.25(120), 800.5(120), 800.6(130)
        assert_eq!(samples.len(), 4);
        assert!((samples[0].timestamp - 995749800.0).abs() < 0.01);
        assert_eq!(samples[0].heart_rate, 120);
        assert!((samples[1].timestamp - 995749800.25).abs() < 0.01);
        assert_eq!(samples[1].heart_rate, 120);
        assert!((samples[2].timestamp - 995749800.5).abs() < 0.01);
        assert_eq!(samples[2].heart_rate, 120);
        assert!((samples[3].timestamp - 995749800.6).abs() < 0.01);
        assert_eq!(samples[3].heart_rate, 130);
    }
}
