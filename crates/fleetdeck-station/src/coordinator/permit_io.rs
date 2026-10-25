//! The per-binding permit of an HTTP reply, held until the reply bytes have
//! left the station's buffers (`docs/v2/design/mcp-p1.md` §3.1).
//!
//! `hyper` takes the last frame of a `Full` body into its write buffer and
//! flushes it later, so a permit that lives as long as the body is released
//! too early. [`PermitIo`] wraps the connection IO instead: the service puts
//! the permit of an authenticated request into the connection's
//! [`PermitSlot`] when it has built the response, and the slot is cleared
//! when the inner `poll_flush` completes, when the connection shuts down or
//! when the IO drops.
//!
//! The IO also ends a stalled write (MCP P4b §8.2): a write that stays
//! pending for [`WRITE_STALL`] fails with `TimedOut`, so `hyper` closes the
//! connection and a long-lived body and its permits drop.
//!
//! HTTP/1.1 serves one request at a time on a connection, so a permit in the
//! slot belongs to the response that is being written. `hyper` 1.12 encodes
//! the response into its write buffer and calls the inner `poll_flush` only
//! after the buffer drained (`proto/h1/dispatch.rs`, `conn.rs`): a flush that
//! completes with an empty buffer can only release a permit whose bytes are
//! already flushed.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use hyper::rt::{Read, ReadBufCursor, Write};
use tokio::sync::OwnedSemaphorePermit;
use tokio::time::Sleep;

/// A write that stays pending this long ends the connection.
pub(super) const WRITE_STALL: Duration = Duration::from_secs(30);

/// The permits that wait for the end of a write on one connection.
#[derive(Clone, Default)]
pub(super) struct PermitSlot(Arc<Mutex<Vec<OwnedSemaphorePermit>>>);

impl PermitSlot {
    /// Keeps `permit` until the connection flushed, shut down or dropped.
    pub(super) fn hold(&self, permit: OwnedSemaphorePermit) {
        self.lock().push(permit);
    }

    /// Drops every held permit.
    pub(super) fn clear(&self) {
        // The permits drop after the guard, outside the lock.
        let released = std::mem::take(&mut *self.lock());
        drop(released);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<OwnedSemaphorePermit>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The connection IO of the HTTP ingress: `inner` plus the permit slot.
pub(super) struct PermitIo<T> {
    inner: T,
    slot: PermitSlot,
    /// The deadline of the pending write, armed by its first `Pending`.
    stall: Option<Pin<Box<Sleep>>>,
}

impl<T> PermitIo<T> {
    pub(super) fn new(inner: T, slot: PermitSlot) -> Self {
        Self {
            inner,
            slot,
            stall: None,
        }
    }

    /// The result of a write poll of `inner`: a `Ready` result clears the
    /// deadline; a `Pending` one arms it once and fails the write when the
    /// deadline passed.
    fn guard_write<R>(
        &mut self,
        cx: &mut Context<'_>,
        polled: Poll<io::Result<R>>,
    ) -> Poll<io::Result<R>> {
        if polled.is_ready() {
            self.stall = None;
            return polled;
        }
        let stall = self
            .stall
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(WRITE_STALL)));
        if stall.as_mut().poll(cx).is_ready() {
            self.stall = None;
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::TimedOut)));
        }
        Poll::Pending
    }
}

impl<T> Drop for PermitIo<T> {
    fn drop(&mut self) {
        self.slot.clear();
    }
}

impl<T: Read + Unpin> Read for PermitIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T: Write + Unpin> Write for PermitIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_write(cx, buf);
        this.guard_write(cx, polled)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_write_vectored(cx, bufs);
        this.guard_write(cx, polled)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let polled = Pin::new(&mut self.inner).poll_flush(cx);
        if matches!(polled, Poll::Ready(Ok(()))) {
            self.slot.clear();
        }
        polled
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.slot.clear();
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
