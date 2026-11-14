# ADR-0029: Git-shaped command line

- Status: Accepted
- Date: 2026-11-14

## Context

ADR-0028 brings Git's refs, worktrees, and reflogs into the store and
leaves the command line to this ADR. Today every world command takes
the world as a positional argument, `--store` defaults to `./.sekai`,
`checkout` rewrites files without moving the active ref, `ref` manages
refs, and `log` lists every commit oldest first. This ADR gives the
command line Git's shape. Behavior follows Git (as of 2.53) unless this
ADR says otherwise. Every deviation from Git is marked
**(deviation)**.

## Decision

### Locating the store and the worktree

- Two global options locate the store and the worktree:
  - `--world <world>` starts discovery at `<world>` instead of the
    current directory (ADR-0028).
  - `--store <store>` overrides the store location (ADR-0028).
- No command takes the world as a positional argument, and `diff` no
  longer has its own `--world`.
- No environment variable stands in for these options.
- A store has no HEAD of its own. Outside a worktree, every default
  that would read HEAD fails, whereas a Git bare repository has a HEAD
  **(deviation)**. With `--store`, these commands still run:
  - `branch` and `tag` when they list, create at an explicit start,
    delete with `-D`, or rename with both names given;
  - `log` with a revision or `--all`;
  - `reflog` with a branch;
  - `scope`, `worktree list`, and `worktree prune`.

### Revisions

- A commit-ish argument accepts:
  - a 64-character commit ID, or a ref name resolved as ADR-0028
    describes;
  - `HEAD`, and `@` as its alias;
  - `<rev>~<n>` and `<rev>~`: the n-th first-parent ancestor;
  - `<rev>^<n>` and `<rev>^`: the n-th parent, where `^0` is the commit
    itself;
  - `<ref>@{<n>}`: the n-th prior value in the reflog of `<ref>`, which
    is `HEAD` or a branch. `@{<n>}` reads the current branch's log, or
    HEAD's log when HEAD is detached. Tags have no log (ADR-0028).
  - `@{-<n>}`: the n-th branch or commit checked out before the current
    one, read from the `checkout: moving from` entries of `head_log`.
- `log` also accepts `<a>..<b>` and `^<rev>`.
- Not adopted: `@{<date>}`, `<a>...<b>`, `@{upstream}`, `@{push}`,
  `<rev>^{<type>}`, `:/<text>`, `<rev>:<path>`, and `ORIG_HEAD`.
- The grammar is checked at parse time, and resolution happens at run
  time (ADR-0013).

### Commands

| Command | Role |
| --- | --- |
| `init` | registers a world (ADR-0028) |
| `commit` | records the world on HEAD |
| `status` | previews a commit (ADR-0010) |
| `checkout` | switches HEAD, or restores an area |
| `branch` | manages branches |
| `tag` | manages lightweight tags |
| `log` | walks the history |
| `diff` | compares the world and commits |
| `reflog` | shows ref logs |
| `worktree` | manages worktrees (ADR-0028) |
| `scope` | manages named scopes (ADR-0021) |

`ref` is removed.

#### `commit`

```text
sekai commit [<selection>]
```

- `commit` records the world on HEAD (ADR-0028).
- It first prints Git's summary line, `[main (root-commit) <id>]`,
  `[main <id>]`, or `[detached HEAD <id>]`, and then sekai's report.
- A commit has no message, so `-m` is not adopted (ADR-0028).

#### `status`

- The first line follows Git: `On branch <name>`, `HEAD detached at
  <x>`, or `HEAD detached from <x>`. An unborn HEAD adds
  `No commits yet`.
- `<x>` is the name or ID that HEAD was detached at, read from
  `head_log` as Git reads it.
- The preview below that line is unchanged.

#### `checkout`

```text
sekai checkout [-f] [--detach] <branch> | <commit-ish>
sekai checkout [-f] (-b <new> | -B <branch>) [<start>]
sekai checkout [-f] [<commit-ish>] <selection>
```

Switching:

- An argument names a branch when `refs/heads/<argument>` exists.
  Otherwise it is resolved as a commit-ish. `-` means `@{-1}`.
- A branch attaches HEAD to it, and the world is rebuilt from the
  branch's commit. A branch checked out elsewhere is refused
  (ADR-0028).
- Any other commit-ish, or `--detach`, detaches HEAD. sekai then prints
  Git's detached HEAD advice, with `sekai checkout -b <new-branch-name>`
  in place of `git switch -c` and `sekai checkout -` in place of
  `git switch -`.
- `-b` creates a branch at `<start>` and switches to it, and it fails
  when the branch exists. `-B` creates the branch or moves it to
  `<start>`, even when it is HEAD's branch, and then switches to it.
  `<start>` defaults to HEAD. A branch checked out in another worktree
  is refused.
- Messages: `Switched to branch '<b>'`, `Switched to a new branch
  '<b>'`, `Reset branch '<b>'`, `Already on '<b>'`, and
  `HEAD is now at <id>`.
- The reflogs record `checkout: moving from <a> to <b>`. A reset by
  `-B` also records `branch: Reset to <x>`.
- With no argument and no selection, `checkout` does nothing.

Restoring an area:

- With a selection (`--in`, `--region`, `--kind`, or `--scope`),
  `checkout` rewrites only the selected area from `<commit-ish>`, which
  defaults to HEAD. HEAD does not move, as with
  `git checkout <tree-ish> -- <pathspec>`.
- `-b`, `-B`, and `--detach` cannot be combined with a selection.
- The next commit records the restored area on HEAD with HEAD as its
  only parent. Nothing records where the area came from.

Uncommitted changes:

- Without `-f`, `checkout` refuses when anything it would write or
  delete differs from HEAD.
  - The check also applies with a selection. Git overwrites a pathspec
    without checking **(deviation)**.
  - Changes that `checkout` leaves untouched carry over, as in Git.
  - With an unborn HEAD, every region file is uncommitted.
- A region file that the target commit lacks is deleted unless
  `--keep-post-commit-files` is given, even when HEAD lacks it too. Git
  keeps untracked files **(deviation)**.
- `--keep-post-commit-files`, `--keep-post-commit-chunks`,
  `--keep-tombstoned-chunks`, `--on-missing-blob`, and
  `--on-missing-file` stay. The check counts only what these options
  let `checkout` touch.
- `--orphan`, `--track`, `--merge`, and `--patch` are not adopted.

#### `branch`

```text
sekai branch [--list] [<pattern>...]
sekai branch [-f] <name> [<start>]
sekai branch (-d | -D) <name>...
sekai branch (-m | -M) [<old>] <new>
sekai branch --show-current
```

- The list follows Git:
  - `*` marks HEAD's branch;
  - `+` marks a branch checked out in another worktree;
  - a detached HEAD comes first, as `(HEAD detached at <x>)` or
    `(HEAD detached from <x>)`.
- Patterns are globs matched against short names.
- `<start>` defaults to HEAD. `-f` moves an existing branch, and a
  checked-out branch is never moved (ADR-0028).
- `-d` refuses a branch that HEAD cannot reach. `-D` deletes it anyway.
  A checked-out branch is never deleted.
- `-m` renames a branch, and `-M` renames it over an existing one. With
  one name, HEAD's branch is renamed, including an unborn one
  (ADR-0028).
- The mode options are mutually exclusive, and parsing rejects any
  combination of them.
- Not adopted: `-c`, `-C`, `-v`, `--contains`, `--merged`, `--sort`,
  `--format`, `-u`, `-r`, and `-a`.

#### `tag`

```text
sekai tag [-l] [<pattern>...]
sekai tag [-f] <name> [<commit-ish>]
sekai tag -d <name>...
```

- `<commit-ish>` defaults to HEAD.
- `-f` replaces an existing tag and prints
  `Updated tag '<name>' (was <id>)`.
- `-d` prints `Deleted tag '<name>' (was <id>)`.
- Only lightweight tags exist, so `-a`, `-m`, and `-s` are not adopted.

#### `log`

```text
sekai log [--all] [--reverse] [--oneline] [--[no-]decorate] [--stat]
          [<revision-range>...]
```

- `log` walks parents from HEAD, or from the given revisions, newest
  first in Git's default order. `--all` starts from every ref and HEAD.
- Each entry follows Git's `medium` format, without the author and the
  message, which sekai does not record. `--oneline` prints the ID and
  the decorations on one line.
- Decorations follow Git's `--decorate=short`, such as
  `(HEAD -> main, tag: v1)`. They appear when stdout is a terminal,
  and `--decorate` and `--no-decorate` override that.
- `--stat` keeps sekai's per-commit statistics.
- An unborn HEAD fails with
  `your current branch '<b>' does not have any commits yet`.

#### `diff`

```text
sekai diff [<selection>] [<commit-ish> [<commit-ish>]]
sekai diff [<selection>] <a>..<b>
```

- With no commit-ish, `diff` compares the world with HEAD. With one, it
  compares the world with that commit. With two, or a range, it
  compares the two commits.
- The implicit comparison of a commit with its first parent is removed.
  `diff <c>^ <c>` replaces it.
- With an unborn HEAD, the default fails as an unknown revision, as Git
  fails on `HEAD`.

#### `reflog`

```text
sekai reflog [show] [<ref>]
```

- `<ref>` is `HEAD` or a branch, and defaults to `HEAD`.
- Lines follow Git, newest first: `<id> HEAD@{<n>}: <message>`.
- `expire`, `delete`, and `exists` are not adopted.

### Options

- `-j` becomes the short form of `--jobs`.
- `--progress` and `--no-progress` follow Git: progress shows by
  default when stderr is a terminal. `--json` turns progress off, and
  `--progress` with `--json` stays refused, so stderr stays silent
  (ADR-0002).
- `--timing`, `--json`, `--color`, and the selection options are
  unchanged.

### Output

- Human output copies Git's wording. IDs are always full, because sekai
  does not accept abbreviated IDs (ADR-0028) **(deviation)**.
- Errors keep the anyhow chain, and exit codes stay 0, 1, and 2. Git
  exits with 128 on a fatal error and 129 on a usage error
  **(deviation)**.
- JSON keeps the envelope (ADR-0002):
  - `command` loses `ref` and gains `branch`, `tag`, `reflog`, and
    `worktree`.
  - Ref names are full refnames such as `refs/heads/main`. IDs are
    64-character hex strings, and times are Unix milliseconds.
  - A HEAD object is `{"ref": <refname or null>, "commit": <id or
    null>}`. Attached has both, unborn has a null `commit`, and
    detached has a null `ref`.
  - The `commit`, `status`, and `checkout` reports gain `head`, which
    describes HEAD after the command.
  - `branch` and `tag` return `{"name", "commit"}` for each ref they
    create, move, delete, or list. A deletion reports the commit that
    the ref named. A rename adds `old_name`. A branch in a list adds
    `worktree`, which is the name of the worktree that has it checked
    out, or null. `--show-current` returns `{"name": <refname or
    null>}`.
  - Each `log` entry gains `refs`, the refnames that point at it, and
    `head`, which is true on HEAD's commit. They do not depend on the
    terminal.
  - `reflog` returns `{"selector", "old_commit", "new_commit",
    "message", "at_ms"}` entries, newest first, where `selector` is the
    `HEAD@{<n>}` form.
  - `worktree` returns `{"name", "path", "main", "head"}` records.
- This ADR amends ADR-0013: `branch` and `tag` use Git's mode options
  instead of subcommands, and argument groups keep an invalid
  combination a parse error.

## Consequences

- Inside a world, no command needs a path.
- Every world command drops its positional world, `ref` becomes
  `branch` and `tag`, `checkout` moves HEAD, `log` walks from HEAD,
  `diff` changes its defaults, and the JSON `command` values change.
  The CLI, `docs/json.md`, the book, and the golden tests change
  together.
- `checkout` can refuse, and `-f` overrides it.
- `@{<n>}` recovers a lost position without pasting a 64-character ID.

Out of scope:

- `reset`; `checkout -B` covers moving HEAD's branch together with the
  world;
- merges and `show`;
- splitting `checkout` into `switch` and `restore`;
- `@{<date>}` and `<a>...<b>`;
- `-C` and environment variables;
- abbreviated commit IDs, commit messages, and remotes.
