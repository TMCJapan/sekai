//! Repository endpoints under `/v1/repos` (ADR-0035, ADR-0040).
//!
//! Creation validates and logs the request but runs no storage yet:
//! the init is a TODO until a wasm storage backend is bound. The wire
//! DTOs live here until the `remote` crate's codec lands (ADR-0040).

use sekai_util::RepoName;
use serde::{Deserialize, Serialize};
use worker::{Request, Response, Result, RouteContext, console_log};

use crate::error::ApiError;

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

/// One repository in a `GET /v1/repos` listing (ADR-0041). Unlike
/// [`RepoRecord`], it carries no create-only `created` flag.
#[derive(Serialize)]
struct RepoSummary {
    organization: String,
    name: String,
    description: Option<String>,
}

/// List hosted repositories, optionally filtered by `?organization=`.
/// Every received request is logged so receipt is verifiable while the
/// storage side is still stubbed.
pub async fn list(req: Request, _ctx: RouteContext<()>) -> Result<Response> {
    let organization = req
        .url()?
        .query_pairs()
        .find_map(|(key, value)| (key == "organization").then(|| value.into_owned()));
    console_log!("GET /v1/repos: organization={organization:?}");
    // TODO: enumerate hosted repositories through a wasm storage backend,
    // honoring the organization filter and returning records in address
    // order. Until then the listing is empty.
    let repositories: Vec<RepoSummary> = Vec::new();
    Response::from_json(&repositories)
}

/// Create a repository. Every received request is logged so receipt is
/// verifiable while the storage side is still stubbed.
pub async fn create(mut req: Request, _ctx: RouteContext<()>) -> Result<Response> {
    let body: CreateRepo = match req.json().await {
        Ok(body) => body,
        Err(err) => {
            return ApiError {
                code: "bad_request",
                message: format!("invalid JSON body: {err}"),
            }
            .respond(400);
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
        Err(err) => return err.respond(422),
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn renders_repo_summary() {
        let summary = RepoSummary {
            organization: "my-org".to_owned(),
            name: "my-project".to_owned(),
            description: Some("My new repository".to_owned()),
        };
        assert_eq!(
            serde_json::to_string(&summary).unwrap(),
            r#"{"organization":"my-org","name":"my-project","description":"My new repository"}"#
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
