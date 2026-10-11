//! Bind, dial and accept with the D13 address rules and pinned-key mTLS.
//!
//! The address check runs before any socket operation: a public address never
//! gets a packet. The accept path checks the source address of the TCP peer
//! before the TLS handshake, and both handshakes have a timeout. The byte
//! transport is TCP, or FIFO pairs for loopback on hosts without
//! sockets ([`crate::transport`], DL-025).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ServerConfig};
use tokio::net::TcpStream;
use tokio_rustls::{TlsAcceptor, TlsConnector, client, server};

use crate::tls::SERVER_NAME;
use crate::transport::{Listener, Stream};
use fleetdeck_core::identity::PeerKey;
use fleetdeck_core::netscope::{AddrError, check_bind, check_dial, check_peer};

/// The longest time a TCP connect or a TLS handshake may take.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error(transparent)]
    Addr(#[from] AddrError),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("the TLS handshake took longer than {0:?}")]
    Timeout(Duration),
    #[error("the peer sent no certificate")]
    NoPeerCertificate,
    #[error("the peer certificate is not a node certificate: {0}")]
    PeerKey(String),
    #[error("the TCP connect to {0} reached its own socket (no listener on that port)")]
    SelfConnect(SocketAddr),
}

/// Opens a listener after the D13 bind check.
pub async fn bind(addr: SocketAddr) -> Result<Listener, NetError> {
    check_bind(addr)?;
    Ok(Listener::bind(addr).await?)
}

/// Opens a connection after the D13 dial check.
pub async fn dial_tcp(addr: SocketAddr) -> Result<Stream, NetError> {
    check_dial(addr)?;
    let stream = tokio::time::timeout(HANDSHAKE_TIMEOUT, Stream::connect(addr))
        .await
        .map_err(|_| NetError::Timeout(HANDSHAKE_TIMEOUT))??;
    if let Stream::Tcp(tcp) = &stream {
        refuse_self_connect(tcp)?;
    }
    Ok(stream)
}

/// TCP "simultaneous open": when no process listens on a loopback port, the
/// kernel can give the dialing socket that same port as its local port, and
/// the socket connects to itself. Such a socket would hold the port of the
/// restarting listener (its bind fails) and stall the TLS handshake until the
/// timeout. The dialer drops it at once and retries with backoff.
pub fn refuse_self_connect(stream: &TcpStream) -> Result<(), NetError> {
    let local = stream.local_addr()?;
    if local == stream.peer_addr()? {
        return Err(NetError::SelfConnect(local));
    }
    Ok(())
}

fn peer_of(
    certs: Option<&[rustls::pki_types::CertificateDer<'static>]>,
) -> Result<PeerKey, NetError> {
    let cert = certs
        .and_then(|c| c.first())
        .ok_or(NetError::NoPeerCertificate)?;
    PeerKey::from_cert(cert).map_err(|e| NetError::PeerKey(e.to_string()))
}

/// Dials `addr` and runs the TLS handshake. Returns the stream and the key
/// that the listener proved (already accepted by the config's policy).
pub async fn connect(
    addr: SocketAddr,
    config: Arc<ClientConfig>,
) -> Result<(client::TlsStream<Stream>, PeerKey), NetError> {
    let tcp = dial_tcp(addr).await?;
    let name = ServerName::try_from(SERVER_NAME)
        .map_err(|e| NetError::PeerKey(e.to_string()))?
        .to_owned();
    let tls = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        TlsConnector::from(config).connect(name, tcp),
    )
    .await
    .map_err(|_| NetError::Timeout(HANDSHAKE_TIMEOUT))??;
    let key = peer_of(tls.get_ref().1.peer_certificates())?;
    Ok((tls, key))
}

/// Accepts one connection: D13 source check, then the TLS handshake. Returns
/// the stream, the key that the dialer proved, and its address.
pub async fn accept(
    listener: &Listener,
    config: Arc<ServerConfig>,
) -> Result<(server::TlsStream<Stream>, PeerKey, SocketAddr), NetError> {
    let (tcp, from) = listener.accept().await?;
    let (tls, key) = handshake_accepted(tcp, from, config).await?;
    Ok((tls, key, from))
}

/// The server side of [`accept`] for a TCP connection that a listener loop
/// accepted: D13 source check, then the TLS handshake with a timeout.
pub async fn handshake_accepted(
    tcp: Stream,
    from: SocketAddr,
    config: Arc<ServerConfig>,
) -> Result<(server::TlsStream<Stream>, PeerKey), NetError> {
    check_peer(from)?;
    let tls = tokio::time::timeout(HANDSHAKE_TIMEOUT, TlsAcceptor::from(config).accept(tcp))
        .await
        .map_err(|_| NetError::Timeout(HANDSHAKE_TIMEOUT))??;
    let key = peer_of(tls.get_ref().1.peer_certificates())?;
    Ok((tls, key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    // These two tests are about TCP itself. Where the OS forbids IP sockets
    // (the fifo test transport, DL-025) they have nothing to check.

    #[tokio::test]
    async fn a_socket_connected_to_itself_is_refused() {
        // Reserve a free port, release it, then bind a dialing socket to it
        // and connect to the same address: Linux completes a self-connect.
        let Ok(probe) = TcpListener::bind("127.0.0.1:0").await else {
            eprintln!("skipped: this host forbids TCP sockets");
            return;
        };
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_reuseaddr(true).unwrap();
        socket.bind(addr).unwrap();
        let Ok(stream) = socket.connect(addr).await else {
            return; // this kernel refuses self-connects; nothing to guard
        };
        assert!(matches!(
            refuse_self_connect(&stream),
            Err(NetError::SelfConnect(a)) if a == addr
        ));
    }

    #[tokio::test]
    async fn a_normal_connection_is_kept() {
        let Ok(listener) = TcpListener::bind("127.0.0.1:0").await else {
            eprintln!("skipped: this host forbids TCP sockets");
            return;
        };
        let stream = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        assert!(refuse_self_connect(&stream).is_ok());
    }
}
