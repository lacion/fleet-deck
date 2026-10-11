//! A board API server over loopback HTTPS and a raw HTTP/1.1 client
//! (M2 P3-8, P3-9a).

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_deck::DeckStore;
use fleetdeck_deck::http::assets::BoardAssets;
use fleetdeck_deck::http::tls::pinned_client;
use fleetdeck_deck::http::{Board, BoardServer, BoardTls, LoginLimiter, Origins};
use fleetdeck_deck::link::DeckLink;
use fleetdeck_deck::store::AuditResult;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;

pub struct Http {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Value,
    /// The raw body (the board files are not JSON).
    #[allow(dead_code)]
    pub text: String,
}

impl Http {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn code(&self) -> &str {
        self.body["error"].as_str().unwrap_or("")
    }
}

/// A board WebSocket client.
pub struct Ws(pub tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>);

impl Ws {
    /// The next JSON message (10 s), or `Err(close code)` when the server
    /// closes.
    pub async fn next(&mut self) -> Result<Value, Option<u16>> {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        loop {
            let m = tokio::time::timeout(Duration::from_secs(10), self.0.next())
                .await
                .expect("a websocket message");
            match m {
                Some(Ok(Message::Text(t))) => return Ok(serde_json::from_str(t.as_str()).unwrap()),
                Some(Ok(Message::Close(f))) => return Err(f.map(|f| u16::from(f.code))),
                Some(Ok(_)) => {}
                Some(Err(_)) | None => return Err(None),
            }
        }
    }

    /// The next message of `kind` (skips others).
    pub async fn next_of(&mut self, kind: &str) -> Value {
        loop {
            let m = self.next().await.expect("an open socket");
            if m["type"] == kind {
                return m;
            }
        }
    }

    /// No message within `wait`.
    pub async fn quiet(&mut self, wait: Duration) {
        use futures_util::StreamExt;
        if let Ok(m) = tokio::time::timeout(wait, self.0.next()).await {
            panic!("unexpected message: {m:?}");
        }
    }

    pub async fn send(&mut self, v: Value) {
        use futures_util::SinkExt;
        self.0
            .send(tokio_tungstenite::tungstenite::Message::Text(
                v.to_string().into(),
            ))
            .await
            .unwrap();
    }
}

pub struct BoardClient {
    pub store: DeckStore,
    pub addr: std::net::SocketAddr,
    tls: TlsConnector,
    _server: BoardServer,
    _stop: watch::Sender<bool>,
}

/// Serves the board API of `link` on a loopback port.
pub async fn board(link: Arc<DeckLink>) -> BoardClient {
    serve(link, None, BoardAssets::None).await
}

/// Serves the board API of `node`, with the pairing methods.
#[allow(dead_code)]
pub async fn board_with_node(node: Arc<fleetdeck_deck::link::DeckNode>) -> BoardClient {
    serve(Arc::clone(node.link()), Some(node), BoardAssets::None).await
}

/// Serves the board API of `node` and the board files of `assets`.
#[allow(dead_code)]
pub async fn board_with_assets(
    node: Arc<fleetdeck_deck::link::DeckNode>,
    assets: BoardAssets,
) -> BoardClient {
    serve(Arc::clone(node.link()), Some(node), assets).await
}

async fn serve(
    link: Arc<DeckLink>,
    pairing: Option<Arc<fleetdeck_deck::link::DeckNode>>,
    assets: BoardAssets,
) -> BoardClient {
    let store = link.store.clone();
    let (stop, rx) = watch::channel(false);
    let s = store.clone();
    let server = BoardServer::bind(
        &["127.0.0.1:0".parse().unwrap()],
        move |bound| {
            let (cert, key) = BoardTls::generate(&["127.0.0.1".to_owned()])?;
            let tls = BoardTls::from_pem(&cert, &key, true)?;
            let board = Arc::new(Board {
                store: s,
                link,
                origins: Origins::new(bound, Some("deck.tail1234.ts.net")),
                limiter: LoginLimiter::default(),
                tickets: fleetdeck_deck::http::ws::Tickets::default(),
                ws_recheck: Duration::from_millis(200),
                pairing: pairing.clone(),
                assets: assets.clone(),
            });
            Ok::<_, fleetdeck_deck::http::tls::TlsError>((board, tls))
        },
        rx,
    )
    .await
    .unwrap();
    let tls = TlsConnector::from(pinned_client(server.tls.cert.clone()));
    BoardClient {
        store,
        tls,
        addr: server.addrs[0],
        _server: server,
        _stop: stop,
    }
}

impl BoardClient {
    pub async fn req(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> Http {
        let tcp = TcpStream::connect(self.addr).await.unwrap();
        let name = ServerName::try_from("127.0.0.1").unwrap();
        let mut s = self.tls.connect(name, tcp).await.unwrap();
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let mut text = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n",
            self.addr,
            body.len()
        );
        if !body.is_empty() {
            text.push_str("Content-Type: application/json\r\n");
        }
        for (k, v) in headers {
            text.push_str(&format!("{k}: {v}\r\n"));
        }
        text.push_str("\r\n");
        text.push_str(&body);
        s.write_all(text.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        // The server may end the TLS stream without close_notify after
        // `Connection: close`; the bytes read so far are the response.
        let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut raw))
            .await
            .expect("a response");
        let raw = String::from_utf8(raw).unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        let mut lines = head.lines();
        let status = lines
            .next()
            .unwrap()
            .split(' ')
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers = lines
            .filter_map(|l| l.split_once(": "))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        Http {
            status,
            headers,
            body: serde_json::from_str(body).unwrap_or(Value::Null),
            text: body.to_owned(),
        }
    }

    pub async fn get(&self, path: &str, headers: &[(&str, &str)]) -> Http {
        self.req("GET", path, headers, None).await
    }

    pub async fn post(&self, path: &str, headers: &[(&str, &str)], body: Value) -> Http {
        self.req("POST", path, headers, Some(body)).await
    }

    /// Logs in with a fresh login link; returns (cookie header, csrf).
    pub async fn login(&self) -> (String, String) {
        let token = self.store.create_login_link().await.unwrap();
        let r = self
            .post("/api/auth/link", &[], json!({ "token": token.expose() }))
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
        let set = r.header("set-cookie").unwrap();
        let pair = set.split(';').next().unwrap().to_owned();
        (pair, r.body["csrf"].as_str().unwrap().to_owned())
    }

    /// The board's own origin.
    pub fn origin(&self) -> String {
        format!("https://{}", self.addr)
    }

    /// Opens `GET path` as a WebSocket with `headers`. A refused upgrade
    /// returns the HTTP status.
    pub async fn ws(&self, path: &str, headers: &[(&str, &str)]) -> Result<Ws, u16> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::http::HeaderValue;
        let tcp = TcpStream::connect(self.addr).await.unwrap();
        let name = ServerName::try_from("127.0.0.1").unwrap();
        let tls = self.tls.connect(name, tcp).await.unwrap();
        let mut req = format!("wss://{}{path}", self.addr)
            .into_client_request()
            .unwrap();
        for (k, v) in headers {
            req.headers_mut().insert(
                tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        match tokio_tungstenite::client_async(req, tls).await {
            Ok((ws, _)) => Ok(Ws(ws)),
            Err(tokio_tungstenite::tungstenite::Error::Http(r)) => Err(r.status().as_u16()),
            Err(e) => panic!("websocket: {e}"),
        }
    }

    pub async fn audited(&self, action: &str, result: AuditResult) -> Vec<Option<String>> {
        self.store
            .audit_log(100)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.action == action && e.result == result)
            .map(|e| e.detail)
            .collect()
    }
}
