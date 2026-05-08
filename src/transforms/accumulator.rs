//! Accumulator — wraparound compensation for FIT counter fields.
//!
//! Some FIT fields are physical counters whose wire-level encoding has fewer
//! bits than the cumulative range they represent (e.g. an 8-bit step counter
//! that wraps every 256 steps but is meant to express totals into the
//! thousands). The protocol's solution: each new value is interpreted as a
//! **delta** against the previous value, masked to the field's bit width,
//! and added to a per-field running total.
//!
//! Formula (per `(global_mesg_num, field_def_num)`):
//!
//! ```text
//!   delta        = (new_value - last_value) & ((1 << bits) - 1)
//!   accumulated += delta
//!   last_value   = new_value
//! ```
//!
//! Reference: `guide/fit_binary_learning_notes.md` §"补充知识：Accumulator".

use std::collections::HashMap;

#[derive(Debug, Default, Clone, Copy)]
struct AccumulatedField {
    last_value: u64,
    accumulated: u64,
    /// Whether `last_value` has been initialised. The first observation
    /// becomes the zero-point of the accumulator.
    initialised: bool,
}

/// Per-`(mesg_num, field_def_num)` running totals.
///
/// Cleared at chained-FIT boundaries (the same time `LocalDefinitions` is
/// cleared) — accumulator state must not leak between chained files.
#[derive(Debug, Default)]
pub struct Accumulator {
    state: HashMap<(u16, u8), AccumulatedField>,
}

impl Accumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a new value for `(mesg_num, field_def_num)` whose wire width is
    /// `bits` (clamped to 1..=64). Returns the cumulative total to record
    /// in the message's transformed value.
    pub fn accumulate(
        &mut self,
        mesg_num: u16,
        field_def_num: u8,
        new_value: u64,
        bits: u32,
    ) -> u64 {
        let key = (mesg_num, field_def_num);
        let state = self.state.entry(key).or_default();

        if !state.initialised {
            state.last_value = new_value;
            state.accumulated = new_value;
            state.initialised = true;
            return new_value;
        }

        let bits = bits.clamp(1, 64);
        let mask = if bits >= 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        let delta = new_value.wrapping_sub(state.last_value) & mask;
        state.accumulated = state.accumulated.wrapping_add(delta);
        state.last_value = new_value;
        state.accumulated
    }

    /// Drop all running totals. Call at chained-FIT boundaries.
    pub fn clear(&mut self) {
        self.state.clear();
    }

    /// Number of distinct `(mesg, field)` pairs tracked. Cheap inspection
    /// hook for tests.
    pub fn tracked_fields(&self) -> usize {
        self.state.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_observation_initialises_total() {
        let mut acc = Accumulator::new();
        assert_eq!(acc.accumulate(20, 5, 100, 8), 100);
        assert_eq!(acc.tracked_fields(), 1);
    }

    #[test]
    fn monotonic_8bit_increments_pass_through() {
        let mut acc = Accumulator::new();
        acc.accumulate(20, 5, 10, 8);
        assert_eq!(acc.accumulate(20, 5, 30, 8), 30);
        assert_eq!(acc.accumulate(20, 5, 50, 8), 50);
    }

    #[test]
    fn wraparound_compensation_8bit() {
        // Counter goes 250 → 5 (wrapping past 255 → 0). Delta should be 11.
        let mut acc = Accumulator::new();
        let _ = acc.accumulate(20, 5, 250, 8);
        // Next observation: 5. (5 - 250) & 0xFF = 11.
        assert_eq!(acc.accumulate(20, 5, 5, 8), 250 + 11);
    }

    #[test]
    fn distinct_fields_are_independent() {
        let mut acc = Accumulator::new();
        acc.accumulate(20, 1, 100, 8);
        acc.accumulate(20, 2, 7, 8);
        assert_eq!(acc.tracked_fields(), 2);
        // (mesg_num, field_def_num) keys disambiguate.
        assert_eq!(acc.accumulate(20, 1, 110, 8), 110);
        assert_eq!(acc.accumulate(20, 2, 7, 8), 7);
    }

    #[test]
    fn clear_drops_all_state() {
        let mut acc = Accumulator::new();
        acc.accumulate(20, 1, 100, 8);
        assert_eq!(acc.tracked_fields(), 1);
        acc.clear();
        assert_eq!(acc.tracked_fields(), 0);
        // After clear, the next observation re-initialises.
        assert_eq!(acc.accumulate(20, 1, 200, 8), 200);
    }

    #[test]
    fn wide_field_no_wrap_behaviour() {
        // 32-bit field, no wrap: delta is just (new - last).
        let mut acc = Accumulator::new();
        let _ = acc.accumulate(20, 5, 100, 32);
        assert_eq!(acc.accumulate(20, 5, 1_000_000, 32), 1_000_000);
    }
}
