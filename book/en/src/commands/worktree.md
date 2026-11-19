# Worktrees

```sh
sekai worktree add ../feature            # new branch "feature"
sekai worktree add ../hotfix v1          # detached at v1
sekai worktree list
sekai worktree remove feature
```

A worktree is another world sharing the store, as Git's worktrees share
a repository. Each has its own HEAD, so two worlds can sit on different
branches at once.

- `worktree add <path> [<commit-ish>]` copies this world to `<path>`,
  which must be missing or empty, and rebuilds its region files from the
  commit. Without a commit-ish, it creates a branch named after the
  directory; a branch checks it out, and anything else detaches HEAD. A
  branch checked out elsewhere is refused. On a server, the server
  directory around the world is copied, minus the store and lock files.
- `worktree list` prints each world root with its commit and branch.
- `worktree remove <worktree>` unregisters a linked worktree, named by a
  unique suffix of its path, and removes its link file; the world's
  files stay. The main worktree, whose world holds the store, cannot be
  removed.
