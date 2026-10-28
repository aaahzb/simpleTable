import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import { makeSharedBackend } from "./helpers/shared_bigmodel_fake.mjs";
import { isSupportedDocumentPath } from "../frontend/js/filetypes.js";

test("open entries accept CSV TXT LOG and reject other extensions", () => {
  for (const path of ["a.csv", "b.CSV", "c.txt", "d.TxT", "e.log", "f.LOG"]) {
    assert.equal(isSupportedDocumentPath(path), true, path);
  }
  for (const path of ["a.csv.exe", "README.md", "txt", "acsv", "a.tsv"]) {
    assert.equal(isSupportedDocumentPath(path), false, path);
  }
});

function loadControllerFactory() {
  const app = readFileSync(new URL("../frontend/js/app.js", import.meta.url), "utf8");
  const marker = "function createTabDragController(";
  const start = app.indexOf(marker);
  assert.notEqual(start, -1, "app 应提供 Pointer Capture 拖放控制器");
  const end = app.indexOf("\n/**", start + marker.length);
  assert.ok(end > start, "拖放控制器后应有明确的函数边界");
  const context = { console };
  vm.runInNewContext(`${app.slice(start, end)}\nglobalThis.factory = createTabDragController;`, context);
  return context.factory;
}

function plain(value) {
  return JSON.parse(JSON.stringify(value));
}

function pointerEvent(element, x, y, pointerId = 1) {
  return {
    pointerId, clientX: x, clientY: y, button: 0, isPrimary: true,
    currentTarget: element, defaultPrevented: false,
    preventDefault() { this.defaultPrevented = true; },
  };
}

function harness(overrides = {}) {
  const createTabDragController = loadControllerFactory();
  const calls = [];
  const captures = [];
  const releases = [];
  const element = {
    setPointerCapture(id) { captures.push(id); },
    releasePointerCapture(id) { releases.push(id); },
  };
  let dropOutcome = { kind: "invalid" };
  const deps = {
    invoke: async (cmd, args = {}) => {
      calls.push([cmd, args]);
      if (cmd === "begin_tab_drag") return 7;
      if (cmd === "tab_drag_hover") return { kind: "invalid" };
      if (cmd === "drop_tab_drag") return dropOutcome;
      return null;
    },
    getTab: (id) => ({ id, title: "a.csv", transferState: "idle", model: { sessionId: 11, lease: 3 } }),
    moveTab: async (...args) => { calls.push(["moveTab", args]); return true; },
    transferTabToWindow: async (...args) => { calls.push(["transfer", args]); return {}; },
    createWindow: async (position) => { calls.push(["createWindow", position]); return "doc-9"; },
    discardWindow: async (label) => { calls.push(["discardWindow", label]); },
    computeTarget: (id, x, y) => ({ side: "A", index: 2, id, x, y }),
    previewTarget: () => {},
    clearPreview: () => {},
    setSourceActive: () => {},
    onError: (error) => { calls.push(["error", String(error)]); },
    ...overrides,
  };
  const controller = createTabDragController(deps);
  return {
    controller, calls, captures, releases, element,
    setDrop(outcome) { dropOutcome = outcome; },
    named(name) { return calls.filter(([cmd]) => cmd === name); },
  };
}

async function dragToUp(h) {
  await h.controller.pointerDown(pointerEvent(h.element, 10, 10), 1);
  await h.controller.pointerMove(pointerEvent(h.element, 30, 10));
  return h.controller.pointerUp(pointerEvent(h.element, 30, 10));
}

test("same-window drop only calls moveTab with the computed group index", async () => {
  const h = harness();
  h.setDrop({ kind: "sameWindow", x: 120, y: 30 });
  await dragToUp(h);
  assert.deepEqual(h.named("moveTab").map(([, args]) => args), [[1, "A", 2]]);
  assert.equal(h.named("transfer").length, 0);
  assert.equal(h.named("createWindow").length, 0);
  assert.equal(h.named("discardWindow").length, 0);
  assert.deepEqual(h.captures, [1]);
  assert.deepEqual(h.releases, [1]);
});

test("existing-window drop transfers the same session to the replied side and index", async () => {
  const h = harness();
  h.setDrop({ kind: "existing", target: "doc-2", side: "B", index: 1 });
  await dragToUp(h);
  assert.deepEqual(h.named("transfer").map(([, args]) => args), [[11, "doc-2", "B", 1]]);
  assert.equal(h.named("moveTab").length, 0);
  assert.equal(h.named("createWindow").length, 0);
  assert.equal(h.named("discardWindow").length, 0);
});

test("desktop drop creates a hidden positioned window before transferring", async () => {
  const h = harness();
  h.setDrop({ kind: "newWindow", x: -400, y: 220 });
  await dragToUp(h);
  const order = h.calls.filter(([cmd]) => ["createWindow", "transfer"].includes(cmd));
  assert.deepEqual(plain(order[0]), ["createWindow", { x: -400, y: 220 }]);
  assert.deepEqual(order[1], ["transfer", [11, "doc-9", "A", Number.MAX_SAFE_INTEGER]]);
  assert.equal(h.named("discardWindow").length, 0);
});

test("invalid drop and Escape never migrate or close a source window", async () => {
  for (const escape of [false, true]) {
    const h = harness();
    await h.controller.pointerDown(pointerEvent(h.element, 10, 10), 1);
    await h.controller.pointerMove(pointerEvent(h.element, 30, 10));
    if (escape) await h.controller.cancel("Escape");
    else await h.controller.pointerUp(pointerEvent(h.element, 30, 10));
    assert.equal(h.named("moveTab").length, 0);
    assert.equal(h.named("transfer").length, 0);
    assert.equal(h.named("createWindow").length, 0);
    assert.equal(h.named("discardWindow").length, 0);
    assert.equal(h.named("drop_tab_drag").length, escape ? 0 : 1);
    assert.equal(h.named("cancel_tab_drag").length, escape ? 1 : 0);
  }
});

test("a failed desktop transfer discards only its newly created empty window", async () => {
  const h = harness({
    transferTabToWindow: async () => { throw new Error("accept failed"); },
  });
  h.setDrop({ kind: "newWindow", x: 50, y: 60 });
  assert.equal(await dragToUp(h), false);
  assert.deepEqual(h.named("discardWindow").map(([, label]) => label), ["doc-9"]);
  assert.ok(h.named("error").some(([, message]) => message.includes("accept failed")));
});

test("movement over six CSS pixels starts capture; exactly six does not", async () => {
  const h = harness();
  await h.controller.pointerDown(pointerEvent(h.element, 10, 10), 1);
  await h.controller.pointerMove(pointerEvent(h.element, 16, 10));
  assert.equal(h.named("begin_tab_drag").length, 0);
  assert.equal(h.captures.length, 0);
  await h.controller.pointerMove(pointerEvent(h.element, 17, 10));
  assert.deepEqual(plain(h.named("begin_tab_drag")[0][1]), { session: 11, lease: 3, title: "a.csv", clientX: 17, clientY: 10 });
  assert.deepEqual(h.captures, [1]);
});

test("drag begin carries the real tab title for the native floating preview", async () => {
  const h = harness();
  await dragToUp(h);
  assert.deepEqual(plain(h.named("begin_tab_drag")[0][1]), {
    session: 11, lease: 3, title: "a.csv", clientX: 30, clientY: 10,
  });
});

test("RDP drag carries captured client coordinates through begin hover and drop", async () => {
  const h = harness();
  await h.controller.pointerDown(pointerEvent(h.element, 10, 10), 1);
  await h.controller.pointerMove(pointerEvent(h.element, 30, 20));
  await h.controller.pointerMove(pointerEvent(h.element, -40, 900));
  await h.controller.pointerUp(pointerEvent(h.element, -50, 920));
  assert.deepEqual(plain(h.named("begin_tab_drag")[0][1]), {
    session: 11, lease: 3, title: "a.csv", clientX: 30, clientY: 20,
  });
  assert.deepEqual(plain(h.named("tab_drag_hover").at(-1)[1]), {
    dragToken: 7, clientX: -40, clientY: 900,
  });
  assert.deepEqual(plain(h.named("drop_tab_drag")[0][1]), {
    dragToken: 7, clientX: -50, clientY: 920,
  });
});

test("source tab grabbed styling is applied and restored", () => {
  const app = readFileSync(new URL("../frontend/js/app.js", import.meta.url), "utf8");
  const start = app.indexOf("function setTabDragSourceVisual(");
  assert.notEqual(start, -1, "app 应提供源标签抓取态函数");
  const end = app.indexOf("\n/**", start + 1);
  assert.ok(end > start);
  const element = { style: { opacity: "", cursor: "" } };
  const bodyClasses = new Set();
  const context = {
    tabScrollEl: { querySelector: () => element },
    document: { body: { classList: {
      toggle: (name, force) => {
        if (force) bodyClasses.add(name); else bodyClasses.delete(name);
      },
    } } },
  };
  vm.runInNewContext(app.slice(start, end) + "\nglobalThis.apply = setTabDragSourceVisual;", context);
  context.apply(true, 1);
  assert.equal(element.style.opacity, "0.45");
  assert.equal(element.style.cursor, "grabbing");
  assert.equal(bodyClasses.has("tab-dragging"), true, "整个源窗口都应显示抓取光标");
  context.apply(false, 1);
  assert.equal(element.style.opacity, "");
  assert.equal(element.style.cursor, "");
  assert.equal(bodyClasses.has("tab-dragging"), false);
  assert.ok(app.includes("setTabDragSourceVisual(active, id);"));
  const html = readFileSync(new URL("../frontend/index.html", import.meta.url), "utf8");
  assert.ok(html.includes("body.tab-dragging,"));
  assert.ok(html.includes("body.tab-dragging * { cursor: grabbing !important; }"));
});

test("native preview page uses a text-only title and never starts the document app", () => {
  const file = new URL("../frontend/drag-preview.html", import.meta.url);
  assert.ok(existsSync(file), "应存在独立的原生拖动预览页");
  const html = readFileSync(file, "utf8");
  assert.ok(html.includes("URLSearchParams"));
  assert.ok(html.includes(".textContent = title"));
  assert.doesNotMatch(html, /innerHTML/);
  assert.doesNotMatch(html, /js\/app\.js/);
});

test("target drop computes its real A/B insertion index and rejects grid areas", async () => {
  const h = harness({
    computeTarget: (id, x, y) => (y <= 30 ? { side: "B", index: 2 } : null),
  });
  await h.controller.targetDrop({ dropId: 44, dragToken: 7, x: 100, y: 20 });
  await h.controller.targetDrop({ dropId: 45, dragToken: 7, x: 100, y: 200 });
  const replies = h.named("tab_drag_drop_result").map(([, args]) => args);
  assert.deepEqual(plain(replies), [
    { dropId: 44, side: "B", index: 2 },
    { dropId: 45, side: null, index: null },
  ]);
});

test("target reply side and index reach seal unchanged", async () => {
  const backend = makeSharedBackend({ rows: 2, cols: 1 });
  const source = backend.model("main");
  await source.open("a.csv");
  const sealCalls = [];
  const originalSeal = backend.seal;
  backend.seal = (...args) => {
    sealCalls.push(args);
    return originalSeal(...args);
  };
  const createTabDragController = loadControllerFactory();
  let dropReply = null;
  const target = createTabDragController({
    invoke: async (cmd, args) => {
      if (cmd === "tab_drag_drop_result") dropReply = { side: args.side, index: args.index };
      return null;
    },
    getTab: () => null,
    computeTarget: () => ({ side: "B", index: 2 }),
  });
  const element = { setPointerCapture() {}, releasePointerCapture() {} };
  const sourceController = createTabDragController({
    invoke: async (cmd, args = {}) => {
      if (cmd === "begin_tab_drag") return 7;
      if (cmd === "tab_drag_hover") return { kind: "invalid" };
      if (cmd === "drop_tab_drag") {
        await target.targetDrop({ dropId: 44, dragToken: args.dragToken, x: 100, y: 20 });
        return { kind: "existing", target: "doc-4", ...dropReply };
      }
      return null;
    },
    getTab: () => ({ id: 1, title: "a.csv", transferState: "idle", model: source }),
    computeTarget: () => null,
    transferTabToWindow: async (session, targetLabel, side, index) => {
      const snapshot = await source.prepareTransfer();
      backend.seal(session, "main", source.lease, targetLabel, side, index, snapshot);
    },
  });
  await sourceController.pointerDown(pointerEvent(element, 10, 10), 1);
  await sourceController.pointerMove(pointerEvent(element, 30, 10));
  await sourceController.pointerUp(pointerEvent(element, 30, 10));
  assert.equal(sealCalls.length, 1);
  assert.equal(sealCalls[0][3], "doc-4");
  assert.equal(sealCalls[0][4], "B");
  assert.equal(sealCalls[0][5], 2);
});



test("a real DocTab uses model transfer state and can begin dragging", async () => {
  const h = harness({
    getTab: (id) => ({
      id, title: "a.csv",
      model: { sessionId: 11, lease: 3, _transferState: "idle" },
    }),
  });
  await h.controller.pointerDown(pointerEvent(h.element, 10, 10), 1);
  await h.controller.pointerMove(pointerEvent(h.element, 30, 10));
  assert.equal(h.named("begin_tab_drag").length, 1);
  assert.deepEqual(h.captures, [1]);
});

test("pointer capture starts before pending begin so fast release can drop", async () => {
  let releaseBegin;
  const beginGate = new Promise((resolve) => { releaseBegin = resolve; });
  let h;
  h = harness({
    invoke: async (cmd, args = {}) => {
      h.calls.push([cmd, args]);
      if (cmd === "begin_tab_drag") return beginGate;
      if (cmd === "tab_drag_hover") return { kind: "invalid" };
      if (cmd === "drop_tab_drag") return { kind: "sameWindow", x: 120, y: 30 };
      return null;
    },
  });
  await h.controller.pointerDown(pointerEvent(h.element, 10, 10), 1);
  const moving = h.controller.pointerMove(pointerEvent(h.element, 30, 10));
  try {
    assert.deepEqual(h.captures, [1], "阈值后必须同步捕获，不能等 begin IPC");
  } finally {
    releaseBegin(7);
    await moving;
  }
  await h.controller.pointerUp(pointerEvent(h.element, 30, 10));
  assert.deepEqual(h.named("moveTab").map(([, args]) => args), [[1, "A", 2]]);
});


test("a rejected drop command cancels its native token before local cleanup", async () => {
  const h = harness({
    invoke: async (cmd, args = {}) => {
      h.calls.push([cmd, args]);
      if (cmd === "begin_tab_drag") return 7;
      if (cmd === "tab_drag_hover") return { kind: "invalid" };
      if (cmd === "drop_tab_drag") throw new Error("cursor unavailable");
      return null;
    },
  });
  assert.equal(await dragToUp(h), false);
  assert.deepEqual(plain(h.named("cancel_tab_drag").map(([, args]) => args)), [
    { dragToken: 7 },
  ]);
  assert.ok(h.named("error").some(([, message]) => message.includes("cursor unavailable")));
});

test("an empty window hides the tabbar but one document keeps a draggable tab", () => {
  const app = readFileSync(new URL("../frontend/js/app.js", import.meta.url), "utf8");
  const start = app.indexOf("function renderTabbar()");
  const end = app.indexOf("\n/**", start + 1);
  assert.ok(start >= 0 && end > start, "应能提取真实标签条渲染函数");
  const classes = new Set();
  const classList = {
    toggle(name, force) {
      if (force ?? !classes.has(name)) classes.add(name); else classes.delete(name);
    },
  };
  const groups = Object.fromEntries(["tab-group-a", "tab-group-b"].map((id) => [id, {
    children: [],
    set textContent(value) { if (value === "") this.children = []; },
    appendChild(element) { this.children.push(element); },
  }]));
  const context = {
    tabs: [], activeId: null, dirty: false, splitOn: false, focusedSide: "A",
    sideTab: { A: null, B: null },
    tabbarEl: { classList },
    tabScrollEl: { querySelector: () => null },
    updateTabOverflow: () => {},
    document: {
      getElementById: (id) => groups[id],
      createElement: () => ({
        dataset: {}, style: {}, children: [],
        appendChild(element) { this.children.push(element); },
        addEventListener: () => {},
      }),
    },
  };
  vm.runInNewContext(app.slice(start, end) + "\nglobalThis.render = renderTabbar;", context);
  context.render();
  assert.equal(classes.has("show"), false, "空窗口不应显示空标签条");
  context.tabs = [{ id: 1, title: "c.csv", path: "c.csv", side: "A", dirty: false }];
  context.activeId = 1;
  context.sideTab.A = 1;
  context.render();
  assert.equal(classes.has("show"), true, "单标签窗口必须保留可抓取的源标签");
  assert.equal(groups["tab-group-a"].children.length, 1);
  assert.equal(groups["tab-group-a"].children[0].dataset.id, "1");
});

test("first file materializes and renders a single tab", async () => {
  const app = readFileSync(new URL("../frontend/js/app.js", import.meta.url), "utf8");
  const start = app.indexOf("async function openPathsAsTabs(");
  const end = app.indexOf("\n/**", start + 1);
  assert.ok(start >= 0 && end > start, "应能提取真实首文件打开函数");
  let rendered = 0;
  let activated = 0;
  const context = {
    tabs: [], activeId: null, dirty: false,
    confirmDiscard: async () => true,
    openPath: async () => true,
    ensureTabbed: () => {
      if (context.activeId !== null) return;
      context.tabs.push({ id: 1, path: "a.csv" });
      context.activeId = 1;
    },
    activeTab: () => context.tabs.find((tab) => tab.id === context.activeId) ?? null,
    loadIntoNewTab: async () => { throw new Error("首文件不应新建第二个标签"); },
    activateTab: async () => { activated += 1; return true; },
    renderTabbar: () => { rendered += 1; },
  };
  vm.runInNewContext(app.slice(start, end) + "\nglobalThis.open = openPathsAsTabs;", context);
  assert.equal(await context.open(["a.csv"]), true);
  assert.equal(context.activeId, 1);
  assert.equal(activated, 0, "首签已经活动，不应依赖额外切签");
  assert.equal(rendered, 1, "首签实体化后必须立即渲染标签条");
});

test("a one-tab target exposes drop geometry and whole-window grabbing during external hover", () => {
  const app = readFileSync(new URL("../frontend/js/app.js", import.meta.url), "utf8");
  const extract = (name, next) => {
    const start = app.indexOf(`function ${name}(`);
    const end = app.indexOf(`function ${next}(`, start + 1);
    assert.ok(start >= 0 && end > start, `${name} source boundary`);
    return app.slice(start, end);
  };
  const classes = new Set();
  const classList = {
    add: (...names) => names.forEach((name) => classes.add(name)),
    remove: (...names) => names.forEach((name) => classes.delete(name)),
    toggle: (name, force) => {
      if (force ?? !classes.has(name)) classes.add(name); else classes.delete(name);
    },
  };
  const bodyClasses = new Set();
  const bodyClassList = {
    add: (...names) => names.forEach((name) => bodyClasses.add(name)),
    remove: (...names) => names.forEach((name) => bodyClasses.delete(name)),
  };
  const kid = { dataset: { id: "99" }, getBoundingClientRect: () => ({
    left: 0, right: 100, width: 100,
  }) };
  const group = (side) => ({
    querySelectorAll: () => side === "A" ? [kid] : [],
    getBoundingClientRect: () => {
      const visible = classes.has("show") && (side === "A" || classes.has("dragpeek"));
      return visible
        ? { left: side === "A" ? 0 : 200, top: 0, right: side === "A" ? 200 : 290, bottom: 26, width: side === "A" ? 200 : 90, height: 26 }
        : { left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 };
    },
  });
  const context = {
    console,
    tabs: [{ id: 99, side: "A" }],
    splitOn: false,
    tabbarEl: { classList },
    groupTabs: (side) => [{ id: 99, side: "A" }].filter((tab) => tab.side === side),
    document: {
      body: { classList: bodyClassList },
      getElementById: (id) => id === "tab-insert"
        ? { style: {} }
        : group(id === "tab-group-b" ? "B" : "A"),
    },
  };
  vm.runInNewContext([
    extract("hideInsertIndicator", "tabDropTargetAt"),
    extract("tabDropTargetAt", "previewTabDropTarget"),
    extract("prepareTabDropTarget", "reportTabDragError"),
  ].join("\n"), context);
  assert.equal(context.tabDropTargetAt(null, 50, 10), null);
  context.prepareTabDropTarget();
  assert.equal(bodyClasses.has("tab-dragging"), true, "目标窗口同样显示抓取光标");
  assert.equal(context.tabDropTargetAt(null, 50, 10)?.side, "A");
  assert.equal(context.tabDropTargetAt(null, 240, 10)?.side, "B");
  context.clearTabDragPreview();
  assert.equal(bodyClasses.has("tab-dragging"), false, "目标清理后恢复光标");
  assert.equal(classes.has("show"), true, "清理后单标签窗口仍保留可抓取标签条");
});

test("app wires pointer capture and window-scoped target events", async () => {
  const app = readFileSync(new URL("../frontend/js/app.js", import.meta.url), "utf8");
  assert.ok(app.includes("const tabDragController = createTabDragController({"));
  assert.ok(app.includes("el.addEventListener(\"pointerdown\""));
  assert.doesNotMatch(app, /el.draggables*=s*true/);
  assert.doesNotMatch(app, /dragstart/);
  assert.ok(app.includes("getCurrentWindow().listen(\"tab-drag-drop\""));
  assert.ok(app.includes("await tabDragListenerReady"));
});
