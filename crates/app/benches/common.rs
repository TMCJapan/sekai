//! Deterministic seeded world generator for benchmarks.
//!
//! Produces valid `.mca` files (parseable NBT, zlib framing) with tunable
//! density and compressibility. NOT game-faithful: sekai never interprets
//! game semantics, so payloads only need structural variety. Seeded
//! xorshift keeps every run reproducible without extra dependencies.

use std::path::Path;

/// Seeded xorshift64: deterministic filler without dependencies.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    // Mutates state, so `const` is inapplicable despite the lint's
    // suggestion (`const_mut_refs` cannot prove this sound yet).
    #[allow(clippy::missing_const_for_fn)]
    fn next(&mut self) -> u64 {
        self.0 = step(self.0);
        self.0
    }

    /// Fill `buf`; `cut` permille of 64-byte blocks repeat.
    pub fn fill(&mut self, buf: &mut [u8], cut: u64) {
        let mut i = 0;
        while i < buf.len() {
            let block = if self.next() % 1000 < cut {
                0xAB
            } else {
                (self.next() & 0xFF) as u8
            };
            let end = (i + 64).min(buf.len());
            buf[i..end].fill(block);
            i = end;
        }
    }
}

/// One xorshift64 step (pure so it stays `const`).
const fn step(x: u64) -> u64 {
    let x = x ^ (x.wrapping_shl(13));
    let x = x ^ (x.wrapping_shr(7));
    x ^ (x.wrapping_shl(17))
}

/// World shape knobs. Fractions are permille (`0..=1000`) so the
/// generator needs no float casts.
#[derive(Debug, Clone, Copy)]
pub struct WorldSpec {
    /// Region files per dimension directory.
    pub regions: usize,
    /// Permille of the 1024 chunk slots filled per region.
    pub density_permille: u64,
    /// Raw NBT body bytes per chunk.
    pub payload_bytes: usize,
    /// Permille of 64-byte blocks that repeat (compressibility).
    pub compressible_permille: u64,
    /// Seed for chunk selection and filler.
    pub seed: u64,
}

/// Small smoke shape: 2 sparse regions, fits in milliseconds.
pub const SMALL: WorldSpec = WorldSpec {
    regions: 2,
    density_permille: 100,
    payload_bytes: 512,
    compressible_permille: 500,
    seed: 42,
};

/// Medium shape: 8 dense regions, exercises ingest phases.
pub const MEDIUM: WorldSpec = WorldSpec {
    regions: 8,
    density_permille: 900,
    payload_bytes: 2048,
    compressible_permille: 200,
    seed: 1337,
};

fn chunk_nbt(rng: &mut Rng, spec: &WorldSpec, x: i32, z: i32) -> Vec<u8> {
    let mut body = vec![0u8; spec.payload_bytes];
    rng.fill(&mut body, spec.compressible_permille);
    let mut nbt = vec![10, 0, 0];
    nbt.extend_from_slice(b"\x08\x00\x06Status");
    let status = b"minecraft:full";
    nbt.extend_from_slice(&u16::try_from(status.len()).unwrap().to_be_bytes());
    nbt.extend_from_slice(status);
    nbt.extend_from_slice(b"\x04\x00\x04Data");
    nbt.extend_from_slice(&(x).to_be_bytes());
    nbt.extend_from_slice(&(z).to_be_bytes());
    nbt.extend_from_slice(b"\x07\x00\x07Payload");
    nbt.extend_from_slice(&i32::try_from(body.len()).unwrap().to_be_bytes());
    nbt.extend_from_slice(&body);
    nbt.push(0);
    nbt
}

/// Generate `spec` into `world/region/` (overworld only).
pub fn generate(world: &Path, spec: &WorldSpec) {
    let mut rng = Rng::new(spec.seed);
    for r in 0..spec.regions {
        let rx = i32::try_from(r).expect("few regions");
        let mut builder = sekai_anvil::RegionBuilder::new(rx, 0, 0).expect("region coords fit");
        for slot in 0..1024 {
            if rng.next() % 1000 >= spec.density_permille {
                continue;
            }
            let x = rx * 32 + slot % 32;
            let z = slot / 32;
            let nbt = chunk_nbt(&mut rng, spec, x, z);
            let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&nbt, 6);
            let mut payload = vec![2u8];
            payload.extend_from_slice(&compressed);
            builder.stage_chunk(x, z, &payload).expect("chunk fits");
        }
        let dir = world.join("region");
        std::fs::create_dir_all(&dir).expect("mkdir works");
        let image = builder.image().expect("image assembles");
        std::fs::write(dir.join(format!("r.{r}.0.mca")), image).expect("write works");
    }
}
