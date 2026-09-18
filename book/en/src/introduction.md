# Introduction

Sekai takes chunk-level deduplicated snapshots of Minecraft Java region
files (`.mca`) and rebuilds the world from any snapshot quickly. It is
built for frequent hot-recovery — rewinding a live server after griefing
or a bad update — rather than long-term archival.

Identical chunk payloads are stored once and shared across snapshots via
content-addressed storage, so an unchanged backup costs roughly one
metadata row instead of a full world copy. Rollback reproduces the exact
bytes captured for the snapshot, atomically and verbatim.

Sekai ships two things:

- the `sekai` CLI documented in this guide,
- a reusable Rust library (`sekai-app` over `sekai-core`) for
  server-management software. The library API is documented with rustdoc
  (`cargo doc`); this book covers operating the CLI only.

Design background lives in [ARCHITECTURE.md](https://github.com/TMCJapan/sekai/blob/main/ARCHITECTURE.md):
layering, the two-layer hashing model, and the MVCC/GC data model. The
machine-readable output contract lives in
[docs/json.md](https://github.com/TMCJapan/sekai/blob/main/docs/json.md).
