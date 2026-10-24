//! The request body deadline of the board (FDV2-001).
//!
//! Hyper's header timer stops when the headers are complete. A client that
//! sends the headers of a POST with a `Content-Length` and then withholds
//! the body would hold its connection, and its admission permit, forever:
//! the JSON extractor waits for the body before any limiter runs. This
//! layer wraps every request body in a reader that fails after a deadline
//! that starts when the request reaches the router (the end of the
//! headers): 10 s for `/api/auth/*` and 60 s for every other route. A
//! timed-out request is answered `408 request_timeout` with
//! `Connection: close`, so the connection ends and its permit is released.
//! A request without a body (a WebSocket upgrade, a `GET`) is not wrapped.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use hyper::body::{Body as HttpBody, Frame, SizeHint};
use tokio::time::Sleep;

use super::guard::ApiError;

/// The body deadline of the authentication routes.
pub const AUTH_BODY_DEADLINE: Duration = Duration::from_secs(10);
/// The body deadline of every other route.
pub const BODY_DEADLINE: Duration = Duration::from_secs(60);

/// The deadline of the request `path`.
#[must_use]
pub fn deadline_of(path: &str) -> Duration {
    if path.starts_with("/api/auth/") {
        AUTH_BODY_DEADLINE
    } else {
        BODY_DEADLINE
    }
}

/// The error that a timed-out body read returns.
#[derive(Debug, thiserror::Error)]
#[error("the request body was not received in time")]
struct BodyTimeout;

/// A request body that fails when the deadline passes before its end.
struct Deadline {
    inner: Body,
    sleep: Pin<Box<Sleep>>,
    /// Set when the deadline passed; the layer reads it after the handler.
    fired: Arc<AtomicBool>,
    /// The body ended: the deadline does not matter any more.
    ended: bool,
}

impl HttpBody for Deadline {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let this = self.get_mut();
        if this.fired.load(Ordering::SeqCst) {
            return Poll::Ready(Some(Err(axum::Error::new(BodyTimeout))));
        }
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(None) => {
                this.ended = true;
                Poll::Ready(None)
            }
            Poll::Ready(other) => Poll::Ready(other),
            Poll::Pending => {
                if !this.ended && this.sleep.as_mut().poll(cx).is_ready() {
                    this.fired.store(true, Ordering::SeqCst);
                    return Poll::Ready(Some(Err(axum::Error::new(BodyTimeout))));
                }
                Poll::Pending
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// The layer: wraps the body, and answers `408 request_timeout` with
/// `Connection: close` when the deadline passed during the request.
pub async fn body_deadline(req: Request, next: Next) -> Response {
    if req.body().is_end_stream() {
        return next.run(req).await;
    }
    let limit = deadline_of(req.uri().path());
    let (parts, body) = req.into_parts();
    let fired = Arc::new(AtomicBool::new(false));
    let body = Body::new(Deadline {
        inner: body,
        sleep: Box::pin(tokio::time::sleep(limit)),
        fired: Arc::clone(&fired),
        ended: false,
    });
    let mut response = next.run(Request::from_parts(parts, body)).await;
    if fired.load(Ordering::SeqCst) {
        response = ApiError::new(
            StatusCode::REQUEST_TIMEOUT,
            "request_timeout",
            "the request body was not received in time",
        )
        .into_response();
        response
            .headers_mut()
            .insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_auth_routes_get_10_seconds_and_every_other_route_60() {
        assert_eq!(deadline_of("/api/auth/login"), Duration::from_secs(10));
        assert_eq!(deadline_of("/api/auth/link"), Duration::from_secs(10));
        assert_eq!(
            deadline_of("/api/stations/a/paste"),
            Duration::from_secs(60)
        );
        assert_eq!(deadline_of("/api/auth"), Duration::from_secs(60));
        assert_eq!(deadline_of("/"), Duration::from_secs(60));
    }
}
