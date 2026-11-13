-- Schema version 16: commits name content-addressed snapshot trees
-- (ADR-0027); `chunk_history` is gone. The `idx_parents_parent` index on
-- `commit_parents` is gone too: it served the commit-retirement sweep,
-- which no longer exists, and every parent-edge query resolves through
-- the primary key. There is no migration path from version 15: old stores
-- are rejected via `UnsupportedSchema`.
--
-- Schema version 15: the `diff` column of `chunk_history` (cached volatile
-- diff hashes) was removed. Change detection compares per-chunk history
-- directly, so no derived column remains. There is no migration path from
-- version 14: old stores are rejected via `UnsupportedSchema`.
--
-- Schema version 14: named scopes (`scopes`, `scope_areas`) were added.
-- There is no migration path from version 13: old stores are rejected via
-- `UnsupportedSchema`.
--
-- Schema version 13: `region_state` (persisted region fingerprints) was
-- removed. Commit and checkout decisions now compare per-chunk history
-- directly, so no derived table remains. There is no migration path from
-- version 12: old stores are rejected via `UnsupportedSchema`.
--
-- Schema version 12: `commit_tags` (snapshot tag references) was removed in
-- favour of refs. That step also had no migration path.
--
-- Snapshot trees. Each row stores the exact canonical entry array that the
-- node hash covers, so verification is one hash call. There are no keys
-- between levels: the edges live inside the blobs. Nodes are append-only
-- and shared by hash. A node's entries name only the next level down.
CREATE TABLE root_tree(
    hash BLOB NOT NULL PRIMARY KEY,
    node BLOB NOT NULL
);

CREATE TABLE dim_tree(
    hash BLOB NOT NULL PRIMARY KEY,
    node BLOB NOT NULL
);

CREATE TABLE region_tree(
    hash BLOB NOT NULL PRIMARY KEY,
    node BLOB NOT NULL
);

-- Content-addressed commits live below. `id` is the public commit hash;
-- `seq` is a local ordering key. The sequence is never part of the commit
-- identity and may differ across replicas. Reads order by it, so it must
-- increase monotonically with record order inside a store. `root_hash`
-- anchors the commit to one complete snapshot tree.
CREATE TABLE commits(
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id BLOB NOT NULL UNIQUE,
    created_at_ms INTEGER NOT NULL,
    root_hash BLOB NOT NULL REFERENCES root_tree(hash)
);

-- History graph: which commits a commit continues. The root of a
-- history has no row, an ordinary commit one, a merge several. The
-- graph - not the local record order - decides what a commit inherits.
-- `parent_id` has no cascade on purpose: deleting a commit that
-- children still point at fails loudly, keeping the graph consistent.
CREATE TABLE commit_parents(
    child_id BLOB NOT NULL REFERENCES commits(id) ON DELETE CASCADE,
    parent_id BLOB NOT NULL REFERENCES commits(id),
    PRIMARY KEY(child_id, parent_id)
);

-- Named, movable pointers to commits: the store's branch model. A ref
-- whose `commit_id` is NULL is unborn - it names a history without a
-- commit yet. At most one ref is active, enforced by the partial index
-- below; the application keeps exactly one, because new commits hang
-- from it (the store's HEAD). `commit_id` has no cascade: the default
-- `NO ACTION` blocks deleting a commit a ref still names.
-- Names are byte-wise and case-sensitive: `main` and `Main` are distinct,
-- and `ORDER BY name` follows that byte order. Only the reserved `HEAD`
-- spelling in the validator ignores case.
CREATE TABLE refs(
    name TEXT NOT NULL COLLATE BINARY PRIMARY KEY,
    commit_id BLOB NULL REFERENCES commits(id),
    active INTEGER NOT NULL DEFAULT 0 CHECK (active IN (0, 1)),
    created_at_ms INTEGER NOT NULL
);

-- At most one active ref: the partial index covers the active rows only.
CREATE UNIQUE INDEX idx_refs_active
    ON refs(active) WHERE active = 1;

-- Named scopes: store-saved world areas selected by name. A scope
-- names no commit, so history operations never touch these rows. It
-- always covers every region family, so no kind is stored. Names order
-- byte-wise, as refs do. `AUTOINCREMENT` keeps a deleted scope's `id`
-- from being handed to a later one.
CREATE TABLE scopes(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL COLLATE BINARY UNIQUE,
    created_at_ms INTEGER NOT NULL
);

-- One inclusive, normalized chunk rectangle per row; the scope is the
-- union of its rows. Writers store each area once and readers order
-- them by `(dim, x0, z0, x1, z1)` when decoding, so queries need no
-- ORDER BY. A scope without rows is corruption.
CREATE TABLE scope_areas(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scope INTEGER NOT NULL REFERENCES scopes(id) ON DELETE CASCADE,
    dim INTEGER NOT NULL,
    x0 INTEGER NOT NULL,
    z0 INTEGER NOT NULL,
    x1 INTEGER NOT NULL,
    z1 INTEGER NOT NULL,
    CHECK (x0 <= x1 AND z0 <= z1)
);

-- Serves reading a scope's areas, swapping them on replacement, and the
-- cascade on delete; without it each is a scan of every scope's areas.
CREATE INDEX idx_scope_areas_scope
    ON scope_areas(scope);