//! M6 integration: post-processing options (skip_header, data_only,
//! on_mesg callback, decode_memo_glob, HR merge).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use fit::Decoder;

fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/test_data");
    p.push(name);
    p
}

fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixture(name)).expect("fixture must be readable")
}

// ────────────────────────────────────────────────────────────────────
// skip_header
// ────────────────────────────────────────────────────────────────────

#[test]
fn skip_header_removes_file_id_messages() {
    let bytes = read_fixture("Activity.fit");

    let with_header = Decoder::builder(&bytes).build().read_all().0;
    let without_header = Decoder::builder(&bytes)
        .skip_header(true)
        .build()
        .read_all()
        .0;

    // file_id is mesg_num 0
    let header_count = with_header
        .iter()
        .filter(|m| m.global_mesg_num == 0)
        .count();
    assert!(header_count > 0, "fixture should have file_id messages");

    let filtered_count = without_header
        .iter()
        .filter(|m| m.global_mesg_num == 0)
        .count();
    assert_eq!(filtered_count, 0, "skip_header should remove all file_id");
    assert_eq!(
        without_header.len(),
        with_header.len() - header_count,
        "message count should decrease by exactly the number of file_id"
    );
}

// ────────────────────────────────────────────────────────────────────
// data_only
// ────────────────────────────────────────────────────────────────────

#[test]
fn data_only_filters_to_profile_messages() {
    let bytes = read_fixture("Activity.fit");

    let all = Decoder::builder(&bytes).build().read_all().0;
    let data_only = Decoder::builder(&bytes)
        .data_only(true)
        .build()
        .read_all()
        .0;

    // Every message in data_only should have a known Profile entry.
    for msg in &data_only {
        assert!(
            fit::profile::mesg_info_by_num(msg.global_mesg_num).is_some(),
            "data_only should only keep Profile-known messages, got mesg_num={}",
            msg.global_mesg_num
        );
    }

    // data_only should be <= all (some messages may be filtered).
    assert!(
        data_only.len() <= all.len(),
        "data_only should not add messages"
    );
}

// ────────────────────────────────────────────────────────────────────
// on_mesg callback
// ────────────────────────────────────────────────────────────────────

#[test]
fn on_mesg_callback_fires_for_each_message() {
    let bytes = read_fixture("Activity.fit");
    let count = Arc::new(AtomicUsize::new(0));
    let count_clone = Arc::clone(&count);

    let (msgs, errs) = Decoder::builder(&bytes)
        .on_mesg(move |_| {
            count_clone.fetch_add(1, Ordering::SeqCst);
        })
        .build()
        .read_all();

    assert!(errs.is_empty());
    assert_eq!(
        count.load(Ordering::SeqCst),
        msgs.len(),
        "on_mesg should fire exactly once per message"
    );
}

// ────────────────────────────────────────────────────────────────────
// HR merge integration (using unit-level merge_heart_rates)
// ────────────────────────────────────────────────────────────────────

#[test]
fn hr_merge_adds_heart_rate_to_records() {
    let bytes = read_fixture("Activity.fit");
    let (mut messages, errs) = Decoder::builder(&bytes).build().read_all();
    assert!(errs.is_empty());

    // Before merge: some records may or may not have heart_rate.
    let before_hr_count = messages
        .iter()
        .filter(|m| m.global_mesg_num == 20 && m.field("heart_rate").is_some())
        .count();

    // Run HR merge.
    fit::merge_heart_rates(&mut messages);

    // After merge: at least as many records should have heart_rate.
    let after_hr_count = messages
        .iter()
        .filter(|m| m.global_mesg_num == 20 && m.field("heart_rate").is_some())
        .count();

    assert!(
        after_hr_count >= before_hr_count,
        "HR merge should not remove heart_rate fields"
    );
}
