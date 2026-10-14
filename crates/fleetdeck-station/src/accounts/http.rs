//! HTTPS calls of the account service, through `curl` (DL-173).
//!
//! The URL, the headers and the body go to curl's stdin as a config file, as
//! in [`crate::integrations::http`]: no secret is in argv or in the
//! environment of the child. Unlike a tracker call, an error here keeps no
//! text of the upstream answer: it is a fixed message with the HTTP status
//! only.

use fleetdeck_proto::integrations::code;
use serde_json::Value;

use crate::integrations::exec::{self, Fail, Programs, Spec, curl_env};
use crate::integrations::http::{self, Request};

/// The most bytes of an answer.
const MAX_ANSWER: usize = 1024 * 1024;

/// The percent-encoding of `value` for a query string or a form body.
#[must_use]
pub fn pct(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A form body: `name=value&...`, each part percent-encoded.
#[must_use]
pub fn form(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", pct(k), pct(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The body that a `curl` call without `--fail` leaves, after the marker
/// that `write-out` adds: the HTTP status.
const MARKER: &str = "\nFD-STATUS:";

/// A failed call: the fixed [`Fail`] and the HTTP status (0 when the server
/// gave none).
#[derive(Debug)]
pub struct HttpFail {
    pub fail: Fail,
    pub status: u16,
}

impl From<Fail> for HttpFail {
    fn from(fail: Fail) -> Self {
        Self { fail, status: 0 }
    }
}

impl From<HttpFail> for Fail {
    fn from(e: HttpFail) -> Self {
        e.fail
    }
}

/// Sends `req` and returns the JSON answer of a 2xx status.
///
/// # Errors
/// [`HttpFail`] with a fixed message: `timeout`, `upstream` with the HTTP
/// status, or `bad_answer`.
pub async fn json(programs: &Programs, req: &Request) -> Result<Value, HttpFail> {
    let mut config = http::config(req);
    config.push_str("write-out = \"\\nFD-STATUS:%{http_code}\"\n");
    let spec = Spec {
        program: programs.curl.clone(),
        args: ["-q", "-sS", "-K", "-"].map(str::to_owned).to_vec(),
        cwd: None,
        env: curl_env(),
        stdin: Some(config.into_bytes()),
        max_stdout: MAX_ANSWER,
    };
    let out = exec::run(spec)
        .await
        .map_err(|e| HttpFail::from(exec::fail_of(&e, "curl")))?;
    if out.code == 28 {
        return Err(Fail::new(code::TIMEOUT, "the server did not answer in time").into());
    }
    if out.code != 0 {
        return Err(Fail::new(code::UPSTREAM, "the server could not be reached").into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, status) = text
        .rsplit_once(MARKER)
        .ok_or_else(|| HttpFail::from(Fail::bad_answer("the server answer is not valid")))?;
    let status: u16 = status.trim().parse().unwrap_or(0);
    if !(200..300).contains(&status) {
        return Err(HttpFail {
            fail: Fail::new(code::UPSTREAM, format!("the server answered HTTP {status}")),
            status,
        });
    }
    if body.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(body)
        .map_err(|_| HttpFail::from(Fail::bad_answer("the server answer is not JSON")))
}

/// A JSON POST with `body`.
pub async fn post_json(
    programs: &Programs,
    url: &str,
    headers: Vec<String>,
    body: &Value,
) -> Result<Value, HttpFail> {
    let mut headers = headers;
    headers.push("Accept: application/json".to_owned());
    headers.push("Content-Type: application/json".to_owned());
    json(
        programs,
        &Request {
            url: url.to_owned(),
            method: "POST",
            headers,
            body: Some(body.to_string()),
            ..Request::default()
        },
    )
    .await
}

/// A form POST.
pub async fn post_form(
    programs: &Programs,
    url: &str,
    fields: &[(&str, &str)],
) -> Result<Value, HttpFail> {
    json(
        programs,
        &Request {
            url: url.to_owned(),
            method: "POST",
            headers: vec![
                "Accept: application/json".to_owned(),
                "Content-Type: application/x-www-form-urlencoded".to_owned(),
            ],
            body: Some(form(fields)),
            ..Request::default()
        },
    )
    .await
}

/// A GET with `headers`.
pub async fn get(programs: &Programs, url: &str, headers: Vec<String>) -> Result<Value, HttpFail> {
    let mut headers = headers;
    headers.push("Accept: application/json".to_owned());
    json(
        programs,
        &Request {
            url: url.to_owned(),
            method: "GET",
            headers,
            ..Request::default()
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_percent_encoded() {
        assert_eq!(pct("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
        assert_eq!(pct("AZaz09-._~"), "AZaz09-._~");
        assert_eq!(form(&[("a", "1 2"), ("b&", "x")]), "a=1%202&b%26=x");
    }
}
