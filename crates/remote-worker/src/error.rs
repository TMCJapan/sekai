//! The API-wide error body: HTTP status plus `{"code", "message"}`
//! (ADR-0035). Every handler reports failures through this one shape.

use serde::Serialize;
use worker::{Response, Result};

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    /// Render the body with its HTTP status.
    pub fn respond(self, status: u16) -> Result<Response> {
        Ok(Response::from_json(&self)?.with_status(status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
