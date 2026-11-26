//! The route map of the HTTP API (ADR-0035).
//!
//! One module per resource: a handler lives next to its wire types, and
//! new endpoints register here. The repository surface grows into
//! `repos` (read, delete, sync push/pull) as ADR-0035 lands; wire DTOs
//! move into the `remote` crate's codec when it exists (ADR-0040).

mod health;
mod repos;

use worker::Router;

/// The whole API surface. Handlers are plain functions, so the map
/// stays readable as the surface grows.
pub fn router() -> Router<'static, ()> {
    Router::new()
        .get("/health", health::get)
        .get_async("/v1/repos", repos::list)
        .post_async("/v1/repos", repos::create)
}
