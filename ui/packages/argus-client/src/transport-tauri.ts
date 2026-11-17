import { Channel, invoke } from "@tauri-apps/api/core";
import type { Transport } from "./index";

export function tauriTransport(): Transport {
  return {
    call: (op, input) => invoke("argus_call", { op, input }),
    subscribe(op, input, onValue) {
      const ch = new Channel<unknown>();
      ch.onmessage = onValue;
      const id = invoke<number>("argus_subscribe", { op, input, onValue: ch });
      return () => void id.then((id) => invoke("argus_unsubscribe", { id }));
    },
    blobUrl: (job, blob) => `argus-blob://${job}/${blob}`,
  };
}
