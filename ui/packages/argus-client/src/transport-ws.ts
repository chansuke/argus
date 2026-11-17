import type { Transport } from "./index";

/**
 * Phone transport: one WebSocket to the desktop host carrying ClientFrame/ServerFrame.
 * TODO: Hello{token} on open; id-correlated Call/Reply; Subscribe/Push; on reconnect,
 * resubscribe every live subscription and resend unanswered calls (safe: commands carry
 * request_id, so the host drops duplicates).
 */
export function websocketTransport(baseUrl: string, pairingToken: string): Transport {
  throw new Error("not implemented");
}
