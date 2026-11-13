//! Canonical snapshot-tree codec and tree operations.
//!
//! The stored bytes are the hash preimage: each node is its sorted entry array,
//! and its digest covers a level-specific domain tag followed by those bytes.
//! Nodes only name the level below, so the reference graph is a layered DAG by
//! construction.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt;

use crate::port::meta::MetaStore;
use sekai_util::{
    ChunkCoord, ChunkHash, CommitEntry, CommitId, DimHash, RegionHash, RegionKind, RootHash,
    hash_bytes,
};

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

#[allow(clippy::cast_possible_truncation)]
const fn local_slot(coord: ChunkCoord) -> (u8, u8) {
    // `rem_euclid(32)` is always in `0..32`, so the cast cannot truncate.
    (coord.x.rem_euclid(32) as u8, coord.z.rem_euclid(32) as u8)
}

fn coord_from_parts(dim: i32, kind: RegionKind, rx: i32, rz: i32, cx: u8, cz: u8) -> ChunkCoord {
    ChunkCoord::new(
        sekai_util::Dimension::new(dim),
        kind,
        rx * 32 + i32::from(cx),
        rz * 32 + i32::from(cz),
    )
}

type RegionKeyParts = (RegionKind, i32, i32);
type SlotChanges = BTreeMap<(u8, u8), Option<ChunkHash>>;
type RegionChanges = BTreeMap<RegionKeyParts, SlotChanges>;

/// Build the nodes a commit writes by merging `entries` into its parent tree.
///
/// The first parent is the base when a merge carries several parents; tree
/// merge policy is out of scope until something creates merges. Out-of-scope
/// subtrees are never read: only the changed regions and their dim nodes are
/// loaded, and every unchanged hash is reused.
pub async fn build_update<M: MetaStore>(
    meta: &M,
    parents: &[CommitId],
    entries: &[CommitEntry],
) -> Result<(RootHash, TreeNodes), M::Error> {
    let mut canonical_parents = parents.to_vec();
    canonical_parents.sort();
    canonical_parents.dedup();

    let parent_root_hash = match canonical_parents.first().copied() {
        Some(parent) => meta.lookup_commit(parent).await?.map(|commit| commit.root),
        None => None,
    };
    let (base, had_parent) = match parent_root_hash {
        Some(hash) => (meta.read_root_node(hash).await?, true),
        None => (RootNode::default(), false),
    };

    let mut root_entries: BTreeMap<i32, DimHash> = base
        .entries()
        .iter()
        .map(|entry| (entry.dim, entry.hash))
        .collect();
    let mut changes: BTreeMap<i32, RegionChanges> = BTreeMap::new();
    for entry in entries {
        let coord = entry.coord;
        let region = coord.region_x();
        let region_z = coord.region_z();
        changes
            .entry(coord.dim.raw())
            .or_default()
            .entry((coord.kind, region, region_z))
            .or_default()
            .insert(local_slot(coord), entry.blob);
    }

    let mut nodes = TreeNodes::default();
    for (dim, region_changes) in changes {
        let old_dim_hash = root_entries.get(&dim).copied();
        let old_dim = match old_dim_hash {
            Some(hash) => meta.read_dim_node(hash).await?,
            None => DimNode::default(),
        };
        let mut dim_entries: BTreeMap<RegionKeyParts, RegionHash> = old_dim
            .entries()
            .iter()
            .map(|entry| ((entry.kind, entry.rx, entry.rz), entry.hash))
            .collect();
        for (region_key, slot_changes) in region_changes {
            let (kind, rx, rz) = region_key;
            let old_region_hash = dim_entries.get(&region_key).copied();
            let old_region = match old_region_hash {
                Some(hash) => meta.read_region_node(hash).await?,
                None => RegionNode::default(),
            };
            let mut slots: BTreeMap<(u8, u8), ChunkHash> = old_region
                .entries()
                .iter()
                .map(|entry| ((entry.cx, entry.cz), entry.hash))
                .collect();
            for (slot, blob) in slot_changes {
                match blob {
                    Some(hash) => {
                        slots.insert(slot, hash);
                    }
                    None => {
                        slots.remove(&slot);
                    }
                }
            }
            if slots.is_empty() {
                dim_entries.remove(&region_key);
                continue;
            }
            let region = RegionNode::new(
                slots
                    .into_iter()
                    .map(|((cx, cz), hash)| RegionEntry { cx, cz, hash })
                    .collect(),
            );
            let hash = region.hash();
            if old_region_hash == Some(hash) {
                continue;
            }
            nodes.region.insert(hash, region.encode());
            dim_entries.insert((kind, rx, rz), hash);
        }

        if dim_entries.is_empty() {
            root_entries.remove(&dim);
            continue;
        }
        let dim_node = DimNode::new(
            dim_entries
                .into_iter()
                .map(|((kind, rx, rz), hash)| DimEntry { kind, rx, rz, hash })
                .collect(),
        );
        let hash = dim_node.hash();
        if old_dim_hash == Some(hash) {
            continue;
        }
        nodes.dim.insert(hash, dim_node.encode());
        root_entries.insert(dim, hash);
    }

    let root = RootNode::new(
        root_entries
            .into_iter()
            .map(|(dim, hash)| RootEntry { dim, hash })
            .collect(),
    );
    let hash = root.hash();
    if !had_parent || parent_root_hash != Some(hash) {
        nodes.root.insert(hash, root.encode());
    }
    Ok((hash, nodes))
}

/// Record `entries` as a commit, building its tree from the parent snapshot.
pub async fn apply_entries<M: MetaStore>(
    meta: &mut M,
    created_at_ms: u64,
    parents: &[CommitId],
    entries: &[CommitEntry],
) -> Result<sekai_util::ApplyOutcome, M::Error> {
    let (root, nodes) = build_update(meta, parents, entries).await?;
    meta.commit_tree(created_at_ms, parents, root, &nodes).await
}

/// Effective present entry for one coordinate at a commit.
pub async fn lookup_chunk<M: MetaStore>(
    meta: &M,
    commit: CommitId,
    coord: &ChunkCoord,
) -> Result<Option<CommitEntry>, M::Error> {
    let Some(record) = meta.lookup_commit(commit).await? else {
        return Ok(None);
    };
    let root = meta.read_root_node(record.root).await?;
    let Some(dim_hash) = root.get(coord.dim.raw()) else {
        return Ok(None);
    };
    let dim = meta.read_dim_node(dim_hash).await?;
    let (rx, rz) = (coord.region_x(), coord.region_z());
    let Some(region_hash) = dim.get(coord.kind, rx, rz) else {
        return Ok(None);
    };
    let region = meta.read_region_node(region_hash).await?;
    let (cx, cz) = local_slot(*coord);
    Ok(region
        .get(cx, cz)
        .map(|hash| CommitEntry::new(*coord, Some(hash))))
}

/// Visit every present chunk of a snapshot, in global coordinate order.
pub async fn visit_chunks<M: MetaStore, F>(
    meta: &M,
    commit: CommitId,
    mut visit: F,
) -> Result<(), M::Error>
where
    F: FnMut(&CommitEntry) -> bool + Send,
{
    let Some(record) = meta.lookup_commit(commit).await? else {
        return Ok(());
    };
    let root = meta.read_root_node(record.root).await?;
    let mut entries = Vec::new();
    for root_entry in root.entries() {
        let dim = meta.read_dim_node(root_entry.hash).await?;
        for dim_entry in dim.entries() {
            let region = meta.read_region_node(dim_entry.hash).await?;
            for region_entry in region.entries() {
                let coord = coord_from_parts(
                    root_entry.dim,
                    dim_entry.kind,
                    dim_entry.rx,
                    dim_entry.rz,
                    region_entry.cx,
                    region_entry.cz,
                );
                entries.push(CommitEntry::new(coord, Some(region_entry.hash)));
            }
        }
    }
    entries.sort_by_key(|entry| entry.coord);
    for entry in &entries {
        if !visit(entry) {
            break;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{MemMeta, block_on};
    use alloc::vec;
    use sekai_util::{Dimension, RefName};

    const OVER: Dimension = Dimension::OVERWORLD;
    const NETHER: Dimension = Dimension::NETHER;
    const REGION: RegionKind = RegionKind::Region;

    fn coord(dim: Dimension, x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(dim, REGION, x, z)
    }

    fn entry(dim: Dimension, x: i32, z: i32, byte: u8) -> CommitEntry {
        CommitEntry::new(coord(dim, x, z), Some(ChunkHash::new([byte; 32])))
    }

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

    #[test]
    fn scoped_update_only_reads_and_writes_changed_dimensions() {
        let mut meta = MemMeta::default();
        meta.with_active_ref(&RefName::parse("main").unwrap());
        let first = block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[entry(OVER, 0, 0, 1), entry(NETHER, 0, 0, 2)],
        ))
        .unwrap()
        .id;
        let old_root = block_on(meta.lookup_commit(first)).unwrap().unwrap().root;
        let old_root_node = block_on(meta.read_root_node(old_root)).unwrap();
        let over_hash = old_root_node.get(OVER.raw()).unwrap();
        let region_nodes_before = meta.region_nodes.len();

        let (root, nodes) =
            block_on(build_update(&meta, &[first], &[entry(NETHER, 1, 0, 3)])).unwrap();
        assert_ne!(root, old_root);
        assert_eq!(nodes.region.len(), 1, "one changed region node");
        assert_eq!(nodes.dim.len(), 1, "one changed dimension node");
        assert!(nodes.root.contains_key(&root));
        assert_eq!(
            RootNode::new(vec![RootEntry {
                dim: OVER.raw(),
                hash: over_hash,
            }])
            .entries()
            .len(),
            1,
            "the untouched dimension is reused by hash"
        );
        assert!(meta.region_nodes.len() >= region_nodes_before);
    }

    #[test]
    fn an_unchanged_update_reuses_the_parent_root() {
        let mut meta = MemMeta::default();
        meta.with_active_ref(&RefName::parse("main").unwrap());
        let first = block_on(meta.apply_commit_incremental(1_000, &[], &[entry(OVER, 0, 0, 1)]))
            .unwrap()
            .id;
        let root = block_on(meta.lookup_commit(first)).unwrap().unwrap().root;
        let (next, nodes) =
            block_on(build_update(&meta, &[first], &[entry(OVER, 0, 0, 1)])).unwrap();
        assert_eq!(next, root);
        assert!(nodes.is_empty(), "an unchanged tree writes no node");
    }
}
