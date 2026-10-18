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
  busyDialog, columnDialog, confirmDialog, confirmDiscardDialog, errorDialog,
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

/** 置脏标志并同步活动页签（页签脏点与窗口标题同源），随后刷新标题 */
function setDirty(v) {
  dirty = v;
  const t = activeTab();
  if (t) t.dirty = v;
  updateTitle();
}

/** 标脏并刷新标题与状态栏（对齐 _mark_dirty） */
function markDirty() {
  setDirty(true);
  updateStatusText();
}

// ============ 多文件页签 ============
// 小档页签持有完整模型（数据/撤销/脏态随签保留）；大档页签受 Rust 引擎
// 全局单例约束：激活期间引擎驻留，切到小档不腾退（编辑与撤销原样保留），
// 切到另一个大档才关闭重开（驻留档有脏时先弹 保存/放弃/取消）。
// 切换页签时视图重置（筛选/选区/滚动不随签保留，v1 边界）。

/**
 * @typedef {Object} DocTab
 * @property {number} id - 页签 id。
 * @property {string} title - 显示名（文件名/未命名）。
 * @property {string | null} path - 文件路径（null = 未命名）。
 * @property {"small" | "big"} kind - 文档类型。
 * @property {TableModel | BigModel | null} model - 小档恒为模型实例；大档仅引擎驻留期间有效。
 * @property {boolean} dirty - 未保存修改。
 */
/** @type {DocTab[]} */
let tabs = [];
/** @type {number | null} 活动页签 id；null = 当前档尚未入签（启动/单档态） */
let activeId = null;
/** @type {number | null} 大引擎驻留页签；null = 无驻留（未入签时引擎归当前档） */
let engineTabId = null;
let tabSeq = 1;

// ---- 分屏视图 ----
// 副侧（B）网格常驻实例，分屏时展示"分屏签"；焦点侧（focusedSide）独占键盘、
// 工具栏、编辑窗格与状态栏，非分屏时恒为 "A"。两侧绑定内容稳定：点另一侧
// 仅交换焦点（活动签跟随焦点侧），网格不重绑；激活任一侧之外的页签才把焦点
// 侧重绑到新签。大档受引擎单例约束：分屏拒绝双大档。
/** @type {boolean} 分屏开关 */
let splitOn = false;
/** @type {number | null} 副侧（分屏签）页签 id */
let splitTabId = null;
/** @type {"A" | "B"} 焦点侧：工具栏/快捷键/编辑窗格的作用侧 */
let focusedSide = "A";
/** 分屏各侧绑定的页签 id（非分屏恒 null；焦点侧绑定 = 活动签） */
const sideTab = {
  A: /** @type {number | null} */ (null),
  B: /** @type {number | null} */ (null),
};

const tabbarEl = document.getElementById("tabbar");
const tabScrollEl = document.getElementById("tab-scroll");

/** 文件名（路径分隔符后的末段；无路径 = 未命名） */
function tabTitleOf(p) {
  return p ? p.split(/[\\/]/).pop() : "未命名";
}

/** @returns {DocTab | null} 活动页签（未入签返回 null） */
function activeTab() {
  return tabs.find((t) => t.id === activeId) ?? null;
}

/** @returns {"A" | "B" | null} 页签绑定侧（未分屏返回 null） */
function sideOfTab(id) {
  if (!splitOn) return null;
  if (sideTab.A === id) return "A";
  if (sideTab.B === id) return "B";
  return null;
}

/** 当前档状态写回活动页签（切换/渲染前调用，live 变量 ↔ 页签镜像同步） */
function syncActiveTab() {
  const t = activeTab();
  if (!t) return;
  t.path = path;
  t.dirty = dirty;
  if (!model.isBig || engineTabId === t.id) t.model = model;
}

/** 当前档装入页签体系（保持内容/脏态不变；幂等） */
function ensureTabbed() {
  if (activeId !== null) return;
  const t = {
    id: tabSeq++, title: tabTitleOf(path), path, kind: model.isBig ? "big" : "small",
    model: model.isBig ? model : model, dirty,
  };
  tabs.push(t);
  activeId = t.id;
  if (model.isBig) engineTabId = t.id;
}

/** 重建页签条 DOM 并刷新溢出态；激活页签自动滚入可视区 */
function renderTabbar() {
  tabbarEl.classList.toggle("show", tabs.length >= 2);
  tabScrollEl.textContent = "";
  for (const t of tabs) {
    const el = document.createElement("div");
    el.className = "tab" + (t.id === activeId ? " active" : "")
      + (splitOn && t.id === splitTabId ? " split-side" : "");
    el.title = t.path || t.title;
    el.dataset.id = String(t.id);
    const label = document.createElement("span");
    label.className = "tab-label";
    const d = t.id === activeId ? dirty : t.dirty;
    label.textContent = d ? `${t.title} ●` : t.title;
    el.appendChild(label);
    const x = document.createElement("button");
    x.className = "tab-close";
    x.textContent = "×";
    x.title = "关闭（鼠标中键同效）";
    x.addEventListener("click", (e) => {
      e.stopPropagation();
      void closeTab(t.id);
    });
    el.appendChild(x);
    el.addEventListener("click", () => void activateTab(t.id));
    el.addEventListener("auxclick", (e) => {
      if (e.button === 1) {
        e.preventDefault();
        void closeTab(t.id);
      }
    });
    el.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      showTabMenu(t, e.clientX, e.clientY);
    });
    tabScrollEl.appendChild(el);
  }
  updateTabOverflow();
  tabScrollEl.querySelector(".tab.active")
    ?.scrollIntoView({ block: "nearest", inline: "nearest" });
}

/** 溢出态：页签总宽超可视宽 → 显示 ◀▶ 滚动按钮并按位置禁用 */
function updateTabOverflow() {
  const ov = tabScrollEl.scrollWidth > tabScrollEl.clientWidth + 1;
  tabbarEl.classList.toggle("overflow", ov);
  document.getElementById("tab-prev").disabled = tabScrollEl.scrollLeft <= 0;
  document.getElementById("tab-next").disabled =
    tabScrollEl.scrollLeft + tabScrollEl.clientWidth >= tabScrollEl.scrollWidth - 1;
}

document.getElementById("tab-prev").addEventListener("click", () => tabScrollEl.scrollBy({ left: -180 }));
document.getElementById("tab-next").addEventListener("click", () => tabScrollEl.scrollBy({ left: 180 }));
tabScrollEl.addEventListener("wheel", (e) => {
  if (tabScrollEl.scrollWidth <= tabScrollEl.clientWidth + 1) return;
  e.preventDefault();
  tabScrollEl.scrollLeft += e.deltaY || e.deltaX;
}, { passive: false });
tabScrollEl.addEventListener("scroll", updateTabOverflow);
new ResizeObserver(updateTabOverflow).observe(tabScrollEl);

/**
 * 引擎腾退：目标大档要用引擎时，关闭当前驻留（脏的弹 保存/放弃/取消）。
 *
 * @param {number | null} targetTabId - 目标页签 id（null = 当前未入签档）。
 * @returns {Promise<boolean>} true = 引擎可用；false = 用户取消。
 */
async function evictEngineFor(targetTabId) {
  if (engineTabId === targetTabId) return true; // 同档：复用驻留
  if (engineTabId === null) {
    // 引擎归当前未入签档（启动单档态）：用当前 live 状态询问
    if (model.isBig) {
      if (dirty && !(await confirmDiscard())) return false;
      await invoke("big_close").catch(() => {});
    }
    return true;
  }
  const holder = tabs.find((t) => t.id === engineTabId);
  if (holder?.dirty) {
    const choice = await confirmDiscardDialog();
    if (choice === "cancel") return false;
    if (choice === "save") {
      // 驻留档的 BigModel 实例在引擎存活期间始终有效：无需切签即可保存
      const dlg = progressDialog(`保存 ${holder.title}…`);
      const un = await holder.model.onProgress((d, tot, ph, rows) => dlg.set(d, tot, ph, rows));
      try {
        await holder.model.save();
        holder.dirty = false;
      } catch (err) {
        un();
        dlg.close();
        await errorDialog("保存失败", `${err?.message ?? err}`);
        return false;
      }
      un();
      await dlg.finish();
    }
  }
  await invoke("big_close").catch(() => {});
  if (holder) holder.model = null;
  engineTabId = null;
  // 被动侧大档模型随引擎关闭而失效：收起分屏，避免死模型占侧
  if (holder && splitOn && sideOfTab(holder.id) !== null
      && sideOfTab(holder.id) !== focusedSide) {
    closeSplit();
  }
  return true;
}

/**
 * 为大档打开引擎会话（含腾退仲裁与事件绑定）。
 *
 * @param {number | null} tabId - 归属页签（null = 当前未入签档）。
 * @param {string} p - 文件路径。
 * @returns {Promise<BigModel | null>} 失败/取消返回 null（已弹窗）。
 */
async function openBigEngineFor(tabId, p) {
  if (!(await evictEngineFor(tabId))) return null;
  const bm = new BigModel();
  bindModelEvents(bm); // 先绑事件再 open：内部 reset/data 通知才能到达外壳
  try {
    await bm.open(p);
  } catch (err) {
    await errorDialog("打开失败", String(err?.message ?? err));
    return null;
  }
  engineTabId = tabId;
  return bm;
}

/**
 * 装载文档（读文件→建模型；大档先仲裁引擎）。不碰当前 live 状态。
 *
 * @param {string} p - 文件路径。
 * @param {boolean} wantBig - 是否走大文件模式。
 * @param {number | null} tabId - 归属页签（大档仲裁/归属用）。
 * @returns {Promise<{kind: "small" | "big", model: any, warnings?: string[]} | null>}
 */
async function loadDoc(p, wantBig, tabId) {
  if (wantBig) {
    const bm = await openBigEngineFor(tabId, p);
    if (!bm) return null;
    return { kind: "big", model: bm };
  }
  let result;
  try {
    result = await invoke("read_csv_file", { path: p });
  } catch (err) {
    await errorDialog("打开失败", String(err));
    return null;
  }
  const { headers, rows, warnings } = parseCsv(result.text);
  const m = new TableModel();
  bindModelEvents(m);
  m.load(headers, rows);
  return { kind: "small", model: m, warnings };
}

/**
 * 换档/装载后的视图重置：清筛选选区、滚动回顶、窗格清空、计数与按钮刷新。
 *
 * @param {Grid} g - 目标网格（焦点侧；分屏焦点交换不调用本函数）。
 */
function applyViewState(g) {
  g.clearFilters();
  g.clearSelection();
  g.setModel(model);
  g.scrollEl.scrollTop = 0;
  g.scrollEl.scrollLeft = 0;
  currentCell = null;
  pane.setText("");
  lastFilterMs = 0;
  // 旧模型还在跑的筛选禁止改写新档状态栏/滚动位置
  clearTimeout(bigFilterTimer);
  bigFilterSeq += 1;
  onCountsChanged(model.rowCount(), model.rowCount());
  refreshBigUi();
}

/**
 * 把装载好的文档应用到当前档（启动/未入签态/重载共用）。
 *
 * @param {{kind: string, model: any, warnings?: string[]}} doc - 装载结果。
 * @param {string} p - 文件路径。
 */
function applyDocToCurrent(doc, p) {
  model = doc.model;
  path = p;
  applyViewState(fgrid());
  setDirty(false);
  const t = activeTab();
  if (t) {
    t.path = p;
    t.kind = doc.kind;
    t.title = tabTitleOf(p);
    t.model = doc.model;
  }
  renderTabbar();
  updateStatusText();
  if (doc.kind === "big") {
    const meta = model.meta();
    statusMsg(
      `大文件模式：${meta.rows} 行 × ${meta.cols} 列（${meta.encoding}），` +
      "筛选框键入即筛（回车立即执行）", 6000);
  }
  if (doc.warnings?.length) {
    void infoDialog("打开提示", doc.warnings.slice(0, 20).join("\n"));
  }
}

/**
 * 切换活动页签：状态写回旧签 → 引擎仲裁 → 换绑视图。
 * 分屏下点另一侧的签 = 纯焦点交换（内容稳定）；点侧外签 = 焦点侧重绑新签。
 *
 * @param {number} id - 目标页签 id。
 * @returns {Promise<boolean>} true = 已切换；false = 用户取消或打开失败。
 */
async function activateTab(id) {
  if (id === activeId) return true;
  const t = tabs.find((x) => x.id === id);
  if (!t) return false;
  syncActiveTab();
  const side = sideOfTab(id);
  if (side && side !== focusedSide) {
    focusSide(side); // 目标签在另一侧：交换焦点即可，两侧绑定不动
    return true;
  }
  if (t.kind === "big") {
    if (!(engineTabId === t.id && t.model)) {
      // 引擎已腾退（或从未打开）：重新打开
      const bm = await openBigEngineFor(t.id, t.path);
      if (!bm) return false;
      t.model = bm;
    }
    model = t.model;
  } else {
    model = t.model;
  }
  activeId = id;
  path = t.path;
  if (splitOn) sideTab[focusedSide] = id; // 分屏激活侧外签：焦点侧重绑新签
  applyViewState(fgrid());
  setDirty(t.dirty);
  renderTabbar();
  return true;
}

/**
 * 新开页签装载文档（批量拖入可不激活，避免连开闪烁）。
 *
 * @param {string} p - 文件路径。
 * @param {boolean} wantBig - 是否走大文件模式。
 * @param {object} [opts] - 选项。
 * @param {boolean} [opts.activate] - 装载后激活（默认 true）。
 * @returns {Promise<DocTab | null>} 新页签或 null（失败已弹窗）。
 */
async function loadIntoNewTab(p, wantBig, opts = {}) {
  const t = {
    id: tabSeq++, title: tabTitleOf(p), path: p,
    kind: wantBig ? "big" : "small", model: null, dirty: false,
  };
  const doc = await loadDoc(p, wantBig, t.id);
  if (!doc) return null;
  t.kind = doc.kind;
  t.model = doc.model;
  tabs.push(t);
  if (opts.activate !== false) {
    await activateTab(t.id);
    if (doc.warnings?.length) {
      void infoDialog("打开提示", doc.warnings.slice(0, 20).join("\n"));
    }
  } else {
    renderTabbar();
  }
  return t;
}

/**
 * 拖入多文件：逐个入签（重复路径激活已有），最终激活列表首个。
 *
 * @param {string[]} paths - CSV 路径列表。
 * @returns {Promise<boolean>} 至少一个文件成功。
 */
async function openPathsAsTabs(paths) {
  // 未入签的脏档（打开带警告 CSV 后弹过提示框、或其他未命名修改）：
  // 直接装进当前档会留下 unnamed 悬空签；先问 保存/放弃/取消。
  if (activeId === null && dirty) {
    if (!(await confirmDiscard())) return false;
  }
  // 收尾激活目标：列表首个文件的归属签。
  let firstId = null;
  if (activeId === null) {
    // 无签状态（启动空档/全关后）：首个文件直接装进当前档，省一个过渡签。
    // 装入后须 ensureTabbed 实体化，否则按路径找不到归属签。
    const first = paths.shift();
    if (first === undefined) return true;
    if (!(await openPath(first))) return false;
    ensureTabbed();
    firstId = activeTab()?.id ?? null;
  } else {
    ensureTabbed();
  }
  for (const p of paths) {
    const exist = tabs.find((t) => t.path === p);
    if (exist) {
      firstId ??= exist.id;
      continue;
    }
    let size;
    try {
      size = await invoke("file_size", { path: p });
    } catch (err) {
      await errorDialog("打开失败", String(err));
      continue;
    }
    const forceBig = await invoke("smoke_force_big");
    const t = await loadIntoNewTab(p, forceBig || size >= BIG_THRESHOLD,
      { activate: false });
    if (t) firstId ??= t.id;
  }
  if (firstId !== null && firstId !== activeId) await activateTab(firstId);
  return firstId !== null;
}

/**
 * 关闭页签：脏档弹 保存/放弃/取消（非活动脏签先切过去，所见即所关）；
 * 活动签关闭后激活相邻签；最后一签关闭后回空表。
 *
 * @param {number} id - 页签 id。
 * @returns {Promise<boolean>} true = 已关闭；false = 用户取消。
 */
async function closeTab(id) {
  if (!tabs.some((t) => t.id === id)) return true;
  if (id !== activeId && !(await activateTab(id))) return false;
  if (!(await confirmDiscard())) return false; // 脏 → 保存/放弃/取消
  const t = activeTab();
  const idx = tabs.indexOf(t);
  if (engineTabId === t.id) {
    await invoke("big_close").catch(() => {});
    engineTabId = null;
  }
  tabs.splice(idx, 1);
  if (splitOn && splitTabId !== null && !tabs.some((x) => x.id === splitTabId)) {
    closeSplit(); // 分屏签被关：分屏随之收起（其余页签保留）
  }
  if (!tabs.length) {
    resetToEmpty();
    return true;
  }
  activeId = null; // 强制 activateTab 走完整换绑
  const ok = await activateTab(tabs[Math.min(idx, tabs.length - 1)].id);
  renderTabbar();
  return ok;
}

/** 全部页签关闭后回空表（对齐启动态） */
function resetToEmpty() {
  splitOn = false;
  splitTabId = null;
  sideTab.A = null;
  sideTab.B = null;
  focusedSide = "A";
  document.body.classList.remove("split");
  document.getElementById("grid-area").style.flex = ""; // 清掉分屏拖拽残留的主侧定宽
  model = new TableModel();
  bindModelEvents(model);
  path = null;
  activeId = null;
  applyViewState(fgrid());
  model.load([], []);
  setDirty(false);
  renderTabbar();
  updateStatusText();
}

// ============ 网格 ============
// 主网格（A）常驻，恒展示焦点签；副网格（B）仅分屏时展示分屏签。
// 工具栏/快捷键/右键菜单统一经 fgrid() 落到焦点侧。

/**
 * 构造一侧网格的钩子集。焦点侧守卫：编辑窗格/行计数/状态栏/工具按钮等
 * 共享外壳状态只随焦点侧更新，避免副侧滚动取数等非焦点事件改写焦点侧状态
 * （典型：副侧大档预取落定不得刷编辑窗格、不得切换大模式按钮显隐）。
 *
 * @param {"A" | "B"} side - 侧标识。
 * @returns {object} Grid hooks。
 */
function makeHooks(side) {
  const focused = () => focusedSide === side;
  return {
    statusMsg,
    countsChanged: (v, t) => { if (focused()) onCountsChanged(v, t); },
    zoomed: (f) => { if (focused()) statusMsg(`表格缩放 ${Math.round(f * 100)}%`, 1500); },
    undoRequested: () => { if (focused()) undo(); },
    redoRequested: () => { if (focused()) redo(); },
    currentChanged: (r, c) => {
      if (!focused()) return;
      currentCell = [r, c];
      pane.setText(model.rawData(r, c));
    },
    cellMenu: (r, c, e) => { if (focused()) showCellMenu(e); },
    rowMenu: (r, e) => { if (focused()) showRowMenu(e); },
    colMenu: (c, e) => { if (focused()) showColMenu(e); },
    // 大模式：筛选框内回车 = 跳过防抖立即执行一次
    bigFilterRun: () => { if (focused()) runBigFilter(); },
    // 大模式：筛选条件键入即筛（防抖 + 引擎侧可中断扫描）
    bigFilterInput: () => { if (focused()) scheduleBigFilter(); },
    // 大模式：筛选条件变化（刷新列操作按钮态与状态栏）
    bigFilterStatus: () => { if (focused()) refreshBigUi(); },
  };
}

const gridA = new Grid({
  model,
  scrollEl: document.getElementById("grid-scroll"),
  canvasEl: document.getElementById("grid-canvas"),
  headerEl: document.getElementById("col-header"),
  rowsEl: document.getElementById("rows"),
  editorEl: document.getElementById("cell-editor"),
  areaEl: document.getElementById("grid-area"),
  hooks: makeHooks("A"),
});
// 副网格挂独立空模型：不镜像活动档（否则隐藏副网格也在渲染，双份 DOM 会
// 污染全局类选择器的断言/交互）；分屏建立时经 setModel 换绑分屏签。
const gridB = new Grid({
  model: new TableModel(),
  scrollEl: document.getElementById("grid-scroll2"),
  canvasEl: document.getElementById("grid-canvas2"),
  headerEl: document.getElementById("col-header2"),
  rowsEl: document.getElementById("rows2"),
  editorEl: document.getElementById("cell-editor2"),
  areaEl: document.getElementById("grid-area2"),
  hooks: makeHooks("B"),
});

/**
 * 侧 → 网格实例。
 *
 * @param {"A" | "B"} side - 侧标识。
 * @returns {Grid} 该侧网格。
 */
function gridOfSide(side) {
  return side === "A" ? gridA : gridB;
}

/**
 * 焦点侧网格：工具栏/快捷键/右键菜单/视图重置的作用目标。
 *
 * @returns {Grid} 焦点侧网格（非分屏 = gridA）。
 */
function fgrid() {
  return gridOfSide(focusedSide);
}

// 分屏焦点激活：按下任一侧网格（含右键）焦点即交给该侧。捕获阶段先行，
// 网格自身 mousedown 处理选中时，currentChanged 等钩子已见到正确焦点。
for (const [side, g] of [["A", gridA], ["B", gridB]]) {
  g.scrollEl.addEventListener("mousedown", () => {
    if (splitOn && focusedSide !== side) focusSide(side);
  }, true);
}

// ============ 分屏控制 ============

/**
 * 焦点交换：焦点/活动签/live 状态切到指定侧，两侧网格绑定不动（内容稳定）。
 * 全程同步执行：mousedown 激活路径要求后续 contextmenu/键盘事件见到正确焦点。
 *
 * @param {"A" | "B"} side - 目标焦点侧（该侧须已绑定页签）。
 * @returns {void}
 */
function focusSide(side) {
  if (side === focusedSide) return;
  syncActiveTab(); // live 状态写回旧焦点签
  const t = tabs.find((x) => x.id === sideTab[side]);
  if (!t) return;
  focusedSide = side;
  activeId = t.id;
  model = t.model;
  path = t.path;
  currentCell = null;
  pane.setText("");
  lastFilterMs = 0;
  clearTimeout(bigFilterTimer);
  bigFilterSeq += 1; // 旧焦点侧的筛选结果不得改写新焦点侧状态栏
  setDirty(t.dirty); // 同步新活动签脏点 + 窗口标题
  onCountsChanged(model.rowCount(), model.rowCount());
  // 多重筛选闩锁/输入随焦点侧还原（两侧各自记忆）
  const g = fgrid();
  tb.multiFilter.classList.toggle("on", g.multi.on);
  multiWrap.classList.toggle("hidden", !g.multi.on);
  multiInput.value = g.multi.text;
  refreshBigUi();
  renderTabbar();
}

/**
 * 建立分屏：目标签上副侧（B 网格），焦点留在主侧。
 * 目标签为活动签时同样允许（VS Code 行为）：主侧先换显右邻签，原活动签入副侧。
 * 双大档（引擎单例约束）拒绝：提示并返回 false。
 *
 * @param {number} id - 目标页签 id。
 * @returns {Promise<boolean>} true = 分屏已建立。
 */
async function openSplit(id) {
  if (splitOn) return false;
  const t = tabs.find((x) => x.id === id);
  if (!t) return false;
  if (id === activeId) {
    // 活动签入分屏：主侧改显右邻签（末尾回卷首个），分屏签 = 原活动签
    if (tabs.length < 2) return false;
    const idx = tabs.indexOf(t);
    const next = tabs[(idx + 1) % tabs.length];
    if (t.kind === "big" && next.kind === "big") {
      statusMsg("分屏不支持同时打开两个大文件", 3000);
      return false;
    }
    if (!(await activateTab(next.id))) return false; // 主侧重绑邻签（引擎随档仲裁）
  } else if (t.kind === "big" && model.isBig) {
    statusMsg("分屏不支持同时打开两个大文件", 3000);
    return false;
  }
  syncActiveTab();
  if (t.kind === "big" && !(engineTabId === t.id && t.model)) {
    const bm = await openBigEngineFor(t.id, t.path);
    if (!bm) return false;
    t.model = bm;
  }
  splitOn = true;
  splitTabId = id;
  sideTab.A = activeId;
  sideTab.B = id;
  document.body.classList.add("split");
  // 副侧视图重置（对齐 applyViewState 顺序）：上次分屏的筛选/选区不残留
  gridB.clearFilters();
  gridB.clearSelection();
  gridB.setModel(t.model);
  gridB.scrollEl.scrollTop = 0;
  gridB.scrollEl.scrollLeft = 0;
  renderTabbar();
  return true;
}

/**
 * 关闭分屏：副网格隐藏、页签保留（含分屏签），焦点归主侧。
 * 焦点在副侧时主网格接管活动签（视图重置，对齐换档语义）。
 *
 * @returns {void}
 */
function closeSplit() {
  if (!splitOn) return;
  splitOn = false;
  splitTabId = null;
  sideTab.A = null;
  sideTab.B = null;
  document.body.classList.remove("split");
  document.getElementById("grid-area").style.flex = ""; // 清掉分屏拖拽残留的主侧定宽
  if (focusedSide !== "A") {
    focusedSide = "A";
    if (activeTab()) applyViewState(gridA);
  }
  renderTabbar();
}

/**
 * 页签右键菜单：未分屏给「在分屏中打开」（只有一个签时禁用；双大档由动作层
 * 点击后拒绝并提示原因，避免灰着不给说法）；分屏态点分屏签给「关闭分屏」；
 * 其余场景不出菜单。
 *
 * @param {DocTab} t - 目标页签。
 * @param {number} x - 菜单位置 x。
 * @param {number} y - 菜单位置 y。
 * @returns {void}
 */
function showTabMenu(t, x, y) {
  if (!splitOn) {
    showMenu(x, y, [{
      label: "在分屏中打开",
      disabled: tabs.length < 2, // 单签无可对照档
      onClick: () => openSplit(t.id),
    }]);
    return;
  }
  if (t.id === splitTabId) {
    showMenu(x, y, [{ label: "关闭分屏", onClick: () => closeSplit() }]);
  }
}

// ============ 大文件模式 UI（按钮显隐/筛选执行/列操作 pending） ============

// 注：大模式工具按钮在下方工具栏段（tb 初始化后）创建；此处仅函数定义。

/** 按大模式状态刷新专属按钮显隐与可用态 */
function refreshBigUi() {
  const big = !!model.isBig;
  // 模式专属按钮显隐：大模式隐藏「排序…」（F5 快捷键仍有 isBig 守卫兜底）；
  // 列操作两按钮仅 pending 时出现（小模式即时应用，不依赖它们）。
  // 「执行筛选」按钮已移除：筛选框键入即筛，回车立即执行。
  tb.sort.classList.toggle("hidden", big);
  if (big) {
    const pending = model.colOpsPending();
    tb.execColOps.classList.toggle("hidden", !pending);
    tb.cancelColOps.classList.toggle("hidden", !pending);
  } else {
    tb.execColOps.classList.add("hidden");
    tb.cancelColOps.classList.add("hidden");
  }
}

/** @type {number} 大模式键入即筛防抖计时器 */
let bigFilterTimer = 0;
/** @type {number} 筛选请求序号：只有最新请求允许改写状态栏 */
let bigFilterSeq = 0;

/**
 * 收集大模式当前筛选条件（逐列 AND + 多重 OR）。
 *
 * @returns {{kws: Array<[number, string]>, mKw: string, mCols: number[]}}
 *   列号/关键字列表、多重关键字与勾选列。
 */
function collectBigFilter() {
  const cc = model.columnCount();
  const kws = [];
  for (const [col, kw] of fgrid().filters) {
    if (col < cc && kw) kws.push([col, kw]);
  }
  const mKw = fgrid().multi.on ? fgrid().multi.text : "";
  const mCols = mKw ? [...fgrid().multi.cols].filter((c) => c < cc) : [];
  return { kws, mKw, mCols };
}

/**
 * 键入即筛：短防抖后触发一次筛选。整表扫描在 Rust 侧可被下一请求中断，
 * 故不必等上一次扫完；条件清空由 grid._refilter 自动回全量。
 *
 * @returns {void}
 */
function scheduleBigFilter() {
  if (!model.isBig) return;
  clearTimeout(bigFilterTimer);
  bigFilterTimer = setTimeout(() => {
    const { kws, mKw } = collectBigFilter();
    if (!kws.length && !mKw) {
      updateStatusText(); // 条件已清空：收敛状态栏（清空动作由 grid 完成）
      return;
    }
    void runBigFilter({ auto: true });
  }, 180);
}

/**
 * 执行大文件筛选：收集条件 → 引擎扫描 → 状态栏报命中与用时。
 * 引擎按需建列缓存；重复筛选同一列直接读缓存（几十毫秒级）。
 *
 * @param {{auto?: boolean}} [opts] - auto=true 为键入即筛（无模态、状态更轻）。
 * @returns {Promise<void>}
 */
async function runBigFilter(opts = {}) {
  if (!model.isBig) return; // 兜底：小模式此按钮隐藏，防御误触调用小模型不存在的 runFilter
  clearTimeout(bigFilterTimer);
  const { kws, mKw, mCols } = collectBigFilter();
  if (!kws.length && !mKw) {
    // 无条件下执行筛选（如回车）= 回到全量（通常已由 grid 自动清空，此处兜底）
    try {
      await model.clearFilter();
    } catch { /* 会话已关闭等：保持现状 */ }
    return;
  }
  const seq = ++bigFilterSeq;
  // 扫描在 spawn_blocking 中跑、UI 线程不冻结；只报状态，不弹模态
  statusMsg(opts.auto ? "筛选中…" : "正在筛选大文件…", 0);
  try {
    const t0 = performance.now();
    const n = await model.runFilter(kws, mCols, mKw);
    // 被更新请求替代（引擎中断返回 null）或已过期：状态栏归新请求管理
    if (n === null || seq !== bigFilterSeq) return;
    // 筛选切换了滚动坐标系（全量压缩画布 → 命中自然画布）：旧 scrollTop 在新
    // 坐标系无意义——渲染锚点由 scrollTop 反推，旧值会把锚点夹到末行、transform
    // 把 #rows 推到画布外且回灌 scrollHeight 自锁（视口只剩末几行、滚动条跳回）。
    // 回顶后一次重绘落定，同任务内完成不出现中间帧。
    fgrid().resetScroll();
    lastFilterMs = performance.now() - t0;
    statusMsg(`匹配 ${n} 行 · 用时 ${(lastFilterMs / 1000).toFixed(1)} 秒`, 4000);
    updateStatusText();
  } catch (err) {
    if (seq === bigFilterSeq) await errorDialog("筛选失败", String(err?.message ?? err));
  }
}

/** 执行 pending 列操作（物理重写 + 进度条） */
async function applyColOps() {
  if (!model.isBig || !model.colOpsPending()) return;
  const dlg = progressDialog("执行列操作…");
  const un = await model.onProgress((done, total, phase, rows) => dlg.set(done, total, phase, rows));
  let ok = false;
  try {
    await model.applyColOps();
    // 物理重写已把内存状态（含覆盖层编辑/插删行）落盘：文件与内存一致。
    // applyColOps 的 _emitData 经 onData 标了脏，此处复位——否则保存会再做
    // 一次全量流式重写（用户看到的"莫名其妙第二根进度条"）。
    setDirty(false);
    statusMsg("列操作已完成（物理重写，撤销历史已清空）", 4000);
    ok = true;
  } catch (err) {
    await errorDialog("列操作失败", String(err?.message ?? err));
  } finally {
    un();
    // 完成帧短暂停留再关闭：重写期间磁盘 IO 可能饿死渲染管线，进度停在
    // 中间的旧帧上——保证"100% 完成"被看到，而不是"卡住后消失"。
    if (ok) await dlg.finish();
    else dlg.close();
    refreshBigUi();
  }
}

/** 撤销（放弃）全部 pending 列操作 */
async function cancelColOps() {
  if (!model.isBig) return; // 兜底：小模式此按钮隐藏，防御误触调用小模型不存在的 clearColOps
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
  ["goto", "跳转行…", () => gotoRowDialog()],
  ["sort", "排序…", () => openSortDialog()],
  ["clearFilter", "清除筛选", () => {
    fgrid().clearFilters();
    multiInput.value = "";
  }],
  ["multiFilter", "多重筛选", (e) => {
    const on = e.currentTarget.classList.toggle("on");
    fgrid().setMultiOn(on);
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

// 大模式工具按钮：执行/撤销列操作（pending 时出现）
for (const [key, text, fn, title] of [
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
multiInput.addEventListener("input", () => fgrid().setMultiText(multiInput.value));
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
    fgrid().clearSelection();
    clearDock();
    markDirty();
  }
}

/** 重做一步表格改动（Ctrl+Y / Ctrl+Shift+Z） */
async function redo() {
  if (await model.redo()) {
    fgrid().clearSelection();
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
 * 分屏下两侧模型都存活且有事件流：只有焦点侧（m === model）的事件允许
 * 触碰共享外壳状态（脏点/编辑窗格/状态栏），副侧事件仅驱动其自身网格重绘
 * （网格在 setModel 里自行订阅，不经此处）。
 *
 * @param {TableModel | BigModel} m - 模型实例。
 * @returns {void}
 */
function bindModelEvents(m) {
  m.onReset(() => {
    if (m !== model) return; // 非焦点侧（装载中的新档等）：外壳状态不动
    resetPending = true; // 紧随其后的 onData 是装载/恢复本身，不视为脏
    setDirty(false); // 撤销/重做会由各自 wrapper 在返回前重新标脏
    clearDock();
    updateStatusText();
    fgrid().resetScroll(); // WebView2 会话记忆滚动位置，装载须回顶（对齐 Qt）
  });
  m.onData((structural, origin) => {
    if (m !== model) return; // 副侧数据/预取落定：不得改写焦点侧外壳状态
    if (resetPending) resetPending = false;
    // 大模式预取页落定是纯重绘（origin="fetch"），打开/滚动不得误标脏
    else if (!(m.isBig && origin === "fetch")) markDirty();
    if (m.isBig) refreshBigUi();
    if (currentCell === null) return;
    const [r, c] = currentCell;
    if (structural) {
      // 行列增删/排序：越界清空，否则重载内容
      if (r >= m.rowCount() || c >= m.columnCount()) clearDock();
      else {
        // 大模式未命中（页被换出）保留窗格旧内容，数据到齐再收敛
        const t = m.isBig && m.peekData ? (m.peekData(r, c) ?? null) : m.rawData(r, c);
        if (t !== null) pane.setText(t);
      }
      return;
    }
    // 编辑窗格自己发起的写回不再回灌（避免光标跳动）
    if (dockOrigin) return;
    // 大模式：当前格页被换出时 rawData 会读成空串、还顺手重拉该页——与缓存
    // trim 形成乒乓驱逐循环，且把窗格刷空造成频闪。peekData 未命中时保留旧内容。
    const newText = m.isBig && m.peekData ? (m.peekData(r, c) ?? null) : m.rawData(r, c);
    if (newText !== null && pane.editEl.value !== newText) pane.setText(newText);
  });
  m.onUndoStack(() => {
    if (m !== model) return;
    updateUndoButtons();
  });
  // 大表撤销降级：状态栏提示一次（小模型专属；BigModel 无此方法）
  m.onUndoCap?.((cap) => {
    if (m !== model) return;
    statusMsg(`表格较大：撤销上限已降为 ${cap} 步`, 6000);
  });
}
bindModelEvents(model);

// ============ 文件操作（F2/F3/F4） ============

/** 新建表格：弹出列定义对话框（F1）；作为新页签打开（旧档保留） */
async function newFile() {
  const names = await newTableDialog();
  if (names === null) return;
  if (!names.length) {
    await infoDialog("新建表格", "至少需要一列。");
    return;
  }
  const m = new TableModel();
  m.load(names, Array.from({ length: NEW_TABLE_ROWS }, () => names.map(() => "")));
  const pristine = tabs.length === 0 && activeId === null && path === null && !dirty
    && model.rowCount() === 0 && model.columnCount() === 0;
  if (pristine) {
    // 启动空表原位替换（尚未入签体系）
    model = m;
    bindModelEvents(m);
    applyViewState(fgrid());
    setDirty(false);
    renderTabbar();
    updateStatusText();
    return;
  }
  ensureTabbed();
  const t = {
    id: tabSeq++, title: "未命名", path: null, kind: "small", model: m, dirty: false,
  };
  tabs.push(t);
  await activateTab(t.id);
}

/** 打开 CSV：文件对话框选择（作为新页签；已有路径则激活） */
async function openFile() {
  const p = await invoke("pick_open_path");
  if (p) await openPath(p);
}

/**
 * 打开指定路径的 CSV（菜单/拖放/启动/重载共用）。
 * 按文件大小路由：≥ 阈值（或冒烟强制）走大文件模式（mmap 秒开），否则原路径。
 * 入签规则：路径已有签 → 激活；未入签体系 → 装当前档；否则新开页签。
 *
 * @param {string} p - CSV 文件路径。编码自动识别（Rust 层）。
 * @param {object} [opts] - 选项。
 * @param {boolean} [opts.replace] - 重载当前签内容（外部改动重载），先确认丢弃。
 * @param {boolean} [opts.noConfirm] - replace 时跳过丢弃确认（调用方已问过）。
 * @returns {Promise<boolean>} true = 成功打开/切换。
 */
async function openPath(p, opts = {}) {
  let size;
  try {
    size = await invoke("file_size", { path: p });
  } catch (err) {
    await errorDialog("打开失败", String(err));
    return false;
  }
  const forceBig = await invoke("smoke_force_big");
  const wantBig = forceBig || size >= BIG_THRESHOLD;

  if (opts.replace) {
    // 重载当前签：引擎归属不变（big_open 原位替换内容），丢弃前确认
    if (!opts.noConfirm && !(await confirmDiscard())) return false;
    const doc = await loadDoc(p, wantBig, activeId);
    if (!doc) return false;
    applyDocToCurrent(doc, p);
    return true;
  }
  const exist = tabs.find((t) => t.path === p);
  if (exist) return activateTab(exist.id);
  if (tabs.length === 0) {
    // 启动/初始空表：装当前档（暂不生成页签）
    const doc = await loadDoc(p, wantBig, activeId);
    if (!doc) return false;
    applyDocToCurrent(doc, p);
    return true;
  }
  ensureTabbed();
  return (await loadIntoNewTab(p, wantBig)) !== null;
}

/** 保存到当前路径（无路径时转另存为） */
async function saveFile() {
  if (path === null) {
    await saveFileAs();
    return;
  }
  if (model.isBig) {
    const dlg = progressDialog("保存大文件…");
    const un = await model.onProgress((done, total, phase, rows) => dlg.set(done, total, phase, rows));
    try {
      await model.save();
    } catch (err) {
      un();
      dlg.close();
      await errorDialog("保存失败", `${err?.message ?? err}\n文件可能被其他程序占用，可尝试另存为。`);
      return;
    }
    un();
    // 完成帧短暂停留再关闭：重写期间磁盘 IO 可能饿死渲染管线，屏幕停在
    // 中间的旧帧——保证"100% 完成"被看到，而不是"卡住后消失"。
    await dlg.finish();
    setDirty(false);
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
  setDirty(false);
}

/** 另存为 CSV */
async function saveFileAs() {
  let p = await invoke("pick_save_path", { defaultName: "未命名.csv" });
  if (!p) return;
  if (!p.toLowerCase().endsWith(".csv")) p += ".csv";
  path = p;
  await saveFile();
  // 另存后当前档的身份（路径/标题）随之变化，同步回页签
  syncActiveTab();
  renderTabbar();
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
      if (reload) await openPath(path, { replace: true, noConfirm: true });
    }
  } catch { /* 检测失败静默（会话可能已关闭） */ }
});

// ---- 拖放打开 ----
tauri.event.listen("tauri://drag-drop", async (e) => {
  const paths = e.payload?.paths ?? [];
  const csvs = paths.filter((x) => x.toLowerCase().endsWith(".csv"));
  if (csvs.length) await openPathsAsTabs(csvs);
});

// ---- 关闭前确认 ----
tauri.event.listen("close-request", async () => {
  syncActiveTab();
  for (const t of tabs) {
    if (!t.dirty) continue;
    activateTab(t.id);
    if (!(await confirmDiscard())) return; // 用户取消：留在该档，中止退出
  }
  invoke("quit_app");
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
  fgrid().clearSelection();
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
  const rows = fgrid().selectedRows();
  if (!rows.length) return;
  showMenu(e.clientX, e.clientY, [
    { label: "跳转到行…", onClick: gotoRowDialog },
    null,
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
  const cols = fgrid().selectedCols();
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
  const rows = fgrid().selectedRows();
  const cols = fgrid().selectedCols();
  const n = fgrid().selectedCells().length;
  showMenu(e.clientX, e.clientY, [
    { label: "按行拆分粘贴", onClick: () => fgrid().pasteClipboard(true) },
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

/**
 * 跳到显示行号（0 基内部序号）：选中整行并滚入视野，窗格随选中联动。
 * 筛选视图下按可见序号解释（行头看到的序号），与 ensureVisible 同一坐标系。
 *
 * @param {number} vi - 显示行号（0 基；自动夹取到 [0, 行数)）。
 * @returns {boolean} 是否成功跳转。
 */
function gotoRow(vi) {
  const n = fgrid().visCount();
  if (!n) return false;
  const v = Math.min(Math.max(0, Math.trunc(vi)), n - 1);
  const r = fgrid().visAt(v);
  if (r == null) return false;
  fgrid().selectRow(r);
  fgrid().ensureVisible(r, fgrid().sel.current?.[1] ?? 0);
  return true;
}

/** 跳转行对话框（工具栏"跳转行…" / Ctrl+G）：输入 1 基显示行号直达。 */
async function gotoRowDialog() {
  const n = fgrid().visCount();
  if (!n) {
    statusMsg("空表无法跳转", 2000);
    return;
  }
  const cur0 = fgrid().sel.current?.[0];
  const def = cur0 != null ? fgrid().visibleIndexOf(cur0) + 1 : 1;
  const target = await intDialog("跳转到行", `行号（1 – ${n.toLocaleString()}）：`, def, 1, n);
  if (target === null) return;
  gotoRow(target - 1);
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
  fgrid().clearFilters();
}

/**
 * 二次确认后删除指定列并清空筛选。
 *
 * @param {number[]} cols - 列号列表。
 */
async function deleteColumns(cols) {
  if (cols.length && await confirmDialog(`确定删除选中的 ${cols.length} 列及其全部数据？（列操作将预览，需点「执行列操作」生效）`)) {
    op(model.removeColumns(cols));
    fgrid().clearFilters();
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
    op(model.clearCells(fgrid().selectedCells()));
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
  if (mods && !e.shiftKey && (e.key === "g" || e.key === "G")) {
    e.preventDefault();
    gotoRowDialog();
    return;
  }
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

// ========== 分屏分隔条拖拽（#vsplit：主/副网格之间） ==========

(() => {
  const vsplit = document.getElementById("vsplit");
  const gridArea = document.getElementById("grid-area");
  vsplit.addEventListener("mousedown", (e) => {
    e.preventDefault();
    const startX = e.clientX;
    const startW = gridArea.offsetWidth;
    const move = (ev) => {
      // 主侧改定宽、副侧弹性吸收余量；两侧各留最小宽度（含两条 5px 分隔条）
      const maxW = window.innerWidth - document.getElementById("pane").offsetWidth - 240 - 10;
      const w = Math.min(Math.max(startW + (ev.clientX - startX), 240), maxW);
      gridArea.style.flex = `0 0 ${w}px`;
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

// 冒烟/截屏诊断经此代理访问网格：函数调用绑定到当前焦点侧实例，
// 属性读写直通（对齐上方 model 实时代理的同一套路）。
const gridLive = new Proxy(/** @type {any} */ ({}), {
  get: (_t, k) => {
    const g = fgrid();
    const v = g[k];
    return typeof v === "function" ? v.bind(g) : v;
  },
  set: (_t, k, val) => { fgrid()[k] = val; return true; },
});

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
      // 网格走实时代理：分屏焦点交换后，冒烟里的 grid.* 仍作用于焦点侧
      const { runSmoke } = await import("./smoke.js");
      runSmoke({ model, grid: gridLive, pane, invoke, statusMsg });
    } catch (err) {
      await invoke("finish_smoke", { report: `[fatal] 冒烟套件异常: ${err.stack}`, code: 1 });
    }
  }
  const parkMode = await invoke("is_park");
  if (parkMode) {
    // 截屏诊断：停在指定渲染场景待命（进程不退出），供 park_capture.ps1 读像素。
    // catch 里错误标记尽力而为（嵌套 try 包住 invoke，失败不再级联）
    const { runPark } = await import("./park.js");
    runPark({ model, grid: gridLive, pane, invoke, statusMsg, mode: parkMode }).catch(async (err) => {
      try { console.error("park error", err); } catch (_) { /* ignore */ }
      try {
        document.title = "PARK-ERR " + (err && err.message || err);
        const dir = await invoke("smoke_tmp_dir");
        await invoke("smoke_write_bytes", {
          path: `${dir}/park_err.txt`,
          bytes: [...new TextEncoder().encode(String((err && err.stack) || err))],
        });
      } catch (_) { /* 尽力而为 */ }
    });
  }
})();

// 供冒烟测试与调试使用
window.__app = {
  get model() { return model; },
  get grid() { return fgrid(); },
  pane, invoke, statusMsg,
  newFile, openPath, saveFile, undo, redo,
  applyColOps, cancelColOps, runBigFilter, gotoRow, gotoRowDialog,
  isDirty: () => dirty, getPath: () => path,
  isBigMode: () => model.isBig === true,
  // 多档页签：快照 / 切换 / 关闭 / 批量拖入
  tabs: () => tabs.map((t) => ({
    id: t.id, title: t.title, path: t.path, kind: t.kind,
    dirty: t.id === activeId ? dirty : t.dirty,
    active: t.id === activeId,
  })),
  activateTab, closeTab, openPathsAsTabs,
  // 分屏视图：状态快照 / 建立 / 关闭
  split: () => ({ on: splitOn, splitTabId, focused: focusedSide }),
  openSplit, closeSplit,
};
