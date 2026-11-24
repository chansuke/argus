import { describe, expect, it } from "vitest";
import { createClient, OPS, type Transport } from "../src/index";

type Seen = { kind: "call" | "subscribe"; op: string; input: unknown };
type OpMethod = (input: unknown, onValue?: (v: unknown) => void) => unknown;

function isOpMethod(v: unknown): v is OpMethod {
  return typeof v === "function";
}

function fakeTransport(seen: Seen[]): Transport {
  return {
    call(op, input) {
      seen.push({ kind: "call", op, input });
      return Promise.resolve({ op });
    },
    subscribe(op, input, onValue) {
      seen.push({ kind: "subscribe", op, input });
      onValue({ op });
      return () => {
        seen.push({ kind: "subscribe", op: `${op}:closed`, input });
      };
    },
    blobUrl: (job, blob) => `fake://${job}/${blob}`,
  };
}

describe("createClient", () => {
  it("routes every subscribe op to subscribe and every other op to call", async () => {
    const seen: Seen[] = [];
    const methods = new Map<string, unknown>(Object.entries(createClient(fakeTransport(seen))));
    for (const [op, meta] of Object.entries(OPS)) {
      const method = methods.get(op);
      if (!isOpMethod(method)) {
        throw new Error(`createClient has no method for the op ${op}`);
      }
      const input = { op };
      if (meta.kind === "subscribe") {
        const pushed: unknown[] = [];
        const close = method(input, (v) => pushed.push(v));
        expect(pushed, op).toEqual([{ op }]);
        if (!isOpMethod(close)) {
          throw new Error(`subscribing to ${op} returned no unsubscribe function`);
        }
        close(undefined);
        expect(seen.slice(-2), op).toEqual([
          { kind: "subscribe", op, input },
          { kind: "subscribe", op: `${op}:closed`, input },
        ]);
      } else {
        await expect(method(input), op).resolves.toEqual({ op });
        expect(seen.at(-1), op).toEqual({ kind: "call", op, input });
      }
    }
    expect(seen.filter((s) => !s.op.endsWith(":closed"))).toHaveLength(Object.keys(OPS).length);
  });

  it("passes blob URLs through to the transport", () => {
    const api = createClient(fakeTransport([]));
    expect(api.blobUrl("j-1", "b-2")).toBe("fake://j-1/b-2");
  });
});
