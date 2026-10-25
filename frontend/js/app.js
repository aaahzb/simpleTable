/**
 * simpleTable 前端总装：工具栏、状态栏、文件操作、右键菜单、
 * 编辑窗格同步、全局快捷键、拖放打开、关闭确认、撤销重做整合。
 *
 * 行为逐项对齐 Qt 版 main_window.py；数据模型/网格/对话框见各自模块。
 */

import { serializeCsv, serializeRecords } from "./csv.js";
import { BigModel, setDefaultUndoBudgetMiB } from "./bigmodel.js";
import { Grid } from "./grid.js";
import { EditPane } from "./pane.js";
import { showMenu } from "./menu.js";
import {
  busyDialog, columnDialog, confirmDialog, confirmDiscardDialog,
  confirmExitSortDialog, confirmFixedPointDialog, confirmRamSortDialog,
  errorDialog, fmtBytes, infoDialog, intDialog, newTableDialog, progressDialog,
  shortcutsDialog, sortDialog, undoBudgetDialog,
} from "./dialogs.js";
import { createUndoBudgetPrefs } from "./prefs.js";

/** @type {any} Tauri v2 全局对象（withGlobalTauri） */
const tauri = window.__TAURI__;
const invoke = (cmd, args) => tauri.core.invoke(cmd, args);

const undoPrefs = createUndoBudgetPrefs(window.localStorage);
let undoBudgetMiB = undoPrefs.get();
setDefaultUndoBudgetMiB(undoBudgetMiB);

/**
 * 创建带统一固定点确认的文档模型。
 *
 * @param {object | undefined} io - 可选的 invoke/listen 测试替身。
 * @returns {BigModel} 独立文档模型。
 */
function makeDocumentModel(io) {
  // 部分 io 也要保留默认 invoke/listen，不能只传确认回调。
  return new BigModel({
    invoke,
    listen: (ev, cb) => tauri.event.listen(ev, cb),
    ...io,
    confirmFixedPoint: confirmFixedPointDialog,
  });
}

// ============ 全局状态 ============

/** 当前模型：统一 BigModel（启动为无会话占位空表；打开/新建时替换或复用） */
let model = makeDocumentModel();
let path = null;      // 当前文件路径（null = 未命名）
let dirty = false;    // 未保存修改
// 工具栏就绪旗标：gridA 构造期（const tb 初始化前）refreshBigUi/refreshHeaderlessUi
// 可被 bigFilterStatus 钩子触发——占位 BigModel 恒 isBig，大分支会走到工具栏刷新。
let tbReady = false;
/** @type {number} 大模式最近一次筛选用时（ms），状态栏展示 */
let lastFilterMs = 0;
/** @type {[number, number] | null} 编辑窗格跟踪的源坐标 */
let currentCell = null;
let dockOrigin = false; // 编辑窗格发起的写回中（避免回灌）
/** 编辑窗格最近一次异步写回（切换「首行非列名」前必须等它落定） */
let paneWrite = Promise.resolve();
/** 「首行非列名」切换进行中：防重入，同时禁用按钮 */
let headerlessBusy = false;

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

/**
 * 毫秒时长 → 用户可读文本（<1s 用 ms，否则秒保留 2 位小数）。
 *
 * @param {number} ms - 毫秒。
 * @returns {string} 展示文本。
 */
function fmtDuration(ms) {
  if (!Number.isFinite(ms) || ms < 0) return "-";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(2)} 秒`;
}

/**
 * 大模式耗时后缀（状态栏常驻）：首屏（毫秒级）+ 全量索引（秒级）。
 *
 * @param {BigModel} m - 大文件模型。
 * @returns {string} 形如 " · 首屏 320 ms · 索引 115.2 秒"；非大模式返回空串。
 */
function bigTimingSuffix(m) {
  const meta = m.meta?.();
  if (!meta) return "";
  const parts = [];
  const first = m.firstPaintMs?.() || meta.open_ms || 0;
  if (first) parts.push(`首屏 ${fmtDuration(first)}`);
  if (!meta.indexing) {
    if (meta.index_sync) {
      // 小文件在打开阶段同步扫完，open_ms 已包含索引耗时，单独报数字会误导。
      parts.push("索引随首屏完成");
    } else {
      const idx = meta.index_ms || meta.elapsed_ms || meta.open_ms;
      if (idx) parts.push(`索引 ${fmtDuration(idx)}`);
    }
  }
  return parts.length ? ` · ${parts.join(" · ")}` : "";
}

/**
 * 大文件打开完成摘要（状态栏瞬时消息）：行列、编码、首屏与索引耗时。
 *
 * @param {BigModel} m - 大文件模型。
 * @returns {string} 摘要文本。
 */
function bigOpenSummary(m) {
  const meta = m.meta();
  const idx = meta.index_ms || meta.elapsed_ms || meta.open_ms;
  const idxText = meta.index_sync ? "索引随首屏完成" : `索引 ${fmtDuration(idx)}`;
  return `大文件模式：${Number(meta.rows).toLocaleString()} 行 × ${meta.cols} 列` +
    `（${meta.encoding}）· 首屏 ${fmtDuration(m.firstPaintMs?.() || meta.open_ms)}` +
    ` · ${idxText}`;
}

/** 刷新左侧状态（显示 X / 共 Y 行；索引中显示进度与耗时） */
function updateStatusText() {
  const meta = model.meta();
  // 渐进索引进行中：百分比 + 已可用行数 + 首屏/已用耗时（EmEditor 式反馈）
  if (meta?.indexing) {
    const total = Number(meta.total_bytes) || 0;
    const pct = total
      ? Math.min(99, Math.floor((Number(meta.indexed_bytes) || 0) * 100 / total))
      : 0;
    const first = model.firstPaintMs?.() || meta.open_ms || 0;
    statusText.textContent =
      `索引中 ${pct}% · 已可用 ${Number(meta.rows).toLocaleString()} 行` +
      (first ? ` · 首屏 ${fmtDuration(first)}` : "") +
      ` · 已用 ${fmtDuration(meta.elapsed_ms)}（筛选/跳转稍后可用）`;
    return;
  }
  // 排序视图：明确提示数据在内存、尚未落盘（保存时才按当前顺序重写）。
  const sortSuffix = meta?.sorted
    ? ` · 已排序（内存 ${fmtBytes(meta.total_bytes)}，未保存）`
    : "";
  if (model.filterActive()) {
    statusText.textContent = `匹配 ${lastCounts[0]} / 共 ${meta?.rows ?? lastCounts[1]} 行` +
      (lastFilterMs ? ` · 用时 ${fmtDuration(lastFilterMs)}` : "") +
      sortSuffix;
    return;
  }
  statusText.textContent =
    `显示 ${lastCounts[0]} / 共 ${lastCounts[1]} 行${bigTimingSuffix(model)}${sortSuffix}`;
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
// 所有磁盘 CSV 页签各自持有一个 Rust 引擎会话（按会话号隔离）：切换页签
// 不关闭、不重开文件，数据/撤销/脏态随签保留。新建表格为内存引擎档（Ram，无路径）。
// 切换页签时视图重置（筛选/选区/滚动不随签保留，v1 边界）。

/**
 * @typedef {Object} DocTab
 * @property {number} id - 页签 id。
 * @property {string} title - 显示名（文件名/未命名）。
 * @property {string | null} path - 文件路径（null = 未命名）。
 * @property {BigModel | null} model - 大引擎会话（异常/装载失败时可暂为 null；
 *   无路径内存档的 model 永不卸载——内存数据离开会话即不可恢复）。
 * @property {boolean} dirty - 未保存修改。
 * @property {boolean} headerless - 首行是否为数据（列名自动生成），按档/文件独立。
 * @property {"A" | "B"} side - 所属页签组（未分屏恒 "A"）。
 */
/** @type {DocTab[]} */
let tabs = [];
/** @type {number | null} 活动页签 id；null = 当前档尚未入签（启动/单档态） */
let activeId = null;
let tabSeq = 1;

// ---- 分屏视图 ----
// 分屏时页签栏分左(A)/右(B)两组，与左右网格一一对应：每组有自己的「组当前签」
// （sideTab），该组网格恒显示其组当前签；页签可拖到任意一侧换组。焦点侧
// （focusedSide）独占键盘、工具栏、编辑窗格与状态栏，非分屏恒 "A"。点另一组
// 的组当前签 = 纯焦点交换（两侧绑定不动）；点另一组的其他签 = 该组重绑到该签
// 且焦点过去。引擎按页签多会话：切换页签只换模型，不重开、不重建索引。
// 不变量：
//  I1 组邻接：tabs 数组中 A 组签全部排在 B 组签之前；组内顺序 = 数组相对顺序。
//  I2 splitOn ⇔ 存在 side="B" 的签 ⇔ sideTab.B !== null。
//  I3 分屏期间 A 组非空且 sideTab.A !== null（A 是主组，禁止拖空；关空则 B 升格）。
//  I4 sideTab[S] 恒为 S 组成员且是该组网格显示内容；activeId === sideTab[focusedSide]。
//  I5 未分屏 ⇒ 全部 side="A"、sideTab.B=null、focusedSide="A"。
/** @type {boolean} 分屏开关 */
let splitOn = false;
/** @type {"A" | "B"} 焦点侧：工具栏/快捷键/编辑窗格的作用侧 */
let focusedSide = "A";
/** 各组的组当前签 id（该组网格正显示的签；非分屏 B 恒 null，焦点侧 = 活动签） */
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

/** @returns {"A" | "B" | null} 页签所属组（未分屏返回 null） */
function sideOfTab(id) {
  if (!splitOn) return null;
  return tabs.find((t) => t.id === id)?.side ?? null;
}

/** @returns {DocTab[]} 指定组的页签（组内顺序 = 数组相对顺序） */
function groupTabs(side) {
  return tabs.filter((t) => t.side === side);
}

/**
 * 把页签插入指定组的指定位置（维护 I1 组邻接：A 组恒在 B 组之前）。
 * 调用前须保证 t 不在数组中（新建或已摘除）。
 *
 * @param {DocTab} t - 待插入页签。
 * @param {"A" | "B"} side - 目标组。
 * @param {number} [index] - 组内插入位（越界夹取），默认末尾。
 * @returns {void}
 */
function insertTabIntoGroup(t, side, index = Infinity) {
  t.side = side;
  const groupLen = groupTabs(side).length;
  const i = Math.max(0, Math.min(index === Infinity ? groupLen : index, groupLen));
  const abs = (side === "B" ? groupTabs("A").length : 0) + i;
  tabs.splice(abs, 0, t);
}

/** 当前档状态写回活动页签（切换/渲染前调用，live 变量 ↔ 页签镜像同步） */
function syncActiveTab() {
  const t = activeTab();
  if (!t) return;
  t.path = path;
  t.dirty = dirty;
  t.headerless = model.isHeaderless?.() === true;
  t.model = model;
}

/** 当前档装入页签体系（保持内容/脏态不变；幂等；分屏时落焦点侧组） */
function ensureTabbed() {
  if (activeId !== null) return;
  const t = {
    id: tabSeq++, title: tabTitleOf(path), path,
    model, dirty,
    headerless: model.isHeaderless?.() === true,
    side: "A",
  };
  insertTabIntoGroup(t, splitOn ? focusedSide : "A");
  activeId = t.id;
  if (splitOn) sideTab[focusedSide] = t.id; // I4：当前档即焦点组当前签
}

/** 重建页签条 DOM（分屏时左右两组）并刷新溢出态；激活页签自动滚入可视区 */
function renderTabbar() {
  tabbarEl.classList.toggle("show", tabs.length >= 2);
  tabbarEl.classList.toggle("split", splitOn);
  const groupA = document.getElementById("tab-group-a");
  const groupB = document.getElementById("tab-group-b");
  groupA.textContent = "";
  groupB.textContent = "";
  const otherCur = splitOn ? sideTab[focusedSide === "A" ? "B" : "A"] : null;
  for (const t of tabs) { // I1 组邻接：A 组恒在 B 组之前，一趟遍历自然分段
    const el = document.createElement("div");
    el.className = "tab" + (t.id === activeId ? " active" : "")
      + (t.id === otherCur ? " split-side" : ""); // 绿条 = 另一侧网格正显示的组当前签
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
    el.draggable = true; // 拖拽换组/重排：事件层只做坐标换算，逻辑全在 moveTab
    el.addEventListener("dragstart", (e) => onTabDragStart(e, t.id));
    el.addEventListener("dragend", onTabDragEnd);
    (splitOn && t.side === "B" ? groupB : groupA).appendChild(el);
  }
  updateTabOverflow();
  tabScrollEl.querySelector(".tab.active")
    ?.scrollIntoView({ block: "nearest", inline: "nearest" });
}

/** @returns {HTMLElement} 焦点组的滚动容器（◀▶ 按钮的作用对象） */
function focusedGroupEl() {
  return document.getElementById(splitOn && focusedSide === "B" ? "tab-group-b" : "tab-group-a");
}

/** 溢出态：任一组页签总宽超可视宽 → 显示 ◀▶ 按钮；禁用态按焦点组滚动位置 */
function updateTabOverflow() {
  const ga = document.getElementById("tab-group-a");
  const gb = document.getElementById("tab-group-b");
  const ov = ga.scrollWidth > ga.clientWidth + 1 || gb.scrollWidth > gb.clientWidth + 1;
  tabbarEl.classList.toggle("overflow", ov);
  const sc = focusedGroupEl();
  document.getElementById("tab-prev").disabled = sc.scrollLeft <= 0;
  document.getElementById("tab-next").disabled =
    sc.scrollLeft + sc.clientWidth >= sc.scrollWidth - 1;
  if (splitOn) syncTabSplit(); // ◀▶ 显隐平移滚区左缘 22px，需重对齐
}

document.getElementById("tab-prev").addEventListener("click", () => focusedGroupEl().scrollBy({ left: -180 }));
document.getElementById("tab-next").addEventListener("click", () => focusedGroupEl().scrollBy({ left: 180 }));
// 滚轮横滚只作用于悬停组；滚动/尺寸变化刷新 ◀▶ 禁用态
for (const gid of ["tab-group-a", "tab-group-b"]) {
  const sc = document.getElementById(gid);
  sc.addEventListener("wheel", (e) => {
    if (sc.scrollWidth <= sc.clientWidth + 1) return;
    e.preventDefault();
    sc.scrollLeft += e.deltaY || e.deltaX;
  }, { passive: false });
  sc.addEventListener("scroll", updateTabOverflow);
  new ResizeObserver(updateTabOverflow).observe(sc);
}

/**
 * 页签组界与网格分界像素对齐：分屏时左组定宽 = 左网格右缘 − 页签滚区左缘 + 2px
 *（右组左缘的 2px 分隔线对到 5px vsplit 中心）。滚区左缘取实时 rect：◀▶ 按钮
 * 出现时滚区右挤 22px 仍对齐。未分屏清定宽恢复弹性全宽。
 *
 * @returns {void}
 */
function syncTabSplit() {
  const ga = document.getElementById("tab-group-a");
  if (!splitOn) {
    if (ga.style.flex) ga.style.flex = ""; // 收屏/回空：恢复弹性全宽
    return;
  }
  const gridR = document.getElementById("grid-area").getBoundingClientRect();
  const scrollL = document.getElementById("tab-scroll").getBoundingClientRect().left;
  const v = `0 0 ${Math.max(60, Math.round(gridR.right - scrollL + 2))}px`;
  if (ga.style.flex !== v) ga.style.flex = v; // 同值不写，阻断 RO 回路
}
// 窗口缩放/编辑窗格开合/分隔条拖拽等一切左网格尺寸变化，兜底同步页签组界
new ResizeObserver(() => syncTabSplit()).observe(document.getElementById("grid-area"));

// ---- 页签拖拽换组（HTML5 DnD）----
// 事件层只做「坐标 → (side, index)」换算，全部状态逻辑在 moveTab（可冒烟直调）。
// dataTransfer 在 dragover 里读不到数据（规范限制），被拖签 id 走模块级 dragTabId。

/** @type {number | null} 被拖签 id */
let dragTabId = null;
/** @type {{ side: "A" | "B", index: number } | null} 当前 dragover 算出的投放点 */
let dropTarget = null;

/** 隐藏插入位置指示器 */
function hideInsertIndicator() {
  document.getElementById("tab-insert").style.display = "none";
}

/**
 * 开始拖拽：记录被拖签；未分屏时亮出空的右组投放区（拖出分屏的入口）。
 *
 * @param {DragEvent} e - 事件。
 * @param {number} id - 被拖签 id。
 * @returns {void}
 */
function onTabDragStart(e, id) {
  dragTabId = id;
  e.dataTransfer.effectAllowed = "move";
  e.dataTransfer.setData("text/plain", String(id)); // 双保险（合成事件/跨窗口场景）
  if (!splitOn) tabbarEl.classList.add("dragpeek");
}

/** 拖拽结束（投放或取消）：清理全部拖拽态（真实序列 drop → dragend，清理幂等）。 */
function onTabDragEnd() {
  dragTabId = null;
  dropTarget = null;
  tabbarEl.classList.remove("dragpeek");
  hideInsertIndicator();
}

/**
 * 组容器上悬停：按 midX 算插入位（排除被拖签）并定位插入指示器。
 * 左组单签对右组不亮指示器且 dropEffect="none"（与 moveTab 的拖空保护一致）。
 *
 * @param {DragEvent} e - 事件（须 preventDefault 才允许投放）。
 * @param {"A" | "B"} side - 悬停组。
 * @returns {void}
 */
function onGroupDragOver(e, side) {
  if (dragTabId === null) return;
  const t = tabs.find((x) => x.id === dragTabId);
  if (!t) return;
  if (side === "B" && t.side === "A" && groupTabs("A").length === 1) {
    e.dataTransfer.dropEffect = "none"; // 左组是主组，禁止拖空（I3）
    dropTarget = null;
    hideInsertIndicator();
    return;
  }
  e.preventDefault(); // 允许 drop
  e.dataTransfer.dropEffect = "move";
  const group = document.getElementById(side === "B" ? "tab-group-b" : "tab-group-a");
  const kids = [...group.querySelectorAll(".tab")]
    .filter((el) => Number(el.dataset.id) !== dragTabId);
  let index = kids.length; // 默认末尾
  for (let i = 0; i < kids.length; i += 1) {
    const r = kids[i].getBoundingClientRect();
    if (e.clientX < r.left + r.width / 2) { index = i; break; }
  }
  dropTarget = { side, index };
  // 插入指示器：#tab-scroll 内绝对定位（CSS 隐藏态，此处只管显示与横坐标）
  const wrapR = tabScrollEl.getBoundingClientRect();
  let x;
  if (index < kids.length) {
    x = kids[index].getBoundingClientRect().left - wrapR.left - 1;
  } else if (kids.length) {
    x = kids[kids.length - 1].getBoundingClientRect().right - wrapR.left - 1;
  } else {
    x = group.getBoundingClientRect().left - wrapR.left + 2; // 空组（dragpeek）：指组首
  }
  const ind = document.getElementById("tab-insert");
  ind.style.left = `${x}px`;
  ind.style.display = "block";
}

/**
 * 拖离组容器：藏指示器（进入组内子元素不算拖离）。
 *
 * @param {DragEvent} e - 事件。
 * @returns {void}
 */
function onGroupDragLeave(e) {
  if (e.relatedTarget && e.currentTarget.contains(e.relatedTarget)) return;
  dropTarget = null;
  hideInsertIndicator();
}

/**
 * 投放：按 dragover 算好的 (side, index) 调 moveTab。
 *
 * @param {DragEvent} e - 事件。
 * @param {"A" | "B"} side - 投放组。
 * @returns {void}
 */
function onGroupDrop(e, side) {
  e.preventDefault();
  const id = dragTabId ?? (Number(e.dataTransfer.getData("text/plain")) || null);
  const target = dropTarget;
  onTabDragEnd(); // 投放即结束：清理拖拽态（真实拖拽随后还有 dragend，幂等）
  if (id === null || !target || target.side !== side) return;
  void moveTab(id, side, target.index);
}

// 组容器拖拽监听（静态挂接一次；页签的 dragstart/dragend 在 renderTabbar 里随签挂）
for (const [gid, side] of [["tab-group-a", "A"], ["tab-group-b", "B"]]) {
  const g = document.getElementById(gid);
  g.addEventListener("dragover", (e) => onGroupDragOver(e, side));
  g.addEventListener("dragleave", onGroupDragLeave);
  g.addEventListener("drop", (e) => onGroupDrop(e, side));
}

/**
 * 打开一个独立的大文件引擎会话（每个页签一个，互不腾退）。
 *
 * 统一走大引擎后，普通 CSV 也会多个页签并存；引擎按会话号隔离，
 * 切换页签不再关闭/重开文件，未保存修改与撤销栈随签保留。
 *
 * @param {number | null} tabId - 归属页签（仅用于调试；会话号由模型自持）。
 * @param {string} p - 文件路径。
 * @param {boolean} [headerless] - 首行是否作为数据打开。
 * @param {BigModel | null} [reuseModel] - 重载时复用原模型（同一会话号原位替换）。
 * @returns {Promise<BigModel | null>} 失败返回 null（已弹窗）。
 */
async function openBigEngineFor(tabId, p, headerless = false, reuseModel = null) {
  const bm = reuseModel ?? makeDocumentModel();
  if (!reuseModel) bindModelEvents(bm); // 先绑事件再 open：reset/data 通知才能到达外壳
  try {
    await bm.open(p, { headerless: !!headerless });
  } catch (err) {
    await errorDialog("打开失败", String(err?.message ?? err));
    return null;
  }
  return bm;
}

/**
 * 装载文档：所有磁盘 CSV 一律走大引擎（mmap + 渐进索引）。
 *
 * 「新建表格」（无磁盘文件）为内存引擎档（Ram，无路径）；排序需要把大引擎
 * 载入内存，属于排序模式自己的事（见 openSortDialog）。
 *
 * @param {string} p - 文件路径。
 * @param {number | null} tabId - 归属页签（多会话定位用；仅调试展示）。
 * @param {boolean} [headerless] - 首行是否作为数据（默认 false = 首行是列名）。
 * @param {BigModel | null} [reuseModel] - 重载时复用原模型（复用会话号，旧会话由引擎替换）。
 * @returns {Promise<{model: BigModel} | null>}
 */
async function loadDoc(p, tabId, headerless = false, reuseModel = null) {
  const bm = await openBigEngineFor(tabId, p, headerless, reuseModel);
  if (!bm) return null;
  return { model: bm };
}

/**
 * 换档/装载后的视图重置：清筛选选区、滚动回顶、窗格清空、计数与按钮刷新。
 *
 * @param {Grid} g - 目标网格（焦点侧；分屏焦点交换不调用本函数）。
 */
function applyViewState(g) {
  g.clearFilters({ anchor: false });
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
  // 大模式：视图已装载完成，锁定首屏耗时（只认每个模型会话的第一次）
  model.markFirstPaint?.();
}

/**
 * 把装载好的文档应用到当前档（启动/未入签态/重载共用）。
 *
 * @param {{model: any, warnings?: string[]}} doc - 装载结果。
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
    t.title = tabTitleOf(p);
    t.model = doc.model;
    t.headerless = doc.model.isHeaderless?.() === true;
  }
  renderTabbar();
  updateStatusText();
  const meta = model.meta();
  if (meta.indexing) {
    // 秒开路径：首批行已可用，其余在后台索引；筛选/跳转完成前不可用
    statusMsg(
      `大文件模式：首批 ${Number(meta.rows).toLocaleString()} 行已可查看` +
      ` · 首屏 ${fmtDuration(model.firstPaintMs?.() || meta.open_ms)}` +
      " · 后台索引中…（筛选/跳转稍后可用）", 6000);
  } else {
    statusMsg(`${bigOpenSummary(model)}，筛选框键入即筛（回车立即执行）`, 8000);
  }
  if (doc.warnings?.length) {
    void infoDialog("打开提示", doc.warnings.slice(0, 20).join("\n"));
  }
}

/**
 * 切换活动页签：状态写回旧签 → 引擎仲裁 → 换绑视图。
 * 分屏下点另一组的组当前签 = 纯焦点交换（两侧绑定不动）；点另一组的其他签
 * = 该组先静默重绑到目标签（bindSide），焦点再交换过去（顺序不能反：
 * focusSide 从 sideTab 取签）。点焦点组内其他签/未分屏 = 现状换绑路径。
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
    if (sideTab[side] !== id) {
      // 另一组的背景签：该组先静默重绑到目标签，再交换焦点
      if (!t.model) {
        const bm = await openBigEngineFor(t.id, t.path, t.headerless);
        if (!bm) return false;
        t.model = bm;
      }
      bindSide(side, t);
    }
    focusSide(side);
    return true;
  }
  if (!t.model) {
    // 页签模型缺失（异常/旧状态）：按该页签路径重开引擎
    const bm = await openBigEngineFor(t.id, t.path, t.headerless);
    if (!bm) return false;
    t.model = bm;
  }
  model = t.model;
  t.headerless = model.isHeaderless?.() === true;
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
 * 磁盘 CSV 一律走大引擎（mmap + 渐进索引）；每个页签独立会话。
 *
 * @param {string} p - 文件路径。
 * @param {object} [opts] - 选项。
 * @param {boolean} [opts.activate] - 装载后激活（默认 true）。
 * @returns {Promise<DocTab | null>} 新页签或 null（失败已弹窗）。
 */
async function loadIntoNewTab(p, opts = {}) {
  const t = {
    id: tabSeq++, title: tabTitleOf(p), path: p,
    model: null, dirty: false, headerless: false, side: "A",
  };
  // 不记忆解读方式：每次打开都从「首行是列名」开始
  const doc = await loadDoc(p, t.id);
  if (!doc) return null;
  t.model = doc.model;
  t.headerless = doc.model.isHeaderless?.() === true;
  insertTabIntoGroup(t, splitOn ? focusedSide : "A"); // 新签落焦点侧组（I1 组邻接）
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
    // 脏档确认已在本函数开头完成，openPath 不再重复弹窗
    if (!(await openPath(first, { noConfirm: true }))) return false;
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
    const t = await loadIntoNewTab(p, { activate: false });
    if (t) firstId ??= t.id;
  }
  if (firstId !== null && firstId !== activeId) await activateTab(firstId);
  return firstId !== null;
}

/**
 * 关闭页签：不抢焦点、不重绑任何网格（双组模型下点另一组的签不应换显本侧）。
 * 脏签按标题弹三选确认：「保存」先激活该签走既有保存流程（成功才继续关）、
 * 「放弃」直接关、「取消」不动。被关的是组当前签 → 同组右邻（无则左邻）补位；
 * 右组关空 → 自动收屏；左组关空且右组非空 → 右组整体升格补位（promoteGroupB）。
 *
 * @param {number} id - 页签 id。
 * @returns {Promise<boolean>} true = 已关闭（或本就不存在）；false = 用户取消/保存失败。
 */
async function closeTab(id) {
  const t = tabs.find((x) => x.id === id);
  if (!t) return true;
  const wasSide = splitOn ? t.side : "A";
  // 脏确认：活动签走 live 路径；后台脏签按标题确认，选「保存」才激活走既有保存流
  if (id === activeId) {
    if (!(await confirmDiscard())) return false;
  } else if (t.dirty) {
    const v = await confirmDiscardDialog(t.title);
    if (v === "cancel") return false;
    if (v === "save") {
      if (!(await activateTab(id))) return false;
      await saveFile();
      if (dirty) return false; // 保存失败/取消另存为：中止关签
    }
  }
  const closingActive = id === activeId; // 「保存」路径可能刚激活了目标签
  const group = groupTabs(wasSide);
  const neighbor = group[group.indexOf(t) + 1] ?? group[group.indexOf(t) - 1] ?? null;
  // 只关闭本页签的引擎会话，其他页签不受影响
  try { await t.model?.close?.(); } catch { /* 会话已关闭 */ }
  tabs.splice(tabs.indexOf(t), 1);
  if (!tabs.length) {
    resetToEmpty();
    return true;
  }
  if (splitOn && groupTabs("B").length === 0) {
    closeSplit(); // 右组关空：自动收屏（焦点在 B 时左组当前签接管）
    return true;
  }
  if (splitOn && groupTabs("A").length === 0) {
    promoteGroupB(); // 左组关空：右组整体升格补位
    return true;
  }
  if (closingActive || (splitOn && sideTab[wasSide] === id)) {
    // （组）当前签被关：同组右邻（无则左邻）补位
    if (!splitOn || wasSide === focusedSide) {
      activeId = null; // 强制 activateTab 走完整换绑
      await activateTab(neighbor.id);
    } else {
      bindSide(wasSide, neighbor);
    }
  }
  renderTabbar();
  return true;
}

/** 全部页签关闭后回空表（对齐启动态） */
function resetToEmpty() {
  splitOn = false;
  sideTab.A = null;
  sideTab.B = null;
  focusedSide = "A";
  document.body.classList.remove("split");
  document.getElementById("grid-area").style.flex = ""; // 清掉分屏拖拽残留的主侧定宽
  syncTabSplit(); // 页签左组定宽同步复位
  model = makeDocumentModel();
  bindModelEvents(model);
  path = null;
  activeId = null;
  applyViewState(fgrid());
  setDirty(false);
  renderTabbar();
  updateStatusText();
}

/**
 * 冒烟专用：关掉全部页签的大引擎会话，回到空表；有数据则经内存字节源建会话并预热。
 *
 * 统一大引擎后所有表格（含新建）都是 BigModel 会话；本钩子经 create + warmup
 * 装载内存引擎夹具表，预热后同步 rawData 断言立即可读，且能确保前序页签会话全部释放，
 * 避免旧事件落到新视图。
 *
 * @param {string[]} headers - 列名。
 * @param {string[][]} rows - 数据行。
 * @returns {Promise<void>}
 */
async function smokeLoadSmall(headers, rows) {
  closeSplit(); // 先收副网格，避免残留旧模型
  for (const t of tabs) {
    if (t.model) {
      try { await t.model.close(); } catch { /* 会话可能已关闭 */ }
    }
  }
  tabs.length = 0;
  resetToEmpty(); // 裸 BigModel 占位、清页签/分屏/计数
  if (headers.length || rows.length) {
    // 引擎化装载：内存字节源建会话（首行是列名），预热后同步 rawData 可读。
    // 单列空单元格必须落成 '""'：引擎跳过空白行，纯空行会被 from_bytes 丢掉（对齐 newFile 特判）。
    const hs = headers.map(String);
    const rs = rows.map((row) => row.map(String));
    const esc1 = (v) => (/[",\r\n]/.test(v) || v === "" ? `"${v.replace(/"/g, '""')}"` : v);
    const text = hs.length === 1
      ? [hs[0], ...rs.map((r) => esc1(r[0]))].join("\r\n") + "\r\n"
      : serializeCsv(hs, rs);
    await model.create(text, { headerless: false });
    await model.warmup();
  }
  fgrid().renderAll();
  onCountsChanged(model.rowCount(), model.rowCount());
  refreshBigUi();
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
    confirmColOps: () => confirmColOpsHistoryLoss(),
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
    model: makeDocumentModel(),
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
 * 副侧静默重绑：网格换显指定页签，不触外壳状态（编辑窗格/状态栏/计数/焦点）。
 * 视图重置对齐 applyViewState 顺序：上次内容的筛选/选区不残留。
 * 仅用于非焦点侧；焦点侧换绑走 activateTab（live 变量交换 + applyViewState）。
 *
 * @param {"A" | "B"} side - 目标侧。
 * @param {DocTab} t - 目标页签（model 须已就绪）。
 * @returns {void}
 */
function bindSide(side, t) {
  const g = gridOfSide(side);
  g.clearFilters({ anchor: false });
  g.clearSelection();
  g.setModel(t.model);
  g.scrollEl.scrollTop = 0;
  g.scrollEl.scrollLeft = 0;
  sideTab[side] = t.id;
}

/**
 * 移动页签到目标组指定位置：拖拽/右键菜单/冒烟共用的唯一原语。
 * 同组移动 = 纯重排（两侧绑定不动）；跨组移动时被移签成为目标组的组当前签；
 * 被移签原是来源组当前签时，来源组改显同组右邻（无右邻取左邻）。
 * 右组被拖空 → 自动收屏；左组是主组（I3），禁止拖空（返回 false）。
 * 焦点恒留原焦点组。
 *
 * @param {number} id - 待移动页签 id。
 * @param {"A" | "B"} targetSide - 目标组。
 * @param {number} [index] - 目标组内插入位（先摘除被移签后计），默认末尾。
 * @returns {Promise<boolean>} false = 被拒绝（拖空左组 / 签不存在 / 引擎重开失败）。
 */
async function moveTab(id, targetSide, index = Infinity) {
  const t = tabs.find((x) => x.id === id);
  if (!t) return false;
  const srcSide = splitOn ? t.side : "A";
  if (srcSide === targetSide) {
    // 同组移动：纯重排，两侧绑定不动
    tabs.splice(tabs.indexOf(t), 1);
    insertTabIntoGroup(t, targetSide, index);
    renderTabbar();
    return true;
  }
  // 左组保护：A 是主组，不允许拖空（未分屏仅一签时入 B 同此拒绝，对齐旧 openSplit）
  if (targetSide === "B" && groupTabs("A").length === 1) return false;
  if (!t.model) {
    // 防御：模型缺失（异常/旧状态）时按该签路径重开引擎
    const bm = await openBigEngineFor(t.id, t.path, t.headerless);
    if (!bm) return false;
    t.model = bm;
  }
  if (!splitOn) {
    // 未分屏 → B：立分屏（拖出分屏/「在分屏中打开」共用此路径）
    splitOn = true;
    sideTab.A = activeId;
    document.body.classList.add("split");
    syncTabSplit(); // 页签组界对到网格分界（取分屏后网格实宽）
  }
  syncActiveTab(); // live 状态先写回（被移签可能就是活动签）
  const srcGroup = groupTabs(srcSide);
  const neighbor = srcGroup[srcGroup.indexOf(t) + 1] ?? srcGroup[srcGroup.indexOf(t) - 1] ?? null;
  tabs.splice(tabs.indexOf(t), 1);
  if (sideTab[srcSide] === id && neighbor) {
    // 来源组善后：组当前签被移走，同组右邻（无则左邻）补位
    if (srcSide === focusedSide) {
      activeId = null; // 强制 activateTab 走完整换绑（live 交换 + 视图重置）
      await activateTab(neighbor.id);
    } else {
      bindSide(srcSide, neighbor);
    }
  }
  if (srcSide === "B" && groupTabs("B").length === 0) {
    closeSplit(); // 右组被拖空：自动收屏（被移签尚处摘除态，收屏不受影响）
  }
  insertTabIntoGroup(t, targetSide, index);
  sideTab[targetSide] = t.id;
  if (targetSide === focusedSide) {
    // 移入焦点组：成为活动签（live 交换 + 视图重置）
    activeId = null;
    await activateTab(t.id);
  } else {
    bindSide(targetSide, t);
  }
  renderTabbar();
  return true;
}

/**
 * 建立分屏：目标签入右组（B 网格），焦点留在左组。
 * 薄包装：语义全部由 moveTab 提供（活动签入右组时左组自动改显右邻签；
 * 仅一签时被左组保护拒绝）。每个页签都有独立引擎会话，两侧可同时开两个大文件。
 *
 * @param {number} id - 目标页签 id。
 * @returns {Promise<boolean>} true = 分屏已建立。
 */
async function openSplit(id) {
  if (splitOn) return false;
  return moveTab(id, "B");
}

/**
 * 关闭分屏：右组签依原相对序并入左组（I1 组邻接：本就在数组尾，视觉顺序不变），
 * 副网格隐藏、页签全保留。焦点在右组时左组当前签接管活动档（focusSide 从
 * sideTab.A 无条件重取：关签触发的收屏 activeId 可能残留已删签）。
 *
 * @returns {void}
 */
function closeSplit() {
  if (!splitOn) return;
  splitOn = false;
  for (const t of tabs) t.side = "A";
  sideTab.B = null;
  document.body.classList.remove("split");
  document.getElementById("grid-area").style.flex = ""; // 清掉分屏拖拽残留的主侧定宽
  syncTabSplit(); // 页签左组定宽同步复位
  if (focusedSide !== "A") {
    focusSide("A"); // 左组当前签接管活动档（live 交换 + 外壳状态还原）
  }
  if (!tabs.some((t) => t.id === sideTab.A)) sideTab.A = activeId; // I5 兜底
  renderTabbar();
}

/**
 * 左组被关空且右组非空：右组整体升格为左组并收屏（关签不能拒绝，补位结果确定）。
 * 借 closeSplit 的「焦点在 B」接管路径：升格后左组/主网格显原右组当前签。
 *
 * @returns {void}
 */
function promoteGroupB() {
  sideTab.A = sideTab.B; // 右组当前签升格为全局当前
  focusedSide = "B"; // 借 closeSplit 的焦点接管路径完成 live 交换
  closeSplit();
}

/**
 * 页签右键菜单：未分屏给「在分屏中打开」（只有一个签时禁用）；分屏态对任意签
 * 给「移到右侧/移到左侧」（与拖拽共用 moveTab 原语；A 组单签时「移到右侧」禁用，
 * 与拖空保护一致）外加「关闭分屏」。
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
  const toB = t.side === "A";
  showMenu(x, y, [
    {
      label: toB ? "移到右侧" : "移到左侧",
      disabled: toB && groupTabs("A").length === 1, // 左组是主组，禁止拖空（I3）
      onClick: () => void moveTab(t.id, toB ? "B" : "A"),
    },
    { label: "关闭分屏", onClick: () => closeSplit() },
  ]);
}

// ============ 大文件模式 UI（按钮显隐/筛选执行/列操作 pending） ============

// 注：大模式工具按钮在下方工具栏段（tb 初始化后）创建；此处仅函数定义。

/** 按引擎状态刷新专属按钮显隐与可用态（占位空表无会话：按钮恒隐藏） */
function refreshBigUi() {
  if (!tbReady) return; // 工具栏创建前（gridA 构造期）的钩子触发静默忽略
  // 排序需载入内存；索引完成前不可点。列操作两按钮仅 pending 时出现。
  // 「执行筛选」按钮已移除：筛选框键入即筛，回车立即执行。
  tb.sort.classList.remove("hidden");
  tb.sort.disabled = model.indexing?.() === true;
  const pending = model.colOpsPending?.() === true;
  const sorted = model.meta?.()?.sorted === true;
  tb.exitSort?.classList.toggle("hidden", !sorted);
  tb.execColOps.classList.toggle("hidden", !pending);
  tb.cancelColOps.classList.toggle("hidden", !pending);
  refreshHeaderlessUi();
}

/** 刷新「首行非列名」按钮：开启态、可用性、说明 */
function refreshHeaderlessUi() {
  if (!tbReady) return; // 同上：构造期静默
  if (!tb.headerless) return;
  const on = model.isHeaderless?.() === true;
  const pending = model.colOpsPending?.() === true;
  const sorted = model.meta?.()?.sorted === true;
  tb.headerless.classList.toggle("on", on);
  // 无列（无法生成列1..N）、列操作待执行或排序视图中（切换会错位）时禁用
  tb.headerless.disabled = headerlessBusy || model.columnCount() === 0 || pending || sorted;
  tb.headerless.title = "开启后：第一行作为数据参与筛选/编辑，列名显示为 列1、列2…；"
    + "关闭时：当前第一行成为列名并从数据中移除。仅对当前页签生效，不写入磁盘。";
}

/**
 * 切换「首行非列名」：先落定未提交编辑，再清筛选/视图，最后换模型状态。
 *
 * 大模式格内编辑与编辑窗格写回是异步的；必须在改变行坐标前等待，
 * 否则写回会落到错误的行。切换是视图属性：清空筛选/选区/撤销，但不标脏。
 *
 * @returns {Promise<boolean>} true = 切换后处于无列名状态。
 */
async function toggleHeaderless() {
  if (headerlessBusy) return model.isHeaderless?.() === true;
  if (model.colOpsPending?.()) {
    statusMsg("列操作待执行：请先执行或撤销列操作", 3000);
    return model.isHeaderless?.() === true;
  }
  const want = !(model.isHeaderless?.() === true);
  headerlessBusy = true;
  refreshHeaderlessUi();
  try {
    // 1) 未提交的格内编辑与窗格异步写回先落定（行坐标即将 ±1）
    await fgrid().commitEdit();
    await paneWrite;
    // 2) 先退出筛选：旧命中行号在切换后的坐标系里会错位
    if (model.filterActive()) {
      try { await model.clearFilter(); } catch { /* 会话已关闭：继续切换 */ }
    }
    // 3) 清视图（筛选框/多重输入/选区/滚动/窗格）
    fgrid().clearFilters({ anchor: false });
    fgrid().clearSelection();
    fgrid().resetScroll();
    multiInput.value = "";
    clearDock();
    lastFilterMs = 0;
    clearTimeout(bigFilterTimer);
    bigFilterSeq += 1;
    // 4) 模型/引擎切换；失败信息原样给用户，状态保持不变
    await model.setHeaderless(want);
    // 5) 页签镜像 + 计数与按钮（不做持久化：绿色轻量，重开即默认）
    const t = activeTab();
    if (t) t.headerless = model.isHeaderless?.() === true;
    onCountsChanged(model.rowCount(), model.rowCount());
    statusMsg(model.isHeaderless?.() === true
      ? "首行已作为数据参与筛选/编辑（列名显示为 列1、列2…）"
      : "当前第一行已成为列名", 3000);
  } catch (err) {
    statusMsg(String(err?.message ?? err), 4000);
  } finally {
    headerlessBusy = false;
    refreshHeaderlessUi();
  }
  return model.isHeaderless?.() === true;
}

/** @type {number} 大模式键入即筛防抖计时器 */
let bigFilterTimer = 0;
/** @type {number} 筛选请求序号：只有最新请求允许改写状态栏 */
let bigFilterSeq = 0;

/**
 * 收集大模式当前筛选条件（逐列 AND + 多重 OR）。
 *
 * @returns {{kws: Array<[number, string, number]>, mKw: string, mCols: number[]}}
 *   列号/关键字/严格模式（0 包含 / 1 等于·不分大小写 / 2 等于·区分大小写）、
 *   多重关键字与勾选列。
 */
function collectBigFilter() {
  const cc = model.columnCount();
  const kws = [];
  for (const [col, kw] of fgrid().filters) {
    if (col < cc && kw) kws.push([col, kw, fgrid().filterModes.get(col) ?? 0]);
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
  if (model.indexing?.()) {
    statusMsg("文件仍在索引中，暂不支持筛选（已可用 " +
      `${Number(model.meta()?.rows ?? 0).toLocaleString()} 行）`, 2500);
    return;
  }
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
  if (model.indexing?.()) {
    statusMsg("文件仍在索引中，暂不支持筛选（已可用 " +
      `${Number(model.meta()?.rows ?? 0).toLocaleString()} 行）`, 2500);
    return;
  }
  clearTimeout(bigFilterTimer);
  const { kws, mKw, mCols } = collectBigFilter();
  if (!kws.length && !mKw) {
    // 无条件下执行筛选（如回车）= 回到全量（通常已由 grid 自动清空，此处兜底）；
    // 走 grid.clearFilters 统一锚定路径（选中行/视口回到原位置，不回顶）。
    // 会话已关闭时 filterActive 为 false 短路，链路无 invoke，不会抛。
    fgrid().clearFilters();
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
    statusMsg(`匹配 ${n} 行 · 用时 ${fmtDuration(lastFilterMs)}`, 4000);
    updateStatusText();
  } catch (err) {
    if (seq === bigFilterSeq) await errorDialog("筛选失败", String(err?.message ?? err));
  }
}

/**
 * 确认执行将清空撤销/重做历史的列操作。
 *
 * @returns {Promise<boolean>} 用户是否确认继续。
 */
function confirmColOpsHistoryLoss() {
  return confirmDialog("执行列操作将清空此前所有撤销/重做历史，且无法恢复。是否继续？");
}

/** 执行 pending 列操作（确认历史丢失后物理重写 + 进度条） */
async function applyColOps() {
  if (!model.colOpsPending()) return;
  if (model.indexing?.()) {
    statusMsg("文件仍在索引中，暂不支持列操作", 2500);
    return;
  }
  const confirmed = await confirmColOpsHistoryLoss();
  if (!confirmed) return;
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
    let write;
    try {
      // 同格连续击键合并为一步撤销（对齐 set_data_merged）
      write = model.setDataMerged(r, c, value);
    } finally {
      dockOrigin = false;
    }
    // 大模式写回异步：串成链跟踪全部落定，切换「首行非列名」前等待，
    // 避免前面的写回在行坐标变化后才完成而落到错误的行。
    paneWrite = paneWrite
      .then(() => write)
      .catch((err) => {
        statusMsg(String(err?.message ?? err), 3000);
      });
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
  ["headerless", "首行非列名", () => void toggleHeaderless()],
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
  ["exitSort", "退出排序", () => void exitSortMode(), "恢复排序前的行顺序并释放排序内存；未保存的单元格修改保留"],
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

const btnUndoBudget = document.createElement("button");
btnUndoBudget.id = "btn-undo-budget";
btnUndoBudget.textContent = "撤销内存…";
btnUndoBudget.title = "设置每个页签独立的撤销/重做历史内存预算";
btnUndoBudget.addEventListener("click", () => { void changeUndoBudget(); });
btnUndoBudget.addEventListener("mousedown", (e) => e.preventDefault());
toolbar.appendChild(btnUndoBudget);
tb.undoBudget = btnUndoBudget;

// 快捷键说明：靠右对齐，位于编辑窗格「显示符号」按钮的正上方。
// 窗口较窄/高 DPI 时工具栏换行，按钮仍保持在所在行的最右。
const btnShortcuts = document.createElement("button");
btnShortcuts.id = "btn-shortcuts";
btnShortcuts.textContent = "快捷键";
btnShortcuts.title = "查看键盘快捷键";
btnShortcuts.addEventListener("click", () => { void shortcutsDialog(); });
btnShortcuts.addEventListener("mousedown", (e) => e.preventDefault());
toolbar.appendChild(btnShortcuts);
tb.shortcuts = btnShortcuts;

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
tbReady = true;
refreshBigUi(); // 确立占位空表初始按钮态（显隐/可用）

/** 按撤销栈状态刷新工具栏按钮（对齐 _update_undo_actions） */
function updateUndoButtons() {
  tb.undo.disabled = !model.canUndo();
  tb.redo.disabled = !model.canRedo();
}

/**
 * 将引擎脏点同步到模型所属页签，不污染异步完成后的其他焦点外壳。
 *
 * @param {BigModel} m - 已完成预算或历史操作的模型。
 * @returns {void}
 */
function syncModelDirty(m) {
  const modified = m.modified();
  if (m === model) {
    setDirty(modified);
    updateStatusText();
  } else {
    for (const t of tabs) if (t.model === m) t.dirty = modified;
  }
  renderTabbar();
}

/** 设置已打开及在途文档的历史预算，成功后再持久化之后新文档的默认值。 */
async function changeUndoBudget() {
  const value = await undoBudgetDialog(undoBudgetMiB);
  if (value === null) return;
  const models = new Set([model, ...tabs.map((t) => t.model), gridB.model]);
  // 下发期间禁止再次发起，避免旧请求后完成覆盖新请求。
  btnUndoBudget.disabled = true;
  try {
    // 设置期间及在途 open/create 都追随目标值；失败恢复旧默认，成功才持久化。
    setDefaultUndoBudgetMiB(value);
    for (const m of models) {
      if (!m?.meta()) continue;
      await m.setUndoBudget(value);
      // 缩容可能淘汰历史或建立固定点；每个成功模型都立即同步自己的脏点。
      syncModelDirty(m);
    }
    undoBudgetMiB = value;
    undoPrefs.set(value);
    statusMsg(`撤销/重做预算已设为每文档 ${value} MiB`, 3000);
  } catch (err) {
    setDefaultUndoBudgetMiB(undoBudgetMiB);
    await errorDialog("设置撤销内存失败", String(err?.message ?? err));
  } finally {
    btnUndoBudget.disabled = false;
  }
}

// ============ 撤销/重做（G3） ============

/** 撤销一步表格改动（Ctrl+Z；大模型 undo 为异步） */
async function undo() {
  const m = model;
  if (await m.undo()) {
    // await 后可能已切签，只清理仍属于发起模型的当前视图。
    if (m === model) {
      fgrid().clearSelection();
      clearDock();
    }
    syncModelDirty(m);
  }
}

/** 重做一步表格改动（Ctrl+Y / Ctrl+Shift+Z） */
async function redo() {
  const m = model;
  if (await m.redo()) {
    if (m === model) {
      fgrid().clearSelection();
      clearDock();
    }
    syncModelDirty(m);
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
 * 绑定模型事件（换模型时重绑；旧模型连同监听一并废弃）。
 * 分屏下两侧模型都存活且有事件流：只有焦点侧（m === model）的事件允许
 * 触碰共享外壳状态（脏点/编辑窗格/状态栏），副侧事件仅驱动其自身网格重绘
 * （网格在 setModel 里自行订阅，不经此处）。
 *
 * @param {BigModel} m - 模型实例。
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
    // 视图属性、预取和历史通知不直接标脏；历史脏点由 wrapper 按引擎 meta 同步。
    else if (origin !== "view" && origin !== "fetch" && origin !== "history") {
      markDirty();
    }
    refreshBigUi();
    if (currentCell === null) return;
    const [r, c] = currentCell;
    if (structural) {
      // 行列增删/排序：越界清空，否则重载内容
      if (r >= m.rowCount() || c >= m.columnCount()) clearDock();
      else {
        // 页未命中（页被换出）保留窗格旧内容，数据到齐再收敛
        const t = m.peekData(r, c) ?? null;
        if (t !== null) pane.setText(t);
      }
      return;
    }
    // 编辑窗格自己发起的写回不再回灌（避免光标跳动）
    if (dockOrigin) return;
    // 当前格页被换出时 rawData 会读成空串、还顺手重拉该页——与缓存
    // trim 形成乒乓驱逐循环，且把窗格刷空造成频闪。peekData 未命中时保留旧内容。
    const newText = m.peekData(r, c) ?? null;
    if (newText !== null && pane.editEl.value !== newText) pane.setText(newText);
  });
  m.onUndoStack(() => {
    if (m !== model) return;
    updateUndoButtons();
  });
  // 渐进索引：轮询进度刷新状态栏；完成瞬间报一次首屏/索引耗时
  let wasIndexing = m.indexing?.() === true;
  m.onIndexProgress?.((meta) => {
    if (m !== model) return; // 副侧进度不得改写焦点侧状态栏
    updateStatusText();
    const now = meta?.indexing === true;
    if (wasIndexing && !now) statusMsg(`${bigOpenSummary(m)}，筛选已可用`, 8000);
    wasIndexing = now;
  });
  // 大表撤销降级：状态栏提示一次（小模型专属；BigModel 无此方法）
  m.onUndoCap?.((cap) => {
    if (m !== model) return;
    statusMsg(`表格较大：撤销上限已降为 ${cap} 步`, 6000);
  });
}
bindModelEvents(model);

// ============ 文件操作（F2/F3/F4） ============

/**
 * 新建表格：只选列数 × 行数；建好后处于「首行非列名」模式。
 *
 * 用户可在第一行填写列名，再关闭「首行非列名」把首行提升为列名。
 * 作为新页签打开（旧档保留）。
 *
 * @returns {Promise<void>}
 */
async function newFile() {
  const spec = await newTableDialog();
  if (spec === null) return;
  // 统一大引擎：新建即内存字节源会话（不写盘、无路径，另存为后自动获得路径）。
  const rows = Array.from({ length: spec.rows }, () => Array(spec.cols).fill(""));
  // 单列全空行序列化为 "" 才是非空行（引擎与 parseCsv 一样跳过空白行）：
  // 逐行加引号的空字段 "" 既非空白行，解析回来仍是一个空串字段。
  const text = spec.cols === 1
    ? Array.from({ length: spec.rows }, () => '""').join("\r\n") + "\r\n"
    : serializeRecords(rows);
  const m = makeDocumentModel();
  bindModelEvents(m); // 先绑事件再建会话：与 openBigEngineFor 同序
  try {
    await m.create(text, { headerless: true });
  } catch (err) {
    await errorDialog("新建失败", String(err?.message ?? err));
    return;
  }
  const pristine = tabs.length === 0 && activeId === null && path === null && !dirty
    && model.rowCount() === 0 && model.columnCount() === 0;
  if (pristine) {
    // 启动空表原位替换（尚未入签体系；事件已在建会话前绑定）
    model = m;
    applyViewState(fgrid());
    setDirty(false);
    renderTabbar();
    updateStatusText();
    return;
  }
  ensureTabbed();
  const t = {
    id: tabSeq++, title: "未命名", path: null, model: m, dirty: false,
    headerless: true, side: "A",
  };
  insertTabIntoGroup(t, splitOn ? focusedSide : "A"); // 新签落焦点侧组（I1 组邻接）
  await activateTab(t.id);
}

/** 打开 CSV：文件对话框选择（作为新页签；已有路径则激活） */
async function openFile() {
  const p = await invoke("pick_open_path");
  if (p) await openPath(p);
}

/**
 * 打开指定路径的 CSV（菜单/拖放/启动/重载共用）。
 *
 * 所有磁盘 CSV 一律走大引擎（mmap + 渐进索引）：打开秒回、筛选在 Rust
 * 分块扫描；需要排序时再由用户确认载入内存。
 * 入签规则：路径已有签 → 激活；未入签体系 → 装当前档；否则新开页签。
 *
 * @param {string} p - CSV 文件路径。编码自动识别（Rust 层）。
 * @param {object} [opts] - 选项。
 * @param {boolean} [opts.replace] - 重载当前签内容（外部改动重载），先确认丢弃。
 * @param {boolean} [opts.noConfirm] - 未入签换档/replace 时跳过丢弃确认
 *   （调用方已问过，避免重复弹窗）。
 * @returns {Promise<boolean>} true = 成功打开/切换。
 */
async function openPath(p, opts = {}) {
  if (opts.replace) {
    // 重载当前签：复用同一会话号原位替换内容，丢弃前确认。
    // 沿用当前档的「首行非列名」解读，避免外部改动重载后视图含义突变。
    if (!opts.noConfirm && !(await confirmDiscard())) return false;
    const headerless = model.isHeaderless?.() === true;
    const reuse = model;
    const doc = await loadDoc(p, activeId, headerless, reuse);
    if (!doc) return false;
    applyDocToCurrent(doc, p);
    return true;
  }
  const exist = tabs.find((t) => t.path === p);
  if (exist) return activateTab(exist.id);
  if (tabs.length === 0) {
    // 启动/初始空表：装当前档（暂不生成页签）；不记忆上次解读。
    // 未入签的档也可能已被编辑（首个文件尚未形成页签）：换文件前必须确认，
    // 否则未保存修改会静默丢失。
    if (dirty && !opts.noConfirm && !(await confirmDiscard())) return false;
    const doc = await loadDoc(p, activeId);
    if (!doc) return false;
    applyDocToCurrent(doc, p);
    return true;
  }
  ensureTabbed();
  return (await loadIntoNewTab(p)) !== null;
}

/**
 * 大文件模式写盘共用流程：进度框 + 引擎调用 + 视图/筛选/脏态复位。
 *
 * @param {() => Promise<void>} write - 实际写盘（model.save 或 model.saveAs）。
 * @param {string} action - 动作名（用于失败对话框标题）。
 * @param {string} okMsg - 成功后的状态栏文案。
 * @param {string} hint - 失败对话框的操作建议。
 * @returns {Promise<boolean>} true = 已写入成功。
 */
async function runBigWrite(write, action, okMsg, hint) {
  const hadFilter = model.filterActive();
  const dlg = progressDialog("保存大文件…");
  const un = await model.onProgress((done, total, phase, rows) => dlg.set(done, total, phase, rows));
  try {
    await write();
  } catch (err) {
    un();
    dlg.close();
    await errorDialog(`${action}失败`, `${err?.message ?? err}\n${hint}`);
    return false;
  }
  un();
  // 完成帧短暂停留再关闭：重写期间磁盘 IO 可能饿死渲染管线，屏幕停在
  // 中间的旧帧——保证"100% 完成"被看到，而不是"卡住后消失"。
  await dlg.finish();
  setDirty(false);
  // 引擎重开文件后不再保留筛选结果：同步清掉筛选框，避免"框里有条件、
  // 视图却是全量"的错位（只在保存前确有筛选时动它，不误清多重筛选输入）。
  if (hadFilter) fgrid().clearFilters({ anchor: false });
  // 排序保存会按当前顺序物理重写并退出排序视图：按钮/状态随之刷新。
  refreshBigUi();
  onCountsChanged(model.rowCount(), model.meta()?.rows ?? model.rowCount());
  statusMsg(okMsg, 3000);
  return true;
}

/**
 * 保存到当前路径（无路径时转另存为）。
 *
 * @returns {Promise<boolean>} true = 已写入成功。
 */
async function saveFile() {
  // 启动占位空表（无会话）：没有可保存的东西
  if (!model.meta()) {
    statusMsg("空表格无需保存：请先新建或打开文件", 2500);
    return false;
  }
  if (model.indexing?.()) {
    // 索引未完成时引擎无法流式重写（后台线程仍持有映射），提示稍候
    statusMsg("文件仍在索引中，请稍候再保存", 2500);
    return false;
  }
  if (path === null) {
    // 未命名内存档：直存无目标，走另存为
    return saveFileAs();
  }
  return runBigWrite(
    () => model.save(),
    "保存",
    "已保存（大文件模式：流式重写完成）",
    "文件可能被其他程序占用，可尝试另存为。",
  );
}

/**
 * 另存为 CSV。
 *
 * 大文件模式把当前视图流式写到新路径（原文件保持不变），成功后引擎会话
 * 与会话路径一并切换；只有写入成功才更新文档身份，失败时保留原路径，
 * 避免“标题已变、磁盘没变”的错位。未命名内存档另存为后自动获得路径。
 *
 * @param {string | null} target - 可选目标路径（冒烟测试跳过原生对话框）。
 * @returns {Promise<boolean>} true = 已写入成功。
 */
async function saveFileAs(target = null) {
  if (!model.meta()) {
    statusMsg("空表格无需保存：请先新建或打开文件", 2500);
    return false;
  }
  if (model.indexing?.()) {
    statusMsg("文件仍在索引中，请稍候再另存为", 2500);
    return false;
  }
  let p = target;
  if (!p) {
    p = await invoke("pick_save_path", { defaultName: "未命名.csv" });
    if (!p) return false;
  }
  if (!p.toLowerCase().endsWith(".csv")) p += ".csv";

  const ok = await runBigWrite(
    () => model.saveAs(p),
    "另存为",
    "已另存为",
    "请检查目标目录是否可写，或换一个位置。",
  );
  if (!ok) return false;
  path = p;
  // 另存后当前档的身份（路径/标题）随之变化，同步回页签
  syncActiveTab();
  renderTabbar();
  return true;
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
  if (!path) return;
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

/**
 * 大模式排序：估算 → 确认 → 可取消载入 → 引擎排列 → 刷新视图。
 *
 * 排序只改变视图（不写盘）；保存时才按当前顺序物理重写。
 *
 * @param {Array<{col: number, numeric: boolean, asc: boolean}>} levels - 排序级别。
 * @param {{confirm?: boolean}} [opts] - confirm=false 跳过内存确认（冒烟/内部复用）。
 * @returns {Promise<boolean>} true = 已进入排序视图。
 */
async function applyBigSort(levels, opts = {}) {
  if (!model.meta() || !levels?.length) return false;
  if (model.indexing?.()) {
    statusMsg("文件仍在索引中，请稍候再排序", 2500);
    return false;
  }
  // 未提交的格内编辑/窗格写回先落定：排序会改变显示行坐标。
  await fgrid().commitEdit();
  await paneWrite;
  if (model.meta()?.ram_backed !== true) {
    if (opts.confirm !== false) {
      const est = await model.sortEstimate(levels.length);
      if (!(await confirmRamSortDialog(est))) return false;
    }
    const dlg = progressDialog("为排序载入内存…", {
      phases: ["正在载入内存…"],
      cancelText: "取消载入",
      onCancel: () => { void model.cancelSortLoad(); },
    });
    const un = await model.onSortProgress((done, total) => dlg.set(done, total, 0));
    let loaded = false;
    try {
      loaded = await model.loadIntoRam();
    } catch (err) {
      un();
      dlg.close();
      await errorDialog("载入失败", String(err?.message ?? err));
      return false;
    }
    un();
    if (!loaded) {
      dlg.close();
      statusMsg("已取消载入，文件未改变", 3000);
      return false;
    }
    dlg.close();
  }
  const busy = busyDialog("正在排序…");
  try {
    await model.sort(levels);
  } catch (err) {
    busy.close();
    await errorDialog("排序失败", String(err?.message ?? err));
    return false;
  }
  busy.close();
  fgrid().clearSelection();
  clearDock();
  fgrid().resetScroll();
  // 筛选命中集已随排序重映射：可见数取当前视图，总数始终是全表行数。
  onCountsChanged(model.rowCount(), model.meta()?.rows ?? model.rowCount());
  refreshBigUi();
  statusMsg(`已按 ${levels.length} 个级别排序（尚未写入磁盘；保存时按当前顺序重写）`, 5000);
  return true;
}

/**
 * 打开排序对话框并执行排序；排序前清空选中避免行号失效。
 *
 * 内存档与磁盘档统一走
 * {@link applyBigSort}（内存估算 + 可取消载入 + 排列层）。
 *
 * @returns {Promise<void>}
 */
async function openSortDialog() {
  const names = model.headers();
  if (!names.length) return; // 占位空表无列：对话框无从谈起
  if (model.indexing?.()) {
    statusMsg("文件仍在索引中，请稍候再排序", 2500);
    return;
  }
  await fgrid().commitEdit();
  await paneWrite;
  const levels = await sortDialog(names);
  if (!levels || !levels.length) return;
  await applyBigSort(levels.map(([col, mode, asc]) => ({
    col, numeric: mode === "numeric", asc,
  })));
}

/**
 * 退出排序视图：提示保存/放弃，随后释放排序内存并恢复原行顺序。
 *
 * @returns {Promise<boolean>} true = 已退出（或保存成功）。
 */
async function exitSortMode() {
  if (model.meta?.()?.sorted !== true) return false;
  await fgrid().commitEdit();
  await paneWrite;
  const choice = await confirmExitSortDialog();
  if (choice === "cancel") return false;
  if (choice === "save") return saveFile();
  try {
    const meta = await model.exitSort();
    fgrid().clearSelection();
    clearDock();
    fgrid().resetScroll();
    // 退出排序后筛选命中集已映射回原始行号，仍可能处于筛选视图。
    onCountsChanged(model.rowCount(), meta.rows);
    // 排序本身不算修改：引擎仍有内容修改时才保留脏点。
    setDirty(meta.modified === true);
    refreshBigUi();
    statusMsg(`已退出排序并释放排序内存（文件 ${fmtBytes(meta.total_bytes)} 回到磁盘映射视图）`, 4000);
    return true;
  } catch (err) {
    await errorDialog("退出排序失败", String(err?.message ?? err));
    return false;
  }
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
    { label: "重命名列…", disabled: model.isHeaderless?.() === true, onClick: () => renameColumn(col) },
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
  if (model.indexing?.()) {
    statusMsg("文件仍在索引中，暂不支持跳转行（已可用 " +
      `${Number(model.meta()?.rows ?? 0).toLocaleString()} 行）`, 2500);
    return;
  }
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
  // 无列名模式至少留一行：否则保存会产出空文件/空行，重开语义不稳定。
  // 在确认框之前拦下，避免用户确认后才看到错误。
  if (rows.length && model.isHeaderless?.() && rows.length >= model.rowCount()) {
    statusMsg("无列名模式需至少保留一行", 3000);
    return;
  }
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
  fgrid().clearFilters({ anchor: false });
}

/**
 * 二次确认后删除指定列并清空筛选。
 *
 * @param {number[]} cols - 列号列表。
 */
async function deleteColumns(cols) {
  if (cols.length && await confirmDialog(`确定删除选中的 ${cols.length} 列及其全部数据？（列操作将预览，需点「执行列操作」生效）`)) {
    op(model.removeColumns(cols));
    fgrid().clearFilters({ anchor: false });
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
  if (model.isHeaderless?.() === true) {
    statusMsg("无列名模式不可重命名列（列名为自动生成的 列1、列2…）", 3000);
    return;
  }
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
      syncTabSplit(); // 页签组界跟随分隔条
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
  updateStatusText();
  refreshHeaderlessUi();
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
  // 冒烟专用：立即把瞬时消息还原为常驻状态（不受 8 秒超时影响）
  refreshStatus: updateStatusText,
  newFile, openPath, saveFile, saveFileAs, undo, redo,
  openSortDialog, exitSortMode, applyBigSort,
  applyColOps, cancelColOps, runBigFilter, gotoRow, gotoRowDialog,
  isDirty: () => dirty, getPath: () => path,
  isBigMode: () => model.isBig === true,
  isHeaderless: () => model.isHeaderless?.() === true,
  toggleHeaderless,
  // 冒烟专用：复位页签会话后装载内存引擎夹具表
  smokeLoadSmall,
  // 多档页签：快照 / 切换 / 关闭 / 批量拖入 / 移组
  tabs: () => tabs.map((t) => ({
    id: t.id, title: t.title, path: t.path,
    dirty: t.id === activeId ? dirty : t.dirty,
    active: t.id === activeId,
    headerless: t.id === activeId ? model.isHeaderless?.() === true : t.headerless,
    side: t.side,
    current: splitOn && sideTab[t.side] === t.id,
  })),
  activateTab, closeTab, openPathsAsTabs, moveTab,
  // 分屏视图：状态快照 / 建立 / 关闭
  split: () => ({
    on: splitOn,
    focused: focusedSide,
    a: groupTabs("A").map((t) => t.id),
    b: splitOn ? groupTabs("B").map((t) => t.id) : [],
    curA: sideTab.A,
    curB: sideTab.B,
  }),
  openSplit, closeSplit,
};
