//! Write-side construction of fresh `.mca` images.

use alloc::vec::Vec;

use crate::error::AnvilError;
use crate::region::{
    FIRST_DATA_SECTOR, HEADER_LEN, MAX_SECTOR_OFFSET, RegionLoc, SECTOR_LEN, sectors_for,
};

#[derive(Debug)]
struct StagedChunk {
    slot: u32,
    payload: Vec<u8>,
}

/// Builder for a complete region image.
#[derive(Debug)]
pub struct RegionBuilder {
    loc: RegionLoc,
    timestamp: u32,
    staged: Vec<StagedChunk>,
}

impl RegionBuilder {
    /// Creates an empty builder for one region.
    pub fn new(region_x: i32, region_z: i32, timestamp: u32) -> Result<Self, AnvilError> {
        Ok(Self {
            loc: RegionLoc::new(region_x, region_z)?,
            timestamp,
            staged: Vec::new(),
        })
    }

    pub const fn region_x(&self) -> i32 {
        self.loc.region_x()
    }

    pub const fn region_z(&self) -> i32 {
        self.loc.region_z()
    }

    /// Adds or replaces a stored chunk payload.
    pub fn stage_chunk(&mut self, x: i32, z: i32, payload: &[u8]) -> Result<(), AnvilError> {
        if payload.is_empty() {
            return Err(AnvilError::EmptyPayload);
        }

        sectors_for(payload.len())?;

        let slot = self.loc.slot_of(x, z)?;

        match self.staged.binary_search_by_key(&slot, |chunk| chunk.slot) {
            Ok(index) => {
                self.staged[index].payload = payload.to_vec();
            }

            Err(index) => {
                self.staged.insert(
                    index,
                    StagedChunk {
                        slot,
                        payload: payload.to_vec(),
                    },
                );
            }
        }

        Ok(())
    }

    /// Removes a previously staged chunk.
    pub fn remove_chunk(&mut self, x: i32, z: i32) -> Result<(), AnvilError> {
        let slot = self.loc.slot_of(x, z)?;

        if let Ok(index) = self.staged.binary_search_by_key(&slot, |chunk| chunk.slot) {
            self.staged.remove(index);
        }

        Ok(())
    }

    /// Assembles the complete `.mca` image.
    pub fn image(&self) -> Result<Vec<u8>, AnvilError> {
        let header_len =
            usize::try_from(HEADER_LEN).map_err(|_| AnvilError::ImageTooLarge { sectors: 0 })?;

        let sector_len =
            usize::try_from(SECTOR_LEN).map_err(|_| AnvilError::ImageTooLarge { sectors: 0 })?;

        let mut image = alloc::vec![0; header_len];

        let mut offset = FIRST_DATA_SECTOR;

        for chunk in &self.staged {
            let sectors = sectors_for(chunk.payload.len())?;

            if offset > MAX_SECTOR_OFFSET {
                return Err(AnvilError::ImageTooLarge { sectors: offset });
            }

            let offset_u32 =
                u32::try_from(offset).map_err(|_| AnvilError::ImageTooLarge { sectors: offset })?;

            let sectors_u32 = u32::try_from(sectors)
                .map_err(|_| AnvilError::ImageTooLarge { sectors: offset })?;

            let entry = (offset_u32 << 8) | sectors_u32;

            let entry_at = usize::try_from(chunk.slot)
                .map_err(|_| AnvilError::ImageTooLarge { sectors: offset })?
                * 4;

            image[entry_at..entry_at + 4].copy_from_slice(&entry.to_be_bytes());

            let timestamp_at = sector_len + entry_at;

            image[timestamp_at..timestamp_at + 4].copy_from_slice(&self.timestamp.to_be_bytes());

            let payload_len =
                u32::try_from(chunk.payload.len()).map_err(|_| AnvilError::ChunkTooLarge {
                    len: chunk.payload.len(),
                })?;

            image.extend_from_slice(&payload_len.to_be_bytes());
            image.extend_from_slice(&chunk.payload);

            let allocation = sectors * SECTOR_LEN;

            let stored_len =
                u64::try_from(chunk.payload.len()).map_err(|_| AnvilError::ChunkTooLarge {
                    len: chunk.payload.len(),
                })? + 4;

            let padding = usize::try_from(allocation - stored_len)
                .map_err(|_| AnvilError::ImageTooLarge { sectors: offset })?;

            image.extend(core::iter::repeat_n(0, padding));

            offset += sectors;
        }

        Ok(image)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::entry_of;

    fn builder() -> RegionBuilder {
        RegionBuilder::new(0, 0, 0x1234_5678).unwrap()
    }

    #[test]
    fn stages_and_forgets() {
        let mut builder = builder();

        builder.stage_chunk(1, 2, &[2, 9, 9]).unwrap();

        builder.remove_chunk(1, 2).unwrap();

        builder.remove_chunk(1, 2).unwrap();

        assert_eq!(builder.image().unwrap().len(), 8192);
    }

    #[test]
    fn stages_replace_existing_payload() {
        let mut builder = builder();

        builder.stage_chunk(0, 0, &[2, 1]).unwrap();

        builder.stage_chunk(0, 0, &[2, 2, 3]).unwrap();

        let image = builder.image().unwrap();

        assert_eq!(&image[8192..8196], &3u32.to_be_bytes());
        assert_eq!(&image[8196..8199], &[2, 2, 3]);
    }

    #[test]
    fn rejects_foreign_empty_and_huge() {
        let mut builder = builder();

        assert!(matches!(
            builder.stage_chunk(32, 0, &[1]),
            Err(AnvilError::WrongRegion { .. })
        ));

        assert!(matches!(
            builder.stage_chunk(-1, 0, &[1]),
            Err(AnvilError::WrongRegion { .. })
        ));

        assert!(matches!(
            builder.stage_chunk(0, 0, &[]),
            Err(AnvilError::EmptyPayload)
        ));

        assert!(matches!(
            builder.stage_chunk(0, 0, &alloc::vec![0; 255 * 4096]),
            Err(AnvilError::ChunkTooLarge { .. })
        ));

        assert!(matches!(
            RegionBuilder::new(i32::MAX, 0, 0),
            Err(AnvilError::CoordinateOverflow { .. })
        ));
    }

    #[test]
    fn keeps_chunks_in_slot_order() {
        let mut builder = builder();

        builder.stage_chunk(1, 1, &[3, 1]).unwrap();

        builder.stage_chunk(0, 0, &[3, 0]).unwrap();

        let image = builder.image().unwrap();

        assert_eq!(&image[0..4], &entry_of(2, 1));

        assert_eq!(&image[132..136], &entry_of(3, 1));
    }

    #[test]
    fn packs_image_with_header_and_padding() {
        let mut builder = builder();

        builder.stage_chunk(0, 0, &[2, 7]).unwrap();

        let big = alloc::vec![5u8; 4093];

        builder.stage_chunk(1, 1, &big).unwrap();

        let image = builder.image().unwrap();

        assert_eq!(&image[0..4], &entry_of(2, 1));

        assert_eq!(&image[33 * 4..33 * 4 + 4], &entry_of(3, 2));

        assert_eq!(&image[4096..4100], &0x1234_5678u32.to_be_bytes());

        assert_eq!(
            &image[4096 + 33 * 4..4096 + 33 * 4 + 4],
            &0x1234_5678u32.to_be_bytes()
        );

        assert_eq!(&image[4096 + 4..4096 + 8], &[0; 4]);

        assert_eq!(&image[8192..8196], &2u32.to_be_bytes());

        assert_eq!(&image[8196..8198], &[2, 7]);

        let second = 8192 + 4096;

        assert_eq!(&image[second..second + 4], &4093u32.to_be_bytes());

        assert!(image.len().is_multiple_of(4096));

        assert_eq!(image.len(), 8192 + 4096 + 8192);
    }
}
