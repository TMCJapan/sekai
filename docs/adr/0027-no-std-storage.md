# ADR-0027: `no_std` storage boundary

- Status: Accepted
- Date: 2026-11-15

## Context

`sekai-storage` owns the backend-neutral store boundary (ADR-0016), and a
wasm backend is a stated goal: the traits exist so a browser or in-memory
backend can arrive as a sibling crate without touching `core`/`app`. The
crate itself, however, still pulled in `std`: `StorageError` derived
`thiserror`, carried `PathBuf` paths, and boxed a `std::io::Error` in its
`Io` variant. Every backend therefore inherited a host-only error surface,
and the crate could not join the `no_std` layer (`util`, `anvil`, `core`,
`world`) that the portability gates already enforce.

## Decision

`sekai-storage` becomes `no_std` + `alloc`, unconditionally, like the pure
crates:

- `thiserror` is dropped; `StorageError` gets hand-written
  `core::fmt::Display` and `core::error::Error` impls with unchanged
  messages.
- `Io` and `StoreMissing` carry `String` paths instead of `PathBuf`; the
  boundary never interprets a path, only the backend does (the opener
  already takes a `&str` location).
- `Io` keeps its own variant but boxes its source as
  `dyn core::error::Error + Send + Sync`, exactly like `Backend`, so a
  backend on any target can report an I/O failure at a location.
- `io_error` generalizes to
  `(impl Into<String>, impl core::error::Error + Send + Sync + 'static)`;
  host backends convert their paths with `Path::display()` at the call
  site.
- The crate joins the `thumbv7m-none-eabi` portability gate alongside the
  other pure crates (it already linted for `wasm32-unknown-unknown`).

## Consequences

- One error surface for every backend, host or `no_std`; a wasm backend
  implements the same traits and constructs the same errors without a
  `std` shim.
- Breaking API change, acceptable pre-release: `Io`/`StoreMissing` fields
  become `String`, and `io_error` no longer accepts `AsRef<Path>` /
  `std::io::Error` concretely. `sekai-storage-local` converts paths at its
  boundary; no other consumer matched on the changed fields.
- `StorageError::Io` no longer exposes `std::io::Error::kind()`; callers
  that need it downcast the boxed source. Nothing in the workspace does
  today.
- ADR-0001's layer split is refined, not overturned: filesystem, clocks,
  and databases still live in the outer crates (`world-local`,
  `storage-local`, `app`, CLI) by construction.
