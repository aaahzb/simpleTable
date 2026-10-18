// model.js 单元测试：快照撤销栈、行列操作、排序（对照 table_model.py）
import assert from "node:assert/strict";
import { test } from "node:test";

import { SORT_NUMERIC, SORT_TEXT, TableModel } from "../frontend/js/model.js";

/**
 * 构造 3x3 测试模型。
 *
 * @returns {TableModel} 测试模型。
 */
function makeModel() {
  const m = new TableModel();
  m.load(["目的地", "评分", "备注"], [
    ["大理", "3", "古城"],
    ["丽江", "10", "雪山"],
    ["洱海", "5", ""],
  ]);
  return m;
}

test("load 可标记为无列名：行数据原样保留", () => {
  const m = new TableModel();
  m.load(["列1", "列2"], [["张三", "1"], ["李四", "2"]], { headerless: true });
  assert.equal(m.isHeaderless(), true);
  assert.deepEqual(m.headers(), ["列1", "列2"]);
  assert.deepEqual(m.allRows(), [["张三", "1"], ["李四", "2"]]);
});

test("默认 load 仍为有列名模式", () => {
  const m = makeModel();
  assert.equal(m.isHeaderless(), false);
});

test("开启无列名：原表头成为首行，列名变为 列1..N", () => {
  const m = makeModel();
  m.setHeaderless(true);
  assert.equal(m.isHeaderless(), true);
  assert.equal(m.rowCount(), 4);
  assert.deepEqual(m.headers(), ["列1", "列2", "列3"]);
  assert.deepEqual(m.allRows()[0], ["目的地", "评分", "备注"]);
  assert.deepEqual(m.allRows()[1], ["大理", "3", "古城"]);
});

test("关闭无列名：当前首行（含编辑）提升为列名", () => {
  const m = makeModel();
  m.setHeaderless(true);
  m.setData(0, 0, "地点");
  m.setData(0, 2, "说明");
  m.setHeaderless(false);
  assert.equal(m.isHeaderless(), false);
  assert.equal(m.rowCount(), 3);
  assert.deepEqual(m.headers(), ["地点", "评分", "说明"]);
  assert.deepEqual(m.allRows()[0], ["大理", "3", "古城"]);
});

test("0 行时关闭无列名抛错且保持无列名", () => {
  const m = new TableModel();
  m.load(["列1"], [], { headerless: true });
  assert.throws(() => m.setHeaderless(false), /没有数据行可作为列名/);
  assert.equal(m.isHeaderless(), true);
});

test("0 列时开启无列名抛错", () => {
  const m = new TableModel();
  assert.throws(() => m.setHeaderless(true), /没有列可作为列名/);
  assert.equal(m.isHeaderless(), false);
});

test("开关清空撤销/重做栈", () => {
  const m = makeModel();
  m.setData(0, 0, "X");
  assert.equal(m.canUndo(), true);
  m.undo();
  assert.equal(m.canRedo(), true);
  m.setHeaderless(true);
  assert.equal(m.canUndo(), false);
  assert.equal(m.canRedo(), false);
});

test("开关以 origin='view' 通知数据变化", () => {
  const m = makeModel();
  const events = [];
  m.onData((structural, origin) => events.push([structural, origin]));
  m.setHeaderless(true);
  assert.deepEqual(events.at(-1), [true, "view"]);
  m.setHeaderless(false);
  assert.deepEqual(events.at(-1), [true, "view"]);
});

test("两个模型实例的开关互相独立", () => {
  const a = makeModel();
  const b = makeModel();
  a.setHeaderless(true);
  assert.equal(a.isHeaderless(), true);
  assert.equal(b.isHeaderless(), false);
  assert.equal(b.rowCount(), 3);
});

test("无列名下列增删后列名保持 列1..N 连续", () => {
  const m = makeModel();
  m.setHeaderless(true);
  m.appendColumn("随便");
  assert.deepEqual(m.headers(), ["列1", "列2", "列3", "列4"]);
  m.insertColumns(1, 2);
  assert.deepEqual(m.headers(), ["列1", "列2", "列3", "列4", "列5", "列6"]);
  m.removeColumn(0);
  assert.deepEqual(m.headers(), ["列1", "列2", "列3", "列4", "列5"]);
  m.removeColumns([1, 3]);
  assert.deepEqual(m.headers(), ["列1", "列2", "列3"]);
});

test("无列名下重命名列被忽略", () => {
  const m = makeModel();
  m.setHeaderless(true);
  m.renameColumn(0, "新名");
  assert.equal(m.headers()[0], "列1");
});

test("无列名模式禁止删除最后一行", () => {
  const m = new TableModel();
  m.load(["列1"], [["only"]], { headerless: true });
  assert.throws(() => m.removeRows([0]), /无列名模式需至少保留一行/);
  assert.equal(m.rowCount(), 1);
});

test("无列名模式允许删除部分行，但不能清空", () => {
  const m = new TableModel();
  m.load(["列1"], [["a"], ["b"], ["c"]], { headerless: true });
  m.removeRows([0, 2]);
  assert.deepEqual(m.allRows(), [["b"]]);
  assert.throws(() => m.removeRows([0]), /无列名模式需至少保留一行/);
});

test("load 清空撤销栈并触发结构通知", () => {
  const m = makeModel();
  let structural = 0;
  let undoEvents = 0;
  m.onData((s) => { if (s) structural += 1; });
  m.onUndoStack(() => { undoEvents += 1; });
  m.setData(0, 0, "X");
  assert.equal(m.canUndo(), true);
  m.load(["a"], [["b"]]);
  assert.equal(m.canUndo(), false);
  assert.equal(m.canRedo(), false);
  assert.equal(structural >= 1, true); // load 本身算一次结构通知
  assert.equal(undoEvents >= 2, true);
});

test("setData → undo 复原 → redo 恢复；新改动清空 redo", () => {
  const m = makeModel();
  m.setData(0, 0, "昆明");
  assert.equal(m.rawData(0, 0), "昆明");
  assert.equal(m.undo(), true);
  assert.equal(m.rawData(0, 0), "大理");
  assert.equal(m.redo(), true);
  assert.equal(m.rawData(0, 0), "昆明");
  m.undo();
  m.setData(1, 1, "99");
  assert.equal(m.canRedo(), false);
});

test("撤销栈上限 100（超出丢最旧）", () => {
  const m = makeModel();
  for (let i = 0; i < 105; i += 1) {
    m.setData(0, 0, `v${i}`);
  }
  // 内部栈检查：105 步压栈后应只剩 100
  assert.equal(m._undoStack.length, 100);
});

test("大表（字符超预算）撤销上限降为 20 且只提示一次", () => {
  const m = makeModel();
  // 3,000,000 字符预算：250 行 × 25 列 × 500 字符/格 = 3,125,000 字符
  const bigCell = "x".repeat(500);
  m.load(["h"], Array.from({ length: 250 },
    () => Array.from({ length: 25 }, () => bigCell)));
  let notices = 0;
  m.onUndoCap?.(() => { notices += 1; });
  for (let i = 0; i < 25; i += 1) {
    m.setData(0, 0, `v${i}`);
  }
  assert.equal(m._undoStack.length, 20);
  assert.equal(notices, 1, "撤销降级应恰好提示一次");
});

test("setDataMerged 同格连续 = 一步；换格 = 新一步", () => {
  const m = makeModel();
  m.setDataMerged(0, 0, "昆");
  m.setDataMerged(0, 0, "昆明");
  m.setDataMerged(0, 0, "昆明市");
  assert.equal(m._undoStack.length, 1);
  m.setDataMerged(0, 1, "8");
  assert.equal(m._undoStack.length, 2);
  m.undo();
  assert.equal(m.rawData(0, 1), "3");
  assert.equal(m.rawData(0, 0), "昆明市");
  m.undo();
  assert.equal(m.rawData(0, 0), "大理");
});

test("batch 内多次改动 = 一步", () => {
  const m = makeModel();
  m.batch(() => {
    m.setData(0, 0, "a");
    m.setData(1, 1, "b");
    m.insertRows(1, 2);
  });
  assert.equal(m._undoStack.length, 1);
  m.undo();
  assert.equal(m.rawData(0, 0), "大理");
  assert.equal(m.rowCount(), 3);
});

test("removeColumns 批量 = 一步", () => {
  const m = makeModel();
  m.removeColumns([0, 2]);
  assert.equal(m._undoStack.length, 1);
  assert.deepEqual(m.headers(), ["评分"]);
  m.undo();
  assert.deepEqual(m.headers(), ["目的地", "评分", "备注"]);
  assert.equal(m.rowCount(), 3);
});

test("行列增删与越界夹取", () => {
  const m = makeModel();
  m.insertRows(99, 2); // 夹取到末尾
  assert.equal(m.rowCount(), 5);
  m.appendRows(1);
  assert.equal(m.rowCount(), 6);
  m.removeRows([0, 5, 99]); // 99 忽略
  assert.equal(m.rowCount(), 4);
  m.insertColumns(0, 2);
  assert.deepEqual(m.headers(), ["新列", "新列2", "目的地", "评分", "备注"]);
  m.removeColumn(0);
  m.removeColumn(0);
  assert.deepEqual(m.headers(), ["目的地", "评分", "备注"]);
  m.renameColumn(0, "城市");
  assert.equal(m.headers()[0], "城市");
});

test("clearRows/clearColumns/clearCells 保留结构", () => {
  const m = makeModel();
  m.clearRows([0, 1]);
  assert.equal(m.rowCount(), 3);
  assert.deepEqual(m.rawData(0, 0), "");
  m.clearColumns([0]);
  assert.deepEqual(m.rawData(2, 0), "");
  assert.equal(m.headers().length, 3);
  m.setData(0, 0, "x");
  m.clearCells([[0, 0], [2, 2]]);
  assert.deepEqual(m.rawData(0, 0), "");
  assert.deepEqual(m.rawData(2, 2), "");
});

test("sortRows 字符升/降序（稳定，码点序）", () => {
  const m = makeModel();
  m.appendRows(1); // 追加空行（"" 在码点序中最小，排最前）
  // 码点序：丽(U+4E3D) < 大(U+5927) < 洱(U+6D31)，与 Python/JS 一致
  m.sortRows([[0, SORT_TEXT, true]]);
  assert.deepEqual(m.rawData(0, 0), "");
  assert.deepEqual(m.rawData(1, 0), "丽江");
  assert.deepEqual(m.rawData(2, 0), "大理");
  assert.deepEqual(m.rawData(3, 0), "洱海");
  m.sortRows([[0, SORT_TEXT, false]]);
  assert.deepEqual(m.rawData(0, 0), "洱海");
});

test("sortRows 数字方式：非数字恒排最后（与升降序无关）", () => {
  const m = new TableModel();
  m.load(["n"], [["3"], ["x"], ["1"], [""], ["2"]]);
  m.sortRows([[0, SORT_NUMERIC, true]]);
  assert.deepEqual(
    m.allRows().map((r) => r[0]), ["1", "2", "3", "x", ""]);
  m.sortRows([[0, SORT_NUMERIC, false]]);
  assert.deepEqual(
    m.allRows().map((r) => r[0]), ["3", "2", "1", "x", ""]);
});

test("sortRows 多层级优先级", () => {
  const m = new TableModel();
  m.load(["a", "b"], [["x", "2"], ["y", "1"], ["x", "1"]]);
  m.sortRows([[0, SORT_TEXT, true], [1, SORT_NUMERIC, true]]);
  assert.deepEqual(m.allRows(), [["x", "1"], ["x", "2"], ["y", "1"]]);
});

test("displayData 压平换行 Tab", () => {
  const m = new TableModel();
  m.load(["a"], [["line1\nline2\tend"]]);
  assert.equal(m.displayData(0, 0), "line1 line2 end");
});

test("appendRows/insertRows 在零列模型上为空操作", () => {
  const m = new TableModel();
  m.load([], []);
  m.appendRows(3);
  m.insertRows(0, 3);
  assert.equal(m.rowCount(), 0);
  assert.equal(m.canUndo(), false);
});

test("undo 后 sortRows 恢复原行序", () => {
  const m = makeModel();
  const before = m.allRows();
  m.sortRows([[0, SORT_TEXT, true]]);
  m.undo();
  assert.deepEqual(m.allRows(), before);
});
