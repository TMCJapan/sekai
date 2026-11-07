//! Minimal level-chunk payloads for end-to-end app tests.
//!
//! Ingest hashes every chunk through its normalized view, so a fake
//! payload that is not a level chunk has no key. These helpers produce a
//! real 1.18+ chunk whose only content is a distinguishable `Status`, which
//! keeps each test's payloads distinct while staying parseable.

fn name(out: &mut Vec<u8>, key: &str) {
    out.extend_from_slice(&u16::try_from(key.len()).unwrap().to_be_bytes());
    out.extend_from_slice(key.as_bytes());
}

fn int(out: &mut Vec<u8>, key: &str, value: i32) {
    out.push(3);
    name(out, key);
    out.extend_from_slice(&value.to_be_bytes());
}

fn long(out: &mut Vec<u8>, key: &str, value: i64) {
    out.push(4);
    name(out, key);
    out.extend_from_slice(&value.to_be_bytes());
}

fn string(out: &mut Vec<u8>, key: &str, value: &str) {
    out.push(8);
    name(out, key);
    out.extend_from_slice(&u16::try_from(value.len()).unwrap().to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn empty_list(out: &mut Vec<u8>, key: &str) {
    out.push(9);
    name(out, key);
    out.push(0);
    out.extend_from_slice(&0i32.to_be_bytes());
}

fn empty_compound(out: &mut Vec<u8>, key: &str) {
    out.push(10);
    name(out, key);
    out.push(0);
}

/// A framed (uncompressed) level chunk whose `Status` names `marker`.
pub fn chunk_nbt(marker: &str) -> Vec<u8> {
    let mut nbt = vec![10, 0, 0];
    int(&mut nbt, "DataVersion", 1);
    int(&mut nbt, "xPos", 0);
    int(&mut nbt, "yPos", 0);
    int(&mut nbt, "zPos", 0);
    long(&mut nbt, "InhabitedTime", 0);
    long(&mut nbt, "LastUpdate", 0);
    empty_list(&mut nbt, "sections");
    empty_compound(&mut nbt, "Heightmaps");
    string(&mut nbt, "Status", marker);
    empty_list(&mut nbt, "block_entities");
    empty_list(&mut nbt, "block_ticks");
    empty_list(&mut nbt, "fluid_ticks");
    nbt.push(0);

    let mut payload = vec![3]; // Uncompressed header byte
    payload.extend(nbt);
    payload
}
