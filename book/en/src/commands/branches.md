# Branches and tags

```sh
sekai branch                      # list, * marks HEAD's branch
sekai branch experiment           # start a branch at HEAD
sekai branch -f experiment v1     # move it
sekai branch -m trunk             # rename HEAD's branch
sekai branch -d experiment        # delete a branch HEAD reaches
sekai tag v1                      # tag HEAD
sekai tag -d v1
sekai reflog                      # where HEAD has been
```

Branches and tags work as in Git, with Git's mode options:

- `branch [--list] [<pattern>...]` lists branches. `*` marks HEAD's
  branch, `+` one checked out in another worktree, and a detached HEAD
  comes first as `(HEAD detached at <id>)`, where `<id>` is HEAD's
  commit.
- `branch [-f] <name> [<start>]` creates a branch at `<start>`, which
  defaults to HEAD; `-f` moves an existing one. A checked-out branch is
  never moved.
- `branch (-d | -D) <name>...` deletes branches. `-d` refuses one HEAD
  cannot reach, and `-D` deletes it anyway. A checked-out branch is
  never deleted.
- `branch (-m | -M) [<old>] <new>` renames a branch, HEAD's branch when
  only the new name is given; `-M` renames over an existing one.
- `branch --show-current` prints HEAD's branch.
- `tag [-f] <name> [<commit-ish>]` tags a commit, HEAD by default;
  `-f` replaces a tag. `tag -d` deletes tags, and `tag -l` lists them.
  Tags are lightweight: there is no message or signature.
- `reflog [show] [<ref>]` shows where HEAD, or a branch, has pointed,
  newest first, as `<id> HEAD@{<n>}: <message>`.

Names allow ASCII alphanumerics plus `.`, `_`, `-`, and `/`, and run
1-255 bytes. No component may be empty, the name may not contain `..`,
and a new name may not start with `-`. `HEAD` is reserved.

## Revisions

Wherever a commit is expected, sekai takes a commit ID (all 64
characters), a ref name, `HEAD` or `@`, and Git's suffixes:

- `<rev>~<n>`: the n-th first-parent ancestor.

`log` also takes `<a>..<b>` and `^<rev>`, and `diff` takes `<a>..<b>`.
