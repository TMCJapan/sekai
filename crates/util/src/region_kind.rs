use core::{fmt, str::FromStr};

use crate::error::ParseCodeError;

/// Region family identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegionKind(i32);

impl RegionKind {
    /// Block data (`region/`).
    pub const REGION: Self = Self(0);
    /// Entity data (`entities/`).
    pub const ENTITIES: Self = Self(1);
    /// Points of interest (`poi/`).
    pub const POI: Self = Self(2);

    /// Wrap a raw code.
    pub const fn new(raw: i32) -> Self {
        Self(raw)
    }

    /// Raw code (for binary columns and directory names).
    pub const fn raw(self) -> i32 {
        self.0
    }
}

impl FromStr for RegionKind {
    type Err = ParseCodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "region" | "0" => Ok(Self::REGION),
            "entities" | "1" => Ok(Self::ENTITIES),
            "poi" | "2" => Ok(Self::POI),
            other => other
                .parse::<i32>()
                .map(Self::new)
                .map_err(|_| ParseCodeError(())),
        }
    }
}

impl fmt::Display for RegionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::REGION => write!(f, "region"),
            Self::ENTITIES => write!(f, "entities"),
            Self::POI => write!(f, "poi"),
            other => write!(f, "{}", other.0),
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
        assert_eq!("region".parse(), Ok(RegionKind::REGION));
        assert_eq!("REGION".parse(), Ok(RegionKind::REGION));
        assert_eq!("entities".parse(), Ok(RegionKind::ENTITIES));
        assert_eq!("poi".parse(), Ok(RegionKind::POI));
        assert_eq!("POI".parse(), Ok(RegionKind::POI));
        assert_eq!("0".parse(), Ok(RegionKind::REGION));
        assert_eq!("1".parse(), Ok(RegionKind::ENTITIES));
        assert_eq!("2".parse(), Ok(RegionKind::POI));
        assert_eq!("9".parse(), Ok(RegionKind::new(9)));
        assert_eq!(RegionKind::from_str("blocks"), Err(ParseCodeError(())));
        assert_eq!(RegionKind::from_str(""), Err(ParseCodeError(())));
    }

    #[test]
    fn displays_region_kind_names() {
        assert_eq!(RegionKind::REGION.to_string(), "region");
        assert_eq!(RegionKind::ENTITIES.to_string(), "entities");
        assert_eq!(RegionKind::POI.to_string(), "poi");
        assert_eq!(RegionKind::new(9).to_string(), "9");
        for kind in [RegionKind::REGION, RegionKind::ENTITIES, RegionKind::POI] {
            assert_eq!(kind.to_string().parse(), Ok(kind));
        }
    }

    #[test]
    fn raw_codes_round_trip() {
        assert_eq!(RegionKind::REGION.raw(), 0);
        assert_eq!(RegionKind::ENTITIES.raw(), 1);
        assert_eq!(RegionKind::POI.raw(), 2);
        assert_eq!(RegionKind::new(9).raw(), 9);
    }
}
