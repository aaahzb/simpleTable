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
    if (status !== 0) throw new Error(`${kind} smoke exited ${status}`);
    if (!/合计\s+\d+\s+项，失败\s+0\s+项/.test(text)) {
      throw new Error(`${kind} smoke summary is not zero-failure`);
    }
  }
  console.log("dual build, embed timestamps, and dual smoke: PASS");
} catch (error) {
  console.error(error?.stack ?? error);
  process.exitCode = 1;
}
