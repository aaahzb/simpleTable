// prefs.js 单元测试：按文件路径记忆「首行非列名」开关。
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  HEADERLESS_PREF_KEY,
  UNDO_BUDGET_PREF_KEY,
  createHeaderlessPrefs,
  createUndoBudgetPrefs,
  normalizePath,
  undoBudgetBytes,
} from "../frontend/js/prefs.js";

/** 内存版 Storage（只实现 prefs 用到的方法）。 */
function makeStorage(initial = {}) {
  const map = new Map(Object.entries(initial));
  return {
    getItem: (k) => (map.has(k) ? map.get(k) : null),
    setItem: (k, v) => { map.set(k, String(v)); },
    removeItem: (k) => { map.delete(k); },
    _map: map,
  };
}

test("未记录的文件默认关闭", () => {
  const prefs = createHeaderlessPrefs(makeStorage(), { platform: "win32" });
  assert.equal(prefs.get("C:/data/a.csv"), false);
});

test("写入后读取为 true；关闭时删除记录", () => {
  const storage = makeStorage();
  const prefs = createHeaderlessPrefs(storage, { platform: "win32" });
  prefs.set("C:/data/a.csv", true);
  assert.equal(prefs.get("C:/data/a.csv"), true);
  // 只存 true 项
  const raw = JSON.parse(storage.getItem(HEADERLESS_PREF_KEY));
  assert.deepEqual(raw, { "c:/data/a.csv": true });
  prefs.set("C:/data/a.csv", false);
  assert.equal(prefs.get("C:/data/a.csv"), false);
  assert.deepEqual(JSON.parse(storage.getItem(HEADERLESS_PREF_KEY)), {});
});

test("不同文件互不影响", () => {
  const prefs = createHeaderlessPrefs(makeStorage(), { platform: "win32" });
  prefs.set("C:/data/a.csv", true);
  prefs.set("C:/data/b.csv", true);
  prefs.set("C:/data/b.csv", false);
  assert.equal(prefs.get("C:/data/a.csv"), true);
  assert.equal(prefs.get("C:/data/b.csv"), false);
});

test("损坏的 JSON 静默回退，之后仍可正常写入", () => {
  const storage = makeStorage({ [HEADERLESS_PREF_KEY]: "{not json" });
  const prefs = createHeaderlessPrefs(storage, { platform: "win32" });
  assert.equal(prefs.get("C:/data/a.csv"), false);
  prefs.set("C:/data/a.csv", true);
  assert.equal(prefs.get("C:/data/a.csv"), true);
});

test("非对象 JSON（数组/字符串）也回退为空", () => {
  const arrayStorage = makeStorage({ [HEADERLESS_PREF_KEY]: "[true]" });
  assert.equal(
    createHeaderlessPrefs(arrayStorage, { platform: "win32" }).get("C:/a.csv"),
    false,
  );
  const strStorage = makeStorage({ [HEADERLESS_PREF_KEY]: "\"x\"" });
  assert.equal(
    createHeaderlessPrefs(strStorage, { platform: "win32" }).get("C:/a.csv"),
    false,
  );
});

test("Windows 路径忽略大小写与分隔符差异", () => {
  const prefs = createHeaderlessPrefs(makeStorage(), { platform: "win32" });
  prefs.set("C:\\Data\\A.csv", true);
  assert.equal(prefs.get("c:/data/a.csv"), true);
  assert.equal(prefs.get("C:/DATA/A.CSV"), true);
  assert.equal(prefs.get("C:\\Data\\\\A.csv"), true);
});

test("macOS 路径区分大小写，保留分隔符与尾斜杠归一化", () => {
  const prefs = createHeaderlessPrefs(makeStorage(), { platform: "darwin" });
  prefs.set("/Users/me/Data/A.csv", true);
  assert.equal(prefs.get("/Users/me/Data/A.csv"), true);
  assert.equal(prefs.get("/users/me/data/a.csv"), false);
  prefs.set("/Users/me/Data/B.csv/", true);
  assert.equal(prefs.get("/Users/me/Data/B.csv"), true);
});

test("normalizePath：折叠重复分隔符、去尾分隔符", () => {
  assert.equal(normalizePath("C:\\\\data\\\\a.csv", { platform: "win32" }), "c:/data/a.csv");
  assert.equal(normalizePath("/tmp//a.csv/", { platform: "darwin" }), "/tmp/a.csv");
});

test("storage 为 null 时读取为 false、写入/清空不抛错", () => {
  const prefs = createHeaderlessPrefs(null, { platform: "win32" });
  assert.equal(prefs.get("C:/a.csv"), false);
  assert.doesNotThrow(() => prefs.set("C:/a.csv", true));
  assert.doesNotThrow(() => prefs.clear());
});

test("storage 抛异常（配额/隐私模式）时静默回退", () => {
  const storage = {
    getItem: () => { throw new Error("denied"); },
    setItem: () => { throw new Error("quota"); },
    removeItem: () => { throw new Error("denied"); },
  };
  const prefs = createHeaderlessPrefs(storage, { platform: "win32" });
  assert.equal(prefs.get("C:/a.csv"), false);
  assert.doesNotThrow(() => prefs.set("C:/a.csv", true));
  assert.equal(prefs.get("C:/a.csv"), false);
  assert.doesNotThrow(() => prefs.clear());
});

test("clear 清空全部记录", () => {
  const storage = makeStorage();
  const prefs = createHeaderlessPrefs(storage, { platform: "win32" });
  prefs.set("C:/a.csv", true);
  prefs.clear();
  assert.equal(prefs.get("C:/a.csv"), false);
  assert.equal(storage.getItem(HEADERLESS_PREF_KEY), null);
});

test("撤销预算默认 64，损坏与越界回退 64", () => {
  const storage = makeStorage();
  const prefs = createUndoBudgetPrefs(storage);
  assert.equal(prefs.get(), 64);
  for (const raw of ["", "abc", "0", "1025", "1.5", "null", "{}"]) {
    storage.setItem(UNDO_BUDGET_PREF_KEY, raw);
    assert.equal(prefs.get(), 64, raw);
  }
  storage.setItem(UNDO_BUDGET_PREF_KEY, "128");
  assert.equal(prefs.get(), 128);
  assert.equal(undoBudgetBytes(128), 128 * 1024 * 1024);
});

test("撤销预算持久化合法整数且 storage 失败不阻断", () => {
  const storage = makeStorage();
  const prefs = createUndoBudgetPrefs(storage);
  assert.equal(prefs.set(1024), 1024);
  assert.equal(prefs.get(), 1024);
  assert.equal(storage.getItem(UNDO_BUDGET_PREF_KEY), "1024");
  assert.equal(createUndoBudgetPrefs(null).get(), 64);
  const denied = createUndoBudgetPrefs({
    getItem() { throw new Error("denied"); },
    setItem() { throw new Error("quota"); },
  });
  assert.equal(denied.get(), 64);
  assert.equal(denied.set(128), 128);
  assert.equal(denied.get(), 64);
});

test("撤销设置从旧 v1 迁移且双值持久化", async () => {
  const { createUndoSettingsPrefs, UNDO_SETTINGS_PREF_KEY } = await import("../frontend/js/prefs.js");
  assert.equal(typeof createUndoSettingsPrefs, "function");
  const storage = makeStorage({ [UNDO_BUDGET_PREF_KEY]: "32" });
  const prefs = createUndoSettingsPrefs(storage);
  assert.deepEqual(prefs.get(), { budgetMiB: 32, residentPermille: 1000 });
  assert.deepEqual(prefs.set({ budgetMiB: 128, residentPermille: 250 }),
    { budgetMiB: 128, residentPermille: 250 });
  assert.deepEqual(prefs.get(), { budgetMiB: 128, residentPermille: 250 });
  assert.deepEqual(JSON.parse(storage.getItem(UNDO_SETTINGS_PREF_KEY)),
    { budgetMiB: 128, residentPermille: 250 });
  assert.equal(storage.getItem(UNDO_BUDGET_PREF_KEY), "32", "迁移不删除旧设置");
  storage.setItem(UNDO_SETTINGS_PREF_KEY, "{broken");
  assert.deepEqual(prefs.get(), { budgetMiB: 32, residentPermille: 1000 });
});

test("驻留百分比按十进制文本解析且只允许 0.1% 步长", async () => {
  const { parseResidentPercent, normalizeResidentPermille } = await import("../frontend/js/prefs.js");
  assert.equal(typeof parseResidentPercent, "function");
  for (const [raw, expected] of [["0", 0], ["0.1", 1], ["100", 1000],
    ["999.9", 9999], ["1000.0", 10000], [" 2.5 ", 25]]) {
    assert.equal(parseResidentPercent(raw), expected, raw);
  }
  for (const raw of ["-1", "1000.1", "1.23", "abc", "", "NaN", "1.", ".1"]) {
    assert.equal(parseResidentPercent(raw), null, raw);
  }
  assert.equal(normalizeResidentPermille(250), 250);
  assert.equal(normalizeResidentPermille(10001), 1000);
  assert.equal(normalizeResidentPermille(-1), 1000);
});

test("有效预算使用整数向上取整和任意精度 BigInt", async () => {
  const { effectiveUndoBudgetBytes } = await import("../frontend/js/prefs.js");
  assert.equal(typeof effectiveUndoBudgetBytes, "function");
  assert.equal(effectiveUndoBudgetBytes({ budgetMiB: 1, residentPermille: 1,
    fileBytes: 1001n, memoryMode: true }), 1024n * 1024n);
  assert.equal(effectiveUndoBudgetBytes({ budgetMiB: 0, residentPermille: 1,
    fileBytes: 1001n, memoryMode: true }), 2n);
  assert.equal(effectiveUndoBudgetBytes({ budgetMiB: 1, residentPermille: 10000,
    fileBytes: 1001n, memoryMode: false }), 1024n * 1024n);
  const huge = (1n << 70n) + 1n;
  assert.equal(effectiveUndoBudgetBytes({ budgetMiB: 0, residentPermille: 10000,
    fileBytes: huge, memoryMode: true }), (huge * 10000n + 999n) / 1000n);
});

test("任意精度字节格式化不先转成 Number", async () => {
  const { formatByteQuantity } = await import("../frontend/js/prefs.js");
  assert.equal(typeof formatByteQuantity, "function");
  assert.equal(formatByteQuantity(512n), "512 B");
  assert.equal(formatByteQuantity(1536n), "1.50 KiB");
  assert.equal(formatByteQuantity(3n * 1024n * 1024n * 1024n), "3.00 GiB");
  assert.equal(formatByteQuantity(1n << 70n), "1048576.00 PiB");
});
