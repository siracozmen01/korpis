// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a connection is read through: [`Replay`] gives back the bytes read before the proxy
//! decided what to do with it, and [`Idle`] ends a connection that passes no byte either way for
//! as long as the bound allows (§3.2 of docs/07-networking.md).

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Instant, Sleep};

/// A stream that yields `prefix` before anything more of `inner`.
#[derive(Debug)]
pub struct Replay<S> {
    prefix: Vec<u8>,
    at: usize,
    inner: S,
}

impl<S> Replay<S> {
    /// `inner`, with `prefix` read from it already.
    pub const fn new(prefix: Vec<u8>, inner: S) -> Self {
        Self { prefix, at: 0, inner }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Replay<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let rest = this.prefix.get(this.at..).unwrap_or_default();
        if rest.is_empty() {
            return Pin::new(&mut this.inner).poll_read(cx, buf);
        }
        let n = rest.len().min(buf.remaining());
        buf.put_slice(rest.get(..n).unwrap_or_default());
        this.at += n;
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Replay<S> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, b: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, b)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// A stream that fails with `TimedOut` once `period` passes with no byte read or written.
#[derive(Debug)]
pub struct Idle<S> {
    inner: S,
    period: Duration,
    deadline: Pin<Box<Sleep>>,
}

impl<S> Idle<S> {
    /// `inner`, ended after `period` without a byte.
    pub fn new(inner: S, period: Duration) -> Self {
        Self { inner, period, deadline: Box::pin(tokio::time::sleep(period)) }
    }

    fn progressed(&mut self) {
        self.deadline.as_mut().reset(Instant::now() + self.period);
    }

    /// `Pending` while the deadline is ahead, and the error ending the connection once it passes.
    fn expired<T>(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<T>> {
        ready!(self.deadline.as_mut().poll(cx));
        Poll::Ready(Err(io::Error::new(io::ErrorKind::TimedOut, "idle")))
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Idle<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(r) => {
                this.progressed();
                Poll::Ready(r)
            }
            Poll::Pending => this.expired(cx),
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Idle<S> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, b: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_write(cx, b) {
            Poll::Ready(r) => {
                this.progressed();
                Poll::Ready(r)
            }
            Poll::Pending => this.expired(cx),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
