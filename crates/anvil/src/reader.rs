//! Read-side parsing of `.mca` region images.

use alloc::vec::Vec;

use sekai_util::{ChunkHash, RegionKind};

use crate::error::{AnvilError, ChunkHashError};
use crate::region::{FIRST_DATA_SECTOR, RegionLoc, SECTOR_LEN, TABLE_ENTRIES};

/// One present chunk and its exact stored payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk<'a> {
    /// Global chunk X coordinate.
    pub x: i32,
    /// Global chunk Z coordinate.
    pub z: i32,
    /// Region family the chunk was read from.
    pub kind: RegionKind,
    /// Stored compression byte followed by the compressed body.
    pub payload: &'a [u8],
}

impl<'a> Chunk<'a> {
    /// Constructs a chunk from a stored payload.
    ///
    /// A chunk read through [`RegionImage`] already carries the kind of its
    /// region; this constructor is for callers holding a payload directly,
    /// such as a CAS blob.
    pub const fn new(kind: RegionKind, x: i32, z: i32, payload: &'a [u8]) -> Self {
        Self {
            x,
            z,
            kind,
            payload,
        }
    }

    /// Hashes the chunk through its normalized view.
    ///
    /// A `region/` payload hashes its decompressed level-chunk NBT with the
    /// volatile tags excluded. `entities`/`poi` payloads have no level-chunk
    /// schema, so they hash their exact decompressed NBT bytes after
    /// validating that the payload is well-formed NBT. A payload that cannot
    /// be decompressed or parsed has no key and is rejected; there is no
    /// raw-byte fallback.
    ///
    /// # Errors
    ///
    /// [`ChunkHashError::Decompress`] when the stored payload cannot be
    /// decompressed, and [`ChunkHashError::Invalid`] when it is not NBT a
    /// chunk of this kind can be read from.
    pub fn normalized_hash(&self) -> Result<ChunkHash, ChunkHashError> {
        let mut raw_nbt = Vec::new();
        crate::codec::decompress_into(self.payload, &mut raw_nbt)?;
        crate::nbt::chunk_key(self.kind, &raw_nbt)
    }
}

/// Parsed `.mca` image.
///
/// The image owns its backing bytes and exposes chunks only as borrows.
#[derive(Debug)]
pub struct RegionImage {
    loc: RegionLoc,
    kind: RegionKind,
    bytes: Vec<u8>,
}

impl RegionImage {
    /// Parses an in-memory region image.
    ///
    /// The image is assumed to hold `region/` chunks; use
    /// [`Self::from_bytes_with_kind`] for `entities` or `poi` files.
    pub fn from_bytes(bytes: Vec<u8>, region_x: i32, region_z: i32) -> Result<Self, AnvilError> {
        Self::from_bytes_with_kind(bytes, region_x, region_z, RegionKind::Region)
    }

    /// Parses an in-memory region image from the given region family.
    pub fn from_bytes_with_kind(
        bytes: Vec<u8>,
        region_x: i32,
        region_z: i32,
        kind: RegionKind,
    ) -> Result<Self, AnvilError> {
        crate::region::check_image_len(bytes.len() as u64)?;

        Ok(Self {
            loc: RegionLoc::new(region_x, region_z)?,
            kind,
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

    /// Iterates the present chunks in table order.
    ///
    /// Each item is `Result<Chunk, AnvilError>`: a corrupt entry surfaces as
    /// an `Err`. Drive the walk with `?`, e.g.:
    ///
    /// ```text
    /// for item in &image {
    ///     let chunk = item?;
    ///     // use chunk ...
    /// }
    /// ```
    ///
    /// Rust requires this inherent method next to the [`IntoIterator`] impl.
    pub fn iter(&self) -> RegionImageIter<'_> {
        self.into_iter()
    }

    /// Visits all present chunks in table order.
    ///
    /// Returning `false` stops iteration.
    ///
    /// Unlike [`Self::iter`], which yields `Result` items, this visitor stops
    /// on the first `false` and aborts on the first corrupt entry.
    pub fn visit_chunks<F>(&self, mut visit: F) -> Result<(), AnvilError>
    where
        F: FnMut(Chunk<'_>) -> bool,
    {
        let iter = self.iter();
        for item in iter {
            // The error is terminal: stop at the first corrupt entry instead
            // of walking the rest of the table.
            if !visit(item?) {
                break;
            }
        }

        Ok(())
    }

    /// Returns the stored payload of the chunk in table slot `index`.
    ///
    /// An empty slot yields `Ok(None)`; a corrupt one yields the error the
    /// location-table walk reports.
    fn chunk_at(&self, index: u32) -> Result<Option<Chunk<'_>>, AnvilError> {
        // A zero-length image is the empty region, not a corrupt table; keep
        // this guard so the helper stays correct if reused outside the
        // iterator.
        if self.bytes.is_empty() {
            return Ok(None);
        }

        let (offset, count) = self.entry(index)?;

        if offset == 0 && count == 0 {
            return Ok(None);
        }

        if offset < FIRST_DATA_SECTOR || count == 0 {
            return Err(Self::corrupt_entry(index, offset, count));
        }

        // The run must at least start inside the file. Trailing partial
        // sectors are tolerated (vanilla opens such files); exact byte
        // bounds are validated when slicing the payload below.
        let start = offset
            .checked_mul(SECTOR_LEN)
            .ok_or_else(|| Self::corrupt_entry(index, offset, count))?;

        if start >= self.bytes.len() as u64 {
            return Err(Self::corrupt_entry(index, offset, count));
        }

        let payload = self.sector_payload(index, offset, count)?;

        let (x, z) = self.loc.coord_at(index);

        Ok(Some(Chunk::new(self.kind, x, z, payload)))
    }

    /// Returns the stored raw payload for a single chunk at global coordinates `(x, z)`.
    ///
    /// Returns `Ok(None)` if the chunk coordinate is outside this region or if the
    /// chunk has not been generated (is empty).
    pub fn chunk_payload(&self, x: i32, z: i32) -> Result<Option<&[u8]>, AnvilError> {
        // A zero-length image is the empty region the iterator reports, not
        // a corrupt location table.
        if self.bytes.is_empty() {
            return Ok(None);
        }

        let Ok(index) = self.loc.slot_of(x, z) else {
            return Ok(None);
        };

        let (offset, count) = self.entry(index)?;
        if offset == 0 && count == 0 {
            return Ok(None);
        }

        let file_len = self.bytes.len() as u64;
        let start = offset
            .checked_mul(SECTOR_LEN)
            .ok_or_else(|| Self::corrupt_entry(index, offset, count))?;

        if offset < FIRST_DATA_SECTOR || count == 0 || start >= file_len {
            return Err(Self::corrupt_entry(index, offset, count));
        }

        let payload = self.sector_payload(index, offset, count)?;
        Ok(Some(payload))
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

/// Yields the present chunks of a [`RegionImage`] in table order.
///
/// Yields `Result<Chunk, AnvilError>`: a corrupt location-table entry ends
/// the iteration by yielding its error, then the iterator is fused and
/// yields `None` from then on.
///
/// [`core::iter::FusedIterator`] holds because `next` never consults the
/// image again once the table is walked or an error surfaced.
#[derive(Debug, Clone)]
pub struct RegionImageIter<'this> {
    inner: &'this RegionImage,
    next_index: u32,
    done: bool,
}

impl<'this> Iterator for RegionImageIter<'this> {
    type Item = Result<Chunk<'this>, AnvilError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }

        // A zero-length image is the empty region, not a truncated table.
        if self.inner.bytes.is_empty() {
            self.done = true;
            return None;
        }

        while self.next_index < TABLE_ENTRIES {
            let index = self.next_index;
            self.next_index += 1;

            match self.inner.chunk_at(index) {
                Ok(None) => {}
                Ok(Some(chunk)) => return Some(Ok(chunk)),
                Err(err) => {
                    self.done = true;
                    return Some(Err(err));
                }
            }
        }

        self.done = true;
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = if self.done || self.inner.bytes.is_empty() {
            0
        } else {
            usize::try_from(TABLE_ENTRIES.saturating_sub(self.next_index)).unwrap_or(usize::MAX)
        };
        (0, Some(remaining))
    }
}

impl core::iter::FusedIterator for RegionImageIter<'_> {}

impl<'this> IntoIterator for &'this RegionImage {
    type IntoIter = RegionImageIter<'this>;
    type Item = Result<Chunk<'this>, AnvilError>;

    fn into_iter(self) -> Self::IntoIter {
        RegionImageIter {
            inner: self,
            next_index: 0,
            done: false,
        }
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

        assert_eq!(image.image(), &[] as &[u8]);

        let mut count = 0;

        image
            .visit_chunks(|_| {
                count += 1;
                true
            })
            .unwrap();

        assert_eq!(count, 0);

        // The single-chunk lookup agrees: empty region, no corrupt entry.
        assert_eq!(image.chunk_payload(0, 0).unwrap(), None);
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

    /// Two present chunks in slots 0 and 1, payloads `a` and `b`.
    fn two_chunk_image() -> Vec<u8> {
        let mut image = header_only();

        image.extend_from_slice(&[0; 8192]);

        image[0..4].copy_from_slice(&entry_of(2, 1));
        image[4..8].copy_from_slice(&entry_of(3, 1));

        for (sector, payload) in [(8192, b"a"), (12288, b"b")] {
            image[sector..sector + 4].copy_from_slice(&2u32.to_be_bytes());
            image[sector + 4] = 3;
            image[sector + 5] = payload[0];
        }

        image
    }

    #[test]
    fn iterator_yields_chunks_in_table_order() {
        let image = RegionImage::from_bytes(two_chunk_image(), 0, 0).unwrap();

        let seen: Vec<_> = image
            .into_iter()
            .map(|chunk| {
                let chunk = chunk.unwrap();
                (chunk.x, chunk.z, chunk.payload[1])
            })
            .collect();

        assert_eq!(seen, [(0, 0, b'a'), (1, 0, b'b')]);
    }

    #[test]
    fn iterator_matches_visitor_for_empty_images() {
        for bytes in [header_only(), Vec::new()] {
            let image = RegionImage::from_bytes(bytes, 0, 0).unwrap();

            assert_eq!((&image).into_iter().count(), 0);
        }
    }

    #[test]
    fn iterator_is_fused_after_error() {
        let mut image = header_only();

        image.extend_from_slice(&[0; 4096]);
        image[0..4].copy_from_slice(&entry_of(2, 1));
        image[8192..8196].copy_from_slice(&5000u32.to_be_bytes());

        let image = RegionImage::from_bytes(image, 0, 0).unwrap();

        let mut iter = (&image).into_iter();

        assert!(matches!(
            iter.next(),
            Some(Err(AnvilError::CorruptChunk { .. }))
        ));

        // The error is terminal, not skipped over.
        assert!(iter.next().is_none());
        assert!(iter.next().is_none());
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

        // A trailing partial sector is tolerated when unreferenced.
        let image = RegionImage::from_bytes(alloc::vec![0; 8193], 0, 0).unwrap();

        let mut count = 0;

        image
            .visit_chunks(|_| {
                count += 1;
                true
            })
            .unwrap();

        assert_eq!(count, 0);

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

    #[test]
    fn tolerates_trailing_partial_sector_with_intact_chunks() {
        // Mirrors real files with a torn tail (e.g. 1034 sectors + 188
        // bytes): the referenced chunk reads fine, the tail is ignored.
        let mut image = header_only();

        image.extend_from_slice(&[0; 4096]);
        image.extend_from_slice(&[0xAB; 188]);
        image[0..4].copy_from_slice(&entry_of(2, 1));

        let sector = 8192;

        image[sector..sector + 4].copy_from_slice(&5u32.to_be_bytes());
        image[sector + 4] = 2;
        image[sector + 5..sector + 9].copy_from_slice(b"nbt!");

        assert_eq!(image.len() % 4096, 188);

        let image = RegionImage::from_bytes(image, 0, 0).unwrap();

        let mut seen = Vec::new();

        image
            .visit_chunks(|chunk| {
                seen.push(chunk.payload.to_vec());
                true
            })
            .unwrap();

        assert_eq!(seen, alloc::vec![[&[2u8] as &[_], b"nbt!"].concat()]);
    }

    #[test]
    fn chunk_normalized_hash_uses_the_region_shape() {
        use nanonbt::Cesu8;

        let bytes = |inhabited_time| {
            let chunk = crate::nbt::chunk::ChunkPayload {
                data_version: 1,
                x_pos: 0,
                y_pos: 0,
                z_pos: 0,
                inhabited_time,
                last_update: 0,
                sections: Vec::new(),
                heightmaps: crate::nbt::chunk::Heightmaps::default(),
                status: &Cesu8::from_str("minecraft:full"),
                block_entities: Vec::new(),
                block_ticks: Vec::new(),
                fluid_ticks: Vec::new(),
                blending_data: crate::nbt::chunk::RawCompound::default(),
                post_processing: Vec::new(),
                structures: crate::nbt::chunk::RawCompound::default(),
                is_light_on: false,
            };
            let nbt = nanonbt::to_bytes(&chunk).unwrap();
            let mut payload = alloc::vec![3];
            payload.extend(nbt);
            payload
        };

        let base_bytes = bytes(0);
        let varied_bytes = bytes(99);
        let base = Chunk::new(RegionKind::Region, 0, 0, &base_bytes);
        let varied = Chunk::new(RegionKind::Region, 0, 0, &varied_bytes);
        assert_eq!(
            base.normalized_hash().unwrap(),
            varied.normalized_hash().unwrap()
        );
    }

    #[test]
    fn chunk_normalized_hash_keys_sparse_kinds_by_their_nbt_bytes() {
        let nbt = [10u8, 0, 0, 0]; // Empty root compound.
        let payload = [&[3u8] as &[_], &nbt].concat();
        let chunk = Chunk::new(RegionKind::Entities, 0, 0, &payload);

        assert_eq!(
            chunk.normalized_hash().unwrap(),
            ChunkHash::new(sekai_util::hash_bytes(&nbt))
        );
        assert!(matches!(
            Chunk::new(RegionKind::Region, 0, 0, &payload).normalized_hash(),
            Err(ChunkHashError::Invalid(_))
        ));
    }

    #[test]
    fn region_image_carries_its_kind_to_every_chunk() {
        let mut image = header_only();

        image.extend_from_slice(&[0; 4096]);
        image[0..4].copy_from_slice(&entry_of(2, 1));
        image[8192..8196].copy_from_slice(&2u32.to_be_bytes());
        image[8196] = 3;
        image[8197] = 0;

        let image = RegionImage::from_bytes_with_kind(image, 0, 0, RegionKind::Poi).unwrap();
        let mut kinds = Vec::new();
        image
            .visit_chunks(|chunk| {
                kinds.push(chunk.kind);
                true
            })
            .unwrap();

        assert_eq!(kinds, [RegionKind::Poi]);
    }

    #[test]
    fn rejects_runs_starting_past_end_of_file() {
        // Entry points at a sector whose start lies beyond the image,
        // partial tail or not.
        let mut image = header_only();

        image.extend_from_slice(&[0; 4096]);
        image.extend_from_slice(&[0xAB; 100]);
        image[0..4].copy_from_slice(&entry_of(4, 1));

        let image = RegionImage::from_bytes(image, 0, 0).unwrap();

        assert!(matches!(
            image.visit_chunks(|_| true),
            Err(AnvilError::CorruptEntry { .. })
        ));

        assert!(matches!(
            image.chunk_payload(0, 0),
            Err(AnvilError::CorruptEntry { .. })
        ));
    }
}
