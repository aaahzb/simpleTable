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

/** 进程内会话号分配器：每个 BigModel 独占一个引擎会话（多页签互不干扰）。 */
let nextSessionId = 1;

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
    /** @type {Function} 原始 Tauri 调用（_invoke 负责附加会话号） */
    this._rawInvoke = tauri.invoke;
    /** @type {number} 本模型独占的引擎会话号 */
    this._sid = nextSessionId;
    nextSessionId += 1;
    /** @type {Function} */
    this._listen = tauri.listen;
    /** @type {number} 索引进度轮询周期 ms（测试可注入更短值） */
    this._pollMs = tauri.pollMs ?? 200;
    /** @type {object | null} 最近一次的 big_meta 缓存 */
    this._meta = null;
    /** @type {Map<string, string[][]>} 页缓存，键 `${"f"|""}${pageIdx}` */
    this._cache = new Map();
    /** @type {Set<string>} 正在拉取的页 */
    this._inflight = new Set();
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
  }

  /**
   * 本模型独占的引擎会话号（调试/诊断用；命令调用请走 _invoke）。
   *
   * @returns {number} 会话号。
   */
  get sessionId() {
    return this._sid;
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
    return this._rawInvoke(cmd, { session: this._sid, ...args });
  }

  // ---- 生命周期 ----

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
    this._session += 1;
    this._openT0 = performance.now();
    this._firstPaintMs = 0;
    this._meta = await this._invoke("big_open", { path, headerless: !!headerless });
    this._invalidate();
    this._emitReset();
    this._emitData(true);
    this._emitUndo();
    this._emitProgress();
    this._startIndexPolling();
    return this._meta;
  }

  /**
   * 关闭大文件会话（打开新文件/退出时）。
   *
   * @returns {Promise<void>}
   */
  async close() {
    this._stopIndexPolling();
    this._session += 1;
    await this._invoke("big_close");
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
   * 引擎同时清空筛选/撤销/列缓存；本模型同步丢弃窗口缓存与筛选态。
   * 开关属于视图属性：以 origin="view" 通知，不标脏。
   *
   * @param {boolean} on - true=首行不是列名；false=首行是列名。
   * @returns {Promise<void>}
   * @throws {Error} 引擎拒绝（无行可提升列名、待执行列操作等）。
   */
  async setHeaderless(on) {
    const want = !!on;
    if (want === this.isHeaderless()) {
      return;
    }
    this._meta = await this._invoke("big_set_headerless", { on: want });
    // 引擎已清空筛选；让迟到响应作废，前端视图同步回全量。
    this._filterGen += 1;
    this._filterOn = false;
    this._filterCount = 0;
    this._invalidate();
    this._emitData(true, "view");
    this._emitUndo();
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
    return this._listen("big-progress", (e) =>
      cb(e.payload[0], e.payload[1], e.payload[2], e.payload[3]));
  }

  /**
   * 订阅排序模式载入进度（事件带会话号，只接收本模型的）。
   *
   * @param {(done: number, total: number) => void} cb - 回调（字节）。
   * @returns {Promise<Function>} 取消监听函数。
   */
  async onSortProgress(cb) {
    return this._listen("big-sort-progress", (e) => {
      const [done, total, sid] = e.payload;
      if (sid === this._sid) cb(done, total);
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
   * 排序是物理保存的准备动作：发出结构通知（origin="sort"），由 app 标脏。
   * 已激活的筛选结果会随排序重新定位，成员不变。
   *
   * @param {Array<{col: number, numeric: boolean, asc: boolean}>} levels - 排序级别。
   * @returns {Promise<object>} 最新 BigMeta（sorted=true）。
   */
  async sort(levels) {
    const meta = await this._invoke("big_sort_apply", { levels });
    this._meta = meta;
    this._invalidate();
    this._emitData(true, "sort");
    this._emitUndo();
    return meta;
  }

  /**
   * 退出排序视图；默认同时释放私有内存缓冲、回到 mmap 视图。
   *
   * @param {{releaseRam?: boolean}} [opts] - releaseRam=false 时保留内存后备。
   * @returns {Promise<object>} 最新 BigMeta（sorted=false）。
   */
  async exitSort({ releaseRam = true } = {}) {
    const meta = await this._invoke("big_sort_exit", { releaseRam });
    this._meta = meta;
    this._invalidate();
    this._emitData(true, "view");
    this._emitUndo();
    return meta;
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
    return this._cell(row, col);
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
    const rows = this._cache.get(this._key(Math.floor(row / BIG_PAGE)));
    return !!rows?.[row % BIG_PAGE];
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
    const rows = this._cache.get(this._key(Math.floor(row / BIG_PAGE)));
    if (!rows) return undefined;
    const cells = rows[row % BIG_PAGE];
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
    // 拉取时的行数上限：页尾是否已索引，决定这一页落定后是否算"部分页"
    const atRows = this._filterOn ? this._filterCount : Number(this._meta?.rows ?? 0);
    const req = this._filterOn
      ? this._invoke("big_filter_page", { from, count: BIG_PAGE })
      : this._invoke("big_rows", { from, count: BIG_PAGE });
    req.then((rows) => {
      // 结构已变化（代际不同/筛选态翻转）→ 旧响应作废
      if (gen !== this._gen || this._key(page) !== key) return;
      const fullAtFetch = from + BIG_PAGE <= atRows;
      if (!fullAtFetch) {
        const nowRows = this._filterOn ? this._filterCount : Number(this._meta?.rows ?? 0);
        if (nowRows > atRows) {
          // 飞行期间行数增长：本页响应缺尾部行，丢弃后按新行数重拉
          this._inflight.delete(key);
          this._schedule(page);
          return;
        }
        // 页尾尚未索引：登记为部分页，后续行数增长时整体失效
        this._partial.add(key);
      }
      this._cache.set(key, rows);
      this._trim(page);
      this._emitData(false, "fetch"); // 纯预取落定：重绘即可，不得标脏
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
      if (p < around - 2 || p > around + 2) {
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
    this._partial.clear();
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
    this._requireNotIndexing("文件仍在索引中，暂不支持插入/删除行");
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
    this._requireNotIndexing("文件仍在索引中，暂不支持插入/删除行");
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
    this._requireNotIndexing("文件仍在索引中，暂不支持插入/删除行");
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
   * @returns {Promise<number | null>} 命中数；请求已过期/被引擎中断时返回 null。
   */
  async runFilter(cols, multiCols, multiText) {
    this._requireNotIndexing("文件仍在索引中，暂不支持筛选");
    const gen = ++this._filterGen;
    const spec = {
      cols: cols.map(([c, kw]) => [c, kw.toLowerCase()]),
      multi_cols: multiCols,
      multi_text: multiText.toLowerCase(),
    };
    const n = await this._invoke("big_filter", { spec });
    // 引擎中断返回 null；或期间已发起更新的筛选/清空：旧结果一律不落视图
    if (n === null || n === undefined || gen !== this._filterGen) return null;
    this._filterOn = true;
    this._filterCount = n;
    this._invalidate();
    // origin "fetch"：筛选只切视图不改正文——不得标脏（对齐小模式筛选行为）
    this._emitData(true, "fetch");
    return n;
  }

  /**
   * 清除筛选，回到全量视图。
   *
   * @returns {Promise<void>}
   */
  async clearFilter() {
    const gen = ++this._filterGen;
    await this._invoke("big_clear_filter");
    if (gen !== this._filterGen) return; // 期间又发起了新筛选：以新请求为准
    this._filterOn = false;
    this._filterCount = 0;
    this._invalidate();
    // 同 runFilter：视图恢复，不标脏
    this._emitData(true, "fetch");
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
   * 执行全部 pending 列操作（物理重写整个文件，带进度事件）。
   *
   * @returns {Promise<void>}
   */
  async applyColOps() {
    this._requireNotIndexing("文件仍在索引中，暂不支持列操作");
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
    this._requireNotIndexing("文件仍在索引中，请稍候再保存");
    await this._invoke("big_save");
    // 引擎写盘后会重新打开文件并丢弃筛选（排序保存则已按显示顺序落盘）。
    // 前端必须同步回全量视图，否则 rowCount 仍停在旧命中数、缓存仍是旧页。
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
