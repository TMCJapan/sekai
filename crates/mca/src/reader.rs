//! Read side: parse `.mca` images into exact chunk payload views.
//!
//! Rationale: the reader is deliberately strict (misaligned or truncated
//! files, inconsistent location entries, and out-of-range lengths are all
//! hard errors). A backup tool must fail loudly on damage rather than
//! silently snapshotting a torn world. Timestamps are informational only
//! and ignored; absence is simply skipped and becomes a tombstone in
//! `engine`.

use std::fs;
use std::path::Path;

use sekai_core::{Dimension, RawChunk, RegionKind};

use crate::error::McaError;
use crate::region::{
    FIRST_DATA_SECTOR, RegionLoc, SECTOR_LEN, TABLE_ENTRIES, check_image_len, parse_region_name,
};

/// Parsed `.mca` image plus its global namespace.
///
/// The image is owned exclusively and only handed out as borrows, so
/// multi-megabyte copies stay explicit: `Clone` is deliberately absent
/// (use `image().to_vec()` when a copy is really needed).
#[derive(Debug)]
pub struct RegionFile {
    /// Dimension namespace (from the caller, not the file).
    dim: Dimension,
    /// Region family (`region`/`entities`/`poi`, from the caller).
    kind: RegionKind,
    /// Region identity and global chunk-column origin.
    loc: RegionLoc,
    /// Whole file image.
    bytes: Vec<u8>,
}

impl RegionFile {
    /// Load and validate a region file from disk.
    ///
    /// This is the `std::fs` boundary of the crate: `mca` is the designated
    /// file-I/O owner per `ARCHITECTURE.md`, so filesystem access lives in
    /// this thin wrapper while [`RegionFile::from_bytes`] and the sector
    /// math below stay pure and fs-free.
    ///
    /// Coordinates come from the `r.<x>.<z>.mca` file name; `dim`/`kind`
    /// come from the caller (directory layout is `engine`'s concern).
    pub fn open(path: &Path, dim: Dimension, kind: RegionKind) -> Result<Self, McaError> {
        let bytes = fs::read(path).map_err(|source| McaError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let (region_x, region_z) = parse_region_name(name)?;
        Self::from_parts(bytes, dim, kind, region_x, region_z)
    }

    /// Parse an in-memory image with explicit coordinates.
    pub fn from_bytes(
        bytes: Vec<u8>,
        dim: Dimension,
        kind: RegionKind,
        region_x: i32,
        region_z: i32,
    ) -> Result<Self, McaError> {
        Self::from_parts(bytes, dim, kind, region_x, region_z)
    }

    fn from_parts(
        bytes: Vec<u8>,
        dim: Dimension,
        kind: RegionKind,
        region_x: i32,
        region_z: i32,
    ) -> Result<Self, McaError> {
        check_image_len(bytes.len() as u64)?;
        let loc = RegionLoc::new(region_x, region_z)?;
        Ok(Self {
            dim,
            kind,
            loc,
            bytes,
        })
    }

    /// Dimension namespace of this file.
    pub fn dim(&self) -> Dimension {
        self.dim
    }

    /// Region family of this file.
    pub fn kind(&self) -> RegionKind {
        self.kind
    }

    /// Region X from the file name.
    pub fn region_x(&self) -> i32 {
        self.loc.region_x()
    }

    /// Region Z from the file name.
    pub fn region_z(&self) -> i32 {
        self.loc.region_z()
    }

    /// Raw file image (for tests and inspection tooling).
    pub fn image(&self) -> &[u8] {
        &self.bytes
    }

    /// Decode one header slot into `(offset_sectors, count_sectors)`.
    fn entry(&self, index: u32) -> Result<(u64, u64), McaError> {
        // Header presence was validated at construction, so this slice is
        // provably in range; `get` keeps it panic-free regardless.
        let off = index as usize * 4;
        let raw = self
            .bytes
            .get(off..off + 4)
            .and_then(|s| s.try_into().ok())
            .map(u32::from_be_bytes);
        let entry = match raw {
            Some(e) => e,
            None => {
                return Err(McaError::CorruptEntry {
                    index,
                    offset: 0,
                    sectors: 0,
                });
            }
        };
        Ok(((entry >> 8) as u64, (entry & 0xFF) as u64))
    }
}

impl sekai_core::RegionReader for RegionFile {
    type Error = McaError;

    fn visit_chunks<F>(&self, mut visit: F) -> Result<(), Self::Error>
    where
        F: FnMut(RawChunk<'_>) -> bool,
    {
        let total_sectors = self.bytes.len() as u64 / SECTOR_LEN;
        for index in 0..TABLE_ENTRIES {
            let (offset, count) = self.entry(index)?;
            if offset == 0 && count == 0 {
                continue;
            }
            // Sectors 0-1 are the header; a zero count with an offset (or
            // vice versa) can never address a payload.
            if offset < FIRST_DATA_SECTOR || count == 0 {
                return Err(McaError::CorruptEntry {
                    index,
                    offset: offset as u32,
                    sectors: count as u32,
                });
            }
            if offset + count > total_sectors {
                return Err(McaError::CorruptEntry {
                    index,
                    offset: offset as u32,
                    sectors: count as u32,
                });
            }
            let base = offset * SECTOR_LEN;
            let len_raw = self
                .bytes
                .get(base as usize..base as usize + 4)
                .ok_or(McaError::CorruptChunk { index, len: 0 })?;
            let len = u32::from_be_bytes([len_raw[0], len_raw[1], len_raw[2], len_raw[3]]) as u64;
            // Length covers the type byte plus body and must fit the
            // allocated sectors.
            if len < 1 || len + 4 > count * SECTOR_LEN {
                return Err(McaError::CorruptChunk {
                    index,
                    len: len as u32,
                });
            }
            let start = base + 4;
            let payload = self
                .bytes
                .get(start as usize..(start + len) as usize)
                .ok_or(McaError::CorruptChunk {
                    index,
                    len: len as u32,
                })?;
            let coord = self.loc.coord_at(self.dim, self.kind, index);
            if !visit(RawChunk::new(coord, payload)) {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn entry_of(offset: u32, sectors: u32) -> [u8; 4] {
    ((offset << 8) | sectors).to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_core::{ChunkCoord, RegionReader as _};

    /// Minimal valid image: header only, no chunks.
    fn header_only() -> Vec<u8> {
        vec![0u8; 8192]
    }

    fn coords(dim: Dimension, kind: RegionKind, x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(dim, kind, x, z)
    }

    #[test]
    fn empty_region_yields_no_chunks() {
        let f = RegionFile::from_bytes(
            header_only(),
            Dimension::OVERWORLD,
            RegionKind::REGION,
            0,
            0,
        )
        .expect("header-only image must parse");
        let mut count = 0;
        f.visit_chunks(|_| {
            count += 1;
            true
        })
        .expect("visit must succeed");
        assert_eq!(count, 0);
    }

    #[test]
    fn reads_single_chunk_with_global_coords() {
        // Region r.-1.0, slot (lx=3, lz=5) -> global (-29, 5), 1 sector.
        let mut img = header_only();
        img.extend_from_slice(&[0u8; 4096]);
        let slot = (3 + 32 * 5) * 4;
        img[slot..slot + 4].copy_from_slice(&entry_of(2, 1));
        let sector = 8192;
        img[sector..sector + 4].copy_from_slice(&5u32.to_be_bytes());
        img[sector + 4] = 2;
        img[sector + 5..sector + 9].copy_from_slice(b"nbt!");
        let f = RegionFile::from_bytes(img, Dimension::NETHER, RegionKind::ENTITIES, -1, 0)
            .expect("image must parse");
        let mut seen = Vec::new();
        f.visit_chunks(|c| {
            seen.push((c.coord, c.payload.to_vec()));
            true
        })
        .expect("visit must succeed");
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].0,
            coords(Dimension::NETHER, RegionKind::ENTITIES, -29, 5)
        );
        assert_eq!(seen[0].1, [&[2u8] as &[_], b"nbt!"].concat());
    }

    #[test]
    fn visitor_can_stop_early() {
        let mut img = header_only();
        img.extend_from_slice(&[0u8; 8192]);
        img[0..4].copy_from_slice(&entry_of(2, 1));
        img[4..8].copy_from_slice(&entry_of(3, 1));
        for (sector, slot) in [(8192, 0), (12288, 1)] {
            img[sector..sector + 4].copy_from_slice(&2u32.to_be_bytes());
            img[sector + 4] = 3;
            img[sector + 5] = slot;
        }
        let f = RegionFile::from_bytes(img, Dimension::OVERWORLD, RegionKind::REGION, 0, 0)
            .expect("image must parse");
        let mut count = 0;
        f.visit_chunks(|_| {
            count += 1;
            false
        })
        .expect("visit must succeed");
        assert_eq!(count, 1);
    }

    #[test]
    fn rejects_damaged_images() {
        let dim = Dimension::OVERWORLD;
        let kind = RegionKind::REGION;
        // Truncated and misaligned.
        assert!(matches!(
            RegionFile::from_bytes(vec![0u8; 100], dim, kind, 0, 0),
            Err(McaError::TruncatedFile { .. })
        ));
        assert!(matches!(
            RegionFile::from_bytes(vec![0u8; 8193], dim, kind, 0, 0),
            Err(McaError::MisalignedFile { .. })
        ));
        // Entry pointing past EOF.
        let mut img = header_only();
        img.extend_from_slice(&[0u8; 4096]);
        img[0..4].copy_from_slice(&entry_of(9, 1));
        let f = RegionFile::from_bytes(img, dim, kind, 0, 0).expect("header parses");
        assert!(matches!(
            f.visit_chunks(|_| true),
            Err(McaError::CorruptEntry { .. })
        ));
        // Offset set but count zero.
        let mut img = header_only();
        img.extend_from_slice(&[0u8; 4096]);
        img[0..4].copy_from_slice(&entry_of(2, 0));
        let f = RegionFile::from_bytes(img, dim, kind, 0, 0).expect("header parses");
        assert!(matches!(
            f.visit_chunks(|_| true),
            Err(McaError::CorruptEntry { .. })
        ));
        // Declared length escaping the allocation.
        let mut img = header_only();
        img.extend_from_slice(&[0u8; 4096]);
        img[0..4].copy_from_slice(&entry_of(2, 1));
        img[8192..8196].copy_from_slice(&5000u32.to_be_bytes());
        let f = RegionFile::from_bytes(img, dim, kind, 0, 0).expect("header parses");
        assert!(matches!(
            f.visit_chunks(|_| true),
            Err(McaError::CorruptChunk { .. })
        ));
        // Zero length (no room for the type byte).
        let mut img = header_only();
        img.extend_from_slice(&[0u8; 4096]);
        img[0..4].copy_from_slice(&entry_of(2, 1));
        img[8192..8196].copy_from_slice(&0u32.to_be_bytes());
        let f = RegionFile::from_bytes(img, dim, kind, 0, 0).expect("header parses");
        assert!(matches!(
            f.visit_chunks(|_| true),
            Err(McaError::CorruptChunk { .. })
        ));
    }

    #[test]
    fn open_rejects_missing_files_and_absurd_regions() {
        let dim = Dimension::OVERWORLD;
        let kind = RegionKind::REGION;
        assert!(matches!(
            RegionFile::open(Path::new("nope.mca"), dim, kind),
            Err(McaError::Io { .. })
        ));
        assert!(matches!(
            RegionFile::from_bytes(header_only(), dim, kind, i32::MAX, 0),
            Err(McaError::CoordinateOverflow { .. })
        ));
    }
}
