//! Byte transport under TLS: TCP, or named pipes (FIFOs) as a loopback
//! stand-in for hosts without sockets.
//!
//! The link protocol runs over TCP. The reviewer sandbox forbids every socket
//! operation (`socket(AF_INET)`, and `bind`/`connect` of Unix sockets), but it
//! allows named pipes. For such hosts the spike carries **loopback**
//! addresses over FIFO pairs: everything above the byte stream (D13 checks on
//! the logical address, TLS, WebSockets, the link protocol) stays the same,
//! and separate processes still connect (the multi-process tests).
//!
//! Selection, read once per process from `FD_LINK_TRANSPORT` (DL-025):
//!
//! | Value | Transport |
//! | --- | --- |
//! | unset or `tcp` | TCP (the product behaviour) |
//! | `fifo:<dir>` | FIFOs under `<dir>` |
//! | `auto` | TCP if the OS allows an IP socket, else FIFOs under `<tmp>/fd-link-net` |
//!
//! The workspace `.cargo/config.toml` sets `auto` for cargo-run tests and
//! binaries, so the tests use TCP wherever TCP exists.
//!
//! FIFO connection setup: the listener of `127.0.0.1:41000` reads connection
//! ids from `<dir>/127.0.0.1_41000.d/accept`. A dialer makes the two FIFOs
//! `<id>.c2s` and `<id>.s2c`, opens its read end, writes `<id>\n` to the
//! accept FIFO (one write below `PIPE_BUF` is atomic), and opens its write
//! end when the listener has opened the other side.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::unix::pipe;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

/// Environment variable that selects the transport.
pub const ENV: &str = "FD_LINK_TRANSPORT";

/// How long a FIFO dialer waits for the listener to open its side.
const FIFO_OPEN_LIMIT: Duration = Duration::from_secs(5);

/// The byte transport of this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Tcp,
    /// Loopback addresses as FIFO pairs under this directory.
    Fifo(PathBuf),
}

impl Transport {
    /// Parses a `FD_LINK_TRANSPORT` value. `auto` probes the OS.
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim) {
            None | Some("") | Some("tcp") => Ok(Self::Tcp),
            Some("auto") => Ok(if tcp_permitted() {
                Self::Tcp
            } else {
                Self::Fifo(std::env::temp_dir().join("fd-link-net"))
            }),
            Some(v) => match v.strip_prefix("fifo:") {
                Some(dir) if !dir.is_empty() => Ok(Self::Fifo(PathBuf::from(dir))),
                _ => Err(format!(
                    "{ENV}: expected tcp, auto or fifo:<dir>, got {v:?}"
                )),
            },
        }
    }

    /// The transport of this process (first call reads the environment). An
    /// invalid value falls back to TCP, which needs no setup.
    pub fn current() -> &'static Transport {
        static SELECTED: OnceLock<Transport> = OnceLock::new();
        SELECTED.get_or_init(|| {
            let value = std::env::var(ENV).ok();
            Transport::parse(value.as_deref()).unwrap_or(Transport::Tcp)
        })
    }
}

/// True when this process may create an IPv4 socket.
pub fn tcp_permitted() -> bool {
    match std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
        Ok(_) => true,
        Err(e) => e.kind() != io::ErrorKind::PermissionDenied,
    }
}

fn loopback_only(addr: SocketAddr) -> io::Result<()> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("the fifo test transport carries loopback addresses only, not {addr}"),
        ))
    }
}

/// Directory of a loopback address: `127.0.0.1:41000` → `<dir>/127.0.0.1_41000.d`.
pub fn address_dir(dir: &Path, addr: SocketAddr) -> PathBuf {
    let name: String = addr
        .to_string()
        .chars()
        .map(|c| if matches!(c, '[' | ']' | ':') { '_' } else { c })
        .collect();
    dir.join(format!("{name}.d"))
}

fn is_no_reader(e: &io::Error) -> bool {
    e.raw_os_error() == Some(rustix::io::Errno::NXIO.raw_os_error())
}

/// The FIFO transport is a test fallback for Linux sandboxes that deny IP
/// sockets. Other systems (macOS) build it but refuse to use it.
#[cfg(target_os = "linux")]
fn make_fifo(path: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, Mode, mkfifoat};
    mkfifoat(CWD, path, Mode::from_raw_mode(0o600)).map_err(io::Error::from)
}

#[cfg(not(target_os = "linux"))]
fn make_fifo(_path: &Path) -> io::Result<()> {
    Err(fifo_unsupported())
}

#[cfg(not(target_os = "linux"))]
fn fifo_unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "the fifo link transport needs Linux; use FD_LINK_TRANSPORT=tcp",
    )
}

/// Opens the accept FIFO read-write, so it never reports EOF between dialers.
#[cfg(target_os = "linux")]
fn open_accept(accept: &Path) -> io::Result<pipe::Receiver> {
    pipe::OpenOptions::new()
        .read_write(true)
        .open_receiver(accept)
}

#[cfg(not(target_os = "linux"))]
fn open_accept(_accept: &Path) -> io::Result<pipe::Receiver> {
    Err(fifo_unsupported())
}

fn connection_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(
        "{}-{}-{nanos}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// A connected byte stream.
#[derive(Debug)]
pub enum Stream {
    Tcp(TcpStream),
    /// Read end and write end; `None` after `shutdown` (the peer reads EOF).
    Fifo(pipe::Receiver, Option<pipe::Sender>),
}

impl Stream {
    /// Opens a connection to `addr` with the current transport. The caller
    /// does the D13 check first.
    pub async fn connect(addr: SocketAddr) -> io::Result<Stream> {
        match Transport::current() {
            Transport::Tcp => {
                let s = TcpStream::connect(addr).await?;
                s.set_nodelay(true)?;
                Ok(Stream::Tcp(s))
            }
            Transport::Fifo(dir) => fifo_connect(dir, addr).await,
        }
    }
}

fn refused(detail: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionRefused, detail.to_string())
}

async fn fifo_connect(dir: &Path, addr: SocketAddr) -> io::Result<Stream> {
    loopback_only(addr)?;
    let conn_dir = address_dir(dir, addr);
    let accept = conn_dir.join("accept");
    if !accept.exists() {
        return Err(refused(format!("nobody listens on {addr}")));
    }
    let id = connection_id();
    let c2s = conn_dir.join(format!("{id}.c2s"));
    let s2c = conn_dir.join(format!("{id}.s2c"));
    make_fifo(&c2s)?;
    make_fifo(&s2c)?;
    let r = async {
        let rx = pipe::OpenOptions::new().open_receiver(&s2c)?;
        let mut announce = match pipe::OpenOptions::new().open_sender(&accept) {
            Ok(s) => s,
            Err(e) if is_no_reader(&e) => {
                return Err(refused(format!("nobody listens on {addr}")));
            }
            Err(e) => return Err(e),
        };
        announce.write_all(format!("{id}\n").as_bytes()).await?;
        drop(announce);
        let deadline = tokio::time::Instant::now() + FIFO_OPEN_LIMIT;
        let tx = loop {
            match pipe::OpenOptions::new().open_sender(&c2s) {
                Ok(tx) => break tx,
                Err(e) if is_no_reader(&e) && tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                Err(e) if is_no_reader(&e) => {
                    return Err(refused(format!("{addr} did not accept the connection")));
                }
                Err(e) => return Err(e),
            }
        };
        Ok(Stream::Fifo(rx, Some(tx)))
    }
    .await;
    let _ = std::fs::remove_file(&c2s);
    let _ = std::fs::remove_file(&s2c);
    r
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Fifo(rx, _) => Pin::new(rx).poll_read(cx, buf),
        }
    }
}

fn shut() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the write end is shut down")
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Fifo(_, Some(tx)) => Pin::new(tx).poll_write(cx, buf),
            Stream::Fifo(_, None) => Poll::Ready(Err(shut())),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_flush(cx),
            Stream::Fifo(_, Some(tx)) => Pin::new(tx).poll_flush(cx),
            Stream::Fifo(_, None) => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Fifo(_, tx) => {
                // Closing the write end is what the peer sees as EOF.
                *tx = None;
                Poll::Ready(Ok(()))
            }
        }
    }
}

/// A listening socket (or FIFO accept point) with its logical address.
#[derive(Debug)]
pub struct Listener {
    inner: ListenerInner,
    local: SocketAddr,
}

#[derive(Debug)]
enum ListenerInner {
    Tcp(TcpListener),
    Fifo {
        ids: Mutex<BufReader<pipe::Receiver>>,
        dir: PathBuf,
    },
}

impl Listener {
    /// Binds `addr` with the current transport. The caller does the D13
    /// check first. Port 0 picks a free port, as with TCP.
    pub async fn bind(addr: SocketAddr) -> io::Result<Listener> {
        match Transport::current() {
            Transport::Tcp => {
                let l = TcpListener::bind(addr).await?;
                let local = l.local_addr()?;
                Ok(Listener {
                    inner: ListenerInner::Tcp(l),
                    local,
                })
            }
            Transport::Fifo(dir) => bind_fifo(dir, addr),
        }
    }

    /// The logical address that peers dial.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local)
    }

    /// Accepts one connection. A FIFO peer has no IP address; it is local by
    /// construction and is reported as `127.0.0.1:0`.
    pub async fn accept(&self) -> io::Result<(Stream, SocketAddr)> {
        match &self.inner {
            ListenerInner::Tcp(l) => {
                let (s, from) = l.accept().await?;
                s.set_nodelay(true)?;
                Ok((Stream::Tcp(s), from))
            }
            ListenerInner::Fifo { ids, dir } => loop {
                let mut line = String::new();
                if ids.lock().await.read_line(&mut line).await? == 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "accept fifo"));
                }
                let id = line.trim();
                if id.is_empty() || id.contains(['/', '.']) {
                    continue;
                }
                // A dialer that gave up removed its FIFOs: skip it.
                let Ok(rx) = pipe::OpenOptions::new().open_receiver(dir.join(format!("{id}.c2s")))
                else {
                    continue;
                };
                let Ok(tx) = pipe::OpenOptions::new().open_sender(dir.join(format!("{id}.s2c")))
                else {
                    continue;
                };
                let from = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
                return Ok((Stream::Fifo(rx, Some(tx)), from));
            },
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let ListenerInner::Fifo { dir, .. } = &self.inner {
            let _ = std::fs::remove_file(dir.join("accept"));
        }
    }
}

fn bind_fifo(dir: &Path, addr: SocketAddr) -> io::Result<Listener> {
    loopback_only(addr)?;
    let ports: Vec<u16> = if addr.port() == 0 {
        // Free ports are picked from a range that the clock and pid spread.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
            ^ std::process::id().rotate_left(16);
        (0..200u32)
            .map(|i| 20_000 + (seed.wrapping_add(i.wrapping_mul(7919)) % 40_000) as u16)
            .collect()
    } else {
        vec![addr.port()]
    };
    let mut last = io::Error::new(io::ErrorKind::AddrInUse, "no free port");
    for port in ports {
        let local = SocketAddr::new(addr.ip(), port);
        let conn_dir = address_dir(dir, local);
        match claim(&conn_dir) {
            Ok(ids) => {
                return Ok(Listener {
                    inner: ListenerInner::Fifo {
                        ids: Mutex::new(BufReader::new(ids)),
                        dir: conn_dir,
                    },
                    local,
                });
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Claims an address directory: makes the accept FIFO, or takes over one
/// that a killed process left (nobody reads it). A live listener keeps it.
fn claim(conn_dir: &Path) -> io::Result<pipe::Receiver> {
    std::fs::create_dir_all(conn_dir)?;
    let accept = conn_dir.join("accept");
    if accept.exists() {
        match pipe::OpenOptions::new().open_sender(&accept) {
            Ok(_) => return Err(io::Error::from(io::ErrorKind::AddrInUse)),
            Err(e) if is_no_reader(&e) => {}
            Err(e) => return Err(e),
        }
    } else {
        make_fifo(&accept)?;
    }
    open_accept(&accept)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use tokio::io::AsyncReadExt;

    #[test]
    fn values_select_the_transport() {
        assert_eq!(Transport::parse(None), Ok(Transport::Tcp));
        assert_eq!(Transport::parse(Some("tcp")), Ok(Transport::Tcp));
        assert_eq!(
            Transport::parse(Some("fifo:/x/y")),
            Ok(Transport::Fifo(PathBuf::from("/x/y")))
        );
        assert!(Transport::parse(Some("fifo:")).is_err());
        assert!(Transport::parse(Some("quic")).is_err());
        let auto = Transport::parse(Some("auto")).unwrap();
        assert_eq!(auto == Transport::Tcp, tcp_permitted());
    }

    #[test]
    fn address_dirs_are_plain_names() {
        let d = Path::new("/d");
        assert_eq!(
            address_dir(d, "127.0.0.1:41000".parse().unwrap()),
            Path::new("/d/127.0.0.1_41000.d")
        );
        assert_eq!(
            address_dir(d, "[::1]:9".parse().unwrap()),
            Path::new("/d/___1__9.d")
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn fifo_pairs_connect_carry_bytes_both_ways_and_end_with_eof() {
        let dir = tempfile::tempdir().unwrap();
        let l = bind_fifo(dir.path(), "127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = l.local_addr().unwrap();
        assert_ne!(addr.port(), 0);
        // The same address while the listener lives is in use.
        assert!(bind_fifo(dir.path(), addr).is_err());
        let server = tokio::spawn(async move {
            let (mut s, from) = l.accept().await.unwrap();
            assert!(from.ip().is_loopback());
            let mut buf = vec![0u8; 5];
            s.read_exact(&mut buf).await.unwrap();
            s.write_all(&buf).await.unwrap();
            s.shutdown().await.unwrap();
            let mut rest = Vec::new();
            s.read_to_end(&mut rest).await.unwrap();
            (l, rest)
        });
        let mut c = fifo_connect(dir.path(), addr).await.unwrap();
        c.write_all(b"hello").await.unwrap();
        let mut back = Vec::new();
        c.read_to_end(&mut back).await.unwrap();
        assert_eq!(back, b"hello", "echo, then EOF after the peer's shutdown");
        c.write_all(b"bye").await.unwrap();
        drop(c);
        let (l, rest) = server.await.unwrap();
        assert_eq!(rest, b"bye");
        drop(l);
        // Without a listener a dial is refused, as with TCP.
        let e = fifo_connect(dir.path(), addr).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::ConnectionRefused);
        // A non-loopback address is refused.
        assert!(bind_fifo(dir.path(), "10.0.0.5:0".parse().unwrap()).is_err());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_accept_fifo_of_a_killed_listener_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let addr: SocketAddr = "127.0.0.1:20001".parse().unwrap();
        let conn_dir = address_dir(dir.path(), addr);
        std::fs::create_dir_all(&conn_dir).unwrap();
        make_fifo(&conn_dir.join("accept")).unwrap(); // left behind, nobody reads
        let e = fifo_connect(dir.path(), addr).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::ConnectionRefused);
        let l = bind_fifo(dir.path(), addr).unwrap();
        assert_eq!(l.local_addr().unwrap(), addr);
        drop(l);
        assert!(
            !conn_dir.join("accept").exists(),
            "the listener removes its FIFO on drop"
        );
    }
}
