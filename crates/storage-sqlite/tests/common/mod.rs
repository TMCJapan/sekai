//! Shared backend conformance flows.
//!
//! The flows target `sekai-core` traits; backend-specific setup stays in each runner.

use sekai_core::{
    BlobHash, BlobStore, ChunkCoord, CommitEntry, CommitId, Dimension, MetaStore, NamedScope, Rect,
    RefName, RegionKind, ScopeName,
    usecase::gc::{gc_apply, gc_plan},
};

const OVER: Dimension = Dimension::OVERWORLD;
const REGION: RegionKind = RegionKind::Region;

pub const fn coord(x: i32, z: i32) -> ChunkCoord {
    ChunkCoord::new(OVER, REGION, x, z)
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
        Some(vec![RegionKind::Region, RegionKind::Poi]),
        vec![
            (OVER, Rect::new(0, 0, 3, 3)),
            (Dimension::NETHER, Rect::region(-1, 0)),
            (OVER, Rect::new(-5, 2, -5, 2)),
        ],
        1_000,
    )
    .unwrap();
    meta.set_scope(&first).await.unwrap();
    assert_eq!(meta.lookup_scope(&spawn).await.unwrap(), Some(first));

    // A replacement swaps kinds and every area but keeps the creation time.
    let replaced =
        NamedScope::new(spawn.clone(), None, vec![(OVER, Rect::region(0, 0))], 2_000).unwrap();
    meta.set_scope(&replaced).await.unwrap();
    let mut expected = replaced.clone();
    expected.created_at_ms = 1_000;
    assert_eq!(
        meta.lookup_scope(&spawn).await.unwrap(),
        Some(expected.clone())
    );

    let other = NamedScope::new(
        hub.clone(),
        Some(vec![RegionKind::Entities]),
        vec![(Dimension::NETHER, Rect::new(0, 0, 0, 0))],
        3_000,
    )
    .unwrap();
    meta.set_scope(&other).await.unwrap();
    let shouted = NamedScope::new(
        upper.clone(),
        None,
        vec![(OVER, Rect::new(9, 9, 9, 9))],
        4_000,
    )
    .unwrap();
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
        Some(vec![RegionKind::Poi]),
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
    let id = meta.create_commit(1_000, &[]).await.unwrap();
    assert_eq!(meta.latest_commit().await.unwrap().unwrap().id, id);
    assert!(
        meta.lookup_commit(CommitId([0xFF; 32]))
            .await
            .unwrap()
            .is_none()
    );
    meta.record_chunk(id, &coord(0, 0), Some(&BlobHash([1; 32])), None)
        .await
        .unwrap();
    let row = meta.lookup_chunk(id, &coord(0, 0)).await.unwrap().unwrap();
    assert_eq!(row.blob, Some(BlobHash([1; 32])));
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
    let first = meta.create_commit(1_000, &[]).await.unwrap();
    meta.record_chunk(first, &coord(0, 0), Some(&BlobHash([1; 32])), None)
        .await
        .unwrap();
    meta.record_chunk(first, &coord(1, 0), Some(&BlobHash([1; 32])), None)
        .await
        .unwrap();
    let second = meta.create_commit(2_000, &[first]).await.unwrap();
    meta.record_chunk(second, &coord(0, 0), Some(&BlobHash([2; 32])), None)
        .await
        .unwrap();
    meta.record_chunk(second, &coord(1, 0), None, None)
        .await
        .unwrap();

    let mut fresh = 0usize;
    meta.visit_fresh_rows(second, |_| {
        fresh += 1;
        true
    })
    .await
    .unwrap();
    assert_eq!(fresh, 2);
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

/// Pruning folds oldest-first without changing retained effective states.
pub async fn prune_flow<M>(meta: &mut M)
where
    M: MetaStore,
    M::Error: core::fmt::Debug,
{
    use sekai_core::usecase::prune::{PruneReport, prune_apply, prune_plan, select_retained};
    // S1: A@0,0 B@1,0 / S2: C@0,0 (B carried) / S3: D@2,0.
    let first = meta.create_commit(1_000, &[]).await.unwrap();
    meta.record_chunk(first, &coord(0, 0), Some(&BlobHash([1; 32])), None)
        .await
        .unwrap();
    meta.record_chunk(first, &coord(1, 0), Some(&BlobHash([2; 32])), None)
        .await
        .unwrap();
    let second = meta.create_commit(2_000, &[first]).await.unwrap();
    meta.record_chunk(second, &coord(0, 0), Some(&BlobHash([3; 32])), None)
        .await
        .unwrap();
    let third = meta.create_commit(3_000, &[second]).await.unwrap();
    meta.record_chunk(third, &coord(2, 0), Some(&BlobHash([4; 32])), None)
        .await
        .unwrap();
    let before: Vec<Vec<(ChunkCoord, Option<BlobHash>)>> =
        effective_rows(meta, &[first, second, third]).await;

    let listed = sekai_core::usecase::graph::list_commits(&*meta)
        .await
        .unwrap();
    let retained = select_retained(&listed, Some(2), None);
    let plan = prune_plan(&*meta, &retained).await.unwrap();
    assert_eq!(plan.delete, [first]);
    assert_eq!(plan.retained, [second, third]);
    let report = prune_apply(meta, &plan, |_, _| {}).await.unwrap();
    assert_eq!(
        report,
        PruneReport {
            pruned: 1,
            rows_folded: 1,
            rows_dropped: 1,
        }
    );

    let after: Vec<Vec<(ChunkCoord, Option<BlobHash>)>> =
        effective_rows(meta, &[second, third]).await;
    assert_eq!(after, before[1..]);
    assert!(meta.lookup_commit(first).await.unwrap().is_none());

    // A retirement into an older or equal commit, or into one that does
    // not exist, is a backend error rather than a silent no-op: the first
    // would retroactively attach later rows to an earlier commit.
    for (from, into) in [
        (second, first),
        (first, first),
        (second, second),
        (second, CommitId([0xFF; 32])),
        (CommitId([0xFF; 32]), third),
    ] {
        assert!(
            meta.retire_commit(from, into).await.is_err(),
            "retire {from:?} into {into:?} must fail"
        );
    }
    // The rejected attempts changed nothing.
    let after_failures: Vec<Vec<(ChunkCoord, Option<BlobHash>)>> =
        effective_rows(meta, &[second, third]).await;
    assert_eq!(after_failures, after);
}

/// A whole chain retires into its tip: SQLite re-checks descent after each
/// retirement, so the second fold must still see the survivor as its
/// descendant once the first parent is gone. Refs that named a retired
/// commit follow the fold, including the active head.
pub async fn prune_chain_flow<M>(meta: &mut M)
where
    M: MetaStore,
    M::Error: core::fmt::Debug,
{
    use sekai_core::usecase::prune::{prune_apply, prune_plan, select_retained};
    // The production apply path requires an active ref; make that
    // dependency explicit rather than relying on the runner's setup.
    assert!(meta.active_ref().await.unwrap().is_some());
    // Build the chain through the production apply path, so the active
    // ref follows each commit rather than being positioned by hand.
    let first = meta
        .apply_commit_incremental(
            1_000,
            &[],
            &[CommitEntry::new(coord(0, 0), Some(BlobHash([1; 32])), None)],
        )
        .await
        .unwrap()
        .id;
    let second = meta
        .apply_commit_incremental(
            2_000,
            &[first],
            &[CommitEntry::new(coord(1, 0), Some(BlobHash([2; 32])), None)],
        )
        .await
        .unwrap()
        .id;
    let third = meta
        .apply_commit_incremental(
            3_000,
            &[second],
            &[CommitEntry::new(coord(2, 0), Some(BlobHash([3; 32])), None)],
        )
        .await
        .unwrap()
        .id;

    // The active head names the tip; two more refs name commits the prune
    // retires, so the flow exercises the re-pointing UPDATE.
    let head = RefName::parse("main").unwrap();
    let at_first = RefName::parse("at-first").unwrap();
    let at_second = RefName::parse("at-second").unwrap();
    assert_eq!(
        meta.active_ref().await.unwrap().unwrap().commit,
        Some(third)
    );
    meta.set_ref(&at_first, Some(first), 1_100).await.unwrap();
    meta.set_ref(&at_second, Some(second), 2_100).await.unwrap();

    let before = effective_rows(meta, &[third]).await;
    let listed = sekai_core::usecase::graph::list_commits(&*meta)
        .await
        .unwrap();
    let retained = select_retained(&listed, Some(1), None);
    let plan = prune_plan(&*meta, &retained).await.unwrap();
    assert_eq!(plan.delete, [first, second]);
    assert_eq!(plan.retained, [third]);
    let report = prune_apply(meta, &plan, |_, _| {}).await.unwrap();
    assert_eq!(report.pruned, 2);
    assert_eq!(effective_rows(meta, &[third]).await, before);
    assert!(meta.lookup_commit(first).await.unwrap().is_none());
    assert!(meta.lookup_commit(second).await.unwrap().is_none());
    for name in [&at_first, &at_second] {
        let record = meta.lookup_ref(name).await.unwrap().unwrap();
        assert_eq!(
            record.commit,
            Some(third),
            "ref {name} must follow the fold"
        );
    }
    let active = meta.active_ref().await.unwrap().unwrap();
    assert_eq!(active.name, head);
    assert_eq!(active.commit, Some(third));
}

async fn effective_rows<M>(meta: &M, ids: &[CommitId]) -> Vec<Vec<(ChunkCoord, Option<BlobHash>)>>
where
    M: MetaStore,
    M::Error: core::fmt::Debug,
{
    let mut out = Vec::new();
    for id in ids {
        let mut rows = Vec::new();
        meta.visit_commit_chunks(*id, |entry| {
            rows.push((entry.coord, entry.blob));
            true
        })
        .await
        .unwrap();
        rows.sort();
        out.push(rows);
    }
    out
}

/// The history graph: parents, the walk back to the root, and the
/// effective state that follows the graph rather than the identifier
/// order.
pub async fn history_graph<M>(meta: &mut M)
where
    M: MetaStore,
    M::Error: core::fmt::Debug,
{
    use sekai_core::usecase::graph::{commit_ancestors, commit_history};

    // Two independent histories: the second root shares no ancestor with
    // the first.
    let first = meta.create_commit(1_000, &[]).await.unwrap();
    meta.record_chunk(first, &coord(0, 0), Some(&BlobHash([1; 32])), None)
        .await
        .unwrap();
    let second = meta.create_commit(2_000, &[first]).await.unwrap();
    let other = meta.create_commit(3_000, &[]).await.unwrap();
    let child = meta.create_commit(4_000, &[other]).await.unwrap();

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

    // Retiring a commit hands its children the history of its own
    // parents, so the graph stays connected.
    meta.retire_commit(first, second).await.unwrap();
    assert!(meta.lookup_commit(first).await.unwrap().is_none());
    assert_eq!(meta.commit_parents(second).await.unwrap(), vec![]);
    // The folded row still resolves at the surviving commit.
    assert!(
        meta.lookup_chunk(second, &coord(0, 0))
            .await
            .unwrap()
            .is_some()
    );

    // Retiring the middle of a three-deep history hands the grandchild
    // the retired commit's own parent, not a dangling edge.
    let root = meta.create_commit(7_000, &[]).await.unwrap();
    let middle = meta.create_commit(8_000, &[root]).await.unwrap();
    let grandchild = meta.create_commit(9_000, &[middle]).await.unwrap();
    meta.retire_commit(middle, grandchild).await.unwrap();
    assert!(meta.lookup_commit(middle).await.unwrap().is_none());
    assert_eq!(meta.commit_parents(grandchild).await.unwrap(), vec![root]);
    assert_eq!(
        commit_ancestors(meta, grandchild).await.unwrap(),
        vec![root]
    );

    // Folding into a history that does not descend from the retired
    // commit would graft rows the successor never had.
    let unrelated = meta.create_commit(10_000, &[]).await.unwrap();
    assert!(meta.retire_commit(root, unrelated).await.is_err());
}

/// CAS put/fetch/dedup/remove/visit contract.
pub async fn cas_roundtrip<C>(cas: &mut C)
where
    C: BlobStore,
    C::Error: core::fmt::Debug,
{
    let hash = sekai_core::hash_blob(b"payload");
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
    assert!(cas.remove(&hash).await.unwrap());
    assert!(!cas.remove(&hash).await.unwrap());
    assert!(cas.fetch_into(&hash, &mut out).await.is_err());
}

/// Torn-commit simulation: an unreferenced blob is a GC orphan, a
/// referenced one survives planning and applying.
pub async fn torn_orphan_gc<S>(store: &mut S)
where
    S: sekai_storage::SekaiStore,
{
    let live = sekai_core::hash_blob(b"live");
    let orphan = sekai_core::hash_blob(b"orphan");
    assert!(store.cas_mut().put(&live, b"live").await.unwrap());
    assert!(store.cas_mut().put(&orphan, b"orphan").await.unwrap());
    // Sync-then-commit order: blobs are durable before metadata names them.
    store.cas_mut().sync().await.unwrap();
    store
        .meta_mut()
        .apply_commit_incremental(
            1_000,
            &[],
            &[CommitEntry::new(coord(0, 0), Some(live), None)],
        )
        .await
        .unwrap();

    let plan = gc_plan(store.cas(), store.meta()).await.unwrap();
    assert_eq!(plan.orphans, vec![orphan]);
    let (cas, meta) = store.cas_and_meta();
    let report = gc_apply(cas, meta, &plan, |_, _| {}).await.unwrap();
    assert_eq!(report.removed, 1);
    assert!(store.cas().contains(&live).await.unwrap());
    assert!(!store.cas().contains(&orphan).await.unwrap());
}
