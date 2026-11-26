# ADR-0040: `sekai repo create` and the `POST /v1/repos` endpoint

- Status: Accepted
- Date: 2026-11-27

## Context

ADR-0035 designed the remote sync HTTP API but remains unimplemented;
the Cloudflare worker (`crates/remote-worker`) serves only `GET /health`.
The first user-facing remote operation is creating a hosted repository,
shaped after `gh repo create`. Constraints:

- Sekai has no account model and nowhere to store a username, so a
  repository address always spells both parts: `organization/name`.
  ADR-0035's `{owner}` segment is spelled `organization` on this surface.
- The worker has no storage bindings yet (no filesystem, no KV/D1/R2);
  actual creation must wait for a wasm storage backend.
- A new dependency needs an ADR (CONTRIBUTING). The CLI is std + tokio;
  the worker stays wasm-only. The JSON output contract gains a command
  (`docs/json.md` process).

## Decision

### CLI

`sekai repo create <organization/name> --remote <URL> [-d <text>] [--json]`

- The address is required in `organization/name` form. Both components
  validate at parse time (ADR-0013) through the new `sekai_util::RepoAddress`
  and `RepoName` types: 1-255 bytes of ASCII alphanumerics plus `.`, `_`,
  `-`; no `/` separator, no `.`/`..` traversal - ADR-0035's component
  grammar, shared by the CLI and the worker.
- `--remote` names the server base URL (http/https only, validated at
  parse time). There is no remote config yet; ADR-0035's repository-local
  `config.toml` lands with `remote add`. No local store is opened: like
  `gh repo create`, the command runs outside any repository.
- `-d`/`--description` is optional and omitted from the request when
  absent.
- gh's visibility flags (`--public`/`--private`/`--internal`), `--clone`,
  `--source`, `--push`, and template flags are not adopted: the API
  carries no visibility, and sync is unimplemented.
- `--json` emits the standard envelope with command `repo`; the result
  is the remote's repository record passed through unchanged.

### HTTP API

`POST /v1/repos` on the worker accepts
`{"organization": "...", "name": "...", "description": "..."}`
(description optional) and answers 201 with
`{"organization", "name", "description", "created"}`. Errors keep
ADR-0035's single body shape `{"code", "message"}`: 400 `bad_request`
for an unreadable body, 422 `invalid_name` for a failed component. The
handler logs every received request (`console_log!`) and leaves the
actual init as a `// TODO`: no storage runs until a wasm backend is
bound. `created` is always `true` until then; idempotency lands with
the backend.

### Dependencies

- `reqwest` 0.13 (default features off, `json` + `rustls-no-provider`)
  in the CLI only. The binary installs rustls' `ring` crypto provider:
  ring builds with cc alone on all six release targets, while the
  aws-lc-rs default would add a cmake build dependency. Requests carry
  a 30-second timeout.
- `rustls` 0.23 (`ring`, `std`, `tls12`) as the provider crate.
- `deny.toml` gains ISC (ring is Apache-2.0 AND ISC) and
  CDLA-Permissive-2.0 (`webpki-root-certs`, the Mozilla root-store data
  bundle behind rustls-platform-verifier).
- `sekai-remote-worker` depends on `sekai-util` for the shared name
  grammar: an inward edge, util stays a `no_std` leaf.

## Consequences

- The first remote operation is curl-debuggable end to end:
  `wrangler dev` plus
  `sekai repo create my-org/my-project --remote http://localhost:8787`.
- Wire DTOs are duplicated between the CLI and the worker for now; they
  move into the `remote` crate when ADR-0035's codec lands.
- `organization` deviates from ADR-0035's `{owner}` spelling; the sync
  surface adopts it when implemented (follow-up on ADR-0035).
- Out of scope: visibility and permissions, authentication, idempotent
  creation, `GET /v1/repos`, repository deletion, remote config files.