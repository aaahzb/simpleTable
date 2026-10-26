// grid.js 纯几何单元测试（不依赖 DOM）：渐进索引行数增长时的锚点换算。
// 用 Object.create(Grid.prototype) 造一个只有几何字段的假网格，
// 直接调用 _yAnchor/_scrollTopForAnchor 验证压缩滚动的坐标一致性。
import assert from "node:assert/strict";
import { test } from "node:test";

import { Grid } from "../frontend/js/grid.js";

/** 与 grid.js 的 Y_CLAMP 同步（未导出，仅测试用）。 */
const Y_CLAMP = 4_000_000;

/**
 * 造一个仍停留在旧布局几何上的大模式假网格。
 *
 * @param {object} opts - 参数。
 * @param {number} opts.oldN - 上一次 _layout 时的可见行数。
 * @param {number} opts.h - 统一行高。
 * @param {number} opts.oy - 吸顶表头高度。
 * @param {number} opts.viewH - 视口高度。
 * @returns {Grid} 仅含几何字段的假网格（model 初始返回 oldN）。
 */
function makeStaleLayoutGrid({ oldN, h = 20, oy = 52, viewH = 700 }) {
  const g = Object.create(Grid.prototype);
  const real = oldN * h;
  g._uniformRowH = h;
  g._yScale = real > Y_CLAMP ? real / Y_CLAMP : 1;
  g._rowSpan = Math.max(1, Math.floor(viewH / h));
  g._maxScroll = Math.max(0, oy + real / g._yScale - viewH);
  g._lastVisCount = oldN;
  g.visibleRows = null;
  g.model = { isBig: true, rowCount: () => oldN };
  g.headerEl = { offsetHeight: oy };
  g.scrollEl = { scrollTop: 0 };
  return g;
}

test("渐进索引：行数增长但布局未更新时，_yAnchor 仍按旧布局行数换算", () => {
  const oldN = 629_470;
  const newN = 1_000_001;
  const g = makeStaleLayoutGrid({ oldN });
  // 以旧布局把第 50,000 行对齐到表头下方，再模拟模型已报告新行数
  g.scrollEl.scrollTop = g._scrollTopForAnchor(50_000);
  g.model = { isBig: true, rowCount: () => newN };

  const { anchor } = g._yAnchor();

  // 旧实现在这里用 newN 反推，得到 ≈79,470（CI 报错值）；
  // 布局几何仍是 oldN 的，锚点必须保持 ≈50,000。
  assert.ok(
    Math.abs(anchor - 50_000) <= 1,
    `行数增长瞬间锚点应保持 ≈50000，实际 ${anchor}`,
  );
});

test("渐进索引：首次布局前（_lastVisCount=0）回退到当前可见行数", () => {
  const n = 1_000_001;
  const g = makeStaleLayoutGrid({ oldN: n });
  g._lastVisCount = 0;
  // 滚到行区滚动范围中点：按当前行数应落在可视行数的一半附近
  const oy = g.headerEl.offsetHeight;
  g.scrollEl.scrollTop = oy + (g._maxScroll - oy) / 2;

  const { anchor } = g._yAnchor();

  const expect = (n - g._rowSpan) / 2;
  assert.ok(
    Math.abs(anchor - expect) <= 2,
    `首次布局前应按当前行数换算，期望 ≈${Math.round(expect)}，实际 ${anchor}`,
  );
});

test("虚拟缩放滚动到底：可滚动上限少数像素时锚点仍吸附末行", () => {
  // 现场：1550 万行（_yScale≈89.125，行高 23）滚到底，倒数两行永远看不见。
  // 浏览器实际可滚动上限 = scrollHeight - 视口小数高，可能比 _maxScroll
  // 少至多 1px；压缩坐标下 0.5px ≈ 2 行，按比例四舍五入就停在 span-2，
  // 末尾两行被画到视口下缘之外且再也滚不出来。
  const n = 15_500_000;
  const g = makeStaleLayoutGrid({ oldN: n, h: 23, viewH: 761 });
  g.scrollEl.scrollTop = g._maxScroll - 0.5;

  const { anchor } = g._yAnchor();

  const span = n - g._rowSpan;
  assert.equal(anchor, span, `贴底时锚点应吸附到 n-_rowSpan=${span}，实际 ${anchor}`);
});

test("未缩放滚动到底：末行不得被视口下缘裁掉", () => {
  // 视口高 712、行高 20：rawSv/行高 的小数部分 .6 会四舍五入到下一行，
  // 末行下缘超出视口 8px（用户看到半截/看不到）。贴底时必须吸附到
  // n-_rowSpan，让末行整体落在视口内。
  const n = 100_000;
  const g = makeStaleLayoutGrid({ oldN: n, h: 20, viewH: 712 });
  g.scrollEl.scrollTop = g._maxScroll - 0.5;

  const { anchor } = g._yAnchor();

  assert.equal(anchor, n - g._rowSpan,
    `贴底时锚点应为 n-_rowSpan=${n - g._rowSpan}，实际 ${anchor}`);
});

test("锚点精确性：_yAnchor 暴露小数位置 pos（未压缩整行精确）", () => {
  // 渐进索引首批（<20 万行）不压缩：scrollTop = 表头 + 行号×行高，
  // pos 必须精确等于行号（捕获不含量化损失是恢复不动点的前提）。
  const g = makeStaleLayoutGrid({ oldN: 104_858, h: 20, oy: 52, viewH: 700 });
  assert.equal(g._yScale, 1, "首批行数不应触发压缩");
  g.scrollEl.scrollTop = g._originY() + 50_000 * 20;

  const { pos, anchor } = g._yAnchor();

  assert.equal(pos, 50_000, `未压缩 pos 应精确等于 50000，实际 ${pos}`);
  assert.equal(anchor, 50_000);
});

test("锚点恢复不动点：行数增长后恢复再捕获位置不变（CI 漂移根因回归）", () => {
  // CI 实测：索引完成报 49997（期望 50000）。根因是恢复时刻意把锚点
  // 移到表头下方（_scrollTopForRowTop），每次进度事件净下移一个表头高
  // （本地 1 次事件恰好被测量公式抵消、CI 2 次事件漂 3 行）。
  // 恢复必须是不动点：capture → 换新布局 → restore → 再 capture 得到同一位置，
  // 重复多少次都不漂移。
  const n1 = 104_858;   // 首批（未压缩）
  const n2 = 1_000_001; // 索引完成（Y_CLAMP 压缩）
  const ga = makeStaleLayoutGrid({ oldN: n1, h: 20, oy: 52, viewH: 700 });
  ga.scrollEl.scrollTop = ga._originY() + 50_000 * 20;
  const pos0 = ga._yAnchor().pos;

  // 第一次进度事件：布局换成 n2，恢复后重捕获
  const gb = makeStaleLayoutGrid({ oldN: n2, h: 20, oy: 52, viewH: 700 });
  gb.scrollEl.scrollTop = gb._scrollTopForAnchor(pos0);
  const pos1 = gb._yAnchor().pos;
  assert.ok(Math.abs(pos1 - pos0) <= 0.3,
    `行数增长恢复后位置应保持 50000，实际 ${pos1}`);

  // 第二次进度事件（CI 情形：慢盘会捕获中间行数）：必须仍是不动点
  gb.scrollEl.scrollTop = gb._scrollTopForAnchor(pos1);
  const pos2 = gb._yAnchor().pos;
  assert.ok(Math.abs(pos2 - pos1) <= 1e-6,
    `重复恢复不得漂移：${pos1} → ${pos2}`);
  assert.ok(Math.abs(pos2 - pos0) <= 0.3,
    `两次事件后位置仍应≈50000，实际 ${pos2}`);
});

test("取消筛选锚定（未压缩）：无选中锚视口顶行，行内小数偏移保持", () => {
  // oy=52、h=20、yScale=1：scrollTop=oy+(100+0.3)h → pos=100.3
  const g = makeStaleLayoutGrid({ oldN: 10_000, h: 20, oy: 52, viewH: 700 });
  g.sel = { current: null, anchor: null, blocks: [] };
  g._renderRows = () => { g._rendered = (g._rendered ?? 0) + 1; };
  g.scrollEl.scrollTop = 52 + (100 + 0.3) * 20;

  const cap = g._captureClearAnchor();
  assert.equal(cap.k, 100);
  assert.equal(cap.col, null);
  assert.ok(Math.abs(cap.delta - (-0.3)) < 1e-6, `delta=${cap.delta}`);

  g._restoreClearAnchor(cap, 2999); // 命中序号 100 → 清筛后全量行号 2999
  const want = 52 + (2999 + 0.3) * 20;
  assert.ok(Math.abs(g.scrollEl.scrollTop - want) < 1e-6,
    `scrollTop=${g.scrollEl.scrollTop}，期望 ${want}（行内偏移 0.3 行保持）`);
  assert.ok(g._rendered >= 1, "无选中列时应走 _renderRows 重绘");
});

test("取消筛选锚定（压缩坐标系）：视口内行偏移经压缩往返保持", () => {
  const g = makeStaleLayoutGrid({ oldN: 629_470 }); // _yScale≈3.147
  g.sel = { current: null, anchor: null, blocks: [] };
  g._renderRows = () => {};
  const oy = g.headerEl.offsetHeight;
  const maxSv = g._maxScroll - oy;
  g.scrollEl.scrollTop = oy + maxSv * 0.6007; // 深滚，pos 带小数
  const { pos: pos0, anchor: anchor0 } = g._yAnchor();

  const cap = g._captureClearAnchor();
  assert.equal(cap.k, Math.floor(pos0));

  const T = 400_000; // 清筛后全量视图行号
  g._restoreClearAnchor(cap, T);
  const { anchor: anchor1 } = g._yAnchor();
  assert.equal(T - anchor1, cap.k - anchor0,
    `视口内行偏移应保持 ${cap.k - anchor0}，实际 ${T - anchor1}`);
  assert.ok(g.scrollEl.scrollTop > oy + maxSv * 0.63,
    "恢复应把视口移到 target（≈63.6%）附近");
});

test("取消筛选锚定：选中优先于视口顶，select 联动 currentChanged", () => {
  const g = makeStaleLayoutGrid({ oldN: 10_000 });
  g.sel = { current: [5, 2], anchor: [5, 2], blocks: [] };
  g.hooks = { currentChanged: (r, c) => { g._cc = [r, c]; } };
  g.select = (f) => { g._selArg = f; g.hooks.currentChanged(f[0], f[1]); };
  g.scrollEl.scrollTop = 52 + 80 * 20; // 视口顶行 80——不应被用作锚

  const cap = g._captureClearAnchor();
  assert.equal(cap.k, 5, "有选中时锚=选中行（命中序号）");
  assert.equal(cap.col, 2);

  g._restoreClearAnchor(cap, 2999);
  assert.deepEqual(g._selArg, [2999, 2], "选中应 remap 到清筛后行号");
  assert.deepEqual(g._cc, [2999, 2], "编辑窗格应收到 currentChanged 联动");
  const want = 52 + 3074 * 20; // posG = 2999 - (5-80)
  assert.ok(Math.abs(g.scrollEl.scrollTop - want) < 1e-6,
    `scrollTop=${g.scrollEl.scrollTop}，期望 ${want}（选中行视口位置保持）`);
});

test("_rowHeadText：筛选态取原始行号（未缓存空串占位），非筛选连续号", () => {
  const g = Object.create(Grid.prototype);
  g.model = {
    isBig: true,
    filterActive: () => true,
    rowNumber: (r) => (r === 3 ? null : 100 + r),
  };
  assert.equal(g._rowHeadText(0), "101", "筛选态行号=原始行号+1");
  assert.equal(g._rowHeadText(3), "", "页未缓存给空串占位（落定后同帧收敛）");
  g.model = { isBig: true, filterActive: () => false, rowNumber: () => null };
  assert.equal(g._rowHeadText(7), "8", "非筛选大模式=行索引+1");
});

/** 造一个仅含编辑协作者的原型级 Grid，避免 DOM 与真实引擎。 */
function makeEditGrid(model) {
  const g = Object.create(Grid.prototype);
  const messages = [];
  const selections = [];
  g.model = model;
  g.hooks = { statusMsg: (text) => messages.push(text) };
  g.selectedCells = () => [[0, 0], [0, 1], [1, 0]];
  g.select = (cell) => selections.push(["select", cell]);
  g._extendTo = (cell) => selections.push(["extend", cell]);
  return { g, messages, selections };
}

test("Delete 单块反向矩形直接发 clearRect", async () => {
  const calls = [];
  const model = {
    clearRect: async (...args) => { calls.push(args); return { changed: true }; },
    clearCells: async () => { throw new Error("不得展开为单元格"); },
  };
  const { g } = makeEditGrid(model);
  g.sel = { blocks: [[4, 2, 1, 0]] };
  g.selectedSet = new Set([0]);
  g.selectedCells = () => { throw new Error("不得调用 selectedCells"); };
  assert.equal(await g.deleteSelection(), true);
  assert.deepEqual(calls, [[1, 0, 4, 3]]);
});

test("Delete 多块选区保留稀疏坐标", async () => {
  const cells = [[0, 0], [2, 2]];
  const calls = [];
  const model = {
    clearCells: async (value) => { calls.push(value); return { changed: true }; },
    clearRect: async () => { throw new Error("多块不得合并成大矩形"); },
  };
  const { g } = makeEditGrid(model);
  g.sel = { blocks: [[0, 0, 0, 0], [2, 2, 2, 2]] };
  g.selectedSet = new Set([0, 1]);
  g.selectedCells = () => cells;
  await g.deleteSelection();
  assert.deepEqual(calls, [cells]);
});

test("剪切复制拒绝时不得清除", async () => {
  const { g } = makeEditGrid({});
  g.selectedSet = new Set([0]);
  let clears = 0;
  const denied = Promise.reject(new Error("clipboard denied"));
  denied.catch(() => {});
  g.copySelection = () => denied;
  g.deleteSelection = async () => { clears += 1; return true; };
  await assert.rejects(async () => g.cutSelection(), /clipboard denied/);
  assert.equal(clears, 0);
});

test("剪切收到复制失败结果时不得清除", async () => {
  const { g } = makeEditGrid({});
  g.selectedSet = new Set([0]);
  let clears = 0;
  g.copySelection = async () => false;
  g.deleteSelection = async () => { clears += 1; return true; };
  assert.equal(await g.cutSelection(), false);
  assert.equal(clears, 0);
});

test("剪切等待期间选区变化不得清除新选区", async () => {
  const { g } = makeEditGrid({});
  g.sel = { blocks: [[0, 0, 0, 0]] };
  g.selectedSet = new Set([0]);
  let clears = 0;
  g.copySelection = async () => {
    g.sel.blocks = [[2, 2, 2, 2]];
    g.selectedSet = new Set([10]);
    return true;
  };
  g.deleteSelection = async () => { clears += 1; return true; };
  assert.equal(await g.cutSelection(), false);
  assert.equal(clears, 0);
});

test("复制剪切和 Delete 统一经过拒绝捕获入口", () => {
  const { g } = makeEditGrid({});
  g.sel = { current: [0, 0] };
  const calls = [];
  g._runClipboard = (promise) => {
    calls.push("run");
    Promise.resolve(promise).catch(() => {});
  };
  g.copySelection = () => { calls.push("copy"); return Promise.resolve(true); };
  g.cutSelection = () => { calls.push("cut"); return Promise.resolve(true); };
  g.deleteSelection = () => { calls.push("delete"); return Promise.resolve(true); };
  for (const [key, ctrlKey] of [["c", true], ["x", true], ["Delete", false]]) {
    g._onKeyDown({
      key, ctrlKey, metaKey: false, shiftKey: false, altKey: false,
      target: { tagName: "CANVAS" }, preventDefault: () => {},
    });
  }
  assert.deepEqual(calls, ["copy", "run", "cut", "run", "delete", "run"]);
});

test("单值填充把含未加载格的完整选区交给一次 setCells", async () => {
  const writes = [];
  const model = {
    setCells: async (cells) => { writes.push(cells); return true; },
    rawData: () => { throw new Error("不得用缓存旧值过滤"); },
    setData: async () => { throw new Error("不得逐格写入"); },
    batch: async () => { throw new Error("不得走空事务"); },
  };
  const { g } = makeEditGrid(model);
  await g._fillSelection("X");
  assert.deepEqual(writes, [[
    [0, 0, "X"], [0, 1, "X"], [1, 0, "X"],
  ]]);
});

test("扩行粘贴只调 pasteRows，不预先插行或逐格写", async () => {
  const writes = [];
  const rows = [["A"], ["B"], ["C"]];
  const model = {
    rowCount: () => 1,
    pasteRows: async (...args) => {
      writes.push(args);
      return { changed: true, insertedRows: 2 };
    },
    insertRows: async () => { throw new Error("不得另行插行"); },
    setData: async () => { throw new Error("不得逐格写入"); },
  };
  const { g, selections } = makeEditGrid(model);
  await g._pasteBig(0, 0, rows, 1);
  assert.deepEqual(writes, [[0, 0, rows]]);
  assert.deepEqual(selections, [["select", [0, 0]], ["extend", [2, 0]]]);
});

/** 造一个触发内存档自动扩列的最小 Grid。 */
function makeAutoExpandGrid(approved) {
  const calls = [];
  const selections = [];
  const messages = [];
  const g = Object.create(Grid.prototype);
  g.sel = { current: [0, 0] };
  g.selectedSet = new Set([0]);
  g.lastCopiedText = null;
  g.hooks = {
    statusMsg: (text) => { messages.push(text); },
    confirmColOps: async () => {
      calls.push("confirm");
      return approved;
    },
  };
  g.model = {
    columnCount: () => 2,
    meta: () => ({ has_path: false, sorted: false }),
    insertColumns: async (at, count) => { calls.push(`insert:${at}:${count}`); },
    applyColOps: async () => { calls.push("apply"); },
    pasteRows: async () => {
      calls.push("paste");
      return { changed: approved, insertedRows: 0, cancelled: !approved };
    },
    setData: async () => { throw new Error("不得走单格纯文本分支"); },
  };
  g.select = (point) => { selections.push(["select", point]); };
  g._extendTo = (point) => { selections.push(["extend", point]); };
  return { g, calls, selections, messages };
}

/** 临时替换剪贴板，并返回恢复函数。 */
function stubNavigatorClipboard(text) {
  const old = Object.getOwnPropertyDescriptor(globalThis.navigator, "clipboard");
  Object.defineProperty(globalThis.navigator, "clipboard", {
    value: { readText: async () => text }, configurable: true,
  });
  return () => {
    if (old) Object.defineProperty(globalThis.navigator, "clipboard", old);
    else delete globalThis.navigator.clipboard;
  };
}

test("超宽粘贴直接一次 pasteRows，不先确认或执行列操作", async () => {
  const { g, calls, selections } = makeAutoExpandGrid(true);
  const restore = stubNavigatorClipboard("A\tB\tC\r\n");
  try {
    await g.pasteClipboard();
  } finally {
    restore();
  }
  assert.deepEqual(calls, ["paste"]);
  assert.deepEqual(selections, [["select", [0, 0]], ["extend", [0, 2]]]);
});

test("扩列 fixedPoint 取消时不扩展选区并提示取消", async () => {
  const { g, calls, selections, messages } = makeAutoExpandGrid(false);
  const restore = stubNavigatorClipboard("A\tB\tC\r\n");
  try {
    await g.pasteClipboard();
  } finally {
    restore();
  }
  assert.deepEqual(calls, ["paste"]);
  assert.deepEqual(selections, []);
  assert.equal(messages.at(-1), "已取消粘贴");
});

/** 记录系统剪贴板写入，并恢复原始描述符。 */
function stubClipboardWriter(writes) {
  const old = Object.getOwnPropertyDescriptor(globalThis.navigator, "clipboard");
  Object.defineProperty(globalThis.navigator, "clipboard", {
    value: { writeText: async (text) => { writes.push(text); } }, configurable: true,
  });
  return () => {
    if (old) Object.defineProperty(globalThis.navigator, "clipboard", old);
    else delete globalThis.navigator.clipboard;
  };
}

test("大复制完整矩形只拿 token，不写系统剪贴板", async () => {
  const writes = [];
  const calls = [];
  const model = {
    copyRows: async (...args) => {
      calls.push(args);
      return { kind: "app", meta: { token: "7", rows: 4, cols: 3,
        bytes: 9_000_000, version: 1 } };
    },
    clipboardClear: async () => { throw new Error("App 结果不得清 current"); },
  };
  const { g, messages } = makeEditGrid(model);
  g.sel = { current: [1, 0], blocks: [[4, 2, 1, 0]] };
  g.selectedSet = { size: 200_001 };
  g.selectedCells = () => { throw new Error("完整矩形不得展开"); };
  g.lastCopiedText = null;
  const restore = stubClipboardWriter(writes);
  try { assert.equal(await g.copySelection(), true); } finally { restore(); }
  assert.deepEqual(calls, [[1, 5, 0, 3]]);
  assert.deepEqual(writes, []);
  assert.equal(g.lastCopiedText, null);
  assert.ok(messages.some((x) => x.includes("已复制大区域，仅可在本应用内粘贴")));
});

test("小复制 Text 写系统后才清旧应用内 token", async () => {
  const writes = [];
  const events = [];
  const model = {
    copyRows: async () => ({ kind: "text", text: "A", bytes: 1 }),
    clipboardClear: async () => { events.push("clear"); },
  };
  const { g } = makeEditGrid(model);
  g.sel = { current: [0, 0], blocks: [[0, 0, 0, 0]] };
  g.selectedSet = new Set([0]);
  g.lastCopiedText = null;
  const restore = stubClipboardWriter(writes);
  try { assert.equal(await g.copySelection(), true); } finally { restore(); }
  assert.deepEqual(writes, ["A"]);
  assert.deepEqual(events, ["clear"]);
  assert.equal(g.lastCopiedText, "A");
});

test("剪切复制失败返回 false 且不清除", async () => {
  const writes = [];
  let clears = 0;
  const model = {
    copyRows: async () => { throw new Error("copy failed"); },
    clearRect: async () => { clears += 1; return { changed: true }; },
    clearCells: async () => { clears += 1; return { changed: true }; },
  };
  const { g } = makeEditGrid(model);
  g.sel = { current: [0, 0], blocks: [[0, 0, 0, 0]] };
  g.selectedSet = new Set([0]);
  const restore = stubClipboardWriter(writes);
  try { assert.equal(await g.cutSelection(), false); } finally { restore(); }
  assert.equal(clears, 0);
  assert.deepEqual(writes, []);
});
