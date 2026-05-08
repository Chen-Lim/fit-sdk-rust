//! Print a one-line summary of every `record` message in a `.fit` file.
//!
//! Run with:
//!
//! ```text
//! cargo run --example summarise_activity --release -- path/to/Activity.fit
//! ```

use fit::{Decoder, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: summarise_activity <Activity.fit>");
    let bytes = std::fs::read(&path)?;
    let (messages, errors) = Decoder::builder(&bytes).build().read_all();
    if !errors.is_empty() {
        eprintln!("decode errors: {errors:?}");
    }

    let records: Vec<_> = messages.iter().filter(|m| m.name == "record").collect();
    println!("{} record messages", records.len());

    for (i, m) in records.iter().take(5).enumerate() {
        let timestamp = m
            .field("timestamp")
            .and_then(|f| match &f.value {
                Value::DateTime(dt) => Some(dt.to_rfc3339()),
                _ => None,
            })
            .unwrap_or_else(|| "?".into());
        let speed = m
            .field("speed")
            .and_then(|f| match &f.value {
                Value::Float(v) => Some(format!("{v:.2} m/s")),
                _ => None,
            })
            .unwrap_or_else(|| "?".into());
        let hr = m
            .field("heart_rate")
            .and_then(|f| match &f.value {
                Value::UInt(v) => Some(format!("{v} bpm")),
                _ => None,
            })
            .unwrap_or_else(|| "?".into());
        println!("[{i}] {timestamp}  speed={speed}  hr={hr}");
    }
    Ok(())
}
