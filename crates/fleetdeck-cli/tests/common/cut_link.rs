//! A loopback TCP relay that cuts both link channels without killing a node.
//! It passes TLS bytes unchanged; the production fingerprint pin still applies.

use std::io;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

struct State {
    online: bool,
    stopped: bool,
    sockets: Vec<TcpStream>,
}

pub struct CutLink {
    pub addr: SocketAddr,
    state: Arc<Mutex<State>>,
    thread: Option<JoinHandle<()>>,
}

impl CutLink {
    pub fn start(target: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let state = Arc::new(Mutex::new(State {
            online: true,
            stopped: false,
            sockets: Vec::new(),
        }));
        let shared = Arc::clone(&state);
        let thread = thread::spawn(move || {
            let mut copies = Vec::new();
            loop {
                if shared.lock().unwrap().stopped {
                    break;
                }
                match listener.accept() {
                    Ok((mut client, _)) => {
                        let mut state = shared.lock().unwrap();
                        if !state.online {
                            continue; // Close attempts while the network is down.
                        }
                        let Ok(mut server) =
                            TcpStream::connect_timeout(&target, Duration::from_millis(200))
                        else {
                            continue;
                        };
                        let mut client_back = client.try_clone().unwrap();
                        let mut server_back = server.try_clone().unwrap();
                        state.sockets.push(client.try_clone().unwrap());
                        state.sockets.push(server.try_clone().unwrap());
                        copies.push(thread::spawn(move || {
                            let _ = io::copy(&mut client, &mut server);
                            let _ = server.shutdown(Shutdown::Both);
                            let _ = client.shutdown(Shutdown::Both);
                        }));
                        copies.push(thread::spawn(move || {
                            let _ = io::copy(&mut server_back, &mut client_back);
                            let _ = client_back.shutdown(Shutdown::Both);
                            let _ = server_back.shutdown(Shutdown::Both);
                        }));
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("relay accept: {e}"),
                }
            }
            for copy in copies {
                copy.join().unwrap();
            }
        });
        Self {
            addr,
            state,
            thread: Some(thread),
        }
    }

    pub fn set_online(&self, online: bool) {
        let mut state = self.state.lock().unwrap();
        state.online = online;
        if !online {
            for socket in state.sockets.drain(..) {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
    }

    /// Replace only the invitation address; keep the pinned key and proof secret.
    pub fn invitation(&self, original: &str) -> String {
        let (_, fragment) = original.trim().split_once('#').unwrap();
        format!("fdinv1:wss://{}#{fragment}", self.addr)
    }
}

impl Drop for CutLink {
    fn drop(&mut self) {
        self.set_online(false);
        self.state.lock().unwrap().stopped = true;
        self.thread.take().unwrap().join().unwrap();
    }
}
