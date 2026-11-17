// Shared by the Tauri desktop app and the Expo phone app. Transports live in separate
// entry points (`@argus/client/tauri`, `@argus/client/ws`) so the phone bundle never
// imports @tauri-apps/api.
export * from "./gen/api";
import { OPS, type ArgusApi } from "./gen/api";

/** What a transport must do. Two implementations, no op-specific code in either. */
export interface Transport {
  call(op: string, input: unknown): Promise<unknown>;
  subscribe(op: string, input: unknown, onValue: (value: unknown) => void): () => void;
  /** URL for an evidence blob: `argus-blob://` on desktop, `https://host/blob/...` on the phone. */
  blobUrl(job: string, blob: string): string;
}

/** Builds the typed API from the generated OPS table; nothing is written per op. */
export function createClient(t: Transport): ArgusApi & { blobUrl: Transport["blobUrl"] } {
  const api: Record<string, unknown> = { blobUrl: t.blobUrl.bind(t) };
  for (const [op, meta] of Object.entries(OPS)) {
    api[op] =
      meta.kind === "subscribe"
        ? (input: unknown, onValue: (v: unknown) => void) => t.subscribe(op, input, onValue)
        : (input: unknown) => t.call(op, input);
  }
  return api as unknown as ArgusApi & { blobUrl: Transport["blobUrl"] };
}

/** Client-minted id for commands: a retried command is a no-op on the host. */
export function requestId(): string {
  return crypto.randomUUID();
}
