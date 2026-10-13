//! HTTPS calls to Jira and Linear through `curl`.
//!
//! The URL, the method, the headers (with the auth header) and the body go
//! to curl's stdin as a config file (`curl -sS --fail-with-body -K -`), so
//! the token is never in the argv or the environment of the child (R3).

use fleetdeck_proto::integrations::MAX_ASSET_BYTES;

use super::exec::{self, Fail, Programs, Spec, curl_env};

/// One HTTPS request.
#[derive(Debug, Default)]
pub struct Request {
    pub url: String,
    pub method: &'static str,
    /// `Name: value` header lines. They may hold a credential.
    pub headers: Vec<String>,
    pub body: Option<String>,
    /// Follow at most 3 https redirects (images).
    pub follow: bool,
    /// Append the content type after the body ([`split_content_type`]).
    pub content_type: bool,
}

/// `value` as a curl config string: quoted, with the escapes that the
/// config format knows.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{b}' => out.push_str("\\v"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The curl config of `req`.
#[must_use]
pub fn config(req: &Request) -> String {
    let mut out = String::new();
    out.push_str(&format!("proto = {}\n", quote("=https")));
    out.push_str("max-time = 20\n");
    if req.follow {
        out.push_str("location\nmax-redirs = 3\n");
        out.push_str(&format!("proto-redir = {}\n", quote("=https")));
    }
    out.push_str(&format!("url = {}\n", quote(&req.url)));
    out.push_str(&format!("request = {}\n", quote(req.method)));
    for header in &req.headers {
        out.push_str(&format!("header = {}\n", quote(header)));
    }
    if let Some(body) = &req.body {
        out.push_str(&format!("data-binary = {}\n", quote(body)));
    }
    if req.content_type {
        out.push_str(&format!("write-out = {}\n", quote("\n%{content_type}")));
    }
    out
}

/// The body and the content type of an answer that was made with
/// `content_type: true`.
#[must_use]
pub fn split_content_type(stdout: &[u8]) -> (&[u8], String) {
    match stdout.iter().rposition(|b| *b == b'\n') {
        Some(at) => (
            &stdout[..at],
            String::from_utf8_lossy(&stdout[at + 1..])
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase(),
        ),
        None => (stdout, String::new()),
    }
}

/// Sends `req`. A non-2xx answer is a [`Fail`] with the scrubbed body or
/// error text; `secrets` are values that must not appear in it.
///
/// # Errors
/// [`Fail`].
pub async fn send(
    programs: &Programs,
    req: &Request,
    secrets: &[&str],
    max_stdout: usize,
) -> Result<Vec<u8>, Fail> {
    let spec = Spec {
        program: programs.curl.clone(),
        args: ["-sS", "--fail-with-body", "-K", "-"]
            .map(str::to_owned)
            .to_vec(),
        cwd: None,
        env: curl_env(),
        stdin: Some(config(req).into_bytes()),
        max_stdout,
    };
    let out = exec::run(spec)
        .await
        .map_err(|e| exec::fail_of(&e, "curl"))?;
    if out.code == 0 {
        return Ok(out.stdout);
    }
    let body = String::from_utf8_lossy(&out.stdout);
    let detail = if out.stderr.is_empty() {
        body.to_string()
    } else {
        format!("{} {}", out.stderr, body)
    };
    // curl exit 28: the time limit.
    if out.code == 28 {
        return Err(Fail::new(
            fleetdeck_proto::integrations::code::TIMEOUT,
            "the server did not answer in time",
        ));
    }
    Err(Fail::new(
        fleetdeck_proto::integrations::code::UPSTREAM,
        exec::scrub(&detail, secrets),
    ))
}

/// The most bytes of an answer that [`send`] keeps for a JSON call.
pub const MAX_JSON: usize = 4 * 1024 * 1024;
/// The most bytes of an image answer, with room for the content type.
pub const MAX_IMAGE: usize = MAX_ASSET_BYTES + 256;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_holds_the_secret_header_and_escapes_the_body() {
        let req = Request {
            url: "https://acme.atlassian.net/rest/api/3/issue/P-1/comment".into(),
            method: "POST",
            headers: vec![
                "Authorization: Basic c2VjcmV0".into(),
                "Content-Type: application/json".into(),
            ],
            body: Some("{\"a\":\"line\\nbreak \\\"q\\\"\"}\n".into()),
            ..Request::default()
        };
        let text = config(&req);
        assert!(text.contains("header = \"Authorization: Basic c2VjcmV0\"\n"));
        assert!(text.contains("proto = \"=https\"\n"));
        assert!(text.contains("request = \"POST\"\n"));
        // A raw newline would end the option.
        let body_line = text.lines().find(|l| l.starts_with("data-binary")).unwrap();
        assert!(body_line.ends_with("\\n\""), "{body_line}");
        assert!(body_line.contains("\\\\n"), "{body_line}");
        assert!(!text.contains("redirs"));
    }

    #[test]
    fn the_content_type_is_split_from_the_body() {
        let (body, kind) = split_content_type(b"\x89PNG\n\x00data\nimage/png; charset=binary");
        assert_eq!(body, b"\x89PNG\n\x00data");
        assert_eq!(kind, "image/png");
        let (body, kind) = split_content_type(b"nothing");
        assert_eq!((body, kind.as_str()), (b"nothing".as_slice(), ""));
    }
}
