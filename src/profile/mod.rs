//! FIT profile metadata: enum types, message numbers, and per-field schema.
//!
//! Hand-written runtime types live here directly; the bulk of the data
//! (~28k lines of message and type definitions) is generated from
//! `Profile.xlsx` and lives in [`generated`].
//!
//! Regenerate after editing Profile.xlsx with:
//!
//! ```bash
//! cargo run -p fit-codegen
//! ```

mod field_info;

pub use field_info::{Component, FieldInfo, MesgInfo, SubField};

pub mod generated;

// Re-export the most commonly used pieces at module root.
pub use generated::mesg_num::MesgNum;

/// Look up the static [`MesgInfo`] for a global message number.
///
/// O(1): a single `match global_mesg_num { ... }` generated from the
/// `mesg_num` Types-sheet enum. Encoder hot path.
pub use generated::messages::mesg_info_by_num;
