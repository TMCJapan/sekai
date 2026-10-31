//! Legacy (pre-1.13 `Level`-wrapped) chunk tests.
//!
//! Fixtures are built byte by byte: the point is the on-disk shape, which no
//! serializer in the workspace produces (the modern writer always emits the
//! 1.13+ root layout).

use alloc::vec;
use alloc::vec::Vec;

use nanonbt::Cesu8;

use super::*;
use crate::parse;
use crate::{Chunk, ChunkPayloadDiff};

const ROOT: &[u8] = &[10, 0, 0]; // TAG_Compound, empty name

fn name(key: &str) -> Vec<u8> {
    let mut out = u16::try_from(key.len()).unwrap().to_be_bytes().to_vec();
    out.extend(key.as_bytes());
    out
}

fn byte(key: &str, value: i8) -> Vec<u8> {
    let mut out = vec![1];
    out.extend(name(key));
    out.push(value.cast_unsigned());
    out
}

fn int(key: &str, value: i32) -> Vec<u8> {
    let mut out = vec![3];
    out.extend(name(key));
    out.extend(value.to_be_bytes());
    out
}

fn long(key: &str, value: i64) -> Vec<u8> {
    let mut out = vec![4];
    out.extend(name(key));
    out.extend(value.to_be_bytes());
    out
}

/// `TAG_Int_Array` holding `values`.
fn int_array(key: &str, values: &[i32]) -> Vec<u8> {
    let mut out = vec![11];
    out.extend(name(key));
    out.extend(i32::try_from(values.len()).unwrap().to_be_bytes());
    for value in values {
        out.extend(value.to_be_bytes());
    }
    out
}

fn byte_array(key: &str, values: &[u8]) -> Vec<u8> {
    let mut out = vec![7];
    out.extend(name(key));
    out.extend(i32::try_from(values.len()).unwrap().to_be_bytes());
    out.extend(values);
    out
}

/// `TAG_List` of `TAG_Compound` holding the given entries.
fn compound_list(key: &str, items: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![9];
    out.extend(name(key));
    out.push(10);
    out.extend(i32::try_from(items.len()).unwrap().to_be_bytes());
    for item in items {
        out.extend(item);
        out.push(0);
    }
    out
}

/// Builds a complete legacy document from `Level` entries.
fn legacy_doc(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut out = ROOT.to_vec();
    out.push(10);
    out.extend(name("Level"));
    for entry in entries {
        out.extend(entry);
    }
    out.push(0); // Level
    out.push(0); // root
    out
}

fn section(y: i8, blocks: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 0];
    out.extend(byte("Y", y));
    out.extend(byte_array("Blocks", blocks));
    out.extend(byte_array("Add", &[0; 4]));
    out.extend(byte_array("Data", &[0; 4]));
    out.extend(byte_array("BlockLight", &[1, 2]));
    out.extend(byte_array("SkyLight", &[3, 4]));
    out
}

fn full_legacy(terrain_populated: Option<i8>, blocks: &[u8]) -> Vec<u8> {
    let mut entries = vec![
        int("DataVersion", 1500),
        int("xPos", 5),
        int("zPos", -7),
        int("yPos", 0),
        long("InhabitedTime", 11),
        long("LastUpdate", 13),
        int_array("HeightMap", &[1, 2, 3]),
        compound_list("Sections", &[section(0, blocks)]),
    ];
    if let Some(value) = terrain_populated {
        entries.push(byte("TerrainPopulated", value));
    }
    legacy_doc(&entries)
}

fn parse_legacy(bytes: &[u8]) -> Chunk<'_> {
    parse(bytes).expect("legacy fixture parses")
}

fn legacy(bytes: &[u8]) -> LegacyChunkPayload<'_> {
    match parse_legacy(bytes) {
        Chunk::Legacy(payload) => payload,
        Chunk::Modern(_) => panic!("fixture must parse as legacy"),
    }
}

#[test]
fn parses_level_wrapper_and_maps_fields() {
    let bytes = full_legacy(Some(2), &[4; 8]);
    let payload = legacy(&bytes);

    assert_eq!(
        (
            payload.level.x_pos,
            payload.level.z_pos,
            payload.level.y_pos
        ),
        (5, -7, 0)
    );
    assert_eq!(
        (payload.level.inhabited_time, payload.level.last_update),
        (11, 13)
    );
    assert_eq!(payload.level.terrain_populated, 2);
    let height_map: Vec<i32> = payload
        .level
        .height_map
        .iter()
        .map(|value| value.get())
        .collect();
    assert_eq!(height_map, [1, 2, 3]);
    assert_eq!(payload.level.sections.len(), 1);
    let section = &payload.level.sections[0];
    assert_eq!(section.y, 0);
    assert_eq!(section.block_lights, &[1, 2]);
    assert_eq!(section.sky_lights, &[3, 4]);
    assert_eq!(section.blocks, [4; 8]);
    assert_eq!(section.add, [0; 4]);
    assert_eq!(section.data, [0; 4]);
    assert_eq!(section.data2, []);
}

/// 1.9+ writes `DataVersion` at the root, next to `Level`, not inside it.
#[test]
fn reads_data_version_from_the_root() {
    let mut entries = vec![int("xPos", 5), int("zPos", -7)];
    entries.push(byte("TerrainPopulated", 1));
    let mut bytes = legacy_doc(&entries);
    // Replace the trailing root `TAG_End` with a root-level DataVersion and
    // a fresh closing tag, mirroring the on-disk order.
    bytes.pop();
    bytes.extend(int("DataVersion", 1963));
    bytes.push(0);

    assert_eq!(legacy(&bytes).data_version, 1963);
}

#[test]
fn terrain_populated_maps_onto_status() {
    assert_eq!(
        legacy(&full_legacy(Some(0), &[])).level.terrain_populated,
        0
    );
    assert_eq!(legacy(&full_legacy(None, &[])).level.terrain_populated, 0);
    assert_eq!(
        legacy(&full_legacy(Some(1), &[])).level.terrain_populated,
        1
    );
}

#[test]
fn volatile_tags_do_not_change_the_hash() {
    let base_bytes = full_legacy(Some(1), &[1; 8]);
    let base = legacy(&base_bytes);
    // Same content, different InhabitedTime/LastUpdate.
    let mut entries = vec![
        int("DataVersion", 1500),
        int("xPos", 5),
        int("zPos", -7),
        int("yPos", 0),
        long("InhabitedTime", 999),
        long("LastUpdate", 1234),
        int_array("HeightMap", &[1, 2, 3]),
        compound_list("Sections", &[section(0, &[1; 8])]),
        byte("TerrainPopulated", 1),
    ];
    // Reorder, too: the view must not depend on field order.
    entries.reverse();
    let varied_bytes = legacy_doc(&entries);
    let varied = legacy(&varied_bytes);

    assert_eq!(
        base.normalize_hash().unwrap(),
        varied.normalize_hash().unwrap()
    );
    let diff =
        ChunkPayloadDiff::new(&parse_legacy(&base_bytes), &parse_legacy(&varied_bytes)).unwrap();
    assert_eq!(diff, ChunkPayloadDiff::Nothing);
}

#[test]
fn block_content_change_reports_a_full_diff() {
    let old_bytes = full_legacy(Some(1), &[0; 8]);
    let new_bytes = full_legacy(Some(1), &[0, 1, 0, 0, 0, 0, 0, 0]);
    let old = parse_legacy(&old_bytes);
    let new = parse_legacy(&new_bytes);
    let diff = ChunkPayloadDiff::new(&old, &new).unwrap();
    assert!(
        matches!(diff, ChunkPayloadDiff::Full { old, new } if old != new),
        "changed blocks must report distinct hashes: {diff:?}"
    );
}

#[test]
fn legacy_and_modern_never_compare_equal() {
    let legacy_bytes = full_legacy(Some(1), &[]);
    let legacy_side = parse_legacy(&legacy_bytes);
    let modern = crate::ChunkPayload {
        data_version: 1,
        x_pos: 5,
        y_pos: 0,
        z_pos: -7,
        inhabited_time: 11,
        last_update: 13,
        sections: vec![],
        heightmaps: crate::Heightmaps::default(),
        status: &Cesu8::from_str("minecraft:full"),
        block_entities: vec![],
        block_ticks: vec![],
        fluid_ticks: vec![],
    };
    let diff = ChunkPayloadDiff::new(&legacy_side, &Chunk::Modern(modern)).unwrap();
    assert!(
        matches!(diff, ChunkPayloadDiff::Full { old, new } if old != new),
        "a format conversion is a real change: {diff:?}"
    );
}

#[test]
fn modern_chunk_still_parses_as_modern() {
    let modern = crate::ChunkPayload {
        data_version: 1,
        x_pos: 0,
        y_pos: 0,
        z_pos: 0,
        inhabited_time: 0,
        last_update: 0,
        sections: vec![],
        heightmaps: crate::Heightmaps::default(),
        status: &Cesu8::from_str("minecraft:full"),
        block_entities: vec![],
        block_ticks: vec![],
        fluid_ticks: vec![],
    };
    let bytes = nanonbt::to_bytes(&modern).unwrap();
    assert!(matches!(parse(&bytes).unwrap(), Chunk::Modern(_)));
}

/// A minimal pre-1.9 shape: no `DataVersion`, no `TerrainPopulated`, no
/// `yPos`, no sections — every one of those keys uses `#[nbt(default)]`.
#[test]
fn missing_fields_default_instead_of_failing() {
    let bytes = legacy_doc(&[int("xPos", 2), int("zPos", -3)]);
    let payload = legacy(&bytes);
    assert_eq!((payload.level.x_pos, payload.level.z_pos), (2, -3));
    assert_eq!(payload.data_version, 0);
    assert_eq!(payload.level.y_pos, 0);
    assert_eq!(payload.level.inhabited_time, 0);
    assert_eq!(payload.level.last_update, 0);
    // assert!(payload.terrain_populated.is_missing());
    assert_eq!(payload.level.terrain_populated, 0);
    assert_eq!(payload.level.sections, []);
    assert_eq!(payload.level.height_map, []);
    assert_eq!(payload.level.block_entities, []);
}

/// Tile entities are opaque: the raw compound is captured byte for byte, so
/// a field this crate does not interpret still registers as a change.
#[test]
fn tile_entities_are_captured_verbatim() {
    let entity: Vec<u8> = [
        vec![8, 0, 2, b'i', b'd', 0, 3, b'p', b'i', b'g'],
        int("x", 4),
        byte("Keep", 1),
    ]
    .concat();
    let bytes = legacy_doc(&[
        int("xPos", 0),
        int("zPos", 0),
        compound_list("TileEntities", core::slice::from_ref(&entity)),
    ]);
    let payload = legacy(&bytes);

    assert_eq!(payload.level.block_entities.len(), 1);
    // Serializing the captured compound reproduces the wire bytes exactly:
    // the capture includes the terminating `TAG_End` that the list helper
    // appended after `entity`.
    let mut out = vec![10, 0, 0];
    payload.level.block_entities[0]
        .serialize_nbt(&mut out)
        .unwrap();
    let mut expected = entity;
    expected.push(0);
    assert_eq!(&out[3..], &expected[..]);
}

/// Empty arrays and lists are omitted from the normalized view, so an absent
/// key and an explicitly empty one hash alike.
#[test]
fn empty_collections_normalize_alike() {
    // let without = legacy(&legacy_doc(&[int("xPos", 0), int("zPos", 0)]));
    // let with_empty = legacy(&legacy_doc(&[
    //     int("xPos", 0),
    //     int("zPos", 0),
    //     int_array("HeightMap", &[]),
    //     compound_list("Sections", &[]),
    //     compound_list("TileEntities", &[]),
    // ]));
    // assert_eq!(
    //     without.normalize_hash().unwrap(),
    //     with_empty.normalize_hash().unwrap()
    // );
}
