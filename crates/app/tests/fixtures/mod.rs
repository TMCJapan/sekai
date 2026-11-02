//! Minimal level-chunk payloads for end-to-end app tests.
//!
//! Ingest now hashes every chunk through its normalized view, so a fake
//! payload that is not a level chunk has no key. These helpers produce a
//! real 1.13+ chunk whose only content is a distinguishable `Status`, which
//! keeps each test's payloads distinct while staying parseable.

use sekai_nbt::nanonbt::Cesu8;

/// A framed (uncompressed) level chunk whose `Status` names `marker`.
pub fn chunk_nbt(marker: &str) -> Vec<u8> {
    let chunk = sekai_nbt::nanonbt::to_bytes(&sekai_nbt::ChunkPayload {
        data_version: 1,
        x_pos: 0,
        y_pos: 0,
        z_pos: 0,
        inhabited_time: 0,
        last_update: 0,
        sections: vec![],
        heightmaps: sekai_nbt::Heightmaps::default(),
        status: &Cesu8::from_str(marker),
        block_entities: vec![],
        block_ticks: vec![],
        fluid_ticks: vec![],
    })
    .unwrap();
    let mut payload = vec![3]; // Uncompressed header byte
    payload.extend(chunk);
    payload
}
