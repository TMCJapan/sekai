//! Cloudflare Workers host for the sekai remote sync HTTP API (ADR-0035).
//!
//! This is the edge deployment of the API, separate from `sekai serve`:
//! it compiles to `wasm32-unknown-unknown` on the Workers runtime, so there
//! is no tokio and no filesystem - repository state will come from platform
//! bindings through a wasm storage backend. Only `/health` exists so far;
//! the `/v1/repos` surface lands with the `remote` crate's wire codec.

use serde::Serialize;
use worker::{Context, Env, Request, Response, Result, Router, event};

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    Router::new()
        .get("/health", |_, _| {
            Response::from_json(&Health { status: "ok" })
        })
        .run(req, env)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_health_payload() {
        assert_eq!(
            serde_json::to_string(&Health { status: "ok" }).unwrap(),
            r#"{"status":"ok"}"#
        );
    }
}
