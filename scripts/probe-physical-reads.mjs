import { spawn } from "node:child_process";
import { once } from "node:events";
import fs from "node:fs";
import path from "node:path";
import readline from "node:readline";
import { setTimeout as delay } from "node:timers/promises";
import { pathToFileURL } from "node:url";

/** 按 BOM 解码 typeperf 输出；无 BOM 的计数器文本按 UTF-8/ASCII 处理。 */
export function decodeCounterCsv(buffer) {
  if (buffer.length >= 2 && buffer[0] === 0xff && buffer[1] === 0xfe) {
    return buffer.subarray(2).toString("utf16le");
  }
  if (buffer.length >= 3 && buffer[0] === 0xef && buffer[1] === 0xbb && buffer[2] === 0xbf) {
    return buffer.subarray(3).toString("utf8");
  }
  return buffer.toString("utf8");
}

/** 解析一行 RFC 4180 风格 CSV，支持双引号转义。 */
export function parseCsvLine(line) {
  const fields = [];
  let field = "";
  let quoted = false;
  for (let index = 0; index < line.length; index += 1) {
    const char = line[index];
    if (quoted) {
      if (char === '"' && line[index + 1] === '"') {
        field += '"';
        index += 1;
      } else if (char === '"') {
        quoted = false;
      } else {
        field += char;
      }
    } else if (char === '"') {
      quoted = true;
    } else if (char === ",") {
      fields.push(field);
      field = "";
    } else {
      field += char;
    }
  }
  if (quoted) throw new Error("计数器 CSV 引号未闭合");
  fields.push(field);
  return fields;
}

/** 只汇总源文件所在盘符对应的 PhysicalDisk 实例。 */
export function summarizeDiskReads(text, drive, intervalSeconds = 1) {
  const lines = text.split(/\r?\n/).filter((line) => line.startsWith('"'));
  if (lines.length < 2) {
    return {
      status: "unverified",
      readBytes: null,
      sampleCount: 0,
      counterColumns: [],
      detail: "缺少计数器样本",
    };
  }
  const rows = lines.map(parseCsvLine);
  const wanted = drive.toUpperCase();
  const indexes = rows[0].flatMap((name, index) => {
    const match = name.match(/physicaldisk\(([^)]+)\)/i);
    const instance = match?.[1] ?? "";
    return name.toLowerCase().includes("disk read bytes/sec")
      && instance.toUpperCase().split(/\s+/).includes(wanted) ? [index] : [];
  });
  const counterColumns = indexes.map((index) => rows[0][index]);
  if (indexes.length === 0) {
    return {
      status: "unverified",
      readBytes: null,
      sampleCount: 0,
      counterColumns,
      detail: `未找到 ${wanted} 的 PhysicalDisk 实例`,
    };
  }
  let readBytes = 0;
  let sampleCount = 0;
  for (const row of rows.slice(1)) {
    if (row.length !== rows[0].length) continue;
    const values = indexes.map((index) => row[index] === "" ? null : Number(row[index]));
    if (values.some((value) => value === null || !Number.isFinite(value) || value < 0)) continue;
    readBytes += values.reduce((sum, value) => sum + value, 0) * intervalSeconds;
    sampleCount += 1;
  }
  if (sampleCount === 0) {
    return {
      status: "unverified",
      readBytes: null,
      sampleCount,
      counterColumns,
      detail: "目标盘没有有效数值样本",
    };
  }
  const status = readBytes === 0 ? "zero" : "observed";
  return {
    status,
    readBytes,
    sampleCount,
    counterColumns,
    detail: status === "zero"
      ? "目标物理盘在采样区间读取为零"
      : "目标物理盘有读取，需排除其他进程噪声",
  };
}

/** 合成最终报告，避免汇总状态覆盖带覆盖性判定的最终状态。 */
export function finalizeProbeReport(covered, summary, fields) {
  const status = covered && summary.status === "zero"
    ? "verified_zero"
    : covered && summary.status === "observed" ? "reads_observed" : "unverified";
  return { ...fields, covered, ...summary, status };
}

function parseArgs(args) {
  const result = {};
  for (let index = 0; index < args.length; index += 2) {
    if (!args[index].startsWith("--") || index + 1 >= args.length) {
      throw new Error("参数必须为 --name value");
    }
    result[args[index].slice(2)] = args[index + 1];
  }
  return result;
}

async function runProbe(args) {
  if (process.platform !== "win32") throw new Error("PhysicalDisk 探针仅支持 Windows");
  const options = parseArgs(args);
  const exe = path.resolve(options.exe ?? "");
  const source = path.resolve(options.file ?? "");
  const workers = Number(options.workers ?? "1");
  const seconds = Number(options.seconds ?? "60");
  if (!fs.existsSync(exe) || !fs.statSync(exe).isFile()) throw new Error("memory_peak exe 不存在");
  if (!fs.existsSync(source) || !fs.statSync(source).isFile()) throw new Error("合成源文件不存在");
  if (![1, 2, 4].includes(workers)) throw new Error("workers 必须为 1、2、4");
  if (!Number.isInteger(seconds) || seconds < 5 || seconds > 120) {
    throw new Error("seconds 必须为 5–120 的整数");
  }
  const drive = path.parse(source).root.slice(0, 2).toUpperCase();
  if (!/^[A-Z]:$/.test(drive)) throw new Error("源文件必须位于本地盘符路径");
  const outputDir = path.resolve(
    options["output-dir"] ?? path.join("src-tauri", "target", `physical-read-${Date.now()}`),
  );
  fs.mkdirSync(outputDir, { recursive: false });
  const csvPath = path.join(outputDir, "typeperf.csv");
  const reportPath = path.join(outputDir, "physical-read.json");
  const child = spawn(exe, [
    "child",
    "--scenario", "convert",
    "--file", source,
    "--workers", String(workers),
    "--preload", "true",
  ], { stdio: ["pipe", "pipe", "pipe"], windowsHide: true });
  const childExited = once(child, "exit");
  const lines = readline.createInterface({ input: child.stdout });
  const iterator = lines[Symbol.asyncIterator]();
  let stderr = "";
  child.stderr.on("data", (chunk) => { stderr += chunk.toString("utf8"); });
  const nextProtocol = async (prefix) => {
    while (true) {
      const { value, done } = await iterator.next();
      if (done) throw new Error(`等待 ${prefix} 时子进程退出：${stderr}`);
      if (value.startsWith("ERROR ")) throw new Error(value.slice(6));
      if (value.startsWith(prefix)) return value.slice(prefix.length);
    }
  };
  try {
    await nextProtocol("READY ");
    const counter = spawn("typeperf", [
      "\\PhysicalDisk(*)\\Disk Read Bytes/sec",
      "-si", "1",
      "-sc", String(seconds),
      "-f", "CSV",
      "-o", csvPath,
    ], { stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
    let counterText = "";
    counter.stdout.on("data", (chunk) => { counterText += chunk.toString("utf8"); });
    counter.stderr.on("data", (chunk) => { counterText += chunk.toString("utf8"); });
    const spawned = once(counter, "spawn");
    const exited = once(counter, "exit").then(([code]) => ({
      code,
      at: Date.now(),
      text: counterText,
    }));
    await spawned;
    await delay(1100);
    const goAt = Date.now();
    child.stdin.write("GO\n");
    const resultText = await nextProtocol("RESULT ");
    const resultAt = Date.now();
    const counterResult = await exited;
    child.stdin.write("DONE\n");
    const [childCode] = await childExited;
    if (childCode !== 0) throw new Error(`转换子进程退出码 ${childCode}：${stderr}`);
    const summary = summarizeDiskReads(decodeCounterCsv(fs.readFileSync(csvPath)), drive);
    const covered = counterResult.code === 0 && counterResult.at >= resultAt;
    return finalizeProbeReport(covered, summary, {
      source,
      drive,
      workers,
      goAt,
      resultAt,
      conversionMs: resultAt - goAt,
      childResult: JSON.parse(resultText),
      counterText: counterResult.text,
      reportPath,
    });
  } finally {
    if (child.exitCode === null && child.stdin.writable) child.stdin.end();
    lines.close();
  }
}

const invoked = process.argv[1]
  && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href;
if (invoked) {
  runProbe(process.argv.slice(2)).then((report) => {
    fs.writeFileSync(
      report.reportPath,
      `${JSON.stringify(report, null, 2)}\n`,
      { encoding: "utf8", flag: "wx" },
    );
    console.log(JSON.stringify(report, null, 2));
    process.exitCode = report.status === "verified_zero"
      ? 0
      : report.status === "reads_observed" ? 1 : 2;
  }).catch((error) => {
    const outputDir = path.resolve("src-tauri", "target", `physical-read-error-${Date.now()}`);
    fs.mkdirSync(outputDir, { recursive: true });
    const reportPath = path.join(outputDir, "physical-read.json");
    const report = { status: "unverified", detail: error?.stack ?? String(error), reportPath };
    fs.writeFileSync(
      reportPath,
      `${JSON.stringify(report, null, 2)}\n`,
      { encoding: "utf8", flag: "wx" },
    );
    console.error(JSON.stringify(report, null, 2));
    process.exitCode = 2;
  });
}
