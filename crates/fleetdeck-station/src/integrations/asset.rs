//! `issues.asset`: one image of an issue body, through the station.
//!
//! The deck holds no token, so the board asks the station for an image. The
//! station fetches only from the hosts that belong to the binding, only over
//! https, and answers only a PNG, JPEG, GIF or WebP of at most 5 MiB (the
//! bytes are checked, not the server's word). The auth header goes only to
//! the tracker's own host.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_proto::integrations::{Asset, MAX_ASSET_BYTES, code};

use super::exec::{Fail, Programs};
use super::http::{self, MAX_IMAGE, Request};
use super::util::{https_host, sniff_image};

/// Fetches `url` from one of `hosts` with `auth` (a header line and the
/// host that may receive it) when given.
///
/// # Errors
/// [`Fail`]: a URL outside `hosts`, a failed call, a body that is not an
/// image, or an image over the limit.
pub async fn fetch(
    programs: &Programs,
    url: &str,
    hosts: &[String],
    auth: Option<(&str, &str)>,
    secrets: &[&str],
) -> Result<Asset, Fail> {
    let host = https_host(url).ok_or_else(|| {
        Fail::new(
            code::INVALID_PARAMS,
            "the asset URL must be a plain https URL",
        )
    })?;
    if !hosts.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
        return Err(Fail::new(
            code::INVALID_PARAMS,
            "the asset URL is not on a host of this binding",
        ));
    }
    let headers = match auth {
        Some((header, only)) if only.eq_ignore_ascii_case(&host) => vec![header.to_owned()],
        _ => Vec::new(),
    };
    let request = Request {
        url: url.to_owned(),
        method: "GET",
        headers,
        body: None,
        follow: true,
        content_type: true,
    };
    let answer = http::send(programs, &request, secrets, MAX_IMAGE).await?;
    let (body, _) = http::split_content_type(&answer);
    if body.len() > MAX_ASSET_BYTES {
        return Err(Fail::bad_answer("the image is larger than 5 MiB"));
    }
    let content_type = sniff_image(body)
        .ok_or_else(|| Fail::bad_answer("the answer is not a PNG, JPEG, GIF or WebP image"))?;
    Ok(Asset {
        content_type: content_type.to_owned(),
        data_base64: STANDARD.encode(body),
    })
}

/// Fetches a public image: no credential is sent.
///
/// # Errors
/// See [`fetch`].
pub async fn public(programs: &Programs, url: &str, hosts: &[String]) -> Result<Asset, Fail> {
    fetch(programs, url, hosts, None, &[]).await
}
