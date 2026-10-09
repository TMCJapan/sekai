CREATE TABLE snapshots(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms INTEGER NOT NULL
);

-- Dimension identity registry: game-unique names (`minecraft:overworld`,
-- `aether:sky`) and layout-derived keys (`./plugin-folder`) mapped to
-- stable integer codes. Codes 0..=2 stay reserved for the vanilla trio,
-- matching the compiled-in `Dimension` constants; every other name gets
-- the next auto-increment code, so history rows keep narrow INTEGER
-- columns and `Dimension` stays a `Copy` newtype.
CREATE TABLE dimensions(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE
);

INSERT INTO dimensions (id, name) VALUES
    (0, 'minecraft:overworld'),
    (1, 'minecraft:the_nether'),
    (2, 'minecraft:the_end');

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

