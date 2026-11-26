# ADR-0041: `sekai repo list` and the `GET /v1/repos` endpoint

- Status: Accepted
- Date: 2026-11-27

## Context

ADR-0035 lists `GET /v1/repos` (optionally filtered by owner) but the
worker serves only health and repository creation. ADR-0040 left the
collection endpoint out of scope. The next read-side operation is
listing hosted repositories, shaped after `gh repo list`. Constraints:

- The worker still has no storage bindings, so the listing cannot
  contain real records yet.
- The management surface spells the namespace `organization`, not
  ADR-0035's `{owner}` (ADR-0040); listing follows that spelling.
- Adding an operation changes the `repo` JSON payload contract
  (`docs/json.md` process), which needs an ADR.

## Decision

### CLI

`sekai repo list [ORGANIZATION] --remote <URL> [-L <int>] [--json]`

- The optional positional organization filters the listing. It validates
  at parse time through `sekai_util::RepoName` (the shared name grammar
  from ADR-0040) and travels as `?organization=<name>`.
- `--remote` is required and http(s)-only, exactly as for `repo create`.
  No local store is opened. `ls` is a visible alias for `list`, following
  `gh repo ls`.
- `-L`/`--limit` caps the listing. The default is 30; `0` lists every
  returned repository, matching `gh repo list`.
- `--json` emits the standard envelope with command `repo`; the result
  is an array of `{"organization", "name", "description"}` records.
  Human output prints one `organization/name` per line, followed by a
  tab and the description when present.
- gh's `--archived`, `--fork`, `--source`, `--visibility`,
  `--language`, and `--topic` filters are not adopted: sekai repository
  records carry none of that metadata. `--jq` and `--template` are not
  adopted: sekai has no formatting engine, and scripts can pipe
  `--json` through `jq`.
- `--limit` is applied by the CLI after decoding the response, because
  ADR-0035's collection endpoint defines only the organization filter.
  Server-side pagination is a follow-up.

### HTTP API

`GET /v1/repos[?organization=<name>]` answers 200 with a JSON array of
repository summaries. The worker logs every request and returns an empty
array for now: enumerating hosted repositories stays a `// TODO` until a
wasm storage backend is bound (ADR-0040). The route is registered beside
`POST /v1/repos`; wire DTOs remain duplicated between CLI and worker
until the `remote` crate's codec lands.

## Consequences

- `sekai repo list` is curl-debuggable end to end now and returns no
  rows until storage exists, matching the stubbed creation behavior.
- The CLI fetches the whole collection before applying `--limit`; this
  is acceptable while the API cannot return large collections and must
  become server-side pagination with the storage backend.
- Out of scope: pagination, sorting guarantees beyond the server's
  listing order, record fields beyond organization/name/description,
  authentication, and remote config files.