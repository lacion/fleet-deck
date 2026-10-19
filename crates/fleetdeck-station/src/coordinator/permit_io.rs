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
//! HTTP/1.1 serves one request at a time on a connection, so a permit in the
//! slot belongs to the response that is being written. `hyper` 1.12 encodes
//! the response into its write buffer and calls the inner `poll_flush` only
//! after the buffer drained (`proto/h1/dispatch.rs`, `conn.rs`): a flush that
//! completes with an empty buffer can only release a permit whose bytes are
//! already flushed.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use hyper::rt::{Read, ReadBufCursor, Write};
use tokio::sync::OwnedSemaphorePermit;

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
}

impl<T> PermitIo<T> {
    pub(super) fn new(inner: T, slot: PermitSlot) -> Self {
        Self { inner, slot }
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
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
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
