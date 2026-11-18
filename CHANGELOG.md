# Changelog

## Unreleased

### Changed

- **MSRV bumped to Rust 1.77**: updated `rust-version` from `1.75` to `1.77`
  to match current `thiserror 2.0` upstream requirements.

### Fixed

- **Profile metadata parity**: fixed `battery_level`/`cycles` components and
  units in `src/profile/generated/messages.rs`, and marked `total_cycles` as
  accumulated.
- **Decoder conformance & robustness**: synthesized `timestamp` (`fdn 253`) on
  compressed-timestamp records even when missing from the definition, rejected
  records overrunning `data_size`, verified trailing 2-byte file CRC during
  `DecodeMode::Normal` decoding, added `DecodeMode::{SkipHeader, DataOnly}` +
  option conflict validation (`FitError::InvalidOptions`), avoided single-element
  `Vec` allocations on scalar values, and handled zero-length/enum edge cases in
  `decode_memo_glob`.
- **Typed decoder & HR merge parity**: aligned SubField/component priority,
  parent scale suppression for multi-component containers, accumulator seeding
  and rollover handling (`0x400000`), uint8z invalid checks, and big-endian /
  signed-offset developer-field decoding with Garmin's JavaScript SDK.
- **Encoder hardening**: resolved SubFields during wire encoding, suppressed
  multi-component parent scale/offset, set the protocol endian-ability bit
  (`0x80`) on multi-byte `BaseType` definitions, replaced `expect` in segment
  emission with `FitError::SegmentTooLarge`, fixed `u8` wire-size overflow on
  >31 `u64` / >63 `u32` arrays, clamped out-of-range integers to protocol invalid
  sentinels in `encode_int`, and rejected embedded NUL bytes in strings with
  `FitError::EmbeddedNullInString`.

## 0.2.1

First crates.io release. Carries one small breaking change from `0.2.0`
that was merged before publishing.

### Breaking

- **`Value::Enum` now carries `Cow<'static, str>`** so that names returned
  from the static Profile dispatcher stay zero-allocation while
  developer-defined enum values can still own a runtime `String`.
  Consumers construct with `Value::Enum("activity".into())` (works for
  both `&'static str` and `String`); read via the `Deref<Target=str>`
  impl (`s.is_empty()`, `&s[..]`, `&*s`). Pattern-match against the
  variant by value (`Value::Enum(Cow::Borrowed("activity"))` or just
  `matches!(&v, Value::Enum(s) if s == "activity")`).

### Fixed

- Integration tests that broke alongside the `Value::Enum` change now build
  and pass under `cargo test --all-targets`.
- `Cargo.toml`: `license` switched from `GPL-3.0` to `Apache-2.0`,
  `repository` corrected to `github.com/Chen-Lim/fit-sdk-rust`.
- `README.md`: install snippet updated to `fit-sdk-rust = "0.2"`,
  added a note that the crate exposes the library as `fit`.

### Known issues

These were documented in the in-repo code review (`code-review-report.html`)
at `0.2.1` release time (all four have since been resolved in `Unreleased`):

- Encoder array wire-size overflow on > 31 element `u64` / > 63 element
  `u32` arrays (fixed in `Unreleased`).
- `Encoder::encode_int` silently wraps integer values exceeding the wire
  type's range (fixed in `Unreleased`).
- Strings with embedded NUL bytes silently truncate on round-trip (fixed
  in `Unreleased`).
- `decode_memo_glob` can panic on `Value::Array([Value::Bytes(empty)])`
  (fixed in `Unreleased`).

## 0.2.0

Performance + API tightening pass. **Breaking changes throughout the public
surface**; bump major early while there are no known production consumers.

### Breaking

- **`RawValue` split into Scalar / Array variants.** Each numeric base type
  now has both a stack-only `*Scalar(T)` variant (~95% of fields) and a
  heap-boxed `*Array(Box<[T]>)` variant. `String` is `Box<str>`, `Byte` is
  `Box<[u8]>`. Pattern matches that previously used
  `RawValue::UInt32(vec![x])` must change to `RawValue::U32Scalar(x)`.
  Helper accessors (`as_u8`, `as_u16`, `as_u32`, `as_str`,
  `scalar_u64`, `scalar_f64`, `to_f64s`) handle both shapes.
- **`RawMessage` and `RawDevField` are now generic over a lifetime `'a`.**
  Developer-field bytes are borrowed from the input slice as
  `Cow<'a, [u8]>`, eliminating a per-message heap allocation. Use
  `RawMessage::into_owned()` / `RawDevField::into_owned()` to detach.
- **`MessageDefinition::fields` / `dev_fields` are now `SmallVec`** with
  inline capacity 48 / 8. Cloning a definition no longer allocates in the
  common case.
- **`FitError::FieldTooLarge` is now a struct variant** carrying
  `{ kind: FieldTooLargeKind, size: usize }` instead of an owned `String`.
  Removes the only heap-pointed payload from `FitError`.
- **`chrono` is now an optional default feature.** Build with
  `--no-default-features` to drop the dependency. Without `chrono`,
  `Value::DateTime` carries the raw FIT epoch seconds (`u32`) instead of
  `chrono::DateTime<Utc>`, and `merge_heart_rates` is unavailable.

### Performance

- `Decoder::decode_data` no longer clones two `Vec`s per message. The whole
  `MessageDefinition` is `memcpy`'d once on the stack (~256B inline with
  `SmallVec`). For a typical 16-slot table this fits in ~4KB — well within
  L1d.
- Developer-field bytes are zero-copy borrows from the input buffer
  (`Cow::Borrowed`) — no per-message `to_vec()`.
- Scalar fields (the vast majority) decode with no heap allocation.

### Added

- `smallvec = "1.13"` dependency (with `const_generics` feature).
- `RawValue::scalar_u64` / `scalar_f64` / `to_f64s` accessors that work
  uniformly across Scalar and Array variants.
- `RawMessage::into_owned` / `RawDevField::into_owned`.
- `error::FieldTooLargeKind` enum.
