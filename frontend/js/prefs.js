/**
 * 按文件路径记忆的界面偏好（localStorage）。
 *
 * CSV 本身没有元数据位，因此「首行非列名」开关按文件路径保存在本机
 * localStorage 中：同一文件再次打开（含重启后）自动恢复。
 * 文件改名/换机器后偏好丢失，需重新切换（README 已说明）。
 *
 * 注意：自「绿色轻量」决策起，应用层已不再接入本模块——打开文件零写入，
 * 「首行非列名」只对当前页签生效（prefs.test.mjs 仍验证本模块可独立工作）。
 */

/** localStorage 键名（v1：值形如 { "/path/a.csv": true }）。 */
export const HEADERLESS_PREF_KEY = "simpletable.headerless.v1";

/** 撤销/重做历史预算的 localStorage 键。 */
export const UNDO_BUDGET_PREF_KEY = "simpletable.undo_budget.v1";
/** 每个文档的默认历史预算（MiB）。 */
export const DEFAULT_UNDO_BUDGET_MIB = 64;

/**
 * 把撤销预算归一化为 1–1024 MiB 的整数。
 *
 * @param {unknown} raw - 原始设置值。
 * @returns {number} 合法整数；缺失、损坏或越界时回退 64。
 */
export function normalizeUndoBudgetMiB(raw) {
  const n = Number(raw);
  return Number.isInteger(n) && n >= 1 && n <= 1024
    ? n : DEFAULT_UNDO_BUDGET_MIB;
}

/**
 * 把撤销预算从 MiB 转为字节。
 *
 * @param {unknown} raw - 原始 MiB 值。
 * @returns {number} 归一化后的字节预算。
 */
export function undoBudgetBytes(raw) {
  return normalizeUndoBudgetMiB(raw) * 1024 * 1024;
}

/**
 * 创建撤销/重做历史预算偏好存取器。
 *
 * @param {Storage | null | undefined} storage - localStorage 或同接口对象。
 * @returns {{get: () => number, set: (value: unknown) => number}}
 *   读取归一化值；写入返回实际采用的 MiB 值，存储异常不阻断设置。
 */
export function createUndoBudgetPrefs(storage) {
  return {
    /** @returns {number} 当前预算；读取失败时返回默认值。 */
    get() {
      try {
        return normalizeUndoBudgetMiB(storage?.getItem(UNDO_BUDGET_PREF_KEY));
      } catch {
        return DEFAULT_UNDO_BUDGET_MIB;
      }
    },
    /**
     * @param {unknown} value - 待保存的 MiB 值。
     * @returns {number} 实际采用的归一化预算。
     */
    set(value) {
      const normalized = normalizeUndoBudgetMiB(value);
      try {
        storage?.setItem(UNDO_BUDGET_PREF_KEY, String(normalized));
      } catch {
        // 配额或隐私模式只影响持久化，不阻断本次设置。
      }
      return normalized;
    },
  };
}

/** 普通预算与驻留撤销比例的组合设置键。 */
export const UNDO_SETTINGS_PREF_KEY = "simpletable.undo_settings.v2";
/** 默认驻留撤销比例：100%，内部使用整数千分比。 */
export const DEFAULT_RESIDENT_PERMILLE = 1000;
/** 最大驻留撤销比例：1000%。 */
export const MAX_RESIDENT_PERMILLE = 10000;

/**
 * 将十进制百分比文本解析为整数千分比。
 *
 * @param {unknown} raw - 0 至 1000.0 的百分比文本，最多一位小数。
 * @returns {number | null} 合法时返回 0 至 10000；否则返回 null。
 */
export function parseResidentPercent(raw) {
  const text = String(raw).trim();
  const match = /^(\d{1,4})(?:\.(\d))?$/.exec(text);
  if (!match) return null;
  const value = Number(match[1]) * 10 + Number(match[2] || "0");
  return value <= MAX_RESIDENT_PERMILLE ? value : null;
}

/**
 * 归一化内部驻留撤销千分比。
 *
 * @param {unknown} raw - 原始整数千分比。
 * @returns {number} 0 至 10000 的整数；非法值回退 1000。
 */
export function normalizeResidentPermille(raw) {
  const value = Number(raw);
  return Number.isInteger(value) && value >= 0 && value <= MAX_RESIDENT_PERMILLE
    ? value : DEFAULT_RESIDENT_PERMILLE;
}

/**
 * 计算文档当前的有效撤销预算。
 *
 * @param {{budgetMiB: number, residentPermille: number,
 *   fileBytes: number | bigint, memoryMode: boolean}} options - 策略与文档状态。
 * @returns {bigint} 普通预算与驻留预算的较大者；驻留部分向上取整。
 */
export function effectiveUndoBudgetBytes({
  budgetMiB, residentPermille, fileBytes, memoryMode,
}) {
  const budget = BigInt(budgetMiB) * 1024n * 1024n;
  if (!memoryMode) return budget;
  const bytes = typeof fileBytes === "bigint" ? fileBytes : BigInt(fileBytes);
  const permille = BigInt(normalizeResidentPermille(residentPermille));
  const scaled = (bytes * permille + 999n) / 1000n;
  return scaled > budget ? scaled : budget;
}

/**
 * 用二进制单位格式化任意精度字节数，不经过 Number 转换。
 *
 * @param {number | bigint} numberOrBigInt - 非负整数字节数。
 * @returns {string} B 使用整数，其余单位保留两位小数。
 */
export function formatByteQuantity(numberOrBigInt) {
  let bytes = typeof numberOrBigInt === "bigint"
    ? numberOrBigInt : BigInt(numberOrBigInt);
  if (bytes < 1024n) return `${bytes} B`;
  const units = ["KiB", "MiB", "GiB", "TiB", "PiB"];
  let whole = bytes;
  let fraction = 0n;
  let unit = 0;
  while (whole >= 1024n && unit < units.length) {
    fraction = ((whole % 1024n) * 100n) / 1024n;
    whole /= 1024n;
    unit += 1;
  }
  return `${whole}.${fraction.toString().padStart(2, "0")} ${units[unit - 1]}`;
}

/**
 * 创建普通预算与驻留撤销比例的版本化偏好存取器。
 *
 * @param {Storage | null | undefined} storage - localStorage 或同接口对象。
 * @returns {{get: () => {budgetMiB: number, residentPermille: number},
 *   set: (value: object) => {budgetMiB: number, residentPermille: number}}}
 *   优先读取 v2；缺失或损坏时迁移旧 v1 预算，存储异常不阻断设置。
 */
export function createUndoSettingsPrefs(storage) {
  const fallback = () => ({
    budgetMiB: createUndoBudgetPrefs(storage).get(),
    residentPermille: DEFAULT_RESIDENT_PERMILLE,
  });
  return {
    /** @returns {{budgetMiB: number, residentPermille: number}} 当前双值设置。 */
    get() {
      try {
        const saved = JSON.parse(storage?.getItem(UNDO_SETTINGS_PREF_KEY));
        if (saved) {
          return {
            budgetMiB: normalizeUndoBudgetMiB(saved.budgetMiB),
            residentPermille: normalizeResidentPermille(saved.residentPermille),
          };
        }
      } catch {
        // 损坏的 v2 继续走旧版迁移，不让设置页失效。
      }
      return fallback();
    },
    /**
     * @param {{budgetMiB: unknown, residentPermille: unknown}} value - 待保存双值。
     * @returns {{budgetMiB: number, residentPermille: number}} 实际采用值。
     */
    set(value) {
      const normalized = {
        budgetMiB: normalizeUndoBudgetMiB(value.budgetMiB),
        residentPermille: normalizeResidentPermille(value.residentPermille),
      };
      try {
        storage?.setItem(UNDO_SETTINGS_PREF_KEY, JSON.stringify(normalized));
      } catch {
        // 与旧设置一致：持久化失败不阻断当前进程采用新值。
      }
      return normalized;
    },
  };
}

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
