//! Bind, dial and accept with the D13 address rules and pinned-key mTLS.
//!
//! The address check runs before any socket operation: a public address never
//! gets a packet. The accept path checks the source address of the TCP peer
//! before the TLS handshake, and both handshakes have a timeout.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ServerConfig};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector, client, server};

use crate::addr::{AddrError, check_bind, check_dial, check_peer};
use crate::identity::PeerKey;
use crate::tls::SERVER_NAME;

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
}

/// Opens a listener after the D13 bind check.
pub async fn bind(addr: SocketAddr) -> Result<TcpListener, NetError> {
    check_bind(addr)?;
    Ok(TcpListener::bind(addr).await?)
}

/// Opens a TCP connection after the D13 dial check.
pub async fn dial_tcp(addr: SocketAddr) -> Result<TcpStream, NetError> {
    check_dial(addr)?;
    let stream = tokio::time::timeout(HANDSHAKE_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| NetError::Timeout(HANDSHAKE_TIMEOUT))??;
    stream.set_nodelay(true)?;
    Ok(stream)
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
) -> Result<(client::TlsStream<TcpStream>, PeerKey), NetError> {
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
    listener: &TcpListener,
    config: Arc<ServerConfig>,
) -> Result<(server::TlsStream<TcpStream>, PeerKey, SocketAddr), NetError> {
    let (tcp, from) = listener.accept().await?;
    let (tls, key) = handshake_accepted(tcp, from, config).await?;
    Ok((tls, key, from))
}

/// The server side of [`accept`] for a TCP connection that a listener loop
/// accepted: D13 source check, then the TLS handshake with a timeout.
pub async fn handshake_accepted(
    tcp: TcpStream,
    from: SocketAddr,
    config: Arc<ServerConfig>,
) -> Result<(server::TlsStream<TcpStream>, PeerKey), NetError> {
    check_peer(from)?;
    tcp.set_nodelay(true)?;
    let tls = tokio::time::timeout(HANDSHAKE_TIMEOUT, TlsAcceptor::from(config).accept(tcp))
        .await
        .map_err(|_| NetError::Timeout(HANDSHAKE_TIMEOUT))??;
    let key = peer_of(tls.get_ref().1.peer_certificates())?;
    Ok((tls, key))
}
