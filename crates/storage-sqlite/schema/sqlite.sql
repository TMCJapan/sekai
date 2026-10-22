CREATE TABLE commits(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms INTEGER NOT NULL
);

-- History graph: which commits a commit continues. The root of a
-- history has no row, an ordinary commit one, a merge several. The
-- graph - not the identifier order - decides what a commit inherits.
-- `parent_id` has no cascade on purpose: deleting a commit that
-- children still point at fails loudly, so a retirement rewires the
-- children before it drops the row.
CREATE TABLE commit_parents(
    child_id INTEGER NOT NULL REFERENCES commits(id) ON DELETE CASCADE,
    parent_id INTEGER NOT NULL REFERENCES commits(id),
    PRIMARY KEY(child_id, parent_id)
);

-- Serves the retirement sweep that drops the edges naming a retired
-- commit; child lookups use the primary key.
CREATE INDEX idx_parents_parent
    ON commit_parents(parent_id);

CREATE TABLE chunk_history(
    commit_id INTEGER NOT NULL REFERENCES commits(id),
    dim INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    cx INTEGER NOT NULL,
    cz INTEGER NOT NULL,
    blob BLOB NULL,
    diff BLOB NULL,
    PRIMARY KEY(commit_id, dim, kind, cx, cz)
);

CREATE INDEX idx_history_coord
    ON chunk_history(dim, kind, cx, cz, commit_id);

-- Derived per-region fingerprints: which commit last confirmed each
-- file and what the file looked like. Re-observed from live world files
-- when wiped, so they never need data migration.
CREATE TABLE region_state(
    dim INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    rx INTEGER NOT NULL,
    rz INTEGER NOT NULL,
    mtime_ms INTEGER NULL,
    size INTEGER NOT NULL,
    content_hash BLOB NOT NULL,
    commit_id INTEGER NOT NULL REFERENCES commits(id),
    PRIMARY KEY(dim, kind, rx, rz)
);

-- Human-readable commit aliases. Tags die with their commit
-- (`ON DELETE CASCADE`): pruning a tagged commit drops its tags.
CREATE TABLE commit_tags(
    name TEXT PRIMARY KEY,
    commit_id INTEGER NOT NULL REFERENCES commits(id) ON DELETE CASCADE,
    created_at_ms INTEGER NOT NULL
);

-- Named, movable pointers to commits: the store's branch model. A ref
-- whose `commit_id` is NULL is unborn - it names a history without a
-- commit yet. Exactly one ref is active: the one new commits hang from
-- (the store's HEAD). `commit_id` has no cascade on purpose - a ref
-- naming a retired commit follows the fold instead of vanishing.
CREATE TABLE refs(
    name TEXT NOT NULL PRIMARY KEY,
    commit_id INTEGER NULL REFERENCES commits(id),
    active INTEGER NOT NULL DEFAULT 0 CHECK (active IN (0, 1)),
    created_at_ms INTEGER NOT NULL
);

-- At most one active ref: the partial index covers the active rows only.
CREATE UNIQUE INDEX idx_refs_active
    ON refs(active) WHERE active = 1;
