//! Content-addressed snapshot tree hashes.
//!
//! Every level has its own hash type so a node can only name the level below
//! it. The codec that produces these hashes lives in `sekai-core`; this
//! module only owns the shared vocabulary.

use alloc::string::String;
use core::fmt;
use core::str::FromStr;

use crate::error::HexError;
use crate::hash::{hex32_from, hex32_into};

macro_rules! tree_hash {
    ($name:ident, $level:literal) => {
        #[doc = concat!("Blake3 digest of a ", $level, " tree node.")]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 32]);

        impl $name {
            /// Wrap a raw digest.
            #[inline]
            pub const fn new(hash: [u8; 32]) -> Self {
                Self(hash)
            }

            /// Raw 32 bytes (e.g. for binary DB columns).
            #[inline]
            pub const fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            /// Encode as lowercase hex without allocation.
            pub const fn hex_into(self) -> [u8; 64] {
                hex32_into(self.0)
            }

            /// Encode as a lowercase hex string.
            pub fn hex_string(self) -> String {
                String::from_utf8(self.hex_into().to_vec()).unwrap_or_default()
            }

            /// Parse a 64-byte hexadecimal value.
            pub fn from_hex(input: &[u8]) -> Result<Self, HexError> {
                hex32_from(input).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(core::str::from_utf8(&self.hex_into()).map_err(|_| fmt::Error)?)
            }
        }

        impl FromStr for $name {
            type Err = HexError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::from_hex(s.as_bytes())
            }
        }
    };
}

tree_hash!(RootHash, "root");
tree_hash!(DimHash, "dimension");
tree_hash!(RegionHash, "region");

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn tree_hashes_round_trip_and_display() {
        let root = RootHash::new([0xAB; 32]);
        let dim = DimHash::new([0xCD; 32]);
        let region = RegionHash::new([0xEF; 32]);
        assert_eq!(RootHash::from_str(&root.to_string()), Ok(root));
        assert_eq!(DimHash::from_hex(&dim.hex_into()), Ok(dim));
        assert_eq!(region.hex_string().len(), 64);
        assert_ne!(root.as_bytes(), dim.as_bytes());
    }
}
