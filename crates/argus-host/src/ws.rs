//! The parts of HTTP/1.1 and WebSocket (RFC 6455) the phone listener needs, std-only:
//! the request head, the handshake's accept key (SHA-1 + base64), and message framing.
//! Only what a server must do: client frames must be masked, server frames are not;
//! text messages (fragmented or not), ping, pong, and close. Binary messages close.

use std::io::{self, Read, Write};

/// A request line and its headers (names lowercased). The body is never read.
#[derive(Debug, PartialEq)]
pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// The largest request head accepted.
const MAX_HEAD: usize = 16 << 10;

/// Reads up to the blank line that ends the head, one byte at a time so nothing after it
/// (the first WebSocket frame) is consumed. `None` if it is malformed or too long.
pub(crate) fn read_request(r: &mut impl Read) -> Option<Request> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD || r.read(&mut byte).ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    let text = String::from_utf8(head).ok()?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, path, version) = (first.next()?, first.next()?, first.next()?);
    if !version.starts_with("HTTP/1.") || first.next().is_some() {
        return None;
    }
    let headers = lines
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (name, value) = l.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Request {
        method: method.into(),
        path: path.into(),
        headers,
    })
}

/// Whether `req` asks to open a WebSocket; its `Sec-WebSocket-Accept` if so.
pub(crate) fn upgrade_accept(req: &Request) -> Option<String> {
    let has = |name: &str, token: &str| {
        req.header(name)
            .is_some_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
    };
    if req.method != "GET" || !has("upgrade", "websocket") || !has("connection", "upgrade") {
        return None;
    }
    if req.header("sec-websocket-version") != Some("13") {
        return None;
    }
    let key = req.header("sec-websocket-key")?;
    Some(accept_key(key))
}

pub(crate) fn accept_key(key: &str) -> String {
    const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    base64(&sha1(format!("{key}{GUID}").as_bytes()))
}

pub(crate) fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..20 => ((b & c) | (!b & d), 0x5A827999),
                20..40 => (b ^ c ^ d, 0x6ED9EBA1),
                40..60 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, t);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

pub(crate) fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// What a client sent, as the session sees it.
#[derive(Debug, PartialEq)]
pub(crate) enum Message {
    Text(String),
    /// The client closed, or broke the protocol (unmasked, binary, too long, bad UTF-8):
    /// either way the connection ends.
    Close,
}

const CONTINUATION: u8 = 0x0;
const TEXT: u8 = 0x1;
const CLOSE: u8 = 0x8;
const PING: u8 = 0x9;
const PONG: u8 = 0xA;

/// Reads frames until one whole text message (or the end). Pings are answered on `w`.
pub(crate) fn read_message(r: &mut impl Read, w: &mut impl Write, max: usize) -> Message {
    let mut text: Option<Vec<u8>> = None;
    loop {
        let Some((fin, opcode, payload)) = read_frame(r, max) else {
            return Message::Close;
        };
        match opcode {
            PING => {
                if write_frame(w, PONG, &payload).is_err() {
                    return Message::Close;
                }
            }
            PONG => {}
            TEXT if text.is_none() => text = Some(payload),
            CONTINUATION if text.is_some() => {
                let buf = text.as_mut().unwrap();
                if buf.len() + payload.len() > max {
                    return Message::Close;
                }
                buf.extend_from_slice(&payload);
            }
            // Close, binary, reserved, or a fragment out of order.
            _ => return Message::Close,
        }
        if fin && opcode != PING && opcode != PONG {
            let bytes = text.take().unwrap_or_default();
            return String::from_utf8(bytes).map_or(Message::Close, Message::Text);
        }
    }
}

/// One masked client frame: (fin, opcode, unmasked payload).
fn read_frame(r: &mut impl Read, max: usize) -> Option<(bool, u8, Vec<u8>)> {
    let mut head = [0u8; 2];
    r.read_exact(&mut head).ok()?;
    let (fin, rsv, opcode) = (head[0] & 0x80 != 0, head[0] & 0x70, head[0] & 0x0F);
    let masked = head[1] & 0x80 != 0;
    if rsv != 0 || !masked {
        return None;
    }
    let len = match head[1] & 0x7F {
        126 => {
            let mut b = [0u8; 2];
            r.read_exact(&mut b).ok()?;
            u64::from(u16::from_be_bytes(b))
        }
        127 => {
            let mut b = [0u8; 8];
            r.read_exact(&mut b).ok()?;
            u64::from_be_bytes(b)
        }
        n => u64::from(n),
    };
    // Control frames are short and never fragmented.
    if len > max as u64 || (opcode >= CLOSE && (len > 125 || !fin)) {
        return None;
    }
    let mut mask = [0u8; 4];
    r.read_exact(&mut mask).ok()?;
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).ok()?;
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= mask[i % 4];
    }
    Some((fin, opcode, payload))
}

/// One unmasked, unfragmented server frame.
fn write_frame(w: &mut impl Write, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let mut frame = vec![0x80 | opcode];
    match payload.len() {
        n @ 0..126 => frame.push(n as u8),
        n @ 126..65536 => {
            frame.push(126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            frame.push(127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    w.write_all(&frame)?;
    w.flush()
}

pub(crate) fn write_text(w: &mut impl Write, text: &str) -> io::Result<()> {
    write_frame(w, TEXT, text.as_bytes())
}

pub(crate) fn write_close(w: &mut impl Write) -> io::Result<()> {
    write_frame(w, CLOSE, &[])
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn sha1_matches_the_published_vectors() {
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        // Two blocks of padding.
        assert_eq!(
            hex(&sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn base64_pads_the_last_group() {
        let cases = [
            ("", ""),
            ("a", "YQ=="),
            ("ab", "YWI="),
            ("abc", "YWJj"),
            ("abcd", "YWJjZA=="),
        ];
        for (raw, encoded) in cases {
            assert_eq!(base64(raw.as_bytes()), encoded, "{raw:?}");
        }
    }

    #[test]
    fn the_accept_key_is_the_one_in_rfc_6455() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    const HANDSHAKE: &str = "GET /ws HTTP/1.1\r\nHost: argus\r\nUpgrade: websocket\r\nConnection: keep-alive, Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";

    #[test]
    fn a_handshake_head_is_read_without_what_follows_it() {
        let mut bytes = HANDSHAKE.as_bytes().to_vec();
        bytes.extend_from_slice(b"\x81");
        let mut r = &bytes[..];
        let req = read_request(&mut r).unwrap();
        assert_eq!((req.method.as_str(), req.path.as_str()), ("GET", "/ws"));
        assert_eq!(
            upgrade_accept(&req).as_deref(),
            Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
        );
        assert_eq!(r, b"\x81");

        let plain = read_request(&mut &b"GET /ws HTTP/1.1\r\nHost: argus\r\n\r\n"[..]).unwrap();
        assert_eq!(upgrade_accept(&plain), None);
        assert_eq!(read_request(&mut &b"GET /ws HTTP/1.1\r\n"[..]), None);
        assert_eq!(read_request(&mut &b"nonsense\r\n\r\n"[..]), None);
    }

    /// A client frame: masked, as browsers and the phone send it.
    pub(crate) fn client_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [0x37, 0xfa, 0x21, 0x3d];
        let mut f = vec![if fin { 0x80 } else { 0 } | opcode];
        match payload.len() {
            n @ 0..126 => f.push(0x80 | n as u8),
            n @ 126..65536 => {
                f.push(0x80 | 126);
                f.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                f.push(0x80 | 127);
                f.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        f.extend_from_slice(&mask);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        f
    }

    /// Reads one server frame: (opcode, payload).
    pub(crate) fn server_frame(r: &mut impl Read) -> Option<(u8, Vec<u8>)> {
        let mut head = [0u8; 2];
        r.read_exact(&mut head).ok()?;
        assert_eq!(head[1] & 0x80, 0, "server frames are not masked");
        let len = match head[1] & 0x7F {
            126 => {
                let mut b = [0u8; 2];
                r.read_exact(&mut b).ok()?;
                usize::from(u16::from_be_bytes(b))
            }
            127 => {
                let mut b = [0u8; 8];
                r.read_exact(&mut b).ok()?;
                u64::from_be_bytes(b) as usize
            }
            n => usize::from(n),
        };
        let mut payload = vec![0u8; len];
        r.read_exact(&mut payload).ok()?;
        Some((head[0] & 0x0F, payload))
    }

    #[test]
    fn the_masked_frame_in_rfc_6455_reads_as_hello() {
        let frame = [
            0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58,
        ];
        let mut out = Vec::new();
        let got = read_message(&mut &frame[..], &mut out, 1024);
        assert_eq!(got, Message::Text("Hello".into()));
        assert!(out.is_empty());
    }

    #[test]
    fn fragments_join_and_pings_are_answered_in_between() {
        let mut bytes = client_frame(false, TEXT, b"abc");
        bytes.extend(client_frame(true, PING, b"p"));
        bytes.extend(client_frame(true, CONTINUATION, b"def"));
        let mut out = Vec::new();
        let got = read_message(&mut &bytes[..], &mut out, 1024);
        assert_eq!(got, Message::Text("abcdef".into()));
        assert_eq!(server_frame(&mut &out[..]), Some((PONG, b"p".to_vec())));
    }

    #[test]
    fn long_messages_use_the_extended_lengths() {
        for len in [126, 70_000] {
            let text = "x".repeat(len);
            let bytes = client_frame(true, TEXT, text.as_bytes());
            let got = read_message(&mut &bytes[..], &mut Vec::new(), 1 << 20);
            assert_eq!(got, Message::Text(text.clone()));
            let mut out = Vec::new();
            write_text(&mut out, &text).unwrap();
            assert_eq!(server_frame(&mut &out[..]), Some((TEXT, text.into_bytes())));
        }
    }

    #[test]
    fn anything_but_masked_text_within_the_limit_closes() {
        let unmasked = [0x81, 0x02, b'h', b'i'];
        let binary = client_frame(true, 0x2, b"hi");
        let close = client_frame(true, CLOSE, b"");
        let too_long = client_frame(true, TEXT, &[b'x'; 20]);
        let mut split_too_long = client_frame(false, TEXT, &[b'x'; 8]);
        split_too_long.extend(client_frame(true, CONTINUATION, &[b'x'; 8]));
        let stray = client_frame(true, CONTINUATION, b"hi");
        let not_utf8 = client_frame(true, TEXT, &[0xff]);
        let cases: [&[u8]; 8] = [
            &unmasked,
            &binary,
            &close,
            &too_long,
            &split_too_long,
            &stray,
            &not_utf8,
            &[],
        ];
        for (i, bytes) in cases.into_iter().enumerate() {
            let got = read_message(&mut &bytes[..], &mut Vec::new(), 10);
            assert_eq!(got, Message::Close, "case {i}");
        }
    }
}
