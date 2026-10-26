/**
 * 统一数据模型（全表唯一模型——新建内存档与磁盘档同走大引擎）。
 *
 * grid/app 通过它透明访问 Rust 引擎（mmap + 行索引 + 覆盖层）：
 * - 取数走 200 行/页的同步窗口缓存，±2 屏预取，落定经 onData 触发重绘；
 * - 编辑/插删/撤销立即异步落引擎，乐观写缓存，失败回滚并抛错；
 * - 筛选为按钮触发的整表扫描（引擎 rayon 并行），结果集存 Rust 侧；
 * - 列操作先进 pending 列表，执行时提交可撤销的共享 Schema。
 *
 * 依赖注入：构造可传 {invoke, listen}，便于 node:test 脱离 Tauri 环境测试。
 */

import { DEFAULT_UNDO_BUDGET_MIB, normalizeUndoBudgetMiB, undoBudgetBytes } from "./prefs.js";
import { bytesToHex, utf8Chunks } from "./clipboard.js";

/** 之后新建会话使用的默认历史预算；已有会话只能由显式设置修改。 */
let defaultUndoBudgetMiB = DEFAULT_UNDO_BUDGET_MIB;

/**
 * 设置之后 open/create 会话的默认历史预算。
 *
 * @param {unknown} raw - 原始 MiB 值；非法值回退 64。
 * @returns {number} 实际采用的 MiB 值。
 */
export function setDefaultUndoBudgetMiB(raw) {
  defaultUndoBudgetMiB = normalizeUndoBudgetMiB(raw);
  return defaultUndoBudgetMiB;
}

/** 取页大小（行）。与引擎窗口一致；±2 屏缓存约 5 页 ≈ 1000 行。 */
export const BIG_PAGE = 200;

/** 同时在飞的取页请求数上限。拖动途中视口会掠过大量页，全部并发会把
 * 机械盘/引擎 I/O 队列塞满，反而让最终视口页迟到；限流后结合队列裁剪，
 * 只把带宽留给当前视口附近的页。 */
export const PAGE_PARALLEL = 2;

/** 大文件模式阈值（字节）：≥ 走大模式。依据 target/bench/bench_node.mjs：
 * 100MB 装载 ~1.5s/RSS 1.3GB、按键筛选 ~350ms；500MB V8 直接 OOM。
 * 50MiB 时筛选 ≤ ~175ms、RSS < 1GB，仍在"键入即筛"的舒适区。 */
export const BIG_THRESHOLD = 50 * 1024 * 1024;

/** 乐观编辑失败时抛出的统一错误前缀（app 层捕获转状态栏）。 */
const BIG_EDIT_ERR = "大文件编辑失败";

/** accepted 阶段只允许读取；写入必须等 complete 解除只读。 */
const TRANSFER_READ_COMMANDS = new Set([
  "big_meta", "big_rows", "big_filter_page", "big_filter_count",
  "big_filter_hit_view_row", "big_find_edge", "big_stale",
  "big_sort_estimate", "big_copy", "big_clipboard_meta",
]);

/** 这些命令在途时不进入 sealing，避免保存/重写与迁移屏障交叉。 */
const TRANSFER_BLOCK_COMMANDS = new Set([
  "big_save", "big_save_as", "big_sort_load", "big_sort_apply", "big_sort_exit",
  "big_col_apply", "big_paste_import_begin", "big_paste_import_chunk",
  "big_paste_import_finish", "big_paste_clipboard",
]);


/**
 * 大文件模型。
 */
export class BigModel {
  /**
   * @param {{invoke: Function, listen: Function, confirmFixedPoint?: Function}} [io]
   *   Tauri 桥与固定点确认（可注入 mock）。
   */
  constructor(io) {
    /** 大文件模式标记：grid/app 据此刻画双模式行为 */
    this.isBig = true;
    const tauri = io ?? {
      invoke: (cmd, args) => window.__TAURI__.core.invoke(cmd, args),
      listen: (ev, cb) => window.__TAURI__.event.listen(ev, cb),
    };
    /** @type {Function} 原始 Tauri 调用（_invoke 负责附加会话号） */
    this._rawInvoke = tauri.invoke;
    /** @type {number | null} Rust 在 open/create 成功后分配的会话号 */
    this._sid = null;
    /** @type {number | null} 当前 owner 的文档租约 */
    this._lease = null;
    /** @type {Function} */
    this._listen = tauri.listen;
    /** @type {Function} 超大操作确认；未注入时安全取消。 */
    this._confirmFixedPoint = tauri.confirmFixedPoint ?? (async () => false);
    /** @type {number} 索引进度轮询周期 ms（测试可注入更短值） */
    this._pollMs = tauri.pollMs ?? 200;
    /** @type {object | null} 最近一次的 big_meta 缓存 */
    this._meta = null;
    /** @type {Map<string, string[][]>} 页缓存，键 `${"f"|""}${pageIdx}` */
    this._cache = new Map();
    /** @type {Set<string>} 正在拉取或排队中的页 */
    this._inflight = new Set();
    /** @type {number[]} 待拉取页队列（按离视口远近取用，可随时裁剪） */
    this._queue = [];
    /** @type {number} 当前在飞请求数（≤ PAGE_PARALLEL） */
    this._active = 0;
    /** @type {number | null} 最近一次渲染的视口起始行 */
    this._viewFirst = null;
    /** @type {number | null} 最近一次渲染的视口结束行 */
    this._viewLast = null;
    /** @type {Set<string>} 索引中拉取的"部分页"（页尾尚未索引，行数增长后须重拉） */
    this._partial = new Set();
    /** @type {number} 代际号：结构变化后旧响应作废 */
    this._gen = 0;
    /** @type {number} 会话语义号：只被 open/close 递增，撤销等缓存失效不得停掉轮询 */
    this._session = 0;
    /** @type {number} 索引进度轮询计时器（0 = 未在轮询） */
    this._indexTimer = 0;
    /** @type {number | null} 打开起点（performance.now；首屏耗时基准） */
    this._openT0 = null;
    /** @type {number} 首屏耗时 ms（第一次 markFirstPaint 锁定） */
    this._firstPaintMs = 0;
    /** @type {Set<Function>} 索引进度订阅者 */
    this._progressCbs = new Set();
    /** @type {boolean} 筛选结果视图 */
    this._filterOn = false;
    /** @type {number} 筛选命中数（_filterOn 时的 rowCount） */
    this._filterCount = 0;
    /** @type {number} 筛选请求代际：键入即筛时旧请求的迟到响应一律丢弃 */
    this._filterGen = 0;
    /** @type {Set<Function>} */
    this._dataCbs = new Set();
    /** @type {Set<Function>} */
    this._undoCbs = new Set();
    /** @type {Set<Function>} */
    this._resetCbs = new Set();
    /** @type {Map<symbol, {cmd: string, promise: Promise<any>}>} 在途 IPC；迁移屏障等待其落定 */
    this._operations = new Map();
    /** @type {"idle" | "sealing" | "accepted"} 文档迁移状态 */
    this._transferState = "idle";
    /** 固定点确认等待期间禁止开始迁移 */
    this._confirmingFixedPoint = false;
    /** @type {{cols: Array, multiCols: number[], multiText: string} | null} 最近一次筛选控件快照 */
    this._filterView = null;
    /** @type {string | null} 当前文档路径（adopt 不重新读盘） */
    this._path = null;
    /** @type {string} 标签标题快照 */
    this._title = "未命名";
  }

  /**
   * 本模型独占的引擎会话号（调试/诊断用；命令调用请走 _invoke）。
   *
   * @returns {number | null} 会话号；尚未建立会话时为 null。
   */
  get sessionId() {
    return this._sid;
  }

  /** @returns {number | null} 当前文档租约。 */
  get lease() {
    return this._lease;
  }

  /**
   * 调用引擎命令并附加本模型会话号。
   *
   * 所有 big_* 命令都按会话寻址：多页签同时打开时，后台索引轮询/筛选
   * 只能读写自己的会话，不能落到当前活动页签的引擎上。
   *
   * @param {string} cmd - Tauri 命令名。
   * @param {object} [args] - 命令参数（不含 session）。
   * @returns {Promise<any>} 命令返回值。
   */
  _invoke(cmd, args = {}) {
    if (typeof this._sid !== "number" || typeof this._lease !== "number") {
      throw new Error("大文件会话尚未建立");
    }
    if (this._transferState === "sealing") {
      return Promise.reject(new Error("文档正在迁移，请稍后重试"));
    }
    if (this._transferState === "accepted" && !TRANSFER_READ_COMMANDS.has(cmd)) {
      return Promise.reject(new Error("迁移接管中，文档当前只读"));
    }
    const key = Symbol(cmd);
    const promise = Promise.resolve(this._rawInvoke(cmd, {
      session: this._sid, lease: this._lease, ...args,
    }));
    this._operations.set(key, { cmd, promise });
    return promise.finally(() => this._operations.delete(key));
  }

  /**
   * 调编辑命令；单步超预算时确认，并只携带原预检代际重试一次。
   *
   * @param {string} cmd - 编辑命令名。
   * @param {object} [args] - 不含 approval 的命令参数。
   * @returns {Promise<object>} EditOutcome；用户取消时 cancelled=true。
   * @throws {Error} 引擎失败，或确认后预检已经过期。
   */
  async _invokeEdit(cmd, args = {}) {
    const first = await this._invoke(cmd, {
      ...args,
      approval: { allowFixedPoint: false, preflightId: null },
    });
    if (!first.fixedPoint) return first;
    this._confirmingFixedPoint = true;
    let confirmed;
    try {
      confirmed = await this._confirmFixedPoint(first.fixedPoint);
    } finally {
      this._confirmingFixedPoint = false;
    }
    if (!confirmed) {
      return { changed: false, insertedRows: 0, removedRows: 0,
        fixedPoint: null, cancelled: true };
    }
    const retry = await this._invoke(cmd, {
      ...args,
      approval: {
        allowFixedPoint: true,
        preflightId: first.fixedPoint.preflightId,
      },
    });
    if (retry.fixedPoint) throw new Error("固定点预检已过期，请重试");
    return retry;
  }

  /**
   * 实际修改后的统一收尾；取消和空改动不刷新、不失效、不通知。
   *
   * @param {object} outcome - 引擎 EditOutcome。
   * @param {string} [origin] - 数据通知来源。
   * @param {boolean} [forceStructural] - 行序/结构变化时强制结构通知。
   * @returns {Promise<object>} 原 EditOutcome。
   */
  async _afterEdit(outcome, origin = "edit", forceStructural = false) {
    if (!outcome?.changed) return outcome;
    const previousCols = this._meta?.cols;
    this._invalidate();
    // _refreshMeta 已发 onUndoStack，这里不能重复通知。
    await this._refreshMeta();
    const columnsChanged = previousCols !== undefined && this._meta?.cols !== previousCols;
    this._emitData(
      forceStructural || outcome.insertedRows > 0 || outcome.removedRows > 0 || columnsChanged,
      origin,
    );
    return outcome;
  }

  // ---- 生命周期 ----

  /**
   * 按当前默认预算初始化新会话；并发设置改变目标时追平最新值。
   *
   * @returns {Promise<void>} 最新默认预算已下发后完成。
   * @throws {Error} 引擎会话失效或预算命令失败。
   */
  async _applyDefaultUndoBudget() {
    for (;;) {
      const mib = defaultUndoBudgetMiB;
      await this.setUndoBudget(mib);
      if (mib === defaultUndoBudgetMiB) return;
    }
  }

  /**
   * 打开大文件会话（引擎 mmap + 同步扫首批行，秒开）。
   *
   * 引擎打开后立即返回首批可用行（indexing=true），后台线程继续索引；
   * 本模型按 `pollMs` 轮询 big_meta，行数增长经 onData 重绘，进度经
   * onIndexProgress 通知。首屏耗时从本调用开始计。
   *
   * @param {string} path - 文件绝对路径。
   * @param {{headerless?: boolean}} [opts] - headerless=true 表示首行也是
   *   数据（无列名文件）。
   * @returns {Promise<object>} BigMeta（可能 indexing=true）。
   */
  async open(path, { headerless = false } = {}) {
    this._stopIndexPolling();
    const started = performance.now();
    let opened;
    try {
      opened = await this._rawInvoke("big_open", {
        path,
        headerless: !!headerless,
        replaceSession: this._sid,
        replaceLease: this._lease,
      });
      if (typeof opened?.session !== "number" || typeof opened?.lease !== "number" || !opened.meta) {
        throw new Error("big_open 未返回有效 BigSession");
      }
    } catch (error) {
      this._startIndexPolling();
      throw error;
    }
    this._session += 1;
    this._openT0 = started;
    this._firstPaintMs = 0;
    this._sid = opened.session;
    this._lease = opened.lease;
    this._meta = opened.meta;
    this._path = path;
    this._title = path?.split(/[\\/]/).pop() ?? "未命名";
    this._filterView = null;
    await this._applyDefaultUndoBudget();
    this._invalidate();
    this._emitReset();
    this._emitData(true);
    this._emitUndo();
    this._emitProgress();
    this._startIndexPolling();
    return this._meta;
  }

  /**
   * 以内存文本新建会话（统一大引擎：不写盘、无路径，另存为后获得路径）。
   *
   * @param {string} text - CSV 全文（serializeRecords 产出）。
   * @param {{headerless?: boolean}} [opts] - headerless=true 表示首行也是数据。
   * @returns {Promise<object>} BigMeta（同步索引完成，indexing=false）。
   */
  async create(text, { headerless = false } = {}) {
    this._stopIndexPolling();
    const started = performance.now();
    let created;
    try {
      created = await this._rawInvoke("big_new", {
        text,
        headerless: !!headerless,
        replaceSession: this._sid,
        replaceLease: this._lease,
      });
      if (typeof created?.session !== "number" || typeof created?.lease !== "number" || !created.meta) {
        throw new Error("big_new 未返回有效 BigSession");
      }
    } catch (error) {
      this._startIndexPolling();
      throw error;
    }
    this._session += 1;
    this._openT0 = started;
    this._firstPaintMs = 0;
    this._sid = created.session;
    this._lease = created.lease;
    this._meta = created.meta;
    this._path = null;
    this._title = "未命名";
    this._filterView = null;
    await this._applyDefaultUndoBudget();
    this._invalidate();
    this._emitReset();
    this._emitData(true);
    this._emitUndo();
    this._emitProgress();
    this._startIndexPolling();
    return this._meta;
  }

  /**
   * 进入迁移屏障：拒绝保存/重写等在途操作，等待其余 IPC 落定并保存视图快照。
   *
   * @returns {Promise<object>} 随 ticket 传给目标窗口的快照。
   * @throws {Error} 无会话、正在迁移或关键操作尚未完成。
   */
  async prepareTransfer() {
    if (this._sid === null || this._transferState !== "idle") {
      throw new Error("文档正在迁移，请稍后重试");
    }
    const blocked = [...this._operations.values()].some(({ cmd }) => TRANSFER_BLOCK_COMMANDS.has(cmd));
    if (this._confirmingFixedPoint || blocked) {
      throw new Error("当前操作尚未完成，请稍后重试");
    }
    this._transferState = "sealing";
    try {
      const pending = [...this._operations.values()].map(({ promise }) => promise);
      const settled = await Promise.allSettled(pending);
      const failed = settled.find((result) => result.status === "rejected");
      if (failed) throw failed.reason;
    } catch (error) {
      this.rollbackTransfer(this._lease);
      throw error;
    }
    const filter = this._filterOn ? {
      cols: (this._filterView?.cols ?? []).map((item) => item.slice()),
      multiCols: [...(this._filterView?.multiCols ?? [])],
      multiText: this._filterView?.multiText ?? "",
    } : null;
    return {
      filter,
      headerless: this.isHeaderless(),
      path: this._path,
      title: this._title,
      dirty: this.modified(),
      view: null,
    };
  }

  /**
   * 接管 Rust 已 accept 的同一 BigFile；不 open/new，也不重设撤销预算。
   *
   * @param {object} adopted - Rust AdoptedSession。
   * @returns {Promise<object>} 同一引擎会话的 BigMeta。
   */
  async adopt(adopted) {
    if (typeof adopted?.session !== "number" || typeof adopted?.lease !== "number" || !adopted.meta) {
      throw new Error("accept_transfer 未返回有效 AdoptedSession");
    }
    this._stopIndexPolling();
    this._session += 1;
    this._sid = adopted.session;
    this._lease = adopted.lease;
    this._meta = adopted.meta;
    this._path = adopted.path ?? null;
    this._title = adopted.snapshot?.title ?? this._path?.split(/[\\/]/).pop() ?? "未命名";
    this._openT0 = null;
    this._firstPaintMs = 0;
    this._transferState = "accepted";
    this._filterOn = adopted.meta.filtered === true;
    this._filterView = adopted.snapshot?.filter ?? null;
    this._filterCount = 0;
    this._filterGen += 1;
    this._invalidate();
    if (this._filterOn) {
      this._filterCount = Number(await this._invoke("big_filter_count") ?? 0);
    }
    this._emitReset();
    this._emitData(true, "fetch");
    this._emitUndo();
    this._emitProgress();
    this._startIndexPolling();
    return this._meta;
  }

  /**
   * complete/recovered 后解除 accepted 只读；session 和 lease 不变。
   *
   * @param {number} lease - Rust 最终确认的租约。
   * @returns {void}
   */
  finishTransfer(lease) {
    if (typeof lease !== "number") throw new Error("迁移完成未返回有效 lease");
    this._lease = lease;
    this._transferState = "idle";
    this._emitData(true, "fetch");
    this._emitUndo();
    this._emitProgress();
    this._startIndexPolling();
  }

  /**
   * cancel/timeout 后恢复源租约，使旧页响应失效并重新允许源窗口操作。
   *
   * @param {number} restoredLease - Rust 提升后的源租约。
   * @returns {void}
   */
  rollbackTransfer(restoredLease) {
    if (typeof restoredLease !== "number") throw new Error("迁移回滚未返回有效 lease");
    this._lease = restoredLease;
    this._transferState = "idle";
    this._session += 1;
    this._invalidate();
    this._emitData(true, "fetch");
    this._emitUndo();
    this._emitProgress();
    this._startIndexPolling();
  }

  /**
   * 设置本会话的撤销/重做共用历史预算。
   *
   * @param {unknown} mib - 每文档预算，合法范围 1–1024 MiB。
   * @returns {Promise<object>} 设置后的最新 BigMeta。
   * @throws {Error} 引擎会话失效或预算命令失败。
   */
  async setUndoBudget(mib) {
    this._meta = await this._invoke("big_set_undo_budget", {
      budgetBytes: undoBudgetBytes(mib),
    });
    this._emitUndo();
    return this._meta;
  }

  /**
   * 读取引擎保存点脏点，而不是由前端按操作方向猜测。
   *
   * @returns {boolean} 当前内容不能无修改地回到已保存状态时为 true。
   */
  modified() {
    return this._meta?.modified === true;
  }

  /**
   * 预热全部数据页入缓存（冒烟夹具专用）：create/open 后同步 rawData 立即可读。
   *
   * 直走 _invoke + _cache.set，不经过 _fetch 落定回调，因此不会触发 _trim
   * 的视口逐出；会话中途被换（_session 变）时立即作废退出。
   *
   * @returns {Promise<void>}
   */
  async warmup() {
    const session = this._session;
    const n = this.rowCount();
    for (let from = 0; from < n; from += BIG_PAGE) {
      if (session !== this._session) return; // 会话已换，预热作废
      const rows = await this._invoke("big_rows", { from, count: BIG_PAGE });
      this._cache.set(this._key(Math.floor(from / BIG_PAGE)), { rows, vrows: null });
    }
    if (session === this._session) this._emitData(false, "fetch");
  }

  /**
   * 关闭大文件会话（打开新文件/退出时）。
   *
   * @returns {Promise<void>}
   */
  async close() {
    this._stopIndexPolling();
    this._session += 1;
    if (this._sid !== null) await this._invoke("big_close");
    this._sid = null;
    this._lease = null;
    this._meta = null;
    this._openT0 = null;
    this._firstPaintMs = 0;
    this._invalidate();
  }

  /**
   * 是否仍在后台建立行索引（大文件打开后的短暂窗口）。
   *
   * @returns {boolean} true = 索引中：筛选/跳转/保存/结构变更暂不可用。
   */
  indexing() {
    return !!this._meta?.indexing;
  }

  /**
   * 是否处于「首行非列名」模式（meta 缺字段视为 false，兼容旧引擎）。
   *
   * @returns {boolean} 首行作为数据时为 true。
   */
  isHeaderless() {
    return !!this._meta?.headerless;
  }

  /**
   * 切换「首行非列名」模式（引擎段表 O(1) 插/删首行）。
   *
   * 实际切换后清空筛选和窗口缓存，并记录可撤销的 HeaderMode 历史。
   * 开关属于视图属性：以 origin="view" 通知，不额外标脏。
   *
   * @param {boolean} on - true=首行不是列名；false=首行是列名。
   * @returns {Promise<object|undefined>} EditOutcome；同态时返回 undefined。
   * @throws {Error} 引擎拒绝（无行可提升列名、待执行列操作等）。
   */
  async setHeaderless(on) {
    const want = !!on;
    if (want === this.isHeaderless()) {
      return;
    }
    const outcome = await this._invokeEdit("big_set_headerless", { on: want });
    if (!outcome.changed || outcome.cancelled) return outcome;
    // 引擎已清空筛选；让迟到响应作废，前端视图同步回全量。
    this._filterGen += 1;
    this._filterOn = false;
    this._filterCount = 0;
    return this._afterEdit(outcome, "view");
  }

  /**
   * 首屏耗时：open 开始 → app 完成首次视图装载（只认第一次标记）。
   *
   * @returns {number} 毫秒；尚未标记返回 0。
   */
  firstPaintMs() {
    return this._firstPaintMs;
  }

  /**
   * 标记首屏完成（app 在 applyViewState 后调用；只认第一次）。
   *
   * @returns {void}
   */
  markFirstPaint() {
    if (this._openT0 === null || this._firstPaintMs) return;
    this._firstPaintMs = Math.max(0, performance.now() - this._openT0);
  }

  /**
   * 订阅索引进度（每次轮询/打开时通知，载荷为最新 BigMeta）。
   *
   * @param {(meta: object) => void} cb - 回调。
   * @returns {void}
   */
  onIndexProgress(cb) {
    this._progressCbs.add(cb);
  }

  /** 通知索引进度订阅者 */
  _emitProgress() {
    for (const cb of this._progressCbs) cb(this._meta);
  }

  /**
   * 开始轮询索引进度（indexing=false 或无元数据时不启动）。
   *
   * @returns {void}
   */
  _startIndexPolling() {
    if (!this._meta?.indexing || this._indexTimer) return;
    this._indexTimer = setTimeout(() => this._pollIndex(), this._pollMs);
  }

  /** 停止索引进度轮询 */
  _stopIndexPolling() {
    clearTimeout(this._indexTimer);
    this._indexTimer = 0;
  }

  /**
   * 轮询一次索引进度：行数增长时丢弃部分页并触发结构重绘；完成后停表。
   *
   * 用会话语义号（而非缓存代际号）校验迟到响应——索引期间撤销/编辑会
   * 递增缓存代际，但轮询必须继续。
   *
   * @returns {Promise<void>}
   */
  async _pollIndex() {
    this._indexTimer = 0;
    if (!this._meta?.indexing) return;
    const session = this._session;
    let meta;
    try {
      meta = await this._invoke("big_meta");
    } catch {
      return; // 会话已关闭等：静默停止
    }
    if (session !== this._session || !this._meta) return;
    const prevRows = Number(this._meta.rows ?? 0);
    const grew = Number(meta.rows ?? 0) > prevRows;
    const finished = !meta.indexing;
    this._meta = meta;
    if (grew) this._dropPartialPages();
    this._emitProgress();
    // 行数增长/索引完成：画布总高与行号域变化，按结构变化重绘（origin=fetch 不标脏）
    if (grew || finished) this._emitData(true, "fetch");
    if (!finished) this._indexTimer = setTimeout(() => this._pollIndex(), this._pollMs);
  }

  /**
   * 丢弃索引中拉取的部分页：页尾在拉取时尚未索引，行数增长后须按新行数重拉。
   * 完整页（拉取时页尾已在已索引范围内）保留，避免无谓 IPC 与白闪。
   *
   * @returns {void}
   */
  _dropPartialPages() {
    for (const key of this._partial) this._cache.delete(key);
    this._partial.clear();
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
   * 事件载荷 `(加权done, total, 阶段, 行数)`：阶段 0 流式重写 / 1 落盘换名 /
   * 2 重建索引；加权把总体进度切为 流式 0-70% / 落盘 70-78% / 索引至 100%。
   *
   * @param {(done: number, total: number, phase: number, rows: number) => void} cb - 回调。
   * @returns {Promise<Function>} 取消监听函数。
   */
  async onProgress(cb) {
    return this._listen("big-progress", (e) => {
      const { session, lease, done, total, phase, rows } = e.payload;
      if (session === this._sid && lease === this._lease) cb(done, total, phase, rows);
    });
  }

  /**
   * 订阅排序模式载入进度（事件带会话号和租约，只接收本模型当前租约）。
   *
   * @param {(done: number, total: number) => void} cb - 回调（字节）。
   * @returns {Promise<Function>} 取消监听函数。
   */
  async onSortProgress(cb) {
    return this._listen("big-sort-progress", (e) => {
      const { session, lease, done, total } = e.payload;
      if (session === this._sid && lease === this._lease) cb(done, total);
    });
  }

  /**
   * 排序模式内存估算（确认弹窗展示）。
   *
   * @param {number} [levels=1] - 排序级别数。
   * @returns {Promise<{file_bytes: number, row_count: number,
   *   new_bytes: number, peak_bytes: number,
   *   available_bytes: number | null}>} 估算结果（字节）。
   */
  async sortEstimate(levels = 1) {
    return this._invoke("big_sort_estimate", { levels });
  }

  /**
   * 把文件分块载入私有内存缓冲（排序模式）；进度经 {@link onSortProgress}。
   *
   * @returns {Promise<boolean>} true=已载入；false=用户取消（仍用原 mmap 视图）。
   */
  async loadIntoRam() {
    const loaded = await this._invoke("big_sort_load");
    await this._refreshMeta();
    return loaded;
  }

  /**
   * 取消正在进行的排序模式载入（幂等）。
   *
   * @returns {Promise<void>}
   */
  async cancelSortLoad() {
    await this._invoke("big_sort_cancel");
  }

  /**
   * 对当前视图排序（引擎侧排列层，不改文件；需先 {@link loadIntoRam}）。
   *
   * 排序只改变视图：支持固定点确认，发出 origin="view" 的结构通知。
   * 已激活的筛选结果会随排序重新定位，成员不变。
   *
   * @param {Array<{col: number, numeric: boolean, asc: boolean}>} levels - 排序级别。
   * @returns {Promise<object>} EditOutcome；取消时 changed=false、cancelled=true。
   */
  async sort(levels) {
    const outcome = await this._invokeEdit("big_sort_apply", { levels });
    return this._afterEdit(outcome, "view", true);
  }

  /**
   * 退出排序视图；默认同时释放私有内存缓冲、回到 mmap 视图。
   *
   * @param {{releaseRam?: boolean}} [opts] - releaseRam=false 时保留内存后备。
   * @returns {Promise<object>} EditOutcome；取消时 changed=false、cancelled=true。
   */
  async exitSort({ releaseRam = true } = {}) {
    const outcome = await this._invokeEdit("big_sort_exit", { releaseRam });
    return this._afterEdit(outcome, "view", true);
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
   * Excel 风格纵向数据边缘查找（Ctrl+↑/↓）。
   *
   * 扫描在 Rust 引擎内完成：前端窗口缓存只有 ±2 页，跨千万行的边缘
   * 判断不能靠 JS 逐格取数。筛选视图下按命中行序号导航，排序视图下按
   * 当前显示顺序导航。
   *
   * @param {number} row - 当前视图行号（筛选视图为命中序号）。
   * @param {number} col - 视图列号。
   * @param {boolean} down - true=向下（Ctrl+↓）；false=向上（Ctrl+↑）。
   * @returns {Promise<number | null>} 目标视图行号；空表返回 null。
   * @throws {Error} 引擎拒绝（列号越界/会话失效等）。
   */
  async findVerticalEdge(row, col, down) {
    if (!this._meta || this.rowCount() <= 0) return null;
    const result = await this._invoke("big_find_edge", {
      from: row,
      col,
      down: !!down,
      filtered: this._filterOn,
    });
    if (result === null || result === undefined) return null;
    const n = Number(result);
    return Number.isFinite(n) ? n : null;
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
    // 显示文本必须压平（原文 rawData 不动，编辑/保存仍用原值）
    return this._cell(row, col)
      .replace(/\r/g, " ").replace(/\n/g, " ").replace(/\t/g, " ");
  }

  /**
   * 单元格是否已在缓存（grid 键控复用判据：命中才刷新文本，未命中保留旧内容防白闪）。
   *
   * @param {number} row - 视图行号。
   * @param {number} col - 视图列号。
   * @returns {boolean} 页已缓存且该行存在。
   */
  hasData(row, col) {
    if (!this._meta) return false;
    const page = this._cache.get(this._key(Math.floor(row / BIG_PAGE)));
    return !!page?.rows[row % BIG_PAGE];
  }

  /**
   * 只读探测缓存（不触发拉取）：编辑窗格在取页落定瞬间判读当前格——rawData 未
   * 命中会返回空串并调度重拉，与 trim 驱逐形成乒乓循环、还把窗格刷空（频闪根因）；
   * 未命中返回 undefined，调用方保留旧内容。
   *
   * @param {number} row - 视图行号。
   * @param {number} col - 视图列号。
   * @returns {string | undefined} 命中返回值（可为空串），未命中返回 undefined。
   */
  peekData(row, col) {
    if (!this._meta) return undefined;
    const page = this._cache.get(this._key(Math.floor(row / BIG_PAGE)));
    if (!page) return undefined;
    const cells = page.rows[row % BIG_PAGE];
    return cells ? (cells[col] ?? "") : undefined;
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
    const cached = this._cache.get(this._key(page));
    if (cached) {
      const cells = cached.rows[row % BIG_PAGE];
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
    const f = Math.max(0, first);
    const l = Math.max(f, last);
    this._viewFirst = f;
    this._viewLast = l;
    const loPage = Math.floor(f / BIG_PAGE) - 2;
    const hiPage = Math.floor(l / BIG_PAGE) + 2;
    // 快速拖动：丢弃离新视口已远的排队页，避免中间页把 I/O 队列塞满。
    // inflight 里的占位必须一并移除，否则这些页永远无法重新调度。
    if (this._queue.length > 0) {
      const keep = [];
      for (const p of this._queue) {
        if (p >= loPage && p <= hiPage) {
          keep.push(p);
        } else {
          this._inflight.delete(this._key(p));
        }
      }
      this._queue = keep;
    }
    // 按页边界枚举而不是从 first 按 200 行步进：视口起点在页中、末尾
    // 跨到下一页时，步进会直接越过下一页，导致视口底部整段空白。
    const firstPage = Math.floor(f / BIG_PAGE);
    const lastPage = Math.floor(l / BIG_PAGE);
    for (let p = firstPage; p <= lastPage; p += 1) {
      this._schedule(p);
    }
    this._pump();
  }

  /**
   * 调度拉一页：去重后入队，由 _pump 按并发上限与视口距离发出。
   *
   * @param {number} page - 页号。
   * @returns {void}
   */
  _schedule(page) {
    const key = this._key(page);
    if (this._cache.has(key) || this._inflight.has(key)) return;
    this._inflight.add(key);
    this._queue.push(page);
    this._pump();
  }

  /**
   * 发出排队中的取页请求，直到达到并发上限。
   *
   * @returns {void}
   */
  _pump() {
    while (this._active < PAGE_PARALLEL && this._queue.length > 0) {
      this._active += 1;
      this._fetch(this._takeNearest());
    }
  }

  /**
   * 取队列中离当前视口最近的一页（同等距离按入队顺序）。
   *
   * @returns {number} 页号。
   */
  _takeNearest() {
    let best = 0;
    let bestDist = Infinity;
    for (let i = 0; i < this._queue.length; i += 1) {
      const d = this._pageDistance(this._queue[i]);
      if (d < bestDist) {
        best = i;
        bestDist = d;
        if (d === 0) break; // 已在视口内，不可能更近
      }
    }
    return this._queue.splice(best, 1)[0];
  }

  /**
   * 页到当前视口页范围的间距（页数）；视口未知时视为 0。
   *
   * @param {number} page - 页号。
   * @returns {number} 距视口页范围的页数（视口内为 0）。
   */
  _pageDistance(page) {
    if (this._viewFirst === null || this._viewLast === null) return 0;
    const first = Math.floor(this._viewFirst / BIG_PAGE);
    const last = Math.floor(this._viewLast / BIG_PAGE);
    if (page < first) return first - page;
    if (page > last) return page - last;
    return 0;
  }

  /**
   * 真正拉取一页（调用方已完成去重与计数）。
   *
   * @param {number} page - 页号。
   * @returns {void}
   */
  _fetch(page) {
    const key = this._key(page);
    const gen = this._gen;
    const from = page * BIG_PAGE;
    // 本请求是否因行数增长而重新排队：重排后 key 由新请求占用，
    // finally 不能再删除它的 inflight 占位。
    let rescheduled = false;
    // 拉取时的行数上限：页尾是否已索引，决定这一页落定后是否算"部分页"
    const atRows = this._filterOn ? this._filterCount : Number(this._meta?.rows ?? 0);
    const req = this._filterOn
      ? this._invoke("big_filter_page", { from, count: BIG_PAGE })
      : this._invoke("big_rows", { from, count: BIG_PAGE });
    req.then((payload) => {
      // 结构已变化（代际不同/筛选态翻转）→ 旧响应作废
      if (gen !== this._gen || this._key(page) !== key) return;
      const fullAtFetch = from + BIG_PAGE <= atRows;
      if (!fullAtFetch) {
        const nowRows = this._filterOn ? this._filterCount : Number(this._meta?.rows ?? 0);
        if (nowRows > atRows) {
          // 飞行期间行数增长：本页响应缺尾部行，丢弃后按新行数重拉
          rescheduled = true;
          this._inflight.delete(key);
          this._schedule(page);
          return;
        }
        // 页尾尚未索引：登记为部分页，后续行数增长时整体失效
        this._partial.add(key);
      }
      // 筛选页载荷是 FilterPage {rows, vrows}（行内容与原始行号成对）；
      // 全量页是行数组。按捕获的 key 前缀判定解析——飞行期间 _filterOn
      // 可能已翻转，读当前态会把全量页误当筛选页（或反之）错位落定。
      const entry = key[0] === "f"
        ? { rows: payload.rows ?? [], vrows: payload.vrows ?? null }
        : { rows: payload, vrows: null };
      this._cache.set(key, entry);
      this._trim(page);
      this._emitData(false, "fetch"); // 纯预取落定：重绘即可，不得标脏
    }).catch(() => {
      // 拉取失败静默：下次渲染重试（编辑冲突另有错误通道）
    }).finally(() => {
      // 代际已变（_invalidate 清空过占位）或被重排时，占位归新请求所有
      if (!rescheduled && gen === this._gen) this._inflight.delete(key);
      this._active -= 1;
      this._pump();
    });
  }

  /**
   * 缓存换出：保留当前视口 ±2 页（约 5 页 = 1000 行）。
   *
   * 必须以视口为中心而不是以刚落定的页为中心：快速拖动时旧页响应会迟到
   * 落定，若按该页裁剪，恰好位于视口的页会被逐出，界面整屏空白。
   *
   * @param {number} around - 无视口信息时的回退中心页。
   * @returns {void}
   */
  _trim(around) {
    const prefix = this._filterOn ? "f" : "";
    let lo = around - 2;
    let hi = around + 2;
    if (this._viewFirst !== null && this._viewLast !== null) {
      lo = Math.floor(this._viewFirst / BIG_PAGE) - 2;
      hi = Math.floor(this._viewLast / BIG_PAGE) + 2;
    }
    for (const key of this._cache.keys()) {
      if (!key.startsWith(prefix)) continue;
      const p = Number(key.slice(prefix.length));
      if (p < lo || p > hi) {
        this._cache.delete(key);
        this._partial.delete(key);
      }
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
    this._queue = [];
    this._partial.clear();
    this._viewFirst = null;
    this._viewLast = null;
  }

  // ---- 编辑 ----

  /**
   * 索引期间的能力闸门：结构变更/筛选/保存/列操作一律拒绝（单元格编辑放行）。
   *
   * @param {string} msg - 面向用户的中文原因。
   * @returns {void}
   * @throws {Error} 引擎仍在建索引时。
   */
  _requireNotIndexing(msg) {
    if (this.indexing()) throw new Error(msg);
  }

  /**
   * 写单元格（乐观写缓存 + 异步落引擎，失败回滚抛错）。
   *
   * @param {number} row - 视图行号。
   * @param {number} col - 视图列号。
   * @param {string} value - 新值。
   * @returns {Promise<object>} 引擎 EditOutcome。
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
    // 乐观写：有缓存页则原地更新，重绘立即反映。
    const page = Math.floor(row / BIG_PAGE);
    const cached = this._cache.get(this._key(page));
    const restore = () => {
      if (cached && cached.rows[row % BIG_PAGE] && col < cached.rows[row % BIG_PAGE].length) {
        cached.rows[row % BIG_PAGE][col] = oldValue;
      }
    };
    if (cached && cached.rows[row % BIG_PAGE]) {
      const cells = cached.rows[row % BIG_PAGE];
      if (col < cells.length) cells[col] = value;
    }
    try {
      const outcome = await this._invokeEdit("big_set_cell", { vrow: row, col, value });
      if (outcome?.changed) {
        // 编辑开始时未命中缓存会触发取页；旧页可能在 await 期间落定，
        // 因而成功时必须写回当前缓存，而不是只保留开始时捕获的页。
        const current = this._cache.get(this._key(page));
        const cells = current?.rows[row % BIG_PAGE];
        if (cells && col < cells.length) cells[col] = value;
        await this._refreshMeta();
        this._emitData(false);
      } else {
        restore();
      }
      return outcome;
    } catch (err) {
      restore();
      throw new Error(BIG_EDIT_ERR + "：" + err);
    }
  }

  /**
   * 将一组单元格修改作为一次用户操作写入引擎。
   *
   * @param {Array<[number, number, string]>} cells - [视图行, 列, 新值] 列表。
   * @returns {Promise<object>} 引擎 EditOutcome（含 changed/cancelled）。
   * @throws {Error} 筛选态、目标非法、快照超限或会话失败。
   */
  async setCells(cells) {
    if (this._filterOn) {
      throw new Error("筛选结果中不可编辑：请先清除筛选");
    }
    let outcome;
    try {
      outcome = await this._invokeEdit("big_set_cells", { cells });
    } catch (err) {
      // 新批量路径不做乐观写：引擎拒绝时既有缓存原样保留。
      throw new Error(BIG_EDIT_ERR + "：" + err);
    }
    return this._afterEdit(outcome);
  }

  /**
   * 粘贴不规则行块；既有格与末尾自动扩行只产生一条撤销记录。
   *
   * @param {number} row - 起始视图行。
   * @param {number} col - 起始列。
   * @param {string[][]} rows - 粘贴行块，短行保持稀疏语义。
   * @returns {Promise<object>} 引擎 EditOutcome（含 changed/insertedRows/cancelled）。
   * @throws {Error} 筛选态、位置/列非法、快照超限或会话失败。
   */
  /**
   * 将外部文本按 UTF-8 分块上传，再由引擎流式完成粘贴。
   * 固定点重试只复用 importId，不重复上传文本；未消费的导入统一取消。
   *
   * @param {string} text - 外部剪贴板文本。
   * @param {object} destination - PasteDestination。
   * @returns {Promise<object>} EditOutcome。
   */
  async pasteExternalText(text, destination) {
    let importId = null;
    let consumed = false;
    try {
      const begin = await this._invoke("big_paste_import_begin");
      importId = begin.importId;
      let sent = false;
      for (const chunk of utf8Chunks(text)) {
        sent = true;
        await this._invoke("big_paste_import_chunk", {
          importId,
          hex: bytesToHex(chunk.bytes),
          last: chunk.end === text.length,
        });
      }
      if (!sent) {
        await this._invoke("big_paste_import_chunk", { importId, hex: "", last: true });
      }
      const outcome = await this._invokeEdit("big_paste_import_finish", {
        importId, destination,
      });
      if (outcome.cancelled) return outcome;
      consumed = true;
      return await this._afterEdit(outcome, "edit");
    } finally {
      if (!consumed && importId !== null) {
        try {
          await this._invoke("big_paste_import_cancel", { importId });
        } catch {
          // 保留原始粘贴错误；清理失败由会话 spool 的最终清理兜底。
        }
      }
    }
  }

  /**
   * 直接粘贴当前进程的大复制 token，固定点重试复用同一 token。
   *
   * @param {string} token - 应用内剪贴板 token。
   * @param {object} destination - PasteDestination。
   * @returns {Promise<object>} EditOutcome。
   */
  async pasteClipboardToken(token, destination) {
    let outcome;
    try {
      outcome = await this._invokeEdit("big_paste_clipboard", { token, destination });
    } catch (err) {
      throw new Error(BIG_EDIT_ERR + "：" + err);
    }
    return this._afterEdit(outcome);
  }

  async pasteRows(row, col, rows) {
    if (this._filterOn) {
      throw new Error("筛选结果中不可编辑：请先清除筛选");
    }
    let outcome;
    try {
      outcome = await this._invokeEdit("big_paste", { vrow: row, col, rows });
    } catch (err) {
      throw new Error(BIG_EDIT_ERR + "：" + err);
    }
    return this._afterEdit(outcome);
  }

  /**
   * 将结构化清除目标作为一次用户操作交给引擎。
   *
   * @param {object} target - cells/rows/columns/rect 目标。
   * @returns {Promise<object>} 引擎 EditOutcome。
   * @throws {Error} 筛选态、固定点预检过期或引擎拒绝时。
   */
  async _clearTarget(target) {
    if (this._filterOn) {
      throw new Error("筛选结果中不可编辑：请先清除筛选");
    }
    let outcome;
    try {
      outcome = await this._invokeEdit("big_clear", { target });
    } catch (err) {
      throw new Error(BIG_EDIT_ERR + "：" + err);
    }
    return this._afterEdit(outcome);
  }

  /**
   * 清空一批稀疏单元格，整批只占一步撤销。
   *
   * @param {Array<[number, number]>} cells - [行, 列] 列表。
   * @returns {Promise<object>} 引擎 EditOutcome。
   */
  async clearCells(cells) {
    const seen = new Set();
    const unique = [];
    for (const [r, c] of cells) {
      const key = r * 1048576 + c;
      if (seen.has(key)) continue;
      seen.add(key);
      unique.push([r, c]);
    }
    return this._clearTarget({ kind: "cells", cells: unique });
  }

  /**
   * 清空若干整行；行列去重和旧值扫描由引擎完成。
   *
   * @param {number[]} rows - 视图行号列表。
   * @returns {Promise<object>} 引擎 EditOutcome。
   */
  async clearRows(rows) {
    return this._clearTarget({ kind: "rows", rows });
  }

  /**
   * 清空若干整列，不在前端生成行乘列的单元格数组。
   *
   * @param {number[]} cols - 视图列号列表。
   * @returns {Promise<object>} 引擎 EditOutcome。
   */
  async clearColumns(cols) {
    return this._clearTarget({ kind: "columns", cols });
  }

  /**
   * 清空连续矩形，直接发送范围而不是展开单元格。
   *
   * @param {number} row - 起始行。
   * @param {number} col - 起始列。
   * @param {number} rows - 行数。
   * @param {number} cols - 列数。
   * @returns {Promise<object>} 引擎 EditOutcome。
   */
  async clearRect(row, col, rows, cols) {
    return this._clearTarget({ kind: "rect", row, col, rows, cols });
  }

  /**
   * 在指定行前插入空行。
   *
   * @param {number} row - 插入位置（视图行号）。
   * @param {number} [count=1] - 行数。
   * @returns {Promise<void>}
   */
  async insertRows(row, count = 1) {
    this._requireNotIndexing("文件仍在索引中，暂不支持插入/删除行");
    const outcome = await this._invokeEdit("big_insert_rows", { vpos: row, rows: null, count });
    return this._afterEdit(outcome);
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
    this._requireNotIndexing("文件仍在索引中，暂不支持插入/删除行");
    const outcome = await this._invokeEdit("big_insert_rows", { vpos, rows, count: 0 });
    return this._afterEdit(outcome);
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
    this._requireNotIndexing("文件仍在索引中，暂不支持插入/删除行");
    const sorted = [...new Set(rows)].sort((a, b) => a - b);
    const outcome = await this._invokeEdit("big_remove_rows", { vrows: sorted });
    return this._afterEdit(outcome);
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
    if (!ok) return false;
    this._invalidate();
    await this._refreshMeta();
    this._emitData(true, "history");
    return true;
  }

  /**
   * 重做一步。
   *
   * @returns {Promise<boolean>} 是否生效。
   */
  async redo() {
    const ok = await this._invoke("big_redo");
    if (!ok) return false;
    this._invalidate();
    await this._refreshMeta();
    this._emitData(true, "history");
    return true;
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
   * 筛选态行号：命中序号 → 原始数据行号（0 基；随筛选分页 FilterPage 下发）。
   * 纯查缓存不触发拉取——未缓存/非筛选态返回 null（网格给空串占位，
   * 页落定后经 onData 重绘收敛）。
   *
   * @param {number} row - 命中序号（筛选视图行号）。
   * @returns {number | null} 原始数据行号；未缓存/非筛选态 null。
   */
  rowNumber(row) {
    if (!this._filterOn) return null;
    const cached = this._cache.get(this._key(Math.floor(row / BIG_PAGE)));
    return cached?.vrows?.[row % BIG_PAGE] ?? null;
  }

  /**
   * 执行一次筛选（引擎 rayon 全表扫描）。
   *
   * @param {Array<[number, string, number?]>} cols - (列号, 关键字, 模式) AND 组合；
   *   模式 0 包含 / 1 严格等于·不分大小写 / 2 严格等于·区分大小写
   *   （mode 2 关键字保持原样大小写，其余小写化）。
   * @param {number[]} multiCols - 多重筛选勾选列。
   * @param {string} multiText - 多重筛选关键字（小写）。
   * @returns {Promise<number | null>} 命中数；请求已过期/被引擎中断时返回 null。
   */
  async runFilter(cols, multiCols, multiText) {
    this._requireNotIndexing("文件仍在索引中，暂不支持筛选");
    const gen = ++this._filterGen;
    const spec = {
      cols: cols.map(([c, kw, mode]) => [
        c,
        (mode ?? 0) === 2 ? kw : kw.toLowerCase(), // mode 2 区分大小写：保持原样
        mode ?? 0,
      ]),
      multi_cols: multiCols,
      multi_text: multiText.toLowerCase(),
    };
    const n = await this._invoke("big_filter", { spec });
    // 引擎中断返回 null；或期间已发起更新的筛选/清空：旧结果一律不落视图
    if (n === null || n === undefined || gen !== this._filterGen) return null;
    this._filterOn = true;
    this._filterCount = n;
    this._filterView = {
      cols: cols.map((item) => item.slice()),
      multiCols: [...multiCols],
      multiText,
    };
    this._invalidate();
    // origin "fetch"：筛选只切视图不改正文——不得标脏
    this._emitData(true, "fetch");
    return n;
  }

  /**
   * 清除筛选，回到全量视图。
   *
   * @param {number | null} [hitRow] - 取消筛选锚点（命中序号）。给了就先查它在
   *   清筛后全量视图中的行号（必须先于 big_clear_filter——清后命中集即失），
   *   供网格把视口/选中锚回原来的行。
   * @returns {Promise<{applied: boolean, target: number | null}>}
   *   applied=false 表示期间被新筛选/清筛抢断（本次未生效）；target 为锚定目标
   *   行号（hitRow 为空/越界/被抢断时为 null）。
   */
  async clearFilter(hitRow = null) {
    const gen = ++this._filterGen;
    let target = null;
    if (hitRow !== null && hitRow !== undefined && this._filterOn) {
      target = await this._invoke("big_filter_hit_view_row", { idx: hitRow });
      // 查询期间被新筛选/清筛抢断：本次清筛作废（新请求才是现状）
      if (gen !== this._filterGen) return { applied: false, target: null };
    }
    await this._invoke("big_clear_filter");
    if (gen !== this._filterGen) return { applied: false, target: null }; // 同上
    this._filterOn = false;
    this._filterCount = 0;
    this._filterView = null;
    this._invalidate();
    // 同 runFilter：视图恢复，不标脏
    this._emitData(true, "fetch");
    return { applied: true, target };
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
    if (this.isHeaderless()) {
      throw new Error("无列名模式不可重命名列");
    }
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
    this._requireNotIndexing("文件仍在索引中，暂不支持列操作");
    await this._invoke("big_col_push", { op });
    await this._refreshMeta();
    this._emitData(true);
  }

  /**
   * 执行全部 pending 列操作，成功后产生一条可撤销 SchemaChange。
   *
   * @returns {Promise<object>} 引擎 EditOutcome。
   */
  async applyColOps() {
    this._requireNotIndexing("文件仍在索引中，暂不支持列操作");
    const outcome = await this._invokeEdit("big_col_apply");
    return this._afterEdit(outcome, "structure");
  }

  /**
   * 调保存命令；固定点确认只携带原预检代际重试一次。
   *
   * @param {string} cmd - big_save 或 big_save_as。
   * @param {object} [args] - 不含 approval 的命令参数。
   * @returns {Promise<boolean>} 仅引擎 saved=true 时返回 true。
   */
  async _invokeSave(cmd, args = {}) {
    const first = await this._invoke(cmd, {
      ...args,
      approval: { allowFixedPoint: false, preflightId: null },
    });
    if (first.saved) return true;
    if (!first.fixedPoint) return false;
    const confirmed = await this._confirmFixedPoint(first.fixedPoint);
    if (!confirmed) return false;
    const retry = await this._invoke(cmd, {
      ...args,
      approval: {
        allowFixedPoint: true,
        preflightId: first.fixedPoint.preflightId,
      },
    });
    if (retry.fixedPoint) throw new Error("固定点预检已过期，请重试");
    return retry.saved === true;
  }

  // ---- 保存 ----

  /**
   * 保存（引擎流式重写 + 原子换名，进度经 onProgress 订阅）。
   *
   * @returns {Promise<boolean>} true = 已保存；false = 固定点确认取消。
   */
  async save() {
    this._requireNotIndexing("文件仍在索引中，请稍候再保存");
    if (!(await this._invokeSave("big_save"))) return false;
    await this._afterWrite();
    return true;
  }

  /**
   * 另存为：把当前数据视图（含编辑/插入/删除/排序结果）写到 `target`。
   * 原文件保持不变；成功后引擎会话切到新路径，后续 save() 写新文件。
   *
   * @param {string} target - 目标 CSV 全路径。
   * @returns {Promise<boolean>} true = 已保存；false = 固定点确认取消。
   */
  async saveAs(target) {
    this._requireNotIndexing("文件仍在索引中，请稍候再另存为");
    if (!(await this._invokeSave("big_save_as", { path: target }))) return false;
    await this._afterWrite();
    return true;
  }

  /**
   * 写盘后的前端状态复位：引擎已重开文件并丢弃筛选，前端必须同步回
   * 全量视图，否则 rowCount 仍停在旧命中数、缓存仍是旧页。
   *
   * @returns {Promise<void>}
   */
  async _afterWrite() {
    this._filterGen += 1;
    this._filterOn = false;
    this._filterCount = 0;
    this._invalidate();
    await this._refreshMeta();
    // 保存是“落盘”而非内容修改：origin=view 让外壳保持干净，仅重建视图。
    this._emitData(true, "view");
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
   * @returns {Promise<{kind: string, text?: string, bytes?: number, meta?: object}>} CopyOutcome。
   */
  async copyRows(r0, r1, c0, c1) {
    return this._invoke("big_copy", { r0, r1, c0, c1 });
  }

  /** @returns {Promise<object|null>} 当前进程内大复制元数据。 */
  async clipboardMeta() {
    return this._invoke("big_clipboard_meta");
  }

  /** @returns {Promise<void>} 使当前应用内复制 token 失效。 */
  async clipboardClear() {
    return this._invoke("big_clipboard_clear");
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
