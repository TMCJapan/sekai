//! Write side: rebuild `.mca` files from exact payloads, atomically.
//!
//! Rationale: rollback reconstructs whole region files from CAS blobs, so
//! the writer packs staged payloads into a fresh image (header recomputed,
//! chunks in slot order) and swaps it into place via same-directory temp
//! file + `fsync` + `rename`. The target is never touched until the swap,
//! and a torn temp file can never be mistaken for a region (its suffix is
//! not `.mca`). Timestamps are uniform per file (caller-provided, normally
//! the snapshot time); absent slots read zero.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sekai_core::{ChunkCoord, Dimension, RegionKind};

use crate::error::McaError;
use crate::region::{
    HEADER_LEN, MAX_SECTOR_OFFSET, RegionLoc, SECTOR_LEN, parse_region_name, sectors_for,
};

/// Monotonic temp-file disambiguator within this process.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Pending region rewrite; finalize with [`commit`](sekai_core::RegionWriter::commit).
#[derive(Debug)]
pub struct RegionFileWriter {
    /// Dimension namespace (staged coords must match).
    dim: Dimension,
    /// Region family (staged coords must match).
    kind: RegionKind,
    /// Region identity and global chunk-column origin.
    loc: RegionLoc,
    /// Timestamp written for every present chunk.
    timestamp: u32,
    /// Final destination (never written before commit).
    target: PathBuf,
    /// Payloads by header slot, packed in slot order at commit.
    staged: BTreeMap<u32, Vec<u8>>,
}

impl RegionFileWriter {
    /// Prepare a rewrite of `target` (`r.<x>.<z>.mca`).
    ///
    /// Filesystem access is confined to `swap` (like [`crate::RegionFile::open`],
    /// this only parses the target name); staging and image assembly below
    /// are pure in-memory operations.
    ///
    /// Nothing is created on disk yet; `timestamp` fills the timestamp
    /// table for staged chunks. Bad target names fail here so a typo can
    /// never silently map chunks into the wrong region.
    pub fn create(
        target: &Path,
        dim: Dimension,
        kind: RegionKind,
        timestamp: u32,
    ) -> Result<Self, McaError> {
        let name = target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let (region_x, region_z) = parse_region_name(name)?;
        let loc = RegionLoc::new(region_x, region_z)?;
        Ok(Self {
            dim,
            kind,
            loc,
            timestamp,
            target: target.to_path_buf(),
            staged: BTreeMap::new(),
        })
    }

    /// Header slot for `coord`, rejecting foreign coordinates.
    fn slot_of(&self, coord: &ChunkCoord) -> Result<u32, McaError> {
        self.loc.slot_of(self.dim, self.kind, coord)
    }

    /// Assemble the full file image (header + packed sectors).
    fn image(&self) -> Result<Vec<u8>, McaError> {
        let header_len = usize::try_from(HEADER_LEN).map_err(|_| McaError::ImageTooLarge {
            sectors: HEADER_LEN,
        })?;
        let mut img = vec![0u8; header_len];
        let mut offset: u64 = HEADER_LEN / SECTOR_LEN;
        for (index, payload) in &self.staged {
            let sectors = sectors_for(payload.len())?;
            if offset > MAX_SECTOR_OFFSET {
                return Err(McaError::ImageTooLarge { sectors: offset });
            }
            let offset_u32 =
                u32::try_from(offset).map_err(|_| McaError::ImageTooLarge { sectors: offset })?;
            let sectors_u32 =
                u32::try_from(sectors).map_err(|_| McaError::ImageTooLarge { sectors: offset })?;
            let entry = (offset_u32 << 8) | sectors_u32;
            let at = *index as usize * 4;
            img[at..at + 4].copy_from_slice(&entry.to_be_bytes());
            let ts_at = header_len / 2 + *index as usize * 4;
            img[ts_at..ts_at + 4].copy_from_slice(&self.timestamp.to_be_bytes());
            let payload_len_u32 = u32::try_from(payload.len())
                .map_err(|_| McaError::ChunkTooLarge { len: payload.len() })?;
            img.extend_from_slice(&payload_len_u32.to_be_bytes());
            img.extend_from_slice(payload);
            let pad = sectors * SECTOR_LEN - (payload.len() as u64 + 4);
            let pad_usize =
                usize::try_from(pad).map_err(|_| McaError::ImageTooLarge { sectors: offset })?;
            img.extend(std::iter::repeat_n(0, pad_usize));
            offset += sectors;
        }
        Ok(img)
    }

    /// Swap `img` into place via same-directory temp + fsync + rename.
    fn swap(&self, img: &[u8]) -> Result<(), McaError> {
        let io = |path: PathBuf| move |source: std::io::Error| McaError::Io { path, source };
        let parent = self
            .target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let file_name = self
            .target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("region.mca");
        let tmp = parent.join(format!(
            "{file_name}.tmp-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let write = || -> Result<(), McaError> {
            use std::io::Write as _;
            let mut f = fs::File::create(&tmp).map_err(io(tmp.clone()))?;
            f.write_all(img).map_err(io(tmp.clone()))?;
            f.sync_all().map_err(io(tmp.clone()))?;
            drop(f);
            fs::rename(&tmp, &self.target).map_err(io(self.target.clone()))?;
            // Persist the rename itself. Unix-only: opening a directory
            // with `File::open` fails on Windows (ERROR_ACCESS_DENIED),
            // and std offers no directory-fsync equivalent there.
            #[cfg(unix)]
            {
                let dir = fs::File::open(&parent).map_err(io(parent.clone()))?;
                dir.sync_all().map_err(io(parent.clone()))?;
            }
            Ok(())
        };
        let result = write();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

impl sekai_core::RegionWriter for RegionFileWriter {
    type Error = McaError;

    fn stage_chunk(&mut self, coord: &ChunkCoord, payload: &[u8]) -> Result<(), Self::Error> {
        if payload.is_empty() {
            return Err(McaError::EmptyPayload);
        }
        let index = self.slot_of(coord)?;
        // Fail early on oversized payloads, not at commit time.
        sectors_for(payload.len())?;
        self.staged.insert(index, payload.to_vec());
        Ok(())
    }

    fn stage_remove(&mut self, coord: &ChunkCoord) -> Result<(), Self::Error> {
        let index = self.slot_of(coord)?;
        self.staged.remove(&index);
        Ok(())
    }

    fn commit(self) -> Result<(), Self::Error> {
        let img = self.image()?;
        self.swap(&img)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::reader::entry_of;
    use sekai_core::{RegionKind, RegionWriter as _};

    fn writer() -> RegionFileWriter {
        RegionFileWriter::create(
            Path::new("/tmp/r.0.0.mca"),
            Dimension::OVERWORLD,
            RegionKind::REGION,
            0x1234_5678,
        )
        .expect("test writer target must parse")
    }

    fn coord(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, x, z)
    }

    #[test]
    fn create_rejects_bad_target_names() {
        assert!(matches!(
            RegionFileWriter::create(
                Path::new("/tmp/oops.mca"),
                Dimension::OVERWORLD,
                RegionKind::REGION,
                0
            ),
            Err(McaError::BadFilename { .. })
        ));
    }

    #[test]
    fn stages_and_forgets() {
        let mut w = writer();
        w.stage_chunk(&coord(1, 2), &[2, 9, 9]).expect("stage");
        assert_eq!(w.staged.len(), 1);
        w.stage_remove(&coord(1, 2)).expect("remove");
        assert!(w.staged.is_empty());
        // Removing an absent chunk is a no-op (idempotent tombstones).
        w.stage_remove(&coord(1, 2)).expect("repeat remove");
    }

    #[test]
    fn rejects_foreign_and_empty_and_huge() {
        let mut w = writer();
        assert!(matches!(
            w.stage_chunk(&coord(32, 0), &[1]),
            Err(McaError::WrongRegion { .. })
        ));
        assert!(matches!(
            w.stage_chunk(&coord(-1, 0), &[1]),
            Err(McaError::WrongRegion { .. })
        ));
        assert!(matches!(
            w.stage_chunk(
                &ChunkCoord::new(Dimension::NETHER, RegionKind::REGION, 0, 0),
                &[1]
            ),
            Err(McaError::WrongRegion { .. })
        ));
        assert!(matches!(
            w.stage_chunk(&coord(0, 0), &[]),
            Err(McaError::EmptyPayload)
        ));
        assert!(matches!(
            w.stage_chunk(&coord(0, 0), &vec![0u8; 255 * 4096]),
            Err(McaError::ChunkTooLarge { .. })
        ));
    }

    #[test]
    fn packs_image_with_header_and_padding() {
        let mut w = writer();
        // Slot 0: 1 sector; slot 33 (lx=1, lz=1): spills into 2 sectors.
        w.stage_chunk(&coord(0, 0), &[2, 7]).expect("stage");
        let big = vec![5u8; 4093];
        w.stage_chunk(&coord(1, 1), &big).expect("stage big");
        let img = w.image().expect("image builds");
        // Slot 0 -> offset 2, 1 sector; slot 33 -> offset 3, 2 sectors.
        assert_eq!(&img[0..4], &entry_of(2, 1));
        assert_eq!(&img[33 * 4..33 * 4 + 4], &entry_of(3, 2));
        // Timestamps: present slots carry the writer stamp, others zero.
        assert_eq!(&img[4096..4100], &0x1234_5678u32.to_be_bytes());
        assert_eq!(
            &img[4096 + 33 * 4..4096 + 33 * 4 + 4],
            &0x1234_5678u32.to_be_bytes()
        );
        assert_eq!(&img[4096 + 4..4096 + 8], &[0u8; 4]);
        // Payload framing: length prefix equals the staged payload length
        // (the type byte is part of the payload, not the prefix).
        assert_eq!(&img[8192..8196], &2u32.to_be_bytes());
        assert_eq!(&img[8196..8198], &[2u8, 7u8]);
        let second = 8192 + 4096;
        assert_eq!(&img[second..second + 4], &4093u32.to_be_bytes());
        assert!(img.len().is_multiple_of(4096));
        assert_eq!(img.len(), 8192 + 4096 + 8192);
    }
}
