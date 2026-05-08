//! CRC-16 calculation for FIT files.
//!
//! FIT uses a Garmin-specific CRC-16 (it is **not** standard CRC-16/CCITT).
//! Each byte is folded into the accumulator in two halves: low nibble first,
//! then high nibble, using a 16-entry lookup table.
//!
//! Reference: `guide/fit_binary_learning_notes.md` §1.3.

/// 16-entry CRC lookup table. Each entry is the CRC contribution of a 4-bit
/// value when it is the only data folded into a zero accumulator.
const CRC_TABLE: [u16; 16] = [
    0x0000, 0xCC01, 0xD801, 0x1400, 0xF001, 0x3C00, 0x2800, 0xE401, 0xA001, 0x6C00, 0x7800, 0xB401,
    0x5000, 0x9C01, 0x8801, 0x4400,
];

/// Fold one byte into a running CRC.
///
/// The byte is processed in two passes: the low nibble first, then the high
/// nibble. After each pass the accumulator is shifted right 4 bits and XORed
/// with `CRC_TABLE[nibble]`.
#[inline]
pub fn update(mut crc: u16, byte: u8) -> u16 {
    // Pass 1: low nibble of the byte.
    let mut tmp = CRC_TABLE[(crc & 0xF) as usize];
    crc = (crc >> 4) & 0x0FFF;
    crc = crc ^ tmp ^ CRC_TABLE[(byte & 0xF) as usize];

    // Pass 2: high nibble of the byte.
    tmp = CRC_TABLE[(crc & 0xF) as usize];
    crc = (crc >> 4) & 0x0FFF;
    crc = crc ^ tmp ^ CRC_TABLE[((byte >> 4) & 0xF) as usize];

    crc
}

/// Compute the CRC over a full byte slice.
///
/// Equivalent to `data.iter().fold(0, |c, &b| update(c, b))`. The starting
/// value is always `0` per the FIT protocol.
#[inline]
pub fn calculate(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |crc, &byte| update(crc, byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_zero() {
        assert_eq!(calculate(&[]), 0);
    }

    #[test]
    fn single_zero_byte_stays_zero() {
        // update(0, 0): both nibbles are 0, both lookups are 0 → result stays 0.
        assert_eq!(update(0, 0), 0);
        assert_eq!(calculate(&[0]), 0);
    }

    #[test]
    fn calculate_matches_iterative_update() {
        // Calculate must equal repeatedly applying update from 0.
        let data: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03];
        let one_shot = calculate(data);
        let stepwise = data.iter().fold(0u16, |c, &b| update(c, b));
        assert_eq!(one_shot, stepwise);
    }

    #[test]
    fn split_and_combine_is_associative() {
        // Folding bytes one-at-a-time vs. in chunks must produce the same result.
        let data: Vec<u8> = (0u8..=255).collect();
        let full = calculate(&data);
        let mid = data.len() / 2;
        let prefix_crc = calculate(&data[..mid]);
        let combined = data[mid..].iter().fold(prefix_crc, |c, &b| update(c, b));
        assert_eq!(full, combined);
    }

    #[test]
    fn fixed_known_value_after_one_byte() {
        // Byte 0x01 starting from CRC 0:
        //   pass 1 (nibble 0x1): tmp=CRC_TABLE[0]=0; crc=(0>>4)&0xFFF=0; crc = 0 ^ 0 ^ CRC_TABLE[1] = 0xCC01
        //   pass 2 (nibble 0x0): tmp=CRC_TABLE[0xCC01 & 0xF]=CRC_TABLE[1]=0xCC01;
        //                        crc = (0xCC01 >> 4) & 0xFFF = 0x0CC0;
        //                        crc = 0x0CC0 ^ 0xCC01 ^ CRC_TABLE[0] = 0xC0C1
        assert_eq!(update(0, 0x01), 0xC0C1);
    }
}
