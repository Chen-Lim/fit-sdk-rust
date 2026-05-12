//! Transform pipeline that converts a `RawMessage` into a typed `Message`.
//!
//! The pieces here are deliberately decoupled and individually testable so
//! the M5 typed decoder ([`crate::TypedDecoder`]) can compose them in the
//! protocol-mandated order:
//!
//! ```text
//!   1. SubField selection           (subfields)
//!   2. Components unpacking         (components + bit_stream)
//!   3. Accumulator running totals   (accumulator)
//!   4. Scale / Offset application   (scale_offset)
//!   5. Enum int → string            (enum_strings)
//!   6. DateTime conversion          (`crate::datetime`)
//! ```
//!
//! Reference: `guide/fit_binary_learning_notes.md` §"解码处理顺序（关键！）".

pub mod accumulator;
pub mod bit_stream;
pub mod components;
pub mod enum_strings;
#[cfg(feature = "chrono")]
pub mod hr_merge;
pub mod memo_glob;
pub mod scale_offset;
pub mod subfields;

pub use accumulator::Accumulator;
pub use bit_stream::BitStream;
#[cfg(feature = "chrono")]
pub use hr_merge::merge_heart_rates;
pub use memo_glob::decode_memo_glob;
