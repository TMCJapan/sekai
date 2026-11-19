# ADR-0035: Git-modelled refs, worktrees, and reflogs

- Status: Accepted
- Date: 2026-11-14

## Context

Under ADR-0019 a store has one active ref and no tags, no detached HEAD,
and no ref history, so a store can serve only one world. This ADR
replaces that model with Git's refs, worktrees, and reflogs. Worktrees
are keyed by their world root path alone: Git derives a worktree's
identity from its path, and a separate name would carry no information
the path does not. Behavior follows Git (as of 2.53) unless this ADR
says otherwise. Every deviation from Git is marked **(deviation)**.

## Decision

### Refs

- `refs` holds branches (`refs/heads/<name>`) and lightweight tags
  (`refs/tags/<name>`), each keyed by its full Git refname. Every ref
  names a commit, and an unborn branch has no row.
- `RefName` is checked as follows:
  - **Length.** Up to 255 bytes. Git sets no limit **(deviation)**.
    `ScopeName` gets the same limit, and it refuses no path shape,
    which amends ADR-0021.
  - **Characters.** The ASCII character set stays unchanged.
  - **Structure.** A name must not contain `..` or an empty component,
    and must not end in `.`, so a revision range such as `a...b` reads
    one way. A name must not start with `-`, which would read as an
    option; Git accepts such a ref but refuses to create one, so no ref
    ever has one. Git refuses more shapes **(deviation)**.
  - **Names that are allowed.** All-digit names are allowed.
  - **Reserved name.** Only the exact name `HEAD` is reserved. The
    match is case-sensitive.
- A ref cannot be created where it would conflict with another ref as a
  directory and a file would conflict. For example, `refs/heads/df` and
  `refs/heads/df/sub` cannot both exist.

### Resolution

- A 64-character hex string is a commit ID. A ref of the same name
  makes the spelling ambiguous, and resolving it is an error
  **(deviation)**.
- Any other input resolves as gitrevisions describes: the first
  existing key among `<input>`, `refs/<input>`, `refs/tags/<input>`,
  and `refs/heads/<input>`. When more than one exists, resolving it is
  an error **(deviation)**; sekai does not warn and take the first, as
  Git does, because a backup tool must not guess which of two
  same-named refs the user meant.
- An argument that may name a branch, such as the target of
  `checkout` or the `<commit-ish>` of `worktree add`, names that branch
  when `refs/heads/<input>` exists, and HEAD attaches to it. When the
  input also names another ref by the rules above, such as a tag of the
  same name, the branch wins, silently **(deviation)**. Any
  other input resolves by the rules above and detaches HEAD, including
  a qualified form such as `heads/x`, as in Git.
- Human output about one namespace, such as branch and tag listings
  and messages about a branch or a tag, shows names without their
  prefix, as `git branch`, `git tag`, and their messages do. Human
  output that mixes namespaces shows names as `%(refname:short)` does:
  the shortest unambiguous form, which is `x`, `heads/x`, or
  `refs/heads/x`. Machine-readable output is left to the CLI ADR.

### Worktrees

- Each worktree has its own HEAD in one of three states:
  - **attached:** `head_ref` names an existing branch;
  - **unborn:** `head_ref` names a branch that has no row yet;
  - **detached:** `head_commit` holds a commit.
- A branch is checked out by at most one worktree, and this holds for
  an unborn branch too **(deviation)**.
- A worktree is identified by its absolute world root path alone.
  Every worktree records its path, the main one included, and a `main`
  flag marks the worktree whose world holds the store. There is no
  worktree name or ID, so no validation, sanitization, or clash rules
  **(deviation)**.
- The store enforces the following rules, which the schema cannot
  express, inside the transaction that would break them:
  - A checked-out branch is never deleted.
  - A checked-out branch is never moved by force.
  - A rename rewrites the `head_ref` that names the branch. Renaming
    an unborn branch rewrites only `head_ref`.
  - The directory and file conflict rule from *Refs* applies.
- `worktree add` refuses a path that is already registered
  (`WorktreePathTaken`), before it refuses a second main worktree
  (`MainWorktreeExists`).
- On a commit, HEAD's commit becomes the first parent. When HEAD is
  unborn, the commit has no parents. In the same transaction, the
  commit moves the branch (creating it when HEAD is unborn), or it moves
  `head_commit` when HEAD is detached.
- `MetaStore` worktree operations take `path: &str`: `commit_tree`,
  `apply_commit_incremental`, `lookup_worktree`, `remove_worktree`,
  `set_head`, `head`, and `visit_head_log`. `visit_worktrees` visits in
  path order. The app acts on the main worktree, resolved by scanning
  for the `main` flag; a store with no main worktree has no HEAD to
  commit on and fails with a dedicated error.
- Errors name paths: `UnknownWorktree`, `CorruptWorktreeHead`,
  `MainWorktreeProtected`, and `WorktreePathTaken` carry a `path`, and
  `BranchCheckedOut` carries the holder's path.

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
sekai worktree add <path> [<commit-ish>]
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
    source's HEAD. A basename that is not a valid branch name fails the
    command, as in Git.
- When `<commit-ish>` is omitted and the source's HEAD is unborn, the
  command fails. Git would infer `--orphan` **(deviation)**. A given
  `<commit-ish>` works whatever the source's HEAD is, as in Git.
- `-b`, `-B`, `--detach`, `-f`, `--orphan`, `--no-checkout`, and
  `--track` are not adopted **(deviation)**. A world is added about once
  per server, so `branch` creates or moves the branch beforehand.
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
    - a file that cannot be read: an error;
    - a `server.properties` in which every `level-name` value is
      unusable: an error;
    - no `level-name` key at all: the game's default `world` applies,
      so the parent is used when the world directory is named `world`.

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

- A worktree's key is its absolute world root path: for a linked
  worktree, the `<path>` that `worktree add` wrote; for the main
  worktree, the directory that holds the store. Nothing updates a
  recorded path.
- Commands identify a worktree by its path, or by trailing path
  components that are unique among worktrees.
- `.sekai` sits at the world root. For Bukkit layouts, the world root is
  the server directory.
  - **A directory** at `.sekai` is the store. Its world is the main
    worktree, which is the row flagged `main`. A store outside every
    world has no main worktree.
  - **A file** at `.sekai` is a link file, which `init --store` and
    `worktree add` write:

    ```text
    store: /absolute/path/to/store
    worktree: /absolute/path/to/world
    ```

    The `worktree:` line records the world root path, the row's key.
- Discovery walks up from the current directory, or from `--world`, to
  the first `.sekai`. It stops at a filesystem boundary.
- Discovery never falls back to matching paths. Without a `.sekai`, a
  command fails.
- `--store` overrides the store location. The worktree still comes
  from the link file, whereas Git ignores the gitfile **(deviation)**.
- World scanning ignores `.sekai`.
- Besides `add`, only `worktree list` and `worktree remove` exist:
  - `list` follows Git's default output and takes no options.
  - `remove` deletes the row and the link file, but it keeps the
    world's files. Git deletes the working tree **(deviation)**. It
    also clears a worktree whose world is already gone.
  - The main worktree cannot be removed.
  - `move`, `repair`, `prune`, `lock`, `unlock`, and `--force` are not
    adopted **(deviation)**. A world moved with `mv` keeps working,
    because discovery reads its link file. Only the recorded path goes
    stale.

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
    path TEXT NOT NULL COLLATE BINARY PRIMARY KEY,
    main INTEGER NOT NULL CHECK (main IN (0, 1)),
    head_ref TEXT NULL CHECK (head_ref GLOB 'refs/heads/?*'),
    head_commit BLOB NULL REFERENCES commits(id),
    CHECK ((head_ref IS NULL) != (head_commit IS NULL))
) WITHOUT ROWID;

-- Only one row may carry the main flag.
CREATE UNIQUE INDEX idx_worktrees_main
    ON worktrees(main) WHERE main;

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
    worktree TEXT NOT NULL COLLATE BINARY
        REFERENCES worktrees(path) ON DELETE CASCADE ON UPDATE CASCADE,
    old_commit BLOB NULL REFERENCES commits(id),
    new_commit BLOB NOT NULL REFERENCES commits(id),
    message TEXT NOT NULL,
    at_ms INTEGER NOT NULL
);

CREATE INDEX idx_head_log_worktree ON head_log(worktree, seq);
```

- Queries select a namespace with GLOB or a range, never with LIKE.
- `SCHEMA_VERSION` goes from 17 to 19. There is no migration: old
  stores are refused with `UnsupportedSchema` and recreated.
- In the `MetaStore` trait, `set_ref` splits into `create_ref` and
  `move_ref`, `active_ref` and `activate_ref` become the per-worktree
  `head` and `set_head`, and worktree and reflog operations are added.
- This ADR supersedes ADR-0019.
- It amends ADR-0026 in three ways: `init` takes a world, `--store`
  defaults to `<world>/.sekai`, and no ref is seeded.
- It amends ADR-0036's output contract: the `worktree` record is
  `{"path", "main", "head"}`, and the `worktree` field a branch list
  adds is the holder's path, not a name.

## Consequences

- One store backs up several worlds, and a world can be checked out at
  a tag or at a commit.
- One key and one uniqueness rule per worktree: no name validation or
  clash handling, and the main worktree is an ordinary row, so queries
  never special-case it.
- Four ref rules live in the store's code and one worktree rule lives
  in `worktree add`, rather than in the schema.
- `worktree add` copies the whole copy root, including logs and caches.
  The copied non-region files reflect the source as it is now.
- A world copied by hand shares the original's HEAD, and sekai does not
  detect it.
- Moving the world together with the store leaves the main worktree's
  recorded path stale. Discovery still finds the store through
  `.sekai`, but nothing rewrites the path, as for a linked worktree
  moved with `mv`. This is the accepted cost of recording the main
  path.

Out of scope:

- the CLI vocabulary, the global `--world`, and the JSON contract, which
  get a separate ADR;
- revision syntax (`HEAD`, `@`, `~`);
- abbreviated commit IDs;
- commit messages;
- remotes;
- non-ASCII names;
- an exclusion list for `worktree add`;
- repairing a stale worktree path: `worktree move` and `repair` stay
  unadopted.