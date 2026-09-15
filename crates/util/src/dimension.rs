use crate::error::ParseCodeError;
use core::fmt;
use core::str::FromStr;

/// Dimension namespace code. `0..=2` are reserved for vanilla dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dimension(i32);

impl Dimension {
    /// Vanilla overworld.
    pub const OVERWORLD: Self = Self(0);
    /// Vanilla nether.
    pub const NETHER: Self = Self(1);
    /// Vanilla end.
    pub const END: Self = Self(2);

    /// Wrap a raw code.
    pub const fn new(raw: i32) -> Self {
        Self(raw)
    }

    /// Raw code (for binary columns and file names).
    pub const fn raw(self) -> i32 {
        self.0
    }
}

impl FromStr for Dimension {
    type Err = ParseCodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "ow" | "overworld" | "0" => Ok(Self::OVERWORLD),
            "ne" | "nether" | "the_nether" | "dim-1" | "1" => Ok(Self::NETHER),
            "end" | "the_end" | "dim1" | "2" => Ok(Self::END),
            other => other
                .parse::<i32>()
                .map(Self::new)
                .map_err(|_| ParseCodeError(())),
        }
    }
}

impl fmt::Display for Dimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::OVERWORLD => write!(f, "overworld"),
            Self::NETHER => write!(f, "nether"),
            Self::END => write!(f, "end"),
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
    fn parses_dimension_names() {
        assert_eq!("overworld".parse(), Ok(Dimension::OVERWORLD));
        assert_eq!("ow".parse(), Ok(Dimension::OVERWORLD));
        assert_eq!("OVERWORLD".parse(), Ok(Dimension::OVERWORLD));
        assert_eq!("nether".parse(), Ok(Dimension::NETHER));
        assert_eq!("the_nether".parse(), Ok(Dimension::NETHER));
        assert_eq!("DIM-1".parse(), Ok(Dimension::NETHER));
        assert_eq!("end".parse(), Ok(Dimension::END));
        assert_eq!("the_end".parse(), Ok(Dimension::END));
        assert_eq!("DIM1".parse(), Ok(Dimension::END));
        assert_eq!("0".parse(), Ok(Dimension::OVERWORLD));
        assert_eq!("1".parse(), Ok(Dimension::NETHER));
        assert_eq!("2".parse(), Ok(Dimension::END));
        assert_eq!("42".parse(), Ok(Dimension::new(42)));
        assert_eq!("-7".parse(), Ok(Dimension::new(-7)));
        assert_eq!(Dimension::from_str("mordor"), Err(ParseCodeError(())));
        assert_eq!(Dimension::from_str(""), Err(ParseCodeError(())));
    }

    #[test]
    fn displays_dimension_names() {
        assert_eq!(Dimension::OVERWORLD.to_string(), "overworld");
        assert_eq!(Dimension::NETHER.to_string(), "nether");
        assert_eq!(Dimension::END.to_string(), "end");
        assert_eq!(Dimension::new(42).to_string(), "42");
        // Display output round-trips through FromStr.
        for dim in [Dimension::OVERWORLD, Dimension::NETHER, Dimension::END] {
            assert_eq!(dim.to_string().parse(), Ok(dim));
        }
    }

    #[test]
    fn raw_codes_round_trip() {
        assert_eq!(Dimension::OVERWORLD.raw(), 0);
        assert_eq!(Dimension::NETHER.raw(), 1);
        assert_eq!(Dimension::END.raw(), 2);
        assert_eq!(Dimension::new(42).raw(), 42);
    }
}
