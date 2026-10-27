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
-- Content-addressed commits live below. `id` is the public commit hash;
-- `seq` is a local ordering key used by MVCC queries. The sequence is never
-- part of the commit identity and may differ across replicas. Reads order
-- by it, so it must increase monotonically with record order inside a
-- store.
CREATE TABLE commits(
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id BLOB NOT NULL UNIQUE,
    created_at_ms INTEGER NOT NULL
);

-- History graph: which commits a commit continues. The root of a
-- history has no row, an ordinary commit one, a merge several. The
-- graph - not the local record order - decides what a commit inherits.
-- `parent_id` has no cascade on purpose: deleting a commit that
-- children still point at fails loudly, so a retirement rewires the
-- children before it drops the row.
CREATE TABLE commit_parents(
    child_id BLOB NOT NULL REFERENCES commits(id) ON DELETE CASCADE,
    parent_id BLOB NOT NULL REFERENCES commits(id),
    PRIMARY KEY(child_id, parent_id)
);

-- Serves the retirement sweep that drops the edges naming a retired
-- commit; child lookups use the primary key.
CREATE INDEX idx_parents_parent
    ON commit_parents(parent_id);

CREATE TABLE chunk_history(
    commit_id BLOB NOT NULL REFERENCES commits(id),
    seq INTEGER NOT NULL,
    dim INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    cx INTEGER NOT NULL,
    cz INTEGER NOT NULL,
    blob BLOB NULL,
    diff BLOB NULL,
    PRIMARY KEY(commit_id, dim, kind, cx, cz)
);

CREATE INDEX idx_history_coord
    ON chunk_history(dim, kind, cx, cz, seq);

-- Named, movable pointers to commits: the store's branch model. A ref
-- whose `commit_id` is NULL is unborn - it names a history without a
-- commit yet. At most one ref is active, enforced by the partial index
-- below; the application keeps exactly one, because new commits hang
-- from it (the store's HEAD). `commit_id` has no cascade: the default
-- `NO ACTION` blocks deleting a commit a ref still names, so
-- `retire_commit` re-points refs onto the successor before the row goes.
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

-- Named scopes (ADR-0021): store-saved world areas selected by name. A
-- scope names no commit, so history operations never touch these rows.
-- `kind` is a bitmask of `1 << RegionKind::raw()`; NULL selects every
-- region family and the full set is always spelled NULL, so the CHECK
-- admits proper subsets only. Names order byte-wise, as refs do.
CREATE TABLE scopes(
    name TEXT NOT NULL COLLATE BINARY PRIMARY KEY,
    kind INTEGER NULL CHECK (kind BETWEEN 1 AND 6),
    created_at_ms INTEGER NOT NULL
);

-- One inclusive, normalized chunk rectangle per row; the scope is the
-- union of its rows. The primary key collapses identical areas and
-- yields the stored order. A scope without rows is corruption.
CREATE TABLE scope_areas(
    scope TEXT NOT NULL REFERENCES scopes(name) ON DELETE CASCADE,
    dim INTEGER NOT NULL,
    x0 INTEGER NOT NULL,
    z0 INTEGER NOT NULL,
    x1 INTEGER NOT NULL,
    z1 INTEGER NOT NULL,
    CHECK (x0 <= x1 AND z0 <= z1),
    PRIMARY KEY (scope, dim, x0, z0, x1, z1)
) WITHOUT ROWID;
