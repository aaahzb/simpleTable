// prefs.js 单元测试：按文件路径记忆「首行非列名」开关。
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  HEADERLESS_PREF_KEY,
  createHeaderlessPrefs,
  normalizePath,
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
