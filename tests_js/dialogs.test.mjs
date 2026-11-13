// dialogs.js 纯逻辑单测：新建表格的尺寸解析、默认值与夹取。
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  BIG_WRITE_PHASES,
  DEFAULT_NEW_COLS,
  DEFAULT_NEW_ROWS,
  MAX_NEW_COLS,
  MAX_NEW_ROWS,
  fixedPointMessage,
  normalizeNewTableSize,
} from "../frontend/js/dialogs.js";

test("大文件写入阶段区分正文计算、RAM、磁盘和原子换名", () => {
  assert.deepEqual(BIG_WRITE_PHASES, [
    "正在计算正文…", "正在整理内存…", "正在写入磁盘…", "正在完成原子换名…",
  ]);
  assert.equal(BIG_WRITE_PHASES.some((text) => text.includes("重建行索引")), false);
});

test("新建表格：默认 5 列 × 60 行", () => {
  assert.equal(DEFAULT_NEW_COLS, 5);
  assert.equal(DEFAULT_NEW_ROWS, 60);
  assert.deepEqual(normalizeNewTableSize("", ""), { cols: 5, rows: 60 });
  assert.deepEqual(normalizeNewTableSize("   ", null), { cols: 5, rows: 60 });
  assert.deepEqual(normalizeNewTableSize(undefined, undefined), { cols: 5, rows: 60 });
});

test("新建表格：按输入解析列数与行数", () => {
  assert.deepEqual(normalizeNewTableSize("3", "2"), { cols: 3, rows: 2 });
  assert.deepEqual(normalizeNewTableSize("12", "60"), { cols: 12, rows: 60 });
});

test("新建表格：小数取整，0/负数夹到 1，超上限夹到上限", () => {
  assert.deepEqual(normalizeNewTableSize("2.6", "0.4"), { cols: 3, rows: 1 });
  assert.deepEqual(normalizeNewTableSize("0", "-5"), { cols: 1, rows: 1 });
  assert.deepEqual(normalizeNewTableSize("999", "999999"),
    { cols: MAX_NEW_COLS, rows: MAX_NEW_ROWS });
});

test("新建表格：非数字回退默认值", () => {
  assert.deepEqual(normalizeNewTableSize("abc", "12x"), { cols: 5, rows: 60 });
  assert.deepEqual(normalizeNewTableSize(Infinity, NaN), { cols: 5, rows: 60 });
});

const fixedBase = {
  operation: "粘贴", requiredBytes: 9 * 1024 * 1024,
  budgetBytes: 64 * 1024 * 1024, preflightId: 1,
};

test("固定点：单步超预算文案包含体量与撤销边界", () => {
  const text = fixedPointMessage({ ...fixedBase, kind: "stepOverBudget" });
  assert.match(text, /操作：粘贴/);
  assert.match(text, /所需历史体量：9.0 MB/);
  assert.match(text, /当前每文档预算：64.0 MB/);
  assert.match(text, /此前历史与本次操作不可撤销/);
  assert.match(text, /之后的新操作可以撤销到固定点/);
});

test("固定点：保存转换显示具体原因", () => {
  const text = fixedPointMessage({
    ...fixedBase, kind: "saveTransform", operation: "Source 列 age 仍需由历史恢复",
  });
  assert.match(text, /Source 列 age 仍需由历史恢复/);
  assert.match(text, /此次保存将建立固定点/);
  assert.match(text, /之后的新操作可以撤销到固定点/);
});

test("固定点：排序保存使用物化专用文案", () => {
  const text = fixedPointMessage({ ...fixedBase, kind: "sortMaterialization" });
  assert.match(text, /按当前排序保存会物化当前行序/);
  assert.doesNotMatch(text, /列历史清空/);
  assert.match(text, /此前历史与本次操作不可撤销/);
});

test("固定点确认对话框 API：可注入引擎确认流程", async () => {
  const { confirmFixedPointDialog } = await import("../frontend/js/dialogs.js");
  assert.equal(typeof confirmFixedPointDialog, "function");
});

test("撤销预算对话框 API：返回异步 MiB 设置入口", async () => {
  const { undoBudgetDialog } = await import("../frontend/js/dialogs.js");
  assert.equal(typeof undoBudgetDialog, "function");
});

test("旧数字版撤销预算调用继续返回 MiB 数字", async () => {
  const dom = installDialogDom();
  try {
    const { undoBudgetDialog } = await import("../frontend/js/dialogs.js");
    const pending = undoBudgetDialog(32);
    const body = currentDialogBody(dom.document);
    body.querySelector("input").value = "48";
    clickDialogButton(dom.document, "确定");
    assert.equal(await pending, 48);
  } finally {
    dom.restore();
  }
});

test("固定点：退出内存模式说明普通预算和取消保留现场", () => {
  const text = fixedPointMessage({ ...fixedBase, kind: "memoryModeExit" });
  assert.match(text, /退出内存模式将恢复普通撤销预算/);
  assert.match(text, /取消则保持内存模式、RAM 后备和当前编辑现场/);
  assert.match(text, /之后的新操作可以撤销到固定点/);
});

test("撤销设置对话框双输入实时试算并返回整数千分比", async () => {
  const dom = installDialogDom();
  try {
    const { undoBudgetDialog } = await import("../frontend/js/dialogs.js");
    const pending = undoBudgetDialog(
      { budgetMiB: 64, residentPermille: 1000 },
      { hasPath: true, totalBytes: 3n * 1024n * 1024n * 1024n, memoryMode: false },
    );
    const body = currentDialogBody(dom.document);
    const budget = body.querySelector("#undo-budget-mib");
    const resident = body.querySelector("#undo-resident-percent");
    assert.ok(budget && resident, "必须提供普通预算和驻留比例两个输入");
    assert.equal(budget.step, "1");
    assert.equal(resident.step, "0.1");
    assert.match(body.textContent, /max（普通预算，当前后备文件总字节数 × 驻留撤销百分比）/);
    budget.value = "1";
    budget.dispatch("input");
    resident.value = "200.0";
    resident.dispatch("input");
    assert.match(body.textContent, /当前有效预算：1.00 MiB/);
    assert.match(body.textContent, /若开启后的有效预算：6.00 GiB/);
    clickDialogButton(dom.document, "确定");
    assert.deepEqual(await pending, { budgetMiB: 1, residentPermille: 2000 });
  } finally {
    dom.restore();
  }
});

test("无磁盘后备时取消返回 null 且不伪造文件大小", async () => {
  const dom = installDialogDom();
  try {
    const { undoBudgetDialog } = await import("../frontend/js/dialogs.js");
    const pending = undoBudgetDialog(
      { budgetMiB: 32, residentPermille: 250 },
      { hasPath: false, totalBytes: 0, memoryMode: false },
    );
    const body = currentDialogBody(dom.document);
    assert.match(body.textContent, /当前文档无磁盘后备，内存模式不可开启/);
    assert.doesNotMatch(body.textContent, /当前文件大小：0 B/);
    clickDialogButton(dom.document, "取消");
    assert.equal(await pending, null);
  } finally {
    dom.restore();
  }
});

function currentDialogBody(document) {
  return document.body.children[0].children[0].children[1].children[0];
}

function clickDialogButton(document, text) {
  const button = document.body.all("button").find((item) => item.textContent === text);
  assert.ok(button, `找不到按钮：${text}`);
  button.click();
}

function installDialogDom() {
  class FakeNode {
    constructor(tagName) {
      this.tagName = tagName.toUpperCase();
      this.children = [];
      this.parent = null;
      this.style = {};
      this.listeners = new Map();
      this._text = "";
      this.value = "";
    }
    set textContent(value) {
      this._text = String(value);
      for (const child of this.children) child.parent = null;
      this.children = [];
    }
    get textContent() {
      return this._text + this.children.map((child) => child.textContent).join("");
    }
    get isConnected() { return this.parent !== null; }
    appendChild(child) { child.parent = this; this.children.push(child); return child; }
    append(...children) { for (const child of children) this.appendChild(child); }
    remove() {
      if (!this.parent) return;
      this.parent.children = this.parent.children.filter((child) => child !== this);
      this.parent = null;
    }
    addEventListener(type, callback) {
      if (!this.listeners.has(type)) this.listeners.set(type, []);
      this.listeners.get(type).push(callback);
    }
    removeEventListener(type, callback) {
      this.listeners.set(type, (this.listeners.get(type) ?? []).filter((cb) => cb !== callback));
    }
    dispatch(type) { for (const cb of this.listeners.get(type) ?? []) cb({ target: this }); }
    click() { this.dispatch("click"); }
    focus() {}
    matches(selector) {
      if (selector.startsWith("#")) return this.id === selector.slice(1);
      if (selector.startsWith(".")) return this.className?.split(" ").includes(selector.slice(1));
      return this.tagName === selector.toUpperCase();
    }
    all(selector) {
      const selectors = selector.split(",").map((item) => item.trim());
      const found = [];
      const visit = (node) => {
        if (selectors.some((item) => node.matches(item))) found.push(node);
        for (const child of node.children) visit(child);
      };
      for (const child of this.children) visit(child);
      return found;
    }
    querySelector(selector) { return this.all(selector)[0] ?? null; }
  }
  const document = new FakeNode("#document");
  document.body = document.appendChild(new FakeNode("body"));
  document.createElement = (tag) => new FakeNode(tag);
  const previous = globalThis.document;
  globalThis.document = document;
  return {
    document,
    restore() {
      if (previous === undefined) delete globalThis.document;
      else globalThis.document = previous;
    },
  };
}
