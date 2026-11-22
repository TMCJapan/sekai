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

-- Named, movable pointers to commits: the store's branch model. Every row
-- names a commit: an unborn branch has no row and exists only as a
-- worktree's `head_ref`. `commit_id` has no cascade: the default
-- `NO ACTION` blocks deleting a commit a ref still names.
-- Names are full Git refnames (`refs/heads/main`), byte-wise and
-- case-sensitive: `main` and `Main` are distinct, and `ORDER BY name`
-- follows that byte order. Only the reserved `HEAD` spelling in the
-- validator ignores case.
CREATE TABLE refs(
    name TEXT NOT NULL COLLATE BINARY PRIMARY KEY
        CHECK (name GLOB 'refs/heads/?*'),
    commit_id BLOB NOT NULL REFERENCES commits(id)
) WITHOUT ROWID;

-- The worlds a store backs up, keyed by absolute world root. Each row
-- holds its own HEAD: `head_ref` names a branch (born or unborn), or
-- `head_commit` holds a commit directly (detached); exactly one is set.
-- `head_ref` is no foreign key, because a branch a HEAD names need not
-- exist yet. At most one row is the main worktree, and a branch is
-- checked out by at most one worktree, enforced by the partial indexes
-- below.
CREATE TABLE worktrees(
    path TEXT NOT NULL COLLATE BINARY PRIMARY KEY,
    main INTEGER NOT NULL CHECK (main IN (0, 1)),
    head_ref TEXT NULL CHECK (head_ref GLOB 'refs/heads/?*'),
    head_commit BLOB NULL REFERENCES commits(id),
    CHECK ((head_ref IS NULL) != (head_commit IS NULL))
) WITHOUT ROWID;

CREATE UNIQUE INDEX idx_worktrees_main
    ON worktrees(main) WHERE main;

CREATE UNIQUE INDEX idx_worktrees_head_ref
    ON worktrees(head_ref) WHERE head_ref IS NOT NULL;

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
