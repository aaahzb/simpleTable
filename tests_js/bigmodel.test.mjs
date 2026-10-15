// bigmodel.js 单元测试：窗口缓存/乐观编辑/筛选切换/列操作 pending。
// invoke/listen 全部注入 mock，脱离 Tauri 环境（node --test 直跑）。
import assert from "node:assert/strict";
import { test } from "node:test";

import { BigModel, BIG_PAGE, BIG_THRESHOLD } from "../frontend/js/bigmodel.js";

/** 造一个内存版"引擎"：N 行 M 列的顺序数据 + 记录 invoke 调用。 */
function makeFakeEngine({ rows = 1000, cols = 3 } = {}) {
  const data = Array.from({ length: rows }, (_, r) =>
    Array.from({ length: cols }, (_, c) => `r${r}c${c}`),
  );
  const calls = [];
  const meta = () => ({
    rows: data.length,
    cols,
    headers: Array.from({ length: cols }, (_, c) => `h${c}`),
    encoding: "utf-8",
    pending_cols: false,
    can_undo: false,
    can_redo: false,
  });
  let filter = null; // 命中视图行号数组
  const model = new BigModel({
    invoke: async (cmd, args = {}) => {
      calls.push([cmd, args]);
      switch (cmd) {
        case "big_open":
          return meta();
        case "big_meta":
          return meta();
        case "big_rows": {
          const from = Number(args.from);
          return data.slice(from, from + Number(args.count));
        }
        case "big_set_cell": {
          const r = Number(args.vrow);
          if (r >= data.length) throw new Error("行号越界");
          data[r][Number(args.col)] = args.value;
          return null;
        }
        case "big_insert_rows": {
          const at = Math.min(Number(args.vpos), data.length);
          const n = args.rows ? args.rows.length : Number(args.count);
          const blank = Array.from({ length: cols }, () => "");
          for (let i = 0; i < n; i += 1) data.splice(at + i, 0, args.rows ? [...args.rows[i]] : [...blank]);
          return null;
        }
        case "big_remove_rows": {
          for (const v of [...args.vrows].sort((a, b) => b - a)) data.splice(Number(v), 1);
          return args.vrows.length;
        }
        case "big_undo":
        case "big_redo":
          return true;
        case "big_filter": {
          const kw = String(args.spec?.cols?.[0]?.[1] ?? "");
          filter = [];
          for (let r = 0; r < data.length; r += 1) {
            if (data[r].some((c) => c.toLowerCase().includes(kw))) filter.push(r);
          }
          return filter.length;
        }
        case "big_filter_page": {
          const from = Number(args.from);
          return (filter ?? []).slice(from, from + Number(args.count)).map((r) => data[r]);
        }
        case "big_clear_filter":
          filter = null;
          return null;
        case "big_col_push":
          return null;
        case "big_col_pop":
        case "big_col_clear":
          return null;
        case "big_col_apply":
          return null;
        case "big_save":
          return null;
        case "big_stale":
          return false;
        default:
          throw new Error(`unexpected cmd ${cmd}`);
      }
    },
    listen: async () => () => {},
  });
  return { model, data, calls };
}

const tick = () => new Promise((r) => setTimeout(r, 0));

test("open 后同步元数据可用，行/列/表头正确", async () => {
  const { model } = makeFakeEngine({ rows: 1234, cols: 4 });
  await model.open("/x.csv");
  assert.equal(model.rowCount(), 1234);
  assert.equal(model.columnCount(), 4);
  assert.deepEqual(model.headers(), ["h0", "h1", "h2", "h3"]);
  assert.equal(model.encoding(), "utf-8");
  assert.equal(model.canUndo(), false);
});

test("阈值常量为 50MiB", () => {
  assert.equal(BIG_THRESHOLD, 50 * 1024 * 1024);
});

test("displayData 未命中缓存返回空串并调度拉页，落定后读到值", async () => {
  const { model } = makeFakeEngine({ rows: 500, cols: 2 });
  await model.open("/x.csv");
  assert.equal(model.displayData(10, 1), ""); // 未命中 → 占位
  await tick();
  assert.equal(model.displayData(10, 1), "r10c1"); // 页已落定
});

test("prefetchRange 拉齐范围覆盖的页", async () => {
  const { model } = makeFakeEngine({ rows: 10 * BIG_PAGE, cols: 1 });
  await model.open("/x.csv");
  model.prefetchRange(0, BIG_PAGE * 3 - 1);
  await tick();
  assert.equal(model.displayData(0, 0), "r0c0");
  assert.equal(model.displayData(BIG_PAGE * 3 - 1, 0), `r${BIG_PAGE * 3 - 1}c0`);
  // 未预取的页仍为空串占位
  assert.equal(model.displayData(BIG_PAGE * 5, 0), "");
});

test("缓存换出：远离当前页的缓存被逐出，重读重新拉取", async () => {
  const { model, calls } = makeFakeEngine({ rows: 20 * BIG_PAGE, cols: 1 });
  await model.open("/x.csv");
  model.prefetchRange(0, 0);
  await tick();
  assert.equal(model.displayData(0, 0), "r0c0");
  const readsBefore = calls.filter(([c]) => c === "big_rows").length;
  // 跳到很远的地方并触发多页拉取，把 page0 挤出 ±2 窗口
  model.prefetchRange(BIG_PAGE * 10, BIG_PAGE * 14);
  await tick();
  model.prefetchRange(BIG_PAGE * 10, BIG_PAGE * 10);
  await tick();
  model.prefetchRange(0, 0);
  await tick();
  const readsAfter = calls.filter(([c]) => c === "big_rows").length;
  assert.ok(readsAfter > readsBefore, "页 0 被逐出后应重新拉取");
});

test("编辑乐观写立即可读，引擎同步落定", async () => {
  const { model, data } = makeFakeEngine({ rows: 100, cols: 2 });
  await model.open("/x.csv");
  model.prefetchRange(0, 0);
  await tick();
  await model.setData(5, 1, "hello");
  assert.equal(model.displayData(5, 1), "hello"); // 缓存乐观值
  assert.equal(data[5][1], "hello"); // 引擎也已写
});

test("结构变化后旧页响应作废（代际机制）", async () => {
  const { model } = makeFakeEngine({ rows: 100, cols: 1 });
  await model.open("/x.csv");
  // 触发第 0 页拉取，但立刻插入一行使其作废
  model.prefetchRange(0, 0);
  const p = model.insertRows(0, 1);
  await p;
  await tick();
  await tick();
  // 第 0 页应反映插入（r0 现在是空行），而不是旧响应 r0c0
  model.prefetchRange(0, 0);
  await tick();
  assert.equal(model.displayData(1, 0), "r0c0");
});

test("removeRows 排序后传引擎（降序删除是引擎职责，入参升序）", async () => {
  const { model, calls } = makeFakeEngine({ rows: 50, cols: 1 });
  await model.open("/x.csv");
  await model.removeRows([9, 3, 5]);
  const call = calls.find(([c]) => c === "big_remove_rows");
  assert.deepEqual(call[1].vrows, [3, 5, 9]);
  assert.equal(model.rowCount(), 47);
});

test("筛选：runFilter 切结果视图，rowCount=命中数，clearFilter 还原", async () => {
  const { model, data } = makeFakeEngine({ rows: 200, cols: 1 });
  await model.open("/x.csv");
  data[7][0] = "needle";
  data[150][0] = "NEEDLE-x"; // 大小写不敏感（引擎小写匹配；mock 直接 includes）
  const n = await model.runFilter([[0, "needle"]], [], "");
  assert.equal(n, 2);
  assert.equal(model.rowCount(), 2);
  assert.equal(model.filterActive(), true);
  model.prefetchRange(0, 1);
  await tick();
  assert.equal(model.displayData(0, 0), "needle");
  assert.equal(model.displayData(1, 0), "NEEDLE-x");
  await model.clearFilter();
  assert.equal(model.filterActive(), false);
  assert.equal(model.rowCount(), 200);
});

test("筛选结果中禁止编辑/删除", async () => {
  const { model } = makeFakeEngine({ rows: 50, cols: 1 });
  await model.open("/x.csv");
  await model.runFilter([[0, "r1"]], [], "");
  await assert.rejects(() => model.setData(0, 0, "x"), /清除筛选/);
  await assert.rejects(() => model.removeRows([0]), /清除筛选/);
});

test("列操作 pending 后元数据刷新由引擎视图映射（mock 不映射，验证调用面）", async () => {
  const { model, calls } = makeFakeEngine({ rows: 10, cols: 2 });
  await model.open("/x.csv");
  await model.appendColumn("新列");
  const push = calls.find(([c]) => c === "big_col_push");
  assert.deepEqual(push[1].op, { Insert: { pos: 2, name: "新列" } });
  await model.removeColumns([0, 1]);
  const pushes = calls.filter(([c]) => c === "big_col_push").slice(1);
  // 降序 push：先删 col1 再删 col0，避免 pending 位置互相位移
  assert.deepEqual(pushes[0][1].op, { Delete: { col: 1 } });
  assert.deepEqual(pushes[1][1].op, { Delete: { col: 0 } });
  await model.renameColumn(0, "改名");
  assert.deepEqual(calls.filter(([c]) => c === "big_col_push").at(-1)[1].op, {
    Rename: { col: 0, name: "改名" },
  });
  await model.applyColOps();
  assert.ok(calls.some(([c]) => c === "big_col_apply"));
});

test("clearCells 去重后逐格写空", async () => {
  const { model, data, calls } = makeFakeEngine({ rows: 10, cols: 3 });
  await model.open("/x.csv");
  data[2] = ["a", "b", "c"];
  await model.clearCells([
    [2, 0],
    [2, 1],
    [2, 0], // 重复
    [2, 2],
  ]);
  assert.deepEqual(data[2], ["", "", ""]);
  const sets = calls.filter(([c]) => c === "big_set_cell");
  assert.equal(sets.length, 3); // 去重后 3 次
});

test("不支持的能力抛中文错误（app 层禁用入口的保险）", async () => {
  const { model } = makeFakeEngine({});
  await model.open("/x.csv");
  assert.throws(() => model.sortRows(), /不支持排序/);
  assert.throws(() => model.allRows(), /不支持全量读取/);
});

test("编辑失败回滚缓存并抛错", async () => {
  const { model } = makeFakeEngine({ rows: 10, cols: 1 });
  await model.open("/x.csv");
  model.prefetchRange(0, 0);
  await tick();
  // 引擎侧制造失败：直接让后续 set_cell 抛错（monkey-patch invoke）
  const orig = model._invoke;
  model._invoke = async (cmd, args) => {
    if (cmd === "big_set_cell") throw new Error("列操作待执行：请先执行或撤销列操作");
    return orig(cmd, args);
  };
  await assert.rejects(() => model.setData(0, 0, "x"), /大文件编辑失败/);
  assert.equal(model.displayData(0, 0), "r0c0"); // 缓存已回滚
});
