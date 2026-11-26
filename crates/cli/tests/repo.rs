//! `sekai repo create` run as a process against a local mock remote, so
//! stdout, the exit code, and the request the remote receives are what a
//! script sees.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// One captured HTTP request: the request line, the content type, and
/// the body.
struct Captured {
    request_line: String,
    content_type: Option<String>,
    body: String,
}

/// A one-shot mock remote: accepts a single connection, captures the
/// request, and replies with the given status and JSON body. Returns
/// its base URL and the capture handle.
fn mock_remote(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        // A deadline keeps a buggy client from hanging the suite.
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "no request arrived");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("accept failed: {err}"),
            }
        };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut content_length = 0usize;
        let mut content_type = None;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line == "\n" {
                break;
            }
            let (name, value) = line.split_once(':').unwrap();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap();
            } else if name.eq_ignore_ascii_case("content-type") {
                content_type = Some(value.trim().to_owned());
            }
        }
        let mut request_body = vec![0u8; content_length];
        reader.read_exact(&mut request_body).unwrap();
        let response = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut stream = stream;
        stream.write_all(response.as_bytes()).unwrap();
        stream.flush().unwrap();
        Captured {
            request_line: request_line.trim().to_owned(),
            content_type,
            body: String::from_utf8(request_body).unwrap(),
        }
    });
    (url, handle)
}

fn sekai(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sekai"))
        .args(["--color", "never"])
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> &str {
    assert!(
        output.status.success(),
        "sekai failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::str::from_utf8(&output.stdout).unwrap()
}

#[test]
fn creates_a_repository_over_http() {
    let (url, server) = mock_remote(
        201,
        r#"{"organization":"my-org","name":"my-project","description":"My new repository","created":true}"#,
    );
    let output = sekai(&[
        "repo",
        "create",
        "my-org/my-project",
        "--remote",
        &url,
        "-d",
        "My new repository",
        "--json",
    ]);
    assert_eq!(
        stdout(&output),
        "{\"command\":\"repo\",\"status\":\"ok\",\"result\":{\"organization\":\"my-org\",\"name\":\"my-project\",\"description\":\"My new repository\",\"created\":true}}\n"
    );
    let captured = server.join().unwrap();
    assert_eq!(captured.request_line, "POST /v1/repos HTTP/1.1");
    assert_eq!(captured.content_type.as_deref(), Some("application/json"));
    assert_eq!(
        captured.body,
        r#"{"organization":"my-org","name":"my-project","description":"My new repository"}"#
    );
}

#[test]
fn omits_an_absent_description() {
    let (url, server) = mock_remote(
        201,
        r#"{"organization":"my-org","name":"my-project","description":null,"created":true}"#,
    );
    let output = sekai(&["repo", "create", "my-org/my-project", "--remote", &url]);
    // The human line names the remote as the URL normalizes it.
    assert_eq!(
        stdout(&output),
        format!("Created repository my-org/my-project on {url}/\n")
    );
    let captured = server.join().unwrap();
    assert_eq!(
        captured.body,
        r#"{"organization":"my-org","name":"my-project"}"#
    );
}

#[test]
fn a_remote_rejection_fails_with_an_error_envelope() {
    let (url, server) = mock_remote(
        422,
        r#"{"code":"invalid_name","message":"invalid repository name: repository name is empty"}"#,
    );
    let output = sekai(&[
        "repo",
        "create",
        "my-org/my-project",
        "--remote",
        &url,
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["command"], "repo");
    assert_eq!(envelope["status"], "error");
    let error = envelope["error"].as_str().unwrap();
    assert!(error.contains("HTTP 422"), "{error}");
    assert!(error.contains("invalid_name"), "{error}");
    assert!(
        error.contains("invalid repository name: repository name is empty"),
        "{error}"
    );
    server.join().unwrap();
}

#[test]
fn an_unreachable_remote_fails_with_an_error_envelope() {
    // Bind then drop: the port is almost certainly closed again.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let output = sekai(&[
        "repo",
        "create",
        "my-org/my-project",
        "--remote",
        &url,
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["command"], "repo");
    assert_eq!(envelope["status"], "error");
    assert!(
        envelope["error"]
            .as_str()
            .unwrap()
            .contains("failed to reach"),
        "{}",
        envelope["error"]
    );
}

#[test]
fn lists_repositories_over_http() {
    let body = r#"[{"organization":"my-org","name":"alpha","description":"First repository"},{"organization":"my-org","name":"beta","description":null}]"#;
    let (url, server) = mock_remote(200, body);
    let output = sekai(&["repo", "list", "--remote", &url, "--json"]);
    assert_eq!(
        stdout(&output),
        format!("{{\"command\":\"repo\",\"status\":\"ok\",\"result\":{body}}}\n")
    );
    let captured = server.join().unwrap();
    assert_eq!(captured.request_line, "GET /v1/repos HTTP/1.1");
}

#[test]
fn filters_by_organization_and_limits_the_listing() {
    let (url, server) = mock_remote(
        200,
        r#"[{"organization":"my-org","name":"alpha","description":"First repository"},{"organization":"my-org","name":"beta","description":null},{"organization":"my-org","name":"gamma","description":"Third repository"}]"#,
    );
    let output = sekai(&["repo", "list", "my-org", "--remote", &url, "--limit", "2"]);
    assert_eq!(
        stdout(&output),
        "my-org/alpha\tFirst repository\nmy-org/beta\n"
    );
    let captured = server.join().unwrap();
    assert_eq!(
        captured.request_line,
        "GET /v1/repos?organization=my-org HTTP/1.1"
    );
}

#[test]
fn a_zero_limit_lists_every_repository() {
    let (url, server) = mock_remote(
        200,
        r#"[{"organization":"my-org","name":"alpha","description":null},{"organization":"my-org","name":"beta","description":null}]"#,
    );
    let output = sekai(&["repo", "list", "--remote", &url, "-L", "0", "--json"]);
    assert_eq!(
        stdout(&output),
        "{\"command\":\"repo\",\"status\":\"ok\",\"result\":[{\"organization\":\"my-org\",\"name\":\"alpha\",\"description\":null},{\"organization\":\"my-org\",\"name\":\"beta\",\"description\":null}]}\n"
    );
    server.join().unwrap();
}

#[test]
fn a_listing_rejection_fails_with_an_error_envelope() {
    let (url, server) = mock_remote(
        503,
        r#"{"code":"unavailable","message":"storage is not bound"}"#,
    );
    let output = sekai(&["repo", "list", "--remote", &url, "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["command"], "repo");
    assert_eq!(envelope["status"], "error");
    let error = envelope["error"].as_str().unwrap();
    assert!(error.contains("HTTP 503"), "{error}");
    assert!(error.contains("unavailable"), "{error}");
    assert!(error.contains("storage is not bound"), "{error}");
    server.join().unwrap();
}

#[test]
fn an_unreachable_remote_fails_a_listing() {
    // Bind then drop: the port is almost certainly closed again.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let output = sekai(&["repo", "list", "--remote", &url, "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["command"], "repo");
    assert_eq!(envelope["status"], "error");
    assert!(
        envelope["error"]
            .as_str()
            .unwrap()
            .contains("failed to reach"),
        "{}",
        envelope["error"]
    );
}

#[test]
fn usage_errors_exit_two() {
    for args in [
        // Both address parts are required.
        vec![
            "repo",
            "create",
            "my-project",
            "--remote",
            "http://localhost:8787",
        ],
        // The remote is required.
        vec!["repo", "create", "my-org/my-project"],
        // The list remote is required too, and its organization must be
        // one valid name component.
        vec!["repo", "list"],
        vec![
            "repo",
            "list",
            "my org",
            "--remote",
            "http://localhost:8787",
        ],
        vec![
            "repo",
            "list",
            "--remote",
            "http://localhost:8787",
            "--limit",
            "-1",
        ],
    ] {
        let output = sekai(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
}
