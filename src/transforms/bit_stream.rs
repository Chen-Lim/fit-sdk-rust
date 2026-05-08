//! LSB-first bit reader for the Components transform.
//!
//! When a FIT field's `components` list is non-empty, the field's wire bytes
//! are interpreted as a packed integer that is unpacked **least-significant
//! bit first** into multiple synthesised sub-values. For example,
//! `gear_change_data` is a 32-bit field unpacked into four 8-bit lanes:
//! `rear_gear`, `rear_gear_num`, `front_gear`, `front_gear_num`.
//!
//! Reference: `guide/fit_binary_learning_notes.md` §"BitStream（Components 位域展开）".

/// LSB-first bit reader over a borrowed byte slice.
pub struct BitStream<'a> {
    bytes: &'a [u8],
    /// Index of the next byte to refill from.
    byte_offset: usize,
    /// Up to 64 buffered bits; the *low* `bits_in_buffer` bits are valid.
    buffer: u64,
    bits_in_buffer: u32,
}

impl<'a> BitStream<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            byte_offset: 0,
            buffer: 0,
            bits_in_buffer: 0,
        }
    }

    /// Read up to 64 bits LSB-first. If the stream is exhausted, the
    /// remaining buffered bits are returned right-aligned and any further
    /// reads yield 0.
    ///
    /// Each refill takes the next byte from the slice and shifts it into
    /// the *high* end of the unread portion of the buffer (so the byte's
    /// LSB lands at position `bits_in_buffer` and the LSB of the *whole*
    /// stream is the bit that comes out first).
    pub fn read_bits(&mut self, n: u32) -> u64 {
        debug_assert!(n <= 64, "cannot read more than 64 bits at once");
        if n == 0 {
            return 0;
        }

        // Fill the buffer until we have enough bits, or the source is dry.
        while self.bits_in_buffer < n && self.byte_offset < self.bytes.len() {
            self.buffer |= (self.bytes[self.byte_offset] as u64) << self.bits_in_buffer;
            self.bits_in_buffer += 8;
            self.byte_offset += 1;
        }

        let take = n.min(self.bits_in_buffer);
        let mask = if take == 64 { u64::MAX } else { (1u64 << take) - 1 };
        let value = self.buffer & mask;

        // Discard the bits we just consumed.
        if take == 64 {
            self.buffer = 0;
        } else {
            self.buffer >>= take;
        }
        self.bits_in_buffer -= take;
        value
    }

    /// Number of bits still available without consuming them.
    pub fn bits_remaining(&self) -> u64 {
        self.bits_in_buffer as u64 + (self.bytes.len() - self.byte_offset) as u64 * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_full_byte_lsb_first() {
        // Single byte 0b10100101: LSB-first reads 1 then 0 then 1 ...
        let mut bs = BitStream::new(&[0b1010_0101]);
        let mut bits = Vec::new();
        for _ in 0..8 {
            bits.push(bs.read_bits(1) as u8);
        }
        assert_eq!(bits, vec![1, 0, 1, 0, 0, 1, 0, 1]);
    }

    #[test]
    fn unpacks_gear_change_data() {
        // 32-bit value packed as: [rearGear:8 | rearGearNum:8 | frontGear:8 | frontGearNum:8]
        // Bytes (LE): rear=4, rear_num=3, front=2, front_num=1 → [0x04, 0x03, 0x02, 0x01]
        let mut bs = BitStream::new(&[0x04, 0x03, 0x02, 0x01]);
        assert_eq!(bs.read_bits(8), 0x04); // rear_gear
        assert_eq!(bs.read_bits(8), 0x03); // rear_gear_num
        assert_eq!(bs.read_bits(8), 0x02); // front_gear
        assert_eq!(bs.read_bits(8), 0x01); // front_gear_num
        assert_eq!(bs.bits_remaining(), 0);
    }

    #[test]
    fn handles_non_byte_aligned_widths() {
        // Two 12-bit values packed in 3 bytes (LSB-first):
        //   value0 = 0xABC (12 bits)
        //   value1 = 0x123 (12 bits)
        // LSB-first packing: low 8 bits of v0 → byte 0 = 0xBC,
        //                    high 4 bits of v0 + low 4 bits of v1 → byte 1 = 0x3A
        //                    high 8 bits of v1 → byte 2 = 0x12
        let mut bs = BitStream::new(&[0xBC, 0x3A, 0x12]);
        assert_eq!(bs.read_bits(12), 0xABC);
        assert_eq!(bs.read_bits(12), 0x123);
    }

    #[test]
    fn read_zero_bits_is_noop() {
        let mut bs = BitStream::new(&[0xFF]);
        assert_eq!(bs.read_bits(0), 0);
        assert_eq!(bs.read_bits(8), 0xFF);
    }

    #[test]
    fn exhausted_stream_returns_remaining_then_zeros() {
        let mut bs = BitStream::new(&[0x05]); // 8 bits available
        assert_eq!(bs.read_bits(4), 0x5); // low nibble
        assert_eq!(bs.read_bits(8), 0); // only 4 bits left → returns those 4 (= 0)
        assert_eq!(bs.read_bits(8), 0); // truly empty now
    }
}
