import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

import { makeSharedBackend } from "./helpers/shared_bigmodel_fake.mjs";

const app = readFileSync(new URL("../frontend/js/app.js", import.meta.url), "utf8");
const html = readFileSync(new URL("../frontend/index.html", import.meta.url), "utf8");

function deferModelCommand(model, command) {
  const original = model._rawInvoke;
  const commands = [];
  let notifyEntered;
  let releaseGate;
  const entered = new Promise((resolve) => { notifyEntered = resolve; });
  const gate = new Promise((resolve) => { releaseGate = resolve; });
  model._rawInvoke = async function (cmd, args) {
    commands.push(cmd);
    if (cmd === command) {
      notifyEntered();
      await gate;
    }
    return original.call(this, cmd, args);
  };
  return {
    entered,
    release: () => releaseGate(),
    commands,
    restore: () => { model._rawInvoke = original; },
  };
}

async function makeMemoryRegistry() {
  const { createMemoryOperationRegistry } = await import("../frontend/js/windowlifecycle.js");
  assert.equal(typeof createMemoryOperationRegistry, "function");
  return createMemoryOperationRegistry();
}

test("A 的内存任务不阻塞 B 编辑且只通知 A 的页签", async () => {
  const backend = makeSharedBackend({ rows: 2, cols: 1, totalBytes: 64 * 1024 * 1024 });
  const a = backend.model("main");
  const b = backend.model("main");
  await a.open("/a.csv");
  await b.open("/b.csv");
  const estimate = await a.memoryEstimate();
  assert.equal(estimate.available_physical_bytes, 1 << 30);
  assert.equal(estimate.available_commit_bytes, 1 << 30);
  assert.equal(Object.hasOwn(estimate, "available_bytes"), false);
  const gate = deferModelCommand(a, "big_memory_mode_enter");
  const tasks = await makeMemoryRegistry();
  const events = [];
  const pending = tasks.run(a, "enter", () => a.enterMemoryMode(), {
    tabId: 11,
    onChange: () => events.push([11, tasks.kind(a)]),
  });
  try {
    await gate.entered;
    assert.equal(tasks.kind(a), "enter");
    assert.equal(tasks.kind(b), null);
    await assert.rejects(a.setData(0, 0, "A 不应写入"), /内存模式任务正在执行/);
    const changed = await b.setData(0, 0, "B 已完成");
    assert.equal(changed.changed, true);
    assert.equal(b.memoryMode(), false);
    assert.deepEqual(events, [[11, "enter"]]);
  } finally {
    gate.release();
    gate.restore();
  }
  await pending;
  assert.equal(a.memoryMode(), true);
  assert.equal(b.memoryMode(), false);
  assert.deepEqual(events, [[11, "enter"], [11, null]]);
});

test("settle 先 cancel 并等待 enter 落定后才允许关闭引擎", async () => {
  const backend = makeSharedBackend({ rows: 2, cols: 1, totalBytes: 64 * 1024 * 1024 });
  const model = backend.model("main");
  await model.open("/a.csv");
  const gate = deferModelCommand(model, "big_memory_mode_enter");
  const tasks = await makeMemoryRegistry();
  const pending = tasks.run(model, "enter", () => model.enterMemoryMode(), { tabId: 1 });
  let settling;
  try {
    await gate.entered;
    settling = tasks.settle(model);
    assert.equal(backend.calls.at(-1)[0], "big_memory_mode_cancel");
    assert.equal(backend.calls.some(([cmd]) => cmd === "big_close"), false);
  } finally {
    gate.release();
  }
  await settling;
  await pending;
  await model.close();
  gate.restore();
  const commands = gate.commands;
  assert.ok(commands.indexOf("big_memory_mode_enter") < commands.indexOf("big_memory_mode_cancel"));
  assert.ok(commands.indexOf("big_memory_mode_cancel") < commands.lastIndexOf("big_close"));
});

test("App 在关闭引擎前 settle 对应文档任务", () => {
  assert.match(app, /settleDocumentTasks:\s*async\s*\(tab\)/);
  const settle = app.indexOf("await settleMemoryOperation(t.model)");
  const close = app.indexOf("await t.model?.close?.()");
  assert.ok(settle >= 0, "detachTab 必须先等待内存任务");
  assert.ok(close > settle, "big_close 必须发生在 settle 之后");
});

test("网格内只有唯一非模态内存进度条", () => {
  assert.equal((html.match(/id="memory-progress"/g) ?? []).length, 1);
  const grid = html.indexOf('<div id="grid-area">');
  const progress = html.indexOf('id="memory-progress"');
  const scroll = html.indexOf('id="grid-scroll"');
  assert.ok(grid >= 0 && progress > grid && scroll > progress);
  assert.match(html, /\.memory-progress\.hidden\s*\{\s*display:\s*none/);
  assert.match(app, /onMemoryProgress/);
  assert.match(app, /const memoryOperations = new WeakMap\(\)/);
});

test("启动和修改设置均使用同一组双值策略", () => {
  assert.match(app, /createUndoSettingsPrefs/);
  assert.match(app, /undoBudgetDialog\(undoSettings/);
  assert.match(app, /residentPermille:\s*value\.residentPermille/);
  assert.match(app, /applyUndoSettings/);
});

test("保存开始在途刷新互斥状态并在收尾恢复", async () => {
  const operations = new Map();
  let releaseSave;
  let notifyEntered;
  const entered = new Promise((resolve) => { notifyEntered = resolve; });
  const gate = new Promise((resolve) => { releaseSave = resolve; });
  const states = [];
  const dialog = { set() {}, close() {}, finish: async () => {} };
  const context = {
    model: {
      filterActive: () => false,
      onProgress: async () => () => {},
      rowCount: () => 1,
      meta: () => ({ rows: 1 }),
    },
    progressDialog: () => dialog,
    refreshBigUi: () => states.push([...operations.values()]
      .some((operation) => operation.cmd === "big_save")),
    setDirty: () => {},
    fgrid: () => ({ clearFilters: () => {} }),
    onCountsChanged: () => {},
    statusMsg: () => {},
    errorDialog: async (message) => { throw new Error(message); },
  };
  vm.runInNewContext(
    extractFunction(app, "runBigWrite") + "\nglobalThis.run = runBigWrite;", context);
  const write = () => {
    const key = Symbol("save");
    const promise = gate.then(() => { operations.delete(key); return true; });
    operations.set(key, { cmd: "big_save", promise });
    notifyEntered();
    return promise;
  };
  const pending = context.run(write, "保存", "已保存", "重试");
  await entered;
  const during = [...states];
  releaseSave();
  assert.equal(await pending, true);
  assert.deepEqual(during, [true], "保存 IPC 在途时必须立即刷新按钮为互斥禁用");
  assert.equal(states.at(-1), false, "保存收尾必须刷新并解除互斥禁用");
});

test("RAM 提交后磁盘失败保留 dirty 并同步筛选和行数", async () => {
  const dirtySyncs = [];
  const filterClears = [];
  const counts = [];
  const errors = [];
  let finishes = 0;
  const context = {
    model: {
      filterActive: () => true,
      onProgress: async () => () => {},
      rowCount: () => 2,
      meta: () => ({ rows: 5, modified: true }),
    },
    progressDialog: () => ({
      set() {}, close() {}, finish: async () => { finishes += 1; },
    }),
    refreshBigUi: () => {},
    setDirty: (value) => dirtySyncs.push(value),
    fgrid: () => ({ clearFilters: (opts) => filterClears.push(opts) }),
    onCountsChanged: (visible, total) => counts.push([visible, total]),
    statusMsg: () => {},
    errorDialog: async (title, message) => errors.push([title, message]),
  };
  vm.runInNewContext(
    extractFunction(app, "runBigWrite") + "\nglobalThis.run = runBigWrite;", context);
  const saved = await context.run(
    async () => { throw new Error("RAM_COMMITTED:写入临时文件失败"); },
    "保存", "已保存", "请重试",
  );
  assert.equal(saved, false);
  assert.deepEqual(dirtySyncs, [true]);
  assert.equal(filterClears.length, 1);
  assert.equal(filterClears[0].anchor, false);
  assert.deepEqual(counts, [[2, 5]]);
  assert.equal(finishes, 0, "失败不得显示最终 100% 完成帧");
  assert.match(errors[0][1], /RAM_COMMITTED:写入临时文件失败/);
  assert.match(errors[0][1], /请重试/);
});

test("applyColOpsNow 不再读取 meta().memoryMode，也不创建进度对话框", async () => {
  const dirty = [];
  const statuses = [];
  let dialogs = 0;
  const context = {
    model: {
      colOpsPending: () => true,
      indexing: () => false,
      meta: () => ({ memoryMode: true }),
      onProgress: async () => { throw new Error("不得订阅内存进度"); },
      applyColOps: async () => ({ changed: true, cancelled: false }),
    },
    progressDialog: () => {
      dialogs += 1;
      return { set() {}, close() {}, finish: async () => {} };
    },
    setDirty: (value) => dirty.push(value),
    statusMsg: (text) => statuses.push(text),
    errorDialog: async (title, message) => { throw new Error(`${title}:${message}`); },
    refreshBigUi: () => {},
  };
  const source = extractFunction(app, "applyColOpsNow");
  assert.doesNotMatch(source, /memoryMode/, "持久模式没有 pending，字段分支是死代码");
  vm.runInNewContext(source + "\nglobalThis.apply = applyColOpsNow;", context);
  await context.apply();
  assert.equal(dialogs, 0, "列操作预览只提交 Schema，不再有整理进度");
  assert.deepEqual(dirty, [true]);
  assert.deepEqual(statuses, ["列操作已应用，可撤销"]);
});

test("applyColOpsNow 无 pending 时直接返回，不读元数据", async () => {
  let metaCalls = 0;
  const context = {
    model: {
      colOpsPending: () => false,
      indexing: () => false,
      meta: () => { metaCalls += 1; return {}; },
      applyColOps: async () => { throw new Error("不得调用"); },
    },
    setDirty: () => { throw new Error("不得标脏"); },
    statusMsg: () => {},
    errorDialog: async () => {},
    refreshBigUi: () => {},
  };
  vm.runInNewContext(
    extractFunction(app, "applyColOpsNow") + "\nglobalThis.apply = applyColOpsNow;", context);
  await context.apply();
  assert.equal(metaCalls, 0);
});

test("列操作在途时保存等待旧任务与模型列任务，避免保存成功后被重新标脏", async () => {
  const events = [];
  let releaseApply;
  let releaseColumns;
  const applyGate = new Promise((resolve) => { releaseApply = resolve; });
  const columnGate = new Promise((resolve) => { releaseColumns = resolve; });
  const context = {
    model: {
      colOpsPending: () => true,
      indexing: () => false,
      meta: () => ({}),
      columnOpsBusy: () => true,
      waitForColumnOps: async () => {
        await columnGate;
        events.push("column-ops");
      },
    },
    applyColOpsNow: async () => {
      const outcome = await applyGate;
      // 真实 applyColOpsNow 在返回前完成 UI 收尾（标脏）；用事件模拟同一顺序。
      events.push("dirty:true");
      return outcome;
    },
    progressDialog: () => ({ set() {}, close() {}, finish: async () => {} }),
    setDirty: (value) => events.push(`dirty:${value}`),
    statusMsg: () => {},
    errorDialog: async (title, message) => { throw new Error(`${title}:${message}`); },
    refreshBigUi: () => {},
    path: "/a.csv",
    saveFileAs: async () => true,
    runBigWrite: async () => {
      events.push("save-start");
      events.push("dirty:false");
      return true;
    },
  };
  vm.runInNewContext(
    `let columnApplyTask = null;
${extractFunction(app, "waitForColumnApply")}
${extractFunction(app, "applyColOps")}
${extractFunction(app, "saveFile")}
globalThis.apply = applyColOps;
globalThis.save = saveFile;`,
    context,
  );
  const applying = context.apply();
  await new Promise((resolve) => setTimeout(resolve, 0));
  const saving = context.save();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(events, [], "列任务未落定时保存不得开始，也不得提前标脏/清脏");
  releaseApply({ changed: true, cancelled: false });
  await applying;
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(events, ["dirty:true"], "模型列任务未落定时仍不得保存");
  releaseColumns();
  assert.equal(await saving, true);
  assert.deepEqual(events, ["dirty:true", "column-ops", "save-start", "dirty:false"]);
});

test("columnOp：内存模式立即生效才标脏并提示，取消/失败不误标", async () => {
  const run = async (input, memoryMode = true) => {
    const dirty = [];
    const statuses = [];
    const errors = [];
    const context = {
      model: { memoryMode: () => memoryMode },
      setDirty: (value) => dirty.push(value),
      statusMsg: (text) => statuses.push(text),
      errorDialog: async (title, message) => errors.push([title, message]),
    };
    vm.runInNewContext(
      extractFunction(app, "columnOp") + "\nglobalThis.op = columnOp;", context);
    context.op(input);
    await new Promise((resolve) => setTimeout(resolve, 0));
    return { dirty, statuses, errors };
  };
  const changed = await run(Promise.resolve({ changed: true, cancelled: false }));
  assert.deepEqual(changed.dirty, [true]);
  assert.match(changed.statuses[0], /已生效/);
  assert.match(changed.statuses[0], /保存时整理正文/);

  const ordinary = await run(Promise.resolve({ changed: true, cancelled: false }), false);
  assert.deepEqual(ordinary.dirty, [true]);
  assert.deepEqual(ordinary.statuses, ["列操作已应用，可撤销"]);

  const cancelled = await run(Promise.resolve({ changed: false, cancelled: true }));
  assert.deepEqual(cancelled.dirty, []);
  assert.match(cancelled.statuses[0], /取消/);

  const unchanged = await run(Promise.resolve({ changed: false, cancelled: false }));
  assert.deepEqual(unchanged.dirty, []);
  assert.deepEqual(unchanged.statuses, []);

  const failed = await run({ then(_resolve, reject) { reject(new Error("boom")); } });
  assert.deepEqual(failed.dirty, []);
  assert.deepEqual(failed.errors, [["操作失败", "boom"]]);
});

test("工具栏和状态栏区分持久内存与临时排序 RAM", () => {
  assert.match(app, /btn-memory-mode/);
  assert.match(app, /临时排序 RAM/);
  assert.match(app, /保留内存模式/);
  assert.match(app, /first\.fixedPoint/);
  assert.doesNotMatch(app, /first\.fixed_point/);
});


function extractFunction(source, name) {
  const asyncStart = source.indexOf(`async function ${name}(`);
  const syncStart = source.indexOf(`function ${name}(`);
  const start = asyncStart >= 0 ? asyncStart : syncStart;
  const end = source.indexOf("\n/**", start + 1);
  assert.ok(start >= 0 && end > start, `应提取真实 ${name}`);
  return source.slice(start, end);
}

function extractMemoryCommand() {
  const start = app.indexOf("async function runMemoryModeCommand(");
  const end = app.indexOf(String.fromCharCode(10) + "memoryCancelButton.addEventListener", start + 1);
  assert.ok(start >= 0 && end > start, "应精确提取真实 runMemoryModeCommand");
  return app.slice(start, end);
}

function memoryCommandHarness(operationModel, {
  confirmed = false,
  fixedPointConfirmed = false,
} = {}) {
  const errors = [];
  const dirtySyncs = [];
  const statuses = [];
  const confirmations = [];
  const context = {
    model: operationModel,
    MEMORY_EXCLUSIVE_COMMANDS: new Set([
      "big_save", "big_save_as", "big_sort_load", "big_sort_apply", "big_sort_exit",
      "big_col_apply", "big_col_clear",
    ]),
    runMemoryOperation: async (_model, _kind, action) => action(),
    confirmDialog: async (text) => { confirmations.push(text); return confirmed; },
    confirmFixedPointDialog: async () => fixedPointConfirmed,
    formatByteQuantity: (value) => `${value} B`,
    syncModelDirty: (...args) => dirtySyncs.push(args),
    updateUndoButtons: () => {},
    updateStatusText: () => {},
    statusMsg: (text) => statuses.push(text),
    reportMemoryModeError: async (_model, error) => { errors.push(String(error)); },
  };
  const commandSource = extractMemoryCommand();
  const exclusiveSource = extractFunction(app, "hasMemoryExclusiveOperation");
  vm.runInNewContext(
    `${exclusiveSource}\n${commandSource}\nglobalThis.runCommand = runMemoryModeCommand;`,
    context,
  );
  return { context, errors, dirtySyncs, statuses, confirmations };
}

test("内存模式命令函数可按真实模块语法解析", () => {
  const source = extractMemoryCommand();
  assert.doesNotThrow(() => new vm.Script(source, { filename: "runMemoryModeCommand.js" }));
});

test("保存或排序在途时内存命令在估算和登记任务前拒绝", async () => {
  let estimates = 0;
  const operationModel = {
    _operations: new Map([[Symbol("save"), { cmd: "big_save" }]]),
    meta: () => ({ has_path: true }),
    memoryMode: () => false,
    memoryEstimate: async () => { estimates += 1; return { can_enter: false }; },
  };
  const harness = memoryCommandHarness(operationModel);
  assert.equal(await harness.context.runCommand(operationModel, "enter"), null);
  assert.equal(estimates, 0);
  assert.equal(harness.errors.length, 1);
  assert.match(harness.errors[0], /保存、排序或列操作/);
  assert.deepEqual(harness.dirtySyncs, []);
});

test("退出固定点取消不重试并保留内存文档现场", async () => {
  const calls = [];
  const fixedPoint = { kind: "memoryModeExit", preflightId: 91 };
  const operationModel = {
    _operations: new Map(),
    meta: () => ({ has_path: true, memory_mode: true }),
    memoryMode: () => true,
    exitMemoryMode: async (options) => {
      calls.push(options);
      return { changed: false, cancelled: false, fixedPoint };
    },
  };
  const harness = memoryCommandHarness(operationModel);
  const outcome = await harness.context.runCommand(operationModel, "exit");
  assert.equal(outcome.cancelled, true);
  assert.deepEqual(calls, [undefined]);
  assert.deepEqual(harness.dirtySyncs, []);
  assert.deepEqual(harness.statuses, ["已取消退出内存模式"]);
});

test("无路径新建 RAM 排序后不显示临时或持久内存标签", () => {
  const cases = [
    [{ has_path: false, sorted: true, ram_backed: true, memory_mode: false }, ""],
    [{ has_path: true, sorted: true, ram_backed: true, memory_mode: false }, "临时排序 RAM"],
    [{ has_path: true, sorted: true, ram_backed: true, memory_mode: true }, "内存模式"],
    [{ has_path: true, sorted: false, ram_backed: false, memory_mode: false }, ""],
  ];
  for (const [meta, expected] of cases) {
    const statusText = { textContent: "" };
    const context = {
      model: { meta: () => meta, filterActive: () => false },
      lastCounts: [2, 2],
      lastFilterMs: 0,
      bigTimingSuffix: () => "",
      undoSettings: { budgetMiB: 64, residentPermille: 1000 },
      statusText,
      fmtBytes: (value) => `${value} B`,
      fmtDuration: () => "",
    };
    vm.runInNewContext(extractFunction(app, "updateStatusText"), context);
    context.updateStatusText();
    if (expected) assert.match(statusText.textContent, new RegExp(expected));
    else {
      assert.doesNotMatch(statusText.textContent, /临时排序 RAM|内存模式/);
    }
  }
});


test("纯 RAM 提升确认明确不复制且展示真实 105% 估算", async () => {
  const calls = [];
  const operationModel = {
    _operations: new Map(),
    meta: () => ({ has_path: true, ram_backed: true }),
    memoryMode: () => false,
    memoryEstimate: async () => ({
      file_bytes: 100,
      limit_bytes: 105,
      available_physical_bytes: 80,
      available_commit_bytes: 90,
      already_ram: true,
      can_enter: true,
    }),
    enterMemoryMode: async () => {
      calls.push("enter");
      return { changed: true, cancelled: false, fixedPoint: null };
    },
    modified: () => false,
  };
  const harness = memoryCommandHarness(operationModel, { confirmed: true });
  const outcome = await harness.context.runCommand(operationModel, "enter");
  assert.equal(outcome.changed, true);
  assert.deepEqual(calls, ["enter"]);
  assert.equal(harness.confirmations.length, 1);
  assert.match(harness.confirmations[0], /文件大小：100 B/);
  assert.match(harness.confirmations[0], /转换私有内存上限：105 B/);
  assert.match(harness.confirmations[0], /不再复制完整文件/);
  assert.match(harness.confirmations[0], /可用物理内存不足/);
});

test("真实转换进度事件前不显示复制条，纯 RAM 提升保持隐藏", () => {
  const a = { name: "a" };
  const b = { name: "b" };
  const scroll = {};
  const area = {
    insertBefore(element, before) {
      element.parentElement = area;
      element.nextElementSibling = before;
    },
  };
  let hidden = true;
  const memoryProgressEl = {
    parentElement: area,
    nextElementSibling: scroll,
    classList: { toggle: (name, force) => { if (name === "hidden") hidden = force; } },
  };
  const memoryProgressText = { textContent: "" };
  const memoryProgressBar = { value: 0 };
  const memoryCancelButton = { disabled: true };
  const memoryProgress = new WeakMap();
  const context = {
    model: a,
    splitOn: false,
    activeTab: () => ({ model: context.model, side: "A" }),
    memoryTaskRegistry: { kind: (item) => item === a ? "enter" : null },
    memoryProgress,
    memoryProgressEl,
    memoryProgressText,
    memoryProgressBar,
    memoryCancelButton,
    document: { getElementById: (id) => id === "grid-area" ? area : scroll },
    formatByteQuantity: (value) => `${value} B`,
  };
  vm.runInNewContext(
    extractFunction(app, "refreshTabMemoryUi") + "\nglobalThis.refresh = refreshTabMemoryUi;",
    context,
  );
  context.refresh({ model: a });
  assert.equal(hidden, true, "估算、确认和纯 RAM 提升均没有复制事件，不应显示复制条");
  memoryProgress.set(a, { done: 1, total: 2 });
  context.refresh({ model: a });
  assert.equal(hidden, false);
  assert.equal(memoryProgressText.textContent, "转为内存模式：1 B / 2 B");
  assert.equal(memoryProgressBar.value, 50);
  assert.equal(memoryCancelButton.disabled, false);
  context.model = b;
  context.refresh({ model: b });
  assert.equal(hidden, true);
});
