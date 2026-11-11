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
