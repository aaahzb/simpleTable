/**
 * 表格数据模型（table_model.py 的 JS 移植）。
 *
 * 持有表头与二维字符串数据；内置快照式撤销/重做（改动前自动存档）。
 * 事件回调代替 Qt 信号：onData(cb) 数据/结构变化，onUndoStack(cb) 撤销栈状态。
 * 所有单元格地位相同：均可格内编辑，也可在右侧编辑窗格编辑。
 */

/** 排序方式：字符 */
export const SORT_TEXT = "text";
/** 排序方式：数字 */
export const SORT_NUMERIC = "numeric";

/** 撤销栈上限（超出丢弃最旧快照） */
export const MAX_UNDO = 100;

/** 大表字符预算：全表字符数超过则撤销上限降为 MAX_UNDO_LARGE（防内存爆涨） */
const LARGE_CHARS = 3_000_000;
/** 大表撤销上限 */
const MAX_UNDO_LARGE = 20;

/**
 * 尝试把字符串解析为浮点数。
 *
 * @param {string} text - 单元格文本。
 * @returns {number | null} 解析成功返回数值，否则 null。
 */
function toFloat(text) {
  const t = text.trim();
  if (t === "") {
    return null;
  }
  const v = Number(t);
  return Number.isFinite(v) ? v : null;
}

export class TableModel {
  /**
   * 初始化空模型。
   */
  constructor() {
    /** @type {string[]} */
    this._headers = [];
    /** @type {string[][]} */
    this._rows = [];
    /** @type {Array<[string[], string[][]]>} */
    this._undoStack = [];
    /** @type {Array<[string[], string[][]]>} */
    this._redoStack = [];
    /** @type {number} batch 嵌套深度 */
    this._batchDepth = 0;
    /** @type {[number, number] | null} 击键合并目标格 */
    this._mergeCell = null;
    /** @type {Set<Function>} */
    this._dataCbs = new Set();
    /** @type {Set<Function>} */
    this._undoCbs = new Set();
    /** @type {Set<Function>} 整体重置（load/撤销恢复）回调 */
    this._resetCbs = new Set();
    /** @type {number | null} 动态撤销上限（null = 未测算） */
    this._undoCap = null;
    /** @type {boolean} 大表降级是否已提示（提示一次） */
    this._undoCapNotified = false;
    /** @type {Set<Function>} 撤销降级提示回调 */
    this._undoCapCbs = new Set();
    /** @type {boolean} 首行是否为普通数据（无列名模式） */
    this._headerless = false;
  }

  /**
   * 是否处于「首行非列名」模式。
   *
   * @returns {boolean} 首行作为数据时为 true。
   */
  isHeaderless() {
    return this._headerless;
  }

  /**
   * 切换「首行非列名」模式。
   *
   * 开启：原表头成为第一条数据，列名虚拟为 列1..N。
   * 关闭：当前第一条数据（含未保存编辑）提升为列名并从数据中移除。
   * 开关属于视图属性：清空撤销/重做，但不触发整体重置（不标脏）。
   *
   * @param {boolean} on - true=首行不是列名；false=首行是列名。
   * @returns {void}
   * @throws {Error} 开启时没有任何列，或关闭时没有任何数据行。
   */
  setHeaderless(on) {
    if (on === this._headerless) {
      return;
    }
    if (on) {
      if (!this._headers.length) {
        throw new Error("没有列可作为列名");
      }
      this._rows.unshift(this._headers.slice());
      this._headers = this._genericHeaders(this._headers.length);
    } else {
      if (!this._rows.length) {
        throw new Error("没有数据行可作为列名");
      }
      this._headers = this._rows.shift().slice();
    }
    this._headerless = on;
    // 行/列坐标已改变，旧快照不可再撤销。
    this._undoStack = [];
    this._redoStack = [];
    this._mergeCell = null;
    this._undoCap = null;
    this._undoCapNotified = false;
    this._emitData(true, "view");
    this._emitUndo();
  }

  /**
   * 生成列1..列N。
   *
   * @param {number} count - 列数。
   * @returns {string[]} 虚拟列名数组。
   */
  _genericHeaders(count) {
    return Array.from({ length: count }, (_, i) => `列${i + 1}`);
  }

  /**
   * 无列名模式下按当前列数重新生成虚拟列名（列增删后调用）。
   *
   * @returns {void}
   */
  _refreshGenericHeaders() {
    if (this._headerless) {
      this._headers = this._genericHeaders(this._headers.length);
    }
  }

  /**
   * 订阅"大表撤销降级"提示（每装载只发一次）。
   *
   * @param {(cap: number) => void} cb - 回调，参数为实际生效的撤销上限。
   * @returns {void}
   */
  onUndoCap(cb) {
    this._undoCapCbs.add(cb);
  }

  /**
   * 订阅整体重置（load 与 undo/redo 恢复）：用于"打开文件不标脏"等区分。
   *
   * @param {() => void} cb - 回调。
   * @returns {void}
   */
  onReset(cb) {
    this._resetCbs.add(cb);
  }

  /**
   * 触发整体重置通知（在数据通知之前发出）。
   *
   * @returns {void}
   */
  _emitReset() {
    for (const cb of this._resetCbs) {
      cb();
    }
  }

  // ---- 事件 ----
  /**
   * 订阅数据/结构变化（每次改动后触发；结构变化传 structural=true）。
   *
   * @param {(structural: boolean, origin?: string) => void} cb - 回调；
   *   origin="view" 表示纯视图变化（如首行非列名开关），不应标脏。
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
   * 触发数据变化通知。
   *
   * @param {boolean} [structural] - 是否结构变化（行列增删/排序/装载）。
   * @param {string} [origin] - 事件来源："edit"（默认）/ "view"（纯视图）。
   * @returns {void}
   */
  _emitData(structural = false, origin = "edit") {
    for (const cb of this._dataCbs) {
      cb(structural, origin);
    }
  }

  /**
   * 触发撤销栈状态通知。
   *
   * @returns {void}
   */
  _emitUndo() {
    for (const cb of this._undoCbs) {
      cb();
    }
  }

  // ---- 数据装载与读取 ----
  /**
   * 整体替换模型数据（打开/新建 = 清空撤销历史，与 Excel 一致）。
   *
   * @param {string[]} headers - 表头。
   * @param {string[][]} rows - 数据行（列数须与表头一致）。
   * @param {{headerless?: boolean}} [opts] - headerless=true 表示首行
   *   也是数据（headers 须为 parseCsv 生成的 列1..N）。
   * @returns {void}
   */
  load(headers, rows, { headerless = false } = {}) {
    this._headers = headers.slice();
    this._rows = rows.map((r) => r.slice());
    this._headerless = !!headerless;
    this._undoStack = [];
    this._redoStack = [];
    this._mergeCell = null;
    this._undoCap = null; // 重新测算动态撤销上限
    this._undoCapNotified = false;
    this._emitReset();
    this._emitData(true);
    this._emitUndo();
  }

  /**
   * @returns {string[]} 表头副本。
   */
  headers() {
    return this._headers.slice();
  }

  /**
   * @returns {string[][]} 全部数据行副本（供保存 CSV 用）。
   */
  allRows() {
    return this._rows.map((r) => r.slice());
  }

  /**
   * @returns {number} 数据行数。
   */
  rowCount() {
    return this._rows.length;
  }

  /**
   * @returns {number} 列数。
   */
  columnCount() {
    return this._headers.length;
  }

  /**
   * 单元格完整内容（供筛选与编辑窗格使用）。
   *
   * @param {number} row - 行号。
   * @param {number} col - 列号。
   * @returns {string} 完整文本。
   */
  rawData(row, col) {
    return this._rows[row][col];
  }

  /**
   * 显示层文本：换行/Tab 压平为空格（单元格恒为单行不变形）。
   *
   * @param {number} row - 行号。
   * @param {number} col - 列号。
   * @returns {string} 压平后的文本。
   */
  displayData(row, col) {
    return this._rows[row][col]
      .replace(/\r/g, " ").replace(/\n/g, " ").replace(/\t/g, " ");
  }

  // ---- 撤销/重做（快照式：改动前自动存档）----
  /**
   * 生成当前数据快照。
   *
   * @returns {[string[], string[][]]} (表头副本, 行数据副本)。
   */
  _snapshot() {
    return [this._headers.slice(), this._rows.map((r) => r.slice())];
  }

  /**
   * 数据改动前压入撤销快照（所有改动方法在变更前调用）。
   *
   * @param {[number, number] | null} [mergeKey] - 可合并改动的目标格，
   *   null 表示不可合并。
   * @returns {void}
   */
  _checkpoint(mergeKey = null) {
    const hadRedo = this._redoStack.length > 0;
    this._redoStack = [];
    // 大表动态降级：全表字符超预算时撤销上限收紧（首次压栈时测算一次）
    if (this._undoCap === null) {
      let chars = 0;
      for (const r of this._rows) {
        for (const c of r) chars += c.length;
      }
      this._undoCap = chars > LARGE_CHARS ? MAX_UNDO_LARGE : MAX_UNDO;
    }
    let pushed = false;
    if (this._batchDepth > 0) {
      // batch() 进入时已压过快照
    } else if (mergeKey !== null && this._mergeCell !== null &&
               mergeKey[0] === this._mergeCell[0] &&
               mergeKey[1] === this._mergeCell[1]) {
      // 同格连续击键：快照已存在，合并为一步
    } else {
      this._undoStack.push(this._snapshot());
      while (this._undoStack.length > this._undoCap) {
        this._undoStack.shift();
      }
      this._mergeCell = mergeKey;
      pushed = true;
      if (this._undoCap < MAX_UNDO && !this._undoCapNotified) {
        this._undoCapNotified = true;
        for (const cb of this._undoCapCbs) cb(this._undoCap);
      }
    }
    if (pushed || hadRedo) {
      this._emitUndo();
    }
  }

  /**
   * 批量改动：fn 整个调用合并为一步撤销（如粘贴一片区域）。
   *
   * @param {() => void} fn - 批量操作函数。
   * @returns {void}
   */
  batch(fn) {
    if (this._batchDepth === 0) {
      this._checkpoint();
    }
    this._batchDepth += 1;
    try {
      fn();
    } finally {
      this._batchDepth -= 1;
    }
  }

  /**
   * 写入单元格并把同格连续写入合并为一步撤销（编辑窗格击键用）。
   *
   * @param {number} row - 行号。
   * @param {number} col - 列号。
   * @param {string} value - 新文本。
   * @returns {void}
   */
  setDataMerged(row, col, value) {
    if (!this._valid(row, col)) {
      return;
    }
    this._checkpoint([row, col]);
    this._rows[row][col] = String(value);
    this._emitData(false);
  }

  /**
   * @returns {boolean} 是否有可撤销的改动。
   */
  canUndo() {
    return this._undoStack.length > 0;
  }

  /**
   * @returns {boolean} 是否有可重做的改动。
   */
  canRedo() {
    return this._redoStack.length > 0;
  }

  /**
   * 撤销一步改动。
   *
   * @returns {boolean} 有改动被撤销为 true。
   */
  undo() {
    if (!this._undoStack.length) {
      return false;
    }
    this._redoStack.push(this._snapshot());
    this._restore(this._undoStack.pop());
    return true;
  }

  /**
   * 重做一步改动。
   *
   * @returns {boolean} 有改动被重做为 true。
   */
  redo() {
    if (!this._redoStack.length) {
      return false;
    }
    this._undoStack.push(this._snapshot());
    this._restore(this._redoStack.pop());
    return true;
  }

  /**
   * 用快照整体替换当前数据（视图随之整体重建）。
   *
   * @param {[string[], string[][]]} snapshot - (表头, 行数据)。
   * @returns {void}
   */
  _restore(snapshot) {
    this._headers = snapshot[0];
    this._rows = snapshot[1];
    this._mergeCell = null;
    this._emitReset();
    this._emitData(true);
    this._emitUndo();
  }

  /**
   * 校验单元格坐标。
   *
   * @param {number} row - 行号。
   * @param {number} col - 列号。
   * @returns {boolean} 是否有效。
   */
  _valid(row, col) {
    return row >= 0 && row < this._rows.length &&
      col >= 0 && col < this._headers.length;
  }

  /**
   * 普通写入单元格（一步一快照）。
   *
   * @param {number} row - 行号。
   * @param {number} col - 列号。
   * @param {string} value - 新文本。
   * @returns {void}
   */
  setData(row, col, value) {
    if (!this._valid(row, col)) {
      return;
    }
    this._checkpoint();
    this._rows[row][col] = String(value);
    this._emitData(false);
  }

  // ---- 行列操作 ----
  /**
   * 末尾追加空行。
   *
   * @param {number} [count] - 追加行数。
   * @returns {void}
   */
  appendRows(count = 1) {
    if (count <= 0 || !this._headers.length) {
      return;
    }
    this._checkpoint();
    for (let i = 0; i < count; i += 1) {
      this._rows.push(new Array(this._headers.length).fill(""));
    }
    this._emitData(true);
  }

  /**
   * 删除指定的若干行。
   *
   * @param {number[]} rows - 行号列表。
   * @returns {void}
   * @throws {Error} 无列名模式试图删掉最后一行（首行还要留作列名来源）。
   */
  removeRows(rows) {
    const valid = new Set(rows.filter((r) => r >= 0 && r < this._rows.length));
    if (this._headerless && this._rows.length > 0 &&
        valid.size >= this._rows.length) {
      throw new Error("无列名模式需至少保留一行");
    }
    this._checkpoint();
    const sorted = Array.from(valid).sort((a, b) => b - a);
    for (const r of sorted) {
      if (r >= 0 && r < this._rows.length) {
        this._rows.splice(r, 1);
      }
    }
    this._emitData(true);
  }

  /**
   * 在指定位置插入空行。
   *
   * @param {number} row - 插入位置（该行号之前），越界自动夹取。
   * @param {number} [count] - 插入行数。
   * @returns {void}
   */
  insertRows(row, count = 1) {
    if (count <= 0 || !this._headers.length) {
      return;
    }
    const at = Math.max(0, Math.min(row, this._rows.length));
    this._checkpoint();
    const blank = () => new Array(this._headers.length).fill("");
    for (let i = 0; i < count; i += 1) {
      this._rows.splice(at, 0, blank());
    }
    this._emitData(true);
  }

  /**
   * 末尾追加一列。
   *
   * @param {string} name - 列名。
   * @returns {void}
   */
  appendColumn(name) {
    this._checkpoint();
    this._headers.push(name);
    for (const row of this._rows) {
      row.push("");
    }
    this._refreshGenericHeaders();
    this._emitData(true);
  }

  /**
   * 在指定位置插入列，自动命名（新列、新列2…）。
   *
   * @param {number} col - 插入位置（该列号之前），越界自动夹取。
   * @param {number} [count] - 插入列数。
   * @returns {void}
   */
  insertColumns(col, count = 1) {
    if (count <= 0) {
      return;
    }
    const at = Math.max(0, Math.min(col, this._headers.length));
    this._checkpoint();
    for (let i = 0; i < count; i += 1) {
      this._headers.splice(at + i, 0,
        this._headerless ? `列${this._headers.length + 1}` : this._uniqueColumnName("新列"));
      for (const row of this._rows) {
        row.splice(at + i, 0, "");
      }
    }
    this._refreshGenericHeaders();
    this._emitData(true);
  }

  /**
   * 生成不与现有表头冲突的列名。
   *
   * @param {string} base - 基础名。
   * @returns {string} base 本身（无冲突时）或 base+序号（从 2 起）。
   */
  _uniqueColumnName(base) {
    if (!this._headers.includes(base)) {
      return base;
    }
    let n = 2;
    while (this._headers.includes(`${base}${n}`)) {
      n += 1;
    }
    return `${base}${n}`;
  }

  /**
   * 删除一列及其全部数据。
   *
   * @param {number} col - 列号。
   * @returns {void}
   */
  removeColumn(col) {
    if (!(col >= 0 && col < this._headers.length)) {
      return;
    }
    this._checkpoint();
    this._headers.splice(col, 1);
    for (const row of this._rows) {
      row.splice(col, 1);
    }
    this._refreshGenericHeaders();
    this._emitData(true);
  }

  /**
   * 批量删除列（合并为一步撤销）。
   *
   * @param {number[]} cols - 列号列表。
   * @returns {void}
   */
  removeColumns(cols) {
    const sorted = Array.from(new Set(cols)).sort((a, b) => a - b);
    if (!sorted.length) {
      return;
    }
    this.batch(() => {
      for (const col of sorted.slice().reverse()) {
        this.removeColumn(col);
      }
    });
    this._emitData(true);
  }

  /**
   * 重命名列。
   *
   * @param {number} col - 列号。
   * @param {string} name - 新列名。
   * @returns {void}
   */
  renameColumn(col, name) {
    if (this._headerless) {
      return; // 列名是自动生成的，无列名模式不允许重命名。
    }
    this._checkpoint();
    this._headers[col] = name;
    this._emitData(true);
  }

  /**
   * 清除指定行的全部内容（行本身保留）。
   *
   * @param {number[]} rows - 行号列表。
   * @returns {void}
   */
  clearRows(rows) {
    const valid = Array.from(new Set(rows))
      .filter((r) => r >= 0 && r < this._rows.length);
    if (!valid.length || !this._headers.length) {
      return;
    }
    this._checkpoint();
    for (const r of valid) {
      this._rows[r] = new Array(this._headers.length).fill("");
    }
    this._emitData(false);
  }

  /**
   * 清除指定列的全部内容（列本身保留）。
   *
   * @param {number[]} cols - 列号列表。
   * @returns {void}
   */
  clearColumns(cols) {
    const valid = Array.from(new Set(cols))
      .filter((c) => c >= 0 && c < this._headers.length);
    if (!valid.length || !this._rows.length) {
      return;
    }
    this._checkpoint();
    for (const row of this._rows) {
      for (const c of valid) {
        row[c] = "";
      }
    }
    this._emitData(false);
  }

  /**
   * 清除指定单元格集合的内容（结构保留）。
   *
   * @param {Array<[number, number]>} cells - (行号, 列号) 列表。
   * @returns {void}
   */
  clearCells(cells) {
    const valid = cells.filter(([r, c]) => this._valid(r, c));
    if (!valid.length) {
      return;
    }
    this._checkpoint();
    for (const [r, c] of valid) {
      this._rows[r][c] = "";
    }
    this._emitData(false);
  }

  // ---- 物理排序 ----
  /**
   * 按优先级层级物理排序（真实重排数据；稳定排序保持同级原序）。
   *
   * @param {Array<[number, string, boolean]>} levels -
   *   (列号, 方式, 是否升序) 列表，列表顺序即优先级；
   *   方式为 SORT_TEXT 或 SORT_NUMERIC。
   *   数字方式下无法解析的单元格在该层级视为相等并排在最后。
   * @returns {void}
   */
  sortRows(levels) {
    if (!levels.length) {
      return;
    }
    /**
     * 比较两行。
     *
     * @param {string[]} r1 - 行 A。
     * @param {string[]} r2 - 行 B。
     * @returns {number} 负数/0/正数。
     */
    const cmp = (r1, r2) => {
      for (const [col, mode, asc] of levels) {
        const a = r1[col];
        const b = r2[col];
        const dir = asc ? 1 : -1;
        if (mode === SORT_NUMERIC) {
          const fa = toFloat(a);
          const fb = toFloat(b);
          if (fa === null && fb === null) {
            continue; // 都非数字：该层视为相等，落入下一层级
          }
          if (fa === null) {
            return 1; // 非数字恒排最后（与升降序无关）
          }
          if (fb === null) {
            return -1;
          }
          if (fa !== fb) {
            return (fa < fb ? -1 : 1) * dir;
          }
        } else if (a !== b) {
          return (a < b ? -1 : 1) * dir;
        }
      }
      return 0;
    };
    this._checkpoint();
    // Array.prototype.sort 稳定（ES2019+），与原 cmp 排序语义一致
    this._rows.sort(cmp);
    this._emitData(true);
  }
}
