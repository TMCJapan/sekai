//! Cloudflare Workers host for the sekai remote sync HTTP API (ADR-0035).
//!
//! This is the edge deployment of the API, separate from `sekai serve`:
//! it compiles to `wasm32-unknown-unknown` on the Workers runtime, so there
//! is no tokio and no filesystem - repository state will come from platform
//! bindings through a wasm storage backend. `GET /health` and
//! `POST /v1/repos` exist so far; creation acknowledges the request but
//! runs no storage yet (ADR-0040). The rest of the `/v1/repos` surface
//! lands with the `remote` crate's wire codec.

use sekai_util::RepoName;
use serde::{Deserialize, Serialize};
use worker::{Context, Env, Request, Response, Result, Router, console_log, event};

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

/// `POST /v1/repos` request body (ADR-0040). The description is optional;
/// an absent one reads as `None`.
#[derive(Deserialize)]
struct CreateRepo {
    organization: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
}

/// The repository record a successful creation reports.
#[derive(Serialize)]
struct RepoRecord {
    organization: String,
    name: String,
    description: Option<String>,
    created: bool,
}

/// One error body shape for the whole API: HTTP status plus
/// `{"code", "message"}` (ADR-0035).
#[derive(Debug, Serialize)]
struct ApiError {
    code: &'static str,
    message: String,
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    Router::new()
        .get("/health", |_, _| {
            Response::from_json(&Health { status: "ok" })
        })
        .post_async("/v1/repos", |mut req, _ctx| async move {
            let body: CreateRepo = match req.json().await {
                Ok(body) => body,
                Err(err) => {
                    return api_error(400, "bad_request", format!("invalid JSON body: {err}"));
                }
            };
            console_log!(
                "POST /v1/repos: organization={:?} name={:?} description={:?}",
                body.organization,
                body.name,
                body.description
            );
            let (organization, name) = match validate_names(&body.organization, &body.name) {
                Ok(names) => names,
                Err(err) => return api_error(422, err.code, err.message),
            };
            // TODO: run the explicit init (ADR-0026) through a wasm storage
            // backend bound to this worker, idempotently, and report
            // `created: false` for an existing repository. Until then the
            // request is only logged and acknowledged.
            let record = RepoRecord {
                organization: organization.as_str().to_owned(),
                name: name.as_str().to_owned(),
                description: body.description,
                created: true,
            };
            Ok(Response::from_json(&record)?.with_status(201))
        })
        .run(req, env)
        .await
}

/// Check both address components before any state is touched. The CLI
/// validates the same grammar at parse time (`sekai_util::RepoName`).
fn validate_names(organization: &str, name: &str) -> Result<(RepoName, RepoName), ApiError> {
    let organization = RepoName::parse(organization).map_err(|err| ApiError {
        code: "invalid_name",
        message: format!("invalid organization: {err}"),
    })?;
    let name = RepoName::parse(name).map_err(|err| ApiError {
        code: "invalid_name",
        message: format!("invalid repository name: {err}"),
    })?;
    Ok((organization, name))
}

fn api_error(status: u16, code: &'static str, message: String) -> Result<Response> {
    Ok(Response::from_json(&ApiError { code, message })?.with_status(status))
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

    #[test]
    fn reads_create_repo_body() {
        let body: CreateRepo = serde_json::from_str(
            r#"{"organization":"my-org","name":"my-project","description":"My new repository"}"#,
        )
        .unwrap();
        assert_eq!(body.organization, "my-org");
        assert_eq!(body.name, "my-project");
        assert_eq!(body.description.as_deref(), Some("My new repository"));

        let body: CreateRepo =
            serde_json::from_str(r#"{"organization":"my-org","name":"my-project"}"#).unwrap();
        assert_eq!(body.description, None);

        assert!(serde_json::from_str::<CreateRepo>(r#"{"organization":"my-org"}"#).is_err());
    }

    #[test]
    fn renders_repo_record() {
        let record = RepoRecord {
            organization: "my-org".to_owned(),
            name: "my-project".to_owned(),
            description: Some("My new repository".to_owned()),
            created: true,
        };
        assert_eq!(
            serde_json::to_string(&record).unwrap(),
            r#"{"organization":"my-org","name":"my-project","description":"My new repository","created":true}"#
        );
        let record = RepoRecord {
            organization: "my-org".to_owned(),
            name: "my-project".to_owned(),
            description: None,
            created: true,
        };
        assert_eq!(
            serde_json::to_string(&record).unwrap(),
            r#"{"organization":"my-org","name":"my-project","description":null,"created":true}"#
        );
    }

    #[test]
    fn renders_api_error() {
        let error = ApiError {
            code: "invalid_name",
            message: "invalid organization: repository name is empty".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&error).unwrap(),
            r#"{"code":"invalid_name","message":"invalid organization: repository name is empty"}"#
        );
    }

    #[test]
    fn validates_both_address_components() {
        let (organization, name) = validate_names("my-org", "my-project").unwrap();
        assert_eq!(organization.as_str(), "my-org");
        assert_eq!(name.as_str(), "my-project");

        let err = validate_names("", "my-project").unwrap_err();
        assert_eq!(err.code, "invalid_name");
        assert_eq!(
            err.message,
            "invalid organization: repository name is empty"
        );

        let err = validate_names("my-org", "a/b").unwrap_err();
        assert_eq!(err.code, "invalid_name");
        assert_eq!(
            err.message,
            "invalid repository name: repository name holds invalid character: '/'"
        );

        let err = validate_names("my-org", "..").unwrap_err();
        assert_eq!(err.code, "invalid_name");
    }
}
