/**
 * 右键上下文菜单（F6）：屏幕坐标弹层，点击条目或空白处关闭。
 *
 * @typedef {object} MenuItem
 * @property {string} [label] - 显示文本；缺省为分隔线。
 * @property {Function} [onClick] - 点击回调。
 * @property {boolean} [disabled] - 是否禁用。
 */

let openMenu = null;

/** 关闭当前打开的菜单 */
export function closeMenu() {
  if (openMenu) {
    openMenu.remove();
    openMenu = null;
  }
}

/**
 * 在屏幕坐标弹出菜单。
 *
 * @param {number} x - 屏幕 x。
 * @param {number} y - 屏幕 y。
 * @param {MenuItem[]} items - 条目列表。
 */
export function showMenu(x, y, items) {
  closeMenu();
  const menu = document.createElement("div");
  menu.className = "ctx-menu";
  for (const item of items) {
    if (!item || !item.label) {
      const sep = document.createElement("div");
      sep.className = "msep";
      menu.appendChild(sep);
      continue;
    }
    const mi = document.createElement("div");
    mi.className = "mi" + (item.disabled ? " disabled" : "");
    mi.textContent = item.label;
    if (!item.disabled) {
      mi.addEventListener("click", () => {
        closeMenu();
        item.onClick?.();
      });
    }
    menu.appendChild(mi);
  }
  document.body.appendChild(menu);
  // 防溢出：向右/向下翻折
  const rect = menu.getBoundingClientRect();
  const px = Math.min(x, window.innerWidth - rect.width - 4);
  const py = Math.min(y, window.innerHeight - rect.height - 4);
  menu.style.left = `${Math.max(0, px)}px`;
  menu.style.top = `${Math.max(0, py)}px`;
  openMenu = menu;
  setTimeout(() => {
    document.addEventListener("mousedown", function away(e) {
      if (openMenu && !openMenu.contains(e.target)) {
        closeMenu();
        document.removeEventListener("mousedown", away);
      } else if (!openMenu) {
        document.removeEventListener("mousedown", away);
      }
    });
  }, 0);
}
