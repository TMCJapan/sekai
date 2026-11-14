//! Shared backend conformance flows.
//!
//! The flows target `sekai-core` traits; backend-specific setup stays in each runner.

use sekai_core::{
    BlobStore, ChunkCoord, ChunkHash, CommitEntry, CommitId, Dimension, MetaStore, NamedScope,
    Rect, RefName, RegionKind, ScopeName,
};

const OVER: Dimension = Dimension::OVERWORLD;
const REGION: RegionKind = RegionKind::Region;

pub const fn coord(x: i32, z: i32) -> ChunkCoord {
    ChunkCoord::new(OVER, REGION, x, z)
}

const fn present(x: i32, z: i32, byte: u8) -> CommitEntry {
    CommitEntry::new(coord(x, z), Some(ChunkHash::new([byte; 32])))
}

/// Named scope CRUD: create, replace, list in name order, delete.
pub async fn scope_flow<M>(meta: &mut M)
where
    M: MetaStore,
    M::Error: core::fmt::Debug,
{
    let spawn = ScopeName::parse("spawn").unwrap();
    let hub = ScopeName::parse("hub/nether").unwrap();
    // `S` (0x53) sorts before `h` (0x68) byte-wise but after it
    // case-insensitively, and `Spawn` must not collide with `spawn`.
    let upper = ScopeName::parse("Spawn").unwrap();
    assert!(meta.lookup_scope(&spawn).await.unwrap().is_none());

    let first = NamedScope::new(
        spawn.clone(),
        vec![
            (OVER, Rect::new(0, 0, 3, 3)),
            (OVER, Rect::new(2, 2, 5, 5)),
            (Dimension::NETHER, Rect::region(-1, 0)),
            (OVER, Rect::new(-5, 2, -5, 2)),
        ],
        1_000,
    )
    .unwrap();
    meta.set_scope(&first).await.unwrap();
    let stored = meta.lookup_scope(&spawn).await.unwrap();
    // Overlapping areas stay apart: the scope is their union either way.
    assert_eq!(stored.as_ref().map(|scope| scope.areas().len()), Some(4));
    assert_eq!(stored, Some(first));

    // A replacement swaps every area but keeps the creation time.
    let replaced = NamedScope::new(spawn.clone(), vec![(OVER, Rect::region(0, 0))], 2_000).unwrap();
    meta.set_scope(&replaced).await.unwrap();
    let mut expected = replaced.clone();
    expected.created_at_ms = 1_000;
    assert_eq!(
        meta.lookup_scope(&spawn).await.unwrap(),
        Some(expected.clone())
    );

    let other = NamedScope::new(
        hub.clone(),
        vec![(Dimension::NETHER, Rect::new(0, 0, 0, 0))],
        3_000,
    )
    .unwrap();
    meta.set_scope(&other).await.unwrap();
    let shouted =
        NamedScope::new(upper.clone(), vec![(OVER, Rect::new(9, 9, 9, 9))], 4_000).unwrap();
    meta.set_scope(&shouted).await.unwrap();
    assert_eq!(
        meta.lookup_scope(&spawn).await.unwrap(),
        Some(expected),
        "names are case-sensitive"
    );
    let mut names = Vec::new();
    meta.visit_scopes(|scope| {
        names.push(scope.name.clone());
        true
    })
    .await
    .unwrap();
    assert_eq!(
        names,
        vec![upper.clone(), hub.clone(), spawn.clone()],
        "byte-wise name order"
    );

    let mut visited = 0;
    meta.visit_scopes(|_| {
        visited += 1;
        false
    })
    .await
    .unwrap();
    assert_eq!(visited, 1, "the visitor stops early");

    assert!(meta.delete_scope(&spawn).await.unwrap());
    assert!(!meta.delete_scope(&spawn).await.unwrap());
    assert!(meta.lookup_scope(&spawn).await.unwrap().is_none());
    assert_eq!(meta.lookup_scope(&hub).await.unwrap(), Some(other));
    assert_eq!(meta.lookup_scope(&upper).await.unwrap(), Some(shouted));

    // A deleted name starts over: nothing of the old record survives,
    // its creation time included.
    let reborn = NamedScope::new(
        spawn.clone(),
        vec![(Dimension::END, Rect::new(1, 2, 3, 4))],
        5_000,
    )
    .unwrap();
    meta.set_scope(&reborn).await.unwrap();
    assert_eq!(meta.lookup_scope(&spawn).await.unwrap(), Some(reborn));
}

/// Commit CRUD plus point lookups.
pub async fn commit_lifecycle<M>(meta: &mut M)
where
    M: MetaStore,
    M::Error: core::fmt::Debug,
{
    assert!(meta.latest_commit().await.unwrap().is_none());
    let id = meta
        .apply_commit_incremental(1_000, &[], &[present(0, 0, 1)])
        .await
        .unwrap()
        .id;
    assert_eq!(meta.latest_commit().await.unwrap().unwrap().id, id);
    assert!(
        meta.lookup_commit(CommitId([0xFF; 32]))
            .await
            .unwrap()
            .is_none()
    );
    let row = meta.lookup_chunk(id, &coord(0, 0)).await.unwrap().unwrap();
    assert_eq!(row.blob, Some(ChunkHash::new([1; 32])));
    assert!(meta.lookup_chunk(id, &coord(1, 0)).await.unwrap().is_none());
    let listed = sekai_core::usecase::graph::list_commits(&*meta)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
}

/// Fresh-row visits and per-commit statistics.
pub async fn fresh_stats<M>(meta: &mut M)
where
    M: MetaStore,
    M::Error: core::fmt::Debug,
{
    use sekai_core::usecase::graph::{CommitStats, commit_stats};
    let first = meta
        .apply_commit_incremental(1_000, &[], &[present(0, 0, 1), present(1, 0, 1)])
        .await
        .unwrap()
        .id;
    let second = meta
        .apply_commit_incremental(
            2_000,
            &[first],
            &[present(0, 0, 2), CommitEntry::new(coord(1, 0), None)],
        )
        .await
        .unwrap()
        .id;

    assert_eq!(
        commit_stats(&*meta, first).await.unwrap(),
        CommitStats {
            fresh_chunks: 2,
            fresh_tombstones: 0,
            new_blobs: 1,
            effective_chunks: 2,
        }
    );
    assert_eq!(
        commit_stats(&*meta, second).await.unwrap(),
        CommitStats {
            fresh_chunks: 1,
            fresh_tombstones: 1,
            new_blobs: 1,
            effective_chunks: 1,
        }
    );
}

/// Bulk tree reads: one node per input hash in input order, repeated
/// hashes repeat nodes, unknown hashes fail, and a wide walk visits
/// every chunk exactly once in coordinate order.
pub async fn tree_bulk_reads<M>(meta: &mut M)
where
    M: MetaStore,
    M::Error: core::fmt::Debug,
{
    use sekai_core::{DimHash, RegionHash};

    let nether = ChunkCoord::new(Dimension::NETHER, REGION, 0, 0);
    // The two overworld regions hold identical chunks, so they share
    // one region node hash.
    let id = meta
        .apply_commit_incremental(
            1_000,
            &[],
            &[
                present(0, 0, 1),
                present(32, 0, 1),
                CommitEntry::new(nether, Some(ChunkHash::new([2; 32]))),
            ],
        )
        .await
        .unwrap()
        .id;
    let root = meta.lookup_commit(id).await.unwrap().unwrap().root;
    let root_node = meta.read_root_node(root).await.unwrap();
    let over_dim = root_node.get(OVER.raw()).unwrap();
    let nether_dim = root_node.get(Dimension::NETHER.raw()).unwrap();

    let dims = meta
        .read_dim_nodes(&[nether_dim, over_dim, nether_dim])
        .await
        .unwrap();
    assert_eq!(dims.len(), 3);
    assert_eq!(dims[0], dims[2]);
    assert_ne!(dims[0], dims[1]);
    assert_eq!(dims[1], meta.read_dim_node(over_dim).await.unwrap());

    let over = meta.read_dim_node(over_dim).await.unwrap();
    let a = over.get(REGION, 0, 0).unwrap();
    let b = over.get(REGION, 1, 0).unwrap();
    assert_eq!(a, b, "identical regions share one node hash");
    let regions = meta.read_region_nodes(&[a, b]).await.unwrap();
    assert_eq!(regions.len(), 2);
    assert_eq!(regions[0], regions[1]);
    assert_eq!(regions[0].get(0, 0), Some(ChunkHash::new([1; 32])));

    assert_eq!(meta.read_dim_nodes(&[]).await.unwrap(), []);
    assert_eq!(meta.read_region_nodes(&[]).await.unwrap(), []);
    assert!(
        meta.read_dim_nodes(&[DimHash::new([0xFF; 32])])
            .await
            .is_err()
    );
    assert!(
        meta.read_region_nodes(&[RegionHash::new([0xFF; 32])])
            .await
            .is_err()
    );

    // A walk spanning the backend's read batches visits every chunk
    // exactly once, in coordinate order.
    let many: Vec<CommitEntry> = (0..1_100)
        .map(|i| present(i * 32, 0, u8::try_from(i % 251).unwrap()))
        .collect();
    let big = meta
        .apply_commit_incremental(2_000, &[id], &many)
        .await
        .unwrap()
        .id;
    let mut coords = Vec::new();
    meta.visit_commit_chunks(big, |entry| {
        coords.push(entry.coord);
        true
    })
    .await
    .unwrap();
    assert_eq!(
        coords.len(),
        1_101,
        "1100 overworld chunks plus the nether one"
    );
    assert!(coords.windows(2).all(|pair| pair[0] < pair[1]));
}

/// The history graph: parents, the walk back to the root, and the
/// snapshot inherited through the graph rather than identifier order.
pub async fn history_graph<M>(meta: &mut M)
where
    M: MetaStore,
    M::Error: core::fmt::Debug,
{
    use sekai_core::usecase::graph::{commit_ancestors, commit_history};

    let main = RefName::parse("main").unwrap();
    // Two independent histories: the second root shares no ancestor with
    // the first. Pointing the active ref back to unborn starts it.
    let first = meta
        .apply_commit_incremental(1_000, &[], &[present(0, 0, 1)])
        .await
        .unwrap()
        .id;
    let second = meta
        .apply_commit_incremental(2_000, &[first], &[])
        .await
        .unwrap()
        .id;
    meta.set_ref(&main, None, 2_500).await.unwrap();
    let other = meta
        .apply_commit_incremental(3_000, &[], &[])
        .await
        .unwrap()
        .id;
    let child = meta
        .apply_commit_incremental(4_000, &[other], &[])
        .await
        .unwrap()
        .id;

    // The graph, not the local record order, decides what a commit
    // inherits: the chunk of `first` is invisible from `child`.
    assert_eq!(meta.commit_parents(second).await.unwrap(), vec![first]);
    assert_eq!(meta.commit_parents(first).await.unwrap(), vec![]);
    assert_eq!(meta.commit_parents(child).await.unwrap(), vec![other]);
    assert_eq!(meta.commit_parents(other).await.unwrap(), vec![]);
    assert!(
        meta.lookup_chunk(child, &coord(0, 0))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        meta.lookup_chunk(second, &coord(0, 0))
            .await
            .unwrap()
            .is_some()
    );
    // A parent must exist: the graph never points at a missing commit.
    assert!(
        meta.create_commit(5_000, &[CommitId([0xFF; 32])])
            .await
            .is_err()
    );
    // A parent listed twice is recorded once, not a broken edge.
    let twice = meta.create_commit(6_000, &[first, first]).await.unwrap();
    assert_eq!(meta.commit_parents(twice).await.unwrap(), vec![first]);

    // Walking back covers the whole history, nearest first.
    assert_eq!(commit_ancestors(meta, second).await.unwrap(), vec![first]);
    assert_eq!(commit_ancestors(meta, child).await.unwrap(), vec![other]);
    let walked = commit_history(meta, second).await.unwrap();
    assert_eq!(
        walked.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![second, first]
    );
    assert_eq!(walked[0].parents, vec![first]);
    assert!(walked[1].is_root());

    // A merge commit continues two histories at once. The parents are
    // recorded in canonical (ascending hash) order, and the walk reports
    // the direct parents first, in that order.
    let merge = meta.create_commit(6_500, &[other, second]).await.unwrap();
    let mut direct = vec![second, other];
    direct.sort();
    assert_eq!(meta.commit_parents(merge).await.unwrap(), direct);
    let mut ancestors = direct.clone();
    ancestors.push(first);
    assert_eq!(commit_ancestors(meta, merge).await.unwrap(), ancestors);
    let mut history = vec![merge];
    history.extend(direct);
    history.push(first);
    assert_eq!(
        commit_history(meta, merge)
            .await
            .unwrap()
            .iter()
            .map(|c| c.id)
            .collect::<Vec<_>>(),
        history
    );
}

/// CAS put/fetch/dedup/visit contract.
pub async fn cas_roundtrip<C>(cas: &mut C)
where
    C: BlobStore,
    C::Error: core::fmt::Debug,
{
    let hash = ChunkHash::new(sekai_core::hash_bytes(b"payload"));
    assert!(!cas.contains(&hash).await.unwrap());
    assert!(cas.put(&hash, b"payload").await.unwrap());
    assert!(!cas.put(&hash, b"payload").await.unwrap());
    assert!(cas.contains(&hash).await.unwrap());
    let mut out = Vec::new();
    cas.fetch_into(&hash, &mut out).await.unwrap();
    assert_eq!(out, b"payload");
    // `fetch_into` clears the buffer first.
    cas.fetch_into(&hash, &mut out).await.unwrap();
    assert_eq!(out, b"payload");
    let mut count = 0;
    cas.visit_blobs(|_| {
        count += 1;
        true
    })
    .await
    .unwrap();
    assert_eq!(count, 1);
}

/// Torn-commit simulation: the crash order (blobs synced before the
/// metadata commit) keeps every referenced blob resolvable, while a blob
/// whose commit never landed stays in the append-only CAS as an orphan.
pub async fn torn_commit_leaves_orphans<S>(store: &mut S)
where
    S: sekai_storage::SekaiStore,
{
    let live = ChunkHash::new(sekai_core::hash_bytes(b"live"));
    let orphan = ChunkHash::new(sekai_core::hash_bytes(b"orphan"));
    assert!(store.blob_mut().put(&live, b"live").await.unwrap());
    assert!(store.blob_mut().put(&orphan, b"orphan").await.unwrap());
    // Sync-then-commit order: blobs are durable before metadata names them.
    store.blob_mut().sync().await.unwrap();
    let commit = store
        .meta_mut()
        .apply_commit_incremental(1_000, &[], &[CommitEntry::new(coord(0, 0), Some(live))])
        .await
        .unwrap()
        .id;

    // The referenced blob resolves through the committed snapshot.
    let row = store
        .meta()
        .lookup_chunk(commit, &coord(0, 0))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.blob, Some(live));
    let mut out = Vec::new();
    store.blob().fetch_into(&live, &mut out).await.unwrap();
    assert_eq!(out, b"live");

    // The orphan's commit never landed, but the append-only CAS keeps the
    // payload: nothing reclaims it, and it never became a dangling
    // reference.
    assert!(store.blob().contains(&orphan).await.unwrap());
    store.blob().fetch_into(&orphan, &mut out).await.unwrap();
    assert_eq!(out, b"orphan");
}
