//! Stream transports. Each one: accept, establish `Caller` from the `Hello` frame, then
//! pump `ClientFrame`s into `Host::call` / `Host::subscribe` and `ServerFrame`s back.
//! No op-specific code here; that is the invariant that keeps the transports equal.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
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
    addr: Option<SocketAddr>,
}

impl Serving {
    pub fn stop(self) {}

    /// The address a network listener is bound to (its port, when asked for port 0).
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.addr
    }
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

    let wake_path = path.to_path_buf();
    Ok(accept_loop(
        move || listener.accept().map(|(s, _)| s),
        move || {
            let _ = UnixStream::connect(&wake_path);
        },
        move |stream| socket_connection(host.clone(), stream),
        Some(path.to_path_buf()),
        None,
    ))
}

/// A connected stream the accept loop can close from another thread.
trait Conn: Send + Sized + 'static {
    fn clone_handle(&self) -> std::io::Result<Self>;
    fn close(&self);
}

impl Conn for UnixStream {
    fn clone_handle(&self) -> std::io::Result<Self> {
        self.try_clone()
    }
    fn close(&self) {
        let _ = self.shutdown(std::net::Shutdown::Both);
    }
}

impl Conn for TcpStream {
    fn clone_handle(&self) -> std::io::Result<Self> {
        self.try_clone()
    }
    fn close(&self) {
        let _ = self.shutdown(std::net::Shutdown::Both);
    }
}

/// Accepts on its own thread, one thread per connection, until stopped: then closes
/// every open connection and waits for their threads. `wake` unblocks the `accept`.
fn accept_loop<C: Conn>(
    mut accept: impl FnMut() -> std::io::Result<C> + Send + 'static,
    wake: impl Fn() + Send + 'static,
    serve: impl Fn(C) + Send + Sync + 'static,
    cleanup: Option<PathBuf>,
    addr: Option<SocketAddr>,
) -> Serving {
    let stop = Arc::new(AtomicBool::new(false));
    let serve = Arc::new(serve);
    let thread = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let conns: Arc<Mutex<BTreeMap<u64, C>>> = Arc::default();
            let mut threads: Vec<JoinHandle<()>> = Vec::new();
            for n in 0u64.. {
                let stream = accept();
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else {
                    // Out of descriptors, say: back off instead of spinning.
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                };
                let Ok(handle) = stream.clone_handle() else {
                    continue;
                };
                conns.lock().unwrap().insert(n, handle);
                let (serve, conns) = (serve.clone(), conns.clone());
                threads.retain(|t| !t.is_finished());
                threads.push(std::thread::spawn(move || {
                    serve(stream);
                    conns.lock().unwrap().remove(&n);
                }));
            }
            for stream in conns.lock().unwrap().values() {
                stream.close();
            }
            for thread in threads {
                let _ = thread.join();
            }
        })
    };
    Serving {
        stop,
        wake: Box::new(wake),
        thread: Some(thread),
        cleanup,
        addr,
    }
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
pub fn serve_websocket(host: Host, access: &PhoneAccess) -> std::io::Result<Serving> {
    let listener = TcpListener::bind(access.listen.addr())?;
    let addr = listener.local_addr()?;
    let devices = access.devices.clone();
    Ok(accept_loop(
        move || listener.accept().map(|(s, _)| s),
        move || {
            let _ = TcpStream::connect(addr);
        },
        move |stream| phone_connection(&host, &devices, stream),
        None,
        Some(addr),
    ))
}

/// The paired device `token` belongs to. Every device is compared, in constant time.
fn paired(devices: &[PairedDevice], token: &str) -> Option<String> {
    let mut found = None;
    for d in devices {
        if crate::tokens::same(d.token.as_bytes(), token.as_bytes()) {
            found = Some(d.device.clone());
        }
    }
    found
}

/// How long a phone may take to send its request head or `Hello`.
const HELLO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn phone_connection(host: &Host, devices: &[PairedDevice], stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(HELLO_TIMEOUT));
    let mut reader = &stream;
    let Some(req) = crate::ws::read_request(&mut reader) else {
        return;
    };
    let token = req
        .header("authorization")
        .and_then(|v| v.strip_prefix("Bearer "));
    if let Some(rest) = req.path.strip_prefix("/blob/") {
        let device = token.and_then(|t| paired(devices, t));
        serve_blob(host, rest, device.is_some(), &req, &stream);
        return;
    }
    let accept = (req.path == "/ws").then(|| crate::ws::upgrade_accept(&req));
    let Some(Some(accept)) = accept else {
        let _ = respond(&stream, "404 Not Found", &[]);
        return;
    };
    let switched = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    if (&stream).write_all(switched.as_bytes()).is_err() {
        return;
    }
    let shared = Arc::new(Mutex::new(stream));
    let mut frames = Frames(shared.clone(), Vec::new());
    let Ok(reader) = shared.lock().unwrap().try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader);
    let next =
        |reader: &mut BufReader<TcpStream>, frames: &mut Frames| match crate::ws::read_message(
            reader, frames, MAX_FRAME,
        ) {
            crate::ws::Message::Text(text) => frame(&text),
            crate::ws::Message::Close => None,
        };
    let caller = match next(&mut reader, &mut frames) {
        Some(ClientFrame::Hello {
            token: Some(token),
            run: None,
        }) => paired(devices, &token).map(|device| Caller::Phone { device }),
        _ => None,
    };
    let Some(caller) = caller else {
        let _ = crate::ws::write_close(&mut frames);
        return;
    };
    let _ = reader.get_ref().set_read_timeout(None);
    let mut writer = Frames(shared.clone(), Vec::new());
    let (out, writing) =
        writer_thread(move |text: String| crate::ws::write_text(&mut writer, &text));
    let mut session = Session::new(host.clone(), caller, out);
    while let Some(f) = next(&mut reader, &mut frames) {
        if !session.handle(f) {
            break;
        }
    }
    drop(session);
    let _ = writing.join();
    let _ = crate::ws::write_close(&mut frames);
}

/// Writes to a stream shared by the reader (pongs) and the writer thread (messages):
/// a frame is buffered, then written whole under the lock at `flush`, so two frames
/// never interleave.
struct Frames(Arc<Mutex<TcpStream>>, Vec<u8>);

impl Write for Frames {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.1.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let bytes = std::mem::take(&mut self.1);
        let stream = self.0.lock().unwrap();
        (&*stream).write_all(&bytes)?;
        (&*stream).flush()
    }
}

/// `GET /blob/<job>/<id>`: an evidence file, to a paired device only.
fn serve_blob(
    host: &Host,
    rest: &str,
    authorized: bool,
    req: &crate::ws::Request,
    stream: &TcpStream,
) {
    if !authorized {
        let _ = respond(stream, "401 Unauthorized", &[]);
        return;
    }
    let file = (req.method == "GET")
        .then(|| rest.split_once('/'))
        .flatten()
        .and_then(|(job, blob)| host.blob_path(job, blob))
        .and_then(|path| std::fs::File::open(path).ok());
    let Some(mut file) = file else {
        let _ = respond(stream, "404 Not Found", &[]);
        return;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {len}\r\nCache-Control: private, max-age=31536000, immutable\r\nConnection: close\r\n\r\n"
    );
    let mut w = stream;
    if w.write_all(head.as_bytes()).is_ok() {
        let _ = std::io::copy(&mut file, &mut w);
    }
}

fn respond(mut stream: &TcpStream, status: &str, body: &[u8]) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)
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

    // ------------------------------------------------------------ phone

    use crate::ws::tests::{client_frame, server_frame};

    const PHONE_TOKEN: &str = "c0ffee";

    fn phone() -> (Host, Serving, SocketAddr) {
        let root = scratch();
        let host = Host::start(crate::tests::config(root.join("data"))).unwrap();
        let access = PhoneAccess {
            listen: PhoneListen::new("127.0.0.1:0".parse().unwrap()).unwrap(),
            devices: vec![PairedDevice {
                device: "pixel".into(),
                token: PHONE_TOKEN.into(),
            }],
        };
        let serving = serve_websocket(host.clone(), &access).unwrap();
        let addr = serving.local_addr().unwrap();
        (host, serving, addr)
    }

    /// Sends `request` and returns the response head and what followed it so far.
    fn http(addr: SocketAddr, request: &str) -> (TcpStream, String) {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            assert_eq!(
                stream.read(&mut byte).unwrap(),
                1,
                "{:?}",
                String::from_utf8_lossy(&head)
            );
            head.push(byte[0]);
        }
        (stream, String::from_utf8(head).unwrap())
    }

    fn open_ws(addr: SocketAddr) -> TcpStream {
        let (stream, head) = http(
            addr,
            "GET /ws HTTP/1.1\r\nHost: argus\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
        );
        assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
        assert!(head.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"));
        stream
    }

    fn send_text(stream: &mut TcpStream, text: &str) {
        stream
            .write_all(&client_frame(true, 0x1, text.as_bytes()))
            .unwrap();
    }

    /// The next text message as JSON; `None` for a close frame or the end.
    fn next_text(stream: &mut TcpStream) -> Option<Json> {
        loop {
            match server_frame(stream)? {
                (0x1, payload) => return Some(argus_json::parse(&payload).unwrap()),
                (0x8, _) => return None,
                (0xA, _) => continue,
                (op, _) => panic!("unexpected opcode {op}"),
            }
        }
    }

    #[test]
    fn a_paired_phone_is_the_human_over_a_websocket() {
        let (host, serving, addr) = phone();
        let mut ws = open_ws(addr);
        send_text(
            &mut ws,
            &format!(r#"{{"kind":"Hello","token":"{PHONE_TOKEN}","run":null}}"#),
        );
        send_text(
            &mut ws,
            r#"{"kind":"Call","id":1,"op":"presets","input":{}}"#,
        );
        let reply = next_text(&mut ws).unwrap();
        assert!(
            reply.get("ok").is_some_and(|ok| *ok != Json::Null),
            "{}",
            argus_json::write(&reply)
        );

        ws.write_all(&client_frame(true, 0x9, b"hi")).unwrap();
        assert_eq!(server_frame(&mut ws), Some((0xA, b"hi".to_vec())));

        send_text(
            &mut ws,
            r#"{"kind":"Subscribe","id":2,"op":"overview","input":{}}"#,
        );
        let push = next_text(&mut ws).unwrap();
        assert_eq!(kind(&push), "Push", "{}", argus_json::write(&push));

        ws.write_all(&client_frame(true, 0x8, b"")).unwrap();
        assert_eq!(next_text(&mut ws), None);
        serving.stop();
        host.shutdown();
    }

    #[test]
    fn a_phone_without_a_paired_token_is_closed() {
        let (host, serving, addr) = phone();
        for hello in [
            r#"{"kind":"Hello","token":"c0ffef","run":null}"#.to_owned(),
            r#"{"kind":"Hello","token":null,"run":null}"#.to_owned(),
            format!(r#"{{"kind":"Hello","token":"{PHONE_TOKEN}","run":"1"}}"#),
            r#"{"kind":"Call","id":1,"op":"presets","input":{}}"#.to_owned(),
        ] {
            let mut ws = open_ws(addr);
            send_text(&mut ws, &hello);
            assert_eq!(next_text(&mut ws), None, "{hello}");
        }
        let (_, head) = http(addr, "GET / HTTP/1.1\r\nHost: argus\r\n\r\n");
        assert!(head.starts_with("HTTP/1.1 404 "), "{head}");
        serving.stop();
        host.shutdown();
    }

    #[test]
    fn evidence_files_are_served_to_a_paired_phone_only() {
        let (host, serving, addr) = phone();
        let job = Id128(5).to_string();
        let blob = "ab".repeat(32);
        let path = host.blob_path(&job, &blob).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"png bytes").unwrap();

        let get = |auth: &str, at: &str| {
            let (mut stream, head) = http(
                addr,
                &format!("GET {at} HTTP/1.1\r\nHost: argus\r\n{auth}\r\n"),
            );
            let mut body = Vec::new();
            stream.read_to_end(&mut body).unwrap();
            (head, body)
        };
        let at = format!("/blob/{job}/{blob}");
        let bearer = format!("Authorization: Bearer {PHONE_TOKEN}\r\n");
        let (head, body) = get(&bearer, &at);
        assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
        assert!(head.contains("Content-Length: 9\r\n"), "{head}");
        assert_eq!(body, b"png bytes");

        let (head, _) = get("", &at);
        assert!(head.starts_with("HTTP/1.1 401 "), "{head}");
        let (head, _) = get("Authorization: Bearer nope\r\n", &at);
        assert!(head.starts_with("HTTP/1.1 401 "), "{head}");
        let (head, _) = get(&bearer, &format!("/blob/{job}/{}", "cd".repeat(32)));
        assert!(head.starts_with("HTTP/1.1 404 "), "{head}");
        let (head, _) = get(&bearer, &format!("/blob/{job}/../../x"));
        assert!(head.starts_with("HTTP/1.1 404 "), "{head}");
        serving.stop();
        host.shutdown();
    }
}
