# sekai-remote-worker

Cloudflare Workers host for the sekai remote sync HTTP API
([ADR-0035](../../docs/adr/0035-remote-sync-http-api.md)).

This is the edge deployment of the API, separate from the self-hosted
`sekai serve`: it compiles to `wasm32-unknown-unknown` with
[workers-rs](https://github.com/cloudflare/workers-rs) and runs on the
Workers runtime - no tokio, no filesystem. Repository state will come
from platform bindings through a wasm storage backend (future work);
the pure crates (`util`, `anvil`, `core`, `storage`) already lint clean
for wasm, so the wire codec and verification rules can be shared with
the native `remote` crate.

`GET /health`, `GET /v1/repos`, and `POST /v1/repos` are implemented so
far. Creation validates and logs the request but runs no storage yet -
the init is a `// TODO` until a wasm storage backend is bound
([ADR-0040](../../docs/adr/0040-repo-create.md)). Listing logs the
optional organization filter and returns an empty array, with the same
storage TODO ([ADR-0041](../../docs/adr/0041-repo-list.md)).

## Develop

Prerequisites: the `wasm32-unknown-unknown` target (pinned in the
workspace `rust-toolchain.toml`), [wrangler](https://developers.cloudflare.com/workers/wrangler/),
and `worker-build` (`cargo install worker-build`).

```sh
cd crates/remote-worker
wrangler dev    # builds via worker-build, serves on http://localhost:8787
curl http://localhost:8787/health   # {"status":"ok"}
curl -X POST http://localhost:8787/v1/repos \
  -H 'Content-Type: application/json' \
  -d '{"organization":"my-org","name":"my-project","description":"My new repository"}'
# 201 {"organization":"my-org","name":"my-project","description":"My new repository","created":true}
curl 'http://localhost:8787/v1/repos?organization=my-org'
# 200 [] (storage is stubbed)
```

## Deploy

```sh
wrangler deploy
```

## Notes

- The crate is a `cdylib` built by `worker-build` (see `[build]` in
  `wrangler.toml`); `build/` is generated output and git-ignored.
- `compatibility_date` is pinned to the newest date the local workerd
  supports; bump it together with wrangler upgrades.
- Release size tuning (`lto`, `codegen-units = 1`) is a workspace-wide
  profile decision, deferred until the API surface grows.
