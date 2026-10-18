/**
 * 按文件路径记忆的界面偏好（localStorage）。
 *
 * CSV 本身没有元数据位，因此「首行非列名」开关按文件路径保存在本机
 * localStorage 中：同一文件再次打开（含重启后）自动恢复。
 * 文件改名/换机器后偏好丢失，需重新切换（README 已说明）。
 */

/** localStorage 键名（v1：值形如 { "/path/a.csv": true }）。 */
export const HEADERLESS_PREF_KEY = "simpletable.headerless.v1";

/**
 * 探测当前平台标识（用于路径大小写规则）。
 *
 * @returns {string} Node 的 process.platform，或浏览器平台描述；
 *   无法判断时返回空字符串。
 */
function detectPlatform() {
  if (typeof process !== "undefined" && process.platform) {
    return process.platform;
  }
  const nav = globalThis.navigator;
  if (nav) {
    const p = (nav.userAgentData && nav.userAgentData.platform) ||
      nav.platform || "";
    if (/mac/i.test(p)) {
      return "darwin";
    }
    if (/win/i.test(p)) {
      return "win32";
    }
    return p;
  }
  return "";
}

/**
 * 判断平台标识是否 Windows。
 *
 * 注意必须锚定开头：`darwin`（macOS）也包含 "win"，不能用裸 /win/i。
 *
 * @param {string} platform - 平台标识。
 * @returns {boolean} Windows 时为 true。
 */
function isWindows(platform) {
  return /^win/i.test(String(platform ?? ""));
}

/**
 * 归一化文件路径作为偏好键。
 *
 * 规则：分隔符统一为 "/"、折叠重复分隔符、去尾部 "/"（根路径除外）；
 * Windows 忽略大小写（统一小写），macOS 保留大小写（文件系统区分大小写）。
 *
 * @param {string} path - 原始文件路径。
 * @param {{platform?: string}} [opts] - 可注入平台标识（便于单测）。
 * @returns {string} 归一化后的偏好键。
 */
export function normalizePath(path, { platform = detectPlatform() } = {}) {
  let p = String(path ?? "").replace(/\\/g, "/").replace(/\/{2,}/g, "/");
  if (p.length > 1) {
    p = p.replace(/\/+$/, "");
  }
  if (isWindows(platform)) {
    p = p.toLowerCase();
  }
  return p;
}

/**
 * 创建「首行非列名」偏好存取器。
 *
 * 只存 true 项；损坏 JSON、storage 不可用或写入异常（配额/隐私模式）
 * 均静默回退，不影响打开文件。
 *
 * @param {Storage | null | undefined} storage - localStorage 或同接口对象；
 *   null/undefined 时退化为"永远关闭、写入无效"。
 * @param {{platform?: string}} [opts] - 可注入平台标识（便于单测）。
 * @returns {{
 *   get: (path: string) => boolean,
 *   set: (path: string, on: boolean) => void,
 *   clear: () => void,
 * }} 偏好存取器。
 */
export function createHeaderlessPrefs(storage, { platform = detectPlatform() } = {}) {
  /**
   * 读取全部偏好（失败/损坏时返回空对象）。
   *
   * @returns {Record<string, boolean>} 归一化路径 → 是否开启。
   */
  const readAll = () => {
    if (!storage) {
      return {};
    }
    try {
      const raw = storage.getItem(HEADERLESS_PREF_KEY);
      if (!raw) {
        return {};
      }
      const data = JSON.parse(raw);
      if (!data || typeof data !== "object" || Array.isArray(data)) {
        return {};
      }
      return data;
    } catch {
      return {};
    }
  };

  /**
   * 查询某文件是否开启「首行非列名」。
   *
   * @param {string} path - 文件路径。
   * @returns {boolean} 已记录为开启时 true，否则 false。
   */
  const get = (path) => readAll()[normalizePath(path, { platform })] === true;

  /**
   * 写入/删除某文件的偏好（只保留 true 项）。
   *
   * @param {string} path - 文件路径。
   * @param {boolean} on - 是否开启。
   * @returns {void}
   */
  const set = (path, on) => {
    if (!storage) {
      return;
    }
    const data = readAll();
    const key = normalizePath(path, { platform });
    if (on) {
      data[key] = true;
    } else {
      delete data[key];
    }
    try {
      storage.setItem(HEADERLESS_PREF_KEY, JSON.stringify(data));
    } catch {
      // 配额/隐私模式等：静默忽略，不阻断打开文件。
    }
  };

  /**
   * 清空全部偏好（测试与诊断用）。
   *
   * @returns {void}
   */
  const clear = () => {
    if (!storage) {
      return;
    }
    try {
      storage.removeItem(HEADERLESS_PREF_KEY);
    } catch {
      // 同 set：无法清空时静默忽略。
    }
  };

  return { get, set, clear };
}
