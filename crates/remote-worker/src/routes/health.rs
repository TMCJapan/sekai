//! `GET /health`: a liveness probe that touches no bindings.

use serde::Serialize;
use worker::{Request, Response, Result, RouteContext};

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

pub fn get(_: Request, _: RouteContext<()>) -> Result<Response> {
    Response::from_json(&Health { status: "ok" })
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
