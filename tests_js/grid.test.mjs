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
