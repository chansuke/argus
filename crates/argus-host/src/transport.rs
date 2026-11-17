//! Stream transports. Each one: accept, establish `Caller` from the `Hello` frame, then
//! pump `ClientFrame`s into `Host::call` / `Host::subscribe` and `ServerFrame`s back.
//! No op-specific code here; that is the invariant that keeps the transports equal.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use crate::Host;

/// Unix socket, newline-delimited JSON frames. File mode 0600 (same user only).
/// `Hello { run, token }` with a valid per-run token (handed to the agent in its env)
/// -> `Caller::Agent`; without -> `Caller::Local`.
pub fn serve_socket(host: Host, path: &Path) -> std::io::Result<()> {
    todo!("not implemented")
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
