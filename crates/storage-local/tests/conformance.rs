//! Local backend conformance: shared flows plus filesystem-specific checks.

mod common;

use sekai_storage::{
    BlobStore as _, CacheStore as _, MetaStore as _, SekaiStore as _, SekaiStoreOpener,
};
use sekai_storage_local::{LocalStore, Worktree};
use sekai_util::{
    ChunkCoord, ChunkHash, CommitEntry, Dimension, Head, RefName, RegionHash, RegionKind,
};
use std::path::Path;

/// A temp dir whose name starts with the calling test's name (tempfile
/// appends a random suffix), removed when its guard is dropped at the end
/// of the test, panics included.
fn tempdir(name: &str) -> (std::path::PathBuf, tempfile::TempDir) {
    let guard = tempfile::Builder::new()
        .prefix(&format!("sekai-{name}-"))
        .tempdir()
        .unwrap();
    (guard.path().to_path_buf(), guard)
}

async fn init(dir: impl AsRef<Path>) -> sekai_storage_local::LocalStore {
    LocalStore::init(dir).await.unwrap()
}

#[tokio::test]
async fn sqlite_commit_lifecycle() {
    let (dir, _guard) = tempdir("lifecycle");
    let mut store = init(&dir).await;
    common::commit_lifecycle(store.meta_mut()).await;
}

#[tokio::test]
async fn sqlite_scope_flow() {
    let (dir, _guard) = tempdir("scopes");
    let mut store = init(&dir).await;
    common::scope_flow(store.meta_mut()).await;
}

#[tokio::test]
async fn sqlite_fresh_stats() {
    let (dir, _guard) = tempdir("fresh");
    let mut store = init(&dir).await;
    common::fresh_stats(store.meta_mut()).await;
}

#[tokio::test]
async fn sqlite_tree_bulk_reads() {
    let (dir, _guard) = tempdir("bulk-reads");
    let mut store = init(&dir).await;
    common::tree_bulk_reads(store.meta_mut()).await;
}

/// A vanished coordinate stops resolving: absence is the tombstone, and the
/// snapshot walk a checkout plan is built from drops the coordinate instead
/// of asking for a blob.
#[tokio::test]
async fn sqlite_tombstones_shape_and_walk_drop() {
    let (dir, _guard) = tempdir("tombstones");
    let mut store = init(&dir).await;
    let meta = store.meta_mut();
    let present = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
    let gone = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 1, 0);
    let first = meta
        .apply_commit_incremental(
            1_000,
            &[],
            &[
                CommitEntry::new(present, Some(ChunkHash::new([1; 32]))),
                CommitEntry::new(gone, Some(ChunkHash::new([2; 32]))),
            ],
        )
        .await
        .unwrap()
        .id;
    let second = meta
        .apply_commit_incremental(2_000, &[first], &[CommitEntry::new(gone, None)])
        .await
        .unwrap()
        .id;

    assert!(meta.lookup_chunk(second, &gone).await.unwrap().is_none());
    assert_eq!(
        meta.lookup_chunk(second, &present).await.unwrap(),
        Some(CommitEntry::new(present, Some(ChunkHash::new([1; 32]))))
    );

    let mut visited = Vec::new();
    meta.visit_commit_chunks(second, |entry| {
        visited.push(*entry);
        true
    })
    .await
    .unwrap();
    assert_eq!(
        visited,
        vec![CommitEntry::new(present, Some(ChunkHash::new([1; 32])))]
    );
}

#[tokio::test]
async fn sqlite_history_graph() {
    let (dir, _guard) = tempdir("graph");
    let mut store = init(&dir).await;
    common::history_graph(store.meta_mut()).await;
}

#[tokio::test]
async fn sqlite_cas_roundtrip() {
    let (dir, _guard) = tempdir("cas");
    let mut store = init(&dir).await;
    common::cas_roundtrip(store.blob_mut()).await;
}

#[tokio::test]
async fn cache_roundtrip() {
    let (dir, _guard) = tempdir("cache");
    let mut store = init(&dir).await;
    common::cache_roundtrip(store.cache_mut()).await;
}

#[tokio::test]
async fn sqlite_torn_commit_leaves_orphans() {
    let (dir, _guard) = tempdir("torn-commit");
    let mut store = init(&dir).await;
    common::torn_commit_leaves_orphans(&mut store).await;
}

#[tokio::test]
async fn visit_lists_stored_hashes_and_missing_fetch_clears() {
    let (dir, _guard) = tempdir("visit");
    let mut store = init(&dir).await;
    let blobs = store.blob_mut();
    let hash = ChunkHash::new(sekai_util::hash_bytes(b"real"));
    assert!(blobs.put(&hash, b"real").await.unwrap());
    // The keyspace has no file names to skip, so visiting every key is the
    // whole story; a missing hash is a read error, not an ignorable name.
    let mut seen = Vec::new();
    blobs
        .visit_blobs(|h| {
            seen.push(*h);
            true
        })
        .await
        .unwrap();
    assert_eq!(seen, vec![hash]);
    let mut out = vec![1u8, 2, 3];
    let err = blobs
        .fetch_into(&ChunkHash::new([9; 32]), &mut out)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        sekai_storage::StorageError::BlobMissing { .. }
    ));
    assert!(out.is_empty(), "a failed fetch clears the buffer");
}

/// The blob keyspace is a sled directory beside `meta.sqlite`, its default
/// tree keyed by the blob hashes alone.
#[tokio::test]
async fn blob_database_is_created() {
    let (dir, _guard) = tempdir("blob-created");
    let mut store = init(&dir).await;
    let hash = ChunkHash::new(sekai_util::hash_bytes(b"payload"));
    assert!(store.blob_mut().put(&hash, b"payload").await.unwrap());
    store.blob_mut().sync().await.unwrap();
    drop(store);

    let blob_path = dir.join("blobs");
    assert!(blob_path.is_dir());
    let db = sled::open(&blob_path).unwrap();
    assert_eq!(
        db.get(hash.as_bytes()).unwrap().unwrap().as_ref(),
        b"payload"
    );
    assert_eq!(db.tree_names().len(), 1);
    drop(db);
    let _store = LocalStore::open(&dir).await.unwrap();
}

/// The cache keyspace is a sled directory beside `meta.sqlite`, the image
/// in the default tree and its hash in a side tree so a hash lookup never
/// reads image bytes.
#[tokio::test]
async fn cache_database_is_created() {
    let (dir, _guard) = tempdir("cache-created");
    let mut store = init(&dir).await;
    let region = RegionHash::new([7; 32]);
    let image = b"mca image";
    let mca_hash = sekai_util::hash_bytes(image);
    assert!(
        store
            .cache_mut()
            .put(region, mca_hash, image)
            .await
            .unwrap()
    );
    store.cache_mut().sync().await.unwrap();
    drop(store);

    let cache_path = dir.join("cache");
    assert!(cache_path.is_dir());
    let db = sled::open(&cache_path).unwrap();
    assert_eq!(db.get(region.as_bytes()).unwrap().unwrap().as_ref(), image);
    let hashes = db.open_tree("mca_hashes").unwrap();
    assert_eq!(
        hashes.get(region.as_bytes()).unwrap().unwrap().as_ref(),
        &mca_hash[..]
    );
    assert_eq!(db.tree_names().len(), 2);
    drop(hashes);
    drop(db);
    let _store = LocalStore::open(&dir).await.unwrap();
}

/// A wiped cache stays openable: `clear` removes entries, never the
/// keyspace.
#[tokio::test]
async fn cache_clear_keeps_the_keyspace_openable() {
    let (dir, _guard) = tempdir("cache-clear");
    let mut store = init(&dir).await;
    let region = RegionHash::new([7; 32]);
    let image = b"mca image";
    assert!(
        store
            .cache_mut()
            .put(region, sekai_util::hash_bytes(image), image)
            .await
            .unwrap()
    );
    assert_eq!(store.cache_mut().clear().await.unwrap(), 1);
    drop(store);

    let store = LocalStore::open(&dir).await.unwrap();
    assert_eq!(store.cache().mca_hash(region).await.unwrap(), None);
}

/// A store from before the cache existed is refused, not silently
/// upgraded: pre-release policy recreates instead of migrating.
#[tokio::test]
async fn open_of_a_pre_cache_store_fails() {
    let (dir, _guard) = tempdir("pre-cache");
    let store = init(&dir).await;
    drop(store);
    std::fs::remove_dir_all(dir.join("cache")).unwrap();
    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(err, sekai_storage::StorageError::StoreMissing { .. }),
        "unexpected error: {err}"
    );
}

/// Init materializes `version` with the current `STORE_VERSION`, and a
/// re-init keeps the existing file instead of rewriting it.
#[tokio::test]
async fn init_writes_version_and_reinit_keeps_it() {
    let (dir, _guard) = tempdir("version-init");
    let store = init(&dir).await;
    drop(store);
    let version = dir.join("version");
    assert_eq!(
        std::fs::read_to_string(&version).unwrap(),
        format!("{}\n", sekai_storage_local::STORE_VERSION)
    );

    // A spelling only a preserved file can carry (no trailing newline):
    // re-init must not rewrite it.
    let kept = sekai_storage_local::STORE_VERSION.to_string();
    std::fs::write(&version, &kept).unwrap();
    let store = LocalStore::init(&dir).await.unwrap();
    drop(store);
    assert_eq!(std::fs::read_to_string(&version).unwrap(), kept);
}

/// A future store version is refused loudly instead of reinterpreted.
#[tokio::test]
async fn open_gates_the_store_version() {
    let (dir, _guard) = tempdir("version-gate");
    let store = init(&dir).await;
    drop(store);
    std::fs::write(dir.join("version"), "99\n").unwrap();
    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(
            err,
            sekai_storage::StorageError::UnsupportedSchema { found: 99, supported }
                if supported == i64::from(sekai_storage_local::STORE_VERSION)
        ),
        "unexpected error: {err}"
    );
}

/// A store from before worktrees were registered (version 2) is refused,
/// not migrated.
#[tokio::test]
async fn open_refuses_a_version_2_store() {
    let (dir, _guard) = tempdir("version-2");
    let store = init(&dir).await;
    drop(store);
    std::fs::write(dir.join("version"), "2\n").unwrap();
    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(
            err,
            sekai_storage::StorageError::UnsupportedSchema { found: 2, .. }
        ),
        "unexpected error: {err}"
    );
}

/// A store from before refs were keyed by full refname (version 1) is
/// refused, not migrated.
#[tokio::test]
async fn open_refuses_a_version_1_store() {
    let (dir, _guard) = tempdir("version-1");
    let store = init(&dir).await;
    drop(store);
    std::fs::write(dir.join("version"), "1\n").unwrap();
    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(
            err,
            sekai_storage::StorageError::UnsupportedSchema { found: 1, .. }
        ),
        "unexpected error: {err}"
    );
}

/// Re-initializing a store of another version is refused before any
/// database opens, as open refuses it.
#[tokio::test]
async fn reinit_gates_the_store_version() {
    let (dir, _guard) = tempdir("version-reinit");
    let store = init(&dir).await;
    drop(store);
    std::fs::write(dir.join("version"), "99\n").unwrap();
    let err = LocalStore::init(&dir).await.unwrap_err();
    assert!(
        matches!(
            err,
            sekai_storage::StorageError::UnsupportedSchema { found: 99, .. }
        ),
        "unexpected error: {err}"
    );
}

/// A malformed version file fails loudly instead of being reinterpreted.
#[tokio::test]
async fn open_of_a_malformed_version_fails() {
    let (dir, _guard) = tempdir("version-malformed");
    let store = init(&dir).await;
    drop(store);
    std::fs::write(dir.join("version"), "one\n").unwrap();
    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(
            err,
            sekai_storage::StorageError::Backend {
                backend: "version file",
                ..
            }
        ),
        "unexpected error: {err}"
    );
}

/// `version` is part of the layout markers: a store without one is not
/// openable, and nothing is opened or created to find that out.
#[tokio::test]
async fn open_of_a_store_without_version_fails() {
    let (dir, _guard) = tempdir("version-missing");
    let store = init(&dir).await;
    drop(store);
    std::fs::remove_file(dir.join("version")).unwrap();
    assert!(!sekai_storage_local::local_store_exists(&dir));
    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(err, sekai_storage::StorageError::StoreMissing { .. }),
        "unexpected error: {err}"
    );
}

/// sled admits a single opener per keyspace: a second open fails while the
/// first handle lives, and succeeds once it is gone.
#[tokio::test]
async fn a_live_blob_keyspace_refuses_a_second_opener() {
    let (dir, _guard) = tempdir("exclusive");
    let store = init(&dir).await;
    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(
            err,
            sekai_storage::StorageError::Backend {
                backend: "sled",
                ..
            }
        ),
        "unexpected error: {err}"
    );
    drop(store);
    let _store = LocalStore::open(&dir).await.unwrap();
}

/// Only `LocalStore::init` materializes a store; `LocalStore::open` must fail loudly on
/// a root that was never initialized and leave nothing behind (issue #80).
#[tokio::test]
async fn open_of_a_missing_store_fails_without_creating_it() {
    let (base, _guard) = tempdir("missing");
    let dir = base.join("store");
    assert!(!sekai_storage_local::local_store_exists(&dir));
    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(&err, sekai_storage::StorageError::StoreMissing { path } if Path::new(path) == dir),
        "unexpected error: {err}"
    );
    assert!(!dir.exists(), "a failed open must not create anything");

    let store = LocalStore::init(&dir).await.unwrap();
    assert!(dir.join("version").is_file());
    assert!(dir.join("meta.sqlite").is_file());
    assert!(dir.join("blobs/conf").is_file());
    assert!(dir.join("cache/conf").is_file());
    assert!(sekai_storage_local::local_store_exists(&dir));
    // The keyspace admits one opener: release the init handle first.
    drop(store);
    let _store = LocalStore::open(&dir).await.unwrap();
}

/// The opener trait accepts any `AsRef<Path>` holder, not just strings, and
/// keeps the open/init exclusivity of the free functions (issue #80).
#[tokio::test]
async fn opener_trait_takes_path_like_locations() {
    let (base, _guard) = tempdir("opener-trait");
    let dir = base.join("store");

    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(&err, sekai_storage::StorageError::StoreMissing { path } if Path::new(path) == dir),
        "unexpected error: {err}"
    );
    assert!(!dir.exists(), "a failed open must not create anything");

    let store = LocalStore::init(dir.clone()).await.unwrap();
    assert!(sekai_storage_local::local_store_exists(&dir));
    // The keyspace admits one opener: release the init handle first.
    drop(store);

    let url = dir.to_string_lossy().into_owned();
    let store = LocalStore::open(url.as_str()).await.unwrap();
    drop(store);
    let _store = LocalStore::open(dir.as_path()).await.unwrap();
}

/// A plain file at the store location is a link: its content names the real
/// root, and every component (meta, blobs, cache) resolves from there. The
/// link shares the target's keyspace exclusion, and nothing is created
/// beside the link.
#[tokio::test]
async fn open_follows_a_store_link_file() {
    let (base, _guard) = tempdir("store-link");
    let real = base.join("real-store");
    let mut store = init(&real).await;
    let hash = ChunkHash::new(sekai_util::hash_bytes(b"payload"));
    assert!(store.blob_mut().put(&hash, b"payload").await.unwrap());
    store.blob_mut().sync().await.unwrap();
    drop(store);

    let world = base.join("world");
    std::fs::create_dir_all(&world).unwrap();
    let link = world.join(".sekai");
    std::fs::write(&link, format!("{}\n", real.display())).unwrap();

    let mut store = LocalStore::open(&link).await.unwrap();
    // The same keyspace: the real root refuses a second opener while the
    // link handle lives.
    let err = LocalStore::open(&real).await.unwrap_err();
    assert!(
        matches!(
            err,
            sekai_storage::StorageError::Backend {
                backend: "sled",
                ..
            }
        ),
        "unexpected error: {err}"
    );
    let mut out = Vec::new();
    store.blob_mut().fetch_into(&hash, &mut out).await.unwrap();
    assert_eq!(out, b"payload");
    drop(store);

    assert!(!world.join("version").exists());
    assert!(!world.join("meta.sqlite").exists());
    let _store = LocalStore::open(&real).await.unwrap();
}

/// A relative link target resolves against the link's parent directory, not
/// the process working directory, so a moved folder keeps working.
#[tokio::test]
async fn open_resolves_a_relative_store_link_against_its_parent() {
    let (base, _guard) = tempdir("store-link-relative");
    let real = base.join("real-store");
    let store = init(&real).await;
    drop(store);

    let world = base.join("world");
    std::fs::create_dir_all(&world).unwrap();
    let link = world.join(".sekai");
    std::fs::write(&link, "../real-store").unwrap();
    let _store = LocalStore::open(&link).await.unwrap();
}

/// An empty link file fails loudly instead of opening the link's own folder
/// or the working directory.
#[tokio::test]
async fn open_of_an_empty_store_link_fails() {
    let (base, _guard) = tempdir("store-link-empty");
    let link = base.join(".sekai");
    std::fs::write(&link, "  \n").unwrap();
    let err = LocalStore::open(&link).await.unwrap_err();
    assert!(
        matches!(
            err,
            sekai_storage::StorageError::Backend {
                backend: "store link",
                ..
            }
        ),
        "unexpected error: {err}"
    );
}

/// A link to an uninitialized root reports the resolved target and creates
/// nothing, as a direct open of a missing store does.
#[tokio::test]
async fn open_of_a_store_link_to_a_missing_root_fails_without_creating_it() {
    let (base, _guard) = tempdir("store-link-missing");
    let target = base.join("real-store");
    let link = base.join(".sekai");
    std::fs::write(&link, format!("{}\n", target.display())).unwrap();
    let err = LocalStore::open(&link).await.unwrap_err();
    assert!(
        matches!(&err, sekai_storage::StorageError::StoreMissing { path } if Path::new(path) == target),
        "unexpected error: {err}"
    );
    assert!(!target.exists(), "a failed open must not create anything");
}

/// A link is followed once: a target that is itself a link file is not a
/// store root, so the open fails instead of chasing the chain.
#[tokio::test]
async fn open_does_not_chain_store_links() {
    let (base, _guard) = tempdir("store-link-chain");
    let real = base.join("real-store");
    let store = init(&real).await;
    drop(store);
    let second = base.join("second-link");
    std::fs::write(&second, format!("{}\n", real.display())).unwrap();
    let first = base.join("first-link");
    std::fs::write(&first, format!("{}\n", second.display())).unwrap();

    let err = LocalStore::open(&first).await.unwrap_err();
    assert!(
        matches!(&err, sekai_storage::StorageError::StoreMissing { path } if Path::new(path) == second),
        "unexpected error: {err}"
    );
}

/// The version gate runs on the resolved root: a link cannot smuggle an
/// unsupported store past the check.
#[tokio::test]
async fn open_of_a_store_link_gates_the_target_version() {
    let (base, _guard) = tempdir("store-link-version");
    let real = base.join("real-store");
    let store = init(&real).await;
    drop(store);
    std::fs::write(real.join("version"), "99\n").unwrap();
    let link = base.join(".sekai");
    std::fs::write(&link, format!("{}\n", real.display())).unwrap();
    let err = LocalStore::open(&link).await.unwrap_err();
    assert!(
        matches!(
            err,
            sekai_storage::StorageError::UnsupportedSchema { found: 99, .. }
        ),
        "unexpected error: {err}"
    );
}

/// A link whose content is not UTF-8 fails as an I/O error naming the link,
/// never as a lossy path.
#[tokio::test]
async fn open_of_a_non_utf8_store_link_fails() {
    let (base, _guard) = tempdir("store-link-non-utf8");
    let link = base.join(".sekai");
    std::fs::write(&link, [0xff, 0xfe]).unwrap();
    let err = LocalStore::open(&link).await.unwrap_err();
    assert!(
        matches!(&err, sekai_storage::StorageError::Io { path, .. } if Path::new(path) == link),
        "unexpected error: {err}"
    );
}

/// The HEAD a fresh worktree starts on: unborn, on the default branch.
fn on_main() -> Head {
    Head::Branch(RefName::default_branch())
}

fn on_branch(raw: &str) -> Head {
    Head::Branch(RefName::parse(raw).unwrap())
}

/// The store's own worktree row: init registers the canonical store root
/// with HEAD unborn on the default branch.
fn main_worktree(root: &Path) -> Worktree {
    Worktree {
        path: std::fs::canonicalize(root).unwrap(),
        head: on_main(),
    }
}

/// `add_worktree` writes a store link naming the store's absolute root,
/// registers the link path with its HEAD, and open follows that link
/// back to the same store.
#[tokio::test]
async fn add_worktree_writes_a_link_open_follows() {
    let (base, _guard) = tempdir("worktree-add");
    let real = base.join("real-store");
    let store = init(&real).await;

    let world = base.join("world");
    std::fs::create_dir_all(&world).unwrap();
    let link = world.join(".sekai");
    store
        .add_worktree(&link, on_branch("feature"))
        .await
        .unwrap();

    let content = std::fs::read_to_string(&link).unwrap();
    let canonical = std::fs::canonicalize(&real).unwrap();
    assert_eq!(content.trim(), canonical.to_str().unwrap());
    let expected = [
        main_worktree(&real),
        Worktree {
            path: link.clone(),
            head: on_branch("feature"),
        },
    ];
    assert_eq!(store.worktrees().await.unwrap(), expected);
    drop(store);
    let store = LocalStore::open(&link).await.unwrap();
    assert_eq!(store.worktrees().await.unwrap(), expected);
}

/// Registering a worktree never clobbers what is already at the location,
/// and the worktree directory is not materialized: a missing parent fails
/// without creating anything. A refused registration writes no row.
#[tokio::test]
async fn add_worktree_refuses_existing_and_missing_locations() {
    let (base, _guard) = tempdir("worktree-refusal");
    let real = base.join("real-store");
    let store = init(&real).await;

    let link = base.join(".sekai");
    std::fs::write(&link, "keep me").unwrap();
    let err = store.add_worktree(&link, on_main()).await.unwrap_err();
    assert!(
        matches!(err, sekai_storage::StorageError::Io { .. }),
        "unexpected error: {err}"
    );
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "keep me");

    let err = store.add_worktree(&real, on_main()).await.unwrap_err();
    assert!(
        matches!(err, sekai_storage::StorageError::Io { .. }),
        "unexpected error: {err}"
    );

    let missing = base.join("missing").join(".sekai");
    let err = store.add_worktree(&missing, on_main()).await.unwrap_err();
    assert!(
        matches!(err, sekai_storage::StorageError::Io { .. }),
        "unexpected error: {err}"
    );
    assert!(!base.join("missing").exists());
    assert_eq!(store.worktrees().await.unwrap(), [main_worktree(&real)]);
}

/// A branch is the HEAD of at most one worktree: the store itself holds
/// `main`, and a second registration on any already-checked-out branch
/// is refused and writes no row.
#[tokio::test]
async fn add_worktree_refuses_a_branch_already_checked_out() {
    let (base, _guard) = tempdir("worktree-head-clash");
    let store = init(base.join("real-store")).await;

    let first = base.join("first");
    std::fs::create_dir_all(&first).unwrap();
    store
        .add_worktree(first.join(".sekai"), on_branch("feature"))
        .await
        .unwrap();

    for (name, head) in [("second", on_branch("feature")), ("third", on_main())] {
        let world = base.join(name);
        std::fs::create_dir_all(&world).unwrap();
        let err = store
            .add_worktree(world.join(".sekai"), head)
            .await
            .unwrap_err();
        assert!(
            matches!(err, sekai_storage::StorageError::Backend { .. }),
            "unexpected error: {err}"
        );
    }
    assert_eq!(store.worktrees().await.unwrap().len(), 2);
}

/// Detached HEADs may share a commit: only branch HEADs are exclusive.
#[tokio::test]
async fn add_worktree_allows_detached_heads_on_one_commit() {
    let (base, _guard) = tempdir("worktree-detached");
    let mut store = init(base.join("real-store")).await;
    let commit = store
        .meta_mut()
        .apply_commit(1_000, &[], &[])
        .await
        .unwrap();

    for name in ["a", "b"] {
        let world = base.join(name);
        std::fs::create_dir_all(&world).unwrap();
        store
            .add_worktree(world.join(".sekai"), Head::Detached(commit))
            .await
            .unwrap();
    }
    assert_eq!(store.worktrees().await.unwrap().len(), 3);
}

/// Init registers the store itself as its main worktree, keyed by the
/// canonical store root with HEAD unborn on `main`; a re-init keeps the
/// row and its HEAD.
#[tokio::test]
async fn init_registers_the_store_as_its_own_worktree() {
    let (dir, _guard) = tempdir("worktree-init");
    let store = init(&dir).await;
    let expected = [main_worktree(&dir)];
    assert_eq!(store.worktrees().await.unwrap(), expected);
    drop(store);

    let store = LocalStore::init(&dir).await.unwrap();
    assert_eq!(store.worktrees().await.unwrap(), expected);
}

/// A store location that is not valid UTF-8 fails loudly before any link
/// is written, never as a lossy path.
#[cfg(unix)]
#[tokio::test]
async fn add_worktree_refuses_a_non_utf8_link_path() {
    use std::os::unix::ffi::OsStringExt as _;

    let (base, _guard) = tempdir("worktree-non-utf8");
    let real = base.join("real-store");
    let store = init(&real).await;
    let world = base.join(std::ffi::OsString::from_vec(vec![0xff, 0xfe]));
    std::fs::create_dir_all(&world).unwrap();
    let link = world.join(".sekai");

    let err = store
        .add_worktree(&link, on_branch("feature"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            sekai_storage::StorageError::Backend {
                backend: "worktree",
                ..
            }
        ),
        "unexpected error: {err}"
    );
    assert!(!link.exists());
    assert_eq!(store.worktrees().await.unwrap(), [main_worktree(&real)]);
}
