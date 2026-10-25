// dialogs.js 纯逻辑单测：新建表格的尺寸解析、默认值与夹取。
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  DEFAULT_NEW_COLS,
  DEFAULT_NEW_ROWS,
  MAX_NEW_COLS,
  MAX_NEW_ROWS,
  fixedPointMessage,
  normalizeNewTableSize,
} from "../frontend/js/dialogs.js";

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
