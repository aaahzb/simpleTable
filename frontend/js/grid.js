/**
 * 虚拟网格视图：两带式表头 + 可视行渲染 + 选区模型 + 格内编辑器 +
 * 列宽拖拽 + 列拖动排序 + 剪贴板 + Ctrl+滚轮缩放。
 *
 * 对齐 Qt 版 ZoomableTableView + FilterHeaderView 行为：
 * - 行虚拟滚动（等行高 O(1) 定位 + 虚拟缩放），列全量渲染（列数通常远小于行数）
 * - 选区 = 若干矩形块 + 当前格；Ctrl 多块、Shift 块选、拖选、行列头整选
 * - 编辑键位：Enter 下移 / Shift+Enter 上移 / Tab 右移 / Shift+Tab 左移 /
 *   Alt+Enter 换行 / Esc 取消 / 打字即编辑（替换式）
 * - 缩放 0.5–3.0 步进 1.15：列宽按比缩放（最小 24），行高按比缩放（最小 10），
 *   表头/筛选带随 em 自动跟随
 * - 统一大引擎：visibleRows 恒等快路径（null = 无筛选，不建数组）、
 *   统一行高（行高数组不随总行数膨胀）、筛选走引擎整表扫描（键入即筛）、
 *   大选区复制/矩形选区大小设防（防 30M 行选整列卡死）
 */

import { LARGE_TEXT_BYTES, isSingleValueText, utf8ByteLength } from "./clipboard.js";
import { parseCsv, serializeRecords } from "./csv.js";

export const ZOOM_MIN = 0.5;
export const ZOOM_MAX = 3.0;
export const ZOOM_STEP = 1.15;
const MIN_COL_W = 24;
const MIN_ROW_H = 10;
/** 严格三态开关符号：0 包含 / 1 严格等于·不分大小写 / 2 严格等于·区分大小写 */
const STRICT_GLYPHS = ["~", "=", "≡"];
/** 严格三态按钮悬停文案（与 STRICT_GLYPHS 同序） */
const STRICT_TITLES = [
  "包含匹配（默认）——点击切换为「严格等于·不分大小写」",
  "严格等于·不分大小写——点击切换为「严格等于·区分大小写」",
  "严格等于·区分大小写——点击切回「包含」",
];

/**
 * 画布压缩上限（纵向虚拟缩放，见 _yScale）：4,000,000px。
 * 两条 WebView2 实测绘制墙决定不能更大：
 * 1. 元素高度钳制 16,777,214px（2^24-2）——画布本身别超限；
 * 2. 滚动位置墙（截屏实测）——scrollTop 超 ~4.19M 后整层内容被剔除画空白，
 *    且墙随画布高度上浮（4.6M 画布在 4.185M~4.192M 间开始空白，3,999,246px
 *    画布任意深度含最大 scrollTop 全部正常绘制）。
 * 把画布压到 4M 以下，scrollTop 最大值 ≈ 4M-视口高，永远撞不到墙。
 * 压缩后滚动条天然等价于"百分比定位"：拖到 53.1% 即 round(总行数×53.1%) 行，
 * 1px 步进 ≈ 总行数/4M（1500 万行时 ≈ 3.9k 行 ≈ 0.025%），滚轮仍按行推进。
 */
const Y_CLAMP = 4_000_000;

/**
 * 网格视图类。
 */
export class Grid {
  /**
   * @param {object} opts - 构造参数。
   * @param {import("./bigmodel.js").BigModel} opts.model - 数据模型。
   * @param {HTMLElement} opts.scrollEl - 滚动容器（#grid-scroll）。
   * @param {HTMLElement} opts.canvasEl - 画布（#grid-canvas）。
   * @param {HTMLElement} opts.headerEl - 列头容器（#col-header）。
   * @param {HTMLElement} opts.rowsEl - 行容器（#rows）。
   * @param {HTMLElement} opts.editorEl - 格内编辑器（#cell-editor）。
   * @param {object} opts.hooks - 与外壳（app.js）的回调集合。
   * @param {(msg: string, ms?: number) => void} opts.hooks.statusMsg -
   *   状态栏瞬时消息。
   * @param {(visible: number, total: number) => void} opts.hooks.countsChanged -
   *   筛选后行数变化。
   * @param {(r: number, c: number) => void} opts.hooks.currentChanged -
   *   当前格移动（编辑窗格同步）。
   * @param {(zoom: number) => void} opts.hooks.zoomed - 表格缩放因子变化。
   * @param {(r: number, c: number) => string} opts.hooks.cellMenu -
   *   单元格右键菜单（由 app 提供，避免循环依赖）。
   * @param {(r: number, e: MouseEvent) => string} opts.hooks.rowMenu -
   *   行头右键菜单。
   * @param {(c: number, e: MouseEvent) => string} opts.hooks.colMenu -
   *   列头右键菜单。
   * @param {HTMLElement} [opts.areaEl] - --grid-zoom 挂载面（分屏副网格传副侧；
   *   缺省 #grid-area）。
   */
  constructor({ model, scrollEl, canvasEl, headerEl, rowsEl, editorEl, areaEl, hooks }) {
    this.model = model;
    this.scrollEl = scrollEl;
    this.canvasEl = canvasEl;
    this.headerEl = headerEl;
    /**
     * 表头左上角块（行号列 × 列名带的空方块）。
     * 表头重建必须保留它：主/副网格的 id 不同（corner / corner2），
     * 不能按固定 id 判断，否则副网格的角块会被吞掉、列名整体左移。
     * @type {HTMLElement}
     */
    this.cornerEl = headerEl.querySelector(".corner");
    if (!this.cornerEl) {
      // 兜底：旧 DOM 或测试夹具缺角块时补一个，保证两带表头结构完整
      this.cornerEl = document.createElement("div");
      this.cornerEl.className = "corner";
      headerEl.prepend(this.cornerEl);
    }
    this.rowsEl = rowsEl;
    this.editorEl = editorEl;
    // --grid-zoom 的挂载面（分屏时两侧各自缩放；缺省回退全局 #grid-area）
    this.areaEl = areaEl ?? document.getElementById("grid-area");
    this.hooks = hooks;

    this.zoom = 1;
    /** @type {number[]} 每列宽度（当前缩放下 px，按源列号索引） */
    this.colW = [];
    /**
     * 纵向虚拟缩放：画布真实总行高超过 Y_CLAMP 时为 真实行高/压缩后行高，否则为 1。
     * 压缩后滚动条坐标 = 真实坐标 / _yScale，行位置以当前滚动位置锚定（见 _rowTop/_rowAt）。
     * @type {number}
     */
    this._yScale = 1;
    /**
     * 纵向滚动范围（scrollTop 上限）与一屏可容纳的整数行数。
     * 每次 _layout 按画布显式高度重算；压缩滚动按整个滚动范围把行号等比映射，
     * 保证滚到底就是最后一行（不能再按全局真实坐标/scale 直接换算，否则末尾
     * 一屏永远差 viewH×scale/h 行）。
     * @type {number}
     */
    this._maxScroll = 0;
    /** @type {number} 一屏行数 floor(clientHeight / 行高)，至少 1 */
    this._rowSpan = 1;
    /** @type {number[]} 列顺序（源列号数组，位置即显示顺序） */
    this.colOrder = [];
    /** @type {Map<number, string>} 列号 → 筛选关键字 */
    this.filters = new Map();
    /** @type {Map<number, number>} 列号 → 严格模式（1 等于·不分大小写 /
     * 2 等于·区分大小写；0 不入图） */
    this.filterModes = new Map();
    /** 多重筛选状态：on 闩锁（长期选中）；text 关键字；cols 参与"或"匹配的源列号集合 */
    this.multi = { on: false, text: "", cols: new Set() };
    /** @type {number[] | null} 可见源行号（筛选后）；null = 恒等快路径（无筛选） */
    this.visibleRows = null;
    /** @type {number} 统一行高 px（rebuildStructure 计算） */
    this._uniformRowH = 20;
    /** @type {number} 上一次布局的可见行数（渐进索引行数增长判定） */
    this._lastVisCount = 0;
    /** @type {boolean} 选区截断提示已发（防抖） */
    this._selCapNotified = false;
    /** @type {{current: [number, number]|null, anchor: [number, number]|null,
     *   blocks: number[][]}} 选区（源坐标，块为 [r1,c1,r2,c2] 已归一） */
    this.sel = { current: null, anchor: null, blocks: [] };
    /** @type {Set<number>} 选中单元格集合（r * colCount + c） */
    this.selectedSet = new Set();
    /** @type {number} Ctrl+↑/↓ 异步查找代际：后续按键/换模型令旧结果作废 */
    this._navSeq = 0;
    /** @type {{r: number, c: number} | null} 编辑中的单元格 */
    this.editing = null;
    /** @type {Map<number, string>} 等行高键控复用的旧内容兜底（stale-while-revalidate） */
    this._staleText = new Map();
    /** @type {string} 行结构形状键（列数/列序/列宽），变化即整窗重建 */
    this._shapeKey = "";
    /** @type {string} 表头 DOM 重建签名（列序/列名/宽度/多重筛选），未变不重建 */
    this._headerKeyCache = "";
    /** @type {string} 最近一次本软件复制的文本（内部分区粘贴判据） */
    this.lastCopiedText = "";
    this._suppressFilterInput = false;
    this._drag = null;

    this._bindEvents();
    this._observeResize();
    model.onData((structural) => this._onModelData(structural));
    this.rebuildStructure();
  }

  // ============ 几何 ============

  /**
   * 行号列实际像素宽：按当前总行数的位数足以完整显示行号。
   *
   * 行号列（.row-head）在 flex 布局里默认 min-width:auto，长行号会把
   * 它撑得比表头角块宽，数据列随之外移，视觉上列名整体偏左。这里按
   * 最大行号文本实测宽度给出固定 px（同时经 --row-header-w 下发给角块
   * 与行号列），并用 CSS min-width:0 禁止内容再撑宽。
   *
   * @returns {number} 行号列宽（px，含 1px 余量）。
   */
  _rowHeadW() {
    const area = this.canvasEl.parentElement.parentElement;
    const cs = getComputedStyle(area);
    const fontPx = parseFloat(cs.fontSize);
    const base = Math.round(3.2 * fontPx);
    const total = Math.max(1, Number(this.model.rowCount?.() ?? 0));
    const label = String(total);
    if (!this._headCtx) this._headCtx = document.createElement("canvas").getContext("2d");
    if (!this._headCtx) return base; // 无 2D 上下文时退回固定宽
    const key = `${fontPx}|${cs.fontFamily}|${cs.fontWeight}|${label}`;
    if (this._headMeasureKey !== key) {
      // 行号列 font-size 是 0.92em（见 index.html），按同一字体测量
      this._headCtx.font =
        `${cs.fontStyle} ${cs.fontWeight} ${0.92 * fontPx}px ${cs.fontFamily}`;
      this._headMeasure = this._headCtx.measureText(label).width;
      this._headMeasureKey = key;
    }
    // +2px 吸收 measureText 与 DOM 实际排版/取整的细微差异
    return Math.max(base, Math.ceil(this._headMeasure) + 2);
  }

  /**
   * 滚动区真实内容盒（可视区）尺寸，向下取整。
   *
   * clientWidth/clientHeight 返回四舍五入后的整数：真实 761.5px 会报 762。
   * 若把画布按 762 设置，画布就比可视区高 0.5px → 出现滚动条；滚动条又
   * 改变 client 尺寸并触发 ResizeObserver 重排，下一帧画布缩小、滚动条
   * 消失……尺寸逐帧翻转，表现就是空闲时表格区域狂闪。
   *
   * 这里用 getBoundingClientRect 的小数边框盒尺寸减去「边框 + 滚动条」
   * 厚度（offset - client），再向下取整，保证画布永远不超过真实可视区。
   *
   * @returns {{w: number, h: number}} 可视区宽高（px，非负整数）。
   */
  _viewportSize() {
    const el = this.scrollEl;
    const rect = el.getBoundingClientRect();
    return {
      w: Math.max(0, Math.floor(rect.width - (el.offsetWidth - el.clientWidth))),
      h: Math.max(0, Math.floor(rect.height - (el.offsetHeight - el.clientHeight))),
    };
  }

  /**
   * 热交换模型（装载/新建/切档）：订阅新模型数据通知并重建设置。
   * 旧模型随订阅一并废弃（其后再无改动来源，残留闭包无副作用）。
   *
   * @param {object} m - 新模型（BigModel）。
   * @returns {void}
   */
  setModel(m) {
    this.model = m;
    this.visibleRows = null;
    // 旧模型的行元素与旧内容兜底不得复用到新模型（行号会撞）
    this.rowsEl.textContent = "";
    this._staleText.clear();
    this._shapeKey = "";
    this._headerKeyCache = "";
    m.onData((structural) => this._onModelData(structural));
    this.rebuildStructure();
  }

  /** 重算列顺序与默认列宽（结构重建时） */
  rebuildStructure() {
    const n = this.model.columnCount();
    this.colOrder = Array.from({ length: n }, (_, i) => i);
    // 新列给 Qt 默认宽 ~100px；保留已有列宽
    const kept = this.colW;
    this.colW = Array.from({ length: n }, (_, i) =>
      i < kept.length ? kept[i] : Math.round(100 * this.zoom));
    // 统一行高（30M 行也不建行高数组）
    this._uniformRowH = Math.max(
      MIN_ROW_H, Math.round(1.8 * parseFloat(getComputedStyle(this.scrollEl).fontSize)));
    this._rebuildFilterInputs();
    // 列结构变化：多重筛选的勾选列重置为全勾（默认勾选，对齐普通筛选清空的策略）
    this.multi.cols = new Set(this.colOrder);
    this._refilter();
    this._clampSelection();
    this.renderAll();
  }

  /** @returns {number} 可见行数（恒等快路径直接取模型行数） */
  visCount() {
    return this.visibleRows ? this.visibleRows.length : this.model.rowCount();
  }

  /**
   * 可见行序号 → 源行号（恒等快路径 O(1) 直通）。
   *
   * @param {number} vi - 可见行序号。
   * @returns {number} 源行号。
   */
  visAt(vi) {
    return this.visibleRows ? this.visibleRows[vi] : vi;
  }

  /**
   * 源行号 → 可见行序号（无筛选时恒等；越界 -1）。
   *
   * @param {number} r - 源行号。
   * @returns {number} 可见行序号或 -1。
   */
  visibleIndexOf(r) {
    if (this.visibleRows) return this.visibleRows.indexOf(r);
    return r < this.model.rowCount() ? r : -1;
  }

  /**
   * 行高（统一大引擎：恒为统一行高）。
   *
   * @returns {number} 行高 px。
   */
  _rowH() {
    return this._uniformRowH;
  }

  /**
   * 重算可见行布局与画布尺寸（筛选后画布只含可见行，无空隙）。
   *
   * 统一行高：直接按 行数×行高 计算；真实总行高超过 Y_CLAMP 时启用纵向
   * 虚拟缩放，把画布高度压到上限以内（等行高保证缩放映射精确，无累积误差）。
   */
  _layout() {
    const n = this.visCount();
    this._lastVisCount = n;
    const oy = this._originY();
    const vp = this._viewportSize();
    const viewH = vp.h;
    const rowHeadW = this._rowHeadW();
    // 行号列宽统一由 JS 取整后以 px 下发：CSS 的 3.2em 在表头角块（13px）
    // 与行号（0.92em ≈ 11.96px）里会解析出两个不同宽度（41.6 vs 38.3），
    // 造成数据列整体比表头左移。px 变量让角块/行号列/JS 几何完全一致。
    this.areaEl.style.setProperty("--row-header-w", `${rowHeadW}px`);
    const totalW = rowHeadW + this._effWidths().reduce((a, b) => a + b, 0);
    this.canvasEl.style.width = `${Math.max(totalW, vp.w)}px`;
    const h = this._uniformRowH;
    const real = n * h;
    this._yScale = real > Y_CLAMP ? real / Y_CLAMP : 1;
    const compressed = real / this._yScale;
    this._rowSpan = Math.max(1, Math.floor(viewH / h));
    // 画布高 = 吸顶表头 + 全部行（行是 #rows 内绝对定位，#rows 紧随表头之后）
    this.canvasEl.style.height = `${Math.max(oy + compressed, viewH)}px`;
    this._maxScroll = Math.max(0, oy + compressed - viewH);
  }

  /** @returns {number} 行区起始 y（吸顶列头高度，行坐标系与画布坐标系的偏移）。 */
  _originY() { return this.headerEl.offsetHeight; }

  /**
   * 计算每列有效宽度（最后一列拉伸填满视口，对齐 setStretchLastSection）。
   *
   * @returns {number[]} 按显示位置排列的有效宽度。
   */
  _effWidths() {
    const widths = this.colOrder.map((c) => this.colW[c]);
    if (widths.length) {
      const avail = this._viewportSize().w - this._rowHeadW();
      const sum = widths.reduce((a, b) => a + b, 0);
      if (sum < avail) {
        widths[widths.length - 1] += avail - sum;
      }
    }
    return widths;
  }

  /**
   * 行顶 y（行区坐标，不含吸顶表头偏移）。
   *
   * 统一行高：vi×行高/_yScale（_yScale 只是把坐标压进 Y_CLAMP，映射全局精确；
   * 未缩放时退化为 vi×行高，逐像素平滑）。
   *
   * 注意：返回值可远超 2^22（WebView2 实测：绝对定位子元素偏移 ~4.2M px 即不绘制）。
   * 行元素自身绝不直接用它定位——_renderRows 做窗口局部化（行内偏移 ≤ 视口高），
   * 大偏移只经 #rows 容器的 transform 表达。
   *
   * @param {number} vi - 可见行序号。 @returns {number} 行顶 px（行区全局坐标）。
   */
  _rowTop(vi) {
    return (vi * this._uniformRowH) / this._yScale;
  }

  /**
   * 行区 y 坐标 → 可见行序号（夹取）。
   *
   * 等行高：按渲染锚点换算——行以真实行高连续画在画布上、锚点行对齐视口顶，
   * 而滚动条坐标经 _yScale 压缩（1 滚动条 px ≈ scale 真实 px），
   * 必须经锚点还原才与 _renderRows 的画法一致（全局逆映射 floor(y·scale/h)
   * 会把窗口拉成 ~视口×scale/h 行、且与真实行高渲染重叠）。
   *
   * @param {number} y - 行区 y 坐标。 @returns {number} 可见行序号（夹取）。
   */
  _rowAt(y) {
    const { sv, anchor, h } = this._yAnchor();
    const n = this.visCount();
    return Math.max(0, Math.min(n - 1, anchor + Math.floor((y - sv) / h)));
  }

  /**
   * 当前滚动位置的渲染锚点。
   *
   * 行以真实行高连续排布、锚点行对齐视口顶；滚动条坐标经 _yScale 压缩，
   * 锚点 = round(行区滚动位置×_yScale/行高)。_renderRows/_rowAt/_paintTop 共用，
   * 保证渲染、命中测试、编辑器定位同一几何。
   *
   * @returns {{sv: number, anchor: number, h: number, pos: number}}
   *   行区滚动位置/锚点行（整数）/行高/小数锚点位置（索引恢复不动点用）。
   */
  _yAnchor() {
    const h = this._uniformRowH;
    // 渐进索引期间模型行数会先增长、renderAll/_layout 才更新 _yScale/_maxScroll：
    // 此刻必须继续按"上一次布局的行数"换算锚点，否则同一 scrollTop 会被新行数
    // 放大（CI 实测 50,000 → 79,470 的跳走）。首次布局前 _lastVisCount 为 0，
    // 回退当前可见行数。
    const n = this._lastVisCount > 0 ? this._lastVisCount : this.visCount();
    const oy = this._originY();
    const rawSv = Math.max(0, this.scrollEl.scrollTop - oy);
    if (this._yScale > 1) {
      // 压缩坐标按整个滚动范围等比映射到 0..n-一屏行数：
      // scrollTop 到底 ⇔ 锚点为 n-_rowSpan，最后一行正好落在视口底部。
      const maxSv = Math.max(0, this._maxScroll - oy);
      const span = Math.max(0, n - this._rowSpan);
      const sv = Math.min(rawSv, maxSv);
      // 贴底吸附：浏览器实际可滚动上限 = scrollHeight - 视口（小数边框盒），
      // 可能比 _maxScroll 少至多 1px；压缩坐标下 1 滚动 px ≈ _yScale 行，
      // 按比例四舍五入会停在 span 之前（1550 万行实测差 2 行），末尾行被画
      // 到视口下缘之外，用户滚到底也永远看不见。
      const atBottom = maxSv > 0 && maxSv - sv < 1;
      // pos：小数锚点（行单位）。索引恢复用 pos 做不动点换算——若捕获时
      // 就量化成整数行，每次进度事件都会损失子行位置并累积漂移。
      const pos = maxSv > 0
        ? (atBottom ? span : Math.min(span, (sv / maxSv) * span))
        : 0;
      const anchor = Math.max(0, Math.min(n - 1, Math.round(pos)));
      return { sv, anchor, h, pos };
    }
    // 未压缩路径同理：贴底时 rawSv/h 的小数部分会把锚点四舍五入到下一行，
    // 末行下缘最多超出视口近一行。吸附到 n-_rowSpan，末行整体可见。
    const maxSv = Math.max(0, this._maxScroll - oy);
    const pos = maxSv > 0 && rawSv >= maxSv - 1
      ? Math.max(0, n - this._rowSpan)
      : Math.max(0, Math.min(n - 1, rawSv / h));
    return { sv: rawSv, anchor: Math.round(pos), h, pos };
  }

  /**
   * 行 vi 实际画在画布上的 y（含表头偏移；等行高真实几何，锚点语义）。
   *
   * 渲染几何（真实行高）与滚动条坐标系（_yScale 压缩）不同，
   * 编辑器定位等"画在哪"场景必须用本函数，不能用全局虚拟的 _rowTop。
   *
   * @param {number} vi - 可见行序号。 @returns {number} 画布 y px。
   */
  _paintTop(vi) {
    const { sv, anchor, h } = this._yAnchor();
    return this._originY() + sv + (vi - anchor) * h;
  }

  /**
   * 行号列文本：筛选态显示原始数据行号（1 起——命中的内容散布文件
   * 各处，行号应跳号而非 1,2,3… 连编）；页未缓存给空串占位，落定后同帧
   * 收敛。其余 = 行索引+1。
   *
   * @param {number} r - 源行号（筛选态=命中序号）。
   * @returns {string}
   */
  _rowHeadText(r) {
    if (this.model.filterActive?.()) {
      const rn = this.model.rowNumber(r);
      return rn === null || rn === undefined ? "" : String(rn + 1);
    }
    return String(r + 1);
  }

  /**
   * 锚点行对齐视口顶部（吸顶表头之下）所需的 scrollTop。
   *
   * 压缩滚动（_yScale > 1）按 _yAnchor 的等比映射反解；未压缩时行区坐标
   * 就是滚动坐标。返回值已夹取到滚动范围内。
   *
   * @param {number} anchor - 锚点可见行序号。
   * @returns {number} scrollTop px。
   */
  _scrollTopForAnchor(anchor) {
    if (anchor <= 0) return 0;
    const oy = this._originY();
    if (this._yScale <= 1) {
      // 与 _yAnchor 未压缩分支严格互逆（rawSv = scrollTop - oy）；
      // 既有调用方都包在 yScale>1 分支里，此分支只服务索引锚点恢复。
      return Math.max(0, Math.min(this._maxScroll, oy + this._rowTop(anchor)));
    }
    const maxSv = Math.max(0, this._maxScroll - oy);
    if (maxSv <= 0) return 0;
    const span = Math.max(1, this.visCount() - this._rowSpan);
    // 不取整：浏览器接受小数 scrollTop；取整是纯精度损失，会让索引恢复
    // 在每个进度事件上累积量化漂移。
    return Math.max(0, Math.min(this._maxScroll, oy + (anchor / span) * maxSv));
  }

  /**
   * 取消筛选前的视口锚点快照（须在坐标系切换前捕获）。
   *
   * 锚 = 选中行（选中存源坐标，筛选态源坐标即命中序号）；无选中锚视口顶行。
   * delta = k − pos（含行内小数——恢复时行内偏移精确保留）。
   *
   * @returns {{k: number, col: number | null, delta: number} | null}
   *   k=命中序号锚；col=选中列（无选中 null）；delta=视口内行偏移（小数）。
   */
  _captureClearAnchor() {
    if (!this.visCount()) return null;
    const pos = this._yAnchor().pos;
    let k = Math.floor(pos);
    let col = null;
    const cur = this.sel?.current;
    if (cur) {
      // 筛选态 visAt 恒等，选中源坐标即命中序号
      k = cur[0];
      col = cur[1];
    }
    return { k, col, delta: k - pos };
  }

  /**
   * 取消筛选后的锚点恢复：target 行回到捕获时的视口内位置（delta 含行内
   * 小数；_scrollTopForAnchor 不取整，压缩/未压缩坐标系通吃）。有选中列时
   * select remap（联动 hooks.currentChanged → 编辑窗格；多块选区收敛单格，
   * 严格优于回顶现状），否则仅重绘。
   *
   * @param {{k: number, col: number | null, delta: number}} cap - 捕获的锚点。
   * @param {number} target - 锚在清筛后全量视图中的行号（0 基）。
   * @returns {void}
   */
  _restoreClearAnchor(cap, target) {
    const posG = Math.max(0, target - cap.delta);
    this.scrollEl.scrollTop = this._scrollTopForAnchor(posG);
    if (cap.col !== null && cap.col !== undefined) {
      this.select([target, cap.col]);
    } else {
      this._renderRows();
    }
  }

  /**
   * 目标行完整贴在视口底部所需的 scrollTop。
   *
   * @param {number} vi - 可见行序号。
   * @returns {number} scrollTop px。
   */
  _scrollTopForRowBottom(vi) {
    const oy = this._originY();
    const viewH = this._viewportSize().h;
    if (this._yScale > 1) {
      return this._scrollTopForAnchor(Math.max(0, vi - this._rowSpan + 1));
    }
    const rh = this._uniformRowH;
    return Math.max(0, Math.min(this._maxScroll,
      this._rowTop(vi) + rh - (viewH - oy)));
  }

  /**
   * 目标行完整露在吸顶表头下方所需的 scrollTop。
   *
   * @param {number} vi - 可见行序号。
   * @returns {number} scrollTop px。
   */
  _scrollTopForRowTop(vi) {
    if (this._yScale > 1) {
      const h = this._uniformRowH;
      const headRows = Math.max(1, Math.ceil(this._originY() / h));
      return this._scrollTopForAnchor(Math.max(0, vi - headRows));
    }
    return Math.max(0, Math.min(this._maxScroll, this._rowTop(vi)));
  }

  /**
   * 列左缘 x（含行号列偏移，按显示位置）。
   *
   * @param {number} pos - 显示位置。 @returns {number} x 像素。
   */
  _colLeft(pos) {
    const w = this._effWidthsCache;
    let x = this._rowHeadW();
    for (let i = 0; i < pos; i += 1) x += w[i];
    return x;
  }

  /**
   * @param {number} x - 画布 x 坐标。 @returns {number} 显示位置。
   */
  _posAt(x) {
    const w = this._effWidthsCache;
    let left = this._rowHeadW();
    for (let i = 0; i < w.length; i += 1) {
      if (x < left + w[i]) return i;
      left += w[i];
    }
    return w.length - 1;
  }

  // ============ 渲染 ============

  /** 全量重绘（表头 + 画布） */
  renderAll() {
    this._effWidthsCache = this._effWidths();
    // 数据事件（筛选落定/预取落定）会高频走这里；表头结构未变时绝不重建，
    // 否则正在输入的筛选框 DOM 被替换——焦点丢失、后续按键被吞。
    if (this._headerKey() !== this._headerKeyCache) this._renderHeader();
    this._layout();
    this._renderRows();
  }

  /**
   * 表头 DOM 重建签名：列顺序/列名/宽度/多重筛选开关与勾选列。
   *
   * @returns {string} 可比较的签名串。
   */
  _headerKey() {
    const widths = this._effWidthsCache ?? this._effWidths();
    return JSON.stringify([
      this.multi.on,
      [...this.multi.cols].sort((a, b) => a - b),
      this.colOrder,
      this.colOrder.map((c) => this.model.headers()[c] ?? ""),
      widths,
      // 索引中/完成：筛选框禁用态与占位文案不同，完成瞬间须重建一次
      this.model.indexing?.() === true,
    ]);
  }

  /** 重绘列头（列名带 + 筛选带） */
  _renderHeader() {
    this._headerKeyCache = this._headerKey();
    // 后台索引期间：筛选框禁用并显示"索引中…"（EmEditor 式进度语义）
    const indexing = this.model.indexing?.() === true;
    // 保留本侧角块（主/副网格 id 不同），重建列单元格
    for (const el of [...this.headerEl.children]) {
      if (el !== this.cornerEl) el.remove();
    }
    if (this.cornerEl.parentElement !== this.headerEl) {
      this.headerEl.prepend(this.cornerEl);
    }
    const widths = this._effWidthsCache;
    this.colOrder.forEach((srcCol, pos) => {
      const cell = document.createElement("div");
      cell.className = "col-head-cell";
      cell.style.width = `${widths[pos]}px`;
      cell.dataset.col = String(srcCol);
      const name = document.createElement("div");
      name.className = "col-name";
      if (this.multi.on) {
        // 多重筛选选中态：列名前出现复选框（默认勾选），勾选列按"或"参与匹配
        const cb = document.createElement("input");
        cb.type = "checkbox";
        cb.className = "col-multi";
        cb.checked = this.multi.cols.has(srcCol);
        cb.dataset.col = String(srcCol);
        cb.title = "多重筛选：本列是否参与匹配";
        // 复选框交互不触发列选择/拖拽
        cb.addEventListener("mousedown", (e) => e.stopPropagation());
        name.appendChild(cb);
      }
      const nameText = document.createElement("span");
      nameText.className = "col-name-text";
      nameText.textContent = this.model.headers()[srcCol] ?? "";
      name.appendChild(nameText);
      const filter = document.createElement("div");
      filter.className = "col-filter";
      const input = document.createElement("input");
      input.placeholder = indexing ? "索引中…" : "筛选…";
      input.disabled = indexing;
      if (indexing) input.title = "文件仍在后台索引，稍后即可筛选";
      input.value = this.filters.get(srcCol) ?? "";
      input.dataset.col = String(srcCol);
      filter.appendChild(input);
      // 严格三态开关（筛选框内右端常显）：~ 包含 → = 等于·不分大小写 → ≡ 等于·区分大小写
      const stMode = this.filterModes.get(srcCol) ?? 0;
      const st = document.createElement("button");
      st.type = "button";
      st.className = "col-strict";
      st.dataset.col = String(srcCol);
      st.dataset.mode = String(stMode);
      st.textContent = STRICT_GLYPHS[stMode];
      st.title = STRICT_TITLES[stMode];
      st.disabled = indexing;
      // 不触发列选择/拖拽；preventDefault 防止点击把焦点从筛选框抢走
      st.addEventListener("mousedown", (e) => {
        e.preventDefault();
        e.stopPropagation();
      });
      filter.appendChild(st);
      const rz = document.createElement("div");
      rz.className = "col-resize";
      rz.dataset.col = String(srcCol);
      cell.append(name, filter, rz);
      this.headerEl.appendChild(cell);
    });
  }

  /**
   * 重绘可视行（虚拟滚动，上下各 overscan 5 行）。
   *
   * 行内偏移一律窗口局部化（行顶 ≤ 视口高 + 10 行），#rows 容器经 transform
   * 平移到全局坐标——WebView2 实测绝对定位子元素偏移 ~2^22 px（4.2M）即不再绘制，
   * 行元素自身不得携带大偏移（translateY 走合成器，无此限制）。
   */
  _renderRows() {
    const host = this.rowsEl;
    const n = this.visCount();
    if (!n || !this.model.columnCount()) {
      host.textContent = "";
      host.style.transform = "";
      this._staleText.clear();
      return;
    }
    const viewH = this._viewportSize().h;
    // 行画在吸顶表头之下；等行高锚点窗口：行以真实行高连续排布、锚点行
    // 对齐视口顶，base 为行区滚动位置加首行与锚点的真实行高差（渲染几何，
    // 非全局虚拟坐标）。
    const { sv, anchor, h } = this._yAnchor();
    const first = Math.max(0, anchor - 5);
    const last = Math.min(n - 1, anchor + Math.ceil(viewH / h) + 5);
    const base = sv + (first - anchor) * h;
    const topAt = (vi) => (vi - first) * h;
    // 滚动渲染前预热窗口缓存（±2 屏预取由 BigModel 调度）
    this.model.prefetchRange?.(this.visAt(first), this.visAt(last));
    const widths = this._effWidthsCache;
    const cc = this.model.columnCount();
    // 行内偏移 = 全局坐标 - 基准（≤ 窗口高 + 10 行，不过 2^22），大偏移由容器 transform 承担
    host.style.transform = `translateY(${base}px)`;
    // 等行高（含虚拟缩放）：键控复用行元素。页未命中时保留旧内容
    // （stale-while-revalidate）——全部重建会把已填的格子刷成空白占位，拖动滚动条
    // 每帧白闪（频闪根因）。列数/列序/列宽变化（shape 键）时整窗重建。
    const shape = `${cc}|${this.colOrder.join(",")}|${widths.join(",")}`;
    if (shape !== this._shapeKey) {
      host.textContent = "";
      this._shapeKey = shape;
      this._staleText.clear();
    }
    const stale = this._staleText;
    const byRow = new Map();
    for (const el of host.children) byRow.set(Number(el.dataset.row), el);
    const totalW = this._rowHeadW() + widths.reduce((a, b) => a + b, 0);
    const has = this.model.hasData ?? null;
    for (let vi = first; vi <= last; vi += 1) {
      const r = this.visAt(vi);
      let row = byRow.get(r) ?? null;
      if (!row) {
        row = document.createElement("div");
        row.className = "row";
        row.dataset.row = String(r);
        const head = document.createElement("div");
        head.className = "row-head";
        head.dataset.row = String(r); // 行号列自身带行号（菜单/探针读它）
        row.appendChild(head);
        for (const srcCol of this.colOrder) {
          const cell = document.createElement("div");
          cell.className = "cell";
          cell.dataset.row = String(r);
          cell.dataset.col = String(srcCol);
          row.appendChild(cell);
        }
        host.appendChild(row); // 绝对定位子元素，DOM 顺序不影响布局与绘制
      }
      byRow.delete(r);
      row.style.top = `${topAt(vi)}px`;
      row.style.height = `${this._rowH(r)}px`;
      row.style.width = `${totalW}px`;
      const head = row.firstChild;
      head.textContent = this._rowHeadText(r);
      const kids = row.children;
      for (let pos = 0; pos < this.colOrder.length; pos += 1) {
        const srcCol = this.colOrder[pos];
        const cell = kids[pos + 1];
        cell.style.width = `${widths[pos]}px`;
        const key = r * cc + srcCol;
        cell.classList.toggle("sel", this.selectedSet.has(key));
        cell.classList.toggle("cur",
          !!this.sel.current && this.sel.current[0] === r && this.sel.current[1] === srcCol);
        // 命中缓存才刷新文本；未命中保留上次显示值，数据落定后经 onData 重绘收敛
        const t = (!has || has.call(this.model, r, srcCol))
          ? this.model.displayData(r, srcCol)
          : (stale.get(key) ?? "");
        if (cell.textContent !== t) cell.textContent = t; // 同值跳过，避免无谓失效重绘
        stale.set(key, t);
      }
    }
    for (const el of byRow.values()) el.remove();
    // 兜底表限界：只保留当前窗口 ±3000 行（防无限增长；滚出不久滚回仍有旧内容）
    if (stale.size > 12000) {
      const rLo = this.visAt(first) - 3000;
      const rHi = this.visAt(last) + 3000;
      for (const key of stale.keys()) {
        const rr = Math.floor(key / cc);
        if (rr < rLo || rr > rHi) stale.delete(key);
      }
    }
  }

  /** 重建筛选输入框（列结构变化后），保留已有条件文本 */
  _rebuildFilterInputs() {
    // 实际 DOM 由 _renderHeader 统一重建；此处仅清理失效列的条件
    const n = this.model.columnCount();
    for (const col of [...this.filters.keys()]) {
      if (col >= n) this.filters.delete(col);
    }
    for (const col of [...this.filterModes.keys()]) {
      if (col >= n) this.filterModes.delete(col);
    }
  }

  /** 按当前条件重算可见行并刷新行渲染与状态 */
  refilter() { this._refilter(); }

  _refilter() {
    const cc = this.model.columnCount();
    // 条件存在性：逐列筛选框有有效关键字，或多重筛选开且有关键字
    const hasColKw = [...this.filters].some(([col, kw]) => col < cc && kw);
    const hasMulti = this.multi.on && !!this.multi.text;
    const hasCond = hasColKw || hasMulti;
    // 统一大引擎：整表扫描在 Rust 引擎（键入即筛/回车触发），前端只维护视图状态。
    // 取消筛选锚定：捕获必须在坐标系切换（筛选态）之前；
    // 结构性清筛（clearFilters({anchor:false})）不捕获，回落旧回顶行为
    const cap = (!hasCond && this.model.filterActive()
      && this._clearAnchorPref !== false)
      ? this._captureClearAnchor()
      : null;
    this.visibleRows = null;
    if (!hasCond && this.model.filterActive()) {
      // 条件清空 → 自动回全量。锚定路径：先查归位行号再清筛（命中集清后
      // 即失），落定后把选中行/视口顶行送回原来的位置（用户无感）；
      // 被抢断/无锚/越界回落回顶。
      // _suppressIndexAnchor：落定触发的 _onModelData 会把 visCount 增长
      // 误判成索引推进而抢恢复——锚定期间压住它。
      this._suppressIndexAnchor = true;
      Promise.resolve(this.model.clearFilter(cap ? cap.k : null))
        .then((res) => {
          if (!res?.applied) return; // 被新筛选/清筛抢断：锚点作废
          if (cap && res.target !== null && res.target !== undefined) {
            this._restoreClearAnchor(cap, res.target);
          } else {
            this.resetScroll();
            this._renderRows();
          }
        })
        .catch(() => {})
        .finally(() => { this._suppressIndexAnchor = false; });
    }
    this.hooks.bigFilterStatus?.(hasCond);
    this._clampSelection();
    this._renderRows();
    this.hooks.countsChanged(this.visCount(), this.model.rowCount());
  }

  /**
   * 模型数据变化入口。
   *
   * @param {boolean} structural - 是否结构变化（行列增删/排序/装载）。
   */
  _onModelData(structural) {
    // 索引推进：行数增长会改变画布总高与 _yScale，同一 scrollTop 可能
    // 落到别的行——先记住视口锚点位置（小数行号），重排后原位送回。
    // 必须按「视口顶锚点」原位恢复（不动点）：若按「表头下对齐」恢复，每次
    // 进度事件都净下移一个表头高，事件次数随索引速度变（CI 实测两次 −3 行）。
    let anchorPos = null;
    const rowsBefore = this._lastVisCount;
    if (structural && !this._suppressIndexAnchor) {
      const pos = this._yAnchor().pos;
      if (pos > 0 && pos < rowsBefore) anchorPos = pos;
    }
    if (structural) {
      if (this.model.columnCount() !== this.colW.length) {
        this.rebuildStructure();
        return;
      }
    }
    this._refilter();
    this.renderAll();
    if (anchorPos !== null && this.visCount() > rowsBefore) {
      // 行数增长会改变压缩比例/滚动范围：把原锚点位置精确送回（不动点）。
      const top = this._scrollTopForAnchor(anchorPos);
      if (Math.abs(this.scrollEl.scrollTop - top) > 1) {
        this.scrollEl.scrollTop = top;
        this._renderRows();
      }
    }
  }

  /** 窗口尺寸/缩放变化 → 重排（末列拉伸量可能变化）；rAF 去抖防自激循环 */
  _observeResize() {
    let pending = false;
    new ResizeObserver(() => {
      if (pending) return;
      pending = true;
      requestAnimationFrame(() => {
        pending = false;
        this.renderAll();
      });
    }).observe(this.scrollEl);
  }

  // ============ 选区 ============

  /**
   * 设置选区。
   *
   * @param {[number, number]} focus - 焦点格。
   * @param {object} [mode] - 选择方式。
   * @param {boolean} [mode.extend] - 从锚点扩展。
   * @param {boolean} [mode.add] - 追加新块（Ctrl）。
   * @param {boolean} [mode.silent] - 不触发 currentChanged。
   */
  select(focus, { extend = false, add = false, silent = false } = {}) {
    const rows = this.model.rowCount();
    const cols = this.model.columnCount();
    if (!rows || !cols) return;
    const f = [Math.min(Math.max(focus[0], 0), rows - 1),
               Math.min(Math.max(focus[1], 0), cols - 1)];
    if (extend && this.sel.anchor) {
      const a = this.sel.anchor;
      this.sel.blocks = [[a[0], a[1], f[0], f[1]]];
    } else if (add) {
      this.sel.anchor = f;
      this.sel.blocks.push([f[0], f[1], f[0], f[1]]);
    } else {
      this.sel.anchor = f;
      this.sel.blocks = [[f[0], f[1], f[0], f[1]]];
    }
    this.sel.current = f;
    this._rebuildSelectedSet();
    this._renderRows();
    if (!silent) this.hooks.currentChanged(f[0], f[1]);
  }

  /** 清空全部选区与当前格（撤销/排序/装载后，对齐 Qt clearSelection） */
  clearSelection() {
    this.sel = { current: null, anchor: null, blocks: [] };
    this.selectedSet.clear();
    this._renderRows();
  }

  /** Esc：清空选区填充但保留当前格（对齐 Qt 表格视图的 Escape 行为） */
  deselect() {
    if (!this.sel.blocks.length) return;
    this.sel.blocks = [];
    this.selectedSet.clear();
    this._renderRows();
  }

  /** 选整行 */
  selectRow(r, opts = {}) {
    this.select([r, 0], { ...opts, silent: true });
    this._extendTo([r, this.model.columnCount() - 1]);
    if (!opts.silent) this.hooks.currentChanged(this.sel.current[0], this.sel.current[1]);
  }

  /** 选整列 */
  selectCol(c, opts = {}) {
    this.select([0, c], { ...opts, silent: true });
    this._extendTo([this.model.rowCount() - 1, c]);
    if (!opts.silent) this.hooks.currentChanged(this.sel.current[0], this.sel.current[1]);
  }

  /**
   * 把当前块扩展到指定格。
   *
   * @param {[number, number]} to - 目标格。
   */
  _extendTo(to) {
    if (!this.sel.anchor) return;
    const a = this.sel.anchor;
    this.sel.blocks = [[a[0], a[1], to[0], to[1]]];
    this.sel.current = to;
    this._rebuildSelectedSet();
    this._renderRows();
  }

  /** 依据 blocks 重建选中集合 */
  _rebuildSelectedSet() {
    const cc = this.model.columnCount();
    const set = new Set();
    // 逐格集合爆炸上限（选整列 30M 行 × N 列会卡死 JS），
    // 超限时按行截断到首个块（矩形语义不变，范围收敛）
    const cap = Math.max(1, Math.floor(200000 / Math.max(1, cc)));
    for (const [r1, c1, r2, c2] of this.sel.blocks) {
      const ra = Math.min(r1, r2);
      const rb = Math.min(Math.max(r1, r2), ra + cap - 1);
      if (rb < Math.max(r1, r2) && !this._selCapNotified) {
        this._selCapNotified = true;
        this.hooks.statusMsg(`大文件模式：选区已按前 ${cap} 行截断`, 3000);
      }
      for (let r = ra; r <= rb; r += 1) {
        for (let c = Math.min(c1, c2); c <= Math.max(c1, c2); c += 1) {
          set.add(r * cc + c);
        }
      }
    }
    this.selectedSet = set;
  }

  /** 结构变化后收敛选区（越界清除） */
  _clampSelection() {
    const rows = this.model.rowCount();
    const cols = this.model.columnCount();
    if (!this.sel.current || !rows || !cols) {
      if (rows && cols && !this.sel.current) return;
      this.sel = { current: null, anchor: null, blocks: [] };
      this.selectedSet.clear();
      return;
    }
    this.sel.current = [Math.min(this.sel.current[0], rows - 1),
                        Math.min(this.sel.current[1], cols - 1)];
    this.sel.blocks = this.sel.blocks
      .map(([r1, c1, r2, c2]) => [Math.min(r1, rows - 1), Math.min(c1, cols - 1),
                                   Math.min(r2, rows - 1), Math.min(c2, cols - 1)])
      .filter((b) => b[0] >= 0 && b[2] >= 0 && b[1] >= 0 && b[3] >= 0);
    this.sel.anchor = this.sel.current;
    this._rebuildSelectedSet();
  }

  /**
   * 单块选区返回规范化矩形；多块或无块返回 null。
   *
   * @returns {{row: number, col: number, rows: number, cols: number} | null}
   */
  _singleSelectionRect() {
    if (this.sel.blocks.length !== 1) return null;
    const [r1, c1, r2, c2] = this.sel.blocks[0];
    return {
      row: Math.min(r1, r2), col: Math.min(c1, c2),
      rows: Math.abs(r2 - r1) + 1, cols: Math.abs(c2 - c1) + 1,
    };
  }

  /**
   * @returns {Array<[number, number]>} 选中的源坐标单元格列表。
   */
  selectedCells() {
    const cc = this.model.columnCount();
    const out = [];
    for (const key of this.selectedSet) {
      out.push([Math.floor(key / cc), key % cc]);
    }
    return out;
  }

  /** @returns {number[]} 选区覆盖的源行号（升序去重） */
  selectedRows() {
    return [...new Set(this.selectedCells().map(([r]) => r))].sort((a, b) => a - b);
  }

  /** @returns {number[]} 选区覆盖的源列号（升序去重） */
  selectedCols() {
    return [...new Set(this.selectedCells().map(([, c]) => c))].sort((a, b) => a - b);
  }

  // ============ 事件绑定 ============

  /** 筛选条件变化统一尾段：刷新状态并走引擎防抖链。 */
  _filterCondChanged() {
    this._refilter(); // 刷新状态；条件清空时 _refilter 会自动清引擎筛选
    this.hooks.bigFilterInput?.(); // 键入即筛（app 侧防抖 + 可中断扫描）
  }

  _bindEvents() {
    const scroll = this.scrollEl;

    scroll.addEventListener("scroll", () => {
      if (!this.editing) this._renderRows();
      // 列头/行号随画布滚动（sticky 自动处理），无需手动同步
    });

    // ---- 鼠标：单元格 ----
    scroll.addEventListener("mousedown", (e) => {
      if (e.button !== 0) return;
      if (this.editing) this.commitEdit(); // 点别处先提交（Qt 行为）
      const cell = e.target.closest(".cell");
      if (cell) {
        const r = Number(cell.dataset.row);
        const c = Number(cell.dataset.col);
        this.select([r, c], { extend: e.shiftKey, add: e.ctrlKey || e.metaKey });
        this._drag = { kind: "cells", to: [r, c] };
        e.preventDefault();
        scroll.focus();
        return;
      }
      const rh = e.target.closest(".row-head");
      if (rh) {
        const r = Number(rh.dataset.row);
        if (e.shiftKey) {
          this._extendTo([r, this.model.columnCount() - 1]);
        } else {
          this.selectRow(r, { add: e.ctrlKey || e.metaKey });
        }
        this._drag = { kind: "rows", to: r };
        e.preventDefault();
        scroll.focus();
        return;
      }
      const cn = e.target.closest(".col-name");
      if (cn) {
        const c = Number(cn.parentElement.dataset.col);
        if (e.shiftKey) {
          this._extendTo([this.model.rowCount() - 1, c]);
        } else {
          this.selectCol(c, { add: e.ctrlKey || e.metaKey });
        }
        this._drag = { kind: "cols", to: c, startX: e.clientX, moved: false };
        e.preventDefault();
        scroll.focus();
      }
    });

    document.addEventListener("mousemove", (e) => {
      if (!this._drag) return;
      if (this._drag.kind === "cells") {
        const info = this._hitTest(e.clientX, e.clientY);
        if (info) {
          this._drag.to = info;
          this._extendTo(info);
        }
      } else if (this._drag.kind === "rows") {
        // 视口 y → 行区 y → 可见行序号 → 源行号（行区在吸顶表头之下，须减表头高）
        const y = e.clientY - this.scrollEl.getBoundingClientRect().top +
          this.scrollEl.scrollTop - this._originY();
        const rr = this.visAt(this._rowAt(y));
        this._drag.to = rr;
        this._extendTo([rr, this.model.columnCount() - 1]);
      } else if (this._drag.kind === "cols") {
        if (Math.abs(e.clientX - this._drag.startX) > 6) this._drag.moved = true;
        if (this._drag.moved) {
          const x = e.clientX - this.scrollEl.getBoundingClientRect().left + this.scrollEl.scrollLeft;
          const pos = this._posAt(x);
          const over = [...this.headerEl.querySelectorAll(".col-head-cell")];
          over.forEach((el) => el.classList.remove("col-drag-over"));
          if (over[pos]) over[pos].classList.add("col-drag-over");
          this._drag.targetPos = pos;
        }
      } else if (this._drag.kind === "col-resize") {
        const d = this._drag;
        const w = Math.max(MIN_COL_W, d.startW + e.clientX - d.startX);
        this.colW[d.col] = Math.round(w);
        this._effWidthsCache = this._effWidths();
        this._renderHeader();
        this._layout();
        this._renderRows();
      }
    });

    document.addEventListener("mouseup", () => {
      if (!this._drag) return;
      const d = this._drag;
      this._drag = null;
      if (d.kind === "cols" && d.moved && d.targetPos !== undefined) {
        this._moveCol(d.to, d.targetPos);
        [...this.headerEl.querySelectorAll(".col-drag-over")]
          .forEach((el) => el.classList.remove("col-drag-over"));
      }
    });

    // ---- 双击编辑 ----
    scroll.addEventListener("dblclick", (e) => {
      const cell = e.target.closest(".cell");
      if (cell) {
        this.openEditor(Number(cell.dataset.row), Number(cell.dataset.col),
          { selectAll: true });
      }
    });

    // ---- 列宽/行高拖拽热区 ----
    scroll.addEventListener("mousedown", (e) => {
      if (e.button !== 0) return;
      const crz = e.target.closest(".col-resize");
      if (crz) {
        const col = Number(crz.dataset.col);
        this._drag = { kind: "col-resize", col, startX: e.clientX, startW: this.colW[col] };
        e.preventDefault();
        e.stopPropagation();
        return;
      }
    }, true);

    // ---- 筛选输入 ----
    // 条件进 grid 后由 app 侧防抖触发引擎扫描（键入即筛），回车跳过防抖立即执行
    this.headerEl.addEventListener("input", (e) => {
      if (this._suppressFilterInput) return;
      const input = e.target.closest(".col-filter input");
      if (!input) return;
      const col = Number(input.dataset.col);
      const v = input.value;
      if (v) this.filters.set(col, v); else this.filters.delete(col);
      this._filterCondChanged();
    });

    // ---- 严格三态开关点击：0 → 1 → 2 → 0 循环（列级独立设置） ----
    this.headerEl.addEventListener("click", (e) => {
      const st = e.target.closest(".col-strict");
      if (!st) return;
      const col = Number(st.dataset.col);
      const mode = ((this.filterModes.get(col) ?? 0) + 1) % 3;
      if (mode) this.filterModes.set(col, mode); else this.filterModes.delete(col);
      st.dataset.mode = String(mode);
      st.textContent = STRICT_GLYPHS[mode];
      st.title = STRICT_TITLES[mode];
      // 该列有条件文本时与输入等效触发重筛（走 app 侧防抖链）
      if (this.filters.get(col)) this._filterCondChanged();
    });

    // 筛选框内回车 = 跳过防抖立即执行一次
    this.headerEl.addEventListener("keydown", (e) => {
      if (e.key !== "Enter") return;
      if (e.target.closest(".col-filter input")) {
        // 输入法组合态的 Enter 是"选词/上屏"，必须放行给输入法，
        // 否则会拿未上屏的拼音串提前触发筛选。
        if (e.isComposing) return;
        e.preventDefault();
        this.hooks.bigFilterRun?.();
      }
    });

    // ---- 多重筛选列勾选 ----
    this.headerEl.addEventListener("change", (e) => {
      const cb = e.target.closest(".col-multi");
      if (!cb) return;
      const col = Number(cb.dataset.col);
      if (cb.checked) this.multi.cols.add(col); else this.multi.cols.delete(col);
      this._refilter();
      this.hooks.bigFilterInput?.(); // 勾选列变化也影响命中集
    });

    // ---- 右键菜单 ----
    scroll.addEventListener("contextmenu", (e) => {
      const cell = e.target.closest(".cell");
      const rh = e.target.closest(".row-head");
      const cn = e.target.closest(".col-name");
      const fin = e.target.closest(".col-filter input");
      if (fin) return; // 筛选框保留浏览器原生菜单（Qt 同样交给 QLineEdit）
      e.preventDefault();
      if (cell) {
        const r = Number(cell.dataset.row);
        const c = Number(cell.dataset.col);
        if (!this.selectedSet.has(r * this.model.columnCount() + c)) {
          this.select([r, c]);
        }
        this.hooks.cellMenu(r, c, e);
      } else if (rh) {
        const r = Number(rh.dataset.row);
        const rows = this.selectedRows();
        if (!rows.includes(r)) this.selectRow(r);
        this.hooks.rowMenu(r, e);
      } else if (cn) {
        const c = Number(cn.parentElement.dataset.col);
        const cols = this.selectedCols();
        if (!cols.includes(c)) this.selectCol(c);
        this.hooks.colMenu(c, e);
      }
    });

    // ---- 键盘 ----
    scroll.addEventListener("keydown", (e) => this._onKeyDown(e));

    // ---- 超大行数滚轮：按行滚动（滚动条坐标经 _yScale 压缩，原生像素滚动会一次跳几百行） ----
    scroll.addEventListener("wheel", (e) => {
      if (e.ctrlKey || this._yScale === 1) return;
      e.preventDefault();
      const h = this._uniformRowH;
      this._wheelAcc = (this._wheelAcc || 0) + e.deltaY;
      const rows = Math.trunc(this._wheelAcc / h);
      this._wheelAcc -= rows * h;
      if (rows) this.scrollEl.scrollTop += (rows * h) / this._yScale;
    }, { passive: false });

    // ---- Ctrl+滚轮缩放 ----
    scroll.addEventListener("wheel", (e) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      const step = e.deltaY < 0 ? ZOOM_STEP : 1 / ZOOM_STEP;
      this.setZoom(this.zoom * step);
    }, { passive: false });
  }

  /**
   * 视口坐标 → 源坐标命中测试。
   *
   * @param {number} clientX - 客户端 x。 @param {number} clientY - 客户端 y。
   * @returns {[number, number] | null} (源行, 源列)。
   */
  _hitTest(clientX, clientY) {
    const rect = this.scrollEl.getBoundingClientRect();
    const x = clientX - rect.left + this.scrollEl.scrollLeft;
    const y = clientY - rect.top + this.scrollEl.scrollTop - this._originY();
    if (x < this._rowHeadW() || !this.visCount()) return null;
    const r = this.visAt(this._rowAt(y));
    const pos = this._posAt(x);
    return [r, this.colOrder[pos]];
  }

  /**
   * 把源列从原显示位置移到新位置（列拖动排序）。
   *
   * @param {number} srcCol - 源列号。 @param {number} targetPos - 目标显示位置。
   */
  _moveCol(srcCol, targetPos) {
    const from = this.colOrder.indexOf(srcCol);
    if (from < 0) return;
    this.colOrder.splice(from, 1);
    this.colOrder.splice(targetPos, 0, srcCol);
    this.renderAll();
  }

  /**
   * 键盘分发（选择态导航 + 编辑触发 + 剪贴板）。
   *
   * @param {KeyboardEvent} e - 按键事件。
   */
  _onKeyDown(e) {
    if (this.editing) return; // 编辑器自行处理
    // 筛选框等输入控件聚焦时键盘归控件自己，防"打字即编辑"劫持输入
    const tag = e.target?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
    const cur = this.sel.current;
    const mods = e.ctrlKey || e.metaKey;
    if (mods && !e.shiftKey && (e.key === "c" || e.key === "C")) {
      e.preventDefault(); this._runClipboard(this.copySelection()); return;
    }
    if (mods && !e.shiftKey && (e.key === "x" || e.key === "X")) {
      e.preventDefault(); this._runClipboard(this.cutSelection()); return;
    }
    if (mods && !e.shiftKey && (e.key === "v" || e.key === "V")) {
      e.preventDefault(); this.pasteClipboard(false); return;
    }
    if (mods && e.key === "z") { e.preventDefault(); this.hooks.undoRequested?.(); return; }
    if (mods && (e.key === "y" || (e.shiftKey && e.key === "Z"))) {
      e.preventDefault(); this.hooks.redoRequested?.(); return;
    }
    if (e.key === "Delete") { e.preventDefault(); this._runClipboard(this.deleteSelection()); return; }
    if (e.key === "F2") {
      if (cur) { e.preventDefault(); this.openEditor(cur[0], cur[1], { selectAll: true }); }
      return;
    }
    if (e.key === "Tab") {
      e.preventDefault();
      this._moveCurrent(0, e.shiftKey ? -1 : 1);
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      this._moveCurrent(e.shiftKey ? -1 : 1, 0);
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      this.deselect(); // 对齐 Qt 表格：Esc 取消选定
      return;
    }
    // Excel Ctrl+↑/↓：跳到当前列数据区域的上/下边缘。
    // 大模式的数据在 Rust 侧，异步查找；Shift+Ctrl+方向键不在此列（保持逐格扩选）。
    if (mods && !e.shiftKey && !e.altKey
        && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
      e.preventDefault();
      this._jumpVerticalEdge(e.key === "ArrowDown");
      return;
    }
    const arrows = {
      ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1],
    };
    if (arrows[e.key]) {
      e.preventDefault();
      const [dr, dc] = arrows[e.key];
      if (e.shiftKey && cur) {
        this._extendTo([cur[0] + dr, cur[1] + dc]);
        this.hooks.currentChanged?.(cur[0] + dr, cur[1] + dc);
      } else {
        this._moveCurrent(dr, dc);
      }
      return;
    }
    // 打字即编辑（Excel AnyKeyPressed）：可打印字符替换式打开编辑器
    if (!mods && !e.altKey && e.key.length === 1) {
      if (!cur) return;
      e.preventDefault();
      this.openEditor(cur[0], cur[1], { initial: e.key, caretToEnd: true });
    } else if (!mods && !e.altKey && (e.key === "Backspace" || e.key === "Delete")) {
      // 上面已拦截 Delete；Backspace 打开编辑器全选（删除即清空再输入）
      if (!cur) return;
      e.preventDefault();
      this.openEditor(cur[0], cur[1], { selectAll: true });
    }
  }

  /**
   * 移动当前格（选择态导航）。
   *
   * @param {number} dr - 行偏移。 @param {number} dc - 列偏移。
   */
  _moveCurrent(dr, dc) {
    const cur = this.sel.current ?? [0, -1];
    const r = Math.min(Math.max(cur[0] + dr, 0), this.model.rowCount() - 1);
    const c = Math.min(Math.max(cur[1] + dc, 0), this.model.columnCount() - 1);
    if (this.model.rowCount() && this.model.columnCount()) this.select([r, c]);
  }

  /**
   * Ctrl+↑/↓：按 Excel 语义跳到当前列的数据边缘。
   *
   * 小模型同步扫描可见行；大模型经引擎命令扫描（前端窗口缓存只有 ±2 页，
   * 不能在 JS 里跨千万行逐格取数）。异步结果落定前若当前格/模型/编辑态
   * 已变化，直接丢弃。
   *
   * @param {boolean} down - true=向下（Ctrl+↓）；false=向上（Ctrl+↑）。
   * @returns {Promise<void>}
   */
  async _jumpVerticalEdge(down) {
    const cur = this.sel.current;
    if (!cur) return;
    const model = this.model;
    const [row, col] = cur;
    const seq = ++this._navSeq;
    let target;
    try {
      target = await model.findVerticalEdge(row, col, down);
    } catch (err) {
      if (seq === this._navSeq) {
        this.hooks.statusMsg?.(`查找数据边缘失败：${err}`, 4000);
      }
      return;
    }
    if (seq !== this._navSeq || model !== this.model || this.editing) return;
    if (target === null || target === undefined) return;
    const now = this.sel.current;
    if (!now || now[0] !== row || now[1] !== col) return;
    this.select([target, col]);
    this.ensureVisible(target, col);
  }

  /** 滚动回左上角（装载/恢复时调用；WebView2 会记忆上次滚动位置） */
  resetScroll() {
    this.scrollEl.scrollTop = 0;
    this.scrollEl.scrollLeft = 0;
  }

  /** 选中区域滚动到可视（编辑提交移动后调用） */
  ensureVisible(r, c) {
    const vi = this.visibleIndexOf(r);
    if (vi < 0) return;
    const oy = this._originY();
    const viewTop = this.scrollEl.scrollTop;
    const vp = this._viewportSize();
    const viewH = vp.h;
    if (this._yScale > 1) {
      // 压缩滚动：行的屏幕位置由锚点窗口几何（_paintTop）给出，不能用全局压缩
      // 坐标直接判断。上方不可见 → 顶对齐；下方不可见 → 底对齐（末行即滚到底）。
      const h = this._uniformRowH;
      const clientTop = this._paintTop(vi) - viewTop;
      if (clientTop < oy) {
        this.scrollEl.scrollTop = this._scrollTopForRowTop(vi);
      } else if (clientTop + h > viewH) {
        this.scrollEl.scrollTop = this._scrollTopForRowBottom(vi);
      }
    } else {
      // 未压缩：行区坐标即滚动坐标。吸顶表头占视口顶部 oy，可见区域是
      // [scrollTop, scrollTop+viewH-oy]；对齐全按这个区间计算。
      const top = this._rowTop(vi);
      const rh = this._uniformRowH;
      if (top < viewTop) this.scrollEl.scrollTop = Math.max(0, top);
      else if (top + rh > viewTop + viewH - oy) {
        this.scrollEl.scrollTop = Math.max(0, top + rh - (viewH - oy));
      }
    }
    const x = this._colLeft(this.colOrder.indexOf(c));
    const w = this.colW[c];
    const viewL = this.scrollEl.scrollLeft;
    const viewW = vp.w;
    if (x < viewL + this._rowHeadW()) this.scrollEl.scrollLeft = Math.max(0, x - this._rowHeadW());
    else if (x + w > viewL + viewW) this.scrollEl.scrollLeft = x + w - viewW;
  }

  // ============ 格内编辑（C5） ============

  /**
   * 打开格内编辑器。
   *
   * @param {number} r - 源行号。 @param {number} c - 源列号。
   * @param {object} [opt] - 打开方式。
   * @param {boolean} [opt.selectAll] - 全选内容（双击/F2/Backspace）。
   * @param {string} [opt.initial] - 预填字符（打字即编辑）。
   * @param {boolean} [opt.caretToEnd] - 光标置末尾（替换式输入继续）。
   */
  openEditor(r, c, { selectAll = false, initial = undefined, caretToEnd = false } = {}) {
    if (r >= this.model.rowCount() || c >= this.model.columnCount()) return;
    this.closeEditor();
    const el = this.editorEl;
    const pos = this.colOrder.indexOf(c);
    const widths = this._effWidthsCache;
    let x = this._rowHeadW();
    for (let i = 0; i < pos; i += 1) x += widths[i];
    el.style.display = "block";
    el.style.left = `${x}px`;
    const vi = this.visibleIndexOf(r);
    // 纵坐标走 transform + 真实渲染几何（_paintTop）：top 大偏移会超 2^22 不绘制，
    // 全局虚拟 _rowTop 与锚点窗口的画法不一致，编辑器会飘离目标行
    el.style.top = "0px";
    el.style.transform = `translateY(${vi >= 0 ? this._paintTop(vi) : 0}px)`;
    el.style.width = `${Math.max(widths[pos], MIN_COL_W)}px`;
    el.style.height = `${Math.max(this._rowH(r), this._lineH())}px`;
    el.value = initial !== undefined ? initial : this.model.rawData(r, c);
    this.editing = { r, c };
    el.focus();
    if (selectAll) el.select();
    else if (caretToEnd) el.setSelectionRange(el.value.length, el.value.length);
    this._growEditor();

    el.onkeydown = (e) => {
      if (e.key === "Enter" || e.key === "NumpadEnter") {
        if (e.altKey) return; // Alt+Enter：浏览器默认插入换行
        e.preventDefault();
        e.stopPropagation(); // 防止冒泡到网格被当作导航二次移动
        this.commitEdit();
        this._moveCurrent(e.shiftKey ? -1 : 1, 0);
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        e.stopPropagation(); // 同上：提交移动只发生一次
        this.commitEdit();
        this._moveCurrent(0, e.shiftKey ? -1 : 1);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        this.cancelEdit();
        return;
      }
    };
    el.oninput = () => this._growEditor();
    el.onblur = () => { if (this.editing) this.commitEdit(); };
  }

  /** 编辑器随行高自动加高（Qt：浮于下方单元格之上，不改行高，最多 8 行） */
  _growEditor() {
    const el = this.editorEl;
    const lineH = this._lineH();
    const lines = el.value.split("\n").length;
    const want = Math.min(lines + 1, 8) * lineH;
    el.style.height = `${Math.max(this._rowH(this.editing?.r ?? 0), want)}px`;
  }

  /** @returns {number} 编辑器行高 px */
  _lineH() {
    const fs = parseFloat(getComputedStyle(this.scrollEl).fontSize);
    return Math.round(fs * 1.45);
  }

  /**
   * 提交编辑器内容到模型。
   *
   * @returns {Promise<void>} 写入落定（大模型异步写）；无编辑时已 resolve。
   */
  commitEdit() {
    if (!this.editing) return Promise.resolve();
    const { r, c } = this.editing;
    const value = this.editorEl.value;
    this.closeEditor();
    // 大模型写入是异步的：失败（如列操作 pending）转状态栏，避免未捕获拒绝；
    // 返回 Promise 供调用方（切换无列名开关）等待写入落定后再改行坐标。
    return Promise.resolve(this.model.setData(r, c, value)).catch((err) => {
      this.hooks.statusMsg(String(err?.message ?? err), 3000);
    });
  }

  /** 取消编辑（不写回，对齐 Qt RevertModelCache） */
  cancelEdit() {
    this.closeEditor();
  }

  closeEditor() {
    this.editing = null;
    this.editorEl.style.display = "none";
    this.editorEl.onblur = null;
    this.editorEl.onkeydown = null;
    this.editorEl.oninput = null;
  }

  // ============ 剪贴板（G1/G2/G4） ============

  /**
   * 统一捕获复制、剪切和删除的异步拒绝。
   *
   * @param {Promise<unknown>} promise - 剪贴板或清除操作。
   * @returns {void}
   */
  _runClipboard(promise) {
    Promise.resolve(promise).catch((err) => {
      this.hooks.statusMsg(String(err?.message ?? err), 3000);
    });
  }

  /**
   * 复制选区：小结果写系统剪贴板，大结果只保留应用内 token。
   *
   * @returns {Promise<boolean>} 写入成功为 true，失败或空选区为 false。
   */
  async copySelection() {
    if (!this.selectedSet.size) return false;
    try {
      let rect = this._singleSelectionRect();
      let rows = null;
      let cols = null;
      if (!rect) {
        rows = this.selectedRows();
        cols = this.selectedCols();
        if (this.selectedSet.size === rows.length * cols.length) {
          rect = {
            row: rows[0], col: cols[0],
            rows: rows[rows.length - 1] - rows[0] + 1,
            cols: cols[cols.length - 1] - cols[0] + 1,
          };
        }
      }
      let text;
      if (rect) {
        const outcome = await this.model.copyRows(
          rect.row, rect.row + rect.rows, rect.col, rect.col + rect.cols);
        if (outcome?.kind === "app") {
          this.lastCopiedText = null;
          this.hooks.statusMsg("已复制大区域，仅可在本应用内粘贴", 3000);
          return true;
        }
        if (outcome?.kind !== "text") throw new Error("复制结果格式无效");
        text = outcome.text;
      } else {
        if (this.selectedSet.size > 20000) {
          this.hooks.statusMsg("大文件模式：大型选区仅支持完整矩形复制", 3000);
          return false;
        }
        const cc = this.model.columnCount();
        const data = [];
        for (const r of rows) {
          const line = [];
          for (const c of cols) {
            line.push(this.selectedSet.has(r * cc + c) ? this.model.rawData(r, c) : "");
          }
          data.push(line);
        }
        text = serializeRecords(data, "\t");
      }
      await navigator.clipboard.writeText(text);
      // 只有系统剪贴板写入成功后，才使旧的应用内 token 失效。
      await this.model.clipboardClear();
      this.lastCopiedText = text;
      const count = rect ? rect.rows * rect.cols : this.selectedSet.size;
      this.hooks.statusMsg("已复制 " + count + " 个单元格", 2000);
      return true;
    } catch (err) {
      this.hooks.statusMsg(String(err?.message ?? err), 3000);
      return false;
    }
  }

  /**
   * 剪切等于复制成功后才清除原选区。
   *
   * @returns {Promise<boolean>} 实际清除为 true，复制失败或空选区为 false。
   */
  async cutSelection() {
    if (!this.selectedSet.size) return false;
    const blocks = JSON.stringify(this.sel?.blocks ?? []);
    const copied = await this.copySelection();
    if (!copied || blocks !== JSON.stringify(this.sel?.blocks ?? [])) return false;
    return this.deleteSelection(false);
  }

  /**
   * Delete 清除选区；单块直接发矩形，多块保留稀疏坐标。
   *
   * @param {boolean} [say] - 是否在实际变化后发状态消息。
   * @returns {Promise<boolean>} 是否实际清除内容。
   */
  async deleteSelection(say = true) {
    const n = this.selectedSet.size;
    if (!n) return false;
    const rect = this._singleSelectionRect();
    const outcome = rect
      ? await this.model.clearRect(rect.row, rect.col, rect.rows, rect.cols)
      : await this.model.clearCells(this.selectedCells());
    if (outcome?.changed && say) {
      this.hooks.statusMsg("已清除 " + n + " 个单元格的内容", 2000);
    }
    return outcome?.changed === true;
  }

  /**
   * 粘贴（三级判据，对齐 Qt _paste_from_clipboard）。
   *
   * @param {boolean} forceSplit - 无条件按区域解析（右键"按行拆分粘贴"）。
   */
  async pasteClipboard(forceSplit = false) {
    const cur = this.sel.current;
    if (!cur) return;
    const [r0, c0] = cur;
    // 应用内 token 优先于系统文本；失效 token 直接报错，不回退旧文本。
    let tokenMeta = null;
    try {
      tokenMeta = await this.model.clipboardMeta();
    } catch {
      tokenMeta = null;
    }
    if (tokenMeta?.token) {
      let destination = { kind: "rect", row: r0, col: c0 };
      if (!forceSplit && tokenMeta.rows === 1 && tokenMeta.cols === 1
          && this.selectedSet.size > 1) {
        const rect = this._singleSelectionRect();
        destination = {
          kind: "fill",
          target: rect
            ? { kind: "rect", row: rect.row, col: rect.col, rows: rect.rows, cols: rect.cols }
            : { kind: "cells", cells: this.selectedCells() },
        };
      }
      try {
        const outcome = await this.model.pasteClipboardToken(tokenMeta.token, destination);
        if (outcome?.cancelled) {
          this.hooks.statusMsg("已取消粘贴", 2500);
          return;
        }
        this.hooks.statusMsg(destination.kind === "fill" ? "已填充选区" : "已粘贴", 2000);
      } catch (err) {
        this.hooks.statusMsg(String(err?.message ?? err), 3000);
      }
      return;
    }
    let text;
    try {
      text = await navigator.clipboard.readText();
    } catch {
      this.hooks.statusMsg("粘贴失败：无法读取剪贴板", 3000);
      return;
    }
    if (!text) return;
    // TextLines 格内禁止换行：多行外部文本必须走区域粘贴，每行进一格。
    const multiLineTextLines = this.model.isTextLines?.() === true
      && /[\r\n]/.test(text);
    // 一对多填充（Excel）：剪贴板是单格单值（无 Tab/换行的单行，内外部来源皆然）
    // 且选区超过一格 → 全选区填充。必须先于"纯文本整段进当前格"分支判定，
    // 否则外部单值文本会被整段吞进当前格。forceSplit（按行拆分粘贴）不填充。
    // readText 已产生完整 JS 字符串；大文本只做有上限的体量探测，避免 replace/parseCsv 再复制全文。
    if (utf8ByteLength(text, LARGE_TEXT_BYTES + 1) > LARGE_TEXT_BYTES) {
      let destination = { kind: "rect", row: r0, col: c0 };
      if (!forceSplit && this.selectedSet.size > 1 && isSingleValueText(text)) {
        const rect = this._singleSelectionRect();
        destination = {
          kind: "fill",
          target: rect
            ? { kind: "rect", row: rect.row, col: rect.col, rows: rect.rows, cols: rect.cols }
            : { kind: "cells", cells: this.selectedCells() },
        };
      } else if (!forceSplit && !text.includes("\t") && !multiLineTextLines) {
        destination = { kind: "singleCell", row: r0, col: c0 };
      }
      await this._pasteExternalLarge(text, destination);
      return;
    }
    const single = text.replace(/[\r\n]+$/, "");
    if (!forceSplit && single && this.selectedSet.size > 1
        && !single.includes("\t") && !single.includes("\n")) {
      await this._fillSelection(single);
      return;
    }
    // 内部分区判据：剪贴板文本与本软件上次复制一致（Web 无法写自定义 MIME）
    const mine = this.lastCopiedText &&
      (text === this.lastCopiedText ||
       text === this.lastCopiedText.replace(/\r\n$/, "") ||
       text.replace(/\r\n$/, "") === this.lastCopiedText.replace(/\r\n$/, ""));
    if (!forceSplit && !mine && !text.includes("\t") && !multiLineTextLines) {
      // CSV 外部无 Tab 纯文本：整段进当前格（长文粘贴），换行保留
      Promise.resolve(
        this.model.setData(r0, c0, text.replace(/\r\n/g, "\n").replace(/[\r\n]+$/, "")),
      ).catch((err) => this.hooks.statusMsg(String(err?.message ?? err), 3000));
      this.hooks.statusMsg("已粘贴到当前单元格", 2000);
      return;
    }
    const parsed = parseCsv(text, "\t");
    const rows = parsed.rows.length
      ? [parsed.headers, ...parsed.rows]
      : (parsed.headers.length ? [parsed.headers] : []);
    if (!rows.length) return;
    const width = Math.max(...rows.map((row) => row.length));
    // 超宽、扩行与既有格修改统一由引擎合并为一次可撤销粘贴。
    await this._pasteBig(r0, c0, rows, width);
  }

  /**
   * 一对多填充：把单值写入选区每一格，整个选区只占一步撤销。
   * 同值过滤以引擎批前旧值为准，不能读取可能尚未加载的前端缓存作判断。
   *
   * @param {string} value - 填充值。
   * @returns {Promise<void>}
   */
  /** 大文本只提交目的地；解析、扩行和历史成本全部留在引擎流式路径。 */
  async _pasteExternalLarge(text, destination) {
    try {
      const outcome = await this.model.pasteExternalText(text, destination);
      if (outcome?.cancelled) {
        this.hooks.statusMsg("已取消粘贴", 2500);
        return;
      }
      if (destination.kind === "fill") {
        this.hooks.statusMsg(`已填充 ${this.selectedSet.size} 个单元格`, 2000);
      } else if (destination.kind === "singleCell") {
        this.hooks.statusMsg("已粘贴到当前单元格", 2000);
      } else {
        this.hooks.statusMsg("已粘贴大文本", 2000);
      }
    } catch (err) {
      this.hooks.statusMsg(String(err?.message ?? err), 3000);
    }
  }

  async _fillSelection(value) {
    const cells = this.selectedCells();
    if (!cells.length) return;
    try {
      await this.model.setCells(cells.map(([r, c]) => [r, c, value]));
    } catch (err) {
      this.hooks.statusMsg(String(err?.message ?? err), 3000);
      return;
    }
    this.hooks.statusMsg(`已填充 ${cells.length} 个单元格`, 2000);
  }

  /**
   * 引擎粘贴：既有格修改与自动扩行由一条 Paste 记录原子完成。
   *
   * @param {number} r0 - 起始行。
   * @param {number} c0 - 起始列。
   * @param {string[][]} rows - 解析后的行数据。
   * @param {number} width - 列宽。
   * @returns {Promise<void>}
   */
  async _pasteBig(r0, c0, rows, width) {
    try {
      const outcome = await this.model.pasteRows(r0, c0, rows);
      if (outcome?.cancelled) {
        this.hooks.statusMsg("已取消粘贴", 2500);
        return;
      }
      this.select([r0, c0], { silent: true });
      this._extendTo([r0 + rows.length - 1, c0 + width - 1]);
      this.hooks.statusMsg(`已粘贴 ${rows.length} 行`, 2000);
    } catch (err) {
      this.hooks.statusMsg(String(err?.message ?? err), 3000);
    }
  }

  // ============ 筛选（D2/D3） ============

  /**
   * 清空全部筛选条件与输入框（含多重筛选关键字；多重筛选开关状态保留）。
   *
   * @param {object} [opts] - 选项。
   * @param {boolean} [opts.anchor=true] - 用户主动取消筛选时锚定视口/选中
   *   （回到原行原位）；结构性清筛（装载/换档/列操作/保存/首行切换）传 false。
   * @returns {void}
   */
  clearFilters({ anchor = true } = {}) {
    this.filters.clear();
    this.filterModes.clear();
    this.multi.text = "";
    this._suppressFilterInput = true;
    for (const input of this.headerEl.querySelectorAll(".col-filter input")) {
      input.value = "";
    }
    for (const btn of this.headerEl.querySelectorAll(".col-strict")) {
      btn.dataset.mode = "0";
      btn.textContent = STRICT_GLYPHS[0];
      btn.title = STRICT_TITLES[0];
    }
    this._suppressFilterInput = false;
    // 经临时旗标传给 _refilter（同步读完即复位；未初始化(undefined)视为锚定）
    this._clearAnchorPref = anchor;
    try {
      this._refilter();
    } finally {
      this._clearAnchorPref = true;
    }
  }

  /**
   * 多重筛选开关（闩锁，长期选中）。开启时列名前出现复选框（默认全勾）。
   *
   * @param {boolean} on - 是否开启。
   */
  setMultiOn(on) {
    this.multi.on = on;
    if (on && this.multi.cols.size === 0 && this.colOrder.length) {
      this.multi.cols = new Set(this.colOrder);
    }
    this._renderHeader();
    this._refilter();
    this.hooks.bigFilterInput?.(); // 开/关多重筛选改变匹配语义
  }

  /**
   * 设置多重筛选关键字（勾选列间按"或"匹配，忽略大小写）。
   *
   * @param {string} text - 关键字；空串即不生效。
   */
  setMultiText(text) {
    this.multi.text = text;
    this._refilter();
    this.hooks.bigFilterInput?.();
  }

  /** @returns {number} 生效的筛选条件数 */
  filterCount() { return this.filters.size; }

  // ============ 缩放（G5） ============

  /**
   * 设置缩放因子（0.5–3.0，列宽行高按比同步，对齐 Qt _after_zoom）。
   *
   * @param {number} factor - 目标缩放因子。
   */
  setZoom(factor) {
    factor = Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, factor));
    if (Math.abs(factor - this.zoom) < 1e-6) return;
    const ratio = factor / this.zoom;
    this.zoom = factor;
    this.colW = this.colW.map((w) => Math.max(MIN_COL_W, Math.round(w * ratio)));
    this._uniformRowH = Math.max(MIN_ROW_H, Math.round(this._uniformRowH * ratio));
    this.areaEl.style.setProperty("--grid-zoom", String(factor));
    this.renderAll();
    this.hooks.zoomed(factor);
  }
}
