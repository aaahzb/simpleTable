import { spawn } from "node:child_process";
import { once } from "node:events";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { pathToFileURL } from "node:url";
import { decodeCounterCsv, parseCsvLine } from "./probe-physical-reads.mjs";

/** 按采样时间窗口汇总每个盘符；边界采样桶保守计入，不能用进程 I/O 替代。 */
export function summarizeCounterWindows(text, startAt = null, endAt = null) {
  const lines = text.split(/\r?\n/).filter((line) => line.startsWith('"'));
  const result = {
    covered: false,
    drives: {},
    fileApi: { status: "not_collected", detail: "文件 API 计数不是物理盘证据，本探针未采集" },
    processIo: { status: "not_collected", detail: "进程 I/O 包含缓存命中，本探针未采集" },
  };
  if (lines.length < 2) return result;
  const rows = lines.map(parseCsvLine);
  const timedRows = rows.slice(1).flatMap((row) => {
    const timestamp = Date.parse(row[0]);
    return Number.isFinite(timestamp) && row.length === rows[0].length
      ? [{ timestamp, row }] : [];
  });
  const bounded = startAt !== null && endAt !== null;
  const before = !bounded || timedRows.some((row) => row.timestamp <= startAt);
  const after = !bounded || timedRows.some((row) => row.timestamp >= endAt);
  const selected = bounded
    ? timedRows.filter((row) => row.timestamp >= startAt - 1000
      && row.timestamp <= endAt + 1000)
    : timedRows;
  result.covered = before && after && selected.length > 0;
  const groups = new Map();
  rows[0].forEach((name, index) => {
    const match = name.match(/physicaldisk\(([^)]+)\)/i);
    if (!match || !name.toLowerCase().includes("disk read bytes/sec")) return;
    for (const drive of match[1].toUpperCase().split(/\s+/)) {
      if (!/^[A-Z]:$/.test(drive)) continue;
      if (!groups.has(drive)) groups.set(drive, []);
      groups.get(drive).push(index);
    }
  });
  for (const [drive, indexes] of groups) {
    let readBytes = 0;
    let sampleCount = 0;
    for (const { row } of selected) {
      const values = indexes.map((index) => Number(row[index]));
      if (values.some((value) => !Number.isFinite(value) || value < 0)) continue;
      readBytes += values.reduce((sum, value) => sum + value, 0);
      sampleCount += 1;
    }
    const valid = result.covered && sampleCount > 0;
    result.drives[drive] = {
      status: !valid ? "unverified" : readBytes === 0 ? "zero" : "observed",
      readBytes: valid ? readBytes : null,
      sampleCount,
      counterColumns: indexes.map((index) => rows[0][index]),
      detail: !valid ? "采样窗口未完整覆盖点击到完成"
        : readBytes === 0 ? "该物理盘在覆盖窗口内读取为零"
          : "该物理盘有读取，但不能据此归因于 simpletable",
    };
  }
  return result;
}

/** 只有完整覆盖、内存充足且源盘为零才允许 verified_zero。 */
export function uiProbeStatus(covered, capacitySufficient, sourceStatus) {
  if (!covered || !capacitySufficient) return "unverified";
  if (sourceStatus === "zero") return "verified_zero";
  if (sourceStatus === "observed") return "reads_observed";
  return "unverified";
}

function parseArgs(args) {
  const result = {};
  for (let index = 0; index < args.length; index += 2) {
    if (!args[index]?.startsWith("--") || index + 1 >= args.length) {
      throw new Error("参数必须为 --name value");
    }
    result[args[index].slice(2)] = args[index + 1];
  }
  return result;
}

async function freePort() {
  const server = net.createServer();
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const port = server.address().port;
  server.close();
  await once(server, "close");
  return port;
}

async function connectCdp(port) {
  const deadline = Date.now() + 30000;
  let lastError = null;
  while (Date.now() < deadline) {
    try {
      const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      const target = targets.find((item) => item.type === "page" && item.webSocketDebuggerUrl);
      if (!target) throw new Error("尚未出现 WebView2 page target");
      const socket = new WebSocket(target.webSocketDebuggerUrl);
      await new Promise((resolve, reject) => {
        socket.addEventListener("open", resolve, { once: true });
        socket.addEventListener("error", reject, { once: true });
      });
      const pending = new Map();
      let sequence = 0;
      socket.addEventListener("message", (event) => {
        const message = JSON.parse(String(event.data));
        const request = pending.get(message.id);
        if (!request) return;
        pending.delete(message.id);
        if (message.error) request.reject(new Error(JSON.stringify(message.error)));
        else request.resolve(message.result);
      });
      socket.addEventListener("close", () => {
        for (const request of pending.values()) request.reject(new Error("CDP 已关闭"));
        pending.clear();
      });
      const call = (method, params = {}) => new Promise((resolve, reject) => {
        const id = ++sequence;
        pending.set(id, { resolve, reject });
        socket.send(JSON.stringify({ id, method, params }));
      });
      const evaluate = async (expression) => {
        const response = await call("Runtime.evaluate", {
          expression, awaitPromise: true, returnByValue: true,
        });
        if (response.exceptionDetails) {
          throw new Error(JSON.stringify(response.exceptionDetails));
        }
        return response.result?.value;
      };
      return { socket, evaluate };
    } catch (error) {
      lastError = error;
      await delay(250);
    }
  }
  throw new Error(`连接 WebView2 CDP 失败：${lastError?.message ?? lastError}`);
}

async function waitFor(evaluate, expression, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  let lastError = null;
  while (Date.now() < deadline) {
    try {
      if (await evaluate(expression)) return;
    } catch (error) {
      lastError = error;
    }
    await delay(250);
  }
  throw new Error(`等待${label}超时：${lastError?.message ?? "条件未满足"}`);
}

function startCounter(csvPath, seconds) {
  const child = spawn("typeperf", [
    "\\PhysicalDisk(*)\\Disk Read Bytes/sec",
    "-si", "1", "-sc", String(seconds), "-f", "CSV", "-o", csvPath,
  ], { stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
  let text = "";
  child.stdout.on("data", (chunk) => { text += chunk.toString("utf8"); });
  child.stderr.on("data", (chunk) => { text += chunk.toString("utf8"); });
  return {
    child,
    spawned: once(child, "spawn"),
    exited: once(child, "exit").then(([code]) => ({ code, text })),
  };
}

/** 只结束本脚本启动的 PID 及其子进程树，绝不按 exe 名称清理。 */
async function stopOwnedTree(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  const killer = spawn("taskkill", ["/PID", String(child.pid), "/T", "/F"], {
    stdio: "ignore", windowsHide: true,
  });
  await once(killer, "exit");
}

async function runProbe(args) {
  if (process.platform !== "win32") throw new Error("界面 PhysicalDisk 探针仅支持 Windows");
  const options = parseArgs(args);
  const exe = path.resolve(options.exe ?? "");
  const source = path.resolve(options.file ?? "");
  const seconds = Number(options.seconds ?? "180");
  const authorized = path.resolve("E:/simpletable_hdd_test/bench_22gb.csv");
  if (!fs.existsSync(exe) || !fs.statSync(exe).isFile()) throw new Error("release simpletable.exe 不存在");
  if (!fs.existsSync(source) || !fs.statSync(source).isFile()) throw new Error("授权合成源文件不存在");
  if (path.basename(source).toLowerCase() === "price_j8888.csv") throw new Error("禁止读取真实业务文件");
  if (source !== authorized && options["synthetic-copy"] !== "yes") {
    throw new Error("独立合成副本必须显式传 --synthetic-copy yes");
  }
  if (!Number.isInteger(seconds) || seconds < 30 || seconds > 600) {
    throw new Error("seconds 必须为 30–600 的整数");
  }
  const drive = path.parse(source).root.slice(0, 2).toUpperCase();
  if (!/^[A-Z]:$/.test(drive)) throw new Error("源文件必须位于本地盘符路径");
  const outputDir = path.resolve(options["output-dir"]
    ?? path.join("src-tauri", "target", `ui-physical-read-${Date.now()}`));
  if (fs.existsSync(outputDir)) throw new Error("输出目录必须不存在，避免覆盖旧证据");
  fs.mkdirSync(outputDir, { recursive: true });
  const reportPath = path.join(outputDir, "physical-read.json");
  const port = await freePort();
  const child = spawn(exe, [source], {
    stdio: "ignore",
    windowsHide: true,
    env: {
      ...process.env,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:
        `${process.env.WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS ?? ""} --remote-debugging-port=${port}`,
    },
  });
  const appExited = once(child, "exit");
  let cdp = null;
  let conversionCounter = null;
  try {
    cdp = await connectCdp(port);
    const { evaluate } = cdp;
    await waitFor(evaluate, `Boolean(window.__app?.model?.meta()
      && window.__app.getPath()?.toLowerCase() === ${JSON.stringify(source.toLowerCase())}
      && !window.__app.model.indexing())`, 600000, "正常界面打开和完整索引");
    const estimate = await evaluate("window.__app.model.memoryEstimate()");
    if (!estimate?.can_enter) throw new Error(`界面不允许进入内存模式：${estimate?.reason ?? "未知原因"}`);
    const capacitySufficient = estimate.available_physical_bytes >= estimate.file_bytes;

    const baselinePath = path.join(outputDir, "baseline-typeperf.csv");
    const baselineStartedAt = Date.now();
    const baselineCounter = startCounter(baselinePath, 5);
    await baselineCounter.spawned;
    const baselineCounterResult = await baselineCounter.exited;
    const baselineFinishedAt = Date.now();
    const baseline = summarizeCounterWindows(decodeCounterCsv(fs.readFileSync(baselinePath)));
    baseline.startedAt = baselineStartedAt;
    baseline.finishedAt = baselineFinishedAt;
    baseline.counter = baselineCounterResult;

    const conversionPath = path.join(outputDir, "conversion-typeperf.csv");
    conversionCounter = startCounter(conversionPath, seconds);
    await conversionCounter.spawned;
    await delay(1200); // 保证点击前至少有一个 PhysicalDisk 样本，不是文件预热。
    const goAt = Date.now();
    await evaluate(`(() => {
      const button = document.getElementById("btn-memory-mode");
      if (!button || button.disabled || button.classList.contains("hidden")) throw new Error("内存模式按钮不可点击");
      button.click();
      return true;
    })()`);
    await waitFor(evaluate, `document.querySelector(".modal-mask")?.textContent
      .includes("转换私有内存上限")`, 30000, "内存模式确认框");
    await evaluate(`(() => {
      const mask = document.querySelector(".modal-mask");
      const yes = [...(mask?.querySelectorAll("button") ?? [])].find((button) => button.textContent === "是");
      if (!yes) throw new Error("确认框没有是按钮");
      yes.click();
      return true;
    })()`);
    await waitFor(evaluate, `window.__app.model.memoryMode()
      && window.__app.model.meta()?.ram_backed === true
      && window.__app.memoryOperationKind() === null`, seconds * 1000, "真实界面内存化完成");
    const resultAt = Date.now();
    await delay(1200); // 保留完成后的采样桶，再结束自己的 typeperf。
    await stopOwnedTree(conversionCounter.child);
    const conversionCounterResult = await conversionCounter.exited;
    const conversion = summarizeCounterWindows(
      decodeCounterCsv(fs.readFileSync(conversionPath)), goAt, resultAt);
    conversion.counter = conversionCounterResult;
    const sourceSummary = conversion.drives[drive]
      ?? { status: "unverified", readBytes: null, detail: `缺少 ${drive} PhysicalDisk 计数器` };
    const otherDrives = Object.fromEntries(Object.entries(conversion.drives)
      .filter(([name]) => name !== drive));
    const covered = conversion.covered;
    const report = {
      source,
      drive,
      exe,
      exeMtimeMs: fs.statSync(exe).mtimeMs,
      sourceMtimeMs: fs.statSync(source).mtimeMs,
      startup: "normal-release-cli-open",
      explicitPreload: false,
      indexingCompleted: true,
      estimate,
      capacitySufficient,
      goAt,
      resultAt,
      conversionMs: resultAt - goAt,
      baseline,
      conversion: sourceSummary,
      otherDrives,
      fileApi: conversion.fileApi,
      processIo: conversion.processIo,
      conversionSamples: conversion.drives,
      covered,
      status: uiProbeStatus(covered, capacitySufficient, sourceSummary.status),
      reportPath,
    };
    return report;
  } finally {
    if (conversionCounter) await stopOwnedTree(conversionCounter.child);
    if (cdp) {
      try { await cdp.evaluate('window.__app.invoke("close_window")'); } catch { /* 随后只清理自己的树 */ }
      cdp.socket.close();
    }
    await Promise.race([appExited, delay(5000)]);
    await stopOwnedTree(child);
  }
}

const invoked = process.argv[1]
  && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href;
if (invoked) {
  runProbe(process.argv.slice(2)).then((report) => {
    fs.writeFileSync(report.reportPath, `${JSON.stringify(report, null, 2)}\n`, {
      encoding: "utf8", flag: "wx",
    });
    console.log(JSON.stringify(report, null, 2));
    process.exitCode = report.status === "verified_zero"
      ? 0 : report.status === "reads_observed" ? 1 : 2;
  }).catch((error) => {
    const outputDir = path.resolve("src-tauri", "target", `ui-physical-read-error-${Date.now()}`);
    fs.mkdirSync(outputDir, { recursive: true });
    const reportPath = path.join(outputDir, "physical-read.json");
    const report = { status: "unverified", detail: error?.stack ?? String(error), reportPath };
    fs.writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`, {
      encoding: "utf8", flag: "wx",
    });
    console.error(JSON.stringify(report, null, 2));
    process.exitCode = 2;
  });
}
