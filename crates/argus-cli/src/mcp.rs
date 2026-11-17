//! MCP over stdio for agents that prefer tools to shelling out. Tools are generated from
//! `argus_api::OPS` entries with `agent: true` (name, input schema from the op's input
//! type), so the MCP surface cannot drift from the CLI or the API.

use argus_api::{Client, Transport};

pub(crate) fn serve<T: Transport>(client: Client<T>) -> std::io::Result<()> {
    // TODO: JSON-RPC 2.0 loop on stdin/stdout: initialize, tools/list (from OPS where
    //       agent), tools/call -> client.transport.call(op, args).
    todo!("not implemented")
}
