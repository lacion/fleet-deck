//! `issues.asset`: one image of an issue body, through the station.
//!
//! The deck holds no token, so the board asks the station for an image. The
//! station fetches only from the hosts that belong to the binding, only over
//! https, and answers only a PNG, JPEG, GIF or WebP of at most 5 MiB (the
//! bytes are checked, not the server's word). The auth header goes only to
//! the tracker's own host. A redirect is followed by the station, at most 3
//! times, and only to a host of the binding or to a host whose addresses are
//! all public (then without any auth header).

use std::net::IpAddr;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_core::netscope::is_public;
use fleetdeck_proto::integrations::{Asset, MAX_ASSET_BYTES, code};

use super::exec::{Fail, Programs};
use super::http::{self, MAX_IMAGE, Request};
use super::util::{https_host, sniff_image};

/// The most redirects that one fetch follows.
const MAX_REDIRECTS: usize = 3;
/// The longest DNS lookup of a redirect target.
const LOOKUP_WAIT: Duration = Duration::from_secs(20);

/// Fetches `url` from one of `hosts` with `auth` (a header line and the
/// host that may receive it) when given.
///
/// curl never follows a redirect. The station follows at most
/// [`MAX_REDIRECTS`] itself and checks every target (FDV2-004): a host of
/// `hosts` is allowed, with the auth header only for the auth host; any other
/// host is allowed only when every address it resolves to is public, and
/// curl is then pinned to the first address. Such a request has no auth
/// header.
///
/// # Errors
/// [`Fail`]: a URL outside `hosts`, a redirect to a host that is not
/// allowed, too many redirects, a failed call, a body that is not an image,
/// or an image over the limit.
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
    if !in_hosts(hosts, &host) {
        return Err(Fail::new(
            code::INVALID_PARAMS,
            "the asset URL is not on a host of this binding",
        ));
    }
    let mut request = Request {
        url: url.to_owned(),
        method: "GET",
        headers: auth_headers(auth, &host),
        content_type: true,
        ..Request::default()
    };
    let mut redirects = 0;
    let answer = loop {
        let answer = http::send(programs, &request, secrets, MAX_IMAGE).await?;
        let (body, trailer) = http::split_trailer(&answer);
        if !is_redirect(trailer.status) || trailer.redirect_url.is_empty() {
            break answer[..body.len()].to_vec();
        }
        if redirects == MAX_REDIRECTS {
            return Err(Fail::new(
                code::INVALID_PARAMS,
                "the image has too many redirects",
            ));
        }
        redirects += 1;
        request = next_request(&trailer.redirect_url, hosts, auth).await?;
    };
    if answer.len() > MAX_ASSET_BYTES {
        return Err(Fail::bad_answer("the image is larger than 5 MiB"));
    }
    let content_type = sniff_image(&answer)
        .ok_or_else(|| Fail::bad_answer("the answer is not a PNG, JPEG, GIF or WebP image"))?;
    Ok(Asset {
        content_type: content_type.to_owned(),
        data_base64: STANDARD.encode(&answer),
    })
}

fn in_hosts(hosts: &[String], host: &str) -> bool {
    hosts.iter().any(|h| h.eq_ignore_ascii_case(host))
}

/// The auth header, only for the host that may receive it.
fn auth_headers(auth: Option<(&str, &str)>, host: &str) -> Vec<String> {
    match auth {
        Some((header, only)) if only.eq_ignore_ascii_case(host) => vec![header.to_owned()],
        _ => Vec::new(),
    }
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn not_allowed() -> Fail {
    Fail::new(
        code::INVALID_PARAMS,
        "the image redirects to a host that is not allowed",
    )
}

/// The explicit port of an `https://` URL, when it has one.
fn explicit_port(url: &str) -> Option<&str> {
    let authority = url
        .strip_prefix("https://")?
        .split(['/', '?', '#'])
        .next()?;
    authority.split_once(':').map(|(_, port)| port)
}

/// The request for a redirect `target`, or the reason that it is refused.
async fn next_request(
    target: &str,
    hosts: &[String],
    auth: Option<(&str, &str)>,
) -> Result<Request, Fail> {
    let host = https_host(target).ok_or_else(not_allowed)?;
    let mut request = Request {
        url: target.to_owned(),
        method: "GET",
        content_type: true,
        ..Request::default()
    };
    if in_hosts(hosts, &host) {
        request.headers = auth_headers(auth, &host);
        return Ok(request);
    }
    if explicit_port(target).is_some_and(|port| port != "443") {
        return Err(not_allowed());
    }
    let ip = first_public_address(&host).await.ok_or_else(not_allowed)?;
    request.resolve = Some(match ip {
        IpAddr::V4(v4) => format!("{host}:443:{v4}"),
        IpAddr::V6(v6) => format!("{host}:443:[{v6}]"),
    });
    Ok(request)
}

/// The first address of `host` when every address it resolves to is public;
/// `None` when the lookup fails or gives none, or one address is not public.
async fn first_public_address(host: &str) -> Option<IpAddr> {
    let lookup = tokio::net::lookup_host((host, 443));
    let addrs: Vec<IpAddr> = match tokio::time::timeout(LOOKUP_WAIT, lookup).await {
        Ok(Ok(found)) => found.map(|a| a.ip()).collect(),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "the redirect target does not resolve");
            return None;
        }
        Err(_) => {
            tracing::warn!("the lookup of the redirect target timed out");
            return None;
        }
    };
    if addrs.iter().all(|ip| is_public(*ip)) {
        addrs.first().copied()
    } else {
        None
    }
}

/// Fetches a public image: no credential is sent.
///
/// # Errors
/// See [`fetch`].
pub async fn public(programs: &Programs, url: &str, hosts: &[String]) -> Result<Asset, Fail> {
    fetch(programs, url, hosts, None, &[]).await
}

#[cfg(test)]
#[path = "asset_redirect_tests.rs"]
mod redirect_tests;
