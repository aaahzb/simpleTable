/**
 * 模态对话框组件（F1/F5 及各类确认）。
 *
 * 对齐 Qt 版：新建表格（列名列表）、排序（多层级）、重命名列、
 * 添加 N 行、通用确认（是/否）、未保存三选（保存/放弃/取消）、信息/错误提示。
 */

/**
 * 底层模态框：遮罩 + 标题 + 内容 + 按钮行。
 *
 * @param {string} title - 标题文本。
 * @param {HTMLElement} body - 内容节点。
 * @param {Array<{text: string, value: any, primary?: boolean}>} buttons - 按钮定义。
 * @returns {Promise<any>} 点击按钮的 value；点遮罩关闭返回 null。
 */
function modal(title, body, buttons) {
  return new Promise((resolve) => {
    const mask = document.createElement("div");
    mask.className = "modal-mask";
    const box = document.createElement("div");
    box.className = "modal";
    const head = document.createElement("div");
    head.className = "modal-title";
    head.textContent = title;
    const bd = document.createElement("div");
    bd.className = "modal-body";
    bd.appendChild(body);
    const btns = document.createElement("div");
    btns.className = "modal-buttons";
    const done = (v) => { mask.remove(); resolve(v); };
    for (const def of buttons) {
      const b = document.createElement("button");
      b.textContent = def.text;
      if (def.primary) b.style.fontWeight = "bold";
      b.addEventListener("click", () => done(def.value));
      btns.appendChild(b);
    }
    box.append(head, bd, btns);
    mask.appendChild(box);
    mask.addEventListener("mousedown", (e) => {
      if (e.target === mask) done(null);
    });
    document.addEventListener("keydown", function onKey(e) {
      if (!mask.isConnected) {
        document.removeEventListener("keydown", onKey);
        return;
      }
      if (e.key === "Escape") { e.stopPropagation(); done(null); }
      if (e.key === "Enter" && e.target.tagName !== "TEXTAREA") {
        e.stopPropagation();
        const p = buttons.find((x) => x.primary) ?? buttons[buttons.length - 1];
        done(p.value);
      }
    });
    document.body.appendChild(mask);
    const focusable = body.querySelector("input, select, textarea, button");
    (focusable ?? btns.querySelector("button"))?.focus();
  });
}

/**
 * 通用确认（是/否），对齐 QMessageBox.question。
 *
 * @param {string} text - 提示文本。
 * @returns {Promise<boolean>} 点"是"为 true。
 */
export async function confirmDialog(text) {
  const body = document.createElement("div");
  body.textContent = text;
  const v = await modal("确认操作", body, [
    { text: "否", value: false },
    { text: "是", value: true, primary: true },
  ]);
  return v === true;
}

/**
 * 字节数 → 人类可读文本（GB/MB/KB，保留 1 位小数）。
 *
 * @param {number} n - 字节数。
 * @returns {string} 展示文本，如 "2.3 GB"。
 */
export function fmtBytes(n) {
  const v = Number(n);
  if (!Number.isFinite(v) || v < 0) return "-";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let x = v;
  let i = 0;
  while (x >= 1024 && i < units.length - 1) {
    x /= 1024;
    i += 1;
  }
  return `${i === 0 ? Math.round(x) : x.toFixed(1)} ${units[i]}`;
}

/**
 * 排序内存确认（载入前展示可用内存 / 预计新增 / 峰值）。
 *
 * @param {{file_bytes: number, row_count: number, new_bytes: number,
 *   peak_bytes: number, available_bytes: number | null}} est - 引擎估算。
 * @returns {Promise<boolean>} true = 用户确认载入。
 */
export async function confirmRamSortDialog(est) {
  const body = document.createElement("div");
  body.style.whiteSpace = "pre-line";
  const avail = est.available_bytes == null
    ? "无法获取"
    : fmtBytes(est.available_bytes);
  const tight = est.available_bytes != null && est.available_bytes < est.new_bytes;
  body.textContent =
    "启用排序需要把整个文件载入内存，载入期间界面保持可用、可随时取消。\n\n" +
    `文件大小：${fmtBytes(est.file_bytes)}\n` +
    `预计新增内存：${fmtBytes(est.new_bytes)}\n` +
    `载入瞬间峰值：约 ${fmtBytes(est.peak_bytes)}（含可回收的文件缓存）\n` +
    `当前可用内存：${avail}\n\n` +
    (tight
      ? "⚠ 可用内存低于预计新增，载入可能失败或导致系统变慢。\n\n"
      : "") +
    "载入完成后排序通常只需几秒。";
  const v = await modal("启用排序（载入内存）", body, [
    { text: "取消", value: false },
    { text: "载入并排序", value: true, primary: true },
  ]);
  return v === true;
}

/**
 * 进度条模态（大文件保存/列操作物理重写/排序载入）。
 * 非阻塞：返回句柄由调用方驱动 set/close（进度经事件推送）。
 * 阶段文字 + 已用秒数走 UI 侧时钟（250ms 刷新）：即使引擎被写盘 IO 硬卡住
 * 发不出事件，界面也在持续刷新，不会出现"进度条定住像死机"的观感。
 *
 * @param {string} title - 标题文本。
 * @param {{phases?: string[], cancelText?: string, onCancel?: Function}} [opts] -
 *   阶段文字（缺省为保存流程）、取消按钮文字与回调；不传则无取消按钮。
 * @returns {{set: (done: number, total: number, phase?: number, rows?: number) => void,
 *            finish: () => Promise<void>, close: () => void}}
 */
export function progressDialog(title, opts = {}) {
  /** 阶段文字（与引擎 PHASE_* 常量对应）。 */
  const PHASES = opts.phases ?? ["正在重写文件…", "正在落盘替换原文件…", "正在重建行索引…"];
  const mask = document.createElement("div");
  mask.className = "modal-mask";
  const box = document.createElement("div");
  box.className = "modal";
  const head = document.createElement("div");
  head.className = "modal-title";
  head.textContent = title;
  const body = document.createElement("div");
  body.className = "modal-body";
  const bar = document.createElement("progress");
  bar.className = "progress-bar";
  bar.max = 1;
  bar.value = 0;
  const label = document.createElement("div");
  label.className = "progress-label";
  label.textContent = "准备中…";
  body.append(bar, label);
  let cancelBtn = null;
  if (opts.cancelText) {
    cancelBtn = document.createElement("button");
    cancelBtn.textContent = opts.cancelText;
    cancelBtn.style.marginTop = "8px";
    cancelBtn.addEventListener("click", () => {
      if (cancelBtn.disabled) return;
      cancelBtn.disabled = true;
      cancelBtn.textContent = "正在取消…";
      opts.onCancel?.();
    });
    body.appendChild(cancelBtn);
  }
  box.append(head, body);
  mask.appendChild(box);
  document.body.appendChild(mask);

  const t0 = performance.now();
  let lastDone = 0;
  let lastTotal = 1;
  let phase = 0;
  let rows = 0;
  /** 渲染标签：阶段文字 + 百分比 + 走表秒数（+ 流式阶段附行计数）。 */
  const render = () => {
    const sec = ((performance.now() - t0) / 1000).toFixed(1);
    const pct = Math.round((lastDone / Math.max(1, lastTotal)) * 100);
    const rowsTxt = phase === 0 && rows > 0
      ? ` · 已处理 ${rows.toLocaleString()} 行` : "";
    label.textContent =
      `${PHASES[phase] ?? ""} ${pct}% · 已用 ${sec} 秒${rowsTxt}`;
  };
  const timer = setInterval(render, 250);

  return {
    /**
     * 更新进度。
     *
     * @param {number} done - 已完成加权单位（总体进度的分子）。
     * @param {number} total - 总单位（分母不变，加权后仍等于总行数）。
     * @param {number} [ph] - 阶段（0 流式重写 / 1 落盘换名 / 2 重建索引）。
     * @param {number} [rowCount] - 已处理行数（流式阶段显示用）。
     */
    set(done, total, ph = 0, rowCount = 0) {
      const t = Math.max(1, total);
      lastDone = done;
      lastTotal = t;
      phase = ph;
      rows = rowCount;
      bar.max = t;
      bar.value = done;
      render();
    },
    /**
     * 完成帧：置 100% 短暂停留再关闭。
     * 重写期间磁盘 IO（杀软实时扫描等）可能饿死渲染管线，屏幕停在中间旧帧，
     * 弹窗一关就像"进度条卡住没走完"——完成帧保证"走完"被看到。
     *
     * @returns {Promise<void>}
     */
    async finish() {
      clearInterval(timer);
      bar.max = lastTotal;
      bar.value = lastTotal;
      label.textContent = "100%（完成）";
      await new Promise((resolve) => setTimeout(resolve, 400));
      mask.remove();
    },
    /** 关闭并移除模态。 */
    close() {
      clearInterval(timer);
      mask.remove();
    },
  };
}

/**
 * 未确定进度模态（大文件筛选等无行级进度的长操作）：旋转指示 + 已用秒数。
 * 非阻塞：返回句柄由调用方 close。
 *
 * @param {string} title - 标题文本。
 * @returns {{close: () => void}}
 */
export function busyDialog(title) {
  const mask = document.createElement("div");
  mask.className = "modal-mask";
  const box = document.createElement("div");
  box.className = "modal";
  const head = document.createElement("div");
  head.className = "modal-title";
  head.textContent = title;
  const body = document.createElement("div");
  body.className = "modal-body busy-body";
  const spin = document.createElement("div");
  spin.className = "busy-spinner";
  const label = document.createElement("div");
  label.className = "progress-label";
  const t0 = performance.now();
  label.textContent = "已用 0.0 秒";
  const timer = setInterval(() => {
    label.textContent = `已用 ${((performance.now() - t0) / 1000).toFixed(1)} 秒`;
  }, 100);
  body.append(spin, label);
  box.append(head, body);
  mask.appendChild(box);
  document.body.appendChild(mask);
  return {
    /** 关闭并移除模态。 */
    close() {
      clearInterval(timer);
      mask.remove();
    },
  };
}

/**
 * 未保存修改三选（保存/放弃/取消），对齐 _confirm_discard。
 *
 * @returns {Promise<"save" | "discard" | "cancel">} 用户选择。
 */
export async function confirmDiscardDialog() {
  const body = document.createElement("div");
  body.textContent = "当前表格有未保存的修改，是否保存？";
  const v = await modal("未保存的修改", body, [
    { text: "取消", value: "cancel" },
    { text: "放弃", value: "discard" },
    { text: "保存", value: "save", primary: true },
  ]);
  return v ?? "cancel";
}

/**
 * 退出排序模式三选（保存并退出 / 放弃排序 / 取消）。
 *
 * 「放弃排序」只恢复排序前的行顺序并释放内存，单元格修改仍保留在会话里，
 * 因此文案明确区分“放弃排序”而非“放弃修改”。
 *
 * @returns {Promise<"save" | "discard" | "cancel">} 用户选择。
 */
export async function confirmExitSortDialog() {
  const body = document.createElement("div");
  body.style.whiteSpace = "pre-line";
  body.textContent =
    "当前排序尚未写入磁盘。\n\n" +
    "• 保存并退出：按当前排序顺序重写文件。\n" +
    "• 放弃排序：恢复排序前的行顺序并释放排序内存；" +
    "已做的单元格修改仍会保留。";
  const v = await modal("退出排序", body, [
    { text: "取消", value: "cancel" },
    { text: "放弃排序", value: "discard" },
    { text: "保存并退出", value: "save", primary: true },
  ]);
  return v ?? "cancel";
}

/**
 * 单条信息/警告提示（对齐 QMessageBox.information）。
 *
 * @param {string} title - 标题。
 * @param {string} text - 内容。
 */
export async function infoDialog(title, text) {
  const body = document.createElement("div");
  body.style.whiteSpace = "pre-line";
  body.textContent = text;
  await modal(title, body, [{ text: "确定", value: true, primary: true }]);
}

/**
 * 错误提示（对齐 QMessageBox.critical）。
 *
 * @param {string} title - 标题。
 * @param {string} text - 内容。
 */
export async function errorDialog(title, text) {
  await infoDialog(title, text);
}

/**
 * 数字输入对话框（对齐 QInputDialog.getInt）。
 *
 * @param {string} title - 标题。
 * @param {string} label - 输入项标签。
 * @param {number} def - 默认值。
 * @param {number} min - 最小值。
 * @param {number} max - 最大值。
 * @returns {Promise<number | null>} 确认返回数值；取消返回 null。
 */
export async function intDialog(title, label, def, min, max) {
  const body = document.createElement("div");
  const lab = document.createElement("label");
  lab.textContent = label;
  const input = document.createElement("input");
  input.type = "number";
  input.value = String(def);
  input.min = String(min);
  input.max = String(max);
  body.append(lab, input);
  const v = await modal(title, body, [
    { text: "取消", value: null },
    { text: "确定", value: () => {
      const n = Math.round(Number(input.value));
      return Number.isFinite(n) ? Math.min(Math.max(n, min), max) : def;
    }, primary: true },
  ]);
  return typeof v === "function" ? v() : null;
}

/**
 * 新建表格对话框（列名列表增删，对齐 NewTableDialog）。
 *
 * @returns {Promise<string[] | null>} 列名列表（去空、去首尾空格）；取消 null。
 */
export async function newTableDialog() {
  const body = document.createElement("div");
  const hint = document.createElement("div");
  hint.textContent = "定义初始列（之后也可在表格中增删改）：";
  const list = document.createElement("ul");
  list.id = "newtable-cols";
  const btnRow = document.createElement("div");
  const addBtn = document.createElement("button");
  addBtn.textContent = "添加列";
  const removeBtn = document.createElement("button");
  removeBtn.textContent = "删除选中列";
  removeBtn.style.marginLeft = "6px";
  btnRow.append(addBtn, removeBtn);

  const addItem = (name = "") => {
    const li = document.createElement("li");
    const input = document.createElement("input");
    input.type = "text";
    input.value = name;
    li.appendChild(input);
    list.appendChild(li);
    input.focus();
  };
  addBtn.addEventListener("click", () => addItem());
  removeBtn.addEventListener("click", () => {
    // 删除当前聚焦的列名输入行（对齐 Qt 删除选中列：单列表格聚焦即选中）
    const active = document.activeElement;
    if (active && active.tagName === "INPUT" && active.parentElement.parentElement === list) {
      active.parentElement.remove();
      list.querySelector("input")?.focus();
    }
  });
  body.append(hint, list, btnRow);
  addItem(); // 默认先给一个空行便于直接填写（对齐 Qt）

  const v = await modal("新建表格", body, [
    { text: "取消", value: null },
    { text: "确定", value: () =>
        [...list.querySelectorAll("input")]
          .map((i) => i.value.trim())
          .filter((n) => n), primary: true },
  ]);
  if (v === null || v === undefined) return null;
  return v();
}

/**
 * 重命名/添加列对话框（对齐 ColumnDialog）。
 *
 * @param {string} title - 标题。
 * @param {string} initial - 初始列名。
 * @returns {Promise<string | null>} 去空格列名；取消或空为 null。
 */
export async function columnDialog(title, initial = "") {
  const body = document.createElement("div");
  const input = document.createElement("input");
  input.type = "text";
  input.value = initial;
  body.appendChild(input);
  const v = await modal(title, body, [
    { text: "取消", value: null },
    { text: "确定", value: () => input.value.trim(), primary: true },
  ]);
  return v === null || v === undefined ? null : v();
}

/**
 * 排序对话框（多层级，层级自上而下为优先级，对齐 SortDialog）。
 *
 * @param {string[]} headers - 列名。
 * @param {Array<[number, string, boolean]>} [initial] - 初始层级。
 * @returns {Promise<Array<[number, string, boolean]> | null>}
 *   (列号, "text"|"numeric", 是否升序) 列表；取消 null。
 */
export async function sortDialog(headers, initial = []) {
  const body = document.createElement("div");
  const hint = document.createElement("div");
  hint.textContent = "排序层级（自上而下为优先级，先排第一级）：";
  const top = document.createElement("div");
  top.style.display = "flex";
  top.style.gap = "10px";
  const levels = document.createElement("ul");
  levels.id = "sort-levels";
  const right = document.createElement("div");
  const mkSelect = (label, options) => {
    const lab = document.createElement("label");
    lab.textContent = label;
    const sel = document.createElement("select");
    for (const o of options) {
      const op = document.createElement("option");
      op.textContent = o;
      sel.appendChild(op);
    }
    right.append(lab, sel);
    return sel;
  };
  const colSel = mkSelect("列：", headers);
  const modeSel = mkSelect("方式：", ["字符", "数字"]);
  const orderSel = mkSelect("方向：", ["升序", "降序"]);
  const mkBtn = (text, fn) => {
    const b = document.createElement("button");
    b.textContent = text;
    b.style.marginTop = "6px";
    b.addEventListener("click", fn);
    right.appendChild(b);
    return b;
  };

  const renderLevels = () => {
    levels.textContent = "";
    for (const [col, mode, asc] of levelData) {
      const li = document.createElement("li");
      li.textContent = `${headers[col]}（${mode === "numeric" ? "数字" : "字符"}，` +
        `${asc ? "升序" : "降序"}）`;
      levels.appendChild(li);
    }
  };
  /** @type {Array<[number, string, boolean]>} */
  const levelData = [];
  for (const lv of initial) levelData.push([...lv]);
  renderLevels();

  levels.addEventListener("click", (e) => {
    for (const li of [...levels.children]) li.classList.remove("sel");
    e.target.closest("li")?.classList.add("sel");
  });
  const selectedIndex = () => {
    const sel = levels.querySelector("li.sel");
    return sel ? [...levels.children].indexOf(sel) : -1;
  };
  mkBtn("添加层级", () => {
    if (!headers.length) return;
    levelData.push([colSel.selectedIndex,
      modeSel.selectedIndex === 1 ? "numeric" : "text",
      orderSel.selectedIndex === 0]);
    renderLevels();
  });
  mkBtn("移除层级", () => {
    const i = selectedIndex();
    if (i >= 0) { levelData.splice(i, 1); renderLevels(); }
  });
  mkBtn("上移", () => {
    const i = selectedIndex();
    if (i > 0) {
      [levelData[i - 1], levelData[i]] = [levelData[i], levelData[i - 1]];
      renderLevels();
      levels.children[i - 1].classList.add("sel");
    }
  });
  mkBtn("下移", () => {
    const i = selectedIndex();
    if (i >= 0 && i < levelData.length - 1) {
      [levelData[i + 1], levelData[i]] = [levelData[i], levelData[i + 1]];
      renderLevels();
      levels.children[i + 1].classList.add("sel");
    }
  });

  top.append(levels, right);
  body.append(hint, top);
  const v = await modal("排序", body, [
    { text: "取消", value: null },
    { text: "确定", value: () => levelData.map((x) => [...x]), primary: true },
  ]);
  if (v === null || v === undefined) return null;
  return v();
}
