import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { test } from "node:test";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const exists = (path) => existsSync(new URL(`../${path}`, import.meta.url));

test("README is a concise product entry with documentation links", () => {
  const readme = read("README.md");
  assert.ok(readme.split(/\r?\n/).length <= 130, "首页不得超过 130 行");
  for (const text of ["## 下载", "## 核心亮点", "## 上手 30 秒", "docs/guide.md", "docs/development.md"]) {
    assert.ok(readme.includes(text), `首页缺少 ${text}`);
  }
  assert.doesNotMatch(readme, /^#{2,4} (实测性能|常见问题|项目结构)$/m);
  assert.match(readme, /docs\/img\/main\.png/);
});

test("manual and development details remain available in layered docs", () => {
  assert.ok(exists("docs/guide.md"), "用户指南必须存在");
  assert.ok(exists("docs/development.md"), "开发文档必须存在");
  const guide = read("docs/guide.md");
  const development = read("docs/development.md");
  for (const heading of ["TXT/LOG 纯文本逐行", "实测性能", "常用快捷键", "常见问题"]) {
    assert.ok(guide.includes(heading), `用户指南缺少 ${heading}`);
  }
  for (const text of ["cargo test", "verify-local.mjs", "项目结构", "平台验收边界"]) {
    assert.ok(development.includes(text), `开发文档缺少 ${text}`);
  }
  for (const match of read("README.md").matchAll(/\]\((docs\/[^)]+)\)/g)) {
    assert.ok(exists(match[1]), `失效相对链接：${match[1]}`);
  }
});

test("README uses three current PNG screenshots and no stale JPEG", () => {
  const readme = read("README.md");
  for (const image of ["main.png", "split-view.png", "text-lines.png"]) {
    assert.ok(readme.includes(`docs/img/${image}`), `首页未引用 ${image}`);
    assert.ok(exists(`docs/img/${image}`), `正式截图 ${image} 不存在`);
  }
  assert.ok(!readme.includes("docs/img/main.jpg"), "旧图引用必须移除");
  assert.ok(!exists("docs/img/main.jpg"), "旧 JPEG 必须在新图验收后删除");
});
