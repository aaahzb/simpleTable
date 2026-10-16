/**
 * --smoke 对抗自检套件（H1）：在真实 WebView 里跑端到端断言。
 *
 * 覆盖：Rust 命令层（版本/编码/BOM）、网格渲染与虚拟滚动、选区模型、
 * 格内编辑器键位、筛选、编辑窗格、工具栏、剪贴板三级判据、撤销合并、
 * 右键菜单文案、列宽/行高/列拖动、缩放夹取、多文件页签（入签/切换/关闭/
 * 溢出滚动/大档引擎单例）、分屏视图（打开/焦点交换/双大档拒绝/关签联动）、
 * 大文件模式（路由/翻页取数/编辑撤销重做/筛选/行插删/列操作 pending→执行/
 * 保存比对/超高行数虚拟缩放）。
 * 结果写入 simpletable_smoke_report.txt 并以退出码上报（0 = 全过）。
 */

import { parseCsv } from "./csv.js";

/** @type {Array<[string, () => Promise<void>]>} */
const checks = [];
const check = (name, fn) => checks.push([name, fn]);

/** 简易断言 */
function assert(cond, msg) {
  if (!cond) throw new Error(msg || "断言失败");
}

/** 同步休眠 */
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** 派发鼠标事件 */
function mouse(el, type, opts = {}) {
  el.dispatchEvent(new MouseEvent(type, {
    bubbles: true, cancelable: true, button: 0, ...opts,
  }));
}

/** 派发键盘事件 */
function key(target, k, opts = {}) {
  target.dispatchEvent(new KeyboardEvent("keydown", {
    bubbles: true, cancelable: true, key: k, ...opts,
  }));
}

/** 点击指定单元格（先滚到可视区） */
function clickCell(grid, r, c) {
  grid.ensureVisible(r, c);
  grid._renderRows();
  const el = document.querySelector(`.cell[data-row="${r}"][data-col="${c}"]`);
  if (!el) throw new Error(`单元格 ${r},${c} 不在 DOM`);
  mouse(el, "mousedown");
  mouse(document, "mouseup");
  return el;
}

/** 当前格坐标 */
const cur = (grid) => grid.sel.current;

/**
 * 运行冒烟套件。
 *
 * @param {object} ctx - 外壳上下文。
 * @param {import("./model.js").TableModel} ctx.model - 模型。
 * @param {import("./grid.js").Grid} ctx.grid - 网格。
 * @param {import("./pane.js").EditPane} ctx.pane - 编辑窗格。
 * @param {(cmd: string, args?: object) => Promise<any>} ctx.invoke - Tauri invoke。
 */
export async function runSmoke({ model: _model, grid, pane, invoke }) {
  const lines = [];
  let failed = 0;
  const TMP = (await invoke("smoke_tmp_dir"));

  // openPath/newFile 会热交换全局模型（大文件模式）：放弃入参快照，
  // 改经实时代理访问当前模型，避免"断言打在被替换的旧模型上"的假败。
  const model = new Proxy(/** @type {any} */ ({}), {
    get: (_t, k) => {
      const v = window.__app.model[k];
      return typeof v === "function" ? v.bind(window.__app.model) : v;
    },
    set: (_t, k, val) => { window.__app.model[k] = val; return true; },
  });

  // ---- 基础数据 ----
  const headers = ["目的地", "评分", "备注"];
  const rows = [
    ["大理", "3", "古城"],
    ["丽江", "10", "雪山"],
    ["洱海", "5", ""],
  ];
  const loadBasic = () => {
    grid.clearFilters(); // 与 app.js openPath/newFile 一致：装载即清筛选
    model.load(headers, rows.map((r) => r.slice()));
    grid.renderAll();
  };

  // ========== Rust 命令层 ==========

  check("版本号非空且为 beta/release 格式", async () => {
    const v = await invoke("get_version");
    assert(/^(release v|beta\+)/.test(v), `版本格式异常: ${v}`);
  });

  check("写读往返：UTF-8 BOM + 引号转义", async () => {
    const text = 'a,b\r\n"x,y","he said ""hi"""\r\n';
    await invoke("write_csv_file", { path: `${TMP}/rt.csv`, text });
    const back = await invoke("read_csv_file", { path: `${TMP}/rt.csv` });
    assert(back.text === text, "往返文本不一致");
    assert(back.encoding === "utf-8-sig", `编码应为 utf-8-sig，实际 ${back.encoding}`);
  });

  check("GBK 文件自动识别解码", async () => {
    // "中文,1\r\n" 的 GBK 字节（中=D6D0 文=CEC4）
    const gbk = [0xD6, 0xD0, 0xCE, 0xC4, 0x2C, 0x31, 0x0D, 0x0A];
    await invoke("smoke_write_bytes", { path: `${TMP}/gbk.csv`, bytes: gbk });
    const back = await invoke("read_csv_file", { path: `${TMP}/gbk.csv` });
    assert(back.encoding === "gbk", `编码应为 gbk，实际 ${back.encoding}`);
    assert(back.text.startsWith("中文"),
      `GBK 解码内容错误: enc=${back.encoding} codes=` +
      [...back.text].slice(0, 4).map((ch) => ch.codePointAt(0).toString(16)).join(","));
  });

  // ========== 网格渲染 ==========

  check("装载后渲染行/列表头正确", async () => {
    loadBasic();
    const domRows = document.querySelectorAll("#rows .row");
    assert(domRows.length === 3, `应渲染 3 行，实际 ${domRows.length}`);
    const heads = [...document.querySelectorAll(".col-name")].map((e) => e.textContent);
    assert(JSON.stringify(heads) === JSON.stringify(headers), `表头 ${heads}`);
    const filterInputs = document.querySelectorAll(".col-filter input");
    assert(filterInputs.length === 3, "筛选输入框应为 3 个");
    assert(filterInputs[0].placeholder === "筛选…", "占位文本应为 筛选…");
  });

  check("虚拟滚动：8000 行只渲染可视行", async () => {
    model.load(["a"], Array.from({ length: 8000 }, (_, i) => [`v${i}`]));
    grid.renderAll();
    const n = document.querySelectorAll("#rows .row").length;
    assert(n < 100, `可视行应 < 100，实际 ${n}`);
    assert(model.rowCount() === 8000, "总行数应保留 8000");
  });

  check("显示文本压平换行/Tab（单行不变形）", async () => {
    model.load(["a"], [["line1\nline2\ttail"]]);
    grid.renderAll();
    const cell = document.querySelector(".cell");
    assert(cell.textContent === "line1 line2 tail", `压平结果: ${cell.textContent}`);
  });

  // ========== 选区模型 ==========

  check("单击选中 + 当前格高亮", async () => {
    loadBasic();
    clickCell(grid, 1, 1);
    assert(cur(grid)?.[0] === 1 && cur(grid)?.[1] === 1, "当前格应为 1,1");
    assert(document.querySelector(".cell.cur"), "当前格应有 cur 类");
    assert(grid.selectedSet.size === 1, "单选选中数应为 1");
  });

  check("Shift 块选矩形", async () => {
    grid.select([0, 0]);
    grid._extendTo([2, 1]);
    assert(grid.selectedSet.size === 6, `3x2 块选应为 6，实际 ${grid.selectedSet.size}`);
  });

  check("Ctrl 追加多块", async () => {
    grid.clearSelection();
    clickCell(grid, 0, 0);
    grid.select([2, 2], { add: true });
    assert(grid.selectedSet.size === 2, "两块单格应为 2");
    assert(grid.sel.blocks.length === 2, "应为 2 个块");
  });

  check("行头点击选整行", async () => {
    loadBasic();
    grid.selectRow(1);
    assert(grid.selectedSet.size === 3, `整行应选中 3 格，实际 ${grid.selectedSet.size}`);
  });

  check("列头点击选整列", async () => {
    grid.selectCol(2);
    assert(grid.selectedSet.size === 3, `整列应选中 3 格，实际 ${grid.selectedSet.size}`);
  });

  check("Esc 清空选区但保留当前格（对齐 Qt）", () => {
    loadBasic();
    grid.selectRow(1);
    assert(grid.selectedSet.size === 3, `选中整行应 3 格，实际 ${grid.selectedSet.size}`);
    key(document.getElementById("grid-scroll"), "Escape");
    assert(grid.selectedSet.size === 0 && grid.sel.blocks.length === 0,
      "Esc 后选区应为空");
    assert(cur(grid)?.[0] === 1, "Esc 后当前格应保留（对齐 Qt 表格 Escape 行为）");
  });

  check("筛选框聚焦打字不被劫持为格内编辑", () => {
    loadBasic();
    grid.select([0, 0]);
    const input = document.querySelector("#col-header .col-filter input");
    assert(input, "表头应有筛选输入框");
    input.focus();
    key(input, "x");
    assert(!grid.editing, "筛选框打字不应打开格内编辑器");
    assert(cur(grid)?.[0] === 0 && cur(grid)?.[1] === 0, "当前格不应被改动");
  });

  check("方向键移动当前格", async () => {
    grid.select([0, 0]);
    const scroller = document.getElementById("grid-scroll");
    scroller.focus();
    key(scroller, "ArrowDown");
    assert(cur(grid)?.[0] === 1, "下移后行号应为 1");
    key(scroller, "ArrowRight");
    assert(cur(grid)?.[1] === 1, "右移后列号应为 1");
  });

  check("跳转行：直达选中并滚入视野，越界夹取", async () => {
    grid.clearFilters();
    model.load(headers, Array.from({ length: 8000 }, (_, i) => [`R${i}`, String(i), ""]));
    grid.renderAll();
    assert(window.__app.gotoRow(3999) === true, "跳转应成功");
    const r = cur(grid)?.[0];
    assert(r != null && grid.visibleIndexOf(r) === 3999, "当前格应位于显示行 4000");
    grid._renderRows();
    const el = document.querySelector(`.cell[data-row="${r}"][data-col="0"]`);
    assert(el, "目标行应渲染在视口内（ensureVisible 生效）");
    assert(window.__app.gotoRow(99999999) === true, "越界输入应夹取后仍跳转");
    assert(cur(grid)?.[0] === grid.visAt(grid.visCount() - 1), "应夹取到末行");
    window.__app.gotoRow(0);
  });

  check("Tab 右移 / Shift+Tab 左移", async () => {
    grid.select([1, 1]);
    const scroller = document.getElementById("grid-scroll");
    key(scroller, "Tab");
    assert(cur(grid)?.[1] === 2, "Tab 后列号应为 2");
    key(scroller, "Tab", { shiftKey: true });
    assert(cur(grid)?.[1] === 1, "Shift+Tab 后列号应为 1");
  });

  // ========== 格内编辑 ==========

  check("F2 打开编辑器且全选；Esc 取消不写回", async () => {
    loadBasic();
    grid.select([0, 0]);
    const scroller = document.getElementById("grid-scroll");
    key(scroller, "F2");
    assert(grid.editing, "F2 应打开编辑器");
    const ed = document.getElementById("cell-editor");
    assert(ed.value === "大理", "编辑器应装入原文");
    key(ed, "Escape");
    assert(!grid.editing, "Esc 应关闭编辑器");
    assert(model.rawData(0, 0) === "大理", "Esc 不应写回");
  });

  check("打字即编辑：替换式预填字符", async () => {
    grid.select([0, 0]);
    const scroller = document.getElementById("grid-scroll");
    key(scroller, "Z");
    const ed = document.getElementById("cell-editor");
    assert(grid.editing && ed.value === "Z", "应打开编辑器并预填 Z");
    key(ed, "Escape");
  });

  check("Enter 提交并下移", async () => {
    grid.select([0, 0]);
    grid.openEditor(0, 0, { selectAll: true });
    const ed = document.getElementById("cell-editor");
    ed.value = "昆明";
    key(ed, "Enter");
    assert(model.rawData(0, 0) === "昆明", "Enter 应提交");
    assert(cur(grid)?.[0] === 1, "提交后应下移");
  });

  check("Shift+Enter 提交并上移", async () => {
    grid.select([1, 0]);
    grid.openEditor(1, 0, { selectAll: true });
    const ed = document.getElementById("cell-editor");
    ed.value = "曲靖";
    key(ed, "Enter", { shiftKey: true });
    assert(model.rawData(1, 0) === "曲靖");
    assert(cur(grid)?.[0] === 0, "应上移");
  });

  check("Tab 提交并右移 / Shift+Tab 左移", async () => {
    grid.select([0, 1]);
    grid.openEditor(0, 1, { selectAll: true });
    const ed = document.getElementById("cell-editor");
    ed.value = "99";
    key(ed, "Tab");
    assert(model.rawData(0, 1) === "99", "Tab 应提交");
    assert(cur(grid)?.[1] === 2, "应右移");
    grid.openEditor(0, 2, { selectAll: true });
    key(document.getElementById("cell-editor"), "Tab", { shiftKey: true });
    assert(cur(grid)?.[1] === 1, "应左移");
  });

  check("Alt+Enter 插入换行（提交后保留多行）", async () => {
    grid.select([0, 2]);
    grid.openEditor(0, 2, { selectAll: true });
    const ed = document.getElementById("cell-editor");
    ed.value = "第一行";
    // Alt+Enter：编辑器不拦截，浏览器向光标处插换行 → 模拟 input 结果
    ed.value = "第一行\n第二行";
    key(ed, "Enter", { altKey: true });
    // altKey 时 grid 不拦截不提交，仍处编辑态
    assert(grid.editing, "Alt+Enter 不应提交");
    key(ed, "Enter");
    assert(model.rawData(0, 2).includes("\n"), "提交后应保留换行");
  });

  check("多行内容编辑器加高且不改行高", async () => {
    const rowHBefore = grid.rowH[2];
    grid.openEditor(2, 0, { selectAll: true });
    const ed = document.getElementById("cell-editor");
    ed.value = "a\nb\nc";
    grid._growEditor();
    assert(parseFloat(ed.style.height) > grid.rowH[2], "编辑器应加高");
    key(ed, "Escape");
    assert(grid.rowH[2] === rowHBefore, "行高不应变");
  });

  // ========== 筛选 ==========

  check("筛选：子串忽略大小写 + 状态栏计数", async () => {
    loadBasic();
    grid.filters.set(0, "洱");
    grid._refilter();
    assert(grid.visCount() === 1, `可见行应为 1，实际 ${grid.visCount()}`);
    assert(document.getElementById("status-text").textContent.includes("显示 1 / 共 3 行"),
      "状态栏应显示 显示 1 / 共 3 行");
  });

  check("筛选输入框与列同宽对齐（结构断言）", async () => {
    const cell = document.querySelector(".col-head-cell");
    const input = cell.querySelector(".col-filter input");
    assert(cell && input, "筛选输入框应在列头单元格内（共用网格坐标系）");
  });

  check("列插入后筛选条件清空", async () => {
    grid.filters.set(0, "洱");
    model.insertColumns(0, 1);
    grid.clearFilters();
    assert(grid.filters.size === 0, "筛选应已清空");
    assert(grid.visCount() === 3, "清空后应显示全部行");
  });

  check("多列筛选为 AND 关系", async () => {
    loadBasic();
    grid.filters.set(1, "1");
    grid.filters.set(0, "丽");
    grid._refilter();
    assert(grid.visCount() === 1, "AND 后应只剩丽江");
    assert(model.rawData(grid.visAt(0), 0) === "丽江");
  });

  // ========== 多重筛选 ==========

  check("多重筛选：按钮在清除筛选右侧，开启出现输入框与默认勾选的列复选框", async () => {
    loadBasic();
    const names = [...document.querySelectorAll("#toolbar button")].map((b) => b.textContent);
    const iClear = names.indexOf("清除筛选");
    const iMulti = names.indexOf("多重筛选");
    assert(iClear >= 0 && iMulti === iClear + 1, "多重筛选应紧邻清除筛选右侧");
    const btn = [...document.querySelectorAll("#toolbar button")].find((b) => b.textContent === "多重筛选");
    assert(btn && !btn.classList.contains("on"), "初始应为未选中态");
    assert(document.getElementById("multi-filter-wrap").classList.contains("hidden"),
      "未选中时输入框应隐藏");
    btn.click();
    assert(btn.classList.contains("on"), "点击后应呈选中态（可长期选中）");
    assert(!document.getElementById("multi-filter-wrap").classList.contains("hidden"),
      "选中态应显示输入框");
    const cbs = [...document.querySelectorAll(".col-multi")];
    assert(cbs.length === 3 && cbs.every((cb) => cb.checked), "每个列名前应有默认勾选的复选框");
  });

  check("多重筛选：勾选列按或逻辑匹配，取消勾选即剔除该列", async () => {
    model.setData(2, 0, "观山"); // 洱海→观山：与丽江"雪山"分处两列
    grid.setMultiText("山");
    assert(grid.visCount() === 2, "或逻辑应命中两列各一行");
    const cb0 = document.querySelector('.col-multi[data-col="0"]');
    assert(cb0 && cb0.checked, "列0复选框应存在且勾选");
    cb0.checked = false;
    cb0.dispatchEvent(new Event("change", { bubbles: true }));
    assert(grid.visCount() === 1, "取消列0勾选后只应剩备注列命中行");
    assert(model.rawData(grid.visAt(0), 0) === "丽江");
  });

  check("多重筛选结果可再被列筛选框叠加过滤", async () => {
    grid.filters.set(1, "10");
    grid._refilter();
    assert(grid.visCount() === 1, "多重结果应再按列筛选二次过滤");
    assert(model.rawData(grid.visAt(0), 0) === "丽江");
  });

  check("多重筛选关闭后恢复：输入框隐藏、复选框移除", async () => {
    grid.clearFilters(); // 清掉上一步的列筛选
    const btn = [...document.querySelectorAll("#toolbar button")].find((b) => b.textContent === "多重筛选");
    btn.click(); // 关
    assert(!btn.classList.contains("on"), "再次点击应回到未选中态");
    assert(document.getElementById("multi-filter-wrap").classList.contains("hidden"),
      "关闭应隐藏输入框");
    assert(document.querySelectorAll(".col-multi").length === 0, "关闭应移除列名前复选框");
    assert(grid.visCount() === 3, "关闭后应显示全部行");
  });

  check("清除筛选连带清空多重筛选关键字但保留开关", async () => {
    const btn = [...document.querySelectorAll("#toolbar button")].find((b) => b.textContent === "多重筛选");
    btn.click(); // 开
    grid.setMultiText("山");
    grid.filters.set(1, "10");
    grid.clearFilters();
    assert(grid.multi.text === "" && grid.filters.size === 0, "清除筛选应清空全部条件");
    assert(grid.visCount() === 3, "清除后应显示全部行");
    assert(grid.multi.on, "开关状态应保留");
    btn.click(); // 关，恢复默认状态供后续用例
    grid.clearFilters();
  });

  // ========== 编辑窗格 ==========

  check("选中变化同步窗格完整内容", async () => {
    loadBasic();
    model.setData(0, 2, "line1\nline2");
    clickCell(grid, 0, 2);
    const pe = document.getElementById("pane-edit");
    assert(pe.value === "line1\nline2", "窗格应显示完整多行原文");
    assert(document.querySelector(".cell").textContent !== pe.value, "网格显示应被压平");
  });

  check("窗格编辑实时写回并合并为一步撤销", async () => {
    const undoBefore = model.canUndo();
    void undoBefore;
    const pe = document.getElementById("pane-edit");
    pe.value = "line1\nline2X";
    pe.dispatchEvent(new Event("input", { bubbles: true }));
    assert(model.rawData(0, 2) === "line1\nline2X", "写回模型失败");
    const steps = model._undoStack.length;
    pe.value = "line1\nline2XY";
    pe.dispatchEvent(new Event("input", { bubbles: true }));
    assert(model._undoStack.length === steps, "同格连续击键应合并为一步");
  });

  check("窗格 Tab 键入制表符且焦点不逃离（可 Ctrl+Z 撤销）", async () => {
    const pe = document.getElementById("pane-edit");
    pe.focus();
    pe.selectionStart = pe.selectionEnd = pe.value.length;
    const before = pe.value;
    key(pe, "Tab");
    assert(pe.value === before + "\t", "Tab 应作为字符输入窗格内容");
    assert(document.activeElement === pe, "焦点应留在窗格，不被 Tab 导航抢走");
    // Tab 走原生插入管线：撤销栈保留，Ctrl+Z 可撤销本次 Tab
    assert(document.queryCommandEnabled("undo"), "Tab 插入后原生撤销栈应有内容");
    document.execCommand("undo");
    assert(pe.value === before, "Ctrl+Z 应撤销 Tab 插入");
    document.execCommand("redo");
    assert(pe.value === before + "\t", "重做应恢复 Tab");
    document.execCommand("undo");
    assert(pe.value === before, "收尾再撤销，恢复检查前状态");
  });

  check("编辑窗格 Ctrl+Z 撤销输入（原生撤销栈不被回写打断）", async () => {
    loadBasic();
    const pe = document.getElementById("pane-edit");
    clickCell(grid, 0, 2);
    pe.focus();
    // execCommand 走原生输入管线（合成按键不触发默认动作），产生真实撤销步；
    // 同时断言输入事件全链路（onEdit→模型→网格重渲染）不写回编辑域 value，
    // 因为程序化赋值会清空 Chromium 文本域的原生撤销栈
    const proto = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value");
    let spyStack = null;
    Object.defineProperty(pe, "value", {
      get: proto.get,
      set(v) { spyStack = new Error("程序化写入 pane-edit.value").stack; proto.set.call(this, v); },
      configurable: true,
    });
    let inserted = false;
    try {
      inserted = document.execCommand("insertText", false, "ab");
    } finally {
      delete pe.value; // 移除间谍，恢复原型行为
    }
    assert(inserted && pe.value.endsWith("ab"), "原生插入应生效并到达窗格");
    assert(spyStack === null, `输入链路不应写回 value（会清空撤销栈）：\n${spyStack ?? ""}`);
    assert(document.queryCommandEnabled("undo"), "原生撤销栈应有内容");
    document.execCommand("undo");
    assert(!pe.value.endsWith("ab"), "Ctrl+Z 应撤销输入");
    document.execCommand("redo");
    assert(pe.value.endsWith("ab"), "重做应恢复输入");
  });

  check("显示符号/工具栏按钮点击不抢走窗格焦点（Ctrl+Z 路由保持）", async () => {
    const pe = document.getElementById("pane-edit");
    pe.focus();
    const head = document.getElementById("pane-head");
    const sym = head.querySelector("button");
    sym.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    sym.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
    sym.click();
    assert(document.activeElement === pe,
      "点击显示符号后焦点应留在窗格，Ctrl+Z 仍走原生撤销");
    sym.click(); // 还原符号模式
    const multi = [...document.querySelectorAll("#toolbar button")]
      .find((b) => b.textContent === "多重筛选");
    multi.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    assert(document.activeElement === pe, "按下工具栏按钮也不应抢焦点");
    multi.click(); // 开（会显式聚焦输入框）
    assert(document.activeElement?.id === "multi-filter-input",
      `多重筛选开启应显式聚焦输入框（实际 ${document.activeElement?.id ?? document.activeElement?.tagName}）`);
    multi.click(); // 关
  });

  check("显示符号：镜像层显示 · 与 ¶，编辑域保持原文", async () => {
    pane.setText("a b\nc");
    pane.setSymbols(true);
    const mirror = document.getElementById("pane-mirror");
    assert(mirror.textContent.includes("·") || mirror.textContent.includes("¶"),
      "镜像层应包含符号");
    assert(document.getElementById("pane-edit").value === "a b\nc",
      "编辑域应保留原文（含空格与换行）");
    pane.setSymbols(false);
    pane.clear();
  });

  // ========== 工具栏/状态栏/标题 ==========

  check("工具栏按钮齐全且撤销/重做随栈置灰", async () => {
    const texts = [...document.querySelectorAll("#toolbar button")].map((b) => b.textContent);
    for (const t of ["新建", "打开", "保存", "另存为", "撤销", "重做", "排序…", "清除筛选", "多重筛选"]) {
      assert(texts.includes(t), `工具栏缺少 ${t}`);
    }
    loadBasic(); // load 清空撤销栈
    assert(document.querySelector("#toolbar button").parentElement, "工具栏存在");
    const [undoB, redoB] = [document.querySelectorAll("#toolbar button")[4],
      document.querySelectorAll("#toolbar button")[5]];
    assert(undoB.disabled && redoB.disabled, "load 后撤销/重做应置灰");
    model.setData(0, 0, "x");
    assert(!undoB.disabled, "改动后撤销应可用");
  });

  check("显示符号按钮位于编辑窗格标题右侧（工具栏不再有）", async () => {
    const head = document.getElementById("pane-head");
    assert(head, "窗格标题行存在");
    const btns = [...head.querySelectorAll("button")];
    assert(btns.length === 1 && btns[0].textContent === "显示符号",
      "编辑窗格标题右侧应有且仅有显示符号按钮");
    assert(![...document.querySelectorAll("#toolbar button")].some((b) => b.textContent === "显示符号"),
      "工具栏不应再含显示符号");
    const before = document.getElementById("pane").classList.contains("symbols");
    btns[0].click();
    assert(document.getElementById("pane").classList.contains("symbols") === !before,
      "点击应切换符号模式");
    btns[0].click();
    assert(document.getElementById("pane").classList.contains("symbols") === before,
      "再次点击应还原");
  });

  check("格内编辑后标题带脏标记星号", async () => {
    // 标题经 set_title 发到 Rust，前端以 __app.isDirty 断言同等语义
    assert(window.__app.isDirty() === true, "编辑后应为脏");
    loadBasic();
    assert(window.__app.isDirty() === false, "load 后应为干净");
  });

  // ========== 剪贴板 ==========

  check("复制选区为 TSV（含引号规则）", async () => {
    loadBasic();
    grid.clearSelection();
    grid.select([0, 0]);
    grid._extendTo([1, 1]);
    grid.copySelection();
    const expect = '大理\t3\r\n丽江\t10\r\n';
    assert(grid.lastCopiedText === expect, `复制内容: ${JSON.stringify(grid.lastCopiedText)}`);
  });

  check("含 Tab/换行的单元格复制时加引号", async () => {
    model.setData(0, 0, 'a\tb\nc');
    grid.select([0, 0]);
    grid.copySelection();
    assert(grid.lastCopiedText === '"a\tb\nc"\r\n', `引号包裹: ${JSON.stringify(grid.lastCopiedText)}`);
  });

  check("粘贴区域展开 + 超范围自动扩列 + 整块一步撤销", async () => {
    loadBasic();
    grid.select([0, 0]);
    const before = model._undoStack.length;
    grid.lastCopiedText = "A\tB\r\nC\tD\r\n";
    // 走内部解析：绕过系统剪贴板，直接调用核心逻辑
    pasteRegion(grid, model, 0, 0, "A\tB\r\nC\tD\r\n");
    assert(model.rawData(0, 0) === "A" && model.rawData(1, 1) === "D", "区域粘贴内容错");
    assert(model._undoStack.length === before + 1, "整块应为一步撤销");
    model.undo();
    assert(model.rawData(0, 0) === "大理", "撤销后应复原");
  });

  check("粘贴自动扩展行列", async () => {
    model.load(["a"], [["1"], ["2"]]);
    grid.renderAll();
    pasteRegion(grid, model, 1, 0, "x\ty\r\nz\tw\r\n");
    assert(model.columnCount() === 2, "应自动扩到 2 列");
    assert(model.rowCount() === 3, "应自动扩到 3 行");
    assert(model.rawData(2, 1) === "w");
  });

  check("无 Tab 纯文本整段进当前格（不换行摊开）", async () => {
    model.load(["a"], [[""], [""]]);
    grid.renderAll();
    pastePlain(model, 0, 0, "第一段\n第二段");
    assert(model.rawData(0, 0) === "第一段\n第二段", "长文应整段进当前格");
    assert(model.rawData(1, 0) === "", "下一行不应被覆盖");
  });

  check("单格一对多填充：选区>1 且剪贴板为单值 → 全选区填充（Excel）", async () => {
    loadBasic();
    grid.select([0, 0]);
    grid._extendTo([1, 1]); // 2×2 选区
    stubClipboard("填充值\r\n"); // 内部复制的单格格式（带行尾）
    await grid.pasteClipboard();
    assert(model.rawData(0, 0) === "填充值" && model.rawData(0, 1) === "填充值"
      && model.rawData(1, 0) === "填充值" && model.rawData(1, 1) === "填充值",
      "选区每个格都应填充为剪贴板单值");
    assert(model.rawData(2, 0) !== "填充值", "选区外的行不应被改");
  });

  check("单格填充合并为一步撤销", async () => {
    loadBasic();
    grid.select([0, 0]);
    grid._extendTo([1, 1]);
    const before = model._undoStack.length;
    stubClipboard("X");
    await grid.pasteClipboard();
    assert(model._undoStack.length === before + 1, "整块填充应为一步撤销");
    model.undo();
    assert(model.rawData(0, 0) === "大理" && model.rawData(1, 1) === "10",
      "撤销后选区应整体复原");
  });

  check("多行文本不触发填充（保持整段进当前格语义）", async () => {
    model.load(["a"], [["m1"], ["m2"]]);
    grid.renderAll();
    grid.select([0, 0]);
    grid._extendTo([1, 0]); // 当前格=扩展终点 [1,0]
    stubClipboard("第一段\n第二段"); // 外部多行纯文本
    await grid.pasteClipboard();
    // 整段进"当前格"（选区扩展终点，对齐 Excel 活动格语义）
    assert(model.rawData(1, 0) === "第一段\n第二段", "多行文本应整段进当前格");
    assert(model.rawData(0, 0) === "m1", "多行文本不应填充选区");
  });

  // ========== 多文件页签 ==========

  /** 写 UTF-8 文本夹具（页签测试用） */
  async function writeText(p, text) {
    await invoke("smoke_write_bytes", { path: p, bytes: [...new TextEncoder().encode(text)] });
  }

  /** 弹出的"未保存"三选框里点指定按钮（无弹窗则什么都不做） */
  function clickDiscardDialog(text) {
    const mask = document.querySelector(".modal-mask");
    if (!mask || !mask.textContent.includes("未保存")) return false;
    [...mask.querySelectorAll("button")].find((b) => b.textContent === text)?.click();
    return true;
  }

  check("页签：多文件拖入出标签条，全部入签，激活首个", async () => {
    const f1 = `${TMP}/tabm/a.csv`; const f2 = `${TMP}/tabm/b.csv`; const f3 = `${TMP}/tabm/c.csv`;
    await writeText(f1, "目的地,评分\r\n甲,1\r\n");
    await writeText(f2, "目的地,评分\r\n乙,2\r\n");
    await writeText(f3, "目的地,评分\r\n丙,3\r\n");
    const before = window.__app.tabs().length;
    // 前置档是脏的（上一用例打开带警告 CSV 后未保存）：
    // 首个文件装入前先弹 保存/放弃/取消——点"放弃"放行
    const pendingOpen = window.__app.openPathsAsTabs([f1, f2, f3]);
    await sleep(100);
    assert(clickDiscardDialog("放弃") === true, "脏档拖入应先弹未保存确认");
    const opened = await pendingOpen;
    const t = window.__app.tabs();
    assert(opened === true, "openPathsAsTabs 应返回成功");
    // 3 文件入签；此前无签（before=0）时装当前档+ensureTabbed 出首签
    assert(t.length === before + 3, `页签数 ${t.length}`);
    const bar = document.getElementById("tabbar");
    assert(getComputedStyle(bar).display !== "none", "多档时标签条应显示");
    assert(t.find((x) => x.path === f1)?.active === true, "应激活首个拖入文件");
    assert(t.some((x) => x.path === f2) && t.some((x) => x.path === f3), "其余文件应各有一签");
    assert(model.rawData(0, 0) === "甲", "激活档内容应为首个文件");
  });

  check("页签：切换保留数据/脏态/撤销，互不串档", async () => {
    const ta = window.__app.tabs().find((x) => x.path?.endsWith("tabm/a.csv"));
    const tb = window.__app.tabs().find((x) => x.path?.endsWith("tabm/b.csv"));
    await window.__app.activateTab(ta.id);
    model.setData(0, 0, "改过");
    assert(window.__app.isDirty() === true, "编辑后应标脏");
    await window.__app.activateTab(tb.id);
    assert(model.rawData(0, 0) === "乙", "b 档内容");
    assert(window.__app.isDirty() === false, "b 档未改过应不脏");
    await window.__app.activateTab(ta.id);
    assert(model.rawData(0, 0) === "改过", "a 档修改应随签保留");
    assert(window.__app.isDirty() === true, "a 档脏态应随签保留");
    model.undo();
    assert(model.rawData(0, 0) === "甲", "a 档撤销栈应保留");
  });

  check("页签：重复路径激活已有，不重复开签", async () => {
    const n = window.__app.tabs().length;
    const f2 = window.__app.tabs().find((x) => x.path?.endsWith("tabm/b.csv"));
    await window.__app.openPathsAsTabs([f2.path]);
    const t = window.__app.tabs();
    const diag2 = `n=${n} f2=${f2.path} active=${t.find((x) => x.active)?.path || "?"} ` +
      `paths=[${t.map((x) => x.path || "?").join(", ")}] ` +
      `status="${document.getElementById("status-text")?.textContent || "?"}"`;
    assert(t.length === n, `重复拖入不应新增页签 —— ${diag2}`);
    // openPathsAsTabs 语义：列表首个文件激活（重复文件=激活已有签）
    assert(t.find((x) => x.path === f2.path)?.active === true,
      `应激活已有页签 —— ${diag2}`);
  });

  check("页签：关闭脏签弹三选确认，放弃后关签并激活相邻", async () => {
    const a = window.__app.tabs().find((x) => x.path?.endsWith("tabm/a.csv"));
    await window.__app.activateTab(a.id);
    model.setData(0, 0, "又要关"); // 弄脏
    const n = window.__app.tabs().length;
    const p = window.__app.closeTab(a.id); // 不 await：确认框须在同步阶段挂上
    assert(clickDiscardDialog("放弃") === true, "关脏签应弹未保存确认");
    await p;
    const t = window.__app.tabs();
    assert(t.length === n - 1, "放弃后页签应被移除");
    assert(!t.some((x) => x.id === a.id), "被关页签不应再出现");
    const act = t.find((x) => x.active);
    assert(act && act.id !== a.id, "应自动激活相邻页签");
    assert(act.path?.endsWith("tabm/b.csv") === true, "应优先激活右侧相邻签");
  });

  check("页签：关到只剩一档标签条隐藏，全关回空表", async () => {
    for (;;) {
      const t = window.__app.tabs();
      if (!t.length) break;
      const cur = t.find((x) => x.active) ?? t[0];
      const p = window.__app.closeTab(cur.id);
      clickDiscardDialog("放弃");
      await p;
    }
    assert(window.__app.tabs().length === 0, "页签应全部清空");
    assert(getComputedStyle(document.getElementById("tabbar")).display === "none",
      "档数<2 时标签条应隐藏");
    assert(model.rowCount() === 0 && model.columnCount() === 0, "全关后应回空表");
  });

  check("页签：标签超出宽度出滚动按钮且可滚动", async () => {
    const names = [];
    for (let i = 0; i < 24; i += 1) names.push(`${TMP}/tabm/many_${i}.csv`);
    for (let i = 0; i < 24; i += 1) await writeText(names[i], `目的地\r\n行${i}\r\n`);
    await window.__app.openPathsAsTabs(names);
    const bar = document.getElementById("tabbar");
    const scroll = document.getElementById("tab-scroll");
    const next = document.getElementById("tab-next");
    assert(bar.classList.contains("overflow") || scroll.scrollWidth > scroll.clientWidth,
      "页签超出宽度应进入溢出态");
    assert(next.offsetWidth > 0, "溢出时应显示滚动按钮");
    const before = scroll.scrollLeft;
    next.click();
    assert(scroll.scrollLeft > before, "点滚动按钮应滚动页签条");
  });

  // ========== 分屏视图 ==========

  /** 页签元素（标签条内按文件名匹配） */
  const tabEl = (name) => [...document.querySelectorAll("#tab-scroll .tab")]
    .find((x) => x.textContent.includes(name));

  /** 侧侧首格文本（side: "A"=主网格 "#grid-area"，其余 = 副网格 "#grid-area2"） */
  function sideCellText(side, r, c) {
    const root = side === "A" ? "#grid-area" : "#grid-area2";
    return document.querySelector(`${root} .cell[data-row="${r}"][data-col="${c}"]`)
      ?.textContent;
  }

  check("分屏：右键页签「在分屏中打开」——双网格各显其档", async () => {
    const fa = `${TMP}/sv/a.csv`; const fb = `${TMP}/sv/b.csv`;
    await writeText(fa, "目的地\r\n甲A\r\n");
    await writeText(fb, "目的地\r\n乙B\r\n");
    await window.__app.openPathsAsTabs([fa, fb]); // 激活 a
    const ta = window.__app.tabs().find((x) => x.path === fa);
    const tb = window.__app.tabs().find((x) => x.path === fb);
    const el = tabEl("b.csv");
    assert(el, "应找到 b 签元素");
    el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    const item = [...document.querySelectorAll(".ctx-menu .mi")]
      .find((x) => x.textContent === "在分屏中打开");
    assert(item && !item.classList.contains("disabled"), "右键菜单应有可用的在分屏中打开");
    item.click();
    await sleep(80); // openSplit 为 async：setModel 绑定在微任务里落定
    const sp = window.__app.split();
    assert(sp.on === true && sp.splitTabId === tb.id && sp.focused === "A",
      `分屏态应就绪 ${JSON.stringify(sp)}`);
    assert(getComputedStyle(document.getElementById("grid-area2")).display !== "none",
      "副网格应可见");
    assert(document.getElementById("grid-area2").offsetWidth > 0, "副网格应有实际宽度");
    assert(window.__app.tabs().find((x) => x.active)?.id === ta.id, "活动签应保持 a（焦点未动）");
    assert(sideCellText("A", 0, 0) === "甲A", "主侧应显示活动签 a");
    assert(sideCellText("B", 0, 0) === "乙B", "副侧应显示分屏签 b");
  });

  check("分屏：活动签可直接入分屏——主侧自动切显邻签", async () => {
    // 右键点【活动签】也应能分屏（VS Code 行为）：活动签移到副侧，主侧换显右邻签
    await window.__app.closeSplit();
    const fa = `${TMP}/sv/a.csv`; const fb = `${TMP}/sv/b.csv`;
    const ta = window.__app.tabs().find((x) => x.path === fa);
    const tb = window.__app.tabs().find((x) => x.path === fb);
    assert(window.__app.tabs().find((x) => x.active)?.id === ta.id, "活动签应是 a");
    const el = tabEl("a.csv");
    assert(el, "应找到 a 签元素");
    el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    const item = [...document.querySelectorAll(".ctx-menu .mi")]
      .find((x) => x.textContent === "在分屏中打开");
    assert(item && !item.classList.contains("disabled"), "活动签的「在分屏中打开」不应禁用");
    item.click();
    await sleep(80);
    const sp = window.__app.split();
    assert(sp.on === true && sp.splitTabId === ta.id && sp.focused === "A",
      `活动签入分屏态应就绪 ${JSON.stringify(sp)}`);
    assert(window.__app.tabs().find((x) => x.active)?.id === tb.id, "主侧应换显邻签 b");
    assert(sideCellText("B", 0, 0) === "甲A", "副侧应显示原活动签 a");
    assert(sideCellText("A", 0, 0) === "乙B", "主侧应显示邻签 b");
    await window.__app.closeSplit();
    await window.__app.activateTab(ta.id); // 还原：焦点回 a（单视图）
  });

  check("分屏：点副侧焦点交换——内容稳定，编辑落副签", async () => {
    const fa = `${TMP}/sv/a.csv`; const fb = `${TMP}/sv/b.csv`;
    const ta = window.__app.tabs().find((x) => x.path === fa);
    const tb = window.__app.tabs().find((x) => x.path === fb);
    // 自建分屏（a 主 b 副），不依赖前项的分屏残留态
    await window.__app.activateTab(ta.id);
    await window.__app.openSplit(tb.id);
    assert(window.__app.split().on === true, "分屏应建立");
    // 点副侧首格：mousedown 先激活该侧，再处理选中
    const cell = document.querySelector('#grid-area2 .cell[data-row="0"][data-col="0"]');
    assert(cell, "副侧首格应在 DOM");
    mouse(cell, "mousedown");
    mouse(document, "mouseup");
    await sleep(150); // 激活含同步写回与重绑
    const sp = window.__app.split();
    assert(sp.focused === "B", `焦点应切到副侧 ${JSON.stringify(sp)}`);
    assert(window.__app.tabs().find((x) => x.active)?.id === tb.id, "活动签应变 b");
    assert(sideCellText("A", 0, 0) === "甲A" && sideCellText("B", 0, 0) === "乙B",
      "焦点交换后两侧内容应稳定不互换");
    // 打字编辑：应落在 b 签（现焦点侧）
    const scroller = document.getElementById("grid-scroll2");
    key(scroller, "Z");
    const ed = document.getElementById("cell-editor2");
    assert(ed && ed.value === "Z", "副侧应打开编辑器预填 Z");
    key(ed, "Enter");
    await sleep(50);
    const t = window.__app.tabs();
    assert(t.find((x) => x.id === tb.id)?.dirty === true, "编辑应弄脏 b 签");
    assert(t.find((x) => x.id === ta.id)?.dirty === false, "a 签不应脏");
    // 还原：撤销编辑 + 焦点回 A，避免影响后续（经外壳 undo 包装：撤销不改
    // 文档的脏状态——onReset 会清脏，包装末尾重新标脏，这是既定语义）
    await window.__app.activateTab(ta.id);
    assert(window.__app.split().focused === "A", "焦点应可切回主侧");
    await window.__app.activateTab(tb.id);
    await window.__app.undo();
    assert(model.rawData(0, 0) === "乙B", "撤销后 b 签首格应复原");
    assert(window.__app.tabs().find((x) => x.id === tb.id)?.dirty === true,
      "撤销后 b 签应保持脏（供关签确认场景使用）");
    await window.__app.activateTab(ta.id);
  });

  check("分屏：拒绝双大档；大档签可作副侧", async () => {
    // 前两项（打开/焦点交换）把 a|b 分屏留在开着的状态；本项从单视图测起
    await window.__app.closeSplit();
    const b1 = `${TMP}/sv/big1.csv`; const b2 = `${TMP}/sv/big2.csv`;
    await invoke("smoke_set_force_big", { v: true });
    for (const [p, name] of [[b1, "大甲"], [b2, "大乙"]]) {
      await invoke("smoke_write_repeat", { path: p, line: "目的地\r\n", times: 1, append: false });
      await invoke("smoke_write_repeat", { path: p, line: `${name},1\r\n`, times: 800, append: true });
    }
    // 活动签 = big1（首个）；右键 big2 → 双大档：项可点，动作层拒绝并状态栏提示原因
    await window.__app.openPathsAsTabs([b1, b2]);
    const el = tabEl("big2.csv");
    el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    const item = [...document.querySelectorAll(".ctx-menu .mi")]
      .find((x) => x.textContent === "在分屏中打开");
    assert(item && !item.classList.contains("disabled"), "双大档项可点击（由动作层拒绝）");
    item.click(); // 拒绝路径同步执行：状态栏提示立即生效（大档取页落定会随后改写状态栏）
    assert(document.getElementById("status-text").textContent === "分屏不支持同时打开两个大文件",
      "应状态栏提示拒绝原因");
    await sleep(80);
    assert(window.__app.split().on === false, "双大档点击后不应建立分屏");
    // 切到小签 a 激活，再右键 big2 → 大小可分
    const fa = `${TMP}/sv/a.csv`;
    await window.__app.activateTab(window.__app.tabs().find((x) => x.path === fa).id);
    const el2 = tabEl("big2.csv");
    el2.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    const item2 = [...document.querySelectorAll(".ctx-menu .mi")]
      .find((x) => x.textContent === "在分屏中打开");
    assert(item2 && !item2.classList.contains("disabled"), "大小档应允许分屏");
    item2.click();
    await sleep(150); // 大档副侧要经历引擎腾退重开（IPC 往返）
    const sp = window.__app.split();
    assert(sp.on === true && sp.focused === "A", "分屏应建立且焦点在主侧小档");
    assert(window.__app.isBigMode() === false, "焦点侧是小档");
    // 副侧大档首行异步落定后应可断言
    await waitFor(() => sideCellText("B", 0, 0) === "大乙", "副侧大档首行落定");
    await invoke("smoke_set_force_big", { v: false });
    // 清理：关分屏，关掉两个大签
    await window.__app.closeSplit();
    const t = window.__app.tabs();
    for (const p of [b1, b2]) {
      const tab = t.find((x) => x.path === p);
      if (tab) await window.__app.closeTab(tab.id); // 无脏，不弹窗
    }
    await window.__app.activateTab(window.__app.tabs().find((x) => x.path === fa).id);
  });

  check("分屏：关闭分屏与关签联动", async () => {
    const fa = `${TMP}/sv/a.csv`; const fb = `${TMP}/sv/b.csv`;
    await window.__app.activateTab(window.__app.tabs().find((x) => x.path === fa).id);
    const tb = window.__app.tabs().find((x) => x.path === fb);
    await window.__app.openSplit(tb.id);
    assert(window.__app.split().on === true, "分屏应建立");
    // 右键分屏签 → 关闭分屏
    const el = tabEl("b.csv");
    el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    const item = [...document.querySelectorAll(".ctx-menu .mi")]
      .find((x) => x.textContent === "关闭分屏");
    assert(item, "分屏态右键菜单应有关闭分屏");
    item.click();
    assert(window.__app.split().on === false, "关闭分屏后分屏态应消失");
    assert(getComputedStyle(document.getElementById("grid-area2")).display === "none",
      "副网格应隐藏");
    assert(window.__app.tabs().some((x) => x.id === tb.id), "关闭分屏不应关签");
    // 再建分屏后关掉分屏签（b 签仍脏）→ 弹三选，放弃后关签、分屏自动关
    await window.__app.openSplit(tb.id);
    const p = window.__app.closeTab(tb.id);
    await sleep(100);
    assert(clickDiscardDialog("放弃") === true, "关脏分屏签应弹未保存确认");
    await p;
    assert(window.__app.split().on === false, "关掉分屏签应自动关闭分屏");
    assert(getComputedStyle(document.getElementById("grid-area2")).display === "none",
      "副网格应隐藏");
  });

  check("Del 立即清除选区内容（无确认）", async () => {
    loadBasic();
    grid.select([0, 0]);
    grid._extendTo([0, 1]);
    grid.deleteSelection(false);
    assert(model.rawData(0, 0) === "" && model.rawData(0, 1) === "", "Del 应清除内容");
    assert(model.rowCount() === 3, "行结构应保留");
  });

  // ========== 撤销/排序/结构 ==========

  check("Ctrl+Z 撤销 / Ctrl+Y 重做（经网格键盘路径）", async () => {
    loadBasic();
    const scroller = document.getElementById("grid-scroll");
    scroller.focus();
    model.setData(0, 0, "x");
    key(scroller, "z", { ctrlKey: true });
    assert(model.rawData(0, 0) === "大理", "Ctrl+Z 应撤销");
    key(scroller, "y", { ctrlKey: true });
    assert(model.rawData(0, 0) === "x", "Ctrl+Y 应重做");
    key(scroller, "Z", { ctrlKey: true, shiftKey: true });
    assert(model.rawData(0, 0) === "x", "Ctrl+Shift+Z 应重做（对齐 Qt 双绑定）");
    key(scroller, "z", { ctrlKey: true });
    assert(model.rawData(0, 0) === "大理", "Ctrl+Z 应撤销");
  });

  check("物理排序：数字方式非数字恒排最后", async () => {
    model.load(["n"], [["3"], ["x"], ["1"], [""], ["2"]]);
    grid.renderAll();
    model.sortRows([[0, "numeric", true]]);
    assert(JSON.stringify(model.allRows().map((r) => r[0])) ===
      JSON.stringify(["1", "2", "3", "x", ""]), "数字升序错");
    model.sortRows([[0, "numeric", false]]);
    assert(JSON.stringify(model.allRows().map((r) => r[0])) ===
      JSON.stringify(["3", "2", "1", "x", ""]), "数字降序错");
  });

  check("多层级排序优先级", async () => {
    model.load(["a", "b"], [["x", "2"], ["y", "1"], ["x", "1"]]);
    grid.renderAll();
    model.sortRows([[0, "text", true], [1, "numeric", true]]);
    assert(JSON.stringify(model.allRows()) ===
      JSON.stringify([["x", "1"], ["x", "2"], ["y", "1"]]), "多层级错");
  });

  check("行列增删清与越界夹取", async () => {
    loadBasic();
    model.insertRows(99, 2);
    assert(model.rowCount() === 5, "插行夹取到末尾");
    model.removeRows([0, 99]);
    assert(model.rowCount() === 4, "删行忽略越界");
    model.removeColumns([0, 2]);
    assert(JSON.stringify(model.headers()) === JSON.stringify(["评分"]), "批量删列");
    model.undo();
    assert(model.headers().length === 3, "undo 恢复列");
  });

  // ========== 右键菜单 ==========

  check("单元格右键菜单文案对齐 Qt", async () => {
    loadBasic();
    grid.select([1, 1]);
    grid.ensureVisible(1, 1);
    grid._renderRows();
    const cell = document.querySelector('.cell[data-row="1"][data-col="1"]');
    cell.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    await sleep(30);
    const items = [...document.querySelectorAll(".ctx-menu .mi")].map((e) => e.textContent);
    const expect = ["按行拆分粘贴", "在上方插入 1 行", "在左侧插入 1 列",
      "删除选区跨越的 1 行…", "删除选区跨越的 1 列…", "清除选区 1 个单元格内容…"];
    assert(JSON.stringify(items) === JSON.stringify(expect), `菜单项 ${items}`);
    document.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    await sleep(30);
  });

  check("行头右键菜单文案对齐 Qt", async () => {
    grid.selectRow(1);
    const rh = document.querySelector('.row-head[data-row="1"]');
    rh.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    await sleep(30);
    const items = [...document.querySelectorAll(".ctx-menu .mi")].map((e) => e.textContent);
    assert(items[0] === "跳转到行…" && items[1] === "在上方插入 1 行" &&
      items[2] === "在末尾添加 N 行…" &&
      items[3] === "删除选中的 1 行…" && items[4] === "清除选中 1 行的内容…",
      `行菜单 ${items}`);
    document.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    await sleep(30);
  });

  check("列头右键菜单文案对齐 Qt", async () => {
    grid.selectCol(1);
    const cn = document.querySelectorAll(".col-name")[1];
    cn.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    await sleep(30);
    const items = [...document.querySelectorAll(".ctx-menu .mi")].map((e) => e.textContent);
    assert(items[0] === "在左侧插入 1 列" && items[1] === "删除选中的 1 列…" &&
      items[2] === "清除选中 1 列的内容…" && items[3] === "重命名列…",
      `列菜单 ${items}`);
    document.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    await sleep(30);
  });

  // ========== 几何操作 ==========

  check("列宽拖拽改变宽度（最小 24）", async () => {
    loadBasic();
    const before = grid.colW[0];
    const rz = document.querySelector('.col-resize[data-col="0"]');
    mouse(rz, "mousedown", { clientX: 100 });
    mouse(document, "mousemove", { clientX: 100 + 40 });
    mouse(document, "mouseup");
    assert(grid.colW[0] === before + 40, `列宽应 +40，实际 ${grid.colW[0] - before}`);
  });

  check("行高拖拽改变高度（最小 10）", async () => {
    grid.ensureVisible(0, 0); // 滚回顶部，确保第 0 行在可视区
    grid._renderRows();
    const before = grid.rowH[0];
    const rz = document.querySelector('.row-resize[data-row="0"]');
    mouse(rz, "mousedown", { clientY: 100 });
    mouse(document, "mousemove", { clientY: 130 });
    mouse(document, "mouseup");
    assert(grid.rowH[0] === before + 30, `行高应 +30，实际 ${grid.rowH[0] - before}`);
  });

  check("列拖动改变列顺序", async () => {
    const order = grid.colOrder.slice();
    grid._moveCol(order[0], 2);
    assert(grid.colOrder[2] === order[0], "列应移动到位置 2");
    grid._moveCol(order[0], 0); // 还原
  });

  check("缩放夹取 0.5–3.0 且列宽行高同步缩放", async () => {
    loadBasic();
    const w0 = grid.colW[0];
    const h0 = grid.rowH[0];
    grid.setZoom(2);
    assert(grid.colW[0] === Math.max(24, Math.round(w0 * 2)), "列宽应按比放大");
    assert(grid.rowH[0] === Math.max(10, Math.round(h0 * 2)), "行高应按比放大");
    grid.setZoom(0.3);
    assert(grid.zoom === 0.5, "应夹取到 0.5");
    grid.setZoom(9);
    assert(grid.zoom === 3, "应夹取到 3.0");
    grid.setZoom(1);
  });

  check("末列拉伸填满视口", async () => {
    loadBasic();
    grid.renderAll();
    const w = grid._effWidths();
    const sum = w.reduce((a, b) => a + b, 0) + grid._rowHeadW();
    assert(sum >= grid.scrollEl.clientWidth, "末列应拉伸填满视口宽度");
  });

  // ========== 文件对话框集成（信息框自动确认） ==========

  check("打开带警告的 CSV 弹出提示框（可确定关闭）", async () => {
    await invoke("write_csv_file", {
      path: `${TMP}/warn.csv`,
      text: "a,b\r\n1\r\n", // 短行 → 补空警告
    });
    const pending = window.__app.openPath(`${TMP}/warn.csv`);
    await sleep(150);
    const mask = document.querySelector(".modal-mask");
    assert(mask, "应弹出打开提示框");
    const btn = mask.querySelector(".modal-buttons button:last-child");
    btn.click();
    await pending; // 等 openPath 完成（提示框已关闭）
    assert(!document.querySelector(".modal-mask"), "点确定后提示框应关闭");
    assert(model.rawData(0, 1) === "", "短行应补空");
  });

  // ========== 辅助：绕过系统剪贴板的粘贴核心验证 ==========

  /**
   * 打桩 navigator.clipboard.readText（WebView 里读真实剪贴板需权限且
   * 污染系统剪贴板；defineProperty 覆盖实例属性，测完由后续用例自行覆盖）。
   *
   * @param {string} text - 模拟剪贴板文本。
   */
  function stubClipboard(text) {
    Object.defineProperty(navigator, "clipboard", {
      value: { readText: () => Promise.resolve(text), writeText: () => Promise.resolve() },
      configurable: true,
    });
  }

  /**
   * 直接走区域粘贴解析核心（模拟含 Tab 内部分区文本的粘贴效果）。
   *
   * @param {import("./grid.js").Grid} g - 网格。
   * @param {import("./model.js").TableModel} m - 模型。
   * @param {number} r0 - 起始行。
   * @param {number} c0 - 起始列。
   * @param {string} text - TSV 文本。
   */
  function pasteRegion(g, m, r0, c0, text) {
    // 与 grid.pasteClipboard 完全一致的解析+扩展+批量写入路径
    const p = parseCsv(text, "	");
    const rows2 = p.rows.length ? [p.headers, ...p.rows]
      : (p.headers.length ? [p.headers] : []);
    if (!rows2.length) return;
    const width = Math.max(...rows2.map((row) => row.length));
    m.batch(() => {
      const needRows = r0 + rows2.length - m.rowCount();
      if (needRows > 0) m.insertRows(m.rowCount(), needRows);
      const needCols = c0 + width - m.columnCount();
      if (needCols > 0) m.insertColumns(m.columnCount(), needCols);
      rows2.forEach((row, i) => {
        row.forEach((val, j) => m.setData(r0 + i, c0 + j, val));
      });
    });
    void g;
  }

  /**
   * 纯文本整段粘贴核心验证。
   *
   * @param {import("./model.js").TableModel} m - 模型。
   * @param {number} r - 行。 @param {number} c - 列。 @param {string} text - 文本。
   */
  function pastePlain(m, r, c, text) {
    m.setData(r, c, text.replace(/\r\n/g, "\n").replace(/[\r\n]+$/, ""));
  }

  check("显示符号：¶ 在行尾且换行保留（行结构不塌）", () => {
    const text = "a\nb\n\nc";
    pane.setText(text);
    pane.setSymbols(true);
    const mirror = document.getElementById("pane-mirror");
    assert(mirror.textContent === "a¶\nb¶\n¶\nc",
      `镜像应为 "a¶⏎b¶⏎¶⏎c"，实际 ${JSON.stringify(mirror.textContent)}`);
    assert(document.getElementById("pane-edit").value === text,
      "编辑层原文不应被符号污染");
    pane.setSymbols(false);
  });

  check("显示符号：Tab/空格符号占位与编辑域逐位对齐（镜像不串位）", () => {
    const text = "\ta b\tcd e";
    pane.setText(text);
    pane.setSymbols(true);
    const pe = document.getElementById("pane-edit");
    const mirror = document.getElementById("pane-mirror");
    // 探针：与编辑域同样式（同字体/内边距/换行规则/内容宽）的原文渲染参照，
    // 镜像渲染必须与其逐位一致，光标（编辑域排版）才对得上符号
    const probe = document.createElement("div");
    const cs = getComputedStyle(pe);
    probe.style.cssText = "position:absolute;left:-99999px;top:0;visibility:hidden;" +
      `font:${cs.font};padding:${cs.padding};white-space:${cs.whiteSpace};` +
      `word-break:${cs.wordBreak};line-height:${cs.lineHeight};` +
      `width:${pe.clientWidth}px;border:none;margin:0;`;
    probe.textContent = text;
    document.getElementById("pane-wrap").appendChild(probe);
    try {
      /**
       * 在元素内按文本查找锚点段落的几何矩形（相对元素盒原点）。
       *
       * @param {HTMLElement} root - 查找范围。
       * @param {string} needle - 锚点文本。
       * @returns {{left: number, top: number}} 锚点矩形（相对盒原点）。
       */
      const anchorRect = (root, needle) => {
        const walker2 = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
        let node = null;
        let t2;
        while ((t2 = walker2.nextNode())) {
          if (t2.textContent.includes(needle)) { node = t2; break; }
        }
        assert(node, `${root.id || "probe"} 中应找到锚点 ${needle}`);
        const r = document.createRange();
        const off = node.textContent.indexOf(needle);
        r.setStart(node, off);
        r.setEnd(node, off + needle.length);
        const rect = r.getBoundingClientRect();
        const box = root.getBoundingClientRect();
        return { left: rect.left - box.left, top: rect.top - box.top };
      };
      const pA = anchorRect(probe, "cd");
      const mA = anchorRect(mirror, "cd");
      // "e" 在空格盒之后：两锚点同时校验，空格占位宽度错误必然导致 "e" 漂移
      const pB = anchorRect(probe, "e");
      const mB = anchorRect(mirror, "e");
      const dA = Math.max(Math.abs(mA.left - pA.left), Math.abs(mA.top - pA.top));
      const dB = Math.max(Math.abs(mB.left - pB.left), Math.abs(mB.top - pB.top));
      assert(dA <= 1 && dB <= 1,
        `tab/空格符号后文本应逐位对齐：锚点cd Δ=${dA.toFixed(2)}px，` +
        `锚点e Δ=${dB.toFixed(2)}px`);
    } finally {
      probe.remove();
      pane.setSymbols(false);
    }
  });

  // ---- 几何自检：画布无意外滚动条（末列拉伸后应与视口齐平） ----
  check("画布无意外横向溢出（无幽灵滚动条）", () => {
    loadBasic();
    const sc = grid.scrollEl;
    const over = sc.scrollWidth - sc.clientWidth;
    const overV = sc.scrollHeight - sc.clientHeight;
    assert(
      over <= 1 && overV <= 1,
      `横向溢出 ${over}px（scrollWidth=${sc.scrollWidth} clientWidth=${sc.clientWidth}` +
      ` canvas=${sc.firstElementChild.style.width}）纵向溢出 ${overV}px`,
    );
  });

  // ========== 大文件模式（运行期强制开关 + 重复行夹具） ==========
  // 本段位于套件末尾：先把小夹具强制路由进大模式测全套链路，
  // 最后一项关开关并重新打开小文件，确认回到小模式。

  /** 轮询等待条件成真（大模式取数异步落定），超时抛错 */
  async function waitFor(fn, what, timeout = 10000) {
    const t0 = performance.now();
    for (;;) {
      try { if (await fn()) return; } catch { /* 落定前读越界等异常都算未就绪 */ }
      if (performance.now() - t0 > timeout) throw new Error(`等待超时: ${what}`);
      await sleep(50);
    }
  }

  /** 当前大模型（app.js 热交换后的实时模型） */
  const bigM = () => window.__app.model;

  /** 工具栏按钮查找（模式专属按钮始终存在于 DOM，靠 hidden 类显隐） */
  const tbBtn = (t) =>
    [...document.querySelectorAll("#toolbar button")].find((b) => b.textContent === t);

  /** 工具栏按钮是否真正不可见（计算样式 display:none——classList 探测曾被
   *  无对应 CSS 规则的 .hidden 类骗过：类在但 visually 仍显示，故必须查计算样式） */
  const tbHidden = (t) => getComputedStyle(tbBtn(t)).display === "none";

  const BIG_CSV = `${TMP}/big.csv`;

  check("大模式：强制开关 + 生成重复行夹具", async () => {
    await invoke("smoke_set_force_big", { v: true });
    await invoke("smoke_write_repeat",
      { path: BIG_CSV, line: "目的地,评分,备注\r\n", times: 1, append: false });
    await invoke("smoke_write_repeat",
      { path: BIG_CSV, line: "大理,3,古城\r\n", times: 5000, append: true });
    const sz = await invoke("file_size", { path: BIG_CSV });
    assert(sz > 0 && sz < 50 * 1024 * 1024,
      `夹具应小于大模式阈值（靠强制开关路由），实际 ${sz} 字节`);
    // 夹具 sanity：回读编码必须为 utf-8（引擎 GBK 误判会污染后续全部断言）
    const back = await invoke("read_csv_file", { path: BIG_CSV });
    assert(back.encoding === "utf-8",
      `夹具编码应为 utf-8，实际 ${back.encoding}，前 20 字：${back.text.slice(0, 20)}`);
    assert(back.text.startsWith("目的地"), `夹具表头异常：${back.text.slice(0, 20)}`);
  });

  check("大模式：openPath 路由到大模型（mmap 秒开）", async () => {
    await window.__app.openPath(BIG_CSV);
    assert(window.__app.isBigMode() === true, "应进入大文件模式");
    const bm = bigM();
    assert(bm.rowCount() === 5000, `数据行数 ${bm.rowCount()}`);
    assert(bm.columnCount() === 3, `列数 ${bm.columnCount()}`);
    assert(JSON.stringify(bm.headers()) === JSON.stringify(["目的地", "评分", "备注"]),
      `表头 ${bm.headers()}`);
    assert(bm.meta().encoding === "utf-8", `编码 ${bm.meta().encoding}`);
    // 模式专属工具栏显隐：大模式隐藏「排序…」、显示「执行筛选」；
    // 列操作两按钮无 pending 时应隐藏（小模式从不依赖这三个大文件按钮）。
    // 注意：必须断言计算样式 display——.hidden 类曾无 CSS 规则，类在但视觉不隐藏。
    assert(tbHidden("排序…"), "大模式应隐藏排序按钮");
    assert(!tbHidden("执行筛选"), "大模式应显示执行筛选");
    assert(tbHidden("执行列操作"), "无 pending 应隐藏执行列操作");
    assert(tbHidden("撤销列操作预览"), "无 pending 应隐藏撤销列操作预览");
  });

  check("大模式：首屏行异步落定并渲染", async () => {
    await waitFor(() =>
      document.querySelector('.cell[data-row="0"][data-col="0"]')?.textContent === "大理",
      "首屏行渲染");
  });

  check("大模式：跳远页（窗口缓存未命中→拉取→渲染）", async () => {
    grid.ensureVisible(4999, 0);
    grid._renderRows();
    await waitFor(() =>
      document.querySelector('.cell[data-row="4999"][data-col="0"]')?.textContent === "大理",
      "末行渲染");
  });

  check("大模式：异步取页落定不误标脏", async () => {
    // 回归：BigModel 预取页落定经 onData 重绘，不得触发 markDirty（打开即脏/滚动即脏）
    assert(window.__app.isDirty() === false, "跳远页取页落定后应为干净");
    grid.ensureVisible(2500, 0);
    grid._renderRows();
    await waitFor(() =>
      document.querySelector('.cell[data-row="2500"][data-col="0"]')?.textContent === "大理",
      "中行渲染");
    assert(window.__app.isDirty() === false, "再次取页落定后仍应为干净");
  });

  check("大模式：取页落定即重绘（不依赖后续滚动事件）", async () => {
    // 回归：滚动/拖动时新页未命中缓存先渲染空白占位，异步落定后必须立即重绘，
    // 否则空白要等到下一次滚动事件才填上——连续拖动表现为整屏空白/有内容逐帧
    // 交替（疯狂频闪），松手停在新页上还残留空白
    const sc = document.getElementById("grid-scroll");
    const oy = grid._originY();
    const h = grid._uniformRowH;
    let renders = 0;
    const orig = grid._renderRows.bind(grid);
    grid._renderRows = () => { renders += 1; orig(); };
    try {
      sc.scrollTop = Math.round(oy + (4999 * h) / grid._yScale);
      grid._renderRows(); // 同步渲染：新页未命中，格子空白占位并调度拉取
      const before = renders;
      // 落定与重绘同等待：rawData 命中缓存后，renders 必须因落定通知而增加——
      // 快照在渲染前取，避免"落定→重绘→快照"竞态造成假败
      await waitFor(() =>
        bigM().rawData(4999, 0) === "大理" && renders > before, "远页落定并重绘");
      assert(
        document.querySelector('.cell[data-row="4999"][data-col="0"]')?.textContent === "大理",
        "取页落定后格子仍空白");
    } finally {
      grid._renderRows = orig;
    }
  });

  check("大模式：缓存未命中保留旧内容（拖动不白闪）", async () => {
    // 回归：被 trim 换出的页滚回时缓存未命中——渲染必须保留旧内容（stale-while-
    // revalidate），不得整窗 tear-down 成空白占位，否则拖动滚动条每帧白闪（频闪根因）
    const sc2 = document.getElementById("grid-scroll");
    const oy2 = grid._originY();
    const h2 = grid._uniformRowH;
    const jump = (row) => {
      sc2.scrollTop = Math.round(oy2 + (row * h2) / grid._yScale);
      grid._renderRows();
    };
    jump(0);
    await waitFor(() =>
      document.querySelector('.cell[data-row="0"][data-col="0"]')?.textContent === "大理",
      "首屏落定");
    jump(3000); // 拉走缓存：此跳落定后 page0 被 trim 换出
    await waitFor(() => bigM().rawData(3000, 0) === "大理", "远页落定");
    jump(0); // 回旧行：同步渲染后立刻断言（无 await，落定来不及插足）
    assert(document.querySelector('.cell[data-row="0"][data-col="0"]')?.textContent === "大理",
      "缓存未命中时格子应为旧内容而非空白");
  });

  check("大模式：取页落定不换空编辑窗格（防窗格频闪）", async () => {
    // 回归：拖动滚动条时当前格的页被 trim 换出，落定瞬间 onData 经 rawData 读到
    // 空串会把编辑窗格刷空、页回灌又填满——窗格逐帧闪烁。未命中必须保留旧内容。
    grid.select([0, 0]);
    await waitFor(() => window.__app.pane.editEl.value === "大理", "窗格载入首格");
    const rec = [];
    const origSet = window.__app.pane.setText.bind(window.__app.pane);
    window.__app.pane.setText = (t) => { rec.push(String(t)); origSet(t); };
    try {
      const sc2 = document.getElementById("grid-scroll");
      const oy2 = grid._originY();
      const h2 = grid._uniformRowH;
      sc2.scrollTop = Math.round(oy2 + (3000 * h2) / grid._yScale);
      grid._renderRows();
      await waitFor(() => bigM().rawData(3000, 0) === "大理", "远页落定");
      await sleep(250); // 给页 0 回灌落定的机会：即便回灌，"刷空"这一笔也必须不存在
      assert(!rec.includes(""), `落定期间窗格被刷空（setText 记录：${rec.join("|")}）`);
    } finally {
      window.__app.pane.setText = origSet;
    }
  });

  check("大模式：单元格编辑 + 撤销/重做", async () => {
    const bm = bigM();
    grid.ensureVisible(0, 0); // 滚回首屏：编辑断言经窗口缓存读回，须先把缓存锚回首页
    grid._renderRows();
    await waitFor(() => bm.rawData(0, 0) === "大理", "首屏缓存回锚");
    await bm.setData(0, 1, "99");
    await waitFor(() => bm.rawData(0, 1) === "99", "编辑落定");
    assert(bm.canUndo() === true, "编辑后应可撤销");
    assert(await bm.undo() === true, "undo 应成功");
    await waitFor(() => bm.rawData(0, 1) === "3", "撤销复原");
    assert(await bm.redo() === true, "redo 应成功");
    await waitFor(() => bm.rawData(0, 1) === "99", "重做恢复");
    await bm.undo(); // 还原，避免影响后续计数
    await waitFor(() => bm.rawData(0, 1) === "3", "二次撤销复原");
  });

  check("大模式：执行筛选（引擎整表扫描）与命中数", async () => {
    const bm = bigM();
    grid.filters.set(0, "大理"); // 与真实交互同路径：条件进 grid，点执行筛选
    await window.__app.runBigFilter();
    assert(bm.filterActive() === true, "筛选应处于激活态");
    assert(await invoke("big_filter_count") === 5000, "引擎侧命中数不一致");
    grid.filters.set(0, "不存在xyz");
    await window.__app.runBigFilter();
    assert(await invoke("big_filter_count") === 0, "零命中应返回 0");
    grid.clearFilters(); // 条件清空 → 视图自动回全量（fire-and-forget）
    await sleep(150);
    assert(bm.filterActive() === false, "清除筛选后应回全量");
  });

  check("大模式：执行筛选显示进行中弹窗（防界面看似卡死）", async () => {
    grid.filters.set(0, "大理");
    const p = window.__app.runBigFilter(); // 不 await：弹窗须在同步阶段挂上
    const mask = document.querySelector(".modal-mask");
    assert(mask !== null && mask.textContent.includes("筛选"),
      "筛选期间应显示进行中弹窗");
    await p; // 等待完成
    assert(document.querySelector(".modal-mask") === null, "完成后弹窗应消失");
    grid.clearFilters();
    await sleep(150);
  });

  check("大模式：筛选结果分页取行", async () => {
    const bm = bigM();
    grid.filters.set(0, "大理");
    await window.__app.runBigFilter();
    const page = await invoke("big_filter_page", { from: 0, count: 5 });
    assert(page.length === 5, `筛选页行数 ${page.length}`);
    assert(page[4][0] === "大理" && page[4][1] === "3", `筛选页内容 ${page[4]}`);
    grid.clearFilters();
    await sleep(150);
    assert(bm.filterActive() === false, "清除筛选后应回全量");
  });

  check("大模式：插入行即时生效", async () => {
    const bm = bigM();
    const before = bm.rowCount();
    await bm.insertRows(1, 2);
    assert(bm.rowCount() === before + 2, `插行后行数 ${bm.rowCount()}`);
    // 插入点之后的数据行整体后移：原第 1 行"大理"现在应位于第 3 行
    await waitFor(() => bm.rawData(3, 0) === "大理", "插行后移位渲染");
  });

  check("大模式：删除行（含插入行）即时生效", async () => {
    const bm = bigM();
    const before = bm.rowCount();
    await bm.removeRows([1, 2]);
    assert(bm.rowCount() === before - 2, `删行后行数 ${bm.rowCount()}`);
    await waitFor(() => bm.rawData(1, 0) === "大理", "删行后复位渲染");
  });

  check("大模式：大选区复制委派引擎（TSV）", async () => {
    const tsv = await bigM().copyRows(0, 2, 0, 3); // 半开区间：2 行 × 3 列
    const lines = tsv.split("\r\n").filter((l) => l.length > 0); // 末行带行尾
    assert(lines.length === 2, `TSV 行数 ${lines.length}：${tsv.slice(0, 40)}`);
    assert(lines[0].split("\t").length === 3, `TSV 列数 ${lines[0].split("\t").length}`);
    assert(lines[0] === "大理\t3\t古城", `TSV 首行 ${lines[0]}`);
  });

  check("大模式：列操作仅 pending 预览，期间编辑被拒", async () => {
    const bm = bigM();
    await bm.insertColumns(1, 1);
    assert(bm.columnCount() === 4, `pending 后视图像素列数 ${bm.columnCount()}`);
    assert(bm.headers()[1] === "列4", `新列名 ${bm.headers()[1]}`);
    assert(bm.colOpsPending() === true, "应处于列操作 pending 态");
    let rejected = false;
    try { await bm.setData(0, 0, "昆明"); } catch { rejected = true; }
    assert(rejected, "列操作 pending 期间编辑应被引擎拒绝");
  });

  check("大模式：执行列操作（物理重写 + 进度事件 + 清空撤销）", async () => {
    const bm = bigM();
    let saw = null;
    const un = await bm.onProgress((done, total) => { saw = [done, total]; });
    try {
      await window.__app.applyColOps();
    } finally {
      un();
    }
    assert(saw !== null, "应收到至少一次进度事件");
    assert(saw[1] > 0 && saw[0] === saw[1], `进度收尾应到满：${saw}`);
    assert(bm.colOpsPending() === false, "执行后不应再有 pending");
    assert(bm.canUndo() === false, "物理重写后撤销栈应清空");
    assert(window.__app.isDirty() === false,
      "物理重写后文件与内存已一致，必须为干净（保存不得再全盘重写）");
    // 物理生效验证：引擎直读行应有 4 列，表头含新列
    const rows = await invoke("big_rows", { from: 0, count: 1 });
    assert(rows[0].length === 4, `物理行宽 ${rows[0].length}`);
    assert(bm.headers()[1] === "列4", `物理表头 ${bm.headers()}`);
  });

  check("大模式：放弃列操作预览恢复原结构", async () => {
    const bm = bigM();
    await bm.removeColumns([3]);
    assert(bm.columnCount() === 3, `pending 删除后列数 ${bm.columnCount()}`);
    await window.__app.cancelColOps();
    assert(bm.columnCount() === 4, "放弃后应恢复 4 列");
    assert(bm.colOpsPending() === false, "放弃后不应有 pending");
  });

  check("大模式：保存（流式重写）后重载比对", async () => {
    const bm = bigM();
    await bm.setData(0, 0, "昆明");
    await bm.save();
    const back = await invoke("read_csv_file", { path: BIG_CSV });
    const parsed = parseCsv(back.text);
    assert(parsed.headers.length === 4, `保存后表头列数 ${parsed.headers.length}`);
    assert(parsed.headers[1] === "列4", `保存后新列表头 ${parsed.headers[1]}`);
    assert(parsed.rows.length === 5000, `保存后行数 ${parsed.rows.length}`);
    assert(parsed.rows[0][0] === "昆明", `保存后首格 ${parsed.rows[0][0]}`);
    // 列操作后行已变为 4 列：["大理","","3","古城"]
    assert(parsed.rows[4999][3] === "古城", `末行内容应不变：${parsed.rows[4999]}`);
  });

  check("大模式：单格一对多填充逐格写引擎（选区外不动）", async () => {
    const bm = bigM();
    grid.ensureVisible(0, 0);
    grid._renderRows();
    await waitFor(() => bm.rawData(0, 0) === "昆明", "首屏缓存回锚");
    grid.select([0, 1]);
    grid._extendTo([1, 2]); // 2×2 选区（避开 col0，防污染后续大理计数）
    stubClipboard("填big\r\n");
    await grid.pasteClipboard();
    await waitFor(() => bm.rawData(1, 2) === "填big", "填充落定");
    assert(bm.rawData(0, 1) === "填big" && bm.rawData(0, 2) === "填big"
      && bm.rawData(1, 1) === "填big", "选区每个格都应填充");
    assert(bm.rawData(0, 0) === "昆明" && bm.rawData(1, 0) === "大理",
      "选区外的列不应被改");
    for (let i = 0; i < 4; i += 1) await bm.undo(); // 大模式撤销逐条：一格一条
    // undo 落定是异步的（缓存刷新经引擎回推），两个格都要等，不能只等一个
    await waitFor(() => bm.rawData(0, 1) === "" && bm.rawData(1, 2) === "3",
      "撤销后逐格复原");
  });

  check("大模式：筛选后回顶显示结果（旧滚动位置不串坐标系）", async () => {
    // 回归：筛选把坐标系从全量画布切到命中画布，旧 scrollTop 在新坐标系无意义。
    // 渲染锚点由 scrollTop 反推——旧值把锚点夹到末行、transform 把 #rows 推到画布
    // 外，且 transform 回灌 scrollHeight 自锁（滚动条拖不动/跳回、视口只剩末几行、
    // 上部空白）。筛选切换必须像装载一样回顶。
    const p = `${TMP}/big_fscroll.csv`;
    await invoke("smoke_write_repeat", { path: p, line: "目的地,评分,备注\r\n", times: 1, append: false });
    await invoke("smoke_write_repeat", { path: p, line: "大理,3,古城\r\n", times: 100, append: true });
    for (let i = 0; i < 19; i += 1) {
      await invoke("smoke_write_repeat", { path: p, line: "昆明,4,新城\r\n", times: 1000, append: true });
    }
    await invoke("smoke_set_force_big", { v: true });
    // 上一项（单格一对多填充）把大档签留在脏态；腾退换签会先弹 保存/放弃/取消，
    // 与真实交互一致地点掉（放弃夹具编辑），否则 openPath 永远等模态
    const pendingOpen = window.__app.openPath(p);
    await sleep(150);
    assert(clickDiscardDialog("放弃") === true, "腾退换签应先弹未保存确认");
    await pendingOpen;
    assert(window.__app.isBigMode() === true, "夹具应路由到大模式");
    const sc = document.getElementById("grid-scroll");
    const oy = grid._originY();
    const h = grid._uniformRowH;
    sc.scrollTop = Math.round(oy + (19100 * h) / grid._yScale); // 压深 ~99%
    grid._renderRows();
    grid.filters.set(0, "大理");
    await window.__app.runBigFilter();
    await sleep(250); // 等浏览器夹取 scrollTop 与 scroll 事件落定
    assert(Math.round(sc.scrollTop) <= 1,
      `筛选后应回顶，实际 scrollTop=${Math.round(sc.scrollTop)}`);
    assert(sc.scrollHeight < 10000,
      `筛选后画布应只含命中行高，实际 scrollHeight=${sc.scrollHeight}`);
    await waitFor(() =>
      document.querySelector('.cell[data-row="0"][data-col="0"]')?.textContent === "大理",
      "筛选结果首行渲染");
    grid.clearFilters();
    await sleep(250);
    assert(Math.round(sc.scrollTop) <= 1, "清除筛选回全量后也应回顶");
    await invoke("smoke_set_force_big", { v: false });
  });

  check("大模式：关闭开关后重新打开小文件回小模式", async () => {
    await invoke("smoke_set_force_big", { v: false });
    await window.__app.openPath(`${TMP}/rt.csv`);
    assert(window.__app.isBigMode() === false, "关开关后应回到小文件模式");
    assert(window.__app.model.rowCount() === 1, `小模型行数 ${window.__app.model.rowCount()}`);
    // 模式专属工具栏显隐回到小模式态：排序恢复显示，三个大文件按钮隐藏
    // （断言计算样式 display——.hidden 类曾无 CSS 规则，类在但视觉不隐藏）
    assert(!tbHidden("排序…"), "小模式应显示排序按钮");
    assert(tbHidden("执行筛选"), "小模式应隐藏执行筛选");
    assert(tbHidden("执行列操作"), "小模式应隐藏执行列操作");
    assert(tbHidden("撤销列操作预览"), "小模式应隐藏撤销列操作预览");
  });
  check("大模式：超高行数虚拟缩放（滚动到底不空白）", async () => {
    // 回归：元素/滚动高 16,777,214px 上限曾致大文件滚动后视口空白。
    // 100 万数据行 × ~20px ≈ 2000 万 px，超过 Y_CLAMP 触发纵向虚拟缩放。
    const p = `${TMP}/tall.csv`;
    await invoke("smoke_write_repeat", { path: p, line: "a,b,c,d,e\n", times: 1000001, append: false });
    await invoke("smoke_set_force_big", { v: true });
    await window.__app.openPath(p);
    const n = grid.visCount();
    assert(n > 999000, `行数 ${n}`);
    assert(grid._yScale > 1, `yScale=${grid._yScale} 应已启用虚拟缩放`);
    const sc = document.getElementById("grid-scroll");
    const cv = sc.firstElementChild;
    const styleH = parseFloat(cv.style.height);
    assert(styleH < 16800000, `画布 style 高 ${styleH} 应压到上限以内`);
    // 滚到底：锚点应落在末尾窗口，渲染行非空且接近末行
    sc.scrollTop = sc.scrollHeight;
    grid._renderRows();
    const kids = grid.rowsEl.children;
    assert(kids.length > 0, "滚动到底后应有渲染行（回归空白缺陷）");
    // 窗口局部化不变量：行元素自身偏移永不超 2^22（WebView2 绝对定位绘制上限）
    const maxOff = Math.max(...[...kids].map((k) => k.offsetTop));
    assert(maxOff < 4194304, `行内偏移应窗口局部化，maxOff=${maxOff}`);
    const firstRow = Number(kids[0].querySelector(".row-head")?.dataset.row);
    assert(firstRow > n - 10000, `底部首渲染行 ${firstRow} 应接近末行 ${n}`);
    // 命中测试与渲染一致：视口顶命中锚点行（= 首渲染行 + overscan 5，容差 1 行）
    const rect = sc.getBoundingClientRect();
    const hit = grid._hitTest(rect.left + 150, rect.top + 2);
    assert(hit && hit[0] >= firstRow + 4 && hit[0] <= firstRow + 6,
      `命中 ${hit?.[0]} 应≈首渲染行+5 ${firstRow + 5}`);
    // 回归 2^22 绘制上限（滚动条拖到 ~25% / 行 390 万+ 处逐行空白）：
    // 中段渲染行的屏幕矩形必须落在视口内（transform 容器 + 窗口局部偏移的几何不变量）
    const scRect = sc.getBoundingClientRect();
    const midKids = kids[Math.floor(kids.length / 2)];
    const midR = midKids.getBoundingClientRect();
    assert(midR.bottom > scRect.top && midR.top < scRect.bottom,
      `底部中段行屏幕矩形 ${Math.round(midR.top)}..${Math.round(midR.bottom)} 应在视口内`);
    await invoke("smoke_set_force_big", { v: false });
  });

  check("页签：大档引擎单例——第二大档腾退重开，小↔大切换免提示", async () => {
    await invoke("smoke_set_force_big", { v: true });
    const b1 = `${TMP}/tabbig/b1.csv`; const b2 = `${TMP}/tabbig/b2.csv`;
    for (const [p, name] of [[b1, "甲"], [b2, "乙"]]) {
      await invoke("smoke_write_repeat", { path: p, line: "目的地,评分\r\n", times: 1, append: false });
      await invoke("smoke_write_repeat", { path: p, line: `${name},1\r\n`, times: 1500, append: true });
    }
    await window.__app.openPathsAsTabs([b1, b2]);
    const t = window.__app.tabs();
    const tb1 = t.find((x) => x.path === b1);
    const tb2 = t.find((x) => x.path === b2);
    assert(tb1?.kind === "big" && tb2?.kind === "big", "两档都应是大档");
    assert(tb1.active === true && model.rowCount() === 1500, "首个大档应已激活");
    // 引擎腾退（b1 无脏，不弹窗）→ 重开 b2
    await window.__app.activateTab(tb2.id);
    assert(model.rowCount() === 1500, "第二大档行数");
    // 首屏行异步落定：缓存未命中时 rawData 返回空串，须等拉取（同大模式段惯例）
    await waitFor(() => model.rawData(0, 0) === "乙", "第二大档首行落定");
    // 回切 b1：引擎再次重开，数据来自磁盘原样
    await window.__app.activateTab(tb1.id);
    await waitFor(() => model.rawData(0, 0) === "甲", "重开第一大档首行落定");
    await window.__app.activateTab(tb2.id);
    await waitFor(() => model.rawData(0, 0) === "乙", "大小档再切换首行落定");
    await invoke("smoke_set_force_big", { v: false });
  });

  // ---- 执行（看门狗：整体超时强制收尾，防挂死） ----
  const watchdog = setTimeout(async () => {
    lines.push("", `[watchdog] 超时未完成，已通过 ${lines.length} 项`);
    await invoke("finish_smoke", { report: lines.join("\n"), code: 1 });
  }, 120000);
  for (const [name, fn] of checks) {
    try {
      await fn();
      lines.push(`✓ ${name}`);
    } catch (err) {
      failed += 1;
      lines.push(`✗ ${name} —— ${err.message}`);
    }
  }
  clearTimeout(watchdog);
  lines.push("");
  lines.push(`合计 ${checks.length} 项，失败 ${failed} 项`);
  const report = lines.join("\n");
  console.log(report);
  await invoke("finish_smoke", { report, code: failed ? 1 : 0 });
}
