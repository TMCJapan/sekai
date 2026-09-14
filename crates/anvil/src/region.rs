//! Shared Anvil sector layout and coordinate mapping.

use alloc::borrow::ToOwned;

use crate::error::AnvilError;

pub const SECTOR_LEN: u64 = 4096;
pub const HEADER_LEN: u64 = 8192;
pub const TABLE_ENTRIES: u32 = 1024;
pub const ROW_WIDTH: u32 = 32;
pub const FIRST_DATA_SECTOR: u64 = 2;
pub const MAX_SECTORS_PER_CHUNK: u64 = 255;
pub const MAX_SECTOR_OFFSET: u64 = 0xFF_FFFF;

/// Parses `r.<x>.<z>.mca` into region coordinates.
pub fn parse_region_name(file_name: &str) -> Result<(i32, i32), AnvilError> {
    let bad = || AnvilError::BadFilename {
        name: file_name.to_owned(),
    };

    let stem = file_name.strip_suffix(".mca").ok_or_else(bad)?;
    let mut parts = stem.split('.');

    let tag = parts.next();
    let x = parts.next();
    let z = parts.next();

    if tag != Some("r") || parts.next().is_some() {
        return Err(bad());
    }

    let x = x.and_then(|value| value.parse().ok()).ok_or_else(bad)?;

    let z = z.and_then(|value| value.parse().ok()).ok_or_else(bad)?;

    Ok((x, z))
}

/// Region coordinates together with their global chunk origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionLoc {
    region_x: i32,
    region_z: i32,
    base_x: i32,
    base_z: i32,
}

impl RegionLoc {
    pub fn new(region_x: i32, region_z: i32) -> Result<Self, AnvilError> {
        let (base_x, base_z) = base_coords(region_x, region_z)?;

        Ok(Self {
            region_x,
            region_z,
            base_x,
            base_z,
        })
    }

    pub const fn region_x(self) -> i32 {
        self.region_x
    }

    pub const fn region_z(self) -> i32 {
        self.region_z
    }

    pub fn slot_of(self, x: i32, z: i32) -> Result<u32, AnvilError> {
        let wrong = || AnvilError::WrongRegion {
            region_x: self.region_x,
            region_z: self.region_z,
            x,
            z,
        };

        let dx = x.checked_sub(self.base_x).ok_or_else(wrong)?;
        let dz = z.checked_sub(self.base_z).ok_or_else(wrong)?;

        if !(0..ROW_WIDTH.cast_signed()).contains(&dx)
            || !(0..ROW_WIDTH.cast_signed()).contains(&dz)
        {
            return Err(wrong());
        }

        Ok(dx.cast_unsigned() + ROW_WIDTH * dz.cast_unsigned())
    }

    pub const fn coord_at(self, index: u32) -> (i32, i32) {
        (
            self.base_x + (index % ROW_WIDTH).cast_signed(),
            self.base_z + (index / ROW_WIDTH).cast_signed(),
        )
    }
}

/// Returns the global chunk-coordinate origin of a region.
pub fn base_coords(region_x: i32, region_z: i32) -> Result<(i32, i32), AnvilError> {
    fn base(region: i32) -> Option<i32> {
        let base = region.checked_mul(ROW_WIDTH.cast_signed())?;

        base.checked_add(ROW_WIDTH.cast_signed() - 1).map(|_| base)
    }

    let overflow = || AnvilError::CoordinateOverflow { region_x, region_z };

    Ok((
        base(region_x).ok_or_else(overflow)?,
        base(region_z).ok_or_else(overflow)?,
    ))
}

/// Validates an image length and returns its sector count.
pub const fn check_image_len(len: u64) -> Result<u64, AnvilError> {
    if len == 0 {
        return Ok(0);
    }

    if len < HEADER_LEN {
        return Err(AnvilError::TruncatedFile { len });
    }

    if !len.is_multiple_of(SECTOR_LEN) {
        return Err(AnvilError::MisalignedFile { len });
    }

    Ok(len / SECTOR_LEN)
}

/// Returns the number of sectors required for a stored payload.
pub const fn sectors_for(payload_len: usize) -> Result<u64, AnvilError> {
    let total = payload_len as u64 + 4;
    let sectors = total.div_ceil(SECTOR_LEN);

    if sectors > MAX_SECTORS_PER_CHUNK {
        return Err(AnvilError::ChunkTooLarge { len: payload_len });
    }

    Ok(sectors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_region_names() {
        assert_eq!(parse_region_name("r.0.0.mca"), Ok((0, 0)));
        assert_eq!(parse_region_name("r.-1.2.mca"), Ok((-1, 2)));
        assert!(parse_region_name("r.0.0.mcr").is_err());
        assert!(parse_region_name("r.0.mca").is_err());
        assert!(parse_region_name("r.a.b.mca").is_err());
        assert!(parse_region_name("x.0.0.mca").is_err());
        assert!(parse_region_name("r.0.0.1.mca").is_err());
        assert!(parse_region_name("r.99999999999.0.mca").is_err());
    }

    #[test]
    fn rejects_absurd_bases() {
        assert!(base_coords(0, 0).is_ok());
        assert!(base_coords(i32::MAX, 0).is_err());
        assert!(base_coords(67_108_863, 0).is_ok());
        assert!(base_coords(67_108_864, 0).is_err());
        assert!(base_coords(0, i32::MIN).is_err());
    }

    #[test]
    fn validates_image_lengths() {
        assert_eq!(check_image_len(0), Ok(0));

        assert!(matches!(
            check_image_len(1),
            Err(AnvilError::TruncatedFile { .. })
        ));

        assert!(matches!(
            check_image_len(8191),
            Err(AnvilError::TruncatedFile { .. })
        ));

        assert_eq!(check_image_len(8192), Ok(2));

        assert!(matches!(
            check_image_len(8193),
            Err(AnvilError::MisalignedFile { .. })
        ));

        assert_eq!(check_image_len(12288), Ok(3));
    }

    #[test]
    fn sizes_sectors_with_ceiling() {
        assert_eq!(sectors_for(0), Ok(1));
        assert_eq!(sectors_for(4092), Ok(1));
        assert_eq!(sectors_for(4093), Ok(2));

        assert!(matches!(
            sectors_for(255 * 4096),
            Err(AnvilError::ChunkTooLarge { .. })
        ));
    }

    #[test]
    fn region_loc_maps_slots_both_ways() {
        let loc = RegionLoc::new(-1, 2).unwrap();

        assert_eq!(loc.region_x(), -1);
        assert_eq!(loc.region_z(), 2);

        let slot = loc.slot_of(-29, 69).unwrap();

        assert_eq!(slot, 3 + 32 * 5);
        assert_eq!(loc.coord_at(slot), (-29, 69));
        assert!(loc.slot_of(0, 0).is_err());
    }
}
