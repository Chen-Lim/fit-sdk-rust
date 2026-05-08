//! M3 integration: walk a real .fit file, parse every Definition message,
//! and verify the 16-slot table behaves correctly when Data messages refer
//! back to it. Data payloads are skipped (their bytes are consumed but not
//! decoded — that lands in M4).

use std::path::PathBuf;

use fit::profile::MesgNum;
use fit::{ByteStream, FileHeader, LocalDefinitions, MessageDefinition, RecordHeader};

fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/test_data");
    p.push(name);
    p
}

fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixture(name)).expect("fixture must be readable")
}

/// Walks records in a buffered .fit file. Returns:
///   (definitions seen, data records seen, distinct global_mesg_nums in defs)
fn walk(bytes: &[u8]) -> (usize, usize, Vec<u16>) {
    let header = FileHeader::parse(bytes).unwrap();
    let data_end = header.file_crc_offset();

    let mut stream = ByteStream::new(bytes);
    stream.seek(header.header_size as usize).unwrap();

    let mut defs = LocalDefinitions::new();
    let mut def_count = 0usize;
    let mut data_count = 0usize;
    let mut mesg_nums: Vec<u16> = Vec::new();

    while stream.position() < data_end {
        let header_byte = stream.read_u8().unwrap();
        match RecordHeader::classify(header_byte) {
            RecordHeader::Definition {
                local_mesg_num,
                has_dev_data,
            } => {
                let def = MessageDefinition::parse(&mut stream, has_dev_data)
                    .expect("definition must parse cleanly");
                if !mesg_nums.contains(&def.global_mesg_num) {
                    mesg_nums.push(def.global_mesg_num);
                }
                defs.set(local_mesg_num, def);
                def_count += 1;
            }
            RecordHeader::Data { local_mesg_num } => {
                let def = defs.require(local_mesg_num).expect(
                    "every Data record must reference an already-defined local mesg num",
                );
                let size = def.data_size();
                let _payload = stream.read_bytes(size).unwrap();
                data_count += 1;
            }
            RecordHeader::CompressedTimestamp { .. } => {
                panic!("Activity.fit is not expected to use compressed timestamps");
            }
        }
    }

    assert_eq!(
        stream.position(),
        data_end,
        "parser should consume exactly data_size bytes; left {} unread",
        data_end - stream.position()
    );
    (def_count, data_count, mesg_nums)
}

// ────────────────────────────────────────────────────────────────────
// Activity.fit — 94 KB main fixture
// ────────────────────────────────────────────────────────────────────

#[test]
fn activity_definition_count_matches_csv() {
    let bytes = read_fixture("Activity.fit");
    let (def_count, data_count, _) = walk(&bytes);

    // From `awk -F',' '/^Definition,/' tests/fixtures/example_files/Activity.csv | wc -l`.
    // Pinning the exact number guards against parser drift (e.g., if we ever
    // mis-count a Definition as a Data record, this fires immediately).
    assert_eq!(def_count, 11, "expected 11 Definition messages in Activity.fit");
    assert!(data_count > 0, "must have produced some Data records too");
}

#[test]
fn activity_definitions_reference_known_mesg_nums() {
    let bytes = read_fixture("Activity.fit");
    let (_, _, mesg_nums) = walk(&bytes);

    // Every global_mesg_num in Activity.fit's definitions must round-trip
    // through the codegen-produced MesgNum table.
    for num in &mesg_nums {
        assert!(
            MesgNum::from_value(*num).is_some(),
            "Activity.fit defines unknown global_mesg_num {num}",
        );
    }

    // Sanity: at minimum, the file declares file_id (0) and record (20).
    assert!(mesg_nums.contains(&0), "expected file_id (0) definition");
    assert!(mesg_nums.contains(&20), "expected record (20) definition");
}

// ────────────────────────────────────────────────────────────────────
// Other fixtures — same structural invariants, sizes will differ.
// ────────────────────────────────────────────────────────────────────

#[test]
fn hrm_plugin_walks_cleanly() {
    let bytes = read_fixture("HrmPluginTestActivity.fit");
    let (def_count, data_count, _) = walk(&bytes);
    assert!(def_count > 0);
    assert!(data_count > 0);
}

#[test]
fn gear_change_walks_cleanly_and_likely_has_dev_data() {
    // This fixture exists specifically to exercise gear_change components,
    // which are top-level Profile fields, not developer fields. Its presence
    // here is mainly to prove the parser handles a third independent file.
    let bytes = read_fixture("WithGearChangeData.fit");
    let (def_count, data_count, mesg_nums) = walk(&bytes);
    assert!(def_count > 0);
    assert!(data_count > 0);
    // The fixture must declare `event` (mesg_num=21), which carries the
    // gear-change SubField on its `data` field.
    assert!(mesg_nums.contains(&21), "expected event (21) definition");
}

// ────────────────────────────────────────────────────────────────────
// Negative paths — Data without prior Definition is rejected cleanly.
// ────────────────────────────────────────────────────────────────────

#[test]
fn data_without_definition_is_an_error() {
    let bytes = read_fixture("Activity.fit");
    let header = FileHeader::parse(&bytes).unwrap();
    let mut stream = ByteStream::new(&bytes);
    stream.seek(header.header_size as usize).unwrap();

    let defs = LocalDefinitions::new(); // empty — no slots filled

    // The first record in Activity.fit is a Definition. Skip its header byte
    // and try to look up that local slot as if it were Data.
    let _first_header = stream.read_u8().unwrap();
    let result = defs.require(0);
    assert!(matches!(
        result,
        Err(fit::FitError::UndefinedLocalMesgNum(0))
    ));
}
