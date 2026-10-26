import assert from "node:assert/strict";
import { test } from "node:test";
import { createOpenRequestPump } from "../frontend/js/openrequests.js";

test("ready batch and emitted batches are consumed serially with ack", async () => {
  const calls = [];
  const invoke = async (cmd, args) => {
    calls.push([cmd, args]);
    if (cmd === "open_requests_ready") return { batchId: 1, target: "main",
      items: [{ kind: "open", requestId: 11, path: "a.csv" }] };
    if (cmd === "open_requests_ack") return null;
  };
  let release;
  const gate = new Promise((resolve) => { release = resolve; });
  const order = [];
  const pump = createOpenRequestPump({
    invoke,
    listen: async () => {},
    openPath: async (path) => { order.push(`start:${path}`); await gate; order.push(`end:${path}`); },
    activatePath: async () => {},
    reportError: (e) => { throw e; },
  });
  await pump.start();
  pump.schedule({ batchId: 2, target: "main", items: [{ kind: "activate", path: "b.csv", session: 9 }] });
  await Promise.resolve();
  assert.deepEqual(order, ["start:a.csv"]);
  release();
  await pump.idle();
  assert.ok(calls.some(([cmd, args]) => cmd === "open_requests_ack" && args.batchId === 1));
});

test("failed item releases reservation through backend and does not block next item", async () => {
  const seen = [];
  const calls = [];
  const pump = createOpenRequestPump({
    invoke: async (cmd, args) => { calls.push([cmd, args]); return null; },
    listen: async () => {},
    openPath: async (path) => { seen.push(path); if (path === "bad.csv") throw new Error("bad"); },
    activatePath: async (path) => { seen.push(`activate:${path}`); },
    reportError: () => {},
  });
  await pump.consume({ batchId: 1, target: "main", items: [
    { kind: "open", requestId: 1, path: "bad.csv" },
    { kind: "activate", path: "ok.csv", session: 2 },
  ] });
  assert.deepEqual(seen, ["bad.csv", "activate:ok.csv"]);
  assert.ok(calls.some(([cmd, args]) => cmd === "open_requests_fail"
    && args.requestId === 1 && args.path === "bad.csv"));
});
