//! Read-side parsing of `.mca` region images.

use alloc::vec::Vec;

use crate::error::AnvilError;
use crate::region::{FIRST_DATA_SECTOR, RegionLoc, SECTOR_LEN, TABLE_ENTRIES};

/// One present chunk and its exact stored payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk<'a> {
    /// Global chunk X coordinate.
    pub x: i32,

    /// Global chunk Z coordinate.
    pub z: i32,

    /// Stored compression byte followed by the compressed body.
    pub payload: &'a [u8],
}

/// Parsed `.mca` image.
///
/// The image owns its backing bytes and exposes chunks only as borrows.
#[derive(Debug)]
pub struct RegionImage {
    loc: RegionLoc,
    bytes: Vec<u8>,
}

impl RegionImage {
    /// Parses an in-memory region image.
    pub fn from_bytes(bytes: Vec<u8>, region_x: i32, region_z: i32) -> Result<Self, AnvilError> {
        crate::region::check_image_len(bytes.len() as u64)?;

        Ok(Self {
            loc: RegionLoc::new(region_x, region_z)?,
            bytes,
        })
    }

    pub const fn region_x(&self) -> i32 {
        self.loc.region_x()
    }

    pub const fn region_z(&self) -> i32 {
        self.loc.region_z()
    }

    pub fn image(&self) -> &[u8] {
        &self.bytes
    }

    /// Visits all present chunks in table order.
    ///
    /// Returning `false` stops iteration.
    pub fn visit_chunks<F>(&self, mut visit: F) -> Result<(), AnvilError>
    where
        F: FnMut(Chunk<'_>) -> bool,
    {
        if self.bytes.is_empty() {
            return Ok(());
        }

        let total_sectors = self.bytes.len() as u64 / SECTOR_LEN;

        for index in 0..TABLE_ENTRIES {
            let (offset, count) = self.entry(index)?;

            if offset == 0 && count == 0 {
                continue;
            }

            if offset < FIRST_DATA_SECTOR || count == 0 {
                return Err(Self::corrupt_entry(index, offset, count));
            }

            let end = offset
                .checked_add(count)
                .ok_or_else(|| Self::corrupt_entry(index, offset, count))?;

            if end > total_sectors {
                return Err(Self::corrupt_entry(index, offset, count));
            }

            let payload = self.sector_payload(index, offset, count)?;

            let (x, z) = self.loc.coord_at(index);

            if !visit(Chunk { x, z, payload }) {
                break;
            }
        }

        Ok(())
    }

    fn corrupt_entry(index: u32, offset: u64, count: u64) -> AnvilError {
        AnvilError::CorruptEntry {
            index,
            offset: saturate(offset),
            sectors: saturate(count),
        }
    }

    fn entry(&self, index: u32) -> Result<(u64, u64), AnvilError> {
        let offset = index as usize * 4;

        let bytes = self
            .bytes
            .get(offset..offset + 4)
            .ok_or(AnvilError::CorruptEntry {
                index,
                offset: 0,
                sectors: 0,
            })?;

        let entry = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);

        Ok((u64::from(entry >> 8), u64::from(entry & 0xFF)))
    }

    fn sector_payload(&self, index: u32, offset: u64, count: u64) -> Result<&[u8], AnvilError> {
        let base = offset
            .checked_mul(SECTOR_LEN)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| Self::corrupt_entry(index, offset, count))?;

        let len_bytes = self
            .bytes
            .get(base..base + 4)
            .ok_or(AnvilError::CorruptChunk { index, len: 0 })?;

        let len = u32::from_be_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);

        let capacity = count
            .checked_mul(SECTOR_LEN)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(AnvilError::CorruptChunk { index, len })?;

        let len_usize =
            usize::try_from(len).map_err(|_| AnvilError::CorruptChunk { index, len })?;

        if len == 0 || len_usize + 4 > capacity {
            return Err(AnvilError::CorruptChunk { index, len });
        }

        let start = base + 4;
        let end = start + len_usize;

        self.bytes
            .get(start..end)
            .ok_or(AnvilError::CorruptChunk { index, len })
    }
}

fn saturate(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
pub(crate) const fn entry_of(offset: u32, sectors: u32) -> [u8; 4] {
    ((offset << 8) | sectors).to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn header_only() -> Vec<u8> {
        alloc::vec![0; 8192]
    }

    #[test]
    fn empty_region_yields_no_chunks() {
        let image = RegionImage::from_bytes(header_only(), 0, 0).unwrap();

        let mut count = 0;

        image
            .visit_chunks(|_| {
                count += 1;
                true
            })
            .unwrap();

        assert_eq!(count, 0);
    }

    #[test]
    fn zero_length_image_is_empty_region() {
        let image = RegionImage::from_bytes(Vec::new(), 0, 0).unwrap();

        assert!(image.image().is_empty());

        let mut count = 0;

        image
            .visit_chunks(|_| {
                count += 1;
                true
            })
            .unwrap();

        assert_eq!(count, 0);
    }

    #[test]
    fn reads_single_chunk_with_global_coords() {
        let mut image = header_only();

        image.extend_from_slice(&[0; 4096]);

        let slot = (3 + 32 * 5) * 4;

        image[slot..slot + 4].copy_from_slice(&entry_of(2, 1));

        let sector = 8192;

        image[sector..sector + 4].copy_from_slice(&5u32.to_be_bytes());

        image[sector + 4] = 2;

        image[sector + 5..sector + 9].copy_from_slice(b"nbt!");

        let image = RegionImage::from_bytes(image, -1, 0).unwrap();

        let mut seen = Vec::new();

        image
            .visit_chunks(|chunk| {
                seen.push((chunk.x, chunk.z, chunk.payload.to_vec()));
                true
            })
            .unwrap();

        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, -29);
        assert_eq!(seen[0].1, 5);
        assert_eq!(seen[0].2, [&[2u8] as &[_], b"nbt!"].concat());
    }

    #[test]
    fn visitor_can_stop_early() {
        let mut image = header_only();

        image.extend_from_slice(&[0; 8192]);

        image[0..4].copy_from_slice(&entry_of(2, 1));
        image[4..8].copy_from_slice(&entry_of(3, 1));

        for (sector, slot) in [(8192, 0), (12288, 1)] {
            image[sector..sector + 4].copy_from_slice(&2u32.to_be_bytes());
            image[sector + 4] = 3;
            image[sector + 5] = slot;
        }

        let image = RegionImage::from_bytes(image, 0, 0).unwrap();

        let mut count = 0;

        image
            .visit_chunks(|_| {
                count += 1;
                false
            })
            .unwrap();

        assert_eq!(count, 1);
    }

    #[test]
    fn rejects_damaged_images() {
        assert!(matches!(
            RegionImage::from_bytes(alloc::vec![0; 100], 0, 0,),
            Err(AnvilError::TruncatedFile { .. })
        ));

        assert!(matches!(
            RegionImage::from_bytes(alloc::vec![0; 8193], 0, 0,),
            Err(AnvilError::MisalignedFile { .. })
        ));

        let mut image = header_only();

        image.extend_from_slice(&[0; 4096]);
        image[0..4].copy_from_slice(&entry_of(9, 1));

        let image = RegionImage::from_bytes(image, 0, 0).unwrap();

        assert!(matches!(
            image.visit_chunks(|_| true),
            Err(AnvilError::CorruptEntry { .. })
        ));

        let mut image = header_only();

        image.extend_from_slice(&[0; 4096]);
        image[0..4].copy_from_slice(&entry_of(2, 0));

        let image = RegionImage::from_bytes(image, 0, 0).unwrap();

        assert!(matches!(
            image.visit_chunks(|_| true),
            Err(AnvilError::CorruptEntry { .. })
        ));

        let mut image = header_only();

        image.extend_from_slice(&[0; 4096]);
        image[0..4].copy_from_slice(&entry_of(2, 1));
        image[8192..8196].copy_from_slice(&5000u32.to_be_bytes());

        let image = RegionImage::from_bytes(image, 0, 0).unwrap();

        assert!(matches!(
            image.visit_chunks(|_| true),
            Err(AnvilError::CorruptChunk { .. })
        ));

        let mut image = header_only();

        image.extend_from_slice(&[0; 4096]);
        image[0..4].copy_from_slice(&entry_of(2, 1));
        image[8192..8196].copy_from_slice(&0u32.to_be_bytes());

        let image = RegionImage::from_bytes(image, 0, 0).unwrap();

        assert!(matches!(
            image.visit_chunks(|_| true),
            Err(AnvilError::CorruptChunk { .. })
        ));
    }

    #[test]
    fn rejects_absurd_regions() {
        assert!(matches!(
            RegionImage::from_bytes(header_only(), i32::MAX, 0,),
            Err(AnvilError::CoordinateOverflow { .. })
        ));
    }
}
