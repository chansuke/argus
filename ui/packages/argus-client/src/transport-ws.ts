/**
 * Phone transport: one WebSocket to the desktop host carrying ClientFrame/ServerFrame.
 * It is not written yet, so this entry point exports nothing and an import of
 * `websocketTransport(baseUrl, pairingToken)` fails to compile instead of throwing at
 * run time. The design: Hello{token} on open; id-correlated Call/Reply;
 * Subscribe/Push; on reconnect, resubscribe every live subscription and resend
 * unanswered calls (safe: commands carry request_id, so the host drops duplicates).
 */
export {};
