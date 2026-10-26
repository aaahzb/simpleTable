/**
 * 构造标签右键菜单；只描述动作，不直接操作 DOM 或文档会话。
 *
 * @param {object} options - 当前分组、标签和动作适配器。
 * @param {boolean} options.splitOn - 是否已分屏。
 * @param {object} options.tab - 目标标签。
 * @param {number} options.tabCount - 窗口标签总数。
 * @param {number} options.groupACount - A 组标签数。
 * @param {object} options.actions - 分屏、移组和新窗口动作。
 * @returns {Array<{label: string, disabled?: boolean, onClick: Function}>} 菜单项。
 */
export function createTabMenuItems({ splitOn, tab, tabCount, groupACount, actions }) {
  const items = [];
  if (splitOn) {
    const toB = tab.side === "A";
    items.push(
      {
        label: toB ? "移到右侧" : "移到左侧",
        disabled: toB && groupACount === 1,
        onClick: () => void actions.moveTab(tab.id, toB ? "B" : "A"),
      },
      { label: "关闭分屏", onClick: () => actions.closeSplit() },
    );
  } else {
    items.push({
      label: "在分屏中打开",
      disabled: tabCount < 2,
      onClick: () => void actions.openSplit(tab.id),
    });
  }
  items.push({
    label: "在新窗口打开",
    disabled: tab.transferState !== "idle",
    onClick: () => void actions.openTabInNewWindow(tab.id),
  });
  return items;
}
