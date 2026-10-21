// grid.js 纯几何单元测试（不依赖 DOM）：渐进索引行数增长时的锚点换算。
// 用 Object.create(Grid.prototype) 造一个只有几何字段的假网格，
// 直接调用 _yAnchor/_scrollTopForAnchor 验证压缩滚动的坐标一致性。
import assert from "node:assert/strict";
import { test } from "node:test";

import { Grid, cellHit } from "../frontend/js/grid.js";

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

test("cellHit 三态真值表（0 包含 / 1 等于·不分大小写 / 2 等于·区分大小写）", () => {
  // mode 0：包含（折叠子串，现状语义）
  assert.equal(cellHit("xabc", "xABC", "xabc", 0), true);
  assert.equal(cellHit("xyz", "abc", "abc", 0), false);
  // mode 1：严格等于·不分大小写（折叠后整字段相等）
  assert.equal(cellHit("AbC", "abc", "abc", 1), true);
  assert.equal(cellHit("xabc", "abc", "abc", 1), false);
  // mode 2：严格等于·区分大小写（原样比较 kwRaw）
  assert.equal(cellHit("AbC", "AbC", "abc", 2), true);
  assert.equal(cellHit("AbC", "abc", "abc", 2), false);
  // CJK 无大小写：三态一致（等于只在整字段相同时命中）
  assert.equal(cellHit("北京", "北京", "北京", 1), true);
  assert.equal(cellHit("北京市", "北京", "北京", 1), false);
  assert.equal(cellHit("北京", "北京", "北京", 2), true);
  // 空串：空文本等于空关键字（三态同）
  assert.equal(cellHit("", "", "", 0), true);
  assert.equal(cellHit("", "", "", 1), true);
  assert.equal(cellHit("", "", "", 2), true);
});
