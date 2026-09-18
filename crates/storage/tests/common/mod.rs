//! Shared backend conformance flows.
//!
//! The flows target `sekai-core` traits; backend-specific setup stays in each runner.

use sekai_core::{
    BlobHash, BlobStore, ChunkCoord, Dimension, MetaStore, Observation, RegionFingerprint,
    RegionKey, RegionKind, Scope, SnapshotEntry, SnapshotId, SnapshotTag, TagName,
    usecase::{
        backup::{assemble, commit, plan_backup, stage_present},
        gc::{gc_apply, gc_plan},
        rollback::plan_rollback,
    },
};
use std::collections::BTreeSet;

const OVER: Dimension = Dimension::OVERWORLD;
const REGION: RegionKind = RegionKind::REGION;

pub const fn key(rx: i32, rz: i32) -> RegionKey {
    RegionKey::new(OVER, REGION, rx, rz)
}

pub const fn coord(x: i32, z: i32) -> ChunkCoord {
    ChunkCoord::new(OVER, REGION, x, z)
}

pub const fn fingerprint(key: RegionKey) -> RegionFingerprint {
    RegionFingerprint {
        key,
        mtime_ms: Some(1_700_000_000_000),
        size: 8192,
        header_hash: [7; 32],
    }
}

/// Snapshot CRUD plus point lookups.
pub async fn snapshot_lifecycle<M>(meta: &mut M)
where
    M: MetaStore,
    M::Error: core::fmt::Debug,
{
    assert!(meta.latest_snapshot().await.unwrap().is_none());
    let id = meta.create_snapshot(1_000).await.unwrap();
    assert_eq!(meta.latest_snapshot().await.unwrap().unwrap().id, id);
    assert!(
        meta.lookup_snapshot(SnapshotId(999))
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
    let listed = sekai_core::usecase::snapshot::list_snapshots(&*meta)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
}

/// Tag CRUD plus name-ordered visits and ref resolution.
pub async fn tags<M>(meta: &mut M)
where
    M: MetaStore,
    M::Error: core::fmt::Debug,
{
    let first = meta.create_snapshot(1_000).await.unwrap();
    let second = meta.create_snapshot(2_000).await.unwrap();
    let zeta = TagName::parse("zeta").unwrap();
    let alpha = TagName::parse("alpha").unwrap();
    meta.tag_snapshot(&zeta, first, 1_500).await.unwrap();
    meta.tag_snapshot(&alpha, second, 2_500).await.unwrap();

    let record = meta.lookup_tag(&alpha).await.unwrap().unwrap();
    assert_eq!(record, SnapshotTag::new(alpha.clone(), second, 2_500));
    assert!(
        meta.lookup_tag(&TagName::parse("missing").unwrap())
            .await
            .unwrap()
            .is_none()
    );

    let mut seen = Vec::new();
    meta.visit_tags(|tag| {
        seen.push(tag.name.as_str().to_owned());
        true
    })
    .await
    .unwrap();
    assert_eq!(seen, ["alpha", "zeta"]);

    assert_eq!(
        sekai_core::usecase::snapshot::resolve_snapshot_ref(&*meta, "@zeta")
            .await
            .unwrap(),
        first
    );

    assert!(meta.untag(&alpha).await.unwrap());
    assert!(!meta.untag(&alpha).await.unwrap());
}

/// Fresh-row visits and per-snapshot statistics.
pub async fn fresh_stats<M>(meta: &mut M)
where
    M: MetaStore,
    M::Error: core::fmt::Debug,
{
    use sekai_core::usecase::snapshot::{SnapshotStats, snapshot_stats};
    let first = meta.create_snapshot(1_000).await.unwrap();
    meta.record_chunk(first, &coord(0, 0), Some(&BlobHash([1; 32])), None)
        .await
        .unwrap();
    meta.record_chunk(first, &coord(1, 0), Some(&BlobHash([1; 32])), None)
        .await
        .unwrap();
    let second = meta.create_snapshot(2_000).await.unwrap();
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
        snapshot_stats(&*meta, first).await.unwrap(),
        SnapshotStats {
            fresh_chunks: 2,
            fresh_tombstones: 0,
            new_blobs: 1,
            effective_chunks: 2,
        }
    );
    assert_eq!(
        snapshot_stats(&*meta, second).await.unwrap(),
        SnapshotStats {
            fresh_chunks: 1,
            fresh_tombstones: 1,
            new_blobs: 1,
            effective_chunks: 1,
        }
    );
}

/// Fingerprint carry: unchanged regions skip ingest, changed ones re-ingest.
pub async fn backup_carry<M>(meta: &mut M)
where
    M: MetaStore,
    M::Error: core::fmt::Debug,
{
    let fp0 = fingerprint(key(0, 0));
    let fp1 = fingerprint(key(1, 0));
    meta.apply_snapshot_incremental(
        1_000,
        &[
            stage_present(coord(0, 0), BlobHash([1; 32])),
            stage_present(coord(32, 0), BlobHash([2; 32])),
        ],
        None,
        &[fp0, fp1],
        &[],
    )
    .await
    .unwrap();

    let mut changed = fp0;
    changed.size += 1;
    let observed = [
        Observation {
            key: key(0, 0),
            fingerprint: changed,
        },
        Observation {
            key: key(1, 0),
            fingerprint: fp1,
        },
    ];
    let (previous, plan) = plan_backup(&*meta, &observed, &Scope::World).await.unwrap();
    assert_eq!(plan.carries, vec![key(1, 0)]);
    assert_eq!(plan.ingest, vec![key(0, 0)]);

    let staged = assemble(
        plan,
        &previous,
        vec![stage_present(coord(0, 0), BlobHash([9; 32]))],
        BTreeSet::from([coord(0, 0)]),
        1,
        vec![changed],
        &Scope::World,
    );
    let report = commit(meta, &previous, &staged, 2_000).await.unwrap();
    assert_eq!(report.carried_chunks, 1);
    assert_eq!(report.skipped_regions, 1);
    let kept = meta
        .lookup_chunk(report.snapshot, &coord(32, 0))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.blob, Some(BlobHash([2; 32])));
}

/// Vanished coordinates become tombstones; vanished regions drop state.
pub async fn tombstones<M>(meta: &mut M)
where
    M: MetaStore,
    M::Error: core::fmt::Debug,
{
    let fp = fingerprint(key(0, 0));
    meta.apply_snapshot_incremental(
        1_000,
        &[stage_present(coord(0, 0), BlobHash([1; 32]))],
        None,
        &[fp],
        &[],
    )
    .await
    .unwrap();
    let (previous, plan) = plan_backup(&*meta, &[], &Scope::World).await.unwrap();
    assert_eq!(plan.removed, vec![key(0, 0)]);
    let staged = assemble(
        plan,
        &previous,
        Vec::new(),
        BTreeSet::new(),
        0,
        Vec::new(),
        &Scope::World,
    );
    assert_eq!(staged.tombstones, 1);
    let report = commit(meta, &previous, &staged, 2_000).await.unwrap();
    assert_eq!(report.tombstones, 1);
    let tomb = meta
        .lookup_chunk(report.snapshot, &coord(0, 0))
        .await
        .unwrap()
        .unwrap();
    assert!(tomb.is_tombstone());
    let plan = plan_rollback(&*meta, report.snapshot).await.unwrap();
    assert!(plan.groups.is_empty());
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

/// Torn-backup simulation: an unreferenced blob is a GC orphan, a
/// referenced one survives planning and applying.
pub async fn torn_orphan_gc<M, C>(store: &mut sekai_storage::Store<M, C>)
where
    M: MetaStore,
    M::Error: core::fmt::Debug,
    C: BlobStore,
    C::Error: core::fmt::Debug,
{
    let live = sekai_core::hash_blob(b"live");
    let orphan = sekai_core::hash_blob(b"orphan");
    assert!(store.cas_mut().put(&live, b"live").await.unwrap());
    assert!(store.cas_mut().put(&orphan, b"orphan").await.unwrap());
    // Sync-then-commit order: blobs are durable before metadata names them.
    store.cas_mut().sync().await.unwrap();
    store
        .meta_mut()
        .apply_snapshot_incremental(
            1_000,
            &[SnapshotEntry::new(coord(0, 0), Some(live), None)],
            None,
            &[],
            &[],
        )
        .await
        .unwrap();

    let plan = gc_plan(store.cas(), store.meta()).await.unwrap();
    assert_eq!(plan.orphans(), &[orphan]);
    let (cas, meta) = store.cas_and_meta();
    let report = gc_apply(cas, meta, &plan, |_, _| {}).await.unwrap();
    assert_eq!(report.removed, 1);
    assert!(store.cas().contains(&live).await.unwrap());
    assert!(!store.cas().contains(&orphan).await.unwrap());
}
