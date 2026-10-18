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
    // 渐进索引字段：普通夹具视为"已索引完成"
    indexing: false,
    indexed_bytes: 1024,
    total_bytes: 1024,
    open_ms: 3,
    index_ms: 3,
    elapsed_ms: 3,
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
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
/** 轮询等待条件成真（渐进索引用短周期计时器，等待需真实时间流逝）。 */
async function waitFor(fn, what, timeout = 1000) {
  const t0 = performance.now();
  for (;;) {
    if (await fn()) return;
    if (performance.now() - t0 > timeout) throw new Error(`等待超时: ${what}`);
    await sleep(5);
  }
}

/**
 * 造一个"渐进索引"引擎：big_open 只返回前 openRows 行，big_meta 按 steps
 * 依次推进（steps 用尽后返回全量完成态）。big_rows 只返回已索引行——
 * 与真实引擎"未扫描到的记录不可见"一致。
 *
 * @param {{rows?: number, cols?: number, openRows?: number,
 *   steps?: object[], pollMs?: number}} [opts] - 夹具参数。
 * @returns {{model: BigModel, data: string[][], calls: Array, meta: () => object}}
 */
function makeIndexingEngine({ rows = 100, cols = 2, openRows = 20, steps = [], pollMs = 5 } = {}) {
  const data = Array.from({ length: rows }, (_, r) =>
    Array.from({ length: cols }, (_, c) => `r${r}c${c}`),
  );
  const calls = [];
  const headers = Array.from({ length: cols }, (_, c) => `h${c}`);
  let meta = {
    rows: openRows, cols, headers, encoding: "utf-8",
    pending_cols: false, can_undo: false, can_redo: false,
    indexing: true, indexed_bytes: 200, total_bytes: 1024,
    open_ms: 7, index_ms: 0, elapsed_ms: 30,
  };
  let step = 0;
  const model = new BigModel({
    pollMs,
    invoke: async (cmd, args = {}) => {
      calls.push([cmd, args]);
      switch (cmd) {
        case "big_open":
          return { ...meta };
        case "big_meta":
          if (step < steps.length) {
            meta = { ...meta, ...steps[step] };
            step += 1;
          } else if (meta.indexing) {
            meta = { ...meta, rows, indexing: false, index_ms: 1234, elapsed_ms: 1300 };
          }
          return { ...meta };
        case "big_rows": {
          const from = Number(args.from);
          return data.slice(from, Math.min(from + Number(args.count), meta.rows));
        }
        case "big_set_cell": {
          const r = Number(args.vrow);
          if (r >= meta.rows) throw new Error("行号越界");
          data[r][Number(args.col)] = args.value;
          return null;
        }
        case "big_filter":
          return 0;
        case "big_clear_filter":
          return null;
        default:
          return null;
      }
    },
    listen: async () => () => {},
  });
  return { model, data, calls, meta: () => meta };
}

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

test("hasData：仅页已缓存且该行存在时为真（grid 键控复用判据）", async () => {
  const { model } = makeFakeEngine({ rows: 500, cols: 2 });
  await model.open("/x.csv");
  assert.equal(model.hasData(10, 1), false); // 未拉取
  model.prefetchRange(0, 199); // 只拉第 0 页
  await tick();
  assert.equal(model.hasData(10, 1), true);
  assert.equal(model.hasData(199, 0), true);
  assert.equal(model.hasData(200, 0), false); // 第 1 页未拉
  assert.equal(model.hasData(4999, 0), false); // 越界行同样为假
});

test("peekData：命中返回值、未命中 undefined 且不触发拉取", async () => {
  const { model } = makeFakeEngine({ rows: 500, cols: 2 });
  await model.open("/x.csv");
  assert.equal(model.peekData(10, 1), undefined); // 未命中 → undefined（窗格保留旧内容判据）
  model.prefetchRange(0, 199); // 只拉第 0 页
  await tick();
  assert.equal(model.peekData(10, 1), "r10c1");
  assert.equal(model.peekData(200, 0), undefined); // 第 1 页未拉
  assert.equal(model.peekData(4999, 0), undefined); // 越界行
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

/** 筛选响应可手动控制的模型：验证键入即筛的过期响应丢弃。 */
function makeDeferredFilterModel() {
  const pending = [];
  const meta = {
    rows: 100, cols: 1, headers: ["h"], encoding: "utf-8",
    pending_cols: false, can_undo: false, can_redo: false,
  };
  const model = new BigModel({
    invoke: (cmd) => {
      switch (cmd) {
        case "big_open":
        case "big_meta":
          return Promise.resolve(meta);
        case "big_filter":
          return new Promise((resolve) => pending.push(resolve));
        case "big_clear_filter":
          return Promise.resolve(null);
        case "big_rows":
          return Promise.resolve([]);
        default:
          return Promise.resolve(null);
      }
    },
    listen: async () => () => {},
  });
  return { model, pending };
}

test("筛选：迟到的旧请求不得覆盖新结果", async () => {
  const { model, pending } = makeDeferredFilterModel();
  await model.open("/x.csv");
  const p1 = model.runFilter([[0, "a"]], [], "");
  const p2 = model.runFilter([[0, "ab"]], [], "");
  pending[1](5); // 新请求先返回
  assert.equal(await p2, 5);
  assert.equal(model.rowCount(), 5);
  pending[0](9); // 旧请求迟到
  assert.equal(await p1, null);
  assert.equal(model.rowCount(), 5, "旧结果不得覆盖新命中数");
  assert.equal(model.filterActive(), true);
});

test("筛选：引擎中断返回 null 时保持原视图", async () => {
  const { model, pending } = makeDeferredFilterModel();
  await model.open("/x.csv");
  const p = model.runFilter([[0, "a"]], [], "");
  pending[0](null); // 引擎放弃本次扫描
  assert.equal(await p, null);
  assert.equal(model.filterActive(), false);
  assert.equal(model.rowCount(), 100);
});

test("筛选：清空后迟到的筛选结果不得让视图切回命中态", async () => {
  const { model, pending } = makeDeferredFilterModel();
  await model.open("/x.csv");
  const p = model.runFilter([[0, "a"]], [], "");
  await model.clearFilter();
  pending[0](7); // 清空后才到达的旧结果
  assert.equal(await p, null);
  assert.equal(model.filterActive(), false);
  assert.equal(model.rowCount(), 100);
});

// ---- 渐进索引（大文件秒开：首批可用 → 后台索引 → 行数增长） ----

test("渐进索引：打开只含首批行，轮询增长并在完成后停止", async () => {
  const { model, calls } = makeIndexingEngine({
    rows: 100,
    openRows: 20,
    steps: [
      { rows: 50, indexed_bytes: 500, elapsed_ms: 80 },
      { rows: 100, indexed_bytes: 1024, elapsed_ms: 120 },
    ],
  });
  const progress = [];
  model.onIndexProgress((meta) => progress.push(meta.rows));
  await model.open("/x.csv");
  assert.equal(model.indexing(), true);
  assert.equal(model.rowCount(), 20);
  assert.equal(model.meta().total_bytes, 1024);
  assert.equal(model.meta().indexed_bytes, 200);
  await waitFor(() => !model.indexing(), "索引完成");
  assert.equal(model.rowCount(), 100);
  assert.equal(model.meta().index_ms, 1234);
  assert.ok(progress.length >= 2, `进度事件应至少两次，实际 ${progress.length}`);
  assert.equal(progress.at(-1), 100);
  // 完成后必须停止轮询（否则空转 IPC）
  const polls = calls.filter(([c]) => c === "big_meta").length;
  await sleep(30);
  assert.equal(calls.filter(([c]) => c === "big_meta").length, polls, "完成后不应继续轮询");
});

test("渐进索引：索引中拒绝结构/筛选/保存/列操作，单元格编辑允许", async () => {
  const { model, calls } = makeIndexingEngine({ rows: 100, openRows: 20 });
  await model.open("/x.csv");
  await assert.rejects(() => model.insertRows(0, 1), /索引中/);
  await assert.rejects(() => model.removeRows([0]), /索引中/);
  await assert.rejects(() => model.runFilter([[0, "r1"]], [], ""), /索引中/);
  await assert.rejects(() => model.save(), /索引中/);
  await assert.rejects(() => model.appendColumn("新列"), /索引中/);
  assert.ok(!calls.some(([c]) => c === "big_insert_rows"), "不应把插入行发给引擎");
  assert.ok(!calls.some(([c]) => c === "big_remove_rows"), "不应把删除行发给引擎");
  assert.ok(!calls.some(([c]) => c === "big_filter"), "不应把筛选发给引擎");
  // 单元格编辑与撤销照常（覆盖层独立于索引）
  await model.setData(0, 0, "改");
  assert.equal(model.displayData(0, 0), "改");
});

test("渐进索引：行数增长后部分页失效并按新行数重拉", async () => {
  const { model, calls } = makeIndexingEngine({
    rows: 100,
    openRows: 30,
    steps: [{ rows: 100 }],
  });
  await model.open("/x.csv");
  model.prefetchRange(0, 0);
  await tick();
  // 首拉只拿到已索引的 30 行：页 0 是"部分页"
  assert.equal(model.displayData(0, 0), "r0c0");
  assert.equal(model.hasData(29, 0), true);
  assert.equal(model.hasData(40, 0), false);
  await waitFor(() => model.rowCount() === 100, "行数增长");
  // 增长后部分页被丢弃：读 40 行调度重拉，未落定前为空串占位
  assert.equal(model.displayData(40, 0), "");
  await tick();
  assert.equal(model.displayData(40, 0), "r40c0");
  const pageReads = calls.filter(([c]) => c === "big_rows").length;
  assert.ok(pageReads >= 2, `部分页应重拉（实际 ${pageReads} 次取页）`);
});

test("渐进索引：完整页不因行数增长被无谓重拉", async () => {
  const { model, calls } = makeIndexingEngine({ rows: 400, openRows: 300 });
  await model.open("/x.csv");
  model.prefetchRange(0, 0);
  await tick();
  const before = calls.filter(([c]) => c === "big_rows").length;
  await waitFor(() => model.rowCount() === 400, "索引完成");
  model.prefetchRange(0, 0);
  await tick();
  assert.equal(
    calls.filter(([c]) => c === "big_rows").length, before,
    "页 0 在首批 300 行内完整，增长后不应重拉");
  assert.equal(model.displayData(10, 0), "r10c0");
});

test("首屏耗时由第一次 markFirstPaint 锁定", async () => {
  const { model } = makeFakeEngine({ rows: 10, cols: 1 });
  await model.open("/x.csv");
  await sleep(10);
  model.markFirstPaint();
  const first = model.firstPaintMs();
  assert.ok(first > 0, `首屏耗时应为正数，实际 ${first}`);
  await sleep(10);
  model.markFirstPaint();
  assert.equal(model.firstPaintMs(), first, "多次标记只认第一次");
});

test("重新打开会重置首屏耗时与索引轮询", async () => {
  const { model, calls } = makeIndexingEngine({ rows: 100, openRows: 20 });
  await model.open("/x.csv");
  assert.equal(model.indexing(), true);
  await model.open("/y.csv");
  assert.equal(model.indexing(), true);
  assert.equal(model.firstPaintMs(), 0, "新会话首屏耗时归零");
  const metaCalls = calls.filter(([c]) => c === "big_meta").length;
  await sleep(20);
  // 旧会话轮询不得叠加（每次 tick 只应有一次 big_meta）
  const ticks = calls.filter(([c]) => c === "big_meta").length - metaCalls;
  assert.ok(ticks <= 4, `旧轮询不得叠加（观察窗口内 ${ticks} 次 big_meta）`);
});
