//! Region family codes and their on-disk directory names.

use core::{fmt, str::FromStr};

/// Region family identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegionKind {
    Region,
    Entities,
    Poi,
}

impl RegionKind {
    pub const fn new(id: i32) -> Result<Self, UnknownRegionKind> {
        match id {
            0 => Ok(Self::Region),
            1 => Ok(Self::Entities),
            2 => Ok(Self::Poi),
            _ => Err(UnknownRegionKind),
        }
    }

    pub const fn raw(self) -> u8 {
        match self {
            Self::Region => 0,
            Self::Entities => 1,
            Self::Poi => 2,
        }
    }
}

/// Error returned when a code or name does not map to a known region kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnknownRegionKind;

impl fmt::Display for UnknownRegionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown region kind: expected region, entities, or poi")
    }
}

impl core::error::Error for UnknownRegionKind {}

impl FromStr for RegionKind {
    type Err = UnknownRegionKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "region" | "0" => Ok(Self::Region),
            "entities" | "1" => Ok(Self::Entities),
            "poi" | "2" => Ok(Self::Poi),
            _ => Err(UnknownRegionKind),
        }
    }
}

impl fmt::Display for RegionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Region => write!(f, "region"),
            Self::Entities => write!(f, "entities"),
            Self::Poi => write!(f, "poi"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use core::str::FromStr;

    #[test]
    fn parses_region_kind_names() {
        assert_eq!("region".parse(), Ok(RegionKind::Region));
        assert_eq!("REGION".parse(), Ok(RegionKind::Region));
        assert_eq!("entities".parse(), Ok(RegionKind::Entities));
        assert_eq!("poi".parse(), Ok(RegionKind::Poi));
        assert_eq!("POI".parse(), Ok(RegionKind::Poi));
        assert_eq!("0".parse(), Ok(RegionKind::Region));
        assert_eq!("1".parse(), Ok(RegionKind::Entities));
        assert_eq!("2".parse(), Ok(RegionKind::Poi));
        assert_eq!("9".parse::<RegionKind>(), Err(UnknownRegionKind));
        assert_eq!(RegionKind::from_str("blocks"), Err(UnknownRegionKind));
        assert_eq!(RegionKind::from_str(""), Err(UnknownRegionKind));
    }

    #[test]
    fn displays_region_kind_names() {
        assert_eq!(RegionKind::Region.to_string(), "region");
        assert_eq!(RegionKind::Entities.to_string(), "entities");
        assert_eq!(RegionKind::Poi.to_string(), "poi");
        assert_eq!(RegionKind::new(9), Err(UnknownRegionKind));
        for kind in [RegionKind::Region, RegionKind::Entities, RegionKind::Poi] {
            assert_eq!(kind.to_string().parse(), Ok(kind));
        }
    }

    #[test]
    fn raw_codes_round_trip() {
        assert_eq!(RegionKind::Region.raw(), 0);
        assert_eq!(RegionKind::Entities.raw(), 1);
        assert_eq!(RegionKind::Poi.raw(), 2);
        assert_eq!(RegionKind::new(0).unwrap().raw(), 0);
    }
}
