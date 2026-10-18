// csv.js 单元测试：RFC 4180 解析/序列化（对照 Python csv 行为）
import assert from "node:assert/strict";
import { test } from "node:test";

import { parseCsv, serializeCsv, serializeRecords } from "../frontend/js/csv.js";

test("基本读写往返", () => {
  const headers = ["a", "b", "c"];
  const rows = [["1", "2", "3"], ["x,y", 'he said "hi"', "line1\nline2"]];
  const text = serializeCsv(headers, rows);
  assert.equal(text, 'a,b,c\r\n1,2,3\r\n"x,y","he said ""hi""","line1\nline2"\r\n');
  const back = parseCsv(text);
  assert.deepEqual(back.headers, headers);
  assert.deepEqual(back.rows, rows);
  assert.deepEqual(back.warnings, []);
});

test("空文件返回空", () => {
  assert.deepEqual(parseCsv(""), { headers: [], rows: [], warnings: [] });
  assert.deepEqual(parseCsv("\r\n\r\n"), { headers: [], rows: [], warnings: [] });
});

test("短行补空 + 警告", () => {
  const r = parseCsv("a,b,c\r\n1,2\r\n");
  assert.deepEqual(r.rows, [["1", "2", ""]]);
  assert.equal(r.warnings.length, 1);
  assert.match(r.warnings[0], /数据行 1 只有 2 列/);
});

test("长行截断 + 警告", () => {
  const r = parseCsv("a,b\r\n1,2,3,4\r\n");
  assert.deepEqual(r.rows, [["1", "2"]]);
  assert.match(r.warnings[0], /数据行 1 有 4 列/);
});

test("引号内逗号换行双引号", () => {
  const r = parseCsv('a,b,c\r\n"x,\ny","z""q""",plain\r\n');
  assert.deepEqual(r.rows, [["x,\ny", 'z"q"', "plain"]]);
});

test("跳过完全空行但保留空单元格行", () => {
  // 注意：",," 是 3 个空字段，对 2 列表头会产生截断警告（与 Python csv 一致）
  const r = parseCsv("a,b\r\n\r\n,\r\n\r\n1,2\r\n");
  assert.deepEqual(r.rows, [["", ""], ["1", "2"]]);
  assert.deepEqual(r.warnings, []);
});

test("无引号字段原样", () => {
  const r = parseCsv("h1,h2\nv1,v2\n");
  assert.deepEqual(r.headers, ["h1", "h2"]);
  assert.deepEqual(r.rows, [["v1", "v2"]]);
});

test("仅引号包裹不触发重引号", () => {
  // 内容与 Python csv.writer 一致：无特殊字符不加引号
  assert.equal(serializeCsv(["a"], [["plain"]]), "a\r\nplain\r\n");
});

test("无列名：首行保留为数据，列名虚拟为 列1..N", () => {
  const r = parseCsv("张三,1\r\n李四,2\r\n", ",", { headerless: true });
  assert.deepEqual(r.headers, ["列1", "列2"]);
  assert.deepEqual(r.rows, [["张三", "1"], ["李四", "2"]]);
  assert.deepEqual(r.warnings, []);
});

test("无列名：只有一行时也是数据行", () => {
  const r = parseCsv("a,b,c", ",", { headerless: true });
  assert.deepEqual(r.headers, ["列1", "列2", "列3"]);
  assert.deepEqual(r.rows, [["a", "b", "c"]]);
});

test("无列名：参差行补空/截断，警告行号按数据行从 1 起", () => {
  const short = parseCsv("a,b,c\r\n1,2\r\n", ",", { headerless: true });
  assert.deepEqual(short.rows, [["a", "b", "c"], ["1", "2", ""]]);
  assert.equal(short.warnings.length, 1);
  assert.match(short.warnings[0], /数据行 2 只有 2 列（首行 3 列）/);

  const long = parseCsv("a,b\r\n1,2,3\r\n", ",", { headerless: true });
  assert.deepEqual(long.rows, [["a", "b"], ["1", "2"]]);
  assert.match(long.warnings[0], /数据行 2 有 3 列（首行 2 列）/);
});

test("无列名：空文件仍返回空", () => {
  assert.deepEqual(
    parseCsv("", ",", { headerless: true }),
    { headers: [], rows: [], warnings: [] },
  );
});

test("默认仍按首行为列名", () => {
  const r = parseCsv("h1,h2\r\nv1,v2\r\n");
  assert.deepEqual(r.headers, ["h1", "h2"]);
  assert.deepEqual(r.rows, [["v1", "v2"]]);
});

test("serializeRecords 空数组写出单个空行（保存守卫依赖此行为）", () => {
  assert.equal(serializeRecords([]), "\r\n");
});
