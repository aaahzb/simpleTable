import assert from "node:assert/strict";
import { test } from "node:test";
import { makeSharedBackend } from "./helpers/shared_bigmodel_fake.mjs";

test("adopt keeps one session, history, dirty and filter without reopening", async () => {
  const backend = makeSharedBackend({ rows: 3, cols: 1 });
  const source = backend.model("main");
  await source.open("a.csv");
  await source.setData(0, 0, "changed");
  await source.runFilter([[0, "changed", 0]], [], "");
  const before = backend.snapshot(source.sessionId);
  await source.prepareTransfer();
  const ticket = backend.seal(source.sessionId, "main", source.lease, "doc-1", {});
  const adopted = backend.accept(ticket, "doc-1");
  const target = backend.model("doc-1");
  const callsBeforeAdopt = backend.calls.length;
  await target.adopt(adopted);
  const adoptCalls = backend.calls.slice(callsBeforeAdopt);
  assert.equal(target.sessionId, before.session);
  assert.equal(target.modified(), true);
  assert.equal(target.filterActive(), true);
  assert.equal(target.rowCount(), 1);
  assert.equal(target.canUndo(), true);
  assert.equal(
    adoptCalls.filter(([cmd]) => ["big_open", "big_new", "big_set_undo_budget"].includes(cmd)).length,
    0,
  );
  assert.equal(adoptCalls.filter(([cmd]) => cmd === "big_filter_count").length, 1);
  await assert.rejects(target.undo(), /只读|迁移/);
  backend.markAdopted(ticket, "doc-1");
  const completed = backend.complete(ticket, "main");
  target.finishTransfer(completed.lease);
  await target.undo();
  assert.equal(target.canRedo(), true);
  assert.equal(target.modified(), false);
  assert.equal(backend.snapshot(before.session).data[0][0], "r0c0");
});

test("cancel after accept restores the source lease and keeps its tab", async () => {
  const backend = makeSharedBackend({ rows: 2, cols: 1 });
  const source = backend.model("main");
  await source.open("a.csv");
  await source.prepareTransfer();
  const ticket = backend.seal(source.sessionId, "main", source.lease, "doc-1", {});
  const adopted = backend.accept(ticket, "doc-1");
  const target = backend.model("doc-1");
  await target.adopt(adopted);
  const rollback = backend.cancel(ticket, "doc-1");
  source.rollbackTransfer(rollback.lease);
  assert.equal(source.sessionId, adopted.session);
  assert.ok(rollback.lease > adopted.lease);
  await source.setData(0, 0, "source-still-writable");
  await assert.rejects(target.setData(0, 0, "stale-target"), /owner|lease|迁移|只读/);
  assert.equal(backend.tabCount("main"), 1);
});

test("stale lease progress and sort-progress events are ignored", async () => {
  const backend = makeSharedBackend({ rows: 2, cols: 1 });
  const source = backend.model("main");
  await source.open("a.csv");
  const staleLease = source.lease;
  await source.prepareTransfer();
  const ticket = backend.seal(source.sessionId, "main", staleLease, "doc-1", {});
  const adopted = backend.accept(ticket, "doc-1");
  const target = backend.model("doc-1");
  await target.adopt(adopted);
  let progressEvents = 0;
  let sortEvents = 0;
  await target.onProgress(() => { progressEvents += 1; });
  await target.onSortProgress(() => { sortEvents += 1; });
  await backend.emit("big-progress", {
    session: adopted.session, lease: staleLease, done: 1, total: 2, phase: 0, rows: 1,
  }, "doc-1");
  await backend.emit("big-sort-progress", {
    session: adopted.session, lease: staleLease, done: 1, total: 2,
  }, "doc-1");
  assert.equal(progressEvents, 0);
  assert.equal(sortEvents, 0);
  await backend.emit("big-progress", {
    session: adopted.session, lease: adopted.lease, done: 1, total: 2, phase: 0, rows: 1,
  }, "doc-1");
  await backend.emit("big-sort-progress", {
    session: adopted.session, lease: adopted.lease, done: 1, total: 2,
  }, "doc-1");
  assert.equal(progressEvents, 1);
  assert.equal(sortEvents, 1);
});

test("timeout after accept rolls back to a newer source lease", async () => {
  const backend = makeSharedBackend({ rows: 2, cols: 1 });
  const source = backend.model("main");
  await source.open("a.csv");
  await source.prepareTransfer();
  const ticket = backend.seal(source.sessionId, "main", source.lease, "doc-1", {});
  const adopted = backend.accept(ticket, "doc-1");
  const target = backend.model("doc-1");
  await target.adopt(adopted);
  const [rollback] = await backend.advanceTime(16000);
  source.rollbackTransfer(rollback.lease);
  assert.ok(rollback.lease > adopted.lease);
  await source.setData(0, 0, "source-after-timeout");
  await assert.rejects(target.setData(0, 0, "stale-target"), /owner|lease|迁移|只读/);
  assert.equal(backend.tabCount("main"), 1);
});

async function coordinatorHarness(backend, { failure = null, withTarget = true, timeoutMs = 1000, adoptGate = null, createFailure = false } = {}) {
  const { createTransferCoordinator } = await import("../frontend/js/transfer.js");
  const source = backend.model("main");
  await source.open("a.csv");
  await source.setData(0, 0, "changed");
  const sourceTabs = new Set([source.sessionId]);
  const targetTabs = new Set();
  const targetModels = new Map();
  const order = [];
  const errors = [];
  const discarded = [];

  function modelForSession(session) {
    return source.sessionId === session ? source : targetModels.get(session) ?? null;
  }
  async function adoptToTab(adopted) {
    const target = backend.model("doc-1");
    await target.adopt(adopted);
    if (adoptGate) await adoptGate();
    targetModels.set(adopted.session, target);
    if (failure === "adopt") throw new Error("adopt failed");
    targetTabs.add(adopted.session);
    return target;
  }
  function removeSourceTab(session) {
    order.push("remove");
    sourceTabs.delete(session);
    return sourceTabs.size;
  }
  function removeAdoptedTab(session) {
    targetTabs.delete(session);
    targetModels.delete(session);
  }
  function closeEmptyWindow() {
    order.push("close");
  }

  const common = {
    createWindow: async () => {
      if (createFailure) throw new Error("create failed");
      return "doc-1";
    },
    discardWindow: (label) => { discarded.push(label); },
    closeEmptyWindow,
    modelForSession,
    adoptToTab,
    removeSourceTab,
    removeAdoptedTab,
    onChanged: () => {},
    onError: (error) => errors.push(error),
    timeoutMs,
  };
  const sourceCoordinator = createTransferCoordinator({
    ...common,
    invoke: backend.invokeFor("main"),
    listen: (event, callback) => backend.listenFor("main", event, callback),
  });
  let targetCoordinator = null;
  if (withTarget) {
    const targetInvoke = backend.invokeFor("doc-1");
    targetCoordinator = createTransferCoordinator({
      ...common,
      invoke: async (cmd, args) => {
        if (failure === "accept" && cmd === "accept_transfer") throw new Error("accept failed");
        return targetInvoke(cmd, args);
      },
      listen: (event, callback) => backend.listenFor("doc-1", event, callback),
    });
  }
  return { source, sourceTabs, targetTabs, targetModels, order, errors, discarded, sourceCoordinator, targetCoordinator };
}

test("coordinator completes before removing the source and unlocks the target", async () => {
  const backend = makeSharedBackend({ rows: 2, cols: 1 });
  const harness = await coordinatorHarness(backend);
  const session = harness.source.sessionId;
  await harness.sourceCoordinator.transferTabToWindow(session, "doc-1", "A", 0);
  assert.deepEqual(harness.order, ["remove", "close"]);
  assert.equal(harness.sourceTabs.size, 0);
  assert.equal(harness.targetTabs.size, 1);
  assert.ok(backend.calls.some(([cmd]) => cmd === "complete_transfer"));
  assert.equal(backend.calls.filter(([cmd]) => cmd === "big_close").length, 0);
  const target = harness.targetModels.get(session);
  await target.setData(0, 0, "target-writable");
  assert.equal(target.canUndo(), true);
});

for (const failure of ["accept", "adopt"]) {
  test(`${failure} failure never completes or removes the source tab`, async () => {
    const backend = makeSharedBackend({ rows: 2, cols: 1 });
    const harness = await coordinatorHarness(backend, { failure });
    const session = harness.source.sessionId;
    await assert.rejects(
      harness.sourceCoordinator.transferTabToWindow(session, "doc-1", "A", 0),
      /accept|adopt|取消|失败/,
    );
    assert.equal(harness.sourceTabs.size, 1);
    assert.equal(backend.tabCount("main"), 1);
    assert.equal(backend.calls.filter(([cmd]) => cmd === "complete_transfer").length, 0);
    assert.equal(backend.calls.filter(([cmd]) => cmd === "big_close").length, 0);
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.ok(harness.errors.some((error) => String(error).includes(failure)));
    await harness.source.setData(0, 0, "source-preserved");
    assert.equal(harness.source.canUndo(), true);
  });
}

test("coordinator timeout cancels without closing the source document", async () => {
  const backend = makeSharedBackend({ rows: 2, cols: 1 });
  const harness = await coordinatorHarness(backend, { withTarget: false, timeoutMs: 10 });
  const session = harness.source.sessionId;
  await assert.rejects(
    harness.sourceCoordinator.transferTabToWindow(session, "doc-1", "A", 0),
    /超时|取消/,
  );
  assert.equal(harness.sourceTabs.size, 1);
  assert.equal(harness.order.includes("close"), false);
  assert.equal(backend.calls.filter(([cmd]) => cmd === "complete_transfer").length, 0);
  assert.equal(backend.calls.filter(([cmd]) => cmd === "big_close").length, 0);
  await harness.source.setData(0, 0, "source-after-coordinator-timeout");
});

test("app exposes the session transfer coordinator", async () => {
  const { readFileSync } = await import("node:fs");
  const app = readFileSync(new URL("../frontend/js/app.js", import.meta.url), "utf8");
  assert.match(app, /import \{ createTransferCoordinator \} from "\.\/transfer\.js";/);
  assert.match(app, /createTransferCoordinator\(\{/);
  assert.match(app, /transferTabToWindow:\s*transferCoordinator\.transferTabToWindow/);
});

test("document recovery adopts the idle session and unlocks without complete", async () => {
  const backend = makeSharedBackend({ rows: 2, cols: 1 });
  const harness = await coordinatorHarness(backend);
  const session = harness.source.sessionId;
  const ticket = backend.seal(session, "main", harness.source.lease, "doc-1", {});
  const adopted = backend.recover(ticket, "doc-1");
  await backend.emit("document-recovered", adopted, "doc-1");
  await new Promise((resolve) => setTimeout(resolve, 0));
  const target = harness.targetModels.get(session);
  assert.ok(target);
  assert.equal(harness.targetTabs.has(session), true);
  await target.setData(0, 0, "recovered-target-writable");
  assert.equal(backend.snapshot(session).data[0][0], "recovered-target-writable");
  assert.equal(backend.calls.filter(([cmd]) => cmd === "complete_transfer").length, 0);
  assert.equal(backend.calls.filter(([cmd]) => cmd === "big_close").length, 0);
});

test("prepareTransfer rejects an in-flight save and succeeds after it settles", async () => {
  const backend = makeSharedBackend({ rows: 2, cols: 1 });
  const source = backend.model("main");
  await source.open("a.csv");
  const rawInvoke = source._rawInvoke;
  let finishSave;
  source._rawInvoke = (cmd, args) => {
    if (cmd === "big_save") {
      return new Promise((resolve) => { finishSave = resolve; });
    }
    return rawInvoke(cmd, args);
  };
  const saving = source._invoke("big_save");
  await assert.rejects(source.prepareTransfer(), /当前操作尚未完成/);
  assert.equal(source._transferState, "idle");
  finishSave({});
  await saving;
  await source.prepareTransfer();
  source.rollbackTransfer(source.lease);
});

test("sealing blocks new page fetches without synchronously breaking rendering", async () => {
  const backend = makeSharedBackend({ rows: 400, cols: 1 });
  const source = backend.model("main");
  await source.open("a.csv");
  await source.prepareTransfer();
  const before = backend.calls.filter(([cmd]) => cmd === "big_rows").length;
  assert.doesNotThrow(() => source.prefetchRange(0, 50));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(backend.calls.filter(([cmd]) => cmd === "big_rows").length, before);
  source.rollbackTransfer(source.lease);
});

test("cancel during adopt removes the late target tab", async () => {
  const backend = makeSharedBackend({ rows: 2, cols: 1 });
  let releaseAdopt;
  const gate = new Promise((resolve) => { releaseAdopt = resolve; });
  const harness = await coordinatorHarness(backend, { adoptGate: () => gate });
  const transferring = harness.sourceCoordinator.transferTabToWindow(
    harness.source.sessionId, "doc-1", "A", 0,
  );
  for (let i = 0; i < 20 && !backend.calls.some(([cmd]) => cmd === "accept_transfer"); i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
  assert.ok(backend.calls.some(([cmd]) => cmd === "accept_transfer"));
  await backend.advanceTime(16000);
  await assert.rejects(transferring, /取消/);
  releaseAdopt();
  for (let i = 0; i < 20; i += 1) await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(harness.targetTabs.size, 0);
  assert.equal(backend.calls.filter(([cmd]) => cmd === "big_close").length, 0);
});

test("single-tab menu exposes disabled split and enabled new-window action", async () => {
  const { createTabMenuItems } = await import("../frontend/js/tabmenu.js");
  const calls = [];
  const items = createTabMenuItems({
    splitOn: false,
    tab: { id: 7, side: "A", transferState: "idle" },
    tabCount: 1,
    groupACount: 1,
    actions: {
      openSplit: (id) => calls.push(["split", id]),
      moveTab: (id, side) => calls.push(["move", id, side]),
      closeSplit: () => calls.push(["close-split"]),
      openTabInNewWindow: (id) => calls.push(["new-window", id]),
    },
  });
  assert.deepEqual(items.map((item) => item.label), ["在分屏中打开", "在新窗口打开"]);
  assert.equal(items[0].disabled, true);
  assert.equal(items[1].disabled, false);
  items[1].onClick();
  assert.deepEqual(calls, [["new-window", 7]]);
});

test("new-window menu completes before removing the last source tab", async () => {
  const { createTabMenuItems } = await import("../frontend/js/tabmenu.js");
  const backend = makeSharedBackend({ rows: 2, cols: 1 });
  const harness = await coordinatorHarness(backend);
  let migrating;
  const items = createTabMenuItems({
    splitOn: false,
    tab: { id: 7, side: "A", transferState: "idle" },
    tabCount: 1,
    groupACount: 1,
    actions: {
      openSplit: () => {}, moveTab: () => {}, closeSplit: () => {},
      openTabInNewWindow: () => {
        migrating = harness.sourceCoordinator.transferTabToWindow(
          harness.source.sessionId, null, "A", Number.MAX_SAFE_INTEGER,
        );
        return migrating;
      },
    },
  });
  items[1].onClick();
  assert.equal(harness.sourceTabs.size, 1);
  await migrating;
  assert.deepEqual(harness.order, ["remove", "close"]);
  assert.equal(harness.targetTabs.size, 1);
  assert.equal(backend.calls.filter(([cmd]) => cmd === "big_close").length, 0);
});

for (const failure of ["create", "accept", "timeout"]) {
  test(`${failure} failure preserves the source and only discards a new empty target`, async () => {
    const backend = makeSharedBackend({ rows: 2, cols: 1 });
    const harness = await coordinatorHarness(backend, {
      createFailure: failure === "create",
      failure: failure === "accept" ? "accept" : null,
      withTarget: failure !== "timeout",
      timeoutMs: failure === "timeout" ? 10 : 1000,
    });
    await assert.rejects(
      harness.sourceCoordinator.transferTabToWindow(
        harness.source.sessionId, null, "A", Number.MAX_SAFE_INTEGER,
      ),
      /create|accept|取消|超时/,
    );
    assert.equal(harness.sourceTabs.size, 1);
    assert.equal(harness.source.modified(), true);
    assert.equal(harness.source.canUndo(), true);
    assert.deepEqual(harness.discarded, failure === "create" ? [] : ["doc-1"]);
    assert.equal(harness.order.includes("close"), false);
    assert.equal(backend.calls.filter(([cmd]) => cmd === "big_close").length, 0);
    await harness.source.setData(0, 0, "source-preserved");
  });
}

test("app scopes all targeted backend listeners to their window", async () => {
  const { readFileSync } = await import("node:fs");
  const app = readFileSync(new URL("../frontend/js/app.js", import.meta.url), "utf8");
  assert.doesNotMatch(app, /tauri.event.listen/);
  assert.ok(app.split("getCurrentWindow().listen").length - 1 >= 3);
});

test("app wires the pure tab menu and new-window action", async () => {
  const { readFileSync } = await import("node:fs");
  const app = readFileSync(new URL("../frontend/js/app.js", import.meta.url), "utf8");
  assert.match(app, /import \{ createTabMenuItems \} from "\.\/tabmenu\.js";/);
  assert.match(app, /createTabMenuItems\(\{/);
  assert.match(app, /openTabInNewWindow/);
  assert.match(app, /discardWindow:/);
});
