CREATE TABLE snapshots(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms INTEGER NOT NULL
);

-- History graph: which snapshots a snapshot continues. The root of a
-- history has no row, an ordinary snapshot one, a merge several. The
-- graph - not the identifier order - decides what a snapshot inherits.
-- `parent_id` has no cascade on purpose: deleting a snapshot that
-- children still point at fails loudly, so a retirement rewires the
-- children before it drops the row.
CREATE TABLE snapshot_parents(
    child_id INTEGER NOT NULL REFERENCES snapshots(id) ON DELETE CASCADE,
    parent_id INTEGER NOT NULL REFERENCES snapshots(id),
    PRIMARY KEY(child_id, parent_id)
);

CREATE INDEX idx_parents_parent
    ON snapshot_parents(parent_id);

CREATE TABLE chunk_history(
    snapshot_id INTEGER NOT NULL REFERENCES snapshots(id),
    dim INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    cx INTEGER NOT NULL,
    cz INTEGER NOT NULL,
    blob BLOB NULL,
    diff BLOB NULL,
    PRIMARY KEY(snapshot_id, dim, kind, cx, cz)
);

CREATE INDEX idx_history_coord
    ON chunk_history(dim, kind, cx, cz, snapshot_id);

-- Derived per-region fingerprints: which snapshot last confirmed each
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
    snapshot_id INTEGER NOT NULL REFERENCES snapshots(id),
    PRIMARY KEY(dim, kind, rx, rz)
);

-- Human-readable snapshot aliases. Tags die with their snapshot
-- (`ON DELETE CASCADE`): pruning a tagged snapshot drops its tags.
CREATE TABLE snapshot_tags(
    name TEXT PRIMARY KEY,
    snapshot_id INTEGER NOT NULL REFERENCES snapshots(id) ON DELETE CASCADE,
    created_at_ms INTEGER NOT NULL
);

