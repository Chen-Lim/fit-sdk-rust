# Changelog

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
