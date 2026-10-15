/**
 * 大文件数据模型（Big File Mode 前端门面）。
 *
 * 与 TableModel 实现同一公共接口子集（排序/即时列结构变更除外），
 * grid/app 通过它透明访问 Rust 引擎（mmap + 行索引 + 覆盖层）：
 * - 取数走 200 行/页的同步窗口缓存，±2 屏预取，落定经 onData 触发重绘；
 * - 编辑/插删/撤销立即异步落引擎，乐观写缓存，失败回滚并抛错；
 * - 筛选为按钮触发的整表扫描（引擎 rayon 并行），结果集存 Rust 侧；
 * - 列操作先进 pending 列表（视图立即映射），"执行列操作"才物理重写。
 *
 * 依赖注入：构造可传 {invoke, listen}，便于 node:test 脱离 Tauri 环境测试。
 */

/** 取页大小（行）。与引擎窗口一致；±2 屏缓存约 5 页 ≈ 1000 行。 */
export const BIG_PAGE = 200;

/** 大文件模式阈值（字节）：≥ 走大模式。依据 target/bench/bench_node.mjs：
 * 100MB 装载 ~1.5s/RSS 1.3GB、按键筛选 ~350ms；500MB V8 直接 OOM。
 * 50MiB 时筛选 ≤ ~175ms、RSS < 1GB，仍在"键入即筛"的舒适区。 */
export const BIG_THRESHOLD = 50 * 1024 * 1024;

/** 乐观编辑失败时抛出的统一错误前缀（app 层捕获转状态栏）。 */
const BIG_EDIT_ERR = "大文件编辑失败";

/**
 * 大文件模型。
 */
export class BigModel {
  /**
   * @param {{invoke: Function, listen: Function}} [io] - Tauri 桥（可注入 mock）。
   */
  constructor(io) {
    /** 大文件模式标记：grid/app 据此刻画双模式行为 */
    this.isBig = true;
    const tauri = io ?? {
      invoke: (cmd, args) => window.__TAURI__.core.invoke(cmd, args),
      listen: (ev, cb) => window.__TAURI__.event.listen(ev, cb),
    };
    /** @type {Function} */
    this._invoke = tauri.invoke;
    /** @type {Function} */
    this._listen = tauri.listen;
    /** @type {object | null} 最近一次的 big_meta 缓存 */
    this._meta = null;
    /** @type {Map<string, string[][]>} 页缓存，键 `${"f"|""}${pageIdx}` */
    this._cache = new Map();
    /** @type {Set<string>} 正在拉取的页 */
    this._inflight = new Set();
    /** @type {number} 代际号：结构变化后旧响应作废 */
    this._gen = 0;
    /** @type {boolean} 筛选结果视图 */
    this._filterOn = false;
    /** @type {number} 筛选命中数（_filterOn 时的 rowCount） */
    this._filterCount = 0;
    /** @type {Set<Function>} */
    this._dataCbs = new Set();
    /** @type {Set<Function>} */
    this._undoCbs = new Set();
    /** @type {Set<Function>} */
    this._resetCbs = new Set();
  }

  // ---- 生命周期 ----

  /**
   * 打开大文件会话（引擎 mmap + 建索引，秒开）。
   *
   * @param {string} path - 文件绝对路径。
   * @returns {Promise<object>} BigMeta。
   */
  async open(path) {
    this._meta = await this._invoke("big_open", { path });
    this._invalidate();
    this._emitReset();
    this._emitData(true);
    this._emitUndo();
    return this._meta;
  }

  /**
   * 关闭大文件会话（打开新文件/退出时）。
   *
   * @returns {Promise<void>}
   */
  async close() {
    await this._invoke("big_close");
    this._meta = null;
    this._invalidate();
  }

  /**
   * 外部程序是否改动过文件（focus 时轮询）。
   *
   * @returns {Promise<boolean>} true = 已变脏，应提示重载。
   */
  async stale() {
    return this._invoke("big_stale");
  }

  /** @returns {object} 元数据（打开后必非空）。 */
  meta() {
    return this._meta;
  }

  /** @returns {string} 编码名（utf-8 / gbk），状态栏展示。 */
  encoding() {
    return this._meta?.encoding ?? "";
  }

  // ---- 事件 ----

  /**
   * 订阅数据/结构变化。
   *
   * @param {(structural: boolean) => void} cb - 回调。
   * @returns {void}
   */
  onData(cb) {
    this._dataCbs.add(cb);
  }

  /**
   * 订阅撤销栈状态变化。
   *
   * @param {() => void} cb - 回调。
   * @returns {void}
   */
  onUndoStack(cb) {
    this._undoCbs.add(cb);
  }

  /**
   * 订阅整体重置（打开文件）。
   *
   * @param {() => void} cb - 回调。
   * @returns {void}
   */
  onReset(cb) {
    this._resetCbs.add(cb);
  }

  /**
   * 订阅引擎进度事件（保存/列操作重写）。
   *
   * @param {(done: number, total: number) => void} cb - 回调。
   * @returns {Promise<Function>} 取消监听函数。
   */
  async onProgress(cb) {
    return this._listen("big-progress", (e) => cb(e.payload[0], e.payload[1]));
  }

  /**
   * 触发数据变化回调。
   *
   * @param {boolean} [structural] - 是否结构变化（行列增删/筛选切换）。
   * @param {string} [origin] - 来源："fetch"=预取页落定（纯重绘，不应标脏）；
   *   其余（编辑/撤销等数据变化）照常标脏。
   * @returns {void}
   */
  _emitData(structural = false, origin = "edit") {
    for (const cb of this._dataCbs) cb(structural, origin);
  }

  _emitUndo() {
    for (const cb of this._undoCbs) cb();
  }

  _emitReset() {
    for (const cb of this._resetCbs) cb();
  }

  // ---- 同步只读接口（grid 渲染热路径） ----

  /**
   * 当前视图行数（筛选中为命中数）。
   *
   * @returns {number}
   */
  rowCount() {
    return this._filterOn ? this._filterCount : (this._meta?.rows ?? 0);
  }

  /**
   * 列数（pending 列操作已映射的视图列数）。
   *
   * @returns {number}
   */
  columnCount() {
    return this._meta?.cols ?? 0;
  }

  /**
   * 表头（pending 列操作已映射）。
   *
   * @returns {string[]}
   */
  headers() {
    return this._meta?.headers ?? [];
  }

  /**
   * 单元格原文（窗口缓存，未命中返回空串并触发异步拉取）。
   *
   * @param {number} row - 视图行号。
   * @param {number} col - 视图列号。
   * @returns {string}
   */
  rawData(row, col) {
    return this._cell(row, col);
  }

  /**
   * 单元格显示值（大模式无格式化，同 rawData）。
   *
   * @param {number} row - 视图行号。
   * @param {number} col - 视图列号。
   * @returns {string}
   */
  displayData(row, col) {
    return this._cell(row, col);
  }

  // ---- 窗口缓存 ----

  /**
   * 缓存键：筛选视图加 "f" 前缀，互不串扰。
   *
   * @param {number} page - 页号。
   * @returns {string}
   */
  _key(page) {
    return (this._filterOn ? "f" : "") + page;
  }

  /**
   * 读单元格：命中缓存直接返回；未命中调度拉页并返回空串占位。
   *
   * @param {number} row - 视图行号。
   * @param {number} col - 视图列号。
   * @returns {string}
   */
  _cell(row, col) {
    if (!this._meta) return "";
    const page = Math.floor(row / BIG_PAGE);
    const rows = this._cache.get(this._key(page));
    if (rows) {
      const cells = rows[row % BIG_PAGE];
      return cells ? (cells[col] ?? "") : "";
    }
    this._schedule(page);
    return "";
  }

  /**
   * 预取一段行范围覆盖的页（grid 滚动渲染钩子调用）。
   *
   * @param {number} first - 起始视图行。
   * @param {number} last - 结束视图行（含）。
   * @returns {void}
   */
  prefetchRange(first, last) {
    for (let r = Math.max(0, first); r <= last; r += BIG_PAGE) {
      this._schedule(Math.floor(r / BIG_PAGE));
    }
  }

  /**
   * 调度拉一页（去重；响应落定后经 onData 触发重绘）。
   *
   * @param {number} page - 页号。
   * @returns {void}
   */
  _schedule(page) {
    const key = this._key(page);
    if (this._cache.has(key) || this._inflight.has(key)) return;
    this._inflight.add(key);
    const gen = this._gen;
    const from = page * BIG_PAGE;
    const req = this._filterOn
      ? this._invoke("big_filter_page", { from, count: BIG_PAGE })
      : this._invoke("big_rows", { from, count: BIG_PAGE });
    req.then((rows) => {
      // 结构已变化（代际不同/筛选态翻转）→ 旧响应作废
      if (gen === this._gen && this._key(page) === key) {
        this._cache.set(key, rows);
        this._trim(page);
        this._emitData(false, "fetch"); // 纯预取落定：重绘即可，不得标脏
      }
    }).catch(() => {
      // 拉取失败静默：下次渲染重试（编辑冲突另有错误通道）
    }).finally(() => {
      this._inflight.delete(key);
    });
  }

  /**
   * 缓存换出：只保留当前页 ±2 页（约 5 页 = 1000 行）。
   *
   * @param {number} around - 以此页为中心。
   * @returns {void}
   */
  _trim(around) {
    const prefix = this._filterOn ? "f" : "";
    for (const key of this._cache.keys()) {
      if (!key.startsWith(prefix)) continue;
      const p = Number(key.slice(prefix.length));
      if (p < around - 2 || p > around + 2) this._cache.delete(key);
    }
  }

  /**
   * 整缓存失效（结构变化/撤销/筛选切换后）。
   *
   * @returns {void}
   */
  _invalidate() {
    this._gen += 1;
    this._cache.clear();
    this._inflight.clear();
  }

  // ---- 编辑 ----

  /**
   * 写单元格（乐观写缓存 + 异步落引擎，失败回滚抛错）。
   *
   * @param {number} row - 视图行号。
   * @param {number} col - 视图列号。
   * @param {string} value - 新值。
   * @returns {Promise<void>}
   * @throws {Error} 引擎拒绝（如列操作 pending）或网络失败。
   */
  async setData(row, col, value) {
    return this.setDataMerged(row, col, value);
  }

  /**
   * 写单元格并合并连续击键（引擎侧按值比对幂等，直接复用 setData）。
   *
   * @param {number} row - 视图行号。
   * @param {number} col - 视图列号。
   * @param {string} value - 新值。
   * @returns {Promise<void>}
   */
  async setDataMerged(row, col, value) {
    if (this._filterOn) {
      throw new Error("筛选结果中不可编辑：请先清除筛选");
    }
    if (row < 0 || row >= this.rowCount() || col < 0 || col >= this.columnCount()) {
      throw new Error("目标单元格超出表格范围");
    }
    const oldValue = this._cell(row, col);
    // 乐观写：有缓存页则原地更新，重绘立即反映
    const page = Math.floor(row / BIG_PAGE);
    const cached = this._cache.get(this._key(page));
    if (cached && cached[row % BIG_PAGE]) {
      const cells = cached[row % BIG_PAGE];
      if (col < cells.length) cells[col] = value;
    }
    try {
      await this._invoke("big_set_cell", { vrow: row, col, value });
      await this._refreshMeta();
      this._emitData(false);
    } catch (err) {
      if (cached && cached[row % BIG_PAGE] && col < cached[row % BIG_PAGE].length) {
        cached[row % BIG_PAGE][col] = oldValue;
      }
      throw new Error(`${BIG_EDIT_ERR}：${err}`);
    }
  }

  /**
   * 清空一批单元格。
   *
   * @param {Array<[number, number]>} cells - [行, 列] 列表。
   * @returns {Promise<void>}
   */
  async clearCells(cells) {
    if (this._filterOn) {
      throw new Error("筛选结果中不可编辑：请先清除筛选");
    }
    // 按 (row,col) 去重，避免重复写
    const seen = new Set();
    const jobs = [];
    for (const [r, c] of cells) {
      const k = r * 1048576 + c;
      if (seen.has(k)) continue;
      seen.add(k);
      jobs.push([r, c]);
    }
    await this._editChunked(jobs, (r, c) =>
      this._invoke("big_set_cell", { vrow: r, col: c, value: "" })
    );
    this._invalidate();
    this._emitData(true);
  }

  /**
   * 清空若干整行。
   *
   * @param {number[]} rows - 视图行号列表。
   * @returns {Promise<void>}
   */
  async clearRows(rows) {
    const ncols = this.columnCount();
    const cells = [];
    for (const r of rows) {
      for (let c = 0; c < ncols; c += 1) cells.push([r, c]);
    }
    await this.clearCells(cells);
  }

  /**
   * 清空若干整列。
   *
   * @param {number[]} cols - 视图列号列表。
   * @returns {Promise<void>}
   */
  async clearColumns(cols) {
    if (this._filterOn) {
      throw new Error("筛选结果中不可编辑：请先清除筛选");
    }
    const jobs = [];
    for (let r = 0; r < this._meta.rows; r += 1) {
      for (const c of cols) jobs.push([r, c]);
    }
    await this._editChunked(jobs, (r, c) =>
      this._invoke("big_set_cell", { vrow: r, col: c, value: "" })
    );
    this._invalidate();
    this._emitData(true);
  }

  /**
   * 分块并发执行编辑调用（块内并发、块间串行，避免上万请求同时压出）。
   *
   * @param {Array<[number, number]>} jobs - (行, 列) 列表。
   * @param {(r: number, c: number) => Promise<void>} fn - 单次编辑。
   * @returns {Promise<void>}
   */
  async _editChunked(jobs, fn) {
    const CHUNK = 256;
    for (let i = 0; i < jobs.length; i += CHUNK) {
      const part = jobs.slice(i, i + CHUNK);
      const results = await Promise.allSettled(part.map(([r, c]) => fn(r, c)));
      const failed = results.find((x) => x.status === "rejected");
      if (failed) throw new Error(`${BIG_EDIT_ERR}：${failed.reason}`);
    }
    await this._refreshMeta();
  }

  /**
   * 在指定行前插入空行。
   *
   * @param {number} row - 插入位置（视图行号）。
   * @param {number} [count=1] - 行数。
   * @returns {Promise<void>}
   */
  async insertRows(row, count = 1) {
    await this._invoke("big_insert_rows", { vpos: row, rows: null, count });
    await this._afterStructural();
  }

  /**
   * 末尾追加空行。
   *
   * @param {number} [count=1] - 行数。
   * @returns {Promise<void>}
   */
  async appendRows(count = 1) {
    await this.insertRows(this.rowCount(), count);
  }

  /**
   * 带值插入行（粘贴用：TSV 解析后的二维数组）。
   *
   * @param {number} vpos - 插入位置。
   * @param {string[][]} rows - 行数据（列超宽截断/不足补空由引擎保证）。
   * @returns {Promise<void>}
   */
  async insertRowsWithValues(vpos, rows) {
    await this._invoke("big_insert_rows", { vpos, rows, count: 0 });
    await this._afterStructural();
  }

  /**
   * 删除若干行（即时生效，位图标记）。
   *
   * @param {number[]} rows - 视图行号列表（任意顺序）。
   * @returns {Promise<void>}
   */
  async removeRows(rows) {
    if (this._filterOn) {
      throw new Error("筛选结果中不可删除：请先清除筛选");
    }
    const sorted = [...new Set(rows)].sort((a, b) => a - b);
    await this._invoke("big_remove_rows", { vrows: sorted });
    await this._afterStructural();
  }

  /**
   * 结构变化后的统一收尾：失效缓存、刷元数据、发结构通知。
   *
   * @returns {Promise<void>}
   */
  async _afterStructural() {
    this._invalidate();
    await this._refreshMeta();
    this._emitData(true);
  }

  /**
   * 重新拉取元数据并通知撤销栈监听。
   *
   * @returns {Promise<void>}
   */
  async _refreshMeta() {
    this._meta = await this._invoke("big_meta");
    this._emitUndo();
  }

  // ---- 撤销/重做（引擎侧逆操作日志） ----

  /** @returns {boolean} 可撤销。 */
  canUndo() {
    return !!this._meta?.can_undo;
  }

  /** @returns {boolean} 可重做。 */
  canRedo() {
    return !!this._meta?.can_redo;
  }

  /**
   * 撤销一步。
   *
   * @returns {Promise<boolean>} 是否生效。
   */
  async undo() {
    const ok = await this._invoke("big_undo");
    this._invalidate();
    await this._refreshMeta();
    this._emitData(true);
    return ok;
  }

  /**
   * 重做一步。
   *
   * @returns {Promise<boolean>} 是否生效。
   */
  async redo() {
    const ok = await this._invoke("big_redo");
    this._invalidate();
    await this._refreshMeta();
    this._emitData(true);
    return ok;
  }

  // ---- 筛选（按钮触发，结果存 Rust 侧） ----

  /**
   * 当前是否处于筛选结果视图。
   *
   * @returns {boolean}
   */
  filterActive() {
    return this._filterOn;
  }

  /**
   * 执行一次筛选（引擎 rayon 全表扫描）。
   *
   * @param {Array<[number, string]>} cols - (列号, 小写关键字) AND 组合。
   * @param {number[]} multiCols - 多重筛选勾选列。
   * @param {string} multiText - 多重筛选关键字（小写）。
   * @returns {Promise<number>} 命中数。
   */
  async runFilter(cols, multiCols, multiText) {
    const spec = {
      cols: cols.map(([c, kw]) => [c, kw.toLowerCase()]),
      multi_cols: multiCols,
      multi_text: multiText.toLowerCase(),
    };
    const n = await this._invoke("big_filter", { spec });
    this._filterOn = true;
    this._filterCount = n;
    this._invalidate();
    this._emitData(true);
    return n;
  }

  /**
   * 清除筛选，回到全量视图。
   *
   * @returns {Promise<void>}
   */
  async clearFilter() {
    await this._invoke("big_clear_filter");
    this._filterOn = false;
    this._filterCount = 0;
    this._invalidate();
    this._emitData(true);
  }

  // ---- 列操作（pending 预览 → 执行物理重写） ----

  /** @returns {boolean} 有待执行的列操作。 */
  colOpsPending() {
    return !!this._meta?.pending_cols;
  }

  /**
   * 追加一列（pending）。
   *
   * @param {string} name - 列名。
   * @returns {Promise<void>}
   */
  async appendColumn(name) {
    await this._pushColOp({ Insert: { pos: this.columnCount(), name } });
  }

  /**
   * 在指定列前插入列（pending）。
   *
   * @param {number} col - 视图列号。
   * @param {number} [count=1] - 列数（自动生成列名）。
   * @returns {Promise<void>}
   */
  async insertColumns(col, count = 1) {
    for (let i = 0; i < count; i += 1) {
      await this._pushColOp({ Insert: { pos: col + i, name: `列${this.columnCount() + 1 + i}` } });
    }
  }

  /**
   * 删除列（pending）。
   *
   * @param {number[]} cols - 视图列号列表（降序 push，避免位置互相位移）。
   * @returns {Promise<void>}
   */
  async removeColumns(cols) {
    for (const c of [...cols].sort((a, b) => b - a)) {
      await this._pushColOp({ Delete: { col: c } });
    }
  }

  /**
   * 重命名列（pending）。
   *
   * @param {number} col - 视图列号。
   * @param {string} name - 新列名。
   * @returns {Promise<void>}
   */
  async renameColumn(col, name) {
    await this._pushColOp({ Rename: { col, name } });
  }

  /**
   * 撤销最近一次 pending 列操作。
   *
   * @returns {Promise<void>}
   */
  async popColOp() {
    await this._invoke("big_col_pop");
    await this._refreshMeta();
    this._emitData(true);
  }

  /**
   * 清空全部 pending 列操作。
   *
   * @returns {Promise<void>}
   */
  async clearColOps() {
    await this._invoke("big_col_clear");
    await this._refreshMeta();
    this._emitData(true);
  }

  /**
   * push 一条 pending 列操作并刷新视图映射。
   *
   * @param {object} op - ColOp（serde 外部标签：{Insert:{pos,name}} 等）。
   * @returns {Promise<void>}
   */
  async _pushColOp(op) {
    await this._invoke("big_col_push", { op });
    await this._refreshMeta();
    this._emitData(true);
  }

  /**
   * 执行全部 pending 列操作（物理重写整个文件，带进度事件）。
   *
   * @returns {Promise<void>}
   */
  async applyColOps() {
    await this._invoke("big_col_apply");
    this._invalidate();
    await this._refreshMeta();
    this._emitData(true);
  }

  // ---- 保存 ----

  /**
   * 保存（引擎流式重写 + 原子换名，进度经 onProgress 订阅）。
   *
   * @returns {Promise<void>}
   */
  async save() {
    await this._invoke("big_save");
    await this._refreshMeta();
  }

  // ---- 剪贴板（大选区走引擎流式序列化） ----

  /**
   * 矩形选区转 TSV（行区间 [r0,r1)、列区间 [c0,c1)）。
   * 前端不逐格取数，引擎直接序列化，30M 行选整列也不爆内存。
   *
   * @param {number} r0 - 起始视图行（含）。
   * @param {number} r1 - 结束视图行（不含）。
   * @param {number} c0 - 起始视图列（含）。
   * @param {number} c1 - 结束视图列（不含）。
   * @returns {Promise<string>} TSV 文本（\r\n 行尾）。
   */
  async copyRows(r0, r1, c0, c1) {
    return this._invoke("big_copy", { r0, r1, c0, c1 });
  }

  /**
   * batch 兼容：小模式的多步改动合并为一步撤销；大模式每次 invoke
   * 自身落引擎撤销日志（引擎不做事务），batch 仅保证顺序执行。
   *
   * @param {Function} fn - 同步函数（内部可多次调 model 方法）。
   * @returns {Promise<void>} fn 返回的 Promise（如有）。
   */
  async batch(fn) {
    return fn();
  }

  // ---- 明确不支持（大模式 v1 边界，app 层已禁用入口） ----

  /**
   * 排序：大文件模式 v1 不支持。
   *
   * @returns {never}
   * @throws {Error} 固定提示。
   */
  sortRows() {
    throw new Error("大文件模式不支持排序");
  }

  /**
   * 全量行数据：大模式不可取（内存即设计要避免的）。
   *
   * @returns {never}
   * @throws {Error} 固定提示。
   */
  allRows() {
    throw new Error("大文件模式不支持全量读取");
  }

  /**
   * 装载：大模式走 open(path) 路由，不走 load()。
   *
   * @returns {never}
   * @throws {Error} 固定提示。
   */
  load() {
    throw new Error("大文件模式请使用 open(path)");
  }
}
