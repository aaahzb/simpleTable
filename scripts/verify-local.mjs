import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const tauri = path.join(root, "src-tauri");
const exes = [
  ["debug", path.join(tauri, "target", "debug", "simpletable.exe")],
  ["release", path.join(tauri, "target", "release", "simpletable.exe")],
];

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: tauri,
    stdio: "inherit",
    windowsHide: true,
    ...options,
  });
  if (result.error) throw result.error;
  return result.status;
}

function newestMtime(dir) {
  let newest = 0;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const file = path.join(dir, entry.name);
    const stat = fs.statSync(file);
    if (entry.isDirectory()) newest = Math.max(newest, newestMtime(file));
    else newest = Math.max(newest, stat.mtimeMs);
  }
  return newest;
}

/**
 * 强校验 smoke 报告的实际断言行、唯一汇总和清理探针。
 *
 * @param {string} text - UTF-8 报告全文。
 * @param {(path: string) => boolean} pathExists - 清理路径存在性检查。
 * @returns {{passes: number, failures: number, total: number, failed: number,
 *   cleanupPaths: string[]}} 实际行数、汇总数字和清理路径。
 * @throws {Error} 报告缺项、计数不一致或清理路径仍存在。
 */
export function validateSmokeReport(text, pathExists) {
  const lines = text.split(/\r?\n/);
  const passes = lines.filter((line) => line.startsWith("✓")).length;
  const failures = lines.filter((line) => line.startsWith("✗")).length;
  const summaries = [...text.matchAll(/^合计[ \t]+(\d+)[ \t]+项，失败[ \t]+(\d+)[ \t]+项[ \t]*\r?$/gm)];
  if (summaries.length !== 1) throw new Error("冒烟报告必须包含唯一合计汇总");
  const total = Number(summaries[0][1]);
  const failed = Number(summaries[0][2]);
  if (!Number.isSafeInteger(total) || total <= 0) throw new Error("冒烟汇总总数必须大于 0");
  if (!Number.isSafeInteger(failed)) throw new Error("冒烟汇总失败数无效");
  if (passes + failures !== total) {
    throw new Error(`实际 ✓/✗ 行数与汇总总数不一致：${passes}+${failures} != ${total}`);
  }
  if (failures !== failed) {
    throw new Error(`实际 ✗ 行数与汇总失败数不一致：${failures} != ${failed}`);
  }
  const cleanupPaths = [...text.matchAll(/^CLEANUP_PATH=(.+)$/gm)]
    .map((match) => match[1].trim());
  if (cleanupPaths.length < 1) throw new Error("冒烟报告缺少 CLEANUP_PATH");
  for (const cleanupPath of cleanupPaths) {
    if (pathExists(cleanupPath)) throw new Error(`退出后临时目录仍存在：${cleanupPath}`);
  }
  return { passes, failures, total, failed, cleanupPaths };
}

try {
  const before = new Map(exes.map(([, exe]) => [exe,
    fs.existsSync(exe) ? fs.statSync(exe).mtimeMs : 0]));
  if (run("cargo", ["build"]) !== 0) throw new Error("debug build failed");
  if (run("cargo", ["build", "--release"]) !== 0) {
    throw new Error("release build failed");
  }
  const frontendMtime = newestMtime(path.join(root, "frontend"));
  for (const [kind, exe] of exes) {
    const mtime = fs.statSync(exe).mtimeMs;
    if (mtime <= before.get(exe)) throw new Error(`${kind} exe timestamp did not advance`);
    if (mtime < frontendMtime) throw new Error(`${kind} exe embeds stale frontend`);
  }
  for (const [kind, exe] of exes) {
    const report = path.join(tauri, "target", `smoke-${kind}-${Date.now()}.txt`);
    const status = run(exe, ["--smoke"], {
      env: { ...process.env, SMOKE_REPORT_PATH: report },
      timeout: 180_000,
    });
    if (!fs.existsSync(report)) throw new Error(`${kind} smoke report missing: ${report}`);
    const text = fs.readFileSync(report, "utf8");
    process.stdout.write(`\n${kind} report: ${report}\n${text}\n`);
    const stats = validateSmokeReport(text, fs.existsSync);
    if (status !== 0) throw new Error(`${kind} smoke exited ${status}`);
    if (stats.failures !== 0) throw new Error(`${kind} smoke has ${stats.failures} failures`);
  }
  console.log("dual build, embed timestamps, and dual smoke: PASS");
} catch (error) {
  console.error(error?.stack ?? error);
  process.exitCode = 1;
}
