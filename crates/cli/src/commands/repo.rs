//! Repo subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use reqwest::Url;
use sekai_util::RepoAddress;
use serde::{Deserialize, Serialize};

use crate::cli::RepoCommand;
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub async fn run(action: &RepoCommand, style: Styler) -> anyhow::Result<()> {
    match action {
        RepoCommand::Create {
            address,
            description,
            remote,
            json,
        } => create(address, description.as_deref(), remote, *json, style).await,
    }
}

async fn create(
    address: &RepoAddress,
    description: Option<&str>,
    remote: &Url,
    json: bool,
    style: Styler,
) -> anyhow::Result<()> {
    let request = CreateRepoRequest {
        organization: address.organization().as_str(),
        name: address.name().as_str(),
        description,
    };
    let response = client()?
        .post(repos_endpoint(remote)?)
        .json(&request)
        .send()
        .await
        .with_context(|| format!("repo create of {address} failed to reach {remote}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let message = match serde_json::from_str::<RemoteError>(&body) {
            Ok(err) => format!("{} ({})", err.message, err.code),
            Err(_) => body,
        };
        anyhow::bail!("repo create of {address} failed (HTTP {status}): {message}");
    }
    let record: RepoRecord = response
        .json()
        .await
        .with_context(|| format!("repo create of {address} got an unreadable response"))?;
    if json {
        println!("{}", envelope_ok("repo", &record)?);
        return Ok(());
    }
    if record.created {
        println!(
            "Created repository {} on {}",
            style.bold(&record.address()),
            remote
        );
    } else {
        println!(
            "Repository {} already exists on {}",
            style.bold(&record.address()),
            remote
        );
    }
    Ok(())
}

/// HTTP client for the remote API. reqwest's `rustls-no-provider`
/// feature leaves the crypto provider to the binary (ADR-0040): install
/// ring's once per process; a repeated install is a no-op.
fn client() -> anyhow::Result<reqwest::Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("failed to build the HTTP client")
}

/// The collection endpoint under the remote's base URL. A trailing
/// slash keeps `join` from dropping the base's last path segment, so a
/// reverse-proxied base like `http://host/sekai` works too.
fn repos_endpoint(base: &Url) -> anyhow::Result<Url> {
    let mut base = base.clone();
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    base.join("v1/repos").context("invalid remote URL")
}

/// `POST /v1/repos` request body (ADR-0040). An absent description is
/// omitted, not sent as null.
#[derive(Serialize)]
struct CreateRepoRequest<'a> {
    organization: &'a str,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
}

/// The repository record the remote reports, passed through to the JSON
/// envelope unchanged.
#[derive(Deserialize, Serialize)]
struct RepoRecord {
    organization: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    created: bool,
}

impl RepoRecord {
    /// The `organization/name` address for human output.
    fn address(&self) -> String {
        format!("{}/{}", self.organization, self.name)
    }
}

/// The remote's error body: HTTP status plus one `{"code", "message"}`
/// shape (ADR-0035).
#[derive(Deserialize)]
struct RemoteError {
    code: String,
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_create_request() {
        let request = CreateRepoRequest {
            organization: "my-org",
            name: "my-project",
            description: Some("My new repository"),
        };
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"organization":"my-org","name":"my-project","description":"My new repository"}"#
        );
        let request = CreateRepoRequest {
            organization: "my-org",
            name: "my-project",
            description: None,
        };
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"organization":"my-org","name":"my-project"}"#
        );
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
        assert_eq!(record.address(), "my-org/my-project");
    }

    #[test]
    fn reads_the_remote_error_body() {
        let err: RemoteError =
            serde_json::from_str(r#"{"code":"invalid_name","message":"bad name"}"#).unwrap();
        assert_eq!(err.code, "invalid_name");
        assert_eq!(err.message, "bad name");
    }

    #[test]
    fn joins_the_collection_endpoint_onto_the_base() {
        for (base, expected) in [
            ("http://localhost:8787", "http://localhost:8787/v1/repos"),
            ("http://localhost:8787/", "http://localhost:8787/v1/repos"),
            ("http://host/sekai", "http://host/sekai/v1/repos"),
            ("https://host/sekai/", "https://host/sekai/v1/repos"),
        ] {
            let base = Url::parse(base).unwrap();
            assert_eq!(repos_endpoint(&base).unwrap().as_str(), expected);
        }
    }
}
