//! HTTPS calls to Jira and Linear through `curl`.
//!
//! The URL, the method, the headers (with the auth header) and the body go
//! to curl's stdin as a config file (`curl -q -sS --fail-with-body -K -`), so
//! the token is never in the argv or the environment of the child (R3).
//! `-q` is the first argument: curl then ignores the user's `.curlrc`, which
//! could turn on redirect following. The config also says `no-location`.

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
    /// Pin the host of the URL to one address: the value of curl's
    /// `resolve` option, `host:443:address` (images).
    pub resolve: Option<String>,
    /// Append the content type, the status and the redirect target after the
    /// body ([`split_trailer`]). curl never follows a redirect.
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
    // Never follow a redirect: the caller checks each target itself.
    out.push_str("no-location\n");
    if let Some(pin) = &req.resolve {
        out.push_str(&format!("resolve = {}\n", quote(pin)));
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
        out.push_str(&format!(
            "write-out = {}\n",
            quote("\n%{content_type}\n%{http_code}\n%{redirect_url}")
        ));
    }
    out
}

/// What curl appends after the body of a request made with
/// `content_type: true`.
#[derive(Debug, PartialEq, Eq)]
pub struct Trailer {
    /// The content type, lower case, without parameters.
    pub content_type: String,
    /// The HTTP status; 0 when the trailer is missing or not a number.
    pub status: u16,
    /// The absolute target of a redirect; empty when there is none.
    pub redirect_url: String,
}

/// The body and the [`Trailer`] of an answer that was made with
/// `content_type: true`: the last three lines are the content type, the
/// status and the redirect target.
#[must_use]
pub fn split_trailer(stdout: &[u8]) -> (&[u8], Trailer) {
    let mut at = stdout.len();
    let mut cuts = [0_usize; 3];
    for cut in cuts.iter_mut().rev() {
        match stdout[..at].iter().rposition(|b| *b == b'\n') {
            Some(i) => {
                *cut = i;
                at = i;
            }
            None => {
                let none = Trailer {
                    content_type: String::new(),
                    status: 0,
                    redirect_url: String::new(),
                };
                return (stdout, none);
            }
        }
    }
    let text = |from: usize, to: usize| String::from_utf8_lossy(&stdout[from..to]).into_owned();
    let content_type = text(cuts[0] + 1, cuts[1])
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let status = text(cuts[1] + 1, cuts[2]).trim().parse().unwrap_or(0);
    let redirect_url = text(cuts[2] + 1, stdout.len()).trim().to_owned();
    (
        &stdout[..cuts[0]],
        Trailer {
            content_type,
            status,
            redirect_url,
        },
    )
}

/// The argv of every tracker call. `-q` must be first: curl then reads no
/// `.curlrc` (a `location` line there would make curl follow redirects).
pub(crate) const ARGS: [&str; 5] = ["-q", "-sS", "--fail-with-body", "-K", "-"];

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
        args: ARGS.map(str::to_owned).to_vec(),
        cwd: None,
        env: curl_env(),
        stdin: Some(config(req).into_bytes()),
        max_stdout,
        timeout: exec::CALL_TIMEOUT,
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
    fn the_config_pins_the_address_and_never_follows() {
        let req = Request {
            url: "https://cdn.example.test/a.png".into(),
            method: "GET",
            resolve: Some("cdn.example.test:443:93.184.216.34".into()),
            content_type: true,
            ..Request::default()
        };
        let text = config(&req);
        assert!(text.contains("resolve = \"cdn.example.test:443:93.184.216.34\"\n"));
        assert!(
            text.contains("write-out = \"\\n%{content_type}\\n%{http_code}\\n%{redirect_url}\"\n")
        );
        assert!(text.lines().any(|l| l == "no-location"), "{text}");
        assert!(!text.contains("max-redirs"));
        assert!(!config(&Request::default()).contains("resolve"));
    }

    #[test]
    fn the_trailer_is_split_from_the_body() {
        let (body, t) = split_trailer(b"\x89PNG\n\x00data\nImage/PNG; charset=binary\n200\n");
        assert_eq!(body, b"\x89PNG\n\x00data");
        assert_eq!(
            t,
            Trailer {
                content_type: "image/png".into(),
                status: 200,
                redirect_url: String::new(),
            }
        );
        let (body, t) = split_trailer(b"\ntext/html\n302\nhttps://b.test/x");
        assert!(body.is_empty());
        assert_eq!(
            (t.status, t.redirect_url.as_str()),
            (302, "https://b.test/x")
        );
        // Without the three trailer lines the whole answer is the body.
        let (body, t) = split_trailer(b"nothing\nimage/png");
        assert_eq!(body, b"nothing\nimage/png");
        assert_eq!((t.status, t.content_type.as_str()), (0, ""));
    }
}
