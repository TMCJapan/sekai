//! Cloudflare Workers host for the sekai remote sync HTTP API (ADR-0035).
//!
//! This is the edge deployment of the API, separate from `sekai serve`:
//! it compiles to `wasm32-unknown-unknown` on the Workers runtime, so there
//! is no tokio and no filesystem - repository state will come from platform
//! bindings through a wasm storage backend. `GET /health` and
//! `POST /v1/repos` exist so far; creation acknowledges the request but
//! runs no storage yet (ADR-0040). The rest of the `/v1/repos` surface
//! lands with the `remote` crate's wire codec.
//!
//! Layout: this file is only the fetch entry point. `routes` holds the
//! route map and one module per resource (handler next to its wire
//! types); `error` holds the API-wide error body every handler shares.

mod error;
mod routes;

use worker::{Context, Env, Request, Response, Result, event};

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    routes::router().run(req, env).await
}
