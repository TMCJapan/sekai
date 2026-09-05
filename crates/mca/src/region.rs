//! Shared sector-layout constants and validation.
//!
//! Rationale: reader and writer must agree on every layout number (sector
//! size, header length, entry decoding, offset range). A single module owns
//! them so the two sides cannot drift apart.

use sekai_core::{ChunkCoord, Dimension, RegionKind};

use crate::error::McaError;

/// Bytes per sector; files are whole multiples of this.
pub(crate) const SECTOR_LEN: u64 = 4096;
/// Header sectors (locations + timestamps).
pub(crate) const HEADER_SECTORS: u64 = 2;
/// Header bytes; files shorter than this cannot be parsed.
pub(crate) const HEADER_LEN: u64 = HEADER_SECTORS * SECTOR_LEN;
/// Location/timestamp slots per file (32 x 32 chunks).
pub(crate) const TABLE_ENTRIES: u32 = 1024;
/// Sectors per table row (region files are 32 chunks wide).
pub(crate) const ROW_WIDTH: u32 = 32;
/// First data sector (immediately after the header).
pub(crate) const FIRST_DATA_SECTOR: u64 = HEADER_SECTORS;
/// Addressable sectors per chunk (location count field is one byte).
pub(crate) const MAX_SECTORS_PER_CHUNK: u64 = 255;
/// Location offset field is 24 bits wide.
pub(crate) const MAX_SECTOR_OFFSET: u64 = 0xFF_FFFF;

/// Parse `r.<x>.<z>.mca` file names into region coordinates.
///
/// Shared by `engine` discovery so the naming rule lives in exactly one
/// place: whatever this accepts, both reader and writer accept.
pub fn parse_region_name(file_name: &str) -> Result<(i32, i32), McaError> {
    let bad = || McaError::BadFilename {
        name: file_name.to_string(),
    };
    let stem = file_name.strip_suffix(".mca").ok_or_else(bad)?;
    let mut parts = stem.split('.');
    let (tag, xs, zs) = (parts.next(), parts.next(), parts.next());
    if tag != Some("r") || parts.next().is_some() {
        return Err(bad());
    }
    let parse = |s: Option<&str>| s.and_then(|v| v.parse::<i32>().ok()).ok_or_else(bad);
    Ok((parse(xs)?, parse(zs)?))
}

/// Region identity plus its global chunk-column origin.
///
/// Rationale: reader and writer both tracked `region_x`/`region_z` and the
/// derived `base_x`/`base_z` as four loose `i32`s. Grouping them keeps the
/// two sides from drifting apart (single overflow-checked constructor) and
/// gives coordinate mapping one home. Kept `pub(crate)` inside `mca` on
/// purpose: region-file addressing is an MCA layout detail, not workspace
/// domain state, so it does not belong in `core::coords`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RegionLoc {
    region_x: i32,
    region_z: i32,
    base_x: i32,
    base_z: i32,
}

impl RegionLoc {
    /// Build from region coordinates, rejecting bases where `base + 31`
    /// would overflow `i32` (see [`base_coords`]).
    pub(crate) fn new(region_x: i32, region_z: i32) -> Result<Self, McaError> {
        let (base_x, base_z) = base_coords(region_x, region_z)?;
        Ok(Self {
            region_x,
            region_z,
            base_x,
            base_z,
        })
    }

    /// Region X from the file name.
    pub(crate) fn region_x(self) -> i32 {
        self.region_x
    }

    /// Region Z from the file name.
    pub(crate) fn region_z(self) -> i32 {
        self.region_z
    }

    /// Header slot for `coord`, rejecting foreign namespaces/coordinates.
    pub(crate) fn slot_of(
        self,
        dim: Dimension,
        kind: RegionKind,
        coord: &ChunkCoord,
    ) -> Result<u32, McaError> {
        let wrong = || McaError::WrongRegion {
            region_x: self.region_x,
            region_z: self.region_z,
            x: coord.x,
            z: coord.z,
        };
        if coord.dim != dim || coord.kind != kind {
            return Err(wrong());
        }
        let dx = coord.x.checked_sub(self.base_x).ok_or_else(wrong)?;
        let dz = coord.z.checked_sub(self.base_z).ok_or_else(wrong)?;
        if !(0..ROW_WIDTH as i32).contains(&dx) || !(0..ROW_WIDTH as i32).contains(&dz) {
            return Err(wrong());
        }
        Ok(dx as u32 + ROW_WIDTH * dz as u32)
    }

    /// Global coordinate for header slot `index` (`0..1024`).
    ///
    /// `base + 31` was validated at construction, so these additions cannot
    /// wrap; callers must still only pass in-range slots.
    pub(crate) fn coord_at(self, dim: Dimension, kind: RegionKind, index: u32) -> ChunkCoord {
        ChunkCoord::new(
            dim,
            kind,
            self.base_x + (index % ROW_WIDTH) as i32,
            self.base_z + (index / ROW_WIDTH) as i32,
        )
    }
}

/// Global chunk-column base (`region * 32`) with overflow rejection.
///
/// Also rejects bases where `base + 31` would overflow, so per-chunk
/// address math later cannot wrap.
pub(crate) fn base_coords(region_x: i32, region_z: i32) -> Result<(i32, i32), McaError> {
    let overflow = || McaError::CoordinateOverflow { region_x, region_z };
    let base = |r: i32| {
        r.checked_mul(ROW_WIDTH as i32)
            .and_then(|b| b.checked_add(ROW_WIDTH as i32 - 1).map(|_| b))
            .ok_or_else(overflow)
    };
    Ok((base(region_x)?, base(region_z)?))
}

/// Validate total image length; returns the sector count.
pub(crate) fn check_image_len(len: u64) -> Result<u64, McaError> {
    if len < HEADER_LEN {
        return Err(McaError::TruncatedFile { len });
    }
    if !len.is_multiple_of(SECTOR_LEN) {
        return Err(McaError::MisalignedFile { len });
    }
    Ok(len / SECTOR_LEN)
}

/// Sectors needed to store `payload_len` bytes plus the 4-byte prefix.
pub(crate) fn sectors_for(payload_len: usize) -> Result<u64, McaError> {
    let total = payload_len as u64 + 4;
    let sectors = total.div_ceil(SECTOR_LEN);
    if sectors > MAX_SECTORS_PER_CHUNK {
        return Err(McaError::ChunkTooLarge { len: payload_len });
    }
    Ok(sectors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_region_names() {
        assert_eq!(parse_region_name("r.0.0.mca").expect("must parse"), (0, 0));
        assert_eq!(
            parse_region_name("r.-1.2.mca").expect("must parse"),
            (-1, 2)
        );
        assert!(parse_region_name("r.0.0.mcr").is_err());
        assert!(matches!(
            parse_region_name("r.0.mca"),
            Err(McaError::BadFilename { .. })
        ));
        assert!(matches!(
            parse_region_name("r.a.b.mca"),
            Err(McaError::BadFilename { .. })
        ));
        assert!(matches!(
            parse_region_name("x.0.0.mca"),
            Err(McaError::BadFilename { .. })
        ));
        assert!(matches!(
            parse_region_name("r.0.0.1.mca"),
            Err(McaError::BadFilename { .. })
        ));
        // Out-of-`i32` values are bad names, not coordinates.
        assert!(matches!(
            parse_region_name("r.99999999999.0.mca"),
            Err(McaError::BadFilename { .. })
        ));
    }

    #[test]
    fn rejects_absurd_bases() {
        assert!(base_coords(0, 0).is_ok());
        assert!(matches!(
            base_coords(i32::MAX, 0),
            Err(McaError::CoordinateOverflow { .. })
        ));
        // Base itself fits but base + 31 would wrap: 67_108_863 is the
        // largest valid region (base + 31 == i32::MAX exactly).
        assert!(base_coords(67_108_863, 0).is_ok());
        assert!(matches!(
            base_coords(67_108_864, 0),
            Err(McaError::CoordinateOverflow { .. })
        ));
        assert!(matches!(
            base_coords(0, i32::MIN),
            Err(McaError::CoordinateOverflow { .. })
        ));
    }

    #[test]
    fn validates_image_lengths() {
        assert!(matches!(
            check_image_len(0),
            Err(McaError::TruncatedFile { .. })
        ));
        assert!(matches!(
            check_image_len(8191),
            Err(McaError::TruncatedFile { .. })
        ));
        assert_eq!(check_image_len(8192).expect("must validate"), 2);
        assert!(matches!(
            check_image_len(8193),
            Err(McaError::MisalignedFile { .. })
        ));
        assert_eq!(check_image_len(12288).expect("must validate"), 3);
    }

    #[test]
    fn sizes_sectors_with_ceiling() {
        assert_eq!(sectors_for(0).expect("must size"), 1);
        assert_eq!(sectors_for(4092).expect("must size"), 1);
        assert_eq!(sectors_for(4093).expect("must size"), 2);
        assert!(matches!(
            sectors_for(255 * 4096),
            Err(McaError::ChunkTooLarge { .. })
        ));
    }

    #[test]
    fn region_loc_maps_slots_both_ways() {
        use sekai_core::{Dimension, RegionKind};
        let loc = RegionLoc::new(-1, 2).expect("must build");
        assert_eq!(loc.region_x(), -1);
        assert_eq!(loc.region_z(), 2);
        // Local (3, 5) in r.-1.2 -> global (-29, 69), slot 3 + 32 * 5.
        let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, -29, 69);
        let slot = loc
            .slot_of(Dimension::OVERWORLD, RegionKind::REGION, &coord)
            .expect("must map");
        assert_eq!(slot, 3 + 32 * 5);
        assert_eq!(
            loc.coord_at(Dimension::OVERWORLD, RegionKind::REGION, slot),
            coord
        );
        // Foreign namespace or out-of-region coordinates are rejected.
        assert!(
            loc.slot_of(Dimension::NETHER, RegionKind::REGION, &coord)
                .is_err()
        );
        let far = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, 0, 0);
        assert!(
            loc.slot_of(Dimension::OVERWORLD, RegionKind::REGION, &far)
                .is_err()
        );
    }
}
