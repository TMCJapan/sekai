-- Schema version 18: Git-modelled refs, worktrees, and reflogs
-- (ADR-0035). `refs` holds branches and tags keyed by full refname, and
-- each worktree carries its own HEAD. There is no migration path from
-- version 17: old stores are rejected via `UnsupportedSchema`.
--
-- Schema version 17: commits name content-addressed snapshot trees
-- (ADR-0027); `chunk_history` is gone. There is no migration path from
-- version 16: old stores are rejected via `UnsupportedSchema`.
--
-- Schema version 16: the `idx_parents_parent` index on `commit_parents`
-- was removed. It served the commit-retirement sweep, which no longer
-- exists; every remaining parent-edge query resolves through the primary
-- key.
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

-- Branches (`refs/heads/<name>`) and lightweight tags (`refs/tags/<name>`),
-- keyed by full Git refname. Every row names a commit: an unborn branch
-- has no row, only the `head_ref` of the worktree on it. `commit_id` has
-- no cascade: the default `NO ACTION` blocks deleting a commit a ref
-- still names. Names are byte-wise and case-sensitive, and namespace
-- queries select with GLOB or a range, never with LIKE, which ignores
-- case. Directory and file conflicts between names (`a` and `a/b`) are
-- refused by the application, which the schema cannot express.
CREATE TABLE refs(
    name TEXT NOT NULL COLLATE BINARY PRIMARY KEY,
    commit_id BLOB NOT NULL REFERENCES commits(id),
    CHECK (name GLOB 'refs/heads/?*' OR name GLOB 'refs/tags/?*')
) WITHOUT ROWID;

-- One row per world the store backs up, each with its own HEAD: attached
-- or unborn (`head_ref` names a branch, which may have no row yet) or
-- detached (`head_commit`). `head_ref` has no foreign key: an unborn
-- HEAD names a missing branch. A branch rename rewrites `head_ref` in
-- the application.
CREATE TABLE worktrees(
    name TEXT NOT NULL COLLATE BINARY PRIMARY KEY,
    path TEXT NULL UNIQUE,
    head_ref TEXT NULL CHECK (head_ref GLOB 'refs/heads/?*'),
    head_commit BLOB NULL REFERENCES commits(id),
    CHECK ((head_ref IS NULL) != (head_commit IS NULL))
) WITHOUT ROWID;

-- A NULL `path` marks the main worktree, and only one row may have it.
CREATE UNIQUE INDEX idx_worktrees_main
    ON worktrees((path IS NULL)) WHERE path IS NULL;

-- A branch is checked out by at most one worktree, unborn or not.
CREATE UNIQUE INDEX idx_worktrees_head_ref
    ON worktrees(head_ref) WHERE head_ref IS NOT NULL;

-- One reflog per branch. The cascades move the log with a rename and
-- drop it with the branch. A NULL `old_commit` marks a creation.
CREATE TABLE ref_log(
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL COLLATE BINARY
        REFERENCES refs(name) ON DELETE CASCADE ON UPDATE CASCADE
        CHECK (name GLOB 'refs/heads/?*'),
    old_commit BLOB NULL REFERENCES commits(id),
    new_commit BLOB NOT NULL REFERENCES commits(id),
    message TEXT NOT NULL,
    at_ms INTEGER NOT NULL
);

CREATE INDEX idx_ref_log_name ON ref_log(name, seq);

-- One HEAD reflog per worktree, dropped with its worktree. A NULL
-- `old_commit` marks HEAD's first commit.
CREATE TABLE head_log(
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    worktree TEXT NOT NULL
        REFERENCES worktrees(name) ON DELETE CASCADE ON UPDATE CASCADE,
    old_commit BLOB NULL REFERENCES commits(id),
    new_commit BLOB NOT NULL REFERENCES commits(id),
    message TEXT NOT NULL,
    at_ms INTEGER NOT NULL
);

CREATE INDEX idx_head_log_worktree ON head_log(worktree, seq);

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
