//! Tauri v2 shell for the React desktop UI. Embeds the host in-process and exposes it
//! through three generic commands. Nothing here knows any op: adding an op to
//! `argus_api::ops` reaches the desktop without touching this file.

use argus_api::Caller;
use argus_host::{Host, SubscriptionId};
use tauri::ipc::Channel;

/// One-shot op. `op` is any name in `OPS`; `input` is its input view model.
#[tauri::command]
fn argus_call(host: tauri::State<'_, Host>, op: String, input: serde_json::Value) -> Result<serde_json::Value, serde_json::Value> {
    let input = from_serde(input);
    host.call(&Caller::Local, &op, &input).map(to_serde).map_err(|e| to_serde(argus_api::wire::Wire::to_json(&e)))
}

/// Subscription: values arrive on `on_value` (a Tauri v2 channel: ordered, no global
/// event names), first immediately, then on every change.
#[tauri::command]
fn argus_subscribe(
    host: tauri::State<'_, Host>,
    op: String,
    input: serde_json::Value,
    on_value: Channel<serde_json::Value>,
) -> Result<u64, serde_json::Value> {
    let sink = Box::new(move |v| {
        let _ = on_value.send(to_serde(v));
    });
    host.subscribe(&Caller::Local, &op, from_serde(input), sink)
        .map(|SubscriptionId(id)| id)
        .map_err(|e| to_serde(argus_api::wire::Wire::to_json(&e)))
}

#[tauri::command]
fn argus_unsubscribe(host: tauri::State<'_, Host>, id: u64) {
    host.unsubscribe(SubscriptionId(id));
}

fn main() {
    // TODO: HostConfig from the app's settings (data root, tools from PATH, policy,
    //       phone access on/off + pairing token); if the store is locked by a running
    //       `argusd`, run in client mode instead (open question in RATIONALE).
    let host = Host::start(todo!("config")).expect("start host");
    let socket_host = host.clone();
    tauri::Builder::default()
        .manage(host.clone())
        // `argus-blob://<job>/<blob>` -> evidence files (screenshots, recordings) for <img>/<video>.
        .register_uri_scheme_protocol("argus-blob", move |_ctx, request| {
            todo!("host.blob_path(job, blob) -> 200 with file bytes and media type, else 404")
        })
        .invoke_handler(tauri::generate_handler![argus_call, argus_subscribe, argus_unsubscribe])
        .setup(move |_app| {
            // The CLI/agent socket and (if enabled) the phone listener run beside the window.
            std::thread::spawn(move || argus_host::serve_socket(socket_host, todo!("socket path")));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("run tauri");
}

/// serde_json <-> argus_json at the edge; nothing past this line sees serde.
fn from_serde(v: serde_json::Value) -> argus_json::Json {
    todo!("not implemented")
}
fn to_serde(v: argus_json::Json) -> serde_json::Value {
    todo!("not implemented")
}
