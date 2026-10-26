import { BigModel } from "../../frontend/js/bigmodel.js";
import { findVerticalEdge } from "../../frontend/js/nav.js";

function editOutcome({ changed = false, insertedRows = 0, removedRows = 0 } = {}) {
  return { changed, insertedRows, removedRows, fixedPoint: null, cancelled: false };
}

/** 造一个内存版"引擎"：N 行 M 列的顺序数据 + 记录 invoke 调用。 */
function createEngine({ rows = 1000, cols = 3, data: initial = null } = {}, calls) {
  let data = initial
    ? initial.map((r) => r.slice())
    : Array.from({ length: rows }, (_, r) =>
      Array.from({ length: cols }, (_, c) => `r${r}c${c}`),
    );
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
    can_redo: false, modified: false,
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
  const invoke = async (cmd, args = {}) => {
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
          const changed = on !== headerless;
          let insertedRows = 0;
          let removedRows = 0;
          if (changed) {
            if (on) {
              data.unshift(baseHeaders.slice());
              insertedRows = 1;
            } else {
              if (!data.length) throw new Error("没有数据行可作为列名");
              baseHeaders = data.shift().slice();
              removedRows = 1;
            }
            headerless = on;
            filter = null;
          }
          return editOutcome({ changed, insertedRows, removedRows });
        }
        case "big_set_undo_budget":
        case "big_meta":
          return meta();
        case "big_rows": {
          const from = Number(args.from);
          return data.slice(from, from + Number(args.count)).map((row) => [...row]);
        }
        case "big_clear": {
          const target = args.target;
          let cells = [];
          if (target.kind === "cells") {
            cells = target.cells;
          } else if (target.kind === "rows") {
            for (const r of new Set(target.rows)) {
              for (let c = 0; c < ncols; c += 1) cells.push([r, c]);
            }
          } else if (target.kind === "columns") {
            for (let r = 0; r < data.length; r += 1) {
              for (const c of new Set(target.cols)) cells.push([r, c]);
            }
          } else if (target.kind === "rect") {
            for (let r = target.row; r < target.row + target.rows; r += 1) {
              for (let c = target.col; c < target.col + target.cols; c += 1) {
                cells.push([r, c]);
              }
            }
          }
          let changed = false;
          for (const [rawR, rawC] of cells) {
            const r = Number(rawR);
            const c = Number(rawC);
            if (!data[r] || c >= ncols) throw new Error("目标单元格越界");
            if ((data[r][c] ?? "") !== "") {
              data[r][c] = "";
              changed = true;
            }
          }
          return editOutcome({ changed });
        }
        case "big_set_cells": {
          const cells = args.cells.map(([r, c, value]) => [Number(r), Number(c), value]);
          for (const [r, c] of cells) {
            if (!data[r] || c >= ncols) throw new Error("目标单元格越界");
          }
          let changed = false;
          for (const [r, c, value] of cells) {
            if ((data[r][c] ?? "") !== value) {
              data[r][c] = value;
              changed = true;
            }
          }
          return editOutcome({ changed });
        }
        case "big_paste": {
          const start = Number(args.vrow);
          const col = Number(args.col);
          const input = args.rows.map((row) => row.slice());
          const width = Math.max(0, ...input.map((row) => row.length));
          if (start > data.length || col + width > ncols) throw new Error("粘贴位置越界");
          const existing = Math.min(input.length, data.length - start);
          let changed = false;
          for (let i = 0; i < existing; i += 1) {
            for (let c = 0; c < input[i].length; c += 1) {
              if ((data[start + i][col + c] ?? "") !== input[i][c]) {
                data[start + i][col + c] = input[i][c];
                changed = true;
              }
            }
          }
          const appended = input.slice(existing).map((row) =>
            Array.from({ length: ncols }, (_, c) => row[c] ?? ""));
          if (appended.length) {
            data.push(...appended);
            changed = true;
          }
          return editOutcome({ changed, insertedRows: appended.length });
        }
        case "big_set_cell": {
          const r = Number(args.vrow);
          const c = Number(args.col);
          if (r >= data.length) throw new Error("行号越界");
          const changed = (data[r][c] ?? "") !== args.value;
          data[r][c] = args.value;
          return editOutcome({ changed });
        }
        case "big_insert_rows": {
          const at = Math.min(Number(args.vpos), data.length);
          const n = args.rows ? args.rows.length : Number(args.count);
          const blank = Array.from({ length: ncols }, () => "");
          for (let i = 0; i < n; i += 1) data.splice(at + i, 0, args.rows ? [...args.rows[i]] : [...blank]);
          return editOutcome({ changed: n > 0, insertedRows: n });
        }
        case "big_remove_rows": {
          for (const v of [...args.vrows].sort((a, b) => b - a)) data.splice(Number(v), 1);
          return editOutcome({ changed: args.vrows.length > 0, removedRows: args.vrows.length });
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
          return editOutcome({ changed: true });
        case "big_save":
        case "big_save_as":
          // 真实引擎保存/另存为后重开文件：筛选结果被丢弃、排序视图已物理落盘
          filter = null;
          sorted = false;
          return { saved: true, fixedPoint: null, cancelled: false };
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
          return editOutcome({ changed: true });
        }
        case "big_sort_exit":
          sorted = false;
          if (args.releaseRam !== false) ramBacked = false;
          return editOutcome({ changed: true });
        case "big_close":
          return null;
        case "big_stale":
          return false;
        default:
          throw new Error(`unexpected cmd ${cmd}`);
      }
  };
  return { invoke, meta, getData: () => data };
}


/** 共享进程会话表；每个 model 使用独立数据，owner 由 invokeFor 注入。 */
export function makeSharedBackend(options = {}) {
  const calls = [];
  const sessions = new Map();
  const engines = new WeakMap();
  let nextSession = 1;

  function authorize(entry, owner, lease) {
    if (!entry) throw new Error("大文件会话未打开");
    if (entry.owner !== owner) throw new Error("窗口不是文档 owner");
    if (entry.lease !== lease) throw new Error("文档 lease 已过期");
    return entry;
  }

  function invokeFor(owner, initialEngine = null) {
    return async (cmd, args = {}) => {
      if (cmd === "big_open" || cmd === "big_new") {
        if (Object.hasOwn(args, "session") || Object.hasOwn(args, "lease")) {
          throw new Error(`${cmd} 不接受 JS session/lease`);
        }
        const replaceSession = args.replaceSession;
        let replaced = null;
        if (replaceSession !== null && replaceSession !== undefined) {
          replaced = authorize(
            sessions.get(Number(replaceSession)),
            owner,
            args.replaceLease,
          );
        }
        const engine = initialEngine ?? createEngine(options, calls);
        const meta = await engine.invoke(cmd, args);
        const session = nextSession;
        nextSession += 1;
        const entry = { engine, owner, lease: 1, history: [] };
        if (replaced) sessions.delete(replaceSession);
        sessions.set(session, entry);
        return { session, lease: 1, meta };
      }
      const entry = authorize(sessions.get(Number(args.session)), owner, args.lease);
      const result = await entry.engine.invoke(cmd, args);
      if (cmd === "big_close") sessions.delete(args.session);
      return result;
    };
  }

  function model() {
    const engine = createEngine(options, calls);
    const instance = new BigModel({
      invoke: invokeFor("main", engine),
      listen: async () => () => {},
    });
    engines.set(instance, engine);
    return instance;
  }

  function snapshot(session) {
    const entry = sessions.get(Number(session));
    if (!entry) return null;
    return {
      data: entry.engine.getData(),
      owner: entry.owner,
      lease: entry.lease,
      meta: entry.engine.meta(),
      history: entry.history,
    };
  }

  return {
    calls,
    sessions,
    model,
    invokeFor,
    snapshot,
    dataFor(instance) {
      return instance && engines.get(instance)?.getData?.();
    },
  };
}

/** 兼容既有 BigModel 测试的共享 backend 工厂。 */
export function makeFakeEngine(options = {}) {
  const backend = makeSharedBackend(options);
  const instance = backend.model();
  return {
    model: instance,
    data: backend.dataFor(instance),
    calls: backend.calls,
    backend,
  };
}
