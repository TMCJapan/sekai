# ADR-0028: Git-modelled refs, worktrees, and reflogs

- Status: Accepted
- Date: 2026-11-14

## Context

Under ADR-0019 a store has one active ref and no tags, no detached HEAD,
and no ref history, so a store can serve only one world. This ADR
replaces that model with Git's refs, worktrees, and reflogs. Behavior
follows Git (as of 2.53) unless this ADR says otherwise. Every
deviation from Git is marked **(deviation)**.

## Decision

### Refs

- `refs` holds branches (`refs/heads/<name>`) and lightweight tags
  (`refs/tags/<name>`), each keyed by its full Git refname. Every ref
  names a commit, and an unborn branch has no row.
- `RefName` is checked as follows:
  - **Length.** Up to 255 bytes. Git sets no limit **(deviation)**.
    `ScopeName` gets the same limit.
  - **Characters.** The ASCII character set stays unchanged.
  - **Git's component rules.** A component must not contain `..`,
    start with `.`, or end with `.lock`, and the name must not end with
    `.`.
  - **Names that are allowed.** All-digit names are allowed.
  - **Reserved name.** Only the exact name `HEAD` is reserved. The
    match is case-sensitive.
- A ref cannot be created where it would conflict with another ref as a
  directory and a file would conflict. For example, `refs/heads/df` and
  `refs/heads/df/sub` cannot both exist.

### Resolution

- A 64-character hex string is a commit ID. If a ref resolves from the
  same string, sekai warns.
- Any other input resolves as gitrevisions describes: the first
  existing key among `<input>`, `refs/<input>`, `refs/tags/<input>`,
  and `refs/heads/<input>`. When more than one exists, sekai warns and
  takes the first.
- An argument that names a branch, such as the branch for `checkout`
  or `worktree add`, is a branch name. It is not resolved by the rules
  above.
- Output shows names as `%(refname:short)` does: the shortest
  unambiguous form, which is `x`, `heads/x`, or `refs/heads/x`.

### Worktrees

- Each worktree has its own HEAD in one of three states:
  - **attached:** `head_ref` names an existing branch;
  - **unborn:** `head_ref` names a branch that has no row yet;
  - **detached:** `head_commit` holds a commit.
- A branch is checked out by at most one worktree, and this holds for
  an unborn branch too **(deviation)**.
- The application enforces the following rules, which the schema
  cannot express:
  - A checked-out branch is never deleted.
  - A checked-out branch is never moved by force.
  - A rename rewrites every `head_ref` that names the branch. Renaming
    an unborn branch rewrites only `head_ref`.
  - The directory and file conflict rule from *Refs* applies.
  - A linked worktree's path never equals the main worktree's world
    root.
- On a commit, HEAD's commit becomes the first parent. When HEAD is
  unborn, the commit has no parents. In the same transaction, the
  commit moves the branch (creating it when HEAD is unborn), or it moves
  `head_commit` when HEAD is detached.

#### `init`

```text
sekai init [--store <store>] [--world <world>]
```

- `--world` defaults to the current directory, which must be a
  recognizable world.
- `--store` defaults to `<world>/.sekai`.
- `init` registers the world as a worktree and leaves the world's files
  untouched.
- HEAD starts unborn on `refs/heads/main`. Git 2.53 starts on `master`
  **(deviation)**.
- When the store already exists, the result depends on the world:
  - With the same world, `init` reinitializes the store and changes
    nothing.
  - With a different world, `init` is an error.
- `init` is the only way for an existing world to enter a store.

#### `worktree add`

```text
sekai worktree add [-b <new> | -B <branch> | --detach] <path> [<commit-ish>]
```

- The target must be empty or missing.
- The source is the worktree that the command runs in, and the command
  fails outside a worktree.
- `<commit-ish>` decides HEAD:
  - **A branch:** HEAD attaches to it. The branch must not be checked
    out elsewhere.
  - **A tag or a commit:** HEAD is detached.
  - **Omitted:** sekai checks out the branch named after the basename
    of `<path>`. If no such branch exists, sekai creates it at the
    source's HEAD.
- `-b` creates a branch, and fails if the branch already exists.
  `-B` creates the branch, or moves it to `<commit-ish>` when it
  exists. Both start at the source's HEAD when `<commit-ish>` is
  omitted, and `-B` fails when the branch is checked out elsewhere.
  `--detach` detaches HEAD, at the source's HEAD when `<commit-ish>` is
  omitted.
- A source with an unborn HEAD is an error. Git would infer `--orphan`
  **(deviation)**.
- `-f`, `--orphan`, `--no-checkout`, and `--track` are not adopted
  **(deviation)**.
- Region files come from the commit. Everything else under the copy
  root is copied from the source, except for:
  - the region files that the commit restores;
  - this store's directory;
  - the link file;
  - `session.lock`;
  - sekai's lock file.
- The copy root depends on the layout:
  - **Bukkit layouts:** the world root, which is the server directory.
  - **Vanilla layouts:** the parent of the world directory is a server
    directory when it holds a `server.properties` file. That file then
    decides:
    - a `level-name` that names the world directory: the parent;
    - a `level-name` that names any other directory: the world root;
    - a file that cannot be read, or that has no valid `level-name`:
      an error.

    With no `server.properties`, as for a single-player save, the copy
    root is the world root.
- `<path>` becomes the new copy root, and the world root keeps its
  relative position under it.
- The target takes the source's resolved layout. This brings back a
  constructor for roots that do not exist yet, which ADR-0025 removed.
- A dimension that is in the commit but that the source does not
  resolve fails the command.
- A failure removes the target.

#### Identity and discovery

- A worktree's name comes from the basename of `<path>`. For `init`,
  it comes from the basename of the world root. The basename is
  sanitized as Git sanitizes worktree IDs, which replaces characters
  that are invalid in a ref component. A clash gets a numeric suffix.
- The main worktree has a name too. In Git the main worktree has no ID
  **(deviation)**.
- `path` holds the absolute world root of a linked worktree. Only
  `move` and `repair` update it.
- The main worktree's `path` is NULL, and at most one row has a NULL
  `path`. Its world root is the directory that holds the store, so
  moving the world together with the store needs no repair. Git also
  records paths only for linked worktrees.
- Commands identify a worktree by its path, or by trailing path
  components that are unique among worktrees.
- `.sekai` sits at the world root. For Bukkit layouts, the world root is
  the server directory.
  - **A directory** at `.sekai` is the store. Its world is the main
    worktree, which is the row with a NULL `path`. A store outside
    every world has no main worktree.
  - **A file** at `.sekai` is a link file, which `init --store` and
    `worktree add` write:

    ```text
    store: /absolute/path/to/store
    worktree: <name>
    ```
- Discovery walks up from the current directory, or from `--world`, to
  the first `.sekai`. It stops at a filesystem boundary.
- Discovery never falls back to matching paths. Without a `.sekai`, a
  command fails, and the error names `worktree repair <path>`, which
  rewrites a lost link file from another worktree.
- `--store` overrides the store location. The worktree name still comes
  from the link file, whereas Git ignores the gitfile **(deviation)**.
- World scanning ignores `.sekai`.
- `worktree list`, `remove`, `move`, `repair`, and `prune` follow Git,
  with these exceptions:
  - `remove` deletes the row and the link file, but it keeps the
    world's files. Git deletes the working tree **(deviation)**.
  - `move` moves the copy root.
  - The main worktree cannot be removed or moved.
  - `lock` and `--force` are not adopted.

### Reflogs

- `ref_log` keeps one log per branch:
  - A rename moves the log and appends a rename entry.
  - Deleting the branch drops the log.
- `head_log` keeps one log per worktree:
  - It records every change that leaves HEAD at a commit, including a
    change that keeps the commit, such as a branch checkout at the same
    commit or a rename of HEAD's branch.
  - It is dropped with its worktree.
- Tags are not logged.
- An `old_commit` of NULL marks a creation.
- Messages use Git's forms:
  - `commit (initial): `
  - `commit: `
  - `checkout: moving from <a> to <b>`
  - `branch: Created from <x>`
  - `branch: Reset to <x>`
  - `Branch: renamed <a> to <b>`
- Deleting a branch prints `Deleted branch <name> (was <hash>)` with the
  full hash, because sekai does not accept abbreviated IDs
  **(deviation)**.

### Schema

```sql
CREATE TABLE refs(
    name TEXT NOT NULL COLLATE BINARY PRIMARY KEY,
    commit_id BLOB NOT NULL REFERENCES commits(id),
    CHECK (name GLOB 'refs/heads/?*' OR name GLOB 'refs/tags/?*')
) WITHOUT ROWID;

-- `head_ref` has no foreign key: an unborn HEAD names a missing branch.
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

CREATE UNIQUE INDEX idx_worktrees_head_ref
    ON worktrees(head_ref) WHERE head_ref IS NOT NULL;

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
```

- Queries select a namespace with GLOB or a range, never with LIKE.
- `SCHEMA_VERSION` goes from 16 to 17, and old stores are recreated.
- In the `MetaStore` port, `active_ref` and `activate_ref` become
  worktree-scoped HEAD operations.
- This ADR supersedes ADR-0019.
- It amends ADR-0026 in three ways: `init` takes a world, `--store`
  defaults to `<world>/.sekai`, and no ref is seeded.

## Consequences

- One store backs up several worlds, and a world can be checked out at
  a tag or at a commit.
- Four ref rules and one worktree rule live in application code rather
  than in the schema.
- `worktree add` copies the whole copy root, including logs and caches.
  The copied non-region files reflect the source as it is now.
- A world copied by hand shares the original's HEAD, and sekai does not
  detect it.

Out of scope:

- the CLI vocabulary, the global `--world`, and the JSON contract, which
  get a separate ADR;
- revision syntax (`HEAD`, `@`, `~`, `^`, `@{n}`);
- abbreviated commit IDs;
- commit messages;
- remotes;
- non-ASCII names;
- an exclusion list for `worktree add`.
