//! Build script: warn if `Profile.xlsx` is newer than the generated source.
//!
//! This script does **not** regenerate code. The generated files under
//! `src/profile/generated/` are checked in.

use std::path::PathBuf;
use std::time::SystemTime;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // Profile.xlsx lives in a sibling repo; check one level up.
    let xlsx = manifest.join("../guide/garmin_fit_guide/Profile.xlsx");
    let generated = manifest.join("src/profile/generated/mod.rs");

    println!("cargo:rerun-if-changed={}", xlsx.display());
    println!("cargo:rerun-if-changed={}", generated.display());

    let xlsx_mtime = match xlsx.metadata().and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return, // xlsx not present (e.g. published crate); skip silently.
    };
    let gen_mtime = generated
        .metadata()
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);

    if xlsx_mtime > gen_mtime {
        println!(
            "cargo:warning=Profile.xlsx is newer than generated source. \
             Run `cargo run -p fit-codegen` to regenerate."
        );
    }
}
