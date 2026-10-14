/**
 * 虚拟网格视图：两带式表头 + 可视行渲染 + 选区模型 + 格内编辑器 +
 * 列宽/行高拖拽 + 列拖动排序 + 剪贴板 + Ctrl+滚轮缩放。
 *
 * 对齐 Qt 版 ZoomableTableView + FilterHeaderView 行为：
 * - 行虚拟滚动（前缀和定位），列全量渲染（列数通常远小于行数）
 * - 选区 = 若干矩形块 + 当前格；Ctrl 多块、Shift 块选、拖选、行列头整选
 * - 编辑键位：Enter 下移 / Shift+Enter 上移 / Tab 右移 / Shift+Tab 左移 /
 *   Alt+Enter 换行 / Esc 取消 / 打字即编辑（替换式）
 * - 缩放 0.5–3.0 步进 1.15：列宽按比缩放（最小 24），行高按比缩放（最小 10），
 *   表头/筛选带随 em 自动跟随
 */

import { parseCsv, serializeRecords } from "./csv.js";

export const ZOOM_MIN = 0.5;
export const ZOOM_MAX = 3.0;
export const ZOOM_STEP = 1.15;
const MIN_COL_W = 24;
const MIN_ROW_H = 10;

/**
 * 网格视图类。
 */
export class Grid {
  /**
   * @param {object} opts - 构造参数。
   * @param {import("./model.js").TableModel} opts.model - 数据模型。
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
   */
  constructor({ model, scrollEl, canvasEl, headerEl, rowsEl, editorEl, hooks }) {
    this.model = model;
    this.scrollEl = scrollEl;
    this.canvasEl = canvasEl;
    this.headerEl = headerEl;
    this.rowsEl = rowsEl;
    this.editorEl = editorEl;
    this.hooks = hooks;

    this.zoom = 1;
    /** @type {number[]} 每列宽度（当前缩放下 px，按源列号索引） */
    this.colW = [];
    /** @type {number[]} 每行高度（当前缩放下 px） */
    this.rowH = [];
    /** @type {number[]} 可见行行顶前缀和（px，筛选后重算） */
    this.visTops = [0];
    /** @type {number[]} 列顺序（源列号数组，位置即显示顺序） */
    this.colOrder = [];
    /** @type {Map<number, string>} 列号 → 筛选关键字 */
    this.filters = new Map();
    /** @type {number[]} 可见源行号（筛选后） */
    this.visibleRows = [];
    /** @type {{current: [number, number]|null, anchor: [number, number]|null,
     *   blocks: number[][]}} 选区（源坐标，块为 [r1,c1,r2,c2] 已归一） */
    this.sel = { current: null, anchor: null, blocks: [] };
    /** @type {Set<number>} 选中单元格集合（r * colCount + c） */
    this.selectedSet = new Set();
    /** @type {{r: number, c: number} | null} 编辑中的单元格 */
    this.editing = null;
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

  /** @returns {number} 行号列实际像素宽（em → px，随缩放） */
  _rowHeadW() {
    const fontPx = parseFloat(getComputedStyle(this.canvasEl.parentElement.parentElement).fontSize);
    return Math.round(3.2 * fontPx);
  }

  /** 重算列顺序与默认列宽（结构重建时） */
  rebuildStructure() {
    const n = this.model.columnCount();
    this.colOrder = Array.from({ length: n }, (_, i) => i);
    // 新列给 Qt 默认宽 ~100px；保留已有列宽
    const kept = this.colW;
    this.colW = Array.from({ length: n }, (_, i) =>
      i < kept.length ? kept[i] : Math.round(100 * this.zoom));
    const rows = this.model.rowCount();
    const keptH = this.rowH;
    const base = Math.round(1.8 * parseFloat(getComputedStyle(this.scrollEl).fontSize));
    this.rowH = Array.from({ length: rows }, (_, i) =>
      i < keptH.length ? keptH[i] : base);
    this._rebuildFilterInputs();
    this._refilter();
    this._clampSelection();
    this.renderAll();
  }

  /** 重算可见行前缀和与画布尺寸（筛选后画布只含可见行，无空隙） */
  _layout() {
    const tops = [0];
    let t = 0;
    for (const r of this.visibleRows) {
      t += this.rowH[r];
      tops.push(t);
    }
    this.visTops = tops;
    const totalW = this._rowHeadW() + this._effWidths().reduce((a, b) => a + b, 0);
    this.canvasEl.style.width = `${Math.max(totalW, this.scrollEl.clientWidth)}px`;
    // 画布高 = 吸顶表头 + 全部行（行是 #rows 内绝对定位，#rows 紧随表头之后）
    this.canvasEl.style.height =
      `${Math.max(this._originY() + t, this.scrollEl.clientHeight)}px`;
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
      const avail = this.scrollEl.clientWidth - this._rowHeadW();
      const sum = widths.reduce((a, b) => a + b, 0);
      if (sum < avail) {
        widths[widths.length - 1] += avail - sum;
      }
    }
    return widths;
  }

  /** @param {number} vi - 可见行序号。 @returns {number} 行顶 px。 */
  _rowTop(vi) { return this.visTops[vi]; }

  /**
   * @param {number} y - 画布 y 坐标。 @returns {number} 可见行序号（夹取）。
   */
  _rowAt(y) {
    const tops = this.visTops;
    let lo = 0, hi = Math.max(0, tops.length - 2);
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (tops[mid] <= y) lo = mid; else hi = mid - 1;
    }
    return lo;
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
    this._renderHeader();
    this._layout();
    this._renderRows();
  }

  /** 重绘列头（列名带 + 筛选带） */
  _renderHeader() {
    // 保留 corner，重建列单元格
    for (const el of [...this.headerEl.children]) {
      if (el.id !== "corner") el.remove();
    }
    const widths = this._effWidthsCache;
    this.colOrder.forEach((srcCol, pos) => {
      const cell = document.createElement("div");
      cell.className = "col-head-cell";
      cell.style.width = `${widths[pos]}px`;
      cell.dataset.col = String(srcCol);
      const name = document.createElement("div");
      name.className = "col-name";
      name.textContent = this.model.headers()[srcCol] ?? "";
      const filter = document.createElement("div");
      filter.className = "col-filter";
      const input = document.createElement("input");
      input.placeholder = "筛选…";
      input.value = this.filters.get(srcCol) ?? "";
      input.dataset.col = String(srcCol);
      filter.appendChild(input);
      const rz = document.createElement("div");
      rz.className = "col-resize";
      rz.dataset.col = String(srcCol);
      cell.append(name, filter, rz);
      this.headerEl.appendChild(cell);
    });
  }

  /** 重绘可视行（虚拟滚动，上下各 overscan 5 行） */
  _renderRows() {
    const host = this.rowsEl;
    host.textContent = "";
    const n = this.visibleRows.length;
    if (!n || !this.model.columnCount()) return;
    const viewH = this.scrollEl.clientHeight;
    const top = this.scrollEl.scrollTop;
    // 行画在吸顶表头之下：视口 y 换算成行区坐标再取窗口
    const oy = this._originY();
    const y0 = Math.max(0, top - oy);
    const y1 = Math.max(0, top + viewH - oy);
    const first = Math.max(0, this._rowAt(y0) - 5);
    const last = Math.min(n - 1, this._rowAt(y1) + 5);
    const widths = this._effWidthsCache;
    const cc = this.model.columnCount();
    const frag = document.createDocumentFragment();
    for (let vi = first; vi <= last; vi += 1) {
      const r = this.visibleRows[vi];
      const row = document.createElement("div");
      row.className = "row";
      // .row 的包含块是 #rows（其顶边已在吸顶表头之下），行内坐标即行区坐标
      row.style.top = `${this._rowTop(vi)}px`;
      row.style.height = `${this.rowH[r]}px`;
      row.style.width = `${this._rowHeadW() + widths.reduce((a, b) => a + b, 0)}px`;
      const head = document.createElement("div");
      head.className = "row-head";
      head.textContent = String(r + 1);
      head.dataset.row = String(r);
      const rz = document.createElement("div");
      rz.className = "row-resize";
      rz.dataset.row = String(r);
      head.appendChild(rz);
      row.appendChild(head);
      this.colOrder.forEach((srcCol, pos) => {
        const cell = document.createElement("div");
        cell.className = "cell";
        cell.style.width = `${widths[pos]}px`;
        cell.dataset.row = String(r);
        cell.dataset.col = String(srcCol);
        const key = r * cc + srcCol;
        if (this.selectedSet.has(key)) cell.classList.add("sel");
        if (this.sel.current && this.sel.current[0] === r && this.sel.current[1] === srcCol) {
          cell.classList.add("cur");
        }
        cell.textContent = this.model.displayData(r, srcCol);
        row.appendChild(cell);
      });
      frag.appendChild(row);
    }
    host.appendChild(frag);
  }

  /** 重建筛选输入框（列结构变化后），保留已有条件文本 */
  _rebuildFilterInputs() {
    // 实际 DOM 由 _renderHeader 统一重建；此处仅清理失效列的条件
    const n = this.model.columnCount();
    for (const col of [...this.filters.keys()]) {
      if (col >= n) this.filters.delete(col);
    }
  }

  /** 按当前条件重算可见行并刷新行渲染与状态 */
  refilter() { this._refilter(); }

  _refilter() {
    const cc = this.model.columnCount();
    const kws = [];
    for (const [col, kw] of this.filters) {
      if (col < cc && kw) kws.push([col, kw.toLowerCase()]);
    }
    const vis = [];
    for (let r = 0; r < this.model.rowCount(); r += 1) {
      let ok = true;
      for (const [col, kw] of kws) {
        if (!this.model.rawData(r, col).toLowerCase().includes(kw)) {
          ok = false;
          break;
        }
      }
      if (ok) vis.push(r);
    }
    this.visibleRows = vis;
    this._clampSelection();
    this._renderRows();
    this.hooks.countsChanged(vis.length, this.model.rowCount());
  }

  /**
   * 模型数据变化入口。
   *
   * @param {boolean} structural - 是否结构变化（行列增删/排序/装载）。
   */
  _onModelData(structural) {
    if (structural) {
      const rows = this.model.rowCount();
      if (rows !== this.rowH.length) {
        // 行结构变化：补齐/截断行高（fontSize 已含缩放）
        const base = Math.round(1.8 * parseFloat(getComputedStyle(this.scrollEl).fontSize));
        const kept = this.rowH;
        this.rowH = Array.from({ length: rows }, (_, i) => i < kept.length ? kept[i] : base);
      }
      if (this.model.columnCount() !== this.colW.length) {
        this.rebuildStructure();
        return;
      }
    }
    this._refilter();
    this.renderAll();
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
    for (const [r1, c1, r2, c2] of this.sel.blocks) {
      for (let r = Math.min(r1, r2); r <= Math.max(r1, r2); r += 1) {
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
      if (rh && !e.target.classList.contains("row-resize")) {
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
        const rr = this.visibleRows[this._rowAt(y)] ?? 0;
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
      } else if (this._drag.kind === "row-resize") {
        const d = this._drag;
        const h = Math.max(MIN_ROW_H, d.startH + e.clientY - d.startY);
        this.rowH[d.row] = Math.round(h);
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
      const rrz = e.target.closest(".row-resize");
      if (rrz) {
        const row = Number(rrz.dataset.row);
        this._drag = { kind: "row-resize", row, startY: e.clientY, startH: this.rowH[row] };
        e.preventDefault();
        e.stopPropagation();
      }
    }, true);

    // ---- 筛选输入 ----
    this.headerEl.addEventListener("input", (e) => {
      if (this._suppressFilterInput) return;
      const input = e.target.closest(".col-filter input");
      if (!input) return;
      const col = Number(input.dataset.col);
      const v = input.value;
      if (v) this.filters.set(col, v); else this.filters.delete(col);
      this._refilter();
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
    if (x < this._rowHeadW() || !this.visibleRows.length) return null;
    const r = this.visibleRows[this._rowAt(y)];
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
    const cur = this.sel.current;
    const mods = e.ctrlKey || e.metaKey;
    if (mods && !e.shiftKey && (e.key === "c" || e.key === "C")) {
      e.preventDefault(); this.copySelection(); return;
    }
    if (mods && !e.shiftKey && (e.key === "x" || e.key === "X")) {
      e.preventDefault(); this.cutSelection(); return;
    }
    if (mods && !e.shiftKey && (e.key === "v" || e.key === "V")) {
      e.preventDefault(); this.pasteClipboard(false); return;
    }
    if (mods && e.key === "z") { e.preventDefault(); this.hooks.undoRequested?.(); return; }
    if (mods && (e.key === "y" || (e.shiftKey && e.key === "Z"))) {
      e.preventDefault(); this.hooks.redoRequested?.(); return;
    }
    if (e.key === "Delete") { e.preventDefault(); this.deleteSelection(); return; }
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

  /** 滚动回左上角（装载/恢复时调用；WebView2 会记忆上次滚动位置） */
  resetScroll() {
    this.scrollEl.scrollTop = 0;
    this.scrollEl.scrollLeft = 0;
  }

  /** 选中区域滚动到可视（编辑提交移动后调用） */
  ensureVisible(r, c) {
    const vi = this.visibleRows.indexOf(r);
    if (vi < 0) return;
    const top = this._originY() + this._rowTop(vi);
    const h = this.rowH[r];
    const viewTop = this.scrollEl.scrollTop;
    const viewH = this.scrollEl.clientHeight;
    if (top < viewTop) this.scrollEl.scrollTop = top;
    else if (top + h > viewTop + viewH) this.scrollEl.scrollTop = top + h - viewH;
    const x = this._colLeft(this.colOrder.indexOf(c));
    const w = this.colW[c];
    const viewL = this.scrollEl.scrollLeft;
    const viewW = this.scrollEl.clientWidth;
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
    const vi = this.visibleRows.indexOf(r);
    el.style.top = `${vi >= 0 ? this._originY() + this._rowTop(vi) : 0}px`;
    el.style.width = `${Math.max(widths[pos], MIN_COL_W)}px`;
    el.style.height = `${Math.max(this.rowH[r], this._lineH())}px`;
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
    el.style.height = `${Math.max(this.rowH[this.editing?.r ?? 0] ?? lineH, want)}px`;
  }

  /** @returns {number} 编辑器行高 px */
  _lineH() {
    const fs = parseFloat(getComputedStyle(this.scrollEl).fontSize);
    return Math.round(fs * 1.45);
  }

  /** 提交编辑器内容到模型 */
  commitEdit() {
    if (!this.editing) return;
    const { r, c } = this.editing;
    const value = this.editorEl.value;
    this.closeEditor();
    this.model.setData(r, c, value);
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

  /** 复制选区为 TSV（含引号规则，与 Excel 互通） */
  copySelection() {
    if (!this.selectedSet.size) return;
    const rows = this.selectedRows();
    const cols = this.selectedCols();
    const cc = this.model.columnCount();
    const data = [];
    for (const r of rows) {
      const line = [];
      for (const c of cols) {
        line.push(this.selectedSet.has(r * cc + c) ? this.model.rawData(r, c) : "");
      }
      data.push(line);
    }
    const text = serializeRecords(data, "\t");
    this.lastCopiedText = text;
    navigator.clipboard.writeText(text).then(() => {
      this.hooks.statusMsg(`已复制 ${this.selectedSet.size} 个单元格`, 2000);
    });
  }

  /** 剪切 = 复制 + 清除选区 */
  cutSelection() {
    if (!this.selectedSet.size) return;
    this.copySelection();
    this.deleteSelection(false);
  }

  /**
   * Del：立即清除选区内容（Excel 行为，不弹确认）。
   *
   * @param {boolean} [say] - 是否发状态消息。
   */
  deleteSelection(say = true) {
    if (!this.selectedSet.size) return;
    const n = this.selectedSet.size;
    this.model.clearCells(this.selectedCells());
    if (say) this.hooks.statusMsg(`已清除 ${n} 个单元格的内容`, 2000);
  }

  /**
   * 粘贴（三级判据，对齐 Qt _paste_from_clipboard）。
   *
   * @param {boolean} forceSplit - 无条件按区域解析（右键"按行拆分粘贴"）。
   */
  async pasteClipboard(forceSplit = false) {
    let text;
    try {
      text = await navigator.clipboard.readText();
    } catch {
      this.hooks.statusMsg("粘贴失败：无法读取剪贴板", 3000);
      return;
    }
    if (!text) return;
    const cur = this.sel.current;
    if (!cur) return;
    const [r0, c0] = cur;
    // 内部分区判据：剪贴板文本与本软件上次复制一致（Web 无法写自定义 MIME）
    const mine = this.lastCopiedText &&
      (text === this.lastCopiedText ||
       text === this.lastCopiedText.replace(/\r\n$/, "") ||
       text.replace(/\r\n$/, "") === this.lastCopiedText.replace(/\r\n$/, ""));
    if (!forceSplit && !mine && !text.includes("\t")) {
      // 外部无 Tab 纯文本：整段进当前格（长文粘贴），换行保留
      this.model.setData(r0, c0, text.replace(/\r\n/g, "\n").replace(/[\r\n]+$/, ""));
      this.hooks.statusMsg("已粘贴到当前单元格", 2000);
      return;
    }
    const parsed = parseCsv(text, "\t");
    const rows = parsed.rows.length
      ? [parsed.headers, ...parsed.rows]
      : (parsed.headers.length ? [parsed.headers] : []);
    if (!rows.length) return;
    const width = Math.max(...rows.map((row) => row.length));
    // 扩展行列 + 逐格写入合并为一步撤销（Excel：粘贴是单个操作）
    this.model.batch(() => {
      const needRows = r0 + rows.length - this.model.rowCount();
      if (needRows > 0) this.model.insertRows(this.model.rowCount(), needRows);
      const needCols = c0 + width - this.model.columnCount();
      if (needCols > 0) this.model.insertColumns(this.model.columnCount(), needCols);
      rows.forEach((row, i) => {
        row.forEach((val, j) => {
          this.model.setData(r0 + i, c0 + j, val);
        });
      });
    });
    // 选中刚粘贴的区域（Excel 行为；被筛掉的行不选）
    const visSet = new Set(this.visibleRows);
    let selMade = false;
    for (let i = 0; i < rows.length; i += 1) {
      if (visSet.has(r0 + i)) {
        this.select([r0, c0], { silent: true });
        this._extendTo([r0 + rows.length - 1, c0 + width - 1]);
        selMade = true;
        break;
      }
    }
    if (!selMade) this.select([r0, c0]);
    this.hooks.statusMsg(`已粘贴 ${rows.length} 行`, 2000);
  }

  // ============ 筛选（D2/D3） ============

  /** 清空全部筛选条件与输入框 */
  clearFilters() {
    this.filters.clear();
    this._suppressFilterInput = true;
    for (const input of this.headerEl.querySelectorAll(".col-filter input")) {
      input.value = "";
    }
    this._suppressFilterInput = false;
    this._refilter();
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
    this.rowH = this.rowH.map((h) => Math.max(MIN_ROW_H, Math.round(h * ratio)));
    document.getElementById("grid-area").style.setProperty("--grid-zoom", String(factor));
    this.renderAll();
    this.hooks.zoomed(factor);
  }
}
