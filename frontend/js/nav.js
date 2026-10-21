/**
 * Excel 风格键盘导航的纯逻辑（可脱离 DOM/模型单测）。
 *
 * 生产无引用（P5 注记）：grid 的 Ctrl+↑/↓ 已走引擎 findVerticalEdge；
 * 本模块保留供 nav.test.mjs 与 bigmodel.test.mjs（伪引擎）单测复用。
 *
 * Ctrl+↑/↓ 的语义对齐 Excel：
 * - 当前格非空：跳到当前连续非空数据块的上/下边缘；
 * - 当前格已在该边缘：越过空档，跳到下一条非空数据块的第一格；
 * - 当前格为空：向该方向找下一个非空格；
 * - 该方向已无数据：落到表格首/末行。
 */

/**
 * 查找纵向数据边缘。
 *
 * @param {(index: number) => boolean} isNonEmpty - 判断某行当前列是否非空。
 * @param {number} start - 当前行索引。
 * @param {number} count - 可导航行数（筛选视图为可见行数）。
 * @param {boolean} down - true=向下（Ctrl+↓）；false=向上（Ctrl+↑）。
 * @returns {number} 目标行索引；空表返回 -1（调用方不应移动）。
 */
export function findVerticalEdge(isNonEmpty, start, count, down) {
  if (count <= 0) return -1;
  const last = count - 1;
  const begin = Math.min(Math.max(start, 0), last);
  const step = down ? 1 : -1;
  const inRange = (i) => i >= 0 && i <= last;
  const fallback = down ? last : 0;

  // 当前格非空：先走完连续非空块；整块只有当前格时再越过空档找下一块。
  if (isNonEmpty(begin)) {
    let edge = begin;
    let i = begin + step;
    while (inRange(i) && isNonEmpty(i)) {
      edge = i;
      i += step;
    }
    if (edge !== begin) return edge;
    while (inRange(i) && !isNonEmpty(i)) i += step;
    return inRange(i) ? i : fallback;
  }

  // 当前格为空：直接找该方向的下一个非空格。
  let i = begin + step;
  while (inRange(i) && !isNonEmpty(i)) i += step;
  return inRange(i) ? i : fallback;
}
