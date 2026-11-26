# ADR-0035: Git-style remote sync commands and HTTP API

- Status: Proposed
- Date: 2026-11-17

## Context

Sekai plans git-shaped sharing commands - `clone`, `fetch`, `pull`,
`push`, `remote` - plus an HTTP API so a repository can be hosted
centrally and Minecraft servers can back up to it. The data model is
already git-shaped: content-addressed blobs (ADR-0024), a three-level
Merkle snapshot tree (ADR-0027), content-addressed commits over a parent
graph (ADR-0017, ADR-0020), and ref objects with one active ref as
`HEAD` (ADR-0019). What is missing is the wire: how two repositories
exchange objects and move refs safely.

### Prior art surveyed

**GitHub REST API (git database).** Resources hang off
`/repos/{owner}/{repo}`: `git/refs` (list/get/create/`PATCH {sha,
force}`/delete), `git/blobs` (create `{content, encoding}`, get as
base64 JSON or a raw media type), `git/trees` (create `{tree,
base_tree}`, get with `recursive`), `git/commits` (create `{message,
tree, parents}`, get). Writes follow a four-step flow: upload blobs,
build a tree, create a commit, then move a ref. Conventions worth
keeping: the `{owner}/{repo}` addressing, plural collections, hashes in
paths, `force` on ref updates, 409 for conflicts / 422 for validation,
raw media types for binary. Conventions worth dropping: base64-in-JSON
for binary payloads, ISO 8601 timestamps (the CLI JSON contract uses
Unix millis, `docs/json.md`). The `{owner}` segment is adopted as a
pure namespace: sekai has no account model yet, so an owner names a
directory, not an authenticated identity.

**GitLab / Gitea.** GitLab addresses repositories by opaque numeric id
(`/api/v4/projects/{id}/repository/...`); Gitea mirrors GitHub under
`/api/v1`. Both version the API in the URL path. Numeric ids are
rejected: a sekai remote URL should name the repository directly, the
way a git URL does, with no lookup step.

**Git smart HTTP protocol.** `GET $GIT_URL/info/refs?service=...`
advertises refs and capabilities; `POST $GIT_URL/git-upload-pack`
negotiates `want`/`have` and returns a packfile; `POST
$GIT_URL/git-receive-pack` carries ref commands (`old new name`,
compare-and-swap on `old`) plus a packfile and returns a per-ref
report. The server is stateless; all session state lives in the client;
standard HTTP auth, no cookies. The shape to keep: advertise first,
then one transfer verb per direction, CAS ref commands, a report. The
machinery to drop: pkt-line framing, packfiles, and multi-round
negotiation exist for delta compression - sekai objects are independent
and deduplicate by hash, so a single-round set difference replaces the
whole negotiation.

### Constraints

- `util`/`anvil`/`core`/`storage`/`world` stay `no_std` + `alloc`; HTTP
  and tokio live only in outer crates (CONTRIBUTING).
- Crash order holds on the receiving side: blobs durable and synced
  before any metadata names them.
- A blob's CAS key is its `Chunk::normalized_hash`, **not** a hash of
  its bytes: the receiver cannot verify a blob by hashing the payload;
  it must decode and normalize through `anvil`, exactly like ingest.
- Repositories are append-only; a failed transfer leaves at most orphan
  objects, never dangling references (ADR-0027).
- Two-phase destructive operations; no server orchestration in library
  crates; CLI JSON contract changes follow the `docs/json.md`
  breaking-change process.
- The old `clone` (single-commit export) was removed by ADR-0025 because
  its name clashed with `git clone`. A whole-repository clone makes the
  name accurate again; this ADR reintroduces it with git semantics.

## Decision

### Vocabulary: repository, not store

The domain concept is a **repository** (git vocabulary, ADR-0018), not
a "store". The remote surface uses it from the start - URLs, schemas,
and docs say repository (`repo` for short) - and the local user-facing
surface follows when this lands:

- CLI: `--store` becomes `--repo`; `sekai init` initializes a
  repository.
- JSON contract: `store` fields become `repo` (the `init` payload's
  `"store": ".sekai"` becomes `"repo": ".sekai"`).
- Library API: `SekaiStore`/`SekaiStoreOpener` become
  `SekaiRepository`/`SekaiRepositoryOpener`;
  `StorageError::StoreMissing` becomes `RepositoryMissing`.

The storage layer keeps its names (`sekai-storage`, `BlobStore`/
`MetaStore`/`CacheStore`): they name the persistence boundary, not the
domain concept, the way git's object database is not called a
repository. The on-disk layout is unchanged - a repository is still one
self-contained directory (`.sekai/` locally, `<root>/<owner>/<repo>/`
hosted) - so the rename is API-level only and bumps no schema version.

### Two layers: REST resources + sync verbs

Inspection and management are REST resources shaped like GitHub's API:
`/v1/repos/{owner}/{repo}` addressing with git-database sub-resources.
Bulk transfer is a pair of sync verbs shaped like git smart HTTP,
speaking JSON plus a typed binary object stream instead of pkt-line and
packfiles. A remote-as-storage-backend (implementing `BlobStore`/
`MetaStore` over HTTP) is rejected: sync needs two live repositories at
once, and `MetaStore`'s write semantics are local (a commit moves the
active ref). The remote is an app-level peer, not a backend.

### URI structure

The repository URL is the remote URL users write, e.g.
`http://host:8080/v1/repos/alice/survival`; everything
repository-scoped appends to it, the way git appends to `$GIT_URL`.
Versioning is in the path (`/v1`, GitLab style) - simpler than GitHub's
date headers while pre-release.

```text
GET  /v1/repos                           list hosted repositories (?owner=)
POST /v1/repos                           init {owner, name} (idempotent)
GET  /v1/repos/{owner}/{repo}            repository record

# repository base = /v1/repos/{owner}/{repo}
GET    {base}/info/refs                  advertisement (sync entry point)
POST   {base}/fetch                      want/have -> object stream
POST   {base}/push                       commands + object stream -> report

GET    {base}/refs                       all refs, name order
GET    {base}/refs/{name}                one ref (name may contain '/')
POST   {base}/refs                       create {name, commit}
PATCH  {base}/refs/{name}                move {commit, old, force?}
DELETE {base}/refs/{name}                delete (active ref protected)

GET    {base}/commits/{id}               commit object
GET    {base}/commits?ref=&limit=&after= history page (log)
GET    {base}/trees/{level}/{hash}       level = root | dim | region
GET    {base}/blobs/{hash}               raw payload (octet-stream)
HEAD   {base}/blobs/{hash}               existence check
PUT    {base}/blobs/{hash}               idempotent, verified CAS put
```

| Sekai endpoint                    | GitHub analogue                        | Smart HTTP analogue            |
| --------------------------------- | -------------------------------------- | ------------------------------ |
| `GET {base}/info/refs`            | `GET .../git/matching-refs`            | `info/refs?service=...`        |
| `POST {base}/fetch`               | -                                      | `POST .../git-upload-pack`     |
| `POST {base}/push`                | blob -> tree -> commit -> ref flow     | `POST .../git-receive-pack`    |
| `GET/PATCH {base}/refs/{name}`    | `GET/PATCH .../git/ref(s)/{ref}`       | -                              |
| `GET {base}/commits/{id}`         | `GET .../git/commits/{sha}`            | -                              |
| `GET {base}/trees/{level}/{hash}` | `GET .../git/trees/{sha}`              | -                              |
| `GET/PUT {base}/blobs/{hash}`     | `GET/POST .../git/blobs`               | -                              |

Tree and commit creation have no standalone REST write path: GitHub's
four-step flow collapses into `push`, because trees and commits must
land in one transaction with the ref moves that name them. `PUT
blobs/{hash}` stays: a lone CAS put is append-only and touches no
metadata, so it is safe on its own (tooling, small repairs).

### Multi-repository hosting

One `sekai serve` process hosts many repositories under `--root <dir>`.
`{owner}` and `{repo}` are single validated name components (ASCII
alphanumerics plus `.`, `_`, `-`; no separators, no traversal,
case-sensitive) resolved to `<root>/<owner>/<repo>/`. The owner is a
namespace, not an account: there is no user database or ownership
enforcement yet (auth stays with the reverse proxy); it exists so one
server can separate users' repositories the way GitHub URLs do. Each
repository stays the self-contained directory the local backend defines
(`meta.sqlite`, `blobs/`, `cache/`, `config.toml`).

`GET /v1/repos` enumerates hosted repositories (optionally filtered by
`?owner=`); `POST /v1/repos {owner, name}` runs the same explicit init
as `sekai init` (ADR-0026), idempotent, and reports `created`; `GET
/v1/repos/{owner}/{repo}` returns the repository record.

The server opens each repository once, on first use, and shares clones
of the handle across requests: opening takes the system-wide session
lock (ADR-0024, ADR-0029), so a hosted repository is owned by the server
process. A local `sekai` command cannot open the same directory while
it is served, and the server fails loudly (409) when another process
holds the lock. Repositories never share objects: CAS dedup is
per-repository, and moving history between repositories is a
clone/fetch/push, never a shared keyspace.

Requests to different repositories are fully independent. Within one
repository, reads run concurrently (the CAS is append-only and the
adapters are `Clone`), and concurrent pushes resolve transactionally at
the metadata commit: the CAS `old` check lets one push win a ref move
and rejects the other with 409, while content-addressed commit IDs keep
the objects both uploaded from colliding (ADR-0022). Each repository
carries its own schema gate and advertisement; a repository the server
cannot open fails per repository, never taking down its siblings.

### Schemas

JSON is snake_case with 64-char lowercase hex hashes and Unix-millis
timestamps, matching `docs/json.md` rather than GitHub's ISO 8601.
Records reuse the CLI payload shapes:

```jsonc
// GET /v1/repos/{owner}/{repo}
{"owner": "alice", "name": "survival", "schema_version": 17,
 "active": "main"}

// GET {base}/info/refs - the advertisement; clients check it first
{"protocol": 1, "repository": "alice/survival", "schema_version": 17,
 "hash": "blake3-256", "active": "main",
 "refs": [{"name": "main", "commit": "3f3f...3f", "active": true,
           "created_at_ms": 1700000000000}],
 "capabilities": []}

// GET {base}/commits/{id}
{"id": "3f3f...3f", "parents": ["2a2a...2a"],
 "created_at_ms": 1700000000000, "root": "9c9c...9c"}

// GET {base}/trees/region/{hash} (JSON view; Accept: application/octet-stream
// returns the canonical entry array - the exact hash preimage after the tag)
{"hash": "7b7b...7b", "level": "region",
 "entries": [{"cx": 0, "cz": 3, "chunk": "1d1d...1d"}]}

// POST {base}/fetch
{"want": ["3f3f...3f"], "have": ["2a2a...2a"]}

// POST {base}/push - commands record, then the objects they need
{"commands": [{"ref": "main", "old": "2a2a...2a", "new": "3f3f...3f",
               "force": false}]}

// errors: HTTP status + one body shape
{"code": "not_continuing", "message": "new commit does not continue ..."}
```

Status codes follow GitHub's split: 404 unknown repository/commit/ref/
blob, 409 conflicts (`not_continuing`, CAS `old` mismatch,
`active_ref_protected`, `unsupported_schema`, lock held), 422 validation
(bad hex, invalid owner/repo/ref name, hash mismatch, undecodable
payload). Blobs are raw `application/octet-stream`, never base64.
Content-addressed objects (blobs, trees, commits) are immutable: `ETag`
= hash and `Cache-Control: immutable` make them proxy/CDN-cacheable;
`info/refs`, refs, and log pages are `no-cache`.

### Object stream

`fetch` responses and `push` bodies share one framing: a sequence of
length-prefixed typed records - `kind u8 | hash 32B | len u64 BE |
payload`. Kinds: `commands` (push only, JSON), `blob`, `root`, `dim`,
`region`, `commit`. For every content kind the payload is the exact
hash preimage after its domain tag, so one rule verifies everything:
`hash_bytes(tag || payload) == hash` - except blobs, whose key is the
normalized digest: the server decodes the payload through `anvil` and
requires `Chunk::normalized_hash() == hash`, the same ingest rule, with
no raw-byte fallback. Per-object and per-request size caps bound
untrusted input. The codec is pure data and can live `no_std`; the HTTP
client/server around it is std + tokio.

### Sync semantics

- **fetch**: the server walks the `want` commits' full closure (commits,
  tree nodes, blobs), subtracts the `have` closure, and streams the
  difference. One round trip - no deltas, no negotiation rounds. The
  client verifies and inserts everything (CAS dedup makes retries and
  resumes free), then updates remote-tracking refs
  `remotes/<remote>/<name>`; local refs are never touched.
- **push**: the client streams a `commands` record plus the object
  closure its new tips need (minus what its remote-tracking refs already
  cover). The server verifies every object, syncs blobs, then records
  trees, commits, and all ref moves in **one transaction** - atomic,
  unlike git's default per-ref apply. Received commits never move refs
  implicitly; only explicit commands do, so the local invariant "a
  commit moves the active ref" stays a local-commit rule.
- **Ref commands are compare-and-swap**: `old` must equal the current
  target (`null` = unborn or absent) or the push is rejected (409).
  Updates are fast-forward by default (`new` descends from `old`);
  `force: true` rewinds explicitly, like `git push --force`. The active
  ref may be advanced by push (the central-repository case) but never
  deleted. `new: null` deletes a non-active ref.
- A rejected push records nothing; objects already uploaded remain as
  orphans, consistent with the append-only policy.

### Commands

| Command                          | Meaning                                                                 |
| -------------------------------- | ----------------------------------------------------------------------- |
| `sekai remote add/remove/list/set-url` | manage remotes in repository-local `config.toml`                  |
| `sekai clone <url> [<dir>]`      | init + fetch all refs + record `origin` + activate the remote's active ref + checkout its tip (`--no-checkout` skips the checkout) |
| `sekai fetch [<remote>]`         | objects + remote-tracking refs only                                     |
| `sekai pull [<remote>]`          | fetch + fast-forward the active ref; diverged histories fail loudly (no merge policy, ADR-0027); the world is untouched - checkout stays explicit |
| `sekai push [<remote>] [<ref>[:<remote-ref>]]` | default `origin`, active ref -> same name; `--force`, `--dry-run` (plan only) |
| `sekai serve --root <dir> --listen <addr>` | host repositories over HTTP                                     |

Remotes live in a config file beside `meta.sqlite`, never in the
metadata schema and never synced - git keeps remotes in config, not the
object database, and the schema holds source-of-truth history only.
Scopes, the cache keyspace, and the local record sequence never sync
either: filters and derived state are repository-local.

### Placement

A new sibling crate `crates/remote` (std + tokio) owns the wire codec,
the HTTP client, and the server adapter over an opened repository; it
depends on `storage`, `core`, `anvil` (blob verification), and `util`.
`app` orchestrates sync between a `SekaiInstance` and a remote client;
the binary stays a thin clap wrapper (`serve` included). `MetaStore`
grows a detached receive port - record trees and commits without ref
movement - implemented by backends; the schema itself is unchanged.
Pure crates keep their boundaries; the server adapter never supervises
processes.

## Consequences

- Git users get the vocabulary they already know (ADR-0018 pays off):
  the concept is a repository addressed as `{owner}/{repo}`, and
  `clone` returns meaning exactly what `git clone` means, closing
  ADR-0025's naming complaint.
- Breaking, pre-release: `--store` becomes `--repo`, JSON `store`
  fields become `repo`, and the `SekaiStore` trait family renames to
  `SekaiRepository`; `docs/json.md` and the golden tests change
  together. No schema bump - the on-disk layout is untouched.
- The REST layer is curl-debuggable and its immutable objects are
  cacheable; the sync layer is one round trip per direction and
  stateless on the server, so it load-balances like git smart HTTP.
- Atomic multi-ref push and CAS `old` checks prevent lost updates;
  fast-forward-by-default prevents silent history rewrites on a shared
  backup repository.
- Costs: a new crate and HTTP dependency surface, a `MetaStore` port
  addition, a repository config file, and new `docs/json.md` envelope
  commands (`remote`, `clone`, `fetch`, `pull`, `push`) with golden
  tests when implementation lands. The server pays one decompress +
  normalize per new blob on receive - the price of verifying a
  normalized key against untrusted bytes.
- A push may upload objects the server already holds (the client only
  knows its remote-tracking refs); CAS dedup absorbs the waste. A
  server-reported missing-hash list is a possible optimization, not
  part of this decision.
- Out of scope, as follow-ups: accounts and ownership enforcement
  behind `{owner}` (a namespace only for now), authentication beyond
  standard HTTP auth (Bearer via reverse proxy; no cookies, no server
  sessions), ssh and local-path transports, shallow/partial/scoped
  fetch, a merge policy for diverged pulls, repository deletion, and
  protocol v2 capability negotiation.
