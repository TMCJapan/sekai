//! Deterministic seeded world generator for benchmarks.
//!
//! Produces valid `.mca` files (parseable NBT, zlib framing) with tunable
//! density and compressibility. NOT game-faithful: sekai never interprets
//! game semantics, so payloads only need structural variety. Seeded
//! xorshift keeps every run reproducible without extra dependencies.

use std::path::Path;

use criterion::Criterion;

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
// Shared per bench target via `#[path]`; not every target uses it.
#[allow(dead_code)]
pub const MEDIUM: WorldSpec = WorldSpec {
    regions: 8,
    density_permille: 900,
    payload_bytes: 2048,
    compressible_permille: 200,
    seed: 1337,
};

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

fn chunk_nbt(rng: &mut Rng, spec: &WorldSpec, x: i32, z: i32) -> Vec<u8> {
    let mut block_lights = vec![0u8; spec.payload_bytes];
    rng.fill(&mut block_lights, spec.compressible_permille);

    let mut nbt = vec![10, 0, 0];
    int(&mut nbt, "DataVersion", 3_465);
    int(&mut nbt, "xPos", x);
    int(&mut nbt, "yPos", 0);
    int(&mut nbt, "zPos", z);
    long(&mut nbt, "InhabitedTime", 0);
    long(&mut nbt, "LastUpdate", 0);

    nbt.push(9);
    name(&mut nbt, "sections");
    nbt.push(10);
    nbt.extend_from_slice(&1i32.to_be_bytes());
    nbt.push(1);
    name(&mut nbt, "Y");
    nbt.push(0);
    nbt.push(7);
    name(&mut nbt, "BlockLight");
    nbt.extend_from_slice(&i32::try_from(block_lights.len()).unwrap().to_be_bytes());
    nbt.extend_from_slice(&block_lights);
    nbt.push(0);

    empty_compound(&mut nbt, "Heightmaps");
    string(&mut nbt, "Status", "minecraft:full");
    empty_list(&mut nbt, "block_entities");
    empty_list(&mut nbt, "block_ticks");
    empty_list(&mut nbt, "fluid_ticks");
    nbt.push(0);
    nbt
}

/// Generate `spec` into `world/region/` (overworld only).
pub fn generate(world: impl AsRef<Path>, spec: &WorldSpec) {
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
        let dir = world.as_ref().join("region");
        std::fs::create_dir_all(&dir).expect("mkdir works");
        let image = builder.image().expect("image assembles");
        write_region(dir.join(format!("r.{r}.0.mca")), &image);
    }
}

/// Fresh temp dir for one benchmark setup, named after the benchmark so
/// a leaked directory stays traceable. The guard removes it on drop, which
/// also covers panics.
///
/// The parent comes from [`temp_root`]: the medium world and its store run
/// into hundreds of megabytes, more than a modest `/tmp` tmpfs holds.
pub fn tempdir(name: &str) -> tempfile::TempDir {
    let root = temp_root();
    tempfile::Builder::new()
        .prefix(&format!("sekai-bench-{name}-"))
        .tempdir_in(&root)
        .unwrap_or_else(|err| {
            panic!(
                "no bench temp dir under {}: {err}\n\
                 point SEKAI_BENCH_TMP (or TMPDIR) at a roomier filesystem",
                root.display()
            )
        })
}

/// Where benchmark worlds and stores live, in order of preference:
/// `SEKAI_BENCH_TMP`, then `TMPDIR`, then the system temp directory.
///
/// The first one is exported so a failed write can name the filesystem that
/// ran out instead of leaving the caller to infer it.
fn temp_root() -> std::path::PathBuf {
    ["SEKAI_BENCH_TMP", "TMPDIR"]
        .into_iter()
        .find_map(|name| std::env::var_os(name).map(std::path::PathBuf::from))
        .unwrap_or_else(std::env::temp_dir)
}

/// Write one region image, naming the temp root when the disk refuses it.
///
/// Out of space fails every subsequent write too, so a bare `write works`
/// panic would only name the symptom: send the reader to the knob instead.
fn write_region(path: impl AsRef<Path>, image: &[u8]) {
    let path = path.as_ref();
    std::fs::write(path, image).unwrap_or_else(|err| {
        panic!(
            "writing {} failed: {err}\n\
             benchmark data lives under {}; SEKAI_BENCH_TMP moves it",
            path.display(),
            temp_root().display()
        )
    });
}

/// Default bench options (workers follow CPU count).
pub const fn options() -> sekai_app::CommitOptions {
    sekai_app::CommitOptions { concurrency: 0 }
}

/// Multi-threaded runtime shared by async benches.
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime builds")
}

/// Run one Criterion group on a thread with a generous, host-independent stack.
///
/// Criterion drives every benchmark from `main`, and Windows gives that thread
/// only 1 MiB. The debug-build async state machines behind the store benches
/// overflow it, so use a stack size that does not depend on the platform.
pub fn run(benches: fn(&mut Criterion)) {
    std::thread::Builder::new()
        .name("criterion".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let mut criterion = Criterion::default().configure_from_args();
            benches(&mut criterion);
            criterion.final_summary();
        })
        .expect("bench thread starts")
        .join()
        .expect("bench thread finishes");
}

/// Copy the checked-in real-data corpus into a temp world.
///
/// Never points at the repo files themselves (checkout rewrites region
/// files). The checkout and diff benches use it; every bench is its own
/// binary, so the others see it as unused and the dead-code lint stays
/// off.
#[allow(dead_code)]
pub fn corpus_world() -> tempfile::TempDir {
    fn copy_dir(src: impl AsRef<Path>, dst: impl AsRef<Path>) {
        std::fs::create_dir_all(dst.as_ref()).expect("mkdir works");
        for entry in std::fs::read_dir(src.as_ref()).expect("read works") {
            let entry = entry.expect("entry works");
            let target = dst.as_ref().join(entry.file_name());
            if entry.file_type().expect("type works").is_dir() {
                copy_dir(entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), &target).expect("copy works");
            }
        }
    }
    let root = tempdir("corpus");
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    copy_dir(
        manifest.join("..").join("..").join("test-world"),
        root.path().join("world"),
    );
    root
}
