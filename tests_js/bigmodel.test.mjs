// bigmodel.js 单元测试：窗口缓存/乐观编辑/筛选切换/列操作 pending。
// invoke/listen 全部注入 mock，脱离 Tauri 环境（node --test 直跑）。
import assert from "node:assert/strict";
import { test } from "node:test";

import { BigModel, BIG_PAGE, BIG_THRESHOLD } from "../frontend/js/bigmodel.js";
import { findVerticalEdge } from "../frontend/js/nav.js";

/** 造一个内存版"引擎"：N 行 M 列的顺序数据 + 记录 invoke 调用。 */
function makeFakeEngine({ rows = 1000, cols = 3, data: initial = null } = {}) {
  let data = initial
    ? initial.map((r) => r.slice())
    : Array.from({ length: rows }, (_, r) =>
      Array.from({ length: cols }, (_, c) => `r${r}c${c}`),
    );
  const calls = [];
  let headerless = false;
  let ramBacked = false;
  let hasPath = false; // big_open 置 true、big_new 置 false
  let sorted = false;
  let ncols = cols; // big_new 后随文本重估列数
  let baseHeaders = Array.from({ length: cols }, (_, c) => `h${c}`);
  const generic = () => Array.from({ length: ncols }, (_, c) => `列${c + 1}`);
  const meta = () => ({
    rows: data.length,
    cols: ncols,
    headers: headerless ? generic() : baseHeaders.slice(),
    encoding: "utf-8",
    pending_cols: false,
    can_undo: false,
    can_redo: false,
    headerless,
    // 渐进索引字段：普通夹具视为"已索引完成"
    indexing: false,
    ram_backed: ramBacked,
    has_path: hasPath,
    sorted,
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
          headerless = !!args.headerless;
          hasPath = true;
          return meta();
        case "big_new": {
          // 对齐引擎 from_bytes：按行切分、跳过空白行；空文本同语义拒绝
          const recs = String(args.text).split(/\r\n|\n/)
            .filter((l) => l.length > 0)
            .map((l) => l.split(",").map((c) => (c === '""' ? "" : c)));
          if (!recs.length) throw new Error("空文件");
          headerless = !!args.headerless;
          if (headerless) {
            data = recs;
          } else {
            baseHeaders = recs[0].slice();
            data = recs.slice(1);
          }
          ncols = recs[0].length;
          ramBacked = true;
          hasPath = false;
          filter = null;
          return meta();
        }
        case "big_set_headerless": {
          const on = !!args.on;
          if (on !== headerless) {
            if (on) {
              data.unshift(baseHeaders.slice());
            } else {
              if (!data.length) throw new Error("没有数据行可作为列名");
              baseHeaders = data.shift().slice();
            }
            headerless = on;
          }
          filter = null;
          return meta();
        }
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
          const blank = Array.from({ length: ncols }, () => "");
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
          const hits = (filter ?? []).slice(from, from + Number(args.count));
          // 对齐引擎 FilterPage：行内容与各命中行的原始行号（0 基）成对下发
          return { rows: hits.map((r) => data[r]), vrows: hits };
        }
        case "big_filter_hit_view_row": {
          const hit = (filter ?? [])[Number(args.idx)];
          return hit === undefined ? null : hit;
        }
        case "big_find_edge": {
          const list = args.filtered ? (filter ?? []) : data.map((_, i) => i);
          if (!list.length) return null;
          const col = Number(args.col);
          const nonEmpty = (i) => String(data[list[i]]?.[col] ?? "") !== "";
          const edge = findVerticalEdge(nonEmpty, Number(args.from), list.length, !!args.down);
          return edge < 0 ? null : list[edge];
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
        case "big_save_as":
          // 真实引擎保存/另存为后重开文件：筛选结果被丢弃、排序视图已物理落盘
          filter = null;
          sorted = false;
          return null;
        case "big_sort_estimate":
          return {
            file_bytes: 1024,
            row_count: data.length,
            new_bytes: 4096,
            peak_bytes: 8192,
            available_bytes: 1 << 30,
          };
        case "big_sort_load":
          ramBacked = true;
          return true;
        case "big_sort_cancel":
          return null;
        case "big_sort_apply": {
          const levels = args.levels || [];
          const val = (r, l) => {
            const text = String(data[r][Number(l.col)] ?? "");
            if (!l.numeric) return text;
            const n = Number(text.trim());
            return text.trim() === "" || !Number.isFinite(n) ? null : n;
          };
          const idx = data.map((_, i) => i);
          idx.sort((a, b) => {
            for (const l of levels) {
              const x = val(a, l);
              const y = val(b, l);
              if (l.numeric) {
                if (x === null && y !== null) return 1;
                if (y === null && x !== null) return -1;
                if (x === null) continue;
                if (x !== y) return l.asc ? x - y : y - x;
              } else if (x !== y) {
                return l.asc ? (x < y ? -1 : 1) : (x > y ? -1 : 1);
              }
            }
            return a - b;
          });
          data.splice(0, data.length, ...idx.map((i) => data[i]));
          sorted = true;
          return meta();
        }
        case "big_sort_exit":
          sorted = false;
          if (args.releaseRam !== false) ramBacked = false;
          return meta();
        case "big_close":
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
    pending_cols: false, can_undo: false, can_redo: false, headerless: false,
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
        case "big_set_headerless":
          meta = { ...meta, headerless: !!args.on };
          return { ...meta };
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

test("每个模型持有独立会话号，所有 big_* 调用都携带它", async () => {
  const a = makeFakeEngine({ rows: 3, cols: 1 });
  const b = makeFakeEngine({ rows: 4, cols: 1 });
  await a.model.open("/a.csv");
  await b.model.open("/b.csv");
  const sidA = a.calls[0][1].session;
  const sidB = b.calls[0][1].session;
  assert.equal(typeof sidA, "number");
  assert.equal(typeof sidB, "number");
  assert.notEqual(sidA, sidB, "两个模型不应共享会话号");

  // 任意后续命令（取数/筛选/保存/关闭）都必须带上本模型会话号
  a.model.prefetchRange(0, 0);
  await tick();
  await a.model.runFilter([[0, "r"]], [], "");
  await a.model.close();
  const later = a.calls.slice(1);
  assert.ok(later.length >= 3);
  for (const [cmd, args] of later) {
    assert.equal(args.session, sidA, `${cmd} 应携带会话号`);
  }
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

test("筛选：runFilter 载荷携带三态 mode；mode2 关键字保持原样大小写", async () => {
  const { model, calls } = makeFakeEngine({ rows: 10, cols: 3 });
  await model.open("/x.csv");
  await model.runFilter([[0, "AbC", 2], [1, "dEf", 1], [2, "gHi", 0]], [], "");
  const call = calls.find(([c]) => c === "big_filter");
  assert.deepEqual(call[1].spec.cols, [
    [0, "AbC", 2], // mode 2：保持原样大小写（区分大小写比较）
    [1, "def", 1], // mode 0/1：关键字小写化（引擎侧折叠匹配）
    [2, "ghi", 0],
  ]);
});

test("筛选：runFilter 兼容旧二元组调用（mode 缺省按 0 处理）", async () => {
  const { model, calls } = makeFakeEngine({ rows: 10, cols: 1 });
  await model.open("/x.csv");
  await model.runFilter([[0, "Needle"]], [], "");
  const call = calls.find(([c]) => c === "big_filter");
  assert.deepEqual(call[1].spec.cols, [[0, "needle", 0]]);
});

test("save 后筛选态复位：引擎重开文件后视图回到全量", async () => {
  const { model, data } = makeFakeEngine({ rows: 200, cols: 1 });
  await model.open("/x.csv");
  data[7][0] = "needle";
  await model.runFilter([[0, "needle"]], [], "");
  assert.equal(model.rowCount(), 1);
  const events = [];
  model.onData((structural, origin) => events.push([structural, origin]));
  await model.save();
  assert.equal(model.filterActive(), false, "保存后不应再声称筛选视图");
  assert.equal(model.rowCount(), 200, "rowCount 应回到全量行数");
  assert.deepEqual(events.at(-1), [true, "view"],
    "保存后应发结构通知（origin=view，不额外标脏）以重建全量视图");
});

test("saveAs 把目标路径交给引擎，并按保存语义复位视图", async () => {
  const { model, data, calls } = makeFakeEngine({ rows: 200, cols: 1 });
  await model.open("/x.csv");
  data[7][0] = "needle";
  await model.runFilter([[0, "needle"]], [], "");
  assert.equal(model.rowCount(), 1);
  const events = [];
  model.onData((structural, origin) => events.push([structural, origin]));

  await model.saveAs("/out/new.csv");

  const call = calls.find(([cmd]) => cmd === "big_save_as");
  assert.ok(call, "另存为应调用 big_save_as");
  assert.equal(call[1].path, "/out/new.csv", "目标路径必须交给引擎");
  assert.equal(typeof call[1].session, "number", "命令应携带会话号");
  assert.equal(calls.some(([cmd]) => cmd === "big_save"), false,
    "另存为不得退化为写回原文件的 big_save");
  assert.equal(model.filterActive(), false);
  assert.equal(model.rowCount(), 200);
  assert.deepEqual(events.at(-1), [true, "view"],
    "另存为后应按 origin=view 重建全量视图");
});

test("筛选：rowNumber 三态——命中给原始行号，未缓存/非筛选给 null", async () => {
  const { model, data } = makeFakeEngine({ rows: 200, cols: 1 });
  await model.open("/x.csv");
  assert.equal(model.rowNumber(0), null, "非筛选态恒 null");
  data[7][0] = "needle";
  data[150][0] = "needle-x";
  await model.runFilter([[0, "needle"]], [], "");
  model.prefetchRange(0, 1);
  await tick();
  assert.equal(model.rowNumber(0), 7, "命中序号 0 → 原始行号 7");
  assert.equal(model.rowNumber(1), 150, "命中序号 1 → 原始行号 150");
  assert.equal(model.rowNumber(500), null, "未缓存页返回 null（纯查缓存，不触发拉取）");
});

test("筛选：clearFilter(hitRow) 先查归位行号再清筛，返回锚定目标", async () => {
  const { model, data, calls } = makeFakeEngine({ rows: 200, cols: 1 });
  await model.open("/x.csv");
  data[7][0] = "needle";
  data[150][0] = "needle-x";
  await model.runFilter([[0, "needle"]], [], "");
  const res = await model.clearFilter(1); // 命中序号 1 → 原始行号 150
  assert.deepEqual(res, { applied: true, target: 150 });
  const names = calls.map(([c]) => c);
  const iQ = names.lastIndexOf("big_filter_hit_view_row");
  const iC = names.lastIndexOf("big_clear_filter");
  assert.ok(iQ >= 0 && iC > iQ,
    "必须先查询归位行号、后清筛（清后命中集即失，顺序反了锚点丢失）");
  assert.equal(model.filterActive(), false);
});

test("筛选：clearFilter 链被新筛选抢断 → applied:false 且不再发清筛", async () => {
  const { model, data, calls } = makeFakeEngine({ rows: 50, cols: 1 });
  await model.open("/x.csv");
  data[7][0] = "needle";
  await model.runFilter([[0, "needle"]], [], "");
  const p = model.clearFilter(0);
  // runFilter 同步段即翻转 _filterGen：clearFilter 第一次 await 返回时发现已过期
  await model.runFilter([[0, "needle"]], [], "");
  const res = await p;
  assert.equal(res.applied, false, "被抢断的清筛不得声称已应用");
  assert.equal(calls.some(([c]) => c === "big_clear_filter"), false,
    "抢断后不应再发 big_clear_filter（新筛选才是现状）");
  assert.equal(model.filterActive(), true);
});

test("筛选：clearFilter 后 rowNumber 回 null（缓存随 _invalidate 清空）", async () => {
  const { model, data } = makeFakeEngine({ rows: 200, cols: 1 });
  await model.open("/x.csv");
  data[7][0] = "needle";
  await model.runFilter([[0, "needle"]], [], "");
  model.prefetchRange(0, 0);
  await tick();
  assert.equal(model.rowNumber(0), 7);
  await model.clearFilter();
  assert.equal(model.rowNumber(0), null, "非筛选态恒 null");
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

test("open 把 headerless 传给 big_open，缺省为 false", async () => {
  const { model, calls } = makeFakeEngine();
  await model.open("/x.csv");
  assert.equal(calls[0][0], "big_open");
  assert.equal(calls[0][1].path, "/x.csv");
  assert.equal(calls[0][1].headerless, false);
  assert.equal(typeof calls[0][1].session, "number");
  assert.equal(model.isHeaderless(), false);
});

test("open 无列名：isHeaderless=true、表头为 列1..N、首行计入数据", async () => {
  const { model } = makeFakeEngine({ rows: 5, cols: 2 });
  await model.open("/x.csv", { headerless: true });
  assert.equal(model.isHeaderless(), true);
  assert.deepEqual(model.headers(), ["列1", "列2"]);
  assert.equal(model.rowCount(), 5);
});

test("meta 缺 headerless 字段时按 false 兼容", async () => {
  const model = new BigModel({
    invoke: async (cmd) =>
      cmd === "big_open" ? { rows: 0, cols: 0, headers: [], indexing: false } : null,
    listen: async () => () => {},
  });
  await model.open("/x.csv");
  assert.equal(model.isHeaderless(), false);
});

test("setHeaderless 调引擎并更新元数据/表头/行数", async () => {
  const { model, calls } = makeFakeEngine({ rows: 3, cols: 2 });
  await model.open("/x.csv");
  await model.setHeaderless(true);
  assert.equal(calls.at(-1)[0], "big_set_headerless");
  assert.equal(calls.at(-1)[1].on, true);
  assert.equal(typeof calls.at(-1)[1].session, "number");
  assert.equal(model.isHeaderless(), true);
  assert.deepEqual(model.headers(), ["列1", "列2"]);
  assert.equal(model.rowCount(), 4);
  await model.setHeaderless(false);
  assert.equal(model.isHeaderless(), false);
  assert.deepEqual(model.headers(), ["h0", "h1"]);
  assert.equal(model.rowCount(), 3);
});

test("setHeaderless 清空筛选，以 origin='view' 通知且不标脏", async () => {
  const { model } = makeFakeEngine({ rows: 100, cols: 2 });
  await model.open("/x.csv");
  await model.runFilter([[0, "r1"]], [], "");
  assert.equal(model.filterActive(), true);
  const events = [];
  let undoEvents = 0;
  model.onData((s, o) => events.push([s, o]));
  model.onUndoStack(() => { undoEvents += 1; });
  await model.setHeaderless(true);
  assert.equal(model.filterActive(), false);
  assert.deepEqual(events.at(-1), [true, "view"]);
  assert.equal(undoEvents, 1);
});

test("索引期间允许切换 headerless", async () => {
  const { model } = makeIndexingEngine({ rows: 50, openRows: 10 });
  await model.open("/x.csv");
  try {
    assert.equal(model.indexing(), true);
    await model.setHeaderless(true);
    assert.equal(model.isHeaderless(), true);
  } finally {
    await model.close();
  }
});

test("相同状态不重复调用引擎", async () => {
  const { model, calls } = makeFakeEngine();
  await model.open("/x.csv");
  const before = calls.length;
  await model.setHeaderless(false);
  assert.equal(calls.length, before);
});

test("排序模式：估算/载入/取消都带 session，载入后刷新 meta", async () => {
  const { model, calls } = makeFakeEngine();
  await model.open("/x.csv");
  const est = await model.sortEstimate(3);
  assert.equal(calls.at(-1)[0], "big_sort_estimate");
  assert.equal(calls.at(-1)[1].levels, 3);
  assert.equal(typeof calls.at(-1)[1].session, "number");
  assert.equal(est.file_bytes, 1024);
  assert.equal(est.row_count, 1000);
  assert.equal(await model.loadIntoRam(), true);
  assert.equal(calls.at(-1)[0], "big_meta", "载入后应刷新元数据");
  await model.cancelSortLoad();
  assert.equal(calls.at(-1)[0], "big_sort_cancel");
});

test("sort 调引擎带级别，刷新 meta 并以 origin='sort' 通知", async () => {
  const { model, calls } = makeFakeEngine({ rows: 5, cols: 2 });
  await model.open("/x.csv");
  const events = [];
  model.onData((s, o) => events.push([s, o]));
  const levels = [{ col: 1, numeric: false, asc: false }];
  const meta = await model.sort(levels);
  assert.equal(calls.at(-1)[0], "big_sort_apply");
  assert.equal(typeof calls.at(-1)[1].session, "number");
  assert.deepEqual(calls.at(-1)[1].levels, levels);
  assert.equal(meta.sorted, true);
  assert.equal(model.rowCount(), 5, "排序不改变行数");
  assert.deepEqual(events.at(-1), [true, "sort"], "排序应标记为需保存");
});

test("exitSort 默认释放内存并回到未排序 meta", async () => {
  const { model, calls } = makeFakeEngine();
  await model.open("/x.csv");
  await model.loadIntoRam();
  await model.sort([{ col: 0, numeric: false, asc: true }]);
  const events = [];
  model.onData((s, o) => events.push([s, o]));
  const meta = await model.exitSort();
  assert.equal(calls.at(-1)[0], "big_sort_exit");
  assert.equal(calls.at(-1)[1].releaseRam, true);
  assert.equal(typeof calls.at(-1)[1].session, "number");
  assert.equal(meta.sorted, false);
  assert.equal(meta.ram_backed, false);
  assert.deepEqual(events.at(-1), [true, "view"]);
});

test("onSortProgress 只派发本会话进度事件", async () => {
  const handlers = [];
  const model = new BigModel({
    invoke: async () => null,
    listen: async (event, cb) => {
      handlers.push([event, cb]);
      return () => {};
    },
  });
  const got = [];
  await model.onSortProgress((done, total) => got.push([done, total]));
  assert.equal(handlers[0][0], "big-sort-progress");
  handlers[0][1]({ payload: [1, 2, 999] }); // 其他会话
  handlers[0][1]({ payload: [3, 4, model.sessionId] }); // 本会话
  assert.deepEqual(got, [[3, 4]]);
});

test("无列名模式拒绝重命名列且不调用引擎", async () => {
  const { model, calls } = makeFakeEngine();
  await model.open("/x.csv", { headerless: true });
  const before = calls.length;
  await assert.rejects(() => model.renameColumn(0, "x"), /无列名/);
  assert.equal(calls.length, before);
});

test("findVerticalEdge 无筛选：连续块边缘、空档与边界", async () => {
  const { model, calls } = makeFakeEngine({
    cols: 1,
    data: [["a"], ["b"], [""], [""], ["c"], ["d"]],
  });
  await model.open("/x.csv");
  assert.equal(await model.findVerticalEdge(0, 0, true), 1, "向下到连续块末格");
  assert.equal(await model.findVerticalEdge(1, 0, true), 4, "边缘再按一次越过空档");
  assert.equal(await model.findVerticalEdge(4, 0, true), 5, "下一条块末格");
  assert.equal(await model.findVerticalEdge(5, 0, false), 4, "向上到连续块首格");
  assert.equal(await model.findVerticalEdge(4, 0, false), 1, "向上越过空档");
  assert.equal(await model.findVerticalEdge(2, 0, true), 4, "空格向下找非空");
  assert.equal(await model.findVerticalEdge(2, 0, false), 1, "空格向上找非空");
  const last = calls.filter(([cmd]) => cmd === "big_find_edge").at(-1);
  assert.equal(last[1].filtered, false);
  assert.equal(last[1].col, 0);
  assert.equal(last[1].down, false);
  assert.equal(typeof last[1].session, "number");
});

test("findVerticalEdge 筛选视图按命中行导航并传 filtered 标记", async () => {
  const { model, calls } = makeFakeEngine({
    cols: 2,
    data: [
      ["a", "keep"], ["b", "keep"], ["", "keep"], ["c", "keep"], ["d", "drop"],
    ],
  });
  await model.open("/x.csv");
  assert.equal(await model.runFilter([[1, "keep"]], [], ""), 4);
  assert.equal(await model.findVerticalEdge(0, 0, true), 1);
  assert.equal(await model.findVerticalEdge(1, 0, true), 3, "筛选视图内越过空档");
  assert.equal(await model.findVerticalEdge(3, 0, true), 3, "末条命中不越界");
  assert.equal(await model.findVerticalEdge(3, 0, false), 1);
  const last = calls.filter(([cmd]) => cmd === "big_find_edge").at(-1);
  assert.equal(last[1].filtered, true);
  assert.equal(last[1].from, 3, "from 是筛选视图行号");
});

test("findVerticalEdge 空表返回 null", async () => {
  const { model } = makeFakeEngine({ rows: 0, cols: 1 });
  await model.open("/x.csv");
  assert.equal(await model.findVerticalEdge(0, 0, true), null);
});

/**
 * 可控取页引擎：big_rows 返回手动落定的 Promise，用来模拟机械盘慢读、
 * 拖动途中请求乱序返回。其他命令同步返回。
 *
 * @param {{rows?: number, cols?: number}} [opts] - 夹具规模。
 * @returns {{model: BigModel, pending: Array<{page: number, from: number,
 *   resolve: Function}>, resolvePage: (page: number) => Promise<void>}}
 */
function makeDeferredEngine({ rows = 100 * BIG_PAGE, cols = 2 } = {}) {
  const data = Array.from({ length: rows }, (_, r) =>
    Array.from({ length: cols }, (_, c) => `r${r}c${c}`),
  );
  const meta = {
    rows, cols, headers: Array.from({ length: cols }, (_, c) => `h${c}`),
    encoding: "utf-8", pending_cols: false, can_undo: false, can_redo: false,
    headerless: false, indexing: false, indexed_bytes: 1024, total_bytes: 1024,
    open_ms: 1, index_ms: 1, elapsed_ms: 1,
  };
  const pending = [];
  const model = new BigModel({
    invoke: (cmd, args = {}) => {
      switch (cmd) {
        case "big_open":
        case "big_meta":
          return Promise.resolve({ ...meta });
        case "big_rows": {
          const from = Number(args.from);
          const count = Number(args.count);
          return new Promise((resolve) => {
            pending.push({
              page: Math.floor(from / BIG_PAGE),
              from,
              resolve: () => resolve(data.slice(from, from + count)),
            });
          });
        }
        default:
          return Promise.resolve(null);
      }
    },
    listen: async () => () => {},
  });
  /**
   * 落定某一页的请求并等一个宏任务，让 then/finally/后续调度跑完。
   *
   * @param {number} page - 页号。
   * @returns {Promise<void>}
   */
  const resolvePage = async (page) => {
    const i = pending.findIndex((p) => p.page === page);
    assert.notEqual(i, -1, `第 ${page} 页应已有在飞请求，实际 ${pending.map((p) => p.page)}`);
    const [req] = pending.splice(i, 1);
    req.resolve();
    await tick();
  };
  return { model, pending, resolvePage };
}

test("快速拖动：迟到的旧页响应不得逐出当前视口页", async () => {
  const { model, resolvePage } = makeDeferredEngine({ rows: 30 * BIG_PAGE });
  await model.open("/x.csv");
  // 拖动途中经过第 5 页：请求已发出但还没落定
  model.prefetchRange(5 * BIG_PAGE, 5 * BIG_PAGE);
  // 视口继续跳到第 10 页并先落定
  model.prefetchRange(10 * BIG_PAGE, 10 * BIG_PAGE);
  await resolvePage(10);
  assert.equal(model.hasData(10 * BIG_PAGE, 0), true);
  // 旧页迟到：换出窗口必须跟随当前视口，而不是迟到响应的页号
  await resolvePage(5);
  assert.equal(model.hasData(10 * BIG_PAGE, 0), true,
    "迟到的第 5 页响应不得把当前视口第 10 页逐出缓存");
  assert.equal(model.displayData(10 * BIG_PAGE, 0), `r${10 * BIG_PAGE}c0`);
});

test("快速拖动：排队中的旧页被丢弃，当前视口页优先发出", async () => {
  const { model, pending, resolvePage } = makeDeferredEngine({ rows: 100 * BIG_PAGE });
  await model.open("/x.csv");
  // 模拟拖动途中扫过第 0..30 页：请求不应全部同时发出
  model.prefetchRange(0, 30 * BIG_PAGE - 1);
  assert.ok(pending.length <= 3,
    `拖动途中同时发出的取页请求应受限，实际 ${pending.length} 个：${pending.map((p) => p.page)}`);
  // 视口最终跳到第 60 页：旧页排队应被清掉
  model.prefetchRange(60 * BIG_PAGE, 60 * BIG_PAGE);
  await resolvePage(0);
  await resolvePage(1);
  assert.deepEqual(pending.map((p) => p.page), [60],
    `旧页排队应被丢弃，仅保留当前视口页，实际 ${pending.map((p) => p.page)}`);
  await resolvePage(60);
  assert.equal(model.hasData(60 * BIG_PAGE, 0), true);
});

test("prefetchRange 跨页边界：视口起点在页中时也要拉下一页", async () => {
  const { model, pending } = makeDeferredEngine({ rows: 10 * BIG_PAGE });
  await model.open("/x.csv");
  // 视口从第 5 页中部开始，末尾越过分页线进入第 6 页
  const first = 5 * BIG_PAGE + 150;
  const last = first + 100;
  model.prefetchRange(first, last);
  const pages = pending.map((p) => p.page).sort((a, b) => a - b);
  assert.deepEqual(pages, [5, 6],
    `跨页视口应同时拉第 5、6 页，实际 ${pages}`);
});

test("快速拖动：被丢弃的排队页回滚视口后仍能重新拉取", async () => {
  const { model, pending, resolvePage } = makeDeferredEngine({ rows: 100 * BIG_PAGE });
  await model.open("/x.csv");
  // 拖动扫过第 0..30 页后跳到第 60 页：第 0..30 页的排队被丢弃
  model.prefetchRange(0, 30 * BIG_PAGE - 1);
  model.prefetchRange(60 * BIG_PAGE, 60 * BIG_PAGE);
  await resolvePage(0);
  await resolvePage(1);
  await resolvePage(60);
  // 再滚回第 10 页：必须重新发出取页请求（丢弃队列时若只删队列不删
  // inflight，这页会被去重逻辑永久挡住，表现为滚回去也一直空白）
  model.prefetchRange(10 * BIG_PAGE, 10 * BIG_PAGE);
  assert.ok(pending.some((p) => p.page === 10),
    `回到第 10 页应重新拉取，实际在飞 ${pending.map((p) => p.page)}`);
});

test("create：内存文本建会话（big_new 携带 text/headerless/session）", async () => {
  const { model, calls } = makeFakeEngine();
  const meta = await model.create("name,age\r\nAlice,30\r\nbob,25\r\n", { headerless: false });
  assert.equal(calls[0][0], "big_new");
  assert.equal(calls[0][1].text, "name,age\r\nAlice,30\r\nbob,25\r\n");
  assert.equal(calls[0][1].headerless, false);
  assert.equal(typeof calls[0][1].session, "number");
  assert.equal(meta.rows, 2);
  assert.equal(meta.ram_backed, true);
  assert.equal(meta.has_path, false);
  assert.deepEqual(model.headers(), ["name", "age"]);
  assert.equal(model.rowCount(), 2);
});

test("create：headerless 首记录是数据，列名虚拟化", async () => {
  const { model } = makeFakeEngine();
  await model.create("a,1\r\nb,2\r\n", { headerless: true });
  assert.equal(model.isHeaderless(), true);
  assert.equal(model.rowCount(), 2);
  assert.deepEqual(model.headers(), ["列1", "列2"]);
});

test("create：替换既有会话后行列数与缓存即刻切换", async () => {
  const { model } = makeFakeEngine({ rows: 500, cols: 2 });
  await model.open("/x.csv");
  await model.create("x,y,z\r\n1,2,3\r\n", {});
  assert.equal(model.columnCount(), 3);
  assert.equal(model.rowCount(), 1);
  assert.deepEqual(model.headers(), ["x", "y", "z"]);
  assert.equal(model.meta().has_path, false, "create 后应无磁盘路径");
});
