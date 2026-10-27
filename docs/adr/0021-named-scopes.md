# ADR-0021: Named scopes - store-saved world areas selected by name

- Status: Proposed
- Date: 2026-10-26

## Context

ADR-0003 made the scope a filter selected per invocation with `--in`,
`--region`, and `--kind`. Callers that back up or restore the same area
over and over (a spawn build, a nether hub) must repeat the full
coordinate spelling on every run, and a typo silently selects a
different area: the scope is a filter, so a wrong rectangle is not an
error, it is a different truth for a different part of the world.

The metadata store already keeps named objects (refs, ADR-0019), so a
name the operator chose once can stand for the area on every later run.
A scope, unlike a ref, names no commit: it describes a part of the
world, not a position in its history.

## Decision

The metadata store carries a `scopes` table and its `scope_areas`
children:

```sql
CREATE TABLE scopes(
    name TEXT NOT NULL COLLATE BINARY PRIMARY KEY,
    kind INTEGER NULL CHECK (kind BETWEEN 1 AND 6),
    created_at_ms INTEGER NOT NULL
);

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
```

- **One or more areas per scope, each a chunk rectangle.** A row of
  `scope_areas` holds the dimension code and the inclusive, normalized
  chunk rectangle (`Rect`). A single chunk is stored as a 1x1 rectangle
  and a region file as its 32x32 rectangle; both select exactly what
  the rectangle selects (`contains`, `matches_region`, and
  `covers_region` agree), so the spelling the operator used is not
  kept. Areas may overlap and are not merged: the scope is their union,
  and a filter selects the same coordinates either way. Identical areas
  collapse to one row. A scope with no area would select nothing, so
  `create` refuses it, and a stored scope without rows is corruption.
- **`kind` is a bitmask on the scope, not per area,** of
  `1 << RegionKind::raw()` (`region` = 1, `entities` = 2, `poi` = 4).
  `NULL` means every region family, and the full set is normalized to
  `NULL`, so the `CHECK` admits `1..=6` only and each selection has
  exactly one stored spelling. A set needs no ordering or deduplication
  rules. Kinds stay per scope because `Scope::Select` applies its kinds
  uniformly to every area; per-area kinds would need a different
  `Scope`.
- **`ScopeName`** is its own type with `RefName`'s grammar (ASCII
  alphanumerics plus `.`, `_`, `-`, `/`, 1-64 bytes, not all digits, no
  empty or traversing path component) and no reserved name. A separate
  type keeps scope and ref names from being swapped and lets errors say
  "scope name". Stored names re-validate on read; anything else is
  corruption.
- **The record resolves to `Scope::Select`** with one
  `(dim, Area::Rect)` entry per area, in `(dim, x0, z0, x1, z1)` order,
  and the decoded kinds. `Scope` itself is not stored: its
  `Area::Chunks` carries kind-expanded coordinates, which would
  duplicate the `kind` column.

The areas live in a child table rather than a JSON array in a `TEXT`
column. A JSON column would work if the store canonicalized the array
before writing and validated it after reading, since a scope is only
ever read and written whole. The child table is preferred because it
keeps the per-area `CHECK` (SQLite forbids subqueries in `CHECK`, so a
constraint cannot reach into the array), its primary key replaces the
ordering and deduplication rules a JSON spelling would need, it follows
the existing `commit_parents` parent/child shape, and the SQLite crate
gains no `serde_json` dependency.

The CLI surface:

- `scope create <name> (--in DIM:X,Z | --in DIM:X0,Z0..X1,Z1 |
  --region DIM:RX,RZ)... [--kind K]... [--force] [--json]` takes one or
  more areas, mixing `--in` and `--region` as inline selections do. A
  bare `--in DIM` is refused at parse time (ADR-0013). An existing name
  fails unless `--force` replaces it; a replacement keeps the original
  creation time and swaps every area in one transaction.
- `scope delete <name> [--json]` and `scope list [--json]`. `list`
  prints one line per scope in name order: the name, the kinds (`all`
  for `NULL`), and each area spelled as its rectangle
  (`overworld:0,0..0,0`) in stored order. There is no `show`: one line
  holds the whole record.
- `commit`, `status`, `checkout`, `clone`, and `diff` take
  `--scope NAME`, exclusive with `--in`, `--region`, and `--kind`
  (`conflicts_with_all`), and not repeatable. A selection is either
  spelled inline or named, never a mix.
- An unknown scope is a runtime error, since only the store knows its
  names. A command given `--scope` against a store that does not exist
  fails before creating it: no scope can exist there, and a failing
  invocation must not initialize a store as a side effect (#80).

The `MetaStore` port carries `set_scope`, `lookup_scope`,
`visit_scopes`, and `delete_scope`; `set_scope` takes the whole record,
areas included. Like the ref mutations, `create` and `delete` run under
the writer lock and `list` under a shared lock.

The schema version goes to 13 (after #145 takes 12); the pre-release
policy applies: old stores are recreated, never migrated.

## Consequences

A recurring scoped run is `sekai commit --scope spawn`, and the
coordinates are typed and checked once. Scopes reference no commit, so
`prune` and `gc` never see them, and deleting one changes no history.

A scope is resolved when a command runs, not when a commit is recorded:
commits do not remember which scope produced them, and replacing a
scope with `--force` changes what later runs select, never what earlier
commits hold.

`--scope` is not repeatable because two scopes with different kinds have
no single `Scope::Select`: merging `region` over one rectangle with
`entities` over another would select `entities` in the first rectangle
too, and a filter that is too wide is not an error. Running each scope
separately would split one invocation into several commits. An operator
who wants several areas together saves them in one scope instead.
Making the flag repeatable later stays a compatible change.

A whole dimension cannot be an area yet. On its own it is already the
short `--in DIM`; next to rectangles in one scope it would need nullable
rectangle columns, deferred until a caller needs it.

Because chunks and regions are stored as rectangles, `diff --scope`
always enumerates coordinates from the compared sides and filters them
by the scope, instead of taking the explicit-chunk path an inline
`--in DIM:X,Z` uses. A region file that several areas cover only
together is reported as partly covered (`covers_region` checks each
area alone), so it records no fingerprint and is re-read on each scoped
commit, as with inline multi-area selections.

`--in`, `--region`, and `--kind` stay available for one-off selections.
