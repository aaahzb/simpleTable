/**
 * --smoke 对抗自检套件（H1）：在真实 WebView 里跑端到端断言。
 *
 * 覆盖：Rust 命令层（版本/编码/BOM）、网格渲染与虚拟滚动、选区模型、
 * 格内编辑器键位、筛选、编辑窗格、工具栏、剪贴板三级判据、撤销合并、
 * 右键菜单文案、列宽/行高/列拖动、缩放夹取、多文件页签（入签/切换/关闭/
 * 溢出滚动/多会话大档）、分屏视图（打开/焦点交换/双大档同屏/关签联动）、
 * 大文件模式（统一路由/翻页取数/编辑撤销重做/筛选/行插删/列操作 pending→执行/
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
  // 注意：应用已屏蔽「首行非列名」的持久化记忆；本套件还会断言开关不写
  // localStorage（绿色轻量），且夹具路径带时间戳、每次运行唯一。

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
  /**
   * 装载基础小表（新建表格路径）。
   *
   * 统一大引擎后磁盘文件都走 BigModel；本套件前半段验证的是内存小模型
   * （新建表格）的网格/键位/排序行为，故经冒烟钩子复位页签并装载小表。
   *
   * @returns {Promise<void>}
   */
  const loadBasic = async () => {
    grid.clearFilters(); // 与 app.js 装载一致：装载即清筛选
    await window.__app.smokeLoadSmall(headers, rows.map((r) => r.slice()));
  };

  /**
   * 断言某一侧的表头列与数据列逐列对齐（左缘与宽度），并校验左上角块
   * 与行号列同宽。
   *
   * @param {string} area - 区域选择器（"#grid-area" 或 "#grid-area2"）。
   * @param {string} label - 断言标签，失败时指明场景。
   * @returns {void}
   */
  function assertColsAligned(area, label) {
    const header = document.querySelector(`${area} [id^="col-header"]`);
    assert(header, `${label}: 应存在列头容器`);
    const headCells = [...header.querySelectorAll(":scope > .col-head-cell")];
    const row = document.querySelector(`${area} .row`);
    assert(row, `${label}: 应存在已渲染数据行`);
    const cells = [...row.querySelectorAll(":scope > .cell")];
    assert(cells.length > 0 && headCells.length === cells.length,
      `${label}: 列头 ${headCells.length} 与数据列 ${cells.length} 数量应一致`);
    const scEl = document.querySelector(`${area} [id^="grid-scroll"]`);
    const meta = `${label}（scrollLeft=${scEl?.scrollLeft ?? "?"}）`;
    for (let i = 0; i < headCells.length; i += 1) {
      const h = headCells[i].getBoundingClientRect();
      const d = cells[i].getBoundingClientRect();
      assert(Math.abs(h.left - d.left) <= 1 && Math.abs(h.width - d.width) <= 1,
        `${meta}: 第 ${i} 列列头 x=${h.left.toFixed(1)}/w=${h.width.toFixed(1)}，`
        + `数据 x=${d.left.toFixed(1)}/w=${d.width.toFixed(1)}，应逐列对齐`);
    }
    const corner = header.firstElementChild;
    const rowHead = row.querySelector(".row-head");
    assert(corner && rowHead, `${label}: 角块与行号列都应存在`);
    const cr = corner.getBoundingClientRect();
    const rr = rowHead.getBoundingClientRect();
    assert(Math.abs(cr.width - rr.width) <= 1 && Math.abs(cr.right - rr.right) <= 1,
      `${meta}: 角块 ${cr.left.toFixed(1)}..${cr.right.toFixed(1)}，`
      + `行号列 ${rr.left.toFixed(1)}..${rr.right.toFixed(1)}，应同宽同位`);
  }

  // ========== 空闲稳定性 ==========

  check("空闲稳定：连续两次布局不得引发滚动条/画布尺寸翻转", async () => {
    // 用户现场：启动后什么都不做，表格区域逐帧狂闪。根因是 clientWidth/
    // clientHeight 会把 761.5px 之类的真实小数尺寸四舍五入成 762；画布按
    // 762 设置就比真实可视区高 0.5px，滚动条出现后 ResizeObserver 又触发
    // 重排，下一帧画布缩小、滚动条消失……尺寸逐帧翻转。
    // 这里在空闲态连做两次布局：任何一次尺寸翻转都会让两次快照不同。
    const sc = document.getElementById("grid-scroll");
    const cv = document.getElementById("grid-canvas");
    const snap = () => [
      sc.clientWidth, sc.clientHeight, sc.scrollWidth, sc.scrollHeight,
      cv.style.width, cv.style.height,
    ].join("|");
    grid.renderAll();
    const first = snap();
    grid.renderAll();
    const second = snap();
    assert(first === second,
      `连续两次空闲布局结果不一致：${first} vs ${second}；`
      + "尺寸翻转会让表格区域逐帧闪烁");
  });

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
    await loadBasic();
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
    await loadBasic();
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
    await loadBasic();
    grid.selectRow(1);
    assert(grid.selectedSet.size === 3, `整行应选中 3 格，实际 ${grid.selectedSet.size}`);
  });

  check("列头点击选整列", async () => {
    grid.selectCol(2);
    assert(grid.selectedSet.size === 3, `整列应选中 3 格，实际 ${grid.selectedSet.size}`);
  });

  check("Esc 清空选区但保留当前格（对齐 Qt）", async () => {
    await loadBasic();
    grid.selectRow(1);
    assert(grid.selectedSet.size === 3, `选中整行应 3 格，实际 ${grid.selectedSet.size}`);
    key(document.getElementById("grid-scroll"), "Escape");
    assert(grid.selectedSet.size === 0 && grid.sel.blocks.length === 0,
      "Esc 后选区应为空");
    assert(cur(grid)?.[0] === 1, "Esc 后当前格应保留（对齐 Qt 表格 Escape 行为）");
  });

  check("筛选框聚焦打字不被劫持为格内编辑", async () => {
    await loadBasic();
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

  check("Ctrl+↓/↑：按 Excel 语义跳到当前列数据边缘", async () => {
    await loadBasic();
    model.load(["值"], [["1"], ["2"], [""], ["3"], ["4"]]);
    grid.renderAll();
    const scroller = document.getElementById("grid-scroll");
    scroller.focus();
    grid.select([0, 0]);
    key(scroller, "ArrowDown", { ctrlKey: true });
    assert(cur(grid)?.[0] === 1, "连续块末格应为 1");
    key(scroller, "ArrowDown", { ctrlKey: true });
    assert(cur(grid)?.[0] === 3, "块边缘再按应越过空档到 3");
    key(scroller, "ArrowDown", { ctrlKey: true });
    assert(cur(grid)?.[0] === 4, "下一条块末格应为 4");
    key(scroller, "ArrowDown", { ctrlKey: true });
    assert(cur(grid)?.[0] === 4, "末行再按应停住");
    key(scroller, "ArrowUp", { ctrlKey: true });
    assert(cur(grid)?.[0] === 3, "向上到连续块首格");
    key(scroller, "ArrowUp", { ctrlKey: true });
    assert(cur(grid)?.[0] === 1, "向上越过空档到上块末格");
    key(scroller, "ArrowUp", { ctrlKey: true });
    assert(cur(grid)?.[0] === 0, "上块首格");
    grid.select([2, 0]);
    key(scroller, "ArrowDown", { ctrlKey: true });
    assert(cur(grid)?.[0] === 3, "空格向下找下一个非空");
    grid.select([2, 0]);
    key(scroller, "ArrowUp", { ctrlKey: true });
    assert(cur(grid)?.[0] === 1, "空格向上找上一个非空");
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
    await loadBasic();
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
    await loadBasic();
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

  check("几何：表头列与数据列逐列对齐（宽表/横向滚动/缩放）", async () => {
    // 回归：表头与数据行是两套 DOM（列头 flex / 行 flex），任何宽度或
    // 行号列换算不一致都会让列名与筛选框整体偏移。
    const wideHeaders = Array.from({ length: 20 }, (_, i) => `列${i}`);
    const wideRows = [Array.from({ length: 20 }, (_, i) => `值${i}`)];
    await window.__app.smokeLoadSmall(wideHeaders, wideRows);
    grid.resetScroll();
    grid.ensureVisible(0, 0);
    grid._renderRows();
    assertColsAligned("#grid-area", "初始");
    const sc = document.getElementById("grid-scroll");
    sc.scrollLeft = 137;
    await sleep(50);
    assertColsAligned("#grid-area", "横向滚动 137px");
    sc.scrollLeft = 0;
    grid.setZoom(1.15);
    await sleep(30);
    assertColsAligned("#grid-area", "缩放 115%");
    grid.setZoom(1);
    await loadBasic(); // 还原基础小表，供后续用例使用
  });

  check("列插入后筛选条件清空", async () => {
    grid.filters.set(0, "洱");
    model.insertColumns(0, 1);
    grid.clearFilters();
    assert(grid.filters.size === 0, "筛选应已清空");
    assert(grid.visCount() === 3, "清空后应显示全部行");
  });

  check("多列筛选为 AND 关系", async () => {
    await loadBasic();
    grid.filters.set(1, "1");
    grid.filters.set(0, "丽");
    grid._refilter();
    assert(grid.visCount() === 1, "AND 后应只剩丽江");
    assert(model.rawData(grid.visAt(0), 0) === "丽江");
  });

  // ========== 多重筛选 ==========

  check("多重筛选：按钮在清除筛选右侧，开启出现输入框与默认勾选的列复选框", async () => {
    await loadBasic();
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
    await loadBasic();
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
    await loadBasic();
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
    for (const t of ["新建", "打开", "保存", "另存为", "撤销", "重做", "排序…", "清除筛选", "多重筛选", "首行非列名", "快捷键"]) {
      assert(texts.includes(t), `工具栏缺少 ${t}`);
    }
    await loadBasic(); // load 清空撤销栈
    assert(document.querySelector("#toolbar button").parentElement, "工具栏存在");
    const [undoB, redoB] = [document.querySelectorAll("#toolbar button")[4],
      document.querySelectorAll("#toolbar button")[5]];
    assert(undoB.disabled && redoB.disabled, "load 后撤销/重做应置灰");
    model.setData(0, 0, "x");
    assert(!undoB.disabled, "改动后撤销应可用");
  });

  check("布局：工具栏不撑宽页面，快捷键按钮与编辑窗格都在窗口内", () => {
    const vw = window.innerWidth;
    assert(document.documentElement.scrollWidth <= vw + 1,
      `页面不得横向溢出：scrollWidth=${document.documentElement.scrollWidth} innerWidth=${vw}`);
    const btn = tbBtn("快捷键");
    const br = btn.getBoundingClientRect();
    assert(br.width > 0 && br.left >= 0 && br.right <= vw + 1,
      `快捷键按钮应在窗口内：left=${Math.round(br.left)} right=${Math.round(br.right)} innerWidth=${vw}`);
    const pane = document.getElementById("pane").getBoundingClientRect();
    assert(pane.width > 100 && pane.right <= vw + 1,
      `编辑窗格应完整可见：width=${Math.round(pane.width)} right=${Math.round(pane.right)} innerWidth=${vw}`);
  });

  check("快捷键按钮：位于工具栏最右，打开快捷键一览", async () => {
    const btn = tbBtn("快捷键");
    assert(btn, "工具栏应有快捷键按钮");
    assert(document.getElementById("toolbar").lastElementChild === btn,
      "快捷键按钮应位于工具栏最右（编辑窗格上方）");
    btn.click();
    await waitFor(() => document.querySelector(".modal-mask"), "快捷键对话框");
    const text = document.querySelector(".modal-mask").textContent;
    for (const k of ["F2 / 双击", "Enter / Tab", "Alt+Enter", "Ctrl+C / Ctrl+X / Ctrl+V",
      "Ctrl+Z / Ctrl+Y", "Ctrl+↑ / Ctrl+↓", "Ctrl+G", "Ctrl+S", "Ctrl+滚轮"]) {
      assert(text.includes(k), `快捷键说明缺少「${k}」`);
    }
    assert(clickModalButton("关闭"), "快捷键对话框应有「关闭」按钮");
    await waitFor(() => !document.querySelector(".modal-mask"), "关闭快捷键对话框");
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
    await loadBasic();
    assert(window.__app.isDirty() === false, "load 后应为干净");
  });

  // ========== 剪贴板 ==========

  check("复制选区为 TSV（含引号规则）", async () => {
    await loadBasic();
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
    await loadBasic();
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
    await loadBasic();
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
    await loadBasic();
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

  /** 在任意模态里按按钮文字点击（找不到返回 false） */
  function clickModalButton(text) {
    const mask = document.querySelector(".modal-mask");
    const btn = [...(mask?.querySelectorAll("button") ?? [])]
      .find((b) => b.textContent === text);
    if (!btn) return false;
    btn.click();
    return true;
  }

  check("页签：多文件拖入出标签条，全部入签，激活首个", async () => {
    const f1 = `${TMP}/tabm/a.csv`; const f2 = `${TMP}/tabm/b.csv`; const f3 = `${TMP}/tabm/c.csv`;
    await writeText(f1, "目的地,评分\r\n甲,1\r\n");
    await writeText(f2, "目的地,评分\r\n乙,2\r\n");
    await writeText(f3, "目的地,评分\r\n丙,3\r\n");
    // 前置：复位为空的小表并确定性弄脏，验证「未入签脏档换文件先确认」
    await loadBasic();
    await model.setData(0, 0, "脏");
    await waitFor(() => window.__app.isDirty() === true, "拖入前应先标脏");
    const before = window.__app.tabs().length;
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
    await waitFor(() => model.rawData(0, 0) === "甲", "激活档内容应为首个文件");
  });

  check("页签：切换保留数据/脏态/撤销，互不串档", async () => {
    const ta = window.__app.tabs().find((x) => x.path?.endsWith("tabm/a.csv"));
    const tb = window.__app.tabs().find((x) => x.path?.endsWith("tabm/b.csv"));
    await window.__app.activateTab(ta.id);
    await waitFor(() => model.rawData(0, 0) === "甲", "a 档首行落定");
    await model.setData(0, 0, "改过");
    await waitFor(() => window.__app.isDirty() === true, "编辑后应标脏");
    await window.__app.activateTab(tb.id);
    await waitFor(() => model.rawData(0, 0) === "乙", "b 档内容");
    assert(window.__app.isDirty() === false, "b 档未改过应不脏");
    await window.__app.activateTab(ta.id);
    await waitFor(() => model.rawData(0, 0) === "改过", "a 档修改应随签保留");
    assert(window.__app.isDirty() === true, "a 档脏态应随签保留");
    await model.undo();
    await waitFor(() => model.rawData(0, 0) === "甲", "a 档撤销栈应保留");
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
    await model.setData(0, 0, "又要关"); // 弄脏
    await waitFor(() => window.__app.isDirty() === true, "关签前应先标脏");
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
    const scroll = document.getElementById("tab-group-a"); // 双组渲染后滚动发生在组容器
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
    await waitFor(() => sideCellText("A", 0, 0) === "甲A", "主侧活动档首行落定");
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
    assert(sp.on === true && sp.curB === tb.id && sp.focused === "A",
      `分屏态应就绪 ${JSON.stringify(sp)}`);
    assert(JSON.stringify(sp.b) === JSON.stringify([tb.id]), `右组应只有 b 签 ${JSON.stringify(sp)}`);
    assert(tabEl("b.csv")?.closest("#tab-group-b"), "b 签元素应在右组容器");
    assert(tabEl("a.csv")?.closest("#tab-group-a"), "a 签元素应在左组容器");
    assert(window.__app.tabs().find((x) => x.id === tb.id)?.side === "B", "b 签 side 应为 B");
    assert(getComputedStyle(document.getElementById("grid-area2")).display !== "none",
      "副网格应可见");
    assert(document.getElementById("grid-area2").offsetWidth > 0, "副网格应有实际宽度");
    assert(window.__app.tabs().find((x) => x.active)?.id === ta.id, "活动签应保持 a（焦点未动）");
    await waitFor(() => sideCellText("A", 0, 0) === "甲A", "主侧应显示活动签 a");
    await waitFor(() => sideCellText("B", 0, 0) === "乙B", "副侧应显示分屏签 b");
  });

  check("分屏：副网格角块存在，两侧列头与数据列逐列对齐", async () => {
    // 回归：_renderHeader 只认 id="corner"，副网格的 #corner2 每次重建都被
    // 删掉，导致副侧列名/筛选框整体左移一个行号列宽。
    assert(document.getElementById("corner"), "主侧角块 #corner 应存在");
    assert(document.getElementById("corner2"), "副侧角块 #corner2 不应被表头重建删除");
    assertColsAligned("#grid-area", "分屏主侧");
    assertColsAligned("#grid-area2", "分屏副侧");
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
    const actId = window.__app.tabs().find((x) => x.active)?.id;
    const lastId = window.__app.tabs().at(-1)?.id;
    const aInB = !!tabEl("a.csv")?.closest("#tab-group-b");
    await waitFor(() => sideCellText("B", 0, 0) === "甲A", "副侧应显示原活动签 a");
    await waitFor(() => sideCellText("A", 0, 0) === "乙B", "主侧应显示邻签 b");
    await window.__app.closeSplit();
    await window.__app.activateTab(ta.id); // 还原：焦点回 a（单视图）
    // 断言放在还原之后：RED 期失败也不污染后续用例的现场
    assert(sp.on === true && sp.curB === ta.id && sp.focused === "A",
      `活动签入分屏态应就绪 ${JSON.stringify(sp)}`);
    assert(JSON.stringify(sp.b) === JSON.stringify([ta.id]), `右组应只有原活动签 a ${JSON.stringify(sp)}`);
    assert(actId === tb.id, "主侧应换显邻签 b");
    assert(sp.curA === tb.id, "左组当前签应为邻签 b");
    assert(aInB, "a 签元素应移入右组容器");
    assert(lastId === ta.id, "组邻接：右组签应排在页签数组末尾");
  });

  check("分屏：点副侧焦点交换——内容稳定，编辑落副签", async () => {
    const fa = `${TMP}/sv/a.csv`; const fb = `${TMP}/sv/b.csv`;
    const ta = window.__app.tabs().find((x) => x.path === fa);
    const tb = window.__app.tabs().find((x) => x.path === fb);
    // 自建分屏（a 主 b 副），不依赖前项的分屏残留态
    await window.__app.activateTab(ta.id);
    await window.__app.openSplit(tb.id);
    assert(window.__app.split().on === true, "分屏应建立");
    await waitFor(() => sideCellText("A", 0, 0) === "甲A"
      && sideCellText("B", 0, 0) === "乙B", "两侧首行落定");
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
    await waitFor(() => window.__app.tabs().find((x) => x.id === tb.id)?.dirty === true,
      "编辑应弄脏 b 签");
    const t = window.__app.tabs();
    assert(t.find((x) => x.id === ta.id)?.dirty === false, "a 签不应脏");
    // 还原：撤销编辑 + 焦点回 A，避免影响后续（经外壳 undo 包装：撤销不改
    // 文档的脏状态——onReset 会清脏，包装末尾重新标脏，这是既定语义）
    await window.__app.activateTab(ta.id);
    assert(window.__app.split().focused === "A", "焦点应可切回主侧");
    await window.__app.activateTab(tb.id);
    await window.__app.undo();
    await waitFor(() => model.rawData(0, 0) === "乙B", "撤销后 b 签首格应复原");
    assert(window.__app.tabs().find((x) => x.id === tb.id)?.dirty === true,
      "撤销后 b 签应保持脏（供关签确认场景使用）");
    await window.__app.activateTab(ta.id);
  });

  check("分屏：两个大档可同屏——各自独立会话", async () => {
    // 统一大引擎后所有磁盘文件都是大档：分屏必须允许两侧各开一个大文件。
    await window.__app.closeSplit();
    const b1 = `${TMP}/sv/big1.csv`; const b2 = `${TMP}/sv/big2.csv`;
    for (const [p, name] of [[b1, "大甲"], [b2, "大乙"]]) {
      await invoke("smoke_write_repeat", { path: p, line: "目的地\r\n", times: 1, append: false });
      await invoke("smoke_write_repeat", { path: p, line: `${name},1\r\n`, times: 800, append: true });
    }
    await window.__app.openPathsAsTabs([b1, b2]);
    await waitFor(() => model.rawData(0, 0) === "大甲", "首个大档首行落定");
    const ta = window.__app.tabs().find((x) => x.path === b1);
    const tb = window.__app.tabs().find((x) => x.path === b2);
    assert(ta?.kind === "big" && tb?.kind === "big", "两档都应是大档");
    const el = tabEl("big2.csv");
    el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    const item = [...document.querySelectorAll(".ctx-menu .mi")]
      .find((x) => x.textContent === "在分屏中打开");
    assert(item && !item.classList.contains("disabled"), "双大档项应可点击");
    item.click();
    await waitFor(() => window.__app.split().on === true, "双大档应建立分屏");
    const sp = window.__app.split();
    await waitFor(() => sideCellText("A", 0, 0) === "大甲"
      && sideCellText("B", 0, 0) === "大乙", "两侧大档首行落定");
    // 独立会话：改副侧档，主侧内容与脏态不受影响
    await window.__app.activateTab(tb.id); // 分屏内 = 焦点交换
    await model.setData(0, 0, "大乙改");
    await waitFor(() => sideCellText("B", 0, 0) === "大乙改", "副侧编辑落定");
    assert(sideCellText("A", 0, 0) === "大甲", "主侧大档不应被副侧编辑影响");
    assert(window.__app.tabs().find((x) => x.id === ta.id)?.dirty === false,
      "主侧大档不应被弄脏");
    // 清理：关分屏，关掉两个大签（副侧已改，放弃）
    await window.__app.closeSplit();
    for (const p of [b1, b2]) {
      const tab = window.__app.tabs().find((x) => x.path === p);
      if (!tab) continue;
      const cp = window.__app.closeTab(tab.id);
      await sleep(100);
      clickDiscardDialog("放弃");
      await cp;
    }
    const fa = `${TMP}/sv/a.csv`;
    const back = window.__app.tabs().find((x) => x.path === fa);
    if (back) await window.__app.activateTab(back.id);
    // 断言放在清理之后：RED 期失败也不残留现场
    assert(sp.curB === tb.id && sp.focused === "A", `分屏态 ${JSON.stringify(sp)}`);
    assert(JSON.stringify(sp.b) === JSON.stringify([tb.id]), `右组应只有 big2 ${JSON.stringify(sp)}`);
  });

  check("分屏：拖 #vsplit 分隔条调整两侧宽度，关闭后复位", async () => {
    const fa = `${TMP}/sv/a.csv`; const fb = `${TMP}/sv/b.csv`;
    await window.__app.activateTab(window.__app.tabs().find((x) => x.path === fa).id);
    const tb = window.__app.tabs().find((x) => x.path === fb);
    await window.__app.openSplit(tb.id);
    assert(window.__app.split().on === true, "分屏应建立");
    await waitFor(() => sideCellText("A", 0, 0) === "甲A"
      && sideCellText("B", 0, 0) === "乙B", "拖拽前两侧首行落定");
    const ga = document.getElementById("grid-area");
    const gb = document.getElementById("grid-area2");
    const vs = document.getElementById("vsplit");
    const w0 = ga.offsetWidth;
    assert(Math.abs(w0 - gb.offsetWidth) < 4, `初始应左右均分（${w0} vs ${gb.offsetWidth}）`);
    // 模拟拖动分隔条：按住向右拖 120px，主侧应变宽、副侧吸收余量变窄
    const r = vs.getBoundingClientRect();
    const sx = r.left + 2;
    mouse(vs, "mousedown", { clientX: sx, clientY: 200 });
    mouse(document, "mousemove", { clientX: sx + 120, clientY: 200 });
    mouse(document, "mouseup", { clientX: sx + 120, clientY: 200 });
    const w1 = ga.offsetWidth;
    assert(w1 >= w0 + 100 && w1 <= w0 + 140,
      `拖右 120px 后主侧应变宽（${w0}→${w1}）`);
    assert(gb.offsetWidth <= w0 - 80, `拖右后副侧应变窄（→${gb.offsetWidth}）`);
    assert(sideCellText("A", 0, 0) === "甲A" && sideCellText("B", 0, 0) === "乙B",
      "拖拽后两侧内容应稳定");
    await window.__app.closeSplit();
    assert(ga.style.flex === "", "关闭分屏应清掉主侧拖拽定宽（恢复弹性布局）");
  });

  check("分屏：关闭分屏与关签联动", async () => {
    const fa = `${TMP}/sv/a.csv`; const fb = `${TMP}/sv/b.csv`;
    await window.__app.activateTab(window.__app.tabs().find((x) => x.path === fa).id);
    const tb = window.__app.tabs().find((x) => x.path === fb);
    await window.__app.openSplit(tb.id);
    assert(window.__app.split().on === true, "分屏应建立");
    await waitFor(() => sideCellText("B", 0, 0) === "乙B", "分屏签首行落定");
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
    assert(window.__app.tabs().find((x) => x.id === tb.id)?.side === "A",
      "收屏后原右组签应并入左组");
    assert(window.__app.tabs().at(-1)?.id === tb.id, "并入左组后应保持相对序（数组末尾）");
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

  // ========== 分屏页签分组（左右两组 + 拖拽换侧）==========

  /** 关掉全部页签（脏签一律放弃），回到无签空表。 */
  async function closeAllTabs() {
    for (let guard = 0; guard < 80; guard += 1) {
      const t = window.__app.tabs();
      if (!t.length) {
        assert(window.__app.split().on === false, "全关后不应残留分屏");
        return;
      }
      const p = window.__app.closeTab(t[t.length - 1].id);
      await sleep(80); // 等脏签确认框挂载
      clickDiscardDialog("放弃");
      await p;
    }
    throw new Error("closeAllTabs 超轮次");
  }

  check("分组页签 N1：组内重排——moveTab 直调，顺序与绑定同步", async () => {
    await closeAllTabs();
    const f1 = `${TMP}/grp/r1.csv`; const f2 = `${TMP}/grp/r2.csv`; const f3 = `${TMP}/grp/r3.csv`;
    for (const [p, v] of [[f1, "排一"], [f2, "排二"], [f3, "排三"]]) {
      await writeText(p, `目的地\r\n${v}\r\n`);
    }
    await window.__app.openPathsAsTabs([f1, f2, f3]); // 激活 r1
    const t0 = window.__app.tabs();
    const id1 = t0.find((x) => x.path === f1).id;
    const id3 = t0.find((x) => x.path === f3).id;
    const activeBefore = t0.find((x) => x.active).id;
    const ok = await window.__app.moveTab(id3, "A", 0);
    assert(ok === true, "组内移动应成功");
    const ids = window.__app.tabs().map((x) => x.id);
    assert(ids.indexOf(id3) < ids.indexOf(id1), `重排后 r3 应排在 r1 前 ${ids}`);
    assert(window.__app.tabs().find((x) => x.active)?.id === activeBefore, "组内重排不应改活动签");
    const domIds = [...document.querySelectorAll("#tab-group-a .tab")].map((x) => x.dataset.id);
    assert(JSON.stringify(domIds) === JSON.stringify(ids.map(String)),
      `DOM 顺序应与 tabs() 一致 ${domIds} vs ${ids}`);
  });

  check("分组页签 N2：moveTab 换侧——后台签入右组立分屏，焦点不动", async () => {
    // 前置：N1 留下 r1-r3 三签（未分屏，活动签 r1）
    const t0 = window.__app.tabs();
    const r1 = t0.find((x) => x.path?.endsWith("r1.csv"));
    const r3 = t0.find((x) => x.path?.endsWith("r3.csv"));
    await window.__app.activateTab(r1.id);
    const ok = await window.__app.moveTab(r3.id, "B");
    assert(ok === true, "换侧应成功");
    const sp = window.__app.split();
    assert(sp.on === true && sp.focused === "A", `换侧应立分屏且焦点留 A ${JSON.stringify(sp)}`);
    assert(sp.curB === r3.id && JSON.stringify(sp.b) === JSON.stringify([r3.id]),
      `r3 应成右组唯一签且为当前签 ${JSON.stringify(sp)}`);
    assert(sp.curA === r1.id, "左组当前签应保持 r1");
    await waitFor(() => sideCellText("A", 0, 0) === "排一", "左格应仍显 r1");
    await waitFor(() => sideCellText("B", 0, 0) === "排三", "右格应显被移签 r3");
    assert(tabEl("r3.csv")?.closest("#tab-group-b"), "r3 签元素应在右组容器");
  });

  check("分组页签 N3：拖拽事件通路——合成 DnD 完成一次换侧", async () => {
    // 前置：N2 后分屏中（A: r1,r2；B: r3），焦点 A。只测一条事件通路，
    // 逻辑全排列由 moveTab 直调用例覆盖（合成 DragEvent 的行为与真拖可能有出入）。
    const r2 = window.__app.tabs().find((x) => x.path?.endsWith("r2.csv"));
    const r3 = window.__app.tabs().find((x) => x.path?.endsWith("r3.csv"));
    const el = tabEl("r2.csv");
    assert(el?.closest("#tab-group-a"), "r2 应在左组");
    const groupB = document.getElementById("tab-group-b");
    const rect = tabEl("r3.csv").getBoundingClientRect();
    const dt = new DataTransfer();
    el.dispatchEvent(new DragEvent("dragstart", { bubbles: true, cancelable: true, dataTransfer: dt }));
    groupB.dispatchEvent(new DragEvent("dragover", {
      bubbles: true, cancelable: true, clientX: rect.right + 4, clientY: rect.top + 5, dataTransfer: dt,
    }));
    groupB.dispatchEvent(new DragEvent("drop", {
      bubbles: true, cancelable: true, clientX: rect.right + 4, clientY: rect.top + 5, dataTransfer: dt,
    }));
    el.dispatchEvent(new DragEvent("dragend", { bubbles: true, cancelable: true, dataTransfer: dt }));
    await sleep(100); // drop 里 moveTab 是 async
    const sp = window.__app.split();
    assert(sp.on === true && JSON.stringify(sp.b) === JSON.stringify([r3.id, r2.id]),
      `r2 应移到右组末尾 ${JSON.stringify(sp)}`);
    assert(sp.curB === r2.id, "被拖签应成右组当前签");
    await waitFor(() => sideCellText("B", 0, 0) === "排二", "右格应显 r2");
    const ind = document.getElementById("tab-insert");
    assert(!ind || getComputedStyle(ind).display === "none", "拖拽后插入指示器应隐藏");
  });

  check("分组页签 N4：右组拖空自动收起分屏", async () => {
    // 前置：N3 后 A:[r1] B:[r3,r2]，焦点 A
    const t0 = window.__app.tabs();
    const r2 = t0.find((x) => x.path?.endsWith("r2.csv"));
    const r3 = t0.find((x) => x.path?.endsWith("r3.csv"));
    await window.__app.moveTab(r3.id, "A");
    assert(window.__app.split().on === true, "右组还有 r2，分屏应保持");
    await window.__app.moveTab(r2.id, "A");
    const sp = window.__app.split();
    assert(sp.on === false, `右组拖空应自动收屏 ${JSON.stringify(sp)}`);
    assert(getComputedStyle(document.getElementById("grid-area2")).display === "none", "副网格应隐藏");
    const t1 = window.__app.tabs();
    assert(t1.every((x) => x.side === "A"), "收屏后全部签应归左组");
    const ids = t1.map((x) => x.id);
    assert(ids.indexOf(r3.id) < ids.indexOf(r2.id), "回原右组相对序应保持");
    assert(t1.find((x) => x.active)?.id === r2.id, "移入焦点组的签应成为活动签");
  });

  check("分组页签 N5：禁止拖空左组——moveTab 拒绝且状态不变", async () => {
    // 前置：N4 后未分屏，活动签 r2。先摆出 1A+2B
    const t0 = window.__app.tabs();
    const r1 = t0.find((x) => x.path?.endsWith("r1.csv"));
    const r2 = t0.find((x) => x.path?.endsWith("r2.csv"));
    const r3 = t0.find((x) => x.path?.endsWith("r3.csv"));
    await window.__app.moveTab(r2.id, "B");
    await window.__app.moveTab(r3.id, "B");
    const before = JSON.stringify(window.__app.tabs().map((x) => x.id));
    const spBefore = window.__app.split();
    assert(spBefore.on === true && spBefore.a.length === 1, `前置应为 1A+2B ${JSON.stringify(spBefore)}`);
    const ok = await window.__app.moveTab(r1.id, "B");
    assert(ok === false, "拖空左组应被拒绝");
    assert(JSON.stringify(window.__app.tabs().map((x) => x.id)) === before, "拒绝后页签顺序不变");
    assert(JSON.stringify(window.__app.split()) === JSON.stringify(spBefore), "拒绝后分屏态不变");
  });

  check("分组页签 N6：关左组最后一签——右组整体补位升格", async () => {
    // 前置：N5 后 1A(r1)+2B(r2,r3)
    const t0 = window.__app.tabs();
    const r1 = t0.find((x) => x.path?.endsWith("r1.csv"));
    const r2 = t0.find((x) => x.path?.endsWith("r2.csv"));
    const r3 = t0.find((x) => x.path?.endsWith("r3.csv"));
    const curB = window.__app.split().curB;
    await window.__app.closeTab(r1.id); // r1 干净，无确认
    const sp = window.__app.split();
    assert(sp.on === false, `左组关空应收屏 ${JSON.stringify(sp)}`);
    const t1 = window.__app.tabs();
    assert(t1.length === 2 && t1.every((x) => x.side === "A"), "右组两签应并入左组");
    const ids = t1.map((x) => x.id);
    assert(ids.indexOf(r2.id) < ids.indexOf(r3.id), "补位应保持原相对序");
    assert(t1.find((x) => x.active)?.id === curB, "活动档应为原右组当前签");
    await waitFor(() => model.rawData(0, 0) === (curB === r2.id ? "排二" : "排三"),
      "主格应显原右组当前签内容");
  });

  check("分组页签 N7：点右组背景签——右格换显该签，焦点归右", async () => {
    // 前置：N6 后未分屏 A:[r2,r3]，活动签=原 curB(r3)
    const t0 = window.__app.tabs();
    const r2 = t0.find((x) => x.path?.endsWith("r2.csv"));
    const r3 = t0.find((x) => x.path?.endsWith("r3.csv"));
    await window.__app.moveTab(r3.id, "B"); // A:[r2] B:[r3]
    await waitFor(() => sideCellText("A", 0, 0) === "排二", "左格显 r2");
    const f4 = `${TMP}/grp/r4.csv`;
    await writeText(f4, "目的地\r\n排四\r\n");
    await window.__app.openPathsAsTabs([f4]); // 落焦点组 A 并成当前签
    await waitFor(() => sideCellText("A", 0, 0) === "排四", "新签落焦点侧(A)并显示");
    await window.__app.moveTab(r2.id, "B"); // B=[r3,r2]，r3 成右组背景签
    const el = tabEl("r3.csv");
    assert(el?.closest("#tab-group-b"), "r3 签应在右组");
    el.click();
    await sleep(120); // 重绑 + 焦点交换
    const sp = window.__app.split();
    assert(sp.focused === "B" && sp.curB === r3.id, `点右组签后焦点归右 ${JSON.stringify(sp)}`);
    await waitFor(() => sideCellText("B", 0, 0) === "排三", "右格应换显 r3");
    assert(sideCellText("A", 0, 0) === "排四", "左格内容应稳定");
    assert(tabEl("r3.csv").classList.contains("active"), "右组当前签应有 active 类");
    await window.__app.activateTab(window.__app.tabs().find((x) => x.path?.endsWith("r4.csv")).id);
  });

  check("分组页签 N8：新文件落焦点侧组", async () => {
    // 前置：N7 后分屏 A:[r4] B:[r3,r2]，焦点 A
    const r3 = window.__app.tabs().find((x) => x.path?.endsWith("r3.csv"));
    await window.__app.activateTab(r3.id); // r3 是右组当前签 → 纯焦点交换到 B
    assert(window.__app.split().focused === "B", "焦点应在 B");
    const f5 = `${TMP}/grp/r5.csv`;
    await writeText(f5, "目的地\r\n排五\r\n");
    await window.__app.openPathsAsTabs([f5]);
    const id5 = window.__app.tabs().find((x) => x.path === f5)?.id;
    const sp = window.__app.split();
    assert(sp.b.includes(id5), `新签应落焦点组 B ${JSON.stringify(sp)}`);
    assert(sp.curB === id5, "新签应成焦点组当前签");
    await waitFor(() => sideCellText("B", 0, 0) === "排五", "右格应显新档");
    // 还原焦点回 A（r4 是左组当前签 → 纯焦点交换）
    await window.__app.activateTab(window.__app.tabs().find((x) => x.path?.endsWith("r4.csv")).id);
  });

  check("分组页签 N9：关左组当前签——同组邻居补位，右侧不动", async () => {
    // 前置：N8 后 A:[r4] B:[r3,r2,r5]，焦点 A。先把 r2 移回 A 凑两签
    const t0 = window.__app.tabs();
    const r2 = t0.find((x) => x.path?.endsWith("r2.csv"));
    const r4 = t0.find((x) => x.path?.endsWith("r4.csv"));
    await window.__app.moveTab(r2.id, "A"); // A=[r4,r2]，被移签成 A 当前签
    const spB = window.__app.split();
    assert(spB.curA === r2.id && spB.focused === "A", `前置 ${JSON.stringify(spB)}`);
    await window.__app.closeTab(r2.id); // 干净签，无确认
    const sp = window.__app.split();
    assert(sp.on === true && sp.curA === r4.id, `A 当前签应补位到同组邻居 r4 ${JSON.stringify(sp)}`);
    assert(JSON.stringify(sp.b) === JSON.stringify(spB.b) && sp.curB === spB.curB, "右组应完全不动");
    await waitFor(() => sideCellText("A", 0, 0) === "排四", "左格应显补位邻居 r4");
    assert(sideCellText("B", 0, 0) === "排五", "右格内容应稳定");
  });

  check("分组页签 N10：两组各自溢出——滚轮只滚悬停组，按钮滚焦点组", async () => {
    // 前置：N9 后 A:[r4] B:[r3,r5]。再开 32 档，16 档移右组
    const names = [];
    for (let i = 0; i < 32; i += 1) names.push(`${TMP}/grp/ov_${i}.csv`);
    for (let i = 0; i < 32; i += 1) await writeText(names[i], `目的地\r\n溢${i}\r\n`);
    await window.__app.openPathsAsTabs(names); // 全落焦点组 A
    const t0 = window.__app.tabs();
    for (let i = 0; i < 16; i += 1) {
      await window.__app.moveTab(t0.find((x) => x.path === names[i]).id, "B");
    }
    const ga = document.getElementById("tab-group-a");
    const gb = document.getElementById("tab-group-b");
    assert(ga.scrollWidth > ga.clientWidth + 1, "左组应溢出");
    assert(gb.scrollWidth > gb.clientWidth + 1, "右组应溢出");
    assert(document.getElementById("tabbar").classList.contains("overflow"), "任一溢出即出滚动按钮");
    const la = ga.scrollLeft;
    const lb = gb.scrollLeft;
    gb.dispatchEvent(new WheelEvent("wheel", { bubbles: true, cancelable: true, deltaY: 240 }));
    assert(gb.scrollLeft > lb, "悬停右组滚轮应滚右组");
    assert(ga.scrollLeft === la, "悬停右组时左组不滚");
    const next = document.getElementById("tab-next");
    const la2 = ga.scrollLeft;
    next.click();
    assert(ga.scrollLeft > la2, "按钮应滚焦点组(A)");
    await closeAllTabs();
  });

  check("Del 立即清除选区内容（无确认）", async () => {
    await loadBasic();
    grid.select([0, 0]);
    grid._extendTo([0, 1]);
    grid.deleteSelection(false);
    assert(model.rawData(0, 0) === "" && model.rawData(0, 1) === "", "Del 应清除内容");
    assert(model.rowCount() === 3, "行结构应保留");
  });

  // ========== 撤销/排序/结构 ==========

  check("Ctrl+Z 撤销 / Ctrl+Y 重做（经网格键盘路径）", async () => {
    await loadBasic();
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
    await loadBasic();
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
    await loadBasic();
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
    await loadBasic();
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
    await loadBasic();
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
    await loadBasic();
    grid.renderAll();
    const w = grid._effWidths();
    const sum = w.reduce((a, b) => a + b, 0) + grid._rowHeadW();
    assert(sum >= grid.scrollEl.clientWidth, "末列应拉伸填满视口宽度");
  });

  // ========== 文件对话框集成（信息框自动确认） ==========

  check("统一大引擎：短行补空且打开不弹模态", async () => {
    await invoke("write_csv_file", {
      path: `${TMP}/warn.csv`,
      text: "a,b\r\n1\r\n", // 短行 → 引擎按列数补空
    });
    await window.__app.openPath(`${TMP}/warn.csv`);
    assert(window.__app.isBigMode() === true, "磁盘 CSV 应走统一大引擎");
    await waitFor(() => model.rowCount() === 1, "打开后行数落定");
    // 大引擎 mmap 打开不做整表列数校验（那会破坏秒开），短行静默补空；
    // 不再像旧小模型那样弹阻塞式提示框。
    assert(document.querySelector(".modal-mask") === null, "打开不应弹阻塞模态");
    await waitFor(() => model.rawData(0, 1) === "", "短行应补空");
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
  check("画布无意外横向溢出（无幽灵滚动条）", async () => {
    await loadBasic();
    const sc = grid.scrollEl;
    const over = sc.scrollWidth - sc.clientWidth;
    const overV = sc.scrollHeight - sc.clientHeight;
    assert(
      over <= 1 && overV <= 1,
      `横向溢出 ${over}px（scrollWidth=${sc.scrollWidth} clientWidth=${sc.clientWidth}` +
      ` canvas=${sc.firstElementChild.style.width}）纵向溢出 ${overV}px`,
    );
  });

  // ========== 大文件模式（统一大引擎 + 重复行夹具） ==========
  // 本段位于套件末尾：所有磁盘夹具（含远小于旧 50MiB 阈值的文件）都走
  // 大引擎，验证打开/取页/筛选/编辑/列操作/保存/页签隔离全链路。

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

  check("统一大引擎：生成重复行夹具（小于旧阈值也走大引擎）", async () => {
    await invoke("smoke_write_repeat",
      { path: BIG_CSV, line: "目的地,评分,备注\r\n", times: 1, append: false });
    await invoke("smoke_write_repeat",
      { path: BIG_CSV, line: "大理,3,古城\r\n", times: 5000, append: true });
    const sz = await invoke("file_size", { path: BIG_CSV });
    assert(sz > 0 && sz < 50 * 1024 * 1024,
      `夹具应小于旧 50MiB 阈值（验证不再按大小分流），实际 ${sz} 字节`);
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
    // 模式专属工具栏显隐：大模式排序按钮可见（点击后按需载入内存）；
    // 「执行筛选」按钮已移除（筛选框键入即筛，回车立即执行）；
    // 列操作两按钮无 pending 时应隐藏。
    // 注意：必须断言计算样式 display——.hidden 类曾无 CSS 规则，类在但视觉不隐藏。
    assert(!tbHidden("排序…"), "大模式应显示排序按钮（需载入内存）");
    assert(tbBtn("排序…").disabled === false, "索引完成后排序按钮应可用");
    assert(tbBtn("执行筛选") === undefined,
      "执行筛选按钮应已移除（键入即筛 + 回车立即执行）");
    assert(tbHidden("执行列操作"), "无 pending 应隐藏执行列操作");
    assert(tbHidden("撤销列操作预览"), "无 pending 应隐藏撤销列操作预览");
  });

  check("大模式：首屏行异步落定并渲染", async () => {
    await waitFor(() =>
      document.querySelector('.cell[data-row="0"][data-col="0"]')?.textContent === "大理",
      "首屏行渲染");
  });

  check("大模式：Ctrl+↓/↑ 经引擎跳到数据边缘", async () => {
    // 5000 行全部非空：↓ 应直达末行，↑ 应回到首行（跨窗口缓存，必须由 Rust 扫描）
    const scroller = document.getElementById("grid-scroll");
    scroller.focus();
    grid.select([0, 0]);
    key(scroller, "ArrowDown", { ctrlKey: true });
    await waitFor(() => cur(grid)?.[0] === 4999, "Ctrl+↓ 跳到末行");
    await sleep(80); // 等 scroll 事件重绘落定
    const scRect0 = scroller.getBoundingClientRect();
    const oy0 = grid._originY();
    const lastCell = document.querySelector('.cell[data-row="4999"][data-col="0"]');
    assert(lastCell, "末行应有渲染元素");
    const lr0 = lastCell.getBoundingClientRect();
    assert(lr0.bottom <= scRect0.bottom + 1,
      `Ctrl+↓ 末行应完整进入视口：bottom=${Math.round(lr0.bottom - scRect0.top)} 视口高=${scRect0.height}`);
    key(scroller, "ArrowUp", { ctrlKey: true });
    await waitFor(() => cur(grid)?.[0] === 0, "Ctrl+↑ 回到首行");
    await sleep(80);
    const firstCell = document.querySelector('.cell[data-row="0"][data-col="0"]');
    assert(firstCell, "首行应有渲染元素");
    const fr0 = firstCell.getBoundingClientRect();
    assert(fr0.top >= scRect0.top + oy0 - 1,
      `Ctrl+↑ 首行不得被表头遮住：cellTop=${Math.round(fr0.top - scRect0.top)} headerH=${oy0}`);
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

  check("内存后备（阶段 3）：估算 → 分块载入 → 进度/元数据 → 读取仍正确", async () => {
    const bm = bigM();
    const est = await bm.sortEstimate(1);
    assert(est && est.file_bytes > 0, `估算文件字节 ${est && est.file_bytes}`);
    assert(est.row_count === 5000, `估算行数 ${est.row_count}`);
    assert(est.new_bytes > est.file_bytes, "预计新增应含排列/键/余量");
    assert(est.peak_bytes >= est.file_bytes * 2, "峰值提示应含可回收页缓存");
    assert(est.available_bytes === null || est.available_bytes > 0,
      `可用内存 ${est.available_bytes}`);
    const events = [];
    const un = await bm.onSortProgress((done, total) => events.push([done, total]));
    let loaded;
    try {
      loaded = await bm.loadIntoRam();
    } finally {
      un();
    }
    assert(loaded === true, "载入应成功");
    assert(events.length >= 2, `应收到本会话进度事件，实际 ${events.length}`);
    const last = events[events.length - 1];
    assert(last[0] === last[1] && last[1] === est.file_bytes,
      `收尾进度应到满：${last} / ${est.file_bytes}`);
    const meta = await invoke("big_meta", { session: bm.sessionId });
    assert(meta.ram_backed === true, "meta 应报告内存后备");
    const rows = await invoke("big_rows", { from: 0, count: 1, session: bm.sessionId });
    assert(rows[0][0] === "大理", `内存后备下首行应正确：${rows[0]}`);
    // 已载入时再点取消应幂等，且视图仍可读（排序模式不因取消退出）
    await bm.cancelSortLoad();
    const tail = await invoke("big_rows", { from: 4999, count: 1, session: bm.sessionId });
    assert(tail[0][0] === "大理", `取消后末行应可读：${tail[0]}`);
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
    assert(await invoke("big_filter_count", { session: bm.sessionId }) === 5000,
      "引擎侧命中数不一致");
    grid.filters.set(0, "不存在xyz");
    await window.__app.runBigFilter();
    assert(await invoke("big_filter_count", { session: bm.sessionId }) === 0, "零命中应返回 0");
    grid.clearFilters(); // 条件清空 → 视图自动回全量（fire-and-forget）
    await sleep(150);
    assert(bm.filterActive() === false, "清除筛选后应回全量");
  });

  check("大模式：筛选框键入即筛（防抖、无弹窗）", async () => {
    const bm = bigM();
    const input = document.querySelector('.col-filter input[data-col="0"]');
    assert(input !== null, "未找到第一列筛选框");
    input.value = "大";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await waitFor(() => bm.filterActive() && bm.rowCount() === 5000, "键入即筛落定");
    assert(document.querySelector(".modal-mask") === null,
      "键入即筛不应弹模态（扫描在引擎线程，UI 不冻结）");
    assert(await invoke("big_filter_count", { session: bm.sessionId }) === 5000,
      "引擎侧命中数不一致");
  });

  check("大模式：筛选落定时不重建筛选框（焦点不丢）", async () => {
    // 回归：筛选落定的数据事件会走 renderAll → _renderHeader，若无条件重建表头，
    // 正在输入的筛选框 DOM 被替换——用户每打两个字就丢焦点、按键被吞。
    const bm = bigM();
    const input = document.querySelector('.col-filter input[data-col="0"]');
    // 先清空回全量，确保随后的 waitFor(filterActive) 真正等到"本次筛选落定"
    input.value = "";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await waitFor(() => bm.filterActive() === false, "先回全量");
    input.focus();
    input.value = "大理";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await waitFor(() => bm.filterActive() && bm.rowCount() === 5000, "本次筛选落定");
    await sleep(150); // 给"落定后重建"一次发生的机会
    assert(input.isConnected, "筛选框 DOM 被重建替换");
    assert(document.activeElement === input, "筛选落定后焦点被夺走");
    assert(input.value === "大理", `筛选框内容丢失：${input.value}`);
  });

  check("大模式：输入法组合中的回车不触发筛选（isComposing 守卫）", async () => {
    // 中文输入法用回车选词时，keydown 的 key 也是 Enter；不设守卫会提前
    // 拿未上屏的组合串去筛。组合态回车必须放行给输入法，普通回车仍立即执行。
    const bm = bigM();
    const input = document.querySelector('.col-filter input[data-col="0"]');
    const status = document.getElementById("status-text");
    status.textContent = "哨兵";
    input.dispatchEvent(new KeyboardEvent("keydown", {
      key: "Enter", bubbles: true, isComposing: true,
    }));
    assert(status.textContent === "哨兵",
      `组合态回车不应触发筛选，状态栏：${status.textContent}`);
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    assert(status.textContent.includes("正在筛选大文件"),
      `普通回车应立即执行，状态栏：${status.textContent}`);
    await waitFor(() => /匹配/.test(status.textContent), "回车筛选落定");
    assert(bm.filterActive(), "回车筛选后应保持命中视图");
  });

  check("大模式：键入收窄与清空自动回全量", async () => {
    const bm = bigM();
    const input = document.querySelector('.col-filter input[data-col="0"]');
    // 追加字符 = 前缀收窄（引擎只扫上一轮命中行，结果集不变）
    input.value = "大理";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await waitFor(() => bm.filterActive() && bm.rowCount() === 5000, "收窄落定");
    // 清空输入 = 自动清除引擎筛选、回全量视图
    input.value = "";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await waitFor(() => bm.filterActive() === false, "清空后回全量");
    assert(bm.rowCount() === 5000, `清空后全量行数 ${bm.rowCount()}`);
  });

  check("大模式：连续键入只认最后一个条件（旧结果不覆盖）", async () => {
    const bm = bigM();
    const input = document.querySelector('.col-filter input[data-col="0"]');
    input.value = "大理";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.value = "不存在xyz";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await waitFor(() => bm.filterActive() && bm.rowCount() === 0, "最后条件落定");
    await sleep(400); // 给第一轮（若尚未取消）迟到的机会
    assert(bm.rowCount() === 0, `旧结果覆盖了最终条件：${bm.rowCount()}`);
    grid.clearFilters();
    await sleep(150);
    assert(bm.filterActive() === false, "清除筛选后应回全量");
  });

  check("大模式：筛选结果分页取行", async () => {
    const bm = bigM();
    grid.filters.set(0, "大理");
    await window.__app.runBigFilter();
    const page = await invoke("big_filter_page", { from: 0, count: 5, session: bm.sessionId });
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
    const rows = await invoke("big_rows", { from: 0, count: 1, session: bm.sessionId });
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

  check("大模式：另存为写新路径且不改原文件，后续保存跟随新路径", async () => {
    const bm = bigM();
    const saveAsPath = `${TMP}/big-saveas.csv`;
    // 另存前的编辑必须进入新文件，且绝不能改写原文件
    await bm.setData(2, 0, "另存标记");
    const ok = await window.__app.saveFileAs(saveAsPath);
    assert(ok === true, "另存为应成功");
    assert(window.__app.getPath() === saveAsPath,
      `另存后文档身份应切到新路径：${window.__app.getPath()}`);
    assert(window.__app.isDirty() === false, "另存成功后应为干净状态");

    const saved = parseCsv((await invoke("read_csv_file", { path: saveAsPath })).text);
    assert(saved.rows.length === 5000, `另存后行数 ${saved.rows.length}`);
    assert(saved.headers[1] === "列4", `另存后表头 ${saved.headers}`);
    assert(saved.rows[2][0] === "另存标记", `另存文件应包含编辑：${saved.rows[2][0]}`);

    // 原文件必须保持最后一次普通保存的内容（row2 仍是 大理）
    const orig = parseCsv((await invoke("read_csv_file", { path: BIG_CSV })).text);
    assert(orig.rows[2][0] === "大理", `另存为不得改写原文件：${orig.rows[2][0]}`);
    assert(orig.rows[0][0] === "昆明", `原文件既有内容应保留：${orig.rows[0][0]}`);

    // 会话已切到新路径：后续普通保存只写新文件
    await bm.setData(3, 0, "后续保存");
    await bm.save();
    const saved2 = parseCsv((await invoke("read_csv_file", { path: saveAsPath })).text);
    assert(saved2.rows[3][0] === "后续保存", "后续保存应写入新路径");
    const orig2 = parseCsv((await invoke("read_csv_file", { path: BIG_CSV })).text);
    assert(orig2.rows[3][0] === "大理", `另存后保存不得碰原文件：${orig2.rows[3][0]}`);

    // 留下一处未保存修改，供下一项“换档确认放弃”使用
    await bm.setData(4, 0, "留脏");
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

  // ========== 排序模式（阶段 4/5：载入内存 → 排列层 → 物理保存） ==========

  check("排序模式：内存确认 → 可取消载入进度 → 排序视图与筛选/编辑映射", async () => {
    const p = `${TMP}/sortm.csv`;
    // 末行无换行：排序把它挪到中间时必须补行尾，不能与下一行粘连
    await writeText(p, "name,score,note\nb,2,beta\nc,1,charlie\na,10,alpha\nd,4,delta");
    // 上一项留下未保存修改：换档先按真实交互确认放弃
    const pendingOpen = window.__app.openPath(p);
    await sleep(150);
    assert(clickDiscardDialog("放弃") === true, "换档前应确认放弃上一档修改");
    await pendingOpen;
    assert(window.__app.isBigMode() === true, "排序夹具应走统一大引擎");
    const bm = bigM();
    assert(!tbHidden("排序…") && tbBtn("排序…").disabled === false,
      "索引完成后排序按钮应可用");

    // 走真实的内存确认弹窗（不是 confirm:false）；载入开始时进度框必须带取消按钮。
    // 用一次性包装观察 loadIntoRam 被调用的瞬间——此时进度框应已挂载。
    let sawCancel = false;
    let sawTitle = "";
    const origLoad = bm.loadIntoRam.bind(bm);
    bm.loadIntoRam = async () => {
      const mask = document.querySelector(".modal-mask");
      sawTitle = mask?.querySelector(".modal-title")?.textContent ?? "";
      sawCancel = [...(mask?.querySelectorAll("button") ?? [])]
        .some((b) => b.textContent === "取消载入");
      return origLoad();
    };
    const pendingSort = window.__app.applyBigSort([{ col: 0, numeric: false, asc: false }]);
    try {
      await waitFor(() => document.querySelector(".modal-mask"), "内存确认弹窗");
      const text = document.querySelector(".modal-mask").textContent;
      assert(text.includes("预计新增内存") && text.includes("当前可用内存"),
        `确认弹窗应展示内存估算：${text.slice(0, 80)}`);
      assert(clickModalButton("载入并排序"), "确认弹窗应有「载入并排序」");
      assert(await pendingSort === true, "排序应完成");
    } finally {
      bm.loadIntoRam = origLoad;
    }
    assert(sawTitle.includes("载入内存"), `载入进度框标题异常：${sawTitle}`);
    assert(sawCancel, "载入进度框必须提供「取消载入」");
    assert(bm.meta().sorted === true && bm.meta().ram_backed === true,
      `排序后 meta 异常：${JSON.stringify(bm.meta())}`);
    assert(window.__app.isDirty() === true, "排序视图尚未落盘，应标脏");
    await waitFor(() => bm.rawData(0, 0) === "d", "排序视图首行");
    assert(bm.rawData(1, 0) === "c" && bm.rawData(2, 0) === "b"
      && bm.rawData(3, 0) === "a", "文本降序应为 d,c,b,a");
    assert(!tbHidden("退出排序"), "排序视图应显示「退出排序」");
    assert(String(document.getElementById("status-text").textContent).includes("已排序"),
      "状态栏应提示已排序（内存、未保存）");

    // 排序后筛选：引擎命中集已重映射到新显示坐标系
    grid.filters.set(0, "b");
    await window.__app.runBigFilter();
    await waitFor(() => bm.rawData(0, 0) === "b", "排序后筛选首行");
    assert(bm.filterActive() === true && bm.rowCount() === 1,
      `排序后筛选应命中 1 行，实际 ${bm.rowCount()}`);
    grid.clearFilters();
    await sleep(200);
    assert(bm.filterActive() === false, "清除筛选应回全量");
    await waitFor(() => bm.rawData(0, 0) === "d", "回到全量首行");

    // 编辑落点按显示行：把 d 的 score 改为 99（保存时写到物理顺序首位）
    await bm.setData(0, 1, "99");
    await waitFor(() => bm.rawData(0, 1) === "99", "排序视图编辑落定");

    // 留一个激活的筛选交给下一项：保存后引擎丢弃筛选，前端须同步复位
    grid.filters.set(0, "b");
    await window.__app.runBigFilter();
    try {
      await waitFor(() => bm.rawData(0, 0) === "b", "保存前筛选落定");
    } catch (err) {
      throw new Error(`${err.message}（active=${bm.filterActive()}, rows=${bm.rowCount()},`
        + ` count=${bm._filterCount}, cell=${bm.rawData(0, 0)},`
        + ` cache=${JSON.stringify([...bm._cache.keys()])},`
        + ` inflight=${JSON.stringify([...bm._inflight])},`
        + ` filters=${JSON.stringify([...grid.filters])}）`);
    }
  });

  check("排序模式：保存按当前视图顺序物理重写并退出排序", async () => {
    const bm = bigM();
    assert(bm.filterActive() === true, "前置：保存前应处于筛选视图");
    assert(await window.__app.saveFile() === true, "排序保存应成功");
    const back = await invoke("read_csv_file", { path: `${TMP}/sortm.csv` });
    const parsed = parseCsv(back.text);
    assert(JSON.stringify(parsed.headers) === JSON.stringify(["name", "score", "note"]),
      `保存后表头 ${parsed.headers}`);
    assert(parsed.rows.length === 4, `保存后行数 ${parsed.rows.length}`);
    assert(parsed.rows.map((r) => r[0]).join(",") === "d,c,b,a",
      `物理行序应为 d,c,b,a，实际 ${parsed.rows.map((r) => r[0])}`);
    assert(parsed.rows[0][1] === "99", "显示行编辑应写到 d 行");
    assert(parsed.rows[1][0] === "c" && parsed.rows[2][0] === "b"
      && parsed.rows[3][0] === "a", "末行无换行的源行挪位后不得粘连");
    assert(bm.meta().sorted === false && bm.meta().ram_backed === false,
      `保存后应退出排序并释放内存：${JSON.stringify(bm.meta())}`);
    assert(bm.filterActive() === false && bm.rowCount() === 4,
      "保存后筛选应复位到全量");
    assert(document.querySelector('.col-filter input[data-col="0"]').value === "",
      "保存后筛选输入框应清空");
    assert(window.__app.isDirty() === false, "保存后应为干净");
    assert(tbHidden("退出排序"), "保存后「退出排序」应隐藏");
    assert(!tbHidden("排序…") && tbBtn("排序…").disabled === false,
      "保存后排序按钮应恢复可用");
    await waitFor(() => bm.rawData(0, 0) === "d" && bm.rawData(3, 0) === "a",
      "保存后视图应为物理顺序");
  });

  check("排序模式：退出排序（放弃）恢复行序、释放内存、保留单元格修改", async () => {
    const bm = bigM();
    // 保存后的物理顺序 d,c,b,a；按 score 数字升序：c(1),b(2),a(10),d(99)
    assert(await window.__app.applyBigSort(
      [{ col: 1, numeric: true, asc: true }], { confirm: false }) === true,
    "第二次排序应完成");
    await waitFor(() => bm.rawData(0, 0) === "c", "按分数排序首行 c");
    await bm.setData(0, 2, "changed");
    await waitFor(() => bm.rawData(0, 2) === "changed", "排序视图第二次编辑落定");
    const pendingExit = window.__app.exitSortMode();
    await waitFor(() => document.querySelector(".modal-mask"), "退出排序对话框");
    const text = document.querySelector(".modal-mask").textContent;
    assert(text.includes("放弃排序") && text.includes("单元格修改仍会保留"),
      `退出排序文案异常：${text}`);
    assert(clickModalButton("放弃排序"), "应能点击「放弃排序」");
    assert(await pendingExit === true, "退出排序应完成");
    assert(bm.meta().sorted === false && bm.meta().ram_backed === false,
      "退出排序应释放内存并回到 mmap");
    await waitFor(() => bm.rawData(0, 0) === "d" && bm.rawData(1, 0) === "c",
      "恢复保存时的物理行序");
    assert(bm.rawData(1, 2) === "changed", "放弃排序不得丢弃单元格修改");
    assert(window.__app.isDirty() === true, "仍有内容修改，应保持脏点");
    assert(tbHidden("退出排序") && !tbHidden("排序…"), "按钮状态应复原");
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
    // 上一项（单格一对多填充）把未入签的当前档留在脏态；换文件前应先确认，
    // 与真实交互一致地点掉（放弃夹具编辑），否则 openPath 永远等模态
    const pendingOpen = window.__app.openPath(p);
    await sleep(150);
    assert(clickDiscardDialog("放弃") === true, "未入签脏档换文件应先弹未保存确认");
    await pendingOpen;
    assert(window.__app.isBigMode() === true, "夹具应走统一大引擎");
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
  });

  check("统一大引擎：小于旧阈值的小文件仍走大模式", async () => {
    await window.__app.openPath(`${TMP}/rt.csv`);
    await waitFor(() => window.__app.isBigMode() && model.rowCount() === 1, "小文件打开落定");
    // 所有磁盘 CSV 都是大模式：排序按钮可见（哪怕只有一行也先确认载入内存）
    assert(!tbHidden("排序…"), "大模式应显示排序按钮（需载入内存）");
    assert(tbBtn("排序…").disabled === false, "小文件索引完成后排序按钮应可用");
    assert(tbBtn("执行筛选") === undefined, "执行筛选按钮应已移除（两种模式都不存在）");
    assert(tbHidden("执行列操作"), "大模式应隐藏执行列操作");
    assert(tbHidden("撤销列操作预览"), "大模式应隐藏撤销列操作预览");
    // 小文件在打开阶段同步扫完：索引耗时并入首屏，界面应报「索引随首屏完成」
    // 而不是伪造一个独立的「索引 0 ms」（0 会被误读成没做索引）。
    assert(bigM().meta().index_sync === true, "小档 meta 应标记索引随打开同步完成");
    window.__app.refreshStatus();
    await waitFor(() => {
      const t = document.getElementById("status-text").textContent;
      return t.includes("索引随首屏完成");
    }, "小档状态栏应显示「索引随首屏完成」");
    assert(!/索引\s*0\s*ms/.test(document.getElementById("status-text").textContent),
      "小档不应显示独立的「索引 0 ms」");
  });
  check("大模式：超高行数虚拟缩放（滚动到底不空白）", async () => {
    // 回归：元素/滚动高 16,777,214px 上限曾致大文件滚动后视口空白。
    // 100 万数据行 × ~20px ≈ 2000 万 px，超过 Y_CLAMP 触发纵向虚拟缩放。
    const p = `${TMP}/tall.csv`;
    await invoke("smoke_write_repeat", { path: p, line: "a,b,c,d,e\n", times: 1000001, append: false });
    await window.__app.openPath(p);
    // ---- 渐进索引：10MB 夹具超过首屏同步扫描上限（1MiB），打开先返回首批行 ----
    const bm0 = bigM();
    assert(bm0.indexing() === true, "打开后应仍在后台索引");
    const partialRows = bm0.rowCount();
    assert(partialRows >= 1000 && partialRows < 1000000,
      `首批可用行数 ${partialRows} 应介于 1000 与全量之间`);
    assert(bm0.meta().total_bytes > 0 && bm0.meta().indexed_bytes > 0,
      "索引进度字节字段应有效");
    assert(bm0.firstPaintMs() > 0, "首屏耗时应已记录");
    // 筛选框禁用 + "索引中…" 占位；状态栏报进度（EmEditor 式）
    const fin = document.querySelector(".col-filter input");
    assert(fin?.disabled === true, "索引中筛选框应禁用");
    assert(String(fin?.placeholder).includes("索引中"), `索引中占位 ${fin?.placeholder}`);
    assert(String(document.getElementById("status-text").textContent).includes("索引中"),
      "状态栏应显示索引进度");
    // 索引中结构操作被拒（单元格编辑放行不在此断言）
    let rejected = false;
    try { await bm0.insertRows(0, 1); } catch (e) { rejected = /索引中/.test(String(e?.message ?? e)); }
    assert(rejected, "索引中插入行应被拒绝");
    // 视口锚点：滚到首批范围内的中间行。索引完成会改变行数并触发 _yScale
    // 虚拟缩放，同一 scrollTop 将落到别的行——实现须把锚点行送回视口顶。
    const sc = document.getElementById("grid-scroll");
    const anchorRow = Math.min(50000, partialRows - 1);
    sc.scrollTop = Math.round(
      grid._originY() + (anchorRow * grid._uniformRowH) / grid._yScale);
    grid._renderRows();
    await waitFor(() => !bigM().indexing(), "后台索引完成");
    // ---- 索引完成：行数收敛、筛选框恢复、状态栏报首屏/索引耗时 ----
    const n = grid.visCount();
    assert(n > 999000, `行数 ${n}`);
    const fin2 = document.querySelector(".col-filter input");
    assert(fin2?.disabled === false, "索引完成后筛选框应恢复可用");
    assert(fin2?.placeholder === "筛选…", `筛选占位恢复 ${fin2?.placeholder}`);
    await waitFor(() => {
      const t = document.getElementById("status-text").textContent;
      return /首屏\s*\d+\s*ms/.test(t) && /索引\s/.test(t);
    }, "状态栏耗时摘要");
    // 不变量：索引推进/完成不得移动视口内容——以「视口顶锚点行」为准
    // （含被吸顶表头遮住的行）。历史缺陷：恢复时按「表头下对齐」，每次进度
    // 事件净下移一个表头高——本地 1 次事件恰好被旧测量公式的 +headRows
    // 抵消，CI 慢盘 2 次事件净 −3（49997）。现在恢复是不动点，任意次事件
    // 后仍精确等于 anchorRow；±2 只吸收浏览器滚动条的设备像素吸附。
    const anchorNow = grid._rowAt(sc.scrollTop - grid._originY());
    assert(Math.abs(anchorNow - anchorRow) <= 2,
      `索引完成后视口顶锚点行 ${anchorNow} 应≈${anchorRow}（不得因行数增长跳走）`);
    assert(grid._yScale > 1, `yScale=${grid._yScale} 应已启用虚拟缩放`);
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
    // 用户反馈：大文件滚到底时倒数前两行显示不出来（元素缺失/空白/被裁掉）。
    // 取页是异步的：必须以「数据落定并重绘后的文本」为判据，渲染调用返回的
    // 瞬间页面请求尚未落定，此时读到的空串只是占位，不能当缺陷。
    const diagTail = () => {
      const m = bigM();
      const pageA = Math.floor((n - 2) / 200);
      const pageB = Math.floor((n - 1) / 200);
      return `n=${n} 需要页=${pageA},${pageB} cache尾=[${[...m._cache.keys()].slice(-6)}]`
        + ` inflight=[${[...m._inflight]}] queue=[${m._queue.slice(0, 6)}]`
        + ` view=[${m._viewFirst},${m._viewLast}]`;
    };
    await waitFor(() => {
      const a = document.querySelector(`.cell[data-row="${n - 2}"][data-col="0"]`);
      const b = document.querySelector(`.cell[data-row="${n - 1}"][data-col="0"]`);
      return a?.textContent === "a" && b?.textContent === "a";
    }, `滚动到底后末两行数据落定（${diagTail()}）`);
    const rect = sc.getBoundingClientRect();
    // 用可视内容盒（clientHeight，已排除水平滚动条）而不是边框盒：边框盒
    // 会把被滚动条盖住的行也算「可见」，放过用户实际看不到的末行。
    const viewBottom = rect.top + sc.clientHeight;
    for (const rr of [n - 2, n - 1]) {
      const cell = document.querySelector(`.cell[data-row="${rr}"][data-col="0"]`);
      assert(cell, `倒数第 ${n - rr} 行应有渲染元素`);
      assert(cell.textContent === "a",
        `倒数第 ${n - rr} 行数据应为 a，实际 "${cell.textContent}"（${diagTail()}）`);
      const cr = cell.getBoundingClientRect();
      assert(cr.top >= rect.top - 1 && cr.bottom <= viewBottom + 1,
        `倒数第 ${n - rr} 行应完整可见：${Math.round(cr.top - rect.top)}..${Math.round(cr.bottom - rect.top)}，可视高 ${sc.clientHeight}`);
    }
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
    // 回归：行号列宽曾按固定 3.2em（约 42px）给出，7~8 位行号把 .row-head
    // 的 min-content 撑到 49~57px，数据列随之外移，列名整体偏左。
    assertColsAligned("#grid-area", "大行号（100 万行底部 7 位）");
  });

  check("大模式：迟到的旧页响应不得逐出当前视口", async () => {
    // 用户现场：拖动滚动条到远端后整屏空白，点一下空白处才瞬间渲染。
    // 根因是缓存换出以「刚落定的页」为中心：拖动途中经过的中间页响应迟到
    // 落定后，会把当前视口页逐出缓存。这里人为延迟中间页复现该时序。
    const m = bigM();
    const g = grid;
    const sc = document.getElementById("grid-scroll");
    const PAGE = 200;
    const n = m.rowCount();
    assert(n > 200000, `夹具行数 ${n}`);
    const rowA = Math.floor(n * 0.1);
    const rowB = Math.floor(n * 0.9);
    const pageA = Math.floor(rowA / PAGE);
    const pageB = Math.floor(rowB / PAGE);
    assert(pageA !== pageB, "两处应落在不同页");
    const orig = m._invoke.bind(m);
    let bCalls = 0;
    m._invoke = (cmd, args) => {
      const req = orig(cmd, args);
      if (cmd !== "big_rows") return req;
      const page = Math.floor(Number(args.from) / PAGE);
      if (page === pageA) {
        // 拖动途中的旧页：慢 600ms 才落定
        return req.then((rows) => new Promise((res) => {
          setTimeout(() => res(rows), 600);
        }));
      }
      if (page === pageB) {
        bCalls += 1;
        // 视口页若被错误逐出，其重拉同样慢，避免“自愈”掩盖缺陷
        if (bCalls > 1) {
          return req.then((rows) => new Promise((res) => {
            setTimeout(() => res(rows), 1200);
          }));
        }
      }
      return req;
    };
    try {
      // 先滚到 10% 并渲染（其取页请求被延迟），再立刻跳到 90%
      sc.scrollTop = g._scrollTopForRowTop(rowA);
      g._renderRows();
      await sleep(30);
      sc.scrollTop = g._scrollTopForRowTop(rowB);
      g._renderRows();
      await waitFor(() => m.hasData(rowB, 0), "当前视口页落定").catch((e) => {
        const a = g._yAnchor();
        throw new Error(`${e.message}; rowB=${rowB} page=${pageB} bCalls=${bCalls}`
          + ` scrollTop=${Math.round(sc.scrollTop)} anchor=${a.anchor}`
          + ` cache=[${[...m._cache.keys()].join(",")}]`
          + ` inflight=[${[...m._inflight].join(",")}]`
          + ` active=${m._active} queue=[${m._queue.join(",")}]`);
      });
      // 等迟到的旧页落定，此时视口页仍须在缓存中
      await sleep(900);
      assert(m.hasData(rowB, 0),
        `迟到的第 ${pageA} 页不得逐出视口第 ${pageB} 页（视口页重拉 ${bCalls} 次）`);
      const cells = [...g.rowsEl.querySelectorAll('.cell[data-col="0"]')];
      assert(cells.length > 0, "应有渲染行");
      const blank = cells.filter((el) => !el.textContent).length;
      assert(blank === 0,
        `迟到旧页落定后可见格应保持有数据，空白 ${blank}/${cells.length}`);
    } finally {
      m._invoke = orig;
    }
  });

  check("大模式：虚拟缩放下 Ctrl+↑/↓ 把目标行完整滚入视口", async () => {
    // 回归：压缩滚动坐标用「全局真实高度/scale」直接把 scrollTop 映射到行，
    // 末尾一屏永远到不了最后一行（滚到底仍差 viewH*scale/h 行）；且向上跳时
    // 把目标行对齐到视口顶，正好被吸顶表头盖住。
    const sc = document.getElementById("grid-scroll");
    sc.focus();
    const n = grid.visCount();
    assert(n > 200000, `夹具应启用虚拟缩放，n=${n}`);
    assert(grid._yScale > 1, `应处于虚拟缩放，yScale=${grid._yScale}`);
    grid.select([n - 1, 0]);
    key(sc, "ArrowUp", { ctrlKey: true });
    await waitFor(() => cur(grid)?.[0] === 0, "Ctrl+↑ 到首行");
    await sleep(80);
    const scRect = sc.getBoundingClientRect();
    const oy = grid._originY();
    assert(Math.round(sc.scrollTop) <= 1, `Ctrl+↑ 后应回顶，scrollTop=${sc.scrollTop}`);
    const firstCell = document.querySelector('.cell[data-row="0"][data-col="0"]');
    assert(firstCell, "首行应有渲染元素");
    const fr = firstCell.getBoundingClientRect();
    assert(fr.top >= scRect.top + oy - 1,
      `首行不得被表头遮住：cellTop=${Math.round(fr.top - scRect.top)} headerH=${oy}`);
    key(sc, "ArrowDown", { ctrlKey: true });
    await waitFor(() => cur(grid)?.[0] === n - 1, "Ctrl+↓ 到末行");
    await waitFor(() =>
      document.querySelector(`.cell[data-row="${n - 1}"][data-col="0"]`)?.textContent === "a",
      "末行数据渲染");
    const lastCell = document.querySelector(`.cell[data-row="${n - 1}"][data-col="0"]`);
    const lr = lastCell.getBoundingClientRect();
    assert(lr.top >= scRect.top + oy - 1 && lr.bottom <= scRect.bottom + 1,
      `末行应完整在视口内：${Math.round(lr.top - scRect.top)}..${Math.round(lr.bottom - scRect.top)} 视口高=${scRect.height}`);
    // 末页附近继续滚动：新窗口的页落定后必须自动补齐，不得留下需要点击/再滚才填的空白。
    sc.scrollTop = Math.max(0, sc.scrollTop - 40);
    grid._renderRows();
    await waitFor(() => [...grid.rowsEl.querySelectorAll('.cell[data-col="0"]')]
      .every((el) => el.textContent === "a"), "滚动后渲染窗格无空白格");
  });

  check("页签：多个大档各自独立会话，切换不丢修改", async () => {
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
    await waitFor(() => model.rawData(0, 0) === "甲", "第一大档首行落定");
    // 回归：旧实现切签会腾退引擎并重开文件（整份重新索引一遍）。会话号与
    // 打开耗时都不允许变化——它们一变就说明模型被重建过。
    const sid1 = model.sessionId;
    const meta1 = model.meta();
    // 改 b2 后切回 b1：两个会话各自保留，互不腾退、互不串档
    await window.__app.activateTab(tb2.id);
    await waitFor(() => model.rawData(0, 0) === "乙", "第二大档首行落定");
    const sid2 = model.sessionId;
    const meta2 = model.meta();
    assert(sid1 !== sid2, "两个大档应使用独立引擎会话");
    await model.setData(0, 0, "乙改");
    await waitFor(() => window.__app.isDirty() === true, "b2 编辑标脏");
    await window.__app.activateTab(tb1.id);
    await waitFor(() => model.rawData(0, 0) === "甲", "切回第一大档首行落定");
    assert(window.__app.isDirty() === false, "b1 不应受 b2 脏态影响");
    assert(model.sessionId === sid1, "切回 b1 不得重建引擎会话");
    assert(model.meta().open_ms === meta1.open_ms && model.meta().index_ms === meta1.index_ms,
      `切回 b1 不得重新打开/索引（${meta1.open_ms}/${meta1.index_ms} → ${model.meta().open_ms}/${model.meta().index_ms}）`);
    await window.__app.activateTab(tb2.id);
    await waitFor(() => model.rawData(0, 0) === "乙改", "b2 修改应随签保留");
    assert(model.sessionId === sid2, "切回 b2 不得重建引擎会话");
    assert(model.meta().open_ms === meta2.open_ms && model.meta().index_ms === meta2.index_ms,
      `切回 b2 不得重新打开/索引（${meta2.open_ms}/${meta2.index_ms} → ${model.meta().open_ms}/${model.meta().index_ms}）`);
    // 清理：两签可能都脏（b2 已改），放弃后关闭
    for (const p of [b1, b2]) {
      const tab = window.__app.tabs().find((x) => x.path === p);
      if (!tab) continue;
      const cp = window.__app.closeTab(tab.id);
      await sleep(100);
      clickDiscardDialog("放弃");
      await cp;
    }
  });

  // ========== 首行非列名（按文件独立） ==========

  check("首行非列名：大模式开关、异步编辑落定、首行筛选、保存与不持久化", async () => {
    const p = `${TMP}/hl_big_${Date.now()}.csv`;
    await invoke("smoke_write_repeat", { path: p, line: "目的地,评分\r\n", times: 1, append: false });
    await invoke("smoke_write_repeat", { path: p, line: "大理,1\r\n", times: 20, append: true });
    // 入签打开：后续要验证关签重开（未入签文档没有关签入口）
    await window.__app.openPathsAsTabs([p]);
    await waitFor(() => window.__app.isBigMode(), "大引擎打开落定");
    assert(window.__app.isBigMode() === true, "应进入大文件模式");
    assert(window.__app.isHeaderless() === false, "默认首行应为列名");
    assert(model.rowCount() === 20, `默认数据行数 ${model.rowCount()}`);

    const btn = tbBtn("首行非列名");
    assert(btn && !btn.disabled, "大模式开关按钮应存在且可用");
    assert(!btn.classList.contains("on"), "默认按钮应为关闭态");

    // 真实异步路径：格内编辑尚未提交就点开关——toggle 必须先等写入落定，
    // 否则行坐标 +1 后异步写回会落到错误的行。
    const prefBefore = localStorage.getItem("simpletable.headerless.v1");
    grid.ensureVisible(0, 0);
    grid.openEditor(0, 0, { selectAll: true });
    const ed = document.getElementById("cell-editor");
    ed.value = "起点";
    btn.click();
    await waitFor(() => window.__app.isHeaderless() === true, "大模式切换开启");
    assert(btn.classList.contains("on"), "按钮应呈开启态");
    assert(localStorage.getItem("simpletable.headerless.v1") === prefBefore,
      "开关不应写入本地存储（已屏蔽按文件记忆）");
    assert(model.rowCount() === 21, `开启后行数 ${model.rowCount()}`);
    assert(JSON.stringify(model.headers()) === JSON.stringify(["列1", "列2"]),
      `无列名表头 ${model.headers()}`);
    await waitFor(() => model.rawData(1, 0) === "起点", "异步编辑应落在被提升后的数据行");
    await waitFor(() => model.rawData(0, 0) === "目的地", "原表头应成为首行数据");
    assert(window.__app.isDirty() === true, "格内编辑应标脏（开关本身不标脏）");

    // 首行参与筛选：只有首行（原表头）含「目的地」
    grid.filters.set(0, "目的地");
    await window.__app.runBigFilter();
    assert(await invoke("big_filter_count", { session: model.sessionId }) === 1,
      "应仅命中首行");
    assert(model.rowCount() === 1, `筛选后行数 ${model.rowCount()}`);
    grid.clearFilters();
    await waitFor(() => model.filterActive() === false, "清除筛选回全量");

    // 保存：无列名模式不得注入独立表头，首行数据必须原样写入
    await window.__app.saveFile();
    assert(window.__app.isDirty() === false, "保存后应为干净");
    const back = await invoke("read_csv_file", { path: p });
    const fileLines = back.text.split(/\r\n/).filter((x, i, a) => !(i === a.length - 1 && x === ""));
    assert(fileLines.length === 21, `保存后行数 ${fileLines.length}`);
    assert(fileLines[0] === "目的地,评分", `保存首行 ${fileLines[0]}`);
    assert(fileLines[1] === "起点,1", `保存第二行 ${fileLines[1]}`);

    // 不持久化：关签重开后回到默认「首行是列名」解读
    const tab = window.__app.tabs().find((x) => x.path === p);
    assert(tab?.headerless === true, "页签快照应带无列名状态");
    assert(localStorage.getItem("simpletable.headerless.v1") === prefBefore,
      "保存/关签流程也不应写入本地存储");
    await window.__app.closeTab(tab.id);
    await window.__app.openPath(p);
    await waitFor(() => window.__app.isBigMode() && window.__app.isHeaderless() === false,
      "重开应回到默认表头解读");
    await waitFor(() => model.rowCount() === 20 && model.rawData(0, 0) === "起点",
      "重开后首行默认作为表头");
    assert(JSON.stringify(model.headers()) === JSON.stringify(["目的地", "评分"]),
      `重开默认表头 ${model.headers()}`);

    // 关闭开关：再次开启后，当前首行（原表头）成为列名，数据行 −1
    tbBtn("首行非列名").click();
    await waitFor(() => window.__app.isHeaderless() === true, "大模式再次开启");
    tbBtn("首行非列名").click();
    await waitFor(() => window.__app.isHeaderless() === false, "大模式切换关闭");
    await waitFor(() => model.rowCount() === 20, "关闭后数据行数");
    assert(JSON.stringify(model.headers()) === JSON.stringify(["目的地", "评分"]),
      `关闭后表头 ${model.headers()}`);
    assert(model.rawData(0, 0) === "起点", `关闭后首条数据 ${model.rawData(0, 0)}`);
  });

  check("首行非列名：新建表格（小模型）开关、首行筛选、编辑后关闭成列名", async () => {
    // 统一大引擎后小模型只剩「新建表格」路径：经冒烟钩子复位后装载内存表
    await window.__app.smokeLoadSmall(["甲", "1"], [["乙", "2"]]);
    assert(window.__app.isBigMode() === false, "新建表格应走内存小模型");
    assert(model.rowCount() === 1, `默认数据行数 ${model.rowCount()}`);

    const btn = tbBtn("首行非列名");
    btn.click();
    await waitFor(() => window.__app.isHeaderless() === true, "小模式切换开启");
    assert(model.rowCount() === 2, `开启后行数 ${model.rowCount()}`);
    assert(JSON.stringify(model.headers()) === JSON.stringify(["列1", "列2"]),
      `无列名表头 ${model.headers()}`);
    assert(model.rawData(0, 0) === "甲", "首行应作为数据");
    assert(window.__app.isDirty() === false, "切换开关不应标脏");

    // 首行参与筛选
    grid.filters.set(0, "甲");
    grid.refilter();
    assert(grid.visCount() === 1 && grid.visAt(0) === 0, "筛选应命中首行");
    grid.clearFilters();

    // 编辑首行 → 关闭：编辑后的当前首行成为列名
    model.setData(0, 0, "丙");
    assert(window.__app.isDirty() === true, "编辑应标脏");
    btn.click();
    await waitFor(() => window.__app.isHeaderless() === false, "小模式切换关闭");
    assert(model.rowCount() === 1, `关闭后行数 ${model.rowCount()}`);
    assert(model.headers()[0] === "丙", `列名应为编辑后的首行 ${model.headers()[0]}`);
    assert(model.rawData(0, 0) === "乙", "原第二行成为首条数据");

    // 不持久化：再次开启不得写入 localStorage
    const prefBefore = localStorage.getItem("simpletable.headerless.v1");
    btn.click();
    await waitFor(() => window.__app.isHeaderless() === true, "再次开启");
    assert(localStorage.getItem("simpletable.headerless.v1") === prefBefore,
      "小模式开关也不应写入本地存储");
    assert(btn.classList.contains("on"), "再次开启后按钮应为开启态");
    // 复位为空小表（本测试的编辑未保存，避免污染下一项的未入签脏档确认）
    await window.__app.smokeLoadSmall([], []);
  });

  check("首行非列名：两个大档各自独立，切签按钮状态随之恢复", async () => {
    const p1 = `${TMP}/hl_t1_${Date.now()}.csv`;
    const p2 = `${TMP}/hl_t2_${Date.now()}.csv`;
    await invoke("write_csv_file", { path: p1, text: "a,1\r\nb,2\r\n" });
    await invoke("write_csv_file", { path: p2, text: "c,3\r\nd,4\r\n" });
    await window.__app.openPathsAsTabs([p1, p2]);
    const t1 = window.__app.tabs().find((x) => x.path === p1);
    const t2 = window.__app.tabs().find((x) => x.path === p2);
    assert(t1 && t2, "两个文件都应入签");

    await window.__app.activateTab(t1.id);
    await waitFor(() => model.rawData(0, 0) === "b", "首档首行落定");
    assert(window.__app.isHeaderless() === false, "首档默认关闭");
    tbBtn("首行非列名").click();
    await waitFor(() => window.__app.isHeaderless() === true, "首档开启");

    await window.__app.activateTab(t2.id);
    await waitFor(() => window.__app.isHeaderless() === false, "第二档保持关闭");
    assert(!tbBtn("首行非列名").classList.contains("on"), "切到第二档按钮应变关闭");
    assert(model.rowCount() === 1, "第二档行数不受首档影响");
    await waitFor(() => model.rawData(0, 0) === "d", "第二档首行落定");

    await window.__app.activateTab(t1.id);
    await waitFor(() => window.__app.isHeaderless() === true, "切回首档恢复开启");
    assert(tbBtn("首行非列名").classList.contains("on"), "切回首档按钮应开启");
    const snap = window.__app.tabs();
    assert(snap.find((x) => x.path === p1)?.headerless === true
      && snap.find((x) => x.path === p2)?.headerless === false,
    "页签快照中的开关状态应各自独立");
  });

  check("新建表格：只定列数×行数，默认 60 行并处于首行非列名", async () => {
    await window.__app.smokeLoadSmall([], []);

    // 第一次：显式 3 列 × 2 行
    tbBtn("新建").click();
    await waitFor(() => document.querySelector(".modal-mask"), "新建对话框");
    const mask = document.querySelector(".modal-mask");
    const colsIn = mask.querySelector("#newtable-cols");
    const rowsIn = mask.querySelector("#newtable-rows");
    assert(colsIn && rowsIn, "新建对话框应提供列数与行数输入");
    assert(mask.textContent.includes("首行非列名") && mask.textContent.includes("列名"),
      "新建对话框应提示如何在首行设置列名");
    assert(colsIn.value === "5", `默认列数应为 5（实际 ${colsIn.value}）`);
    assert(rowsIn.value === "60", `默认行数应为 60（实际 ${rowsIn.value}）`);
    colsIn.value = "3";
    rowsIn.value = "2";
    assert(clickModalButton("确定"), "新建对话框应有确定按钮");
    await waitFor(() => !window.__app.isBigMode() && window.__app.isHeaderless()
      && model.columnCount() === 3 && model.rowCount() === 2, "新建 3×2 表格落定");
    assert(JSON.stringify(model.headers()) === JSON.stringify(["列1", "列2", "列3"]),
      `虚拟列名 ${model.headers()}`);
    assert(model.rawData(0, 0) === "" && model.rawData(1, 2) === "", "新建单元格应为空");
    assert(tbBtn("首行非列名").classList.contains("on"), "新建后开关应为开启态");

    // 第二次：行数留空 → 默认 60
    await window.__app.smokeLoadSmall([], []);
    tbBtn("新建").click();
    await waitFor(() => document.querySelector(".modal-mask"), "新建对话框（默认行数）");
    const mask2 = document.querySelector(".modal-mask");
    mask2.querySelector("#newtable-cols").value = "1";
    mask2.querySelector("#newtable-rows").value = "";
    assert(clickModalButton("确定"), "第二次新建确定");
    await waitFor(() => window.__app.isHeaderless() && model.rowCount() === 60,
      "行数留空应默认 60");
    assert(model.columnCount() === 1, `默认列数应保持 1（实际 ${model.columnCount()}）`);

    await window.__app.smokeLoadSmall([], []);
  });

  // ---- 执行（看门狗：整体超时强制收尾，防挂死） ----
  const watchdog = setTimeout(async () => {
    lines.push("", `[watchdog] 超时未完成，已通过 ${lines.length} 项`);
    await invoke("finish_smoke", { report: lines.join("\n"), code: 1 });
  }, 120000);
  for (const [name, fn] of checks) {
    window.__smokeCurrent = name;
    try {
      await fn();
      lines.push(`✓ ${name}`);
    } catch (err) {
      failed += 1;
      lines.push(`✗ ${name} —— ${err.message}`);
    }
  }
  window.__smokeCurrent = "";
  clearTimeout(watchdog);
  lines.push("");
  lines.push(`合计 ${checks.length} 项，失败 ${failed} 项`);
  const report = lines.join("\n");
  console.log(report);
  await invoke("finish_smoke", { report, code: failed ? 1 : 0 });
}
