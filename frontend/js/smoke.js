/**
 * --smoke 对抗自检套件（H1）：在真实 WebView 里跑端到端断言。
 *
 * 覆盖：Rust 命令层（版本/编码/BOM）、网格渲染与虚拟滚动、选区模型、
 * 格内编辑器键位、筛选、编辑窗格、工具栏、剪贴板三级判据、撤销合并、
 * 右键菜单文案、列宽/行高/列拖动、缩放夹取。
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
export async function runSmoke({ model, grid, pane, invoke }) {
  const lines = [];
  let failed = 0;
  const TMP = (await invoke("smoke_tmp_dir"));

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
    assert(grid.visibleRows.length === 1, `可见行应为 1，实际 ${grid.visibleRows.length}`);
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
    assert(grid.visibleRows.length === 3, "清空后应显示全部行");
  });

  check("多列筛选为 AND 关系", async () => {
    loadBasic();
    grid.filters.set(1, "1");
    grid.filters.set(0, "丽");
    grid._refilter();
    assert(grid.visibleRows.length === 1, "AND 后应只剩丽江");
    assert(model.rawData(grid.visibleRows[0], 0) === "丽江");
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
    assert(grid.visibleRows.length === 2, "或逻辑应命中两列各一行");
    const cb0 = document.querySelector('.col-multi[data-col="0"]');
    assert(cb0 && cb0.checked, "列0复选框应存在且勾选");
    cb0.checked = false;
    cb0.dispatchEvent(new Event("change", { bubbles: true }));
    assert(grid.visibleRows.length === 1, "取消列0勾选后只应剩备注列命中行");
    assert(model.rawData(grid.visibleRows[0], 0) === "丽江");
  });

  check("多重筛选结果可再被列筛选框叠加过滤", async () => {
    grid.filters.set(1, "10");
    grid._refilter();
    assert(grid.visibleRows.length === 1, "多重结果应再按列筛选二次过滤");
    assert(model.rawData(grid.visibleRows[0], 0) === "丽江");
  });

  check("多重筛选关闭后恢复：输入框隐藏、复选框移除", async () => {
    grid.clearFilters(); // 清掉上一步的列筛选
    const btn = [...document.querySelectorAll("#toolbar button")].find((b) => b.textContent === "多重筛选");
    btn.click(); // 关
    assert(!btn.classList.contains("on"), "再次点击应回到未选中态");
    assert(document.getElementById("multi-filter-wrap").classList.contains("hidden"),
      "关闭应隐藏输入框");
    assert(document.querySelectorAll(".col-multi").length === 0, "关闭应移除列名前复选框");
    assert(grid.visibleRows.length === 3, "关闭后应显示全部行");
  });

  check("清除筛选连带清空多重筛选关键字但保留开关", async () => {
    const btn = [...document.querySelectorAll("#toolbar button")].find((b) => b.textContent === "多重筛选");
    btn.click(); // 开
    grid.setMultiText("山");
    grid.filters.set(1, "10");
    grid.clearFilters();
    assert(grid.multi.text === "" && grid.filters.size === 0, "清除筛选应清空全部条件");
    assert(grid.visibleRows.length === 3, "清除后应显示全部行");
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
      "多重筛选开启应显式聚焦输入框");
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
    assert(items[0] === "在上方插入 1 行" && items[1] === "在末尾添加 N 行…" &&
      items[2] === "删除选中的 1 行…" && items[3] === "清除选中 1 行的内容…",
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

  // ---- 执行（看门狗：整体超时强制收尾，防挂死） ----
  const watchdog = setTimeout(async () => {
    lines.push("", `[watchdog] 超时未完成，已通过 ${lines.length} 项`);
    await invoke("finish_smoke", { report: lines.join("\n"), code: 1 });
  }, 45000);
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
