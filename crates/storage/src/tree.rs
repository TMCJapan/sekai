//! Canonical snapshot-tree codec: the stored representation of a snapshot.
//!
//! The stored bytes are the hash preimage: each node is its sorted entry array,
//! and its digest covers a level-specific domain tag followed by those bytes.
//! Nodes only name the level below, so the reference graph is a layered DAG by
//! construction.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt;

use sekai_util::{ChunkHash, DimHash, RegionHash, RegionKind, RootHash, hash_bytes};

const ROOT_DOMAIN: &[u8] = b"sekai-root-tree-v1\0";
const DIM_DOMAIN: &[u8] = b"sekai-dim-tree-v1\0";
const REGION_DOMAIN: &[u8] = b"sekai-region-tree-v1\0";

const ROOT_ENTRY_LEN: usize = 4 + 32;
const DIM_ENTRY_LEN: usize = 4 + 4 + 4 + 32;
const REGION_ENTRY_LEN: usize = 1 + 1 + 32;

/// Tree level, for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeLevel {
    /// Root node.
    Root,
    /// Dimension node.
    Dim,
    /// Region node.
    Region,
}

impl fmt::Display for NodeLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root => f.write_str("root"),
            Self::Dim => f.write_str("dim"),
            Self::Region => f.write_str("region"),
        }
    }
}

/// A stored tree node failed its canonical encoding or hash check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeError {
    /// Node bytes are not a whole number of fixed-width entries.
    BadLength {
        /// Level that failed.
        level: NodeLevel,
        /// Observed byte length.
        len: usize,
    },
    /// Entries are not in strict coordinate order.
    OutOfOrder {
        /// Level that failed.
        level: NodeLevel,
    },
    /// Two entries name the same key.
    Duplicate {
        /// Level that failed.
        level: NodeLevel,
    },
    /// A dimension entry carries an unknown region-kind code.
    UnknownKind {
        /// Raw code read from the node.
        kind: i32,
    },
    /// A region entry names a local coordinate outside `0..32`.
    InvalidLocalCoord {
        /// Local chunk X.
        cx: u8,
        /// Local chunk Z.
        cz: u8,
    },
    /// Stored bytes do not hash to the key that named them.
    HashMismatch {
        /// Level that failed.
        level: NodeLevel,
    },
}

impl fmt::Display for TreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadLength { level, len } => {
                write!(f, "{level} node has invalid length {len}")
            }
            Self::OutOfOrder { level } => write!(f, "{level} node entries are not sorted"),
            Self::Duplicate { level } => write!(f, "{level} node has duplicate keys"),
            Self::UnknownKind { kind } => write!(f, "dim node has unknown region kind {kind}"),
            Self::InvalidLocalCoord { cx, cz } => {
                write!(f, "region node has invalid local coordinate {cx},{cz}")
            }
            Self::HashMismatch { level } => write!(f, "{level} node hash does not match its key"),
        }
    }
}

impl core::error::Error for TreeError {}

fn tagged_hash(tag: &[u8], node: &[u8]) -> [u8; 32] {
    let mut input = Vec::with_capacity(tag.len() + node.len());
    input.extend_from_slice(tag);
    input.extend_from_slice(node);
    hash_bytes(&input)
}

fn push_i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_be_bytes());
}

const fn read_i32(bytes: &[u8], offset: usize) -> i32 {
    i32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_hash(bytes: &[u8], offset: usize) -> [u8; 32] {
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&bytes[offset..offset + 32]);
    hash
}

/// One root-node entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RootEntry {
    /// Dimension namespace code.
    pub dim: i32,
    /// Hash of the dimension's node.
    pub hash: DimHash,
}

/// Canonical root node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootNode {
    entries: Vec<RootEntry>,
}

impl RootNode {
    /// Build a canonical node: entries sort by dimension, duplicates collapse.
    pub fn new(mut entries: Vec<RootEntry>) -> Self {
        entries.sort_by_key(|entry| entry.dim);
        entries.dedup_by_key(|entry| entry.dim);
        Self { entries }
    }

    /// Entries in canonical order.
    pub fn entries(&self) -> &[RootEntry] {
        &self.entries
    }

    /// Hash of the dimension node, if present.
    pub fn get(&self, dim: i32) -> Option<DimHash> {
        self.entries
            .binary_search_by_key(&dim, |entry| entry.dim)
            .ok()
            .map(|index| self.entries[index].hash)
    }

    /// Canonical encoding (the hash preimage without the domain tag).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.entries.len() * ROOT_ENTRY_LEN);
        for entry in &self.entries {
            push_i32(&mut out, entry.dim);
            out.extend_from_slice(entry.hash.as_bytes());
        }
        out
    }

    /// Node digest.
    pub fn hash(&self) -> RootHash {
        RootHash::new(tagged_hash(ROOT_DOMAIN, &self.encode()))
    }

    /// Decode and verify a stored root node.
    pub fn decode(bytes: &[u8], expected: RootHash) -> Result<Self, TreeError> {
        if !bytes.len().is_multiple_of(ROOT_ENTRY_LEN) {
            return Err(TreeError::BadLength {
                level: NodeLevel::Root,
                len: bytes.len(),
            });
        }
        if tagged_hash(ROOT_DOMAIN, bytes) != *expected.as_bytes() {
            return Err(TreeError::HashMismatch {
                level: NodeLevel::Root,
            });
        }
        let mut entries = Vec::with_capacity(bytes.len() / ROOT_ENTRY_LEN);
        let mut previous: Option<i32> = None;
        for chunk in bytes.as_chunks::<ROOT_ENTRY_LEN>().0 {
            let dim = read_i32(chunk, 0);
            if previous.is_some_and(|prev| prev >= dim) {
                return Err(if previous == Some(dim) {
                    TreeError::Duplicate {
                        level: NodeLevel::Root,
                    }
                } else {
                    TreeError::OutOfOrder {
                        level: NodeLevel::Root,
                    }
                });
            }
            previous = Some(dim);
            entries.push(RootEntry {
                dim,
                hash: DimHash::new(read_hash(chunk, 4)),
            });
        }
        Ok(Self { entries })
    }
}

/// One dimension-node entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DimEntry {
    /// Region family.
    pub kind: RegionKind,
    /// Region X.
    pub rx: i32,
    /// Region Z.
    pub rz: i32,
    /// Hash of the region's node.
    pub hash: RegionHash,
}

/// Canonical dimension node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DimNode {
    entries: Vec<DimEntry>,
}

fn dim_key(entry: &DimEntry) -> (i32, i32, i32) {
    (i32::from(entry.kind.raw()), entry.rx, entry.rz)
}

impl DimNode {
    /// Build a canonical node: entries sort by `(kind, rx, rz)`.
    pub fn new(mut entries: Vec<DimEntry>) -> Self {
        entries.sort_by_key(dim_key);
        entries.dedup_by_key(|entry| (i32::from(entry.kind.raw()), entry.rx, entry.rz));
        Self { entries }
    }

    /// Entries in canonical order.
    pub fn entries(&self) -> &[DimEntry] {
        &self.entries
    }

    /// Hash of the region node, if present.
    pub fn get(&self, kind: RegionKind, rx: i32, rz: i32) -> Option<RegionHash> {
        let key = (i32::from(kind.raw()), rx, rz);
        self.entries
            .binary_search_by_key(&key, dim_key)
            .ok()
            .map(|index| self.entries[index].hash)
    }

    /// Canonical encoding (the hash preimage without the domain tag).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.entries.len() * DIM_ENTRY_LEN);
        for entry in &self.entries {
            push_i32(&mut out, i32::from(entry.kind.raw()));
            push_i32(&mut out, entry.rx);
            push_i32(&mut out, entry.rz);
            out.extend_from_slice(entry.hash.as_bytes());
        }
        out
    }

    /// Node digest.
    pub fn hash(&self) -> DimHash {
        DimHash::new(tagged_hash(DIM_DOMAIN, &self.encode()))
    }

    /// Decode and verify a stored dimension node.
    pub fn decode(bytes: &[u8], expected: DimHash) -> Result<Self, TreeError> {
        if !bytes.len().is_multiple_of(DIM_ENTRY_LEN) {
            return Err(TreeError::BadLength {
                level: NodeLevel::Dim,
                len: bytes.len(),
            });
        }
        if tagged_hash(DIM_DOMAIN, bytes) != *expected.as_bytes() {
            return Err(TreeError::HashMismatch {
                level: NodeLevel::Dim,
            });
        }
        let mut entries = Vec::with_capacity(bytes.len() / DIM_ENTRY_LEN);
        let mut previous: Option<(i32, i32, i32)> = None;
        for chunk in bytes.as_chunks::<DIM_ENTRY_LEN>().0 {
            let raw_kind = read_i32(chunk, 0);
            let kind =
                RegionKind::new(raw_kind).map_err(|_| TreeError::UnknownKind { kind: raw_kind })?;
            let rx = read_i32(chunk, 4);
            let rz = read_i32(chunk, 8);
            let key = (raw_kind, rx, rz);
            if previous.is_some_and(|prev| prev >= key) {
                return Err(if previous == Some(key) {
                    TreeError::Duplicate {
                        level: NodeLevel::Dim,
                    }
                } else {
                    TreeError::OutOfOrder {
                        level: NodeLevel::Dim,
                    }
                });
            }
            previous = Some(key);
            entries.push(DimEntry {
                kind,
                rx,
                rz,
                hash: RegionHash::new(read_hash(chunk, 12)),
            });
        }
        Ok(Self { entries })
    }
}

/// One region-node entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RegionEntry {
    /// Local chunk X (`0..32`).
    pub cx: u8,
    /// Local chunk Z (`0..32`).
    pub cz: u8,
    /// CAS key of the chunk payload.
    pub hash: ChunkHash,
}

/// Canonical region node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegionNode {
    entries: Vec<RegionEntry>,
}

impl RegionNode {
    /// Build a canonical node: entries sort by local coordinate.
    pub fn new(mut entries: Vec<RegionEntry>) -> Self {
        entries.sort_by_key(|entry| (entry.cx, entry.cz));
        entries.dedup_by_key(|entry| (entry.cx, entry.cz));
        Self { entries }
    }

    /// Entries in canonical order.
    pub fn entries(&self) -> &[RegionEntry] {
        &self.entries
    }

    /// Hash of the chunk at a local slot.
    pub fn get(&self, cx: u8, cz: u8) -> Option<ChunkHash> {
        self.entries
            .binary_search_by_key(&(cx, cz), |entry| (entry.cx, entry.cz))
            .ok()
            .map(|index| self.entries[index].hash)
    }

    /// Canonical encoding (the hash preimage without the domain tag).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.entries.len() * REGION_ENTRY_LEN);
        for entry in &self.entries {
            out.push(entry.cx);
            out.push(entry.cz);
            out.extend_from_slice(entry.hash.as_bytes());
        }
        out
    }

    /// Node digest.
    pub fn hash(&self) -> RegionHash {
        RegionHash::new(tagged_hash(REGION_DOMAIN, &self.encode()))
    }

    /// Decode and verify a stored region node.
    pub fn decode(bytes: &[u8], expected: RegionHash) -> Result<Self, TreeError> {
        if !bytes.len().is_multiple_of(REGION_ENTRY_LEN) {
            return Err(TreeError::BadLength {
                level: NodeLevel::Region,
                len: bytes.len(),
            });
        }
        if tagged_hash(REGION_DOMAIN, bytes) != *expected.as_bytes() {
            return Err(TreeError::HashMismatch {
                level: NodeLevel::Region,
            });
        }
        let mut entries = Vec::with_capacity(bytes.len() / REGION_ENTRY_LEN);
        let mut previous: Option<(u8, u8)> = None;
        for chunk in bytes.as_chunks::<REGION_ENTRY_LEN>().0 {
            let cx = chunk[0];
            let cz = chunk[1];
            if cx >= 32 || cz >= 32 {
                return Err(TreeError::InvalidLocalCoord { cx, cz });
            }
            let key = (cx, cz);
            if previous.is_some_and(|prev| prev >= key) {
                return Err(if previous == Some(key) {
                    TreeError::Duplicate {
                        level: NodeLevel::Region,
                    }
                } else {
                    TreeError::OutOfOrder {
                        level: NodeLevel::Region,
                    }
                });
            }
            previous = Some(key);
            entries.push(RegionEntry {
                cx,
                cz,
                hash: ChunkHash::new(read_hash(chunk, 2)),
            });
        }
        Ok(Self { entries })
    }
}

/// Nodes written by one commit, children before parents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeNodes {
    /// Root nodes by hash.
    pub root: BTreeMap<RootHash, Vec<u8>>,
    /// Dimension nodes by hash.
    pub dim: BTreeMap<DimHash, Vec<u8>>,
    /// Region nodes by hash.
    pub region: BTreeMap<RegionHash, Vec<u8>>,
}

impl TreeNodes {
    /// Total node count.
    pub fn len(&self) -> usize {
        self.root.len() + self.dim.len() + self.region.len()
    }

    /// Whether no node is present.
    pub fn is_empty(&self) -> bool {
        self.root.is_empty() && self.dim.is_empty() && self.region.is_empty()
    }
}

/// Empty tree as a stored root node.
pub fn empty_tree() -> (RootHash, TreeNodes) {
    let node = RootNode::default();
    let hash = node.hash();
    let mut nodes = TreeNodes::default();
    nodes.root.insert(hash, node.encode());
    (hash, nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn domains_separate_equal_payloads() {
        let root = RootNode::default();
        let dim = DimNode::default();
        let region = RegionNode::default();
        assert_ne!(root.hash().as_bytes(), dim.hash().as_bytes());
        assert_ne!(dim.hash().as_bytes(), region.hash().as_bytes());
        assert_ne!(root.hash().as_bytes(), region.hash().as_bytes());
    }

    #[test]
    fn node_encoding_sorts_and_round_trips() {
        let region = RegionNode::new(vec![
            RegionEntry {
                cx: 1,
                cz: 1,
                hash: ChunkHash::new([2; 32]),
            },
            RegionEntry {
                cx: 0,
                cz: 1,
                hash: ChunkHash::new([1; 32]),
            },
        ]);
        let encoded = region.encode();
        assert_eq!(&encoded[..2], &[0, 1]);
        assert_eq!(RegionNode::decode(&encoded, region.hash()).unwrap(), region);

        let root = RootNode::new(vec![
            RootEntry {
                dim: 2,
                hash: DimHash::new([2; 32]),
            },
            RootEntry {
                dim: 1,
                hash: DimHash::new([1; 32]),
            },
        ]);
        assert_eq!(root.entries()[0].dim, 1);
        assert_eq!(RootNode::decode(&root.encode(), root.hash()).unwrap(), root);
    }

    #[test]
    fn decode_rejects_noncanonical_nodes() {
        let mut bytes = Vec::new();
        push_i32(&mut bytes, 2);
        bytes.extend_from_slice(DimHash::new([2; 32]).as_bytes());
        push_i32(&mut bytes, 1);
        bytes.extend_from_slice(DimHash::new([1; 32]).as_bytes());
        let hash = RootHash::new(tagged_hash(ROOT_DOMAIN, &bytes));
        assert_eq!(
            RootNode::decode(&bytes, hash),
            Err(TreeError::OutOfOrder {
                level: NodeLevel::Root
            })
        );

        let mut region = Vec::new();
        region.push(32);
        region.push(0);
        region.extend_from_slice(ChunkHash::new([1; 32]).as_bytes());
        let hash = RegionHash::new(tagged_hash(REGION_DOMAIN, &region));
        assert_eq!(
            RegionNode::decode(&region, hash),
            Err(TreeError::InvalidLocalCoord { cx: 32, cz: 0 })
        );
    }
}
