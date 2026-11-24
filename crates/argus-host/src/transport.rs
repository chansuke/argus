//! Stream transports. Each one: accept, establish `Caller` from the `Hello` frame, then
//! pump `ClientFrame`s into `Host::call` / `Host::subscribe` and `ServerFrame`s back.
//! No op-specific code here; that is the invariant that keeps the transports equal.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use argus_api::wire::Wire;
use argus_api::{Caller, ClientFrame, ServerFrame};
use argus_core::ids::{Id128, RunId};

use crate::{Host, SubscriptionId};

/// The largest frame a client may send (one line, or one WebSocket message).
pub(crate) const MAX_FRAME: usize = 16 << 20;

/// A listener running on its own thread. `stop` (or dropping it) closes the listener and
/// every connection it accepted, and waits for their threads.
pub struct Serving {
    stop: Arc<AtomicBool>,
    wake: Box<dyn Fn() + Send>,
    thread: Option<JoinHandle<()>>,
    cleanup: Option<PathBuf>,
}

impl Serving {
    pub fn stop(self) {}
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        (self.wake)();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(path) = self.cleanup.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Unix socket, newline-delimited JSON frames. File mode 0600 (same user only): the
/// socket is bound under a temporary name, restricted, then renamed into place, so it is
/// never reachable with wider permissions. A leftover socket nobody answers on is
/// replaced; one that answers means another host is serving, and is an error.
///
/// `Hello { run, token }` with the token the runner handed that run (`ARGUS_TOKEN`)
/// -> `Caller::Agent`; `Hello` with neither -> `Caller::Local`; anything else closes.
pub fn serve_socket(host: Host, path: &Path) -> std::io::Result<Serving> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("{} is being served", path.display()),
            ));
        }
        std::fs::remove_file(path)?;
    }
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    let listener = UnixListener::bind(&temp)?;
    let placed = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
        .and_then(|()| std::fs::rename(&temp, path));
    if let Err(e) = placed {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }

    let stop = Arc::new(AtomicBool::new(false));
    let conns: Arc<Mutex<BTreeMap<u64, UnixStream>>> = Arc::default();
    let thread = {
        let (stop, conns) = (stop.clone(), conns.clone());
        std::thread::spawn(move || {
            let mut threads: Vec<JoinHandle<()>> = Vec::new();
            for (n, stream) in (0u64..).zip(listener.incoming()) {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let Ok(handle) = stream.try_clone() else {
                    continue;
                };
                conns.lock().unwrap().insert(n, handle);
                let (host, conns) = (host.clone(), conns.clone());
                threads.retain(|t| !t.is_finished());
                threads.push(std::thread::spawn(move || {
                    socket_connection(host, stream);
                    conns.lock().unwrap().remove(&n);
                }));
            }
            for stream in conns.lock().unwrap().values() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
            for thread in threads {
                let _ = thread.join();
            }
        })
    };
    let wake_path = path.to_path_buf();
    Ok(Serving {
        stop,
        wake: Box::new(move || {
            let _ = UnixStream::connect(&wake_path);
        }),
        thread: Some(thread),
        cleanup: Some(path.to_path_buf()),
    })
}

fn socket_connection(host: Host, stream: UnixStream) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let mut lines = BufReader::new(stream);
    let Some(hello) = read_line(&mut lines).and_then(|l| frame(&l)) else {
        return;
    };
    let Some(caller) = socket_caller(&host, hello) else {
        return;
    };
    let (out, writing) = writer_thread(move |text: String| {
        let mut w = &writer;
        w.write_all(text.as_bytes())?;
        w.write_all(b"\n")?;
        w.flush()
    });
    let mut session = Session::new(host, caller, out);
    while let Some(line) = read_line(&mut lines) {
        if !frame(&line).is_some_and(|f| session.handle(f)) {
            break;
        }
    }
    drop(session);
    let _ = writing.join();
}

/// The caller a Unix-socket `Hello` establishes, if it may connect at all.
fn socket_caller(host: &Host, hello: ClientFrame) -> Option<Caller> {
    match hello {
        ClientFrame::Hello {
            run: None,
            token: None,
        } => Some(Caller::Local),
        ClientFrame::Hello {
            run: Some(run),
            token: Some(token),
        } => {
            let id = RunId(Id128::parse(&run)?);
            host.inner
                .tokens
                .check(id, &token)
                .then_some(Caller::Agent { run })
        }
        _ => None,
    }
}

/// One line without its newline; `None` at the end, on an error, or past `MAX_FRAME`.
fn read_line(r: &mut impl BufRead) -> Option<String> {
    let mut line = Vec::new();
    let n = r
        .by_ref()
        .take(MAX_FRAME as u64 + 1)
        .read_until(b'\n', &mut line)
        .ok()?;
    if n == 0 || line.last() != Some(&b'\n') {
        return None;
    }
    line.pop();
    String::from_utf8(line).ok()
}

fn frame(text: &str) -> Option<ClientFrame> {
    ClientFrame::from_json(&argus_json::parse(text.as_bytes()).ok()?).ok()
}

/// Sends what the session queues, in order, until every sender is gone or a write
/// fails. Pushes come from the pulse thread; a slow client must never hold it up.
fn writer_thread(
    mut write: impl FnMut(String) -> std::io::Result<()> + Send + 'static,
) -> (Sender<String>, JoinHandle<()>) {
    let (out, queued) = mpsc::channel::<String>();
    let thread = std::thread::spawn(move || {
        for text in queued {
            if write(text).is_err() {
                return;
            }
        }
    });
    (out, thread)
}

/// One client after its `Hello`, the same for every stream transport. Dropping it ends
/// the client's subscriptions.
pub(crate) struct Session {
    host: Host,
    caller: Caller,
    out: Sender<String>,
    subs: BTreeMap<u32, SubscriptionId>,
}

impl Session {
    pub(crate) fn new(host: Host, caller: Caller, out: Sender<String>) -> Self {
        Session {
            host,
            caller,
            out,
            subs: BTreeMap::new(),
        }
    }

    fn send(&self, frame: ServerFrame) {
        let _ = self.out.send(argus_json::write(&frame.to_json()));
    }

    /// Handles one frame; `false` when the connection should close (a second `Hello`).
    pub(crate) fn handle(&mut self, frame: ClientFrame) -> bool {
        match frame {
            ClientFrame::Hello { .. } => return false,
            ClientFrame::Call { id, op, input } => {
                let reply = match self.host.call(&self.caller, &op, &input) {
                    Ok(value) => ServerFrame::Reply {
                        id,
                        ok: Some(value),
                        err: None,
                    },
                    Err(e) => ServerFrame::Reply {
                        id,
                        ok: None,
                        err: Some(e),
                    },
                };
                self.send(reply);
            }
            // Success is the first `Push`; only a failure is replied to.
            ClientFrame::Subscribe { id, op, input } => {
                if self.subs.contains_key(&id) {
                    self.send(ServerFrame::Reply {
                        id,
                        ok: None,
                        err: Some(argus_api::ApiError::BadInput {
                            path: "id".into(),
                            what: format!("subscription {id} is already open"),
                        }),
                    });
                    return true;
                }
                let out = self.out.clone();
                let sink = Box::new(move |value| {
                    let push = ServerFrame::Push { id, value };
                    let _ = out.send(argus_json::write(&push.to_json()));
                });
                match self.host.subscribe(&self.caller, &op, input, sink) {
                    Ok(sub) => {
                        self.subs.insert(id, sub);
                    }
                    Err(e) => self.send(ServerFrame::Reply {
                        id,
                        ok: None,
                        err: Some(e),
                    }),
                }
            }
            ClientFrame::Unsubscribe { id } => {
                if let Some(sub) = self.subs.remove(&id) {
                    self.host.unsubscribe(sub);
                }
            }
        }
        true
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        for sub in std::mem::take(&mut self.subs).into_values() {
            self.host.unsubscribe(sub);
        }
    }
}

/// Where the phone listener may bind: loopback (behind `tailscale serve` or an SSH
/// tunnel) or a tailnet address (100.64.0.0/10, fd7a:115c:a1e0::/48). `0.0.0.0` and LAN
/// addresses are refused when the config is loaded, so argus is never on an open network.
#[derive(Clone, Copy, Debug)]
pub struct PhoneListen(SocketAddr);

impl PhoneListen {
    pub fn new(addr: SocketAddr) -> Option<Self> {
        let ok = match addr.ip() {
            IpAddr::V4(v4) => {
                v4.is_loopback() || (v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 0x40)
            }
            IpAddr::V6(v6) => v6.is_loopback() || v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
        };
        ok.then_some(PhoneListen(addr))
    }
    pub fn addr(self) -> SocketAddr {
        self.0
    }
}

/// One phone, paired by scanning a QR code on the desktop. A paired phone is the human:
/// same ops as the desktop, including decide and merge (user decision, 2026-11-17).
/// Unpairing deletes the entry, which revokes exactly this token.
#[derive(Clone, Debug)]
pub struct PairedDevice {
    pub device: String,
    /// Random 256-bit token; compared in constant time. Rotation: open question.
    pub token: String,
}

#[derive(Clone, Debug)]
pub struct PhoneAccess {
    pub listen: PhoneListen,
    pub devices: Vec<PairedDevice>,
}

/// HTTP/1.1 + WebSocket for the phone (std-only: handshake needs SHA-1 + base64, both
/// small). `GET /ws`: frames as WebSocket text messages; `Hello { token }` must match a
/// paired device (-> `Caller::Phone { device }`), else close. `GET /blob/<job>/<id>`
/// with the token in a header: evidence files. Plain TCP on loopback or the tailnet
/// (WireGuard-encrypted); TLS beyond that is an open question.
pub fn serve_websocket(host: Host, access: &PhoneAccess) -> std::io::Result<()> {
    todo!("not implemented")
}

#[cfg(test)]
mod tests {
    use super::*;
    use argus_json::Json;
    use std::sync::atomic::AtomicU32;
    use std::time::Duration;

    fn scratch() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "argus-sock-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn serving() -> (Host, Serving, PathBuf) {
        let root = scratch();
        let host = Host::start(crate::tests::config(root.join("data"))).unwrap();
        let path = root.join("argus.sock");
        let serving = serve_socket(host.clone(), &path).unwrap();
        (host, serving, path)
    }

    struct Client {
        lines: BufReader<UnixStream>,
    }

    impl Client {
        fn connect(path: &Path, hello: &str) -> Client {
            let stream = UnixStream::connect(path).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut c = Client {
                lines: BufReader::new(stream),
            };
            c.send(hello);
            c
        }
        fn send(&mut self, line: &str) {
            let mut w = self.lines.get_ref();
            w.write_all(line.as_bytes()).unwrap();
            w.write_all(b"\n").unwrap();
        }
        /// The next frame, or `None` once the host closed the connection.
        fn next(&mut self) -> Option<Json> {
            let mut line = String::new();
            match self.lines.read_line(&mut line) {
                // A connection the listener never got to is reset rather than closed.
                Ok(0) => None,
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => None,
                Ok(_) => Some(argus_json::parse(line.as_bytes()).unwrap()),
                Err(e) => panic!("no frame: {e}"),
            }
        }
    }

    const LOCAL: &str = r#"{"kind":"Hello","token":null,"run":null}"#;

    fn kind(frame: &Json) -> &str {
        frame.get("kind").and_then(Json::as_str).unwrap()
    }

    #[test]
    fn a_local_client_calls_and_subscribes_over_the_socket() {
        let (host, serving, path) = serving();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        let mut c = Client::connect(&path, LOCAL);
        c.send(r#"{"kind":"Call","id":1,"op":"presets","input":{}}"#);
        let reply = c.next().unwrap();
        assert_eq!(kind(&reply), "Reply", "{}", argus_json::write(&reply));
        assert!(reply.get("ok").is_some_and(|ok| *ok != Json::Null));

        c.send(r#"{"kind":"Call","id":2,"op":"no_such_op","input":{}}"#);
        let reply = c.next().unwrap();
        let err = reply.get("err").unwrap();
        assert_eq!(kind(err), "UnknownOp", "{}", argus_json::write(&reply));

        c.send(r#"{"kind":"Subscribe","id":3,"op":"overview","input":{}}"#);
        let push = c.next().unwrap();
        assert_eq!(kind(&push), "Push", "{}", argus_json::write(&push));
        assert_eq!(
            push.get("id"),
            Some(&Json::Num(argus_json::Number::from_i64(3)))
        );

        // A second Hello ends the connection.
        c.send(LOCAL);
        assert!(c.next().is_none());
        serving.stop();
        host.shutdown();
        assert!(!path.exists());
    }

    #[test]
    fn only_a_runs_live_token_makes_the_client_that_agent() {
        let (host, serving, path) = serving();
        let run = RunId(Id128(7));
        let issued = host.inner.tokens.issue(run).unwrap();

        let hello = format!(
            r#"{{"kind":"Hello","token":"{}","run":"{}"}}"#,
            issued.token, run.0
        );
        let mut agent = Client::connect(&path, &hello);
        // `presets` is not an agent op, so the caller is the agent, not the user.
        agent.send(r#"{"kind":"Call","id":1,"op":"presets","input":{}}"#);
        let reply = agent.next().unwrap();
        let err = reply.get("err").unwrap();
        assert_eq!(kind(err), "Forbidden", "{}", argus_json::write(&reply));

        let wrong = format!(
            r#"{{"kind":"Hello","token":"{}","run":"{}"}}"#,
            "0".repeat(64),
            run.0
        );
        assert!(Client::connect(&path, &wrong).next().is_none());
        let other = format!(
            r#"{{"kind":"Hello","token":"{}","run":"{}"}}"#,
            issued.token,
            Id128(8)
        );
        assert!(Client::connect(&path, &other).next().is_none());
        let half = r#"{"kind":"Hello","token":null,"run":"7"}"#;
        assert!(Client::connect(&path, half).next().is_none());

        drop(issued);
        assert!(Client::connect(&path, &hello).next().is_none());
        drop(agent);
        serving.stop();
        host.shutdown();
    }

    #[test]
    fn a_served_socket_is_not_taken_over_but_a_dead_one_is_replaced() {
        let (host, serving, path) = serving();
        let again = serve_socket(host.clone(), &path).err().unwrap();
        assert_eq!(again.kind(), std::io::ErrorKind::AddrInUse);

        // A client still connected when the listener stops is disconnected.
        let mut c = Client::connect(&path, LOCAL);
        serving.stop();
        assert!(c.next().is_none());

        // What a crashed host leaves behind: a socket file nobody listens on.
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let serving = serve_socket(host.clone(), &path).unwrap();
        let mut c = Client::connect(&path, LOCAL);
        c.send(r#"{"kind":"Call","id":1,"op":"presets","input":{}}"#);
        assert_eq!(kind(&c.next().unwrap()), "Reply");
        drop(c);
        serving.stop();
        host.shutdown();
    }
}
