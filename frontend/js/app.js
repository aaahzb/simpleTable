/**
 * simpleTable 前端总装：工具栏、状态栏、文件操作、右键菜单、
 * 编辑窗格同步、全局快捷键、拖放打开、关闭确认、撤销重做整合。
 *
 * 行为逐项对齐 Qt 版 main_window.py；数据模型/网格/对话框见各自模块。
 */

import { parseCsv, serializeCsv } from "./csv.js";
import { TableModel } from "./model.js";
import { BigModel, BIG_THRESHOLD } from "./bigmodel.js";
import { Grid } from "./grid.js";
import { EditPane } from "./pane.js";
import { showMenu } from "./menu.js";
import {
  columnDialog, confirmDialog, confirmDiscardDialog, errorDialog,
  infoDialog, intDialog, newTableDialog, progressDialog, sortDialog,
} from "./dialogs.js";

/** @type {any} Tauri v2 全局对象（withGlobalTauri） */
const tauri = window.__TAURI__;
const invoke = (cmd, args) => tauri.core.invoke(cmd, args);

/** 新建表格的初始空行数（对齐 Qt NEW_TABLE_ROWS） */
const NEW_TABLE_ROWS = 20;

// ============ 全局状态 ============

/** 当前模型：小模式 TableModel / 大模式 BigModel，打开文件时热交换 */
let model = new TableModel();
let path = null;      // 当前文件路径（null = 未命名）
let dirty = false;    // 未保存修改
/** @type {number} 大模式最近一次筛选用时（ms），状态栏展示 */
let lastFilterMs = 0;
/** @type {[number, number] | null} 编辑窗格跟踪的源坐标 */
let currentCell = null;
let dockOrigin = false; // 编辑窗格发起的写回中（避免回灌）

// ============ 状态栏（E5） ============

const statusText = document.getElementById("status-text");
let statusTimer = 0;
let lastCounts = [0, 0];

/**
 * 状态栏瞬时消息（对齐 QStatusBar.showMessage 带超时）。
 *
 * @param {string} msg - 消息文本。
 * @param {number} [ms] - 显示毫秒数，默认 0 = 常驻到下一次刷新。
 */
function statusMsg(msg, ms = 0) {
  statusText.textContent = msg;
  clearTimeout(statusTimer);
  if (ms > 0) {
    statusTimer = setTimeout(updateStatusText, ms);
  }
}

/** 刷新左侧状态（显示 X / 共 Y 行；大模式筛选中显示匹配数与用时） */
function updateStatusText() {
  if (model.isBig && model.filterActive()) {
    statusText.textContent = `匹配 ${lastCounts[0]} / 共 ${model.meta()?.rows ?? lastCounts[1]} 行` +
      (lastFilterMs ? ` · 用时 ${(lastFilterMs / 1000).toFixed(1)} 秒` : "");
    return;
  }
  statusText.textContent = `显示 ${lastCounts[0]} / 共 ${lastCounts[1]} 行`;
}

/**
 * 行数计数变化（筛选/结构变化时由网格回调）。
 *
 * @param {number} visible - 筛选后行数。
 * @param {number} total - 总行数。
 */
function onCountsChanged(visible, total) {
  lastCounts = [visible, total];
  updateStatusText();
}

// ============ 标题（E6） ============

/** 刷新窗口标题（脏标记 + 路径名） */
function updateTitle() {
  const name = path || "未命名";
  const star = dirty ? "*" : "";
  invoke("set_title", { title: `${star}${name} - simpleTable` });
}

/** 标脏并刷新标题与状态栏（对齐 _mark_dirty） */
function markDirty() {
  dirty = true;
  updateTitle();
  updateStatusText();
}

// ============ 网格 ============

const grid = new Grid({
  model,
  scrollEl: document.getElementById("grid-scroll"),
  canvasEl: document.getElementById("grid-canvas"),
  headerEl: document.getElementById("col-header"),
  rowsEl: document.getElementById("rows"),
  editorEl: document.getElementById("cell-editor"),
  hooks: {
    statusMsg,
    countsChanged: onCountsChanged,
    zoomed: (f) => statusMsg(`表格缩放 ${Math.round(f * 100)}%`, 1500),
    undoRequested: () => undo(),
    redoRequested: () => redo(),
    currentChanged: (r, c) => {
      currentCell = [r, c];
      pane.setText(model.rawData(r, c));
    },
    cellMenu: (r, c, e) => showCellMenu(e),
    rowMenu: (r, e) => showRowMenu(e),
    colMenu: (c, e) => showColMenu(e),
    // 大模式：执行筛选（工具栏按钮 / 筛选框回车共用）
    bigFilterRun: () => runBigFilter(),
    // 大模式：筛选条件变化（仅记录，刷新"执行筛选"按钮态）
    bigFilterStatus: () => refreshBigUi(),
  },
});

// ============ 大文件模式 UI（按钮显隐/筛选执行/列操作 pending） ============

// 注：大模式工具按钮在下方工具栏段（tb 初始化后）创建；此处仅函数定义。

/** 按大模式状态刷新专属按钮显隐与可用态 */
function refreshBigUi() {
  const big = !!model.isBig;
  // 大模式不支持排序：按钮置灰 + 悬停提示（保持位置稳定，不隐藏）
  tb.sort.disabled = big;
  tb.sort.title = big ? "大文件模式暂不支持排序" : "";
  tb.execFilter.classList.toggle("hidden", !big);
  if (big) {
    const hasCond = grid.filters.size > 0 || !!grid.multi.text;
    tb.execFilter.disabled = !hasCond || model.colOpsPending();
    const pending = model.colOpsPending();
    tb.execColOps.classList.toggle("hidden", !pending);
    tb.cancelColOps.classList.toggle("hidden", !pending);
  } else {
    tb.execColOps.classList.add("hidden");
    tb.cancelColOps.classList.add("hidden");
  }
}

/**
 * 执行大文件筛选：收集条件 → 引擎整表扫描 → 状态栏报命中与用时。
 *
 * @returns {Promise<void>}
 */
async function runBigFilter() {
  const cc = model.columnCount();
  const kws = [];
  for (const [col, kw] of grid.filters) {
    if (col < cc && kw) kws.push([col, kw]);
  }
  const mKw = grid.multi.on ? grid.multi.text : "";
  const mCols = mKw ? [...grid.multi.cols].filter((c) => c < cc) : [];
  try {
    const t0 = performance.now();
    const n = await model.runFilter(kws, mCols, mKw);
    lastFilterMs = performance.now() - t0;
    statusMsg(`匹配 ${n} 行 · 用时 ${(lastFilterMs / 1000).toFixed(1)} 秒`, 4000);
    updateStatusText();
  } catch (err) {
    await errorDialog("筛选失败", String(err?.message ?? err));
  }
}

/** 执行 pending 列操作（物理重写 + 进度条） */
async function applyColOps() {
  if (!model.isBig || !model.colOpsPending()) return;
  const dlg = progressDialog("执行列操作…");
  const un = await model.onProgress((done, total) => dlg.set(done, total));
  try {
    await model.applyColOps();
    statusMsg("列操作已完成（物理重写，撤销历史已清空）", 4000);
  } catch (err) {
    await errorDialog("列操作失败", String(err?.message ?? err));
  } finally {
    un();
    dlg.close();
    refreshBigUi();
  }
}

/** 撤销（放弃）全部 pending 列操作 */
async function cancelColOps() {
  try {
    await model.clearColOps();
    statusMsg("已放弃列操作预览，恢复原结构", 3000);
  } catch (err) {
    await errorDialog("操作失败", String(err?.message ?? err));
  }
}

// ============ 编辑窗格（E1/E2/E3） ============

const pane = new EditPane({
  paneEl: document.getElementById("pane"),
  editEl: document.getElementById("pane-edit"),
  mirrorEl: document.getElementById("pane-mirror"),
  statusMsg,
  onEdit: (value) => {
    if (currentCell === null) return;
    const [r, c] = currentCell;
    dockOrigin = true;
    try {
      // 同格连续击键合并为一步撤销（对齐 set_data_merged）
      model.setDataMerged(r, c, value);
    } finally {
      dockOrigin = false;
    }
  },
});

// ============ 工具栏（E4） ============

const toolbar = document.getElementById("toolbar");
/** @type {Record<string, HTMLButtonElement>} */
const tb = {};
for (const spec of [
  ["new", "新建", () => newFile()],
  ["open", "打开", () => openFile()],
  ["save", "保存", () => saveFile()],
  ["saveAs", "另存为", () => saveFileAs()],
  "|",
  ["undo", "撤销", () => undo()],
  ["redo", "重做", () => redo()],
  "|",
  ["sort", "排序…", () => openSortDialog()],
  ["clearFilter", "清除筛选", () => {
    grid.clearFilters();
    multiInput.value = "";
  }],
  ["multiFilter", "多重筛选", (e) => {
    const on = e.currentTarget.classList.toggle("on");
    grid.setMultiOn(on);
    multiWrap.classList.toggle("hidden", !on);
    if (on) multiInput.focus();
  }],
]) {
  if (spec === "|") {
    const sep = document.createElement("div");
    sep.className = "sep";
    toolbar.appendChild(sep);
    continue;
  }
  const [key, text, fn] = spec;
  const b = document.createElement("button");
  b.textContent = text;
  b.addEventListener("click", fn);
  // 对齐 Qt 工具栏：点击不抢走键盘焦点，焦点留在窗格/网格，Ctrl+Z 等键位路由不乱
  b.addEventListener("mousedown", (e) => e.preventDefault());
  toolbar.appendChild(b);
  tb[key] = b;
}

// 大模式工具按钮：执行筛选（条件存在时可用）、执行/撤销列操作（pending 时出现）
for (const [key, text, fn, title] of [
  ["execFilter", "执行筛选", () => runBigFilter(), "对大文件执行一次筛选（Enter 同效）"],
  ["execColOps", "执行列操作", () => applyColOps(), "物理重写整个文件（显示进度），完成后不可撤销"],
  ["cancelColOps", "撤销列操作预览", () => cancelColOps(), "放弃待执行的列操作，恢复原结构"],
]) {
  const b = document.createElement("button");
  b.textContent = text;
  b.title = title;
  b.classList.add("hidden");
  b.addEventListener("click", fn);
  b.addEventListener("mousedown", (e) => e.preventDefault());
  toolbar.appendChild(b);
  tb[key] = b;
}
// 多重筛选输入框（紧随按钮，仅选中态显示）
const multiWrap = document.createElement("span");
multiWrap.id = "multi-filter-wrap";
multiWrap.className = "hidden";
const multiInput = document.createElement("input");
multiInput.id = "multi-filter-input";
multiInput.placeholder = "在勾选列中匹配（或）…";
multiInput.addEventListener("input", () => grid.setMultiText(multiInput.value));
multiWrap.appendChild(multiInput);
tb.multiFilter.after(multiWrap);

// 显示符号开关：位于「编辑窗格」标题右侧（功能不变）
const btnSymbols = document.createElement("button");
btnSymbols.textContent = "显示符号";
btnSymbols.addEventListener("click", () => {
  btnSymbols.classList.toggle("on");
  pane.setSymbols(btnSymbols.classList.contains("on"));
});
// 同样不抢焦点：符号模式常配合窗格编辑使用，点击后 Ctrl+Z 仍作用于窗格
btnSymbols.addEventListener("mousedown", (e) => e.preventDefault());
document.getElementById("pane-head").appendChild(btnSymbols);
tb.symbols = btnSymbols;
tb.symbols.title = "在编辑窗格显示空格(·)、Tab(→)、换行符(¶)，类似 Notepad++";
tb.undo.disabled = true;
tb.redo.disabled = true;

/** 按撤销栈状态刷新工具栏按钮（对齐 _update_undo_actions） */
function updateUndoButtons() {
  tb.undo.disabled = !model.canUndo();
  tb.redo.disabled = !model.canRedo();
}

// ============ 撤销/重做（G3） ============

/** 撤销一步表格改动（Ctrl+Z；大模型 undo 为异步） */
async function undo() {
  if (await model.undo()) {
    grid.clearSelection();
    clearDock();
    markDirty();
  }
}

/** 重做一步表格改动（Ctrl+Y / Ctrl+Shift+Z） */
async function redo() {
  if (await model.redo()) {
    grid.clearSelection();
    clearDock();
    markDirty();
  }
}

/** 清空编辑窗格与跟踪位置（对齐 _clear_dock） */
function clearDock() {
  currentCell = null;
  pane.clear();
}

// ============ 模型事件整合 ============

/** load/撤销恢复触发的整体重置：装载即干净（Qt：打开后 _dirty=False） */
let resetPending = false;

/**
 * 绑定模型事件（大小模式热交换时重绑；旧模型连同监听一并废弃）。
 *
 * @param {TableModel | BigModel} m - 模型实例。
 * @returns {void}
 */
function bindModelEvents(m) {
  m.onReset(() => {
    resetPending = true; // 紧随其后的 onData 是装载/恢复本身，不视为脏
    dirty = false; // 撤销/重做会由各自 wrapper 在返回前重新标脏
    updateTitle();
    clearDock();
    updateStatusText();
    grid.resetScroll(); // WebView2 会话记忆滚动位置，装载须回顶（对齐 Qt）
  });
  m.onData((structural) => {
    if (resetPending) resetPending = false;
    else markDirty();
    if (m.isBig) refreshBigUi();
    if (currentCell === null) return;
    const [r, c] = currentCell;
    if (structural) {
      // 行列增删/排序：越界清空，否则重载内容
      if (r >= m.rowCount() || c >= m.columnCount()) clearDock();
      else pane.setText(m.rawData(r, c));
      return;
    }
    // 编辑窗格自己发起的写回不再回灌（避免光标跳动）
    if (dockOrigin) return;
    const newText = m.rawData(r, c);
    if (pane.editEl.value !== newText) pane.setText(newText);
  });
  m.onUndoStack(updateUndoButtons);
  // 大表撤销降级：状态栏提示一次（小模型专属；BigModel 无此方法）
  m.onUndoCap?.((cap) => {
    statusMsg(`表格较大：撤销上限已降为 ${cap} 步`, 6000);
  });
}
bindModelEvents(model);

// ============ 文件操作（F2/F3/F4） ============

/** 新建表格：确认丢弃修改后弹出列定义对话框（F1） */
async function newFile() {
  if (!(await confirmDiscard())) return;
  const names = await newTableDialog();
  if (names === null) return;
  if (!names.length) {
    await infoDialog("新建表格", "至少需要一列。");
    return;
  }
  grid.clearFilters();
  await switchToModel(new TableModel());
  model.load(names, Array.from({ length: NEW_TABLE_ROWS }, () => names.map(() => "")));
  path = null;
  dirty = false;
  updateTitle();
  onCountsChanged(model.rowCount(), model.rowCount());
  refreshBigUi();
}

/** 打开 CSV：文件对话框选择 */
async function openFile() {
  if (!(await confirmDiscard())) return;
  const p = await invoke("pick_open_path");
  if (p) await openPath(p);
}

/**
 * 打开指定路径的 CSV（菜单打开与拖放打开共用）。
 * 按文件大小路由：≥ 阈值（或冒烟强制）走大文件模式（mmap 秒开），否则原路径。
 *
 * @param {string} p - CSV 文件路径。编码自动识别（Rust 层）。
 */
async function openPath(p) {
  let size;
  try {
    size = await invoke("file_size", { path: p });
  } catch (err) {
    await errorDialog("打开失败", String(err));
    return;
  }
  const forceBig = await invoke("smoke_force_big");
  if (forceBig || size >= BIG_THRESHOLD) {
    await openBigPath(p);
    return;
  }
  let result;
  try {
    result = await invoke("read_csv_file", { path: p });
  } catch (err) {
    await errorDialog("打开失败", String(err));
    return;
  }
  const { text } = result;
  const { headers, rows, warnings } = parseCsv(text);
  grid.clearFilters();
  await switchToModel(new TableModel());
  model.load(headers, rows);
  path = p;
  dirty = false;
  updateTitle();
  onCountsChanged(model.rowCount(), model.rowCount());
  refreshBigUi();
  if (warnings.length) {
    await infoDialog("打开提示", warnings.slice(0, 20).join("\n"));
  }
}

/**
 * 模型热交换：grid/窗格/工具栏切换到目标模型（大模式关闭旧会话）。
 *
 * @param {TableModel | BigModel} next - 目标模型。
 * @returns {Promise<void>}
 */
async function switchToModel(next) {
  if (model.isBig) await model.close().catch(() => {});
  model = next;
  grid.setModel(next);
  bindModelEvents(next);
  refreshBigUi();
}

/**
 * 大文件模式打开：mmap + 行索引，秒开；视图经窗口缓存按需取数。
 *
 * @param {string} p - CSV 文件路径。
 * @returns {Promise<void>}
 */
async function openBigPath(p) {
  const bm = new BigModel();
  let meta;
  try {
    // 先绑定事件再 open：open 内部的 reset/data 通知才能到达外壳
    bindModelEvents(bm);
    meta = await bm.open(p);
    model = bm;
    grid.setModel(bm);
  } catch (err) {
    await errorDialog("打开失败", String(err?.message ?? err));
    return;
  }
  grid.clearFilters();
  path = p;
  dirty = false;
  lastFilterMs = 0;
  updateTitle();
  onCountsChanged(model.rowCount(), model.rowCount());
  refreshBigUi();
  statusMsg(
    `大文件模式：${meta.rows} 行 × ${meta.cols} 列（${meta.encoding}），` +
    "筛选需点「执行筛选」", 6000);
}

/** 保存到当前路径（无路径时转另存为） */
async function saveFile() {
  if (path === null) {
    await saveFileAs();
    return;
  }
  if (model.isBig) {
    const dlg = progressDialog("保存大文件…");
    const un = await model.onProgress((done, total) => dlg.set(done, total));
    try {
      await model.save();
    } catch (err) {
      un();
      dlg.close();
      await errorDialog("保存失败", `${err?.message ?? err}\n文件可能被其他程序占用，可尝试另存为。`);
      return;
    }
    un();
    dlg.close();
    dirty = false;
    updateTitle();
    statusMsg("已保存（大文件模式：流式重写完成）", 3000);
    return;
  }
  try {
    await invoke("write_csv_file", {
      path,
      text: serializeCsv(model.headers(), model.allRows()),
    });
  } catch (err) {
    await errorDialog("保存失败", `${err}\n文件可能被其他程序占用，可尝试另存为。`);
    return;
  }
  dirty = false;
  updateTitle();
}

/** 另存为 CSV */
async function saveFileAs() {
  let p = await invoke("pick_save_path", { defaultName: "未命名.csv" });
  if (!p) return;
  if (!p.toLowerCase().endsWith(".csv")) p += ".csv";
  path = p;
  await saveFile();
}

/**
 * 有未保存修改时询问用户（对齐 _confirm_discard）。
 *
 * @returns {Promise<boolean>} true 表示可以继续。
 */
async function confirmDiscard() {
  if (!dirty) return true;
  const choice = await confirmDiscardDialog();
  if (choice === "save") {
    await saveFile();
    return !dirty; // 保存失败（如取消另存为）则视为不继续
  }
  return choice === "discard";
}

// ---- 大文件外部改动检测：窗口重新聚焦时校验 mtime/大小 ----
window.addEventListener("focus", async () => {
  if (!model.isBig || !path) return;
  try {
    if (await model.stale()) {
      const reload = await confirmDialog(
        "文件已被外部程序修改。重新加载将丢失当前未保存的修改，是否重载？");
      if (reload) await openBigPath(path);
    }
  } catch { /* 检测失败静默（会话可能已关闭） */ }
});

// ---- 拖放打开 ----
tauri.event.listen("tauri://drag-drop", async (e) => {
  const paths = e.payload?.paths ?? [];
  const p = paths.find((x) => x.toLowerCase().endsWith(".csv"));
  if (p && await confirmDiscard()) await openPath(p);
});

// ---- 关闭前确认 ----
tauri.event.listen("close-request", async () => {
  if (await confirmDiscard()) invoke("quit_app");
});

// ============ 排序（F5） ============

/** 打开排序对话框并执行物理排序；排序前清空选中避免行号失效 */
async function openSortDialog() {
  if (model.isBig) {
    statusMsg("大文件模式暂不支持排序", 3000);
    return;
  }
  const names = model.headers();
  if (!names.length) return;
  const levels = await sortDialog(names);
  if (!levels || !levels.length) return;
  grid.clearSelection();
  clearDock();
  model.sortRows(levels);
  markDirty();
}

// ============ 右键菜单（F6） ============

/**
 * 模型异步操作统一收尾：大模式各操作为 Promise，失败转错误对话框
 * （典型：列操作 pending 期间编辑/筛选被引擎拒绝）。
 *
 * @param {any} result - 模型操作返回值（Promise 或同步值）。
 * @returns {void}
 */
function op(result) {
  Promise.resolve(result).catch((err) => {
    errorDialog("操作失败", String(err?.message ?? err));
  });
}

/** 行头右键菜单：插入/添加/删除/清除行 */
function showRowMenu(e) {
  const rows = grid.selectedRows();
  if (!rows.length) return;
  showMenu(e.clientX, e.clientY, [
    { label: `在上方插入 ${rows.length} 行`,
      onClick: () => op(model.insertRows(Math.min(...rows), rows.length)) },
    { label: "在末尾添加 N 行…", onClick: appendRowsDialog },
    null,
    { label: `删除选中的 ${rows.length} 行…`, onClick: () => deleteRows(rows) },
    { label: `清除选中 ${rows.length} 行的内容…`, onClick: () => clearRows(rows) },
  ]);
}

/** 列头右键菜单：插入/删除/清除/重命名列 */
function showColMenu(e) {
  const cols = grid.selectedCols();
  if (!cols.length) return;
  const col = cols[0];
  showMenu(e.clientX, e.clientY, [
    { label: `在左侧插入 ${cols.length} 列`, onClick: () => insertColumnsAt(cols) },
    null,
    { label: `删除选中的 ${cols.length} 列…`, onClick: () => deleteColumns(cols) },
    { label: `清除选中 ${cols.length} 列的内容…`, onClick: () => clearColumns(cols) },
    null,
    { label: "重命名列…", onClick: () => renameColumn(col) },
  ]);
}

/** 单元格右键菜单：按选区跨度插入/删除/清除行列 + 拆分粘贴 */
function showCellMenu(e) {
  const rows = grid.selectedRows();
  const cols = grid.selectedCols();
  const n = grid.selectedCells().length;
  showMenu(e.clientX, e.clientY, [
    { label: "按行拆分粘贴", onClick: () => grid.pasteClipboard(true) },
    null,
    { label: `在上方插入 ${rows.length} 行`,
      onClick: () => op(model.insertRows(Math.min(...rows), rows.length)) },
    { label: `在左侧插入 ${cols.length} 列`, onClick: () => insertColumnsAt(cols) },
    null,
    { label: `删除选区跨越的 ${rows.length} 行…`, onClick: () => deleteRows(rows) },
    { label: `删除选区跨越的 ${cols.length} 列…`, onClick: () => deleteColumns(cols) },
    { label: `清除选区 ${n} 个单元格内容…`, onClick: () => clearSelCells(n) },
  ]);
}

/** 末尾添加 N 行对话框（默认 10，1–10000） */
async function appendRowsDialog() {
  const n = await intDialog("添加行", "在末尾添加的行数：", 10, 1, 10000);
  if (n !== null) op(model.insertRows(model.rowCount(), n));
}

/**
 * 二次确认后删除指定行。
 *
 * @param {number[]} rows - 源行号列表。
 */
async function deleteRows(rows) {
  if (rows.length && await confirmDialog(`确定删除选中的 ${rows.length} 行？（可用 Ctrl+Z 撤销）`)) {
    op(model.removeRows(rows));
  }
}

/**
 * 二次确认后清除指定行内容（行结构保留）。
 *
 * @param {number[]} rows - 源行号列表。
 */
async function clearRows(rows) {
  if (rows.length && await confirmDialog(`确定清除选中 ${rows.length} 行的全部内容？（行本身保留）`)) {
    op(model.clearRows(rows));
  }
}

/**
 * 在指定位置插入列并清空筛选（列号已错位，避免误筛）。
 *
 * @param {number[]} cols - 插入位置参考列号。
 */
function insertColumnsAt(cols) {
  op(model.insertColumns(Math.min(...cols), cols.length));
  grid.clearFilters();
}

/**
 * 二次确认后删除指定列并清空筛选。
 *
 * @param {number[]} cols - 列号列表。
 */
async function deleteColumns(cols) {
  if (cols.length && await confirmDialog(`确定删除选中的 ${cols.length} 列及其全部数据？（列操作将预览，需点「执行列操作」生效）`)) {
    op(model.removeColumns(cols));
    grid.clearFilters();
  }
}

/**
 * 二次确认后清除指定列内容（列结构保留）。
 *
 * @param {number[]} cols - 列号列表。
 */
async function clearColumns(cols) {
  if (cols.length && await confirmDialog(`确定清除选中 ${cols.length} 列的全部内容？（列本身保留）`)) {
    op(model.clearColumns(cols));
  }
}

/** 二次确认后清除选区单元格内容（结构保留） */
async function clearSelCells(n) {
  if (n && await confirmDialog(`确定清除选区 ${n} 个单元格的内容？（结构保留）`)) {
    op(model.clearCells(grid.selectedCells()));
  }
}

/**
 * 弹对话框重命名指定列。
 *
 * @param {number} col - 列号。
 */
async function renameColumn(col) {
  const name = await columnDialog("重命名列", model.headers()[col]);
  if (name) op(model.renameColumn(col, name));
}

// ============ 全局快捷键 ============

document.addEventListener("keydown", (e) => {
  const mods = e.ctrlKey || e.metaKey;
  const inField = ["TEXTAREA", "INPUT", "SELECT"].includes(document.activeElement?.tagName ?? "");
  if (mods && !e.shiftKey && (e.key === "s" || e.key === "S")) {
    e.preventDefault();
    saveFile();
    return;
  }
  // Ctrl+Z / Ctrl+Y：输入控件内让浏览器原生文本撤销接管
  if (inField) return;
  if (mods && !e.shiftKey && (e.key === "z" || e.key === "Z")) {
    e.preventDefault();
    undo();
  } else if (mods && ((e.key === "y" || e.key === "Y") ||
             (e.shiftKey && (e.key === "Z" || e.key === "z")))) {
    e.preventDefault();
    redo();
  }
});

// ============ 窗格分隔条拖拽 ============

(() => {
  const split = document.getElementById("splitter");
  const paneEl = document.getElementById("pane");
  split.addEventListener("mousedown", (e) => {
    e.preventDefault();
    const startX = e.clientX;
    const startW = paneEl.offsetWidth;
    const move = (ev) => {
      const w = Math.min(Math.max(startW - (ev.clientX - startX), 120), window.innerWidth - 300);
      paneEl.style.width = `${w}px`;
    };
    const up = () => {
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", up);
    };
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", up);
  });
})();

// ============ 启动 ============

(async () => {
  const version = await invoke("get_version");
  document.getElementById("status-version").textContent = version;
  model.load([], []); // 起始空表
  updateStatusText();
  // 初始标题保持 "simpleTable"（对齐 Qt：首次标脏/文件操作后才刷新标题）

  // 命令行/文件关联传入的 CSV：启动即打开
  const startupPath = await invoke("get_startup_path");
  if (startupPath) await openPath(startupPath);

  if (await invoke("is_smoke")) {
    // 未捕获异常兜底：任何前端错误都要写报告退出，不能让冒烟无声挂死
    window.addEventListener("error", async (e) => {
      await invoke("finish_smoke", {
        report: `[fatal] 未捕获异常: ${e.message} @ ${e.filename}:${e.lineno}`,
        code: 1,
      });
    });
    window.addEventListener("unhandledrejection", async (e) => {
      await invoke("finish_smoke", {
        report: `[fatal] 未处理的 Promise 拒绝: ${e.reason}`,
        code: 1,
      });
    });
    try {
      const { runSmoke } = await import("./smoke.js");
      runSmoke({ model, grid, pane, invoke, statusMsg });
    } catch (err) {
      await invoke("finish_smoke", { report: `[fatal] 冒烟套件异常: ${err.stack}`, code: 1 });
    }
  }
})();

// 供冒烟测试与调试使用
window.__app = {
  get model() { return model; }, grid, pane, invoke, statusMsg,
  newFile, openPath, saveFile, undo, redo,
  applyColOps, cancelColOps, runBigFilter,
  isDirty: () => dirty, getPath: () => path,
  isBigMode: () => model.isBig === true,
};
