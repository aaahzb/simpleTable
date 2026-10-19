// dialogs.js 纯逻辑单测：新建表格的尺寸解析、默认值与夹取。
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  DEFAULT_NEW_COLS,
  DEFAULT_NEW_ROWS,
  MAX_NEW_COLS,
  MAX_NEW_ROWS,
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
