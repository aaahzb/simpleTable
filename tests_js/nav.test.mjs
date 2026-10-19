// nav.js 单元测试：Excel Ctrl+↑/↓ 的数据边缘查找语义。
import assert from "node:assert/strict";
import { test } from "node:test";

import { findVerticalEdge } from "../frontend/js/nav.js";

/**
 * 用布尔数组造一个访问器。
 *
 * @param {boolean[]} values - 每行是否非空。
 * @returns {(i: number) => boolean} 访问器。
 */
function accessor(values) {
  return (i) => values[i];
}

test("非空格向下：停在连续非空块的最后一行", () => {
  const v = [true, true, true, false, false];
  assert.equal(findVerticalEdge(accessor(v), 0, v.length, true), 2);
});

test("非空格向下：已在块边缘时越过空档到下一条非空", () => {
  const v = [true, true, false, false, true, true];
  assert.equal(findVerticalEdge(accessor(v), 1, v.length, true), 4);
  assert.equal(findVerticalEdge(accessor(v), 4, v.length, true), 5);
});

test("空格向下：跳到下一个非空；没有则落到末行", () => {
  const v = [false, false, true, false];
  assert.equal(findVerticalEdge(accessor(v), 0, v.length, true), 2);
  const empty = [false, false, false];
  assert.equal(findVerticalEdge(accessor(empty), 0, empty.length, true), 2);
});

test("向上与向下对称：块边缘越过空档到上一条非空", () => {
  const v = [true, true, false, true, true];
  assert.equal(findVerticalEdge(accessor(v), 4, v.length, false), 3);
  // 从下块首格再按一次：越过空档到上块末格（Excel 语义）
  assert.equal(findVerticalEdge(accessor(v), 3, v.length, false), 1);
  assert.equal(findVerticalEdge(accessor(v), 1, v.length, false), 0);
});

test("空格向上：跳到上一个非空；没有则落到首行", () => {
  const empty = [false, false, false];
  assert.equal(findVerticalEdge(accessor(empty), 2, empty.length, false), 0);
  const v = [true, false, false];
  assert.equal(findVerticalEdge(accessor(v), 2, v.length, false), 0);
});

test("单行/越界起点不会越界", () => {
  assert.equal(findVerticalEdge(accessor([true]), 0, 1, true), 0);
  assert.equal(findVerticalEdge(accessor([true]), 0, 1, false), 0);
  assert.equal(findVerticalEdge(accessor([true, false]), 99, 2, true), 1);
  // 向上越界起点夹到末行；末行非空且上方也非空 → 连续块边缘 0
  assert.equal(findVerticalEdge(accessor([false, true]), 99, 2, false), 0);
});

test("空表返回 -1（调用方据此不动）", () => {
  assert.equal(findVerticalEdge(() => true, 0, 0, true), -1);
});
