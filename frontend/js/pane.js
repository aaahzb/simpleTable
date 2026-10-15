/**
 * 编辑窗格（E1–E3）：右侧多行编辑器，随选中单元格双向同步；
 * "显示符号"用叠加镜像层实现（空格→·、Tab→→、换行→¶），
 * 符号绝对定位叠加在真实占位字符上，排版与编辑域逐像素一致（光标不串位）；
 * 编辑始终作用于原文，不会像文本替换方案那样污染含 · 的中文内容。
 * Ctrl+滚轮独立缩放，与表格互不影响。
 */

import { ZOOM_MAX, ZOOM_MIN, ZOOM_STEP } from "./grid.js";

/**
 * 把原始文本转为镜像层片段（仅展示用）：换行→¶+真实换行；
 * 空格/制表符保留原字符占位（宽度与编辑域逐像素一致），符号以绝对定位叠加。
 * 不能直接替换字符：YaHei 中 · 比空格窄、→ 为全宽，而制表符按 8 停位进距，
 * 替换会让镜像排版偏离编辑域，光标与符号逐位串位。
 *
 * @param {string} raw - 原文。
 * @param {Document} [doc] - 目标文档（默认当前文档，便于测试注入）。
 * @returns {DocumentFragment} 镜像层内容片段。
 */
export function toSymbolsFrag(raw, doc = document) {
  const frag = doc.createDocumentFragment();
  let run = doc.createTextNode("");
  /** 把累积的普通文本段刷入片段 */
  const flush = () => {
    if (run.data) {
      frag.appendChild(run);
      run = doc.createTextNode("");
    }
  };
  for (const ch of raw) {
    if (ch === " " || ch === "\t") {
      flush();
      const box = doc.createElement("span");
      box.className = ch === " " ? "sp" : "tab";
      box.textContent = ch; // 真实占位字符：排版与编辑域一致
      const glyph = doc.createElement("span");
      glyph.className = "g";
      glyph.textContent = ch === " " ? "·" : "→";
      box.appendChild(glyph); // 绝对定位叠加，不参与排版
      frag.appendChild(box);
    } else if (ch === "\n") {
      run.data += "¶\n"; // ¶ 在行尾（换行之前），换行本身保留——镜像层靠真实换行断行
    } else {
      run.data += ch;
    }
  }
  flush();
  return frag;
}

/**
 * 编辑窗格类。
 */
export class EditPane {
  /**
   * @param {object} opts - 构造参数。
   * @param {HTMLElement} opts.paneEl - 窗格容器（#pane）。
   * @param {HTMLElement} opts.editEl - 文本域（#pane-edit）。
   * @param {HTMLElement} opts.mirrorEl - 符号镜像层（#pane-mirror）。
   * @param {(msg: string, ms?: number) => void} opts.statusMsg - 状态消息。
   * @param {(value: string) => void} opts.onEdit - 用户编辑回写回调。
   */
  constructor({ paneEl, editEl, mirrorEl, statusMsg, onEdit }) {
    this.paneEl = paneEl;
    this.editEl = editEl;
    this.mirrorEl = mirrorEl;
    this.statusMsg = statusMsg;
    this.onEdit = onEdit;
    this.symbols = false;
    this.zoom = 1;
    /** 程序化装载中（阻断回写） */
    this._loading = false;

    this.editEl.addEventListener("input", () => {
      if (this._loading) return;
      this._syncMirror();
      this.onEdit(this.editEl.value);
    });
    // Tab 是窗格内的输入字符（对齐 Qt 编辑窗格），不能交给浏览器做焦点导航
    this.editEl.addEventListener("keydown", (e) => {
      if (e.key !== "Tab" || this._loading) return;
      e.preventDefault();
      const el = this.editEl;
      // 优先走原生插入管线：保留文本域撤销栈（手动赋值 value 会清空它，导致 Ctrl+Z 失效），
      // 且连续输入参与原生撤销步合并；原生管线会自动触发 input 事件完成回写
      if (document.execCommand("insertText", false, "\t")) {
        return;
      }
      // 原生插入不可用时退回手动插入（代价：清空撤销栈，退化为无法 Ctrl+Z 撤销本次 Tab）
      const s = el.selectionStart ?? el.value.length;
      const t = el.selectionEnd ?? el.value.length;
      el.value = el.value.slice(0, s) + "\t" + el.value.slice(t);
      el.selectionStart = el.selectionEnd = s + 1;
      this._syncMirror();
      this.onEdit(el.value);
    });
    this.editEl.addEventListener("scroll", () => this._syncScroll());
    this.editEl.addEventListener("wheel", (e) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      const step = e.deltaY < 0 ? ZOOM_STEP : 1 / ZOOM_STEP;
      this.setZoom(this.zoom * step);
    }, { passive: false });
  }

  /**
   * 装载单元格完整内容（选中变化时由外壳调用）。
   *
   * @param {string} text - 原文。
   */
  setText(text) {
    this._loading = true;
    this.editEl.value = text;
    this._loading = false;
    this._syncMirror();
  }

  /** 清空内容（对齐 _clear_dock） */
  clear() {
    this.setText("");
  }

  /**
   * 切换显示符号模式。
   *
   * @param {boolean} on - 是否显示符号。
   */
  setSymbols(on) {
    this.symbols = on;
    this.paneEl.classList.toggle("symbols", on);
    this._syncMirror();
  }

  /**
   * 窗格缩放（独立于表格）。
   *
   * @param {number} factor - 缩放因子。
   */
  setZoom(factor) {
    factor = Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, factor));
    if (Math.abs(factor - this.zoom) < 1e-6) return;
    this.zoom = factor;
    this.paneEl.querySelector("#pane-wrap").style.setProperty(
      "--pane-zoom", String(factor));
    this._syncMirror();
    this.statusMsg(`编辑窗格缩放 ${Math.round(factor * 100)}%`, 1500);
  }

  /** 符号开启时刷新镜像层内容 */
  _syncMirror() {
    if (!this.symbols) return;
    this.mirrorEl.replaceChildren(toSymbolsFrag(this.editEl.value));
    this._syncScroll();
  }

  /** 镜像层滚动跟随（编辑域在上层负责交互） */
  _syncScroll() {
    if (!this.symbols) return;
    this.mirrorEl.scrollTop = this.editEl.scrollTop;
    this.mirrorEl.scrollLeft = this.editEl.scrollLeft;
  }
}
