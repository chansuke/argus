//! Headless host (no window): for remote boxes, or to keep agents running with the app
//! closed. Mutually exclusive with the app's embedded host by the store's locks.

fn main() {
    // TODO: parse flags (data root, socket, --phone <addr>), HostConfig, Host::start,
    //       spawn serve_socket (+ serve_websocket), park until SIGTERM, host.shutdown().
    todo!("not implemented")
}
