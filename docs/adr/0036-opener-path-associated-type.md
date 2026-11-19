# ADR-0036: Opener locations are a backend-owned associated path type

- Status: Accepted
- Date: 2026-11-20

## Context

`SekaiStoreOpener::open`/`init` took a `&str` location (ADR-0016), which
forced every host caller to spell a filesystem path as a string — a lossy
round-trip on non-UTF-8 paths — while the boundary itself never interprets
the location (ADR-0027). The natural host signature is
`impl AsRef<Path>`, but `sekai-storage` is `no_std` + `alloc` and cannot
name `std::path::Path`.

## Decision

- `SekaiStoreOpener` gains an associated type `Path: ?Sized`, and the entry
  points take `impl AsRef<Self::Path>`. The backend names its own location
  type: `std::path::Path` for the local backend, a string or URL type for a
  future remote one. The trait stays `no_std`; `Path` never appears in
  `sekai-storage`.
- An associated type, not a trait parameter (`SekaiStoreOpener<P>`): a
  backend has one canonical location type, the trait mirrors `SekaiStore`'s
  associated adapters, and consumer bounds (`T: SekaiStoreOpener`) stay
  free of an extra parameter.
- The returned futures stay `Send`-bounded for every path holder, including
  `!Send` ones: the local impl owns the path (`to_path_buf`) in the
  synchronous body before building the future.
- `LocalStore` becomes a newtype over `Store<SqliteMeta, SledBlob,
SledCache>` instead of an alias. The orphan rule forbids implementing
  `sekai-storage`'s traits for the foreign `Store` combinator from
  `storage-local`; a local store type is what the backend owns anyway
  (ARCHITECTURE.md, Backend Selection). Adapter accessors are forwarded,
  and `SekaiStore` is implemented alongside `SekaiStoreOpener`.
- `SekaiInstance::<LocalStore>::open`/`init` (local) and `open_with`/`init_with`
  (backend-generic) widen to `impl AsRef<Path>` / `impl AsRef<T::Path>`.

## Consequences

- Host callers pass `&Path`, `PathBuf`, `&str`, or `String` directly; no
  string round-trip, and non-UTF-8 store roots work end to end.
- Breaking API change, acceptable pre-release: `LocalStore` is a struct,
  not an alias, and the opener entry points are generic. All existing
  string-based call sites keep compiling (`str`/`String` implement
  `AsRef<Path>`); `Cow<'_, str>` holders switch to the path itself.
- A wasm or remote backend implements the same trait with its own
  `Path` type without touching `sekai-storage` or `app`.
- ADR-0027's `&str`-location statement is refined, not overturned: the
  error surface still carries display strings, and only the backend
  interprets a location.
