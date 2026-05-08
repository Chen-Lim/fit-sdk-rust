# fit-sdk-rust

Pure-Rust decoder and encoder for Garmin FIT (Flexible and Interoperable Data Transfer) protocol files — activities, workouts, courses, settings, and more.

Supports FIT protocol v2.0 (profile v21.200). No `unsafe` code, no C dependencies.

## Features

- **Streaming decoder** — iterator-based, yields one raw message per record
- **Typed decoder** — profile-aware pipeline with configurable transforms:
  - DateTime conversion (`chrono::DateTime<Utc>`)
  - Enum int-to-string resolution (e.g. `"running"` for `Sport::Running`)
  - Scale/offset → physical values
  - SubField selection and Component unpacking
  - Developer field resolution
  - Heart-rate merge and MemoGlob reassembly (post-processing)
- **Encoder** — round-trips typed messages back to protocol-compliant FIT binary
- **CRC integrity verification** — header and file-level
- **Multi-FIT chain** — transparently handles concatenated FIT files
- **Compressed timestamps** — full protocol §2.3 support

## Quick start

Add to your `Cargo.toml`:

```toml
[dependencies]
fit-sdk-rust = "0.1"
```

### Decode an activity file

```rust
use fit::Decoder;

let bytes = std::fs::read("Activity.fit")?;

// Decode with all transforms enabled (default).
let (messages, errors) = Decoder::builder(&bytes).build().read_all();
assert!(errors.is_empty());

for msg in &messages {
    println!("{} (mesg #{})", msg.name, msg.global_mesg_num);
    for field in &msg.fields {
        println!("  {}: {:?}", field.name, field.value);
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Access specific fields

```rust
use fit::{Decoder, Value};

let bytes = std::fs::read("Activity.fit")?;
let (messages, _) = Decoder::builder(&bytes).build().read_all();

// Find record messages and read heart rate / speed.
for msg in &messages {
    if msg.name == "record" {
        if let Some(hr) = msg.field("heart_rate") {
            if let Some(bpm) = hr.value.as_f64() {
                println!("Heart rate: {bpm} bpm");
            }
        }
        if let Some(speed) = msg.field("enhanced_speed") {
            if let Some(mps) = speed.value.as_f64() {
                println!("Speed: {mps:.2} m/s");
            }
        }
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Encode messages back to FIT

```rust
use fit::{Decoder, Encoder};

let bytes = std::fs::read("Activity.fit")?;
let (messages, _) = Decoder::builder(&bytes).build().read_all();

let encoded: Vec<u8> = Encoder::new().encode(&messages)?;
fit::check_integrity(&encoded)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Raw (low-level) decoding

For streaming or memory-constrained scenarios, use the raw decoder directly:

```rust
use fit::Decoder;

let bytes = std::fs::read("Activity.fit")?;
let (raw_messages, errors) = Decoder::new(&bytes).read_all();

for msg in &raw_messages {
    println!("mesg_num={}, fields={}", msg.global_mesg_num, msg.fields.len());
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Configure transform options

Disable individual transforms for cheaper or less opinionated output:

```rust
use fit::Decoder;

let bytes = std::fs::read("Activity.fit")?;

let (messages, _) = Decoder::builder(&bytes)
    .convert_datetime(false)       // keep raw u32 timestamps
    .convert_types_to_strings(false) // keep raw enum integers
    .apply_scale_and_offset(false)   // keep raw integer values
    .expand_components(false)        // skip component unpacking
    .build()
    .read_all();
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Verify file integrity

```rust
use fit::{is_fit, check_integrity};

let bytes = std::fs::read("Activity.fit")?;

if is_fit(&bytes) {
    check_integrity(&bytes)?;
    println!("Valid FIT file, CRCs OK");
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

## API overview

| Type | Purpose |
|---|---|
| `Decoder` | Streaming raw-message iterator |
| `DecoderBuilder` / `TypedDecoder` | Profile-aware typed pipeline |
| `Encoder` / `EncoderBuilder` | Encode messages back to FIT binary |
| `Message` | Fully-transformed message with named fields |
| `Value` | Typed field value (float, string, enum, datetime, ...) |
| `Field` | Named field with value, kind, and units |
| `FitError` | Unified error type |
| `is_fit` / `check_integrity` | File-level validation |

## MSRV

Rust 1.75 or later.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.
