# 首次 mmap 预建块视图转换 Implementation Plan

> **2026-11-13 状态：历史计划。** 预建独立块视图与预热探针已被 `2026-11-13-m0-direct-copy-ram-materialization.md` 的 M0 直拷和真实界面验收取代，不能作为当前实现或真实界面零读盘证据；下方勾选与数值保持历史原样。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 首次 mmap 转匿名 RAM 时，在完整源映射仍有效时预建全部独立块视图，随后直接并行复制到最终连续 RAM，并复制一块释放一块，同时实测热缓存物理读盘。

**Architecture:** `ram_load` 新增 `ChunkCopy` 数据源抽象和 `MappedChunks`，文件定位读取与预建块视图复制明确分型；`RamRegion` 继续独占连续目标及并行执行器。`engine` 只在全部块视图和目标区域准备完成后解除完整 mmap，成功、取消和失败继续复用现有后备提交与恢复。独立 Node 探针驱动真实 `memory_peak child`，用 Windows PhysicalDisk 计数器覆盖完整转换区间。

**Tech Stack:** Rust 2021、memmap2 0.9、windows-sys 0.59、macOS libc、Node.js `node:test`、Windows `typeperf`/PhysicalDisk PDH 计数器、Tauri 2。

## Global Constraints

- 工作目录固定为当前 worktree，所有命令从 `D:\SVN_Cursor\simpleTable-public\.claude\worktrees\feat+memory-mode` 执行，不进入原始仓库根目录。
- 未经用户明确要求不提交 Git、不推送、不创建标签或 PR；任务终点是测试和 diff 检查点，不是 commit。
- 当前 03 未提交保存快路径必须完整保留，不回滚、不覆盖；本任务不修改 `src-tauri/src/engine/insert_save.rs`、`src-tauri/src/engine/insert_save_tests.rs`、`src-tauri/src/ram_region/streaming_tests.rs`。
- 通用保存和保存后的文件重载继续使用文件 `PositionedRead`，不得改成预建块视图。
- 完整 M0 只能在全部块视图和目标 `RamRegion` 准备成功后解除；任何旧 M0 地址都不得用于复制。
- 默认逻辑块为 8 MiB，最多 4 个 worker；非对齐测试块必须按 Windows 分配粒度或 macOS 页大小对齐映射。
- 私有内存硬门槛保持 `floor(X × 1.05)`，Windows 使用 `PrivateUsage`，macOS 使用 `internal + compressed`；工作集和文件缓存不得混入该公式。
- 物理盘探针只有在计数器覆盖完整转换、目标盘实例匹配且读取量为零时才输出 `verified_zero`。普通进程 I/O、ProcMon 文件事件和 `read_at` 次数不得冒充物理读盘证明。
- 只读使用 `E:\simpletable_hdd_test\bench_22gb.csv` 这一授权合成基准，不修改原文件；绝不读取或试验 `price_j8888.csv`。
- 新文件统一 UTF-8；读取计数器 CSV 时先检测 UTF-16LE/UTF-8 BOM。新增 Rust 函数写明用途、参数、返回和错误，unsafe 块写明安全依据。
- 每个新行为先写失败测试并亲见预期 RED，再写最小实现并复跑 GREEN。产品改动后必须重编本地 exe，最终运行 canonical 双构建双冒烟。
- 不删除已有文件，不覆盖既有基准、旧报告或用户未跟踪杂项。诊断输出写入新的唯一目录。

## File Structure

- Create `scripts/probe-physical-reads.mjs`：真实转换子进程与 PhysicalDisk 计数器对照探针。
- Create `tests_js/physical_read_probe.test.mjs`：CSV 编码、引号、盘符匹配和零读取判定测试。
- Modify `src-tauri/src/ram_load.rs`：`ChunkCopy`、`MappedChunks`、视图所有权与统计、通用执行器复用。
- Modify `src-tauri/src/engine.rs`：首次转换准备顺序、M0 生命周期、排序估算注释和引擎测试。
- Modify `README.md` 与 `docs/development.md`：新转换顺序、探针用法和 fresh 验收结果。
- Modify `docs/superpowers/specs/2026-11-12-mmap-chunk-views-design.md`：只澄清非对齐测试块的对齐视图可能重叠，默认 8 MiB 块不改变已批准架构。
- Modify `TODO.md`、`task_plan.md`、`progress.md`、`findings.md`：追加本任务状态和实际命令结果，保留既有记录。

---

### Task 1: 物理盘探针与旧路径基线

**Files:**
- Create: `scripts/probe-physical-reads.mjs`
- Create: `tests_js/physical_read_probe.test.mjs`
- Create outside version control: `src-tauri/target/mmap-chunk-baseline/read_at-memory_peak.exe`

**Interfaces:**
- Consumes: 现有 `memory_peak child --scenario convert --file <path> --workers <1|2|4> --preload true` 的 READY/GO/RESULT/DONE 协议。
- Produces: `decodeCounterCsv(buffer: Buffer): string`、`parseCsvLine(line: string): string[]`、`summarizeDiskReads(text: string, drive: string, intervalSeconds?: number): DiskSummary`，以及唯一目录中的 `physical-read.json` 和 `typeperf.csv`。

- [ ] **Step 1: 先写计数器解析失败测试**

创建 `tests_js/physical_read_probe.test.mjs`：

```javascript
import test from "node:test";
import assert from "node:assert/strict";
import {
  decodeCounterCsv,
  parseCsvLine,
  summarizeDiskReads,
} from "../scripts/probe-physical-reads.mjs";

const HEADER = '"(PDH-CSV 4.0)","\\\\HOST\\PhysicalDisk(2 C:)\\Disk Read Bytes/sec","\\\\HOST\\PhysicalDisk(0 E:)\\Disk Read Bytes/sec","\\\\HOST\\PhysicalDisk(_Total)\\Disk Read Bytes/sec"';

test("decodeCounterCsv detects UTF-16LE and UTF-8 BOM", () => {
  assert.equal(decodeCounterCsv(Buffer.from([0xff, 0xfe, 0x41, 0x00])), "A");
  assert.equal(decodeCounterCsv(Buffer.from([0xef, 0xbb, 0xbf, 0x41])), "A");
  assert.equal(decodeCounterCsv(Buffer.from("ABC", "utf8")), "ABC");
});

test("parseCsvLine preserves quoted commas and escaped quotes", () => {
  assert.deepEqual(parseCsvLine('"a,b","x""y","3.5"'), ["a,b", 'x"y', "3.5"]);
});

test("summarizeDiskReads selects only the source drive and proves zero", () => {
  const text = `${HEADER}\n"11/12/2026 20:00:00.000","9.0","0.0","9.0"\n"11/12/2026 20:00:01.000","8.0","0.0","8.0"`;
  const result = summarizeDiskReads(text, "E:");
  assert.equal(result.status, "zero");
  assert.equal(result.readBytes, 0);
  assert.equal(result.sampleCount, 2);
  assert.equal(result.counterColumns.length, 1);
});

test("summarizeDiskReads integrates nonzero rates without claiming source attribution", () => {
  const text = `${HEADER}\n"t0","0.0","10.0","10.0"\n"t1","0.0","20.0","20.0"`;
  const result = summarizeDiskReads(text, "E:");
  assert.equal(result.status, "observed");
  assert.equal(result.readBytes, 30);
  assert.match(result.detail, /目标物理盘/);
});

test("summarizeDiskReads rejects missing drive columns and samples", () => {
  assert.equal(summarizeDiskReads(HEADER, "E:").status, "unverified");
  const otherDrive = `${HEADER}\n"t0","0.0","0.0","0.0"`;
  assert.equal(summarizeDiskReads(otherDrive, "F:").status, "unverified");
});
```

- [ ] **Step 2: 运行测试并确认因探针模块不存在而 RED**

Run: `node --test tests_js/physical_read_probe.test.mjs`

Expected: FAIL，错误包含 `Cannot find module` 且指向 `scripts/probe-physical-reads.mjs`；不是测试语法错误。

- [ ] **Step 3: 实现完整探针**

创建 `scripts/probe-physical-reads.mjs`：

```javascript
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
    return { status: "unverified", readBytes: null, sampleCount: 0, counterColumns: [], detail: "缺少计数器样本" };
  }
  const rows = lines.map(parseCsvLine);
  const wanted = drive.toUpperCase();
  const indexes = rows[0].flatMap((name, index) => {
    const match = name.match(/physicaldisk\(([^)]+)\)/i);
    const instance = match?.[1] ?? "";
    return name.toLowerCase().includes("disk read bytes/sec")
      && instance.split(/\s+/).includes(wanted) ? [index] : [];
  });
  const counterColumns = indexes.map((index) => rows[0][index]);
  if (indexes.length === 0) {
    return { status: "unverified", readBytes: null, sampleCount: 0, counterColumns, detail: `未找到 ${wanted} 的 PhysicalDisk 实例` };
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
    return { status: "unverified", readBytes: null, sampleCount, counterColumns, detail: "目标盘没有有效数值样本" };
  }
  const status = readBytes === 0 ? "zero" : "observed";
  return {
    status,
    readBytes,
    sampleCount,
    counterColumns,
    detail: status === "zero" ? "目标物理盘在采样区间读取为零" : "目标物理盘有读取，需排除其他进程噪声",
  };
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
  if (!Number.isInteger(seconds) || seconds < 5 || seconds > 120) throw new Error("seconds 必须为 5–120 的整数");
  const drive = path.parse(source).root.slice(0, 2).toUpperCase();
  if (!/^[A-Z]:$/.test(drive)) throw new Error("源文件必须位于本地盘符路径");
  const outputDir = path.resolve(options["output-dir"] ?? path.join("src-tauri", "target", `physical-read-${Date.now()}`));
  fs.mkdirSync(outputDir, { recursive: false });
  const csvPath = path.join(outputDir, "typeperf.csv");
  const reportPath = path.join(outputDir, "physical-read.json");
  const child = spawn(exe, ["child", "--scenario", "convert", "--file", source, "--workers", String(workers), "--preload", "true"], { stdio: ["pipe", "pipe", "pipe"], windowsHide: true });
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
    const counter = spawn("typeperf", ["\\PhysicalDisk(*)\\Disk Read Bytes/sec", "-si", "1", "-sc", String(seconds), "-f", "CSV", "-o", csvPath], { stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
    let counterText = "";
    counter.stdout.on("data", (chunk) => { counterText += chunk.toString("utf8"); });
    counter.stderr.on("data", (chunk) => { counterText += chunk.toString("utf8"); });
    const spawned = once(counter, "spawn");
    const exited = once(counter, "exit").then(([code]) => ({ code, at: Date.now(), text: counterText }));
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
    const status = covered && summary.status === "zero"
      ? "verified_zero"
      : covered && summary.status === "observed" ? "reads_observed" : "unverified";
    return { source, drive, workers, goAt, resultAt, conversionMs: resultAt - goAt, covered, ...summary, status, childResult: JSON.parse(resultText), counterText: counterResult.text, reportPath };
  } finally {
    if (child.exitCode === null && child.stdin.writable) child.stdin.end();
    lines.close();
  }
}

const invoked = process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href;
if (invoked) {
  runProbe(process.argv.slice(2)).then((report) => {
    fs.writeFileSync(report.reportPath, `${JSON.stringify(report, null, 2)}\n`, { encoding: "utf8", flag: "wx" });
    console.log(JSON.stringify(report, null, 2));
    process.exitCode = report.status === "verified_zero" ? 0 : report.status === "reads_observed" ? 1 : 2;
  }).catch((error) => {
    const outputDir = path.resolve("src-tauri", "target", `physical-read-error-${Date.now()}`);
    fs.mkdirSync(outputDir, { recursive: true });
    const report = { status: "unverified", detail: error?.stack ?? String(error) };
    fs.writeFileSync(path.join(outputDir, "physical-read.json"), `${JSON.stringify(report, null, 2)}\n`, "utf8");
    console.error(JSON.stringify(report, null, 2));
    process.exitCode = 2;
  });
}
```

- [ ] **Step 4: 复跑探针单测并确认 GREEN**

Run: `node --test tests_js/physical_read_probe.test.mjs && node --check scripts/probe-physical-reads.mjs`

Expected: 6 passed、0 failed，语法检查退出 0。

- [ ] **Step 5: 保存旧转换器并采集旧路径基线**

```bash
cargo build --manifest-path src-tauri/Cargo.toml --release --bin memory_peak
mkdir -p src-tauri/target/mmap-chunk-baseline
cp -n src-tauri/target/release/memory_peak.exe src-tauri/target/mmap-chunk-baseline/read_at-memory_peak.exe
node scripts/probe-physical-reads.mjs \
  --exe src-tauri/target/mmap-chunk-baseline/read_at-memory_peak.exe \
  --file 'E:\simpletable_hdd_test\bench_22gb.csv' \
  --workers 1 --seconds 60 \
  --output-dir src-tauri/target/physical-read-baseline-$(date +%s)
```

Expected: 源文件大小保持 `2286250041` 字节，探针生成新 JSON。`verified_zero` 表示旧路径在该轮也没有物理盘读取；`reads_observed` 只表示目标盘有读取，需要保留计数器并排查噪声；`unverified` 不得当作通过。三种结果都如实记录，不能用此基线预先证明新路径收益。把实际输出目录和状态追加到 `progress.md`。

- [ ] **Step 6: 检查点**

Run: `git status --short`

Expected: 只新增本任务两个源码文件、计划文件和既有 03 改动；target 诊断文件被忽略。不提交。

---

### Task 2: `ChunkCopy` 与非对齐 `MappedChunks`

**Files:**
- Modify: `src-tauri/src/ram_load.rs`

**Interfaces:**
- Consumes: `PositionedRead`、`LoadOptions`、`RamRegion::reserve_with_enable_fault`、`RamRegion::for_each_chunk_mut`。
- Produces: `ChunkCopy: Sync { fn len(&self) -> Result<u64, String>; fn copy_chunk(&self, offset: u64, out: &mut [u8]) -> Result<(), String>; }`、`MappedChunks::new(file: &File, chunk_bytes: usize) -> Result<Self, String>`、`MappedChunks::chunk_count(&self) -> usize`。

- [ ] **Step 1: 写非对齐块、末尾块和关闭句柄后的失败测试**

在 `ram_load.rs` 测试模块加入：

```rust
#[test]
fn mapped_chunks_copy_unaligned_blocks_and_tail_after_file_close() {
    for (name, len, chunk) in [("single", 257usize, 1000usize), ("tail", 70_000, 1000)] {
        let (file, _cleanup) = temp_pattern_file(name, len);
        let source = MappedChunks::new(&file, chunk).unwrap();
        drop(file); // 映射复制不得保留或重新打开 File 句柄。
        let chunks = len.div_ceil(chunk);
        assert_eq!(source.chunk_count(), chunks);
        for start in (0..len).step_by(chunk) {
            let count = chunk.min(len - start);
            let mut out = vec![0u8; count];
            source.copy_chunk(start as u64, &mut out).unwrap();
            assert!(out.iter().enumerate().all(|(i, byte)| {
                *byte == ((start + i) % 251) as u8
            }));
        }
    }
}

#[test]
fn mapped_chunks_reject_zero_sized_inputs() {
    let (file, _cleanup) = temp_pattern_file("empty", 0);
    assert!(MappedChunks::new(&file, 1000).err().unwrap().contains("0 字节"));
    let (file, _cleanup) = temp_pattern_file("zero_chunk", 1);
    assert!(MappedChunks::new(&file, 0).err().unwrap().contains("块大小"));
}

#[test]
fn mapped_chunk_cannot_be_taken_twice() {
    let (file, _cleanup) = temp_pattern_file("take_twice", 128);
    let source = MappedChunks::new(&file, 64).unwrap();
    source.copy_chunk(0, &mut [0u8; 64]).unwrap();
    let error = source.copy_chunk(0, &mut [0u8; 64]).unwrap_err();
    assert!(error.contains("已被取得"));
    source.copy_chunk(64, &mut [0u8; 64]).unwrap();
}
```

把测试模块的 `use super::{...}` 扩展为包含 `ChunkCopy` 和 `MappedChunks`，确保 trait 方法可调用的同时新类型尚未实现。

- [ ] **Step 2: 运行测试并确认类型不存在而 RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml mapped_chunk -- --nocapture`

Expected: 编译 FAIL，包含 `cannot find type MappedChunks in this scope`；不得通过先建空类型让测试跳过 RED。

- [ ] **Step 3: 实现复制抽象、对齐映射和 RAII 视图**

在 `PositionedRead` 实现之后加入以下生产代码。保留原 `PositionedRead for File`， blanket adapter 只为旧文件调用方兼容短读；`MappedChunks` 走独立 `copy_chunk`，不调用 `read_at`。

```rust
/// 整块复制数据源；成功返回时 out 必须全部填满。
pub trait ChunkCopy: Sync {
    /// 返回源长度，失败时保留平台错误。
    fn len(&self) -> Result<u64, String>;
    /// 把 offset 对应的完整目标块复制到 out，失败时返回中文错误。
    fn copy_chunk(&self, offset: u64, out: &mut [u8]) -> Result<(), String>;
}

impl<T: PositionedRead> ChunkCopy for T {
    fn len(&self) -> Result<u64, String> {
        PositionedRead::len(self)
    }

    fn copy_chunk(&self, offset: u64, out: &mut [u8]) -> Result<(), String> {
        let mut read = 0usize;
        while read < out.len() {
            let current = offset
                .checked_add(read as u64)
                .ok_or("读取偏移溢出")?;
            let count = self.read_at(current, &mut out[read..])?;
            if count == 0 {
                return Err(format!("文件读取提前结束，偏移 {current}"));
            }
            read += count;
        }
        Ok(())
    }
}

struct MappedView {
    map: memmap2::Mmap,
    start: usize,
}

/// 覆盖完整文件的独立只读块视图；不拥有 File，也不从完整 M0 派生地址。
pub struct MappedChunks {
    views: Vec<Mutex<Option<MappedView>>>,
    total: usize,
    chunk_bytes: usize,
}

impl MappedChunks {
    /// 预建覆盖 file 全部字节的块视图；空文件、零块长、地址溢出或映射失败会清理已建视图。
    pub fn new(file: &File, chunk_bytes: usize) -> Result<Self, String> {
        if chunk_bytes == 0 {
            return Err("块大小必须大于 0".into());
        }
        let total = usize::try_from(file.metadata().map_err(|e| e.to_string())?.len())
            .map_err(|_| "文件大小超出本机地址范围".to_string())?;
        if total == 0 {
            return Err("不能预留 0 字节内存区域".into());
        }
        let granularity = mapping_granularity()?;
        let chunk_count = total
            .checked_add(chunk_bytes - 1)
            .ok_or("块数量计算溢出")?
            / chunk_bytes;
        let mut views = Vec::with_capacity(chunk_count);
        for index in 0..chunk_count {
            let offset = index
                .checked_mul(chunk_bytes)
                .ok_or("块偏移计算溢出")?;
            let len = chunk_bytes.min(total - offset);
            let offset_u64 = offset as u64;
            let aligned_offset = offset_u64 / granularity * granularity;
            let start = usize::try_from(offset_u64 - aligned_offset)
                .map_err(|_| "视图内偏移超出本机地址范围")?;
            let map_len = start.checked_add(len).ok_or("映射长度计算溢出")?;
            // SAFETY: file 在调用期间有效；只读映射拥有独立生命周期，外部并发截断仍沿用现有 mmap 风险边界。
            let map = unsafe {
                memmap2::MmapOptions::new()
                    .offset(aligned_offset)
                    .len(map_len)
                    .map(file)
                    .map_err(|error| format!("建立源块视图失败，偏移 {offset}：{error}"))?
            };
            views.push(Mutex::new(Some(MappedView { map, start })));
        }
        Ok(Self { views, total, chunk_bytes })
    }

    pub fn chunk_count(&self) -> usize {
        self.views.len()
    }
}

impl ChunkCopy for MappedChunks {
    fn len(&self) -> Result<u64, String> {
        Ok(self.total as u64)
    }

    fn copy_chunk(&self, offset: u64, out: &mut [u8]) -> Result<(), String> {
        let offset = usize::try_from(offset).map_err(|_| "复制偏移超出本机地址范围")?;
        if offset % self.chunk_bytes != 0 || offset >= self.total {
            return Err(format!("块视图复制偏移未对齐：{offset}"));
        }
        let index = offset / self.chunk_bytes;
        let expected = self.chunk_bytes.min(self.total - offset);
        if out.len() != expected {
            return Err(format!("块视图复制长度错误：期望 {expected}，实际 {}", out.len()));
        }
        let mut slot = self.views[index].lock().map_err(|_| "块视图锁已中毒")?;
        let mut view = slot.take().ok_or_else(|| format!("块视图已被取得：{index}"))?;
        drop(slot); // 复制期间不持有槽位锁，其他不相交块可并行取得。
        let end = view.start.checked_add(out.len()).ok_or("视图内复制范围溢出")?;
        out.copy_from_slice(&view.map[view.start..end]);
        drop(view); // 每块复制后立即解除映射，不等待其他 worker。
        Ok(())
    }
}

#[cfg(windows)]
fn mapping_granularity() -> Result<u64, String> {
    use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
    let mut info = std::mem::MaybeUninit::<SYSTEM_INFO>::uninit();
    // SAFETY: GetSystemInfo 初始化提供的 SYSTEM_INFO，不保留指针。
    unsafe {
        GetSystemInfo(info.as_mut_ptr());
        let granularity = info.assume_init().dwAllocationGranularity as u64;
        if granularity == 0 { Err("系统映射分配粒度为 0".into()) } else { Ok(granularity) }
    }
}

#[cfg(unix)]
fn mapping_granularity() -> Result<u64, String> {
    // SAFETY: sysconf(_SC_PAGESIZE) 无指针参数，不修改进程内存。
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page <= 0 { Err("无法取得系统映射页大小".into()) } else { Ok(page as u64) }
}
```

把 `prepare_load` 和 `run_prepared_load` 的泛型约束从 `R: PositionedRead` 改为 `R: ChunkCopy`，把 `load_file` 的约束也改为 `R: ChunkCopy`。将 `run_prepared_load` writer 中原来的短读循环替换为：

```rust
let offset = start as u64;
if matches!(options.fault, LoadFault::ReadAfter(limit) if offset >= limit) {
    return Err(format!("注入读取失败，偏移 {offset}"));
}
source.copy_chunk(offset, out)?;
```

保留 writer 开头和结尾的取消检查、进度累加及所有事件。`load_file` 仍依次调用同一 `prepare_load` 和 `run_prepared_load`，保存后重载传入的 `File` 经 blanket adapter 保持短读循环。

- [ ] **Step 4: 运行新测试和原文件读取回归**

Run: `cargo test --manifest-path src-tauri/Cargo.toml ram_load::tests -- --nocapture`

Expected: 新非对齐测试和原有 `load_reads_real_file...`、`commit_and_short_read_faults...` 全部 PASS；Windows 与 macOS 均编译各自 `mapping_granularity`。

- [ ] **Step 5: 检查点**

Run: `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`

Expected: 若格式检查失败，只运行 `rustfmt --edition 2021 src-tauri/src/ram_load.rs` 并复跑本任务测试；不格式化无关文件。不提交。

---

### Task 3: 并行视图生命周期、取消与故障展开

**Files:**
- Modify: `src-tauri/src/ram_load.rs`

**Interfaces:**
- Consumes: Task 2 的 `MappedChunks`、`ChunkCopy` 和 `load_file`。
- Produces: `MappedViewStats { remaining, active, max_active, copied }`、`MappedChunks::view_stats()`、测试专用 `counters_for_test()`；成功、取消和 `ReadAfter` 后均无活动或剩余视图。

- [ ] **Step 1: 写并行上界与展开清理失败测试**

在测试模块加入：

```rust
#[test]
fn mapped_chunks_parallel_copy_releases_every_view() {
    let len = 32 * 1024 + 13;
    let (file, _cleanup) = temp_pattern_file("parallel", len);
    let source = MappedChunks::new(&file, 257).unwrap();
    let initial = source.view_stats();
    assert_eq!(initial.remaining, len.div_ceil(257));
    assert_eq!(initial.active, 0);
    let counters = source.counters_for_test();
    let cancel = AtomicBool::new(false);
    let result = load_file(
        source,
        &cancel,
        LoadOptions { chunk_bytes: 257, workers: 4, fault: LoadFault::None },
        |_| {},
    ).unwrap();
    let LoadResult::Complete(ram) = result else { panic!("unexpected cancel") };
    assert!(ram.as_slice().iter().enumerate().all(|(i, b)| *b == (i % 251) as u8));
    assert_eq!(counters.remaining.load(Ordering::Acquire), 0);
    assert_eq!(counters.active.load(Ordering::Acquire), 0);
    assert_eq!(counters.copied.load(Ordering::Acquire), len.div_ceil(257));
    let max_active = counters.max_active.load(Ordering::Acquire);
    assert!((1..=4).contains(&max_active), "max_active={max_active}");
}

#[test]
fn mapped_chunks_fault_and_cancel_leave_no_live_views() {
    let (file, _cleanup) = temp_pattern_file("mapped_fault", 4096);
    let source = MappedChunks::new(&file, 64).unwrap();
    let counters = source.counters_for_test();
    let cancel = AtomicBool::new(false);
    let error = load_file(
        source,
        &cancel,
        LoadOptions { chunk_bytes: 64, workers: 4, fault: LoadFault::ReadAfter(0) },
        |_| {},
    ).unwrap_err();
    assert!(error.contains("读取"));
    assert_eq!(counters.remaining.load(Ordering::Acquire), 0);
    assert_eq!(counters.active.load(Ordering::Acquire), 0);

    let (file, _cleanup) = temp_pattern_file("mapped_cancel", 64 * 1024);
    let source = MappedChunks::new(&file, 64).unwrap();
    let counters = source.counters_for_test();
    let cancel = AtomicBool::new(false);
    let result = load_file(
        source,
        &cancel,
        LoadOptions { chunk_bytes: 64, workers: 2, fault: LoadFault::None },
        |event| {
            if matches!(event, LoadEvent::Progress { done, .. } if done > 0) {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    ).unwrap();
    assert!(matches!(result, LoadResult::Cancelled));
    assert_eq!(counters.remaining.load(Ordering::Acquire), 0);
    assert_eq!(counters.active.load(Ordering::Acquire), 0);
}
```

- [ ] **Step 2: 运行并确认统计接口不存在而 RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml mapped_chunks_parallel_copy_releases_every_view -- --nocapture`

Expected: 编译 FAIL，包含 `counters_for_test` 或 `ViewCounters` 不存在；这是新增生命周期可观察性的失败，不是重复运行 Task 2 已经通过的复制测试。

- [ ] **Step 3: 用 RAII 增加活动视图统计**

在 `MappedView` 前加入：

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MappedViewStats {
    pub remaining: usize,
    pub active: usize,
    pub max_active: usize,
    pub copied: usize,
}

#[derive(Default)]
struct ViewCounters {
    remaining: std::sync::atomic::AtomicUsize,
    active: std::sync::atomic::AtomicUsize,
    max_active: std::sync::atomic::AtomicUsize,
    copied: std::sync::atomic::AtomicUsize,
}
```

把 `MappedView` 扩展为以下完整实现；未调度视图由槽位析构，活动视图由 worker 局部变量析构，两条路径都减少 remaining：

```rust
struct MappedView {
    map: memmap2::Mmap,
    start: usize,
    counters: std::sync::Arc<ViewCounters>,
    active: bool,
    copied: bool,
}

impl MappedView {
    fn activate(&mut self) {
        self.active = true;
        let active = self.counters.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.counters.max_active.fetch_max(active, Ordering::AcqRel);
    }
}

impl Drop for MappedView {
    fn drop(&mut self) {
        if self.active {
            self.counters.active.fetch_sub(1, Ordering::AcqRel);
        }
        if self.copied {
            self.counters.copied.fetch_add(1, Ordering::AcqRel);
        }
        self.counters.remaining.fetch_sub(1, Ordering::AcqRel);
        // self.map 随后立即析构并解除这一块视图。
    }
}
```

给 `MappedChunks` 增加 `counters: std::sync::Arc<ViewCounters>` 字段，并在 `new` 中初始化：

```rust
let counters = std::sync::Arc::new(ViewCounters::default());
```

每块映射成功后、放入槽位时改为：

```rust
counters.remaining.fetch_add(1, Ordering::AcqRel);
views.push(Mutex::new(Some(MappedView {
    map,
    start,
    counters: counters.clone(),
    active: false,
    copied: false,
})));
```

构造函数返回改为 `Ok(Self { views, counters, total, chunk_bytes })`，并加入：

```rust
pub fn view_stats(&self) -> MappedViewStats {
    MappedViewStats {
        remaining: self.counters.remaining.load(Ordering::Acquire),
        active: self.counters.active.load(Ordering::Acquire),
        max_active: self.counters.max_active.load(Ordering::Acquire),
        copied: self.counters.copied.load(Ordering::Acquire),
    }
}

#[cfg(test)]
fn counters_for_test(&self) -> std::sync::Arc<ViewCounters> {
    self.counters.clone()
}
```

`copy_chunk` 在 `drop(slot)` 后调用 `view.activate()`，在 `copy_from_slice` 成功后设置 `view.copied = true`，随后保持 `drop(view)`。这样复制一块立即释放一块，取消或错误展开也由同一 Drop 清理。

- [ ] **Step 4: 运行完整 `ram_load` 测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml ram_load::tests -- --nocapture`

Expected: 全部 PASS。并行测试证明程序所有权和释放顺序，不把计数器当作物理页回收证明。

---

### Task 4: 引擎准备顺序与恢复接线

**Files:**
- Modify: `src-tauri/src/engine.rs:35`
- Modify: `src-tauri/src/engine.rs:5490-5810`
- Modify: `src-tauri/src/engine.rs:9753-9800`

**Interfaces:**
- Consumes: `MappedChunks::new`、`MappedChunks::chunk_count`、`prepare_load<R: ChunkCopy>`、`run_prepared_load<R: ChunkCopy>`。
- Produces: `LoadEvent::SourcePrepared { chunks: usize }`；首次转换在 SourcePrepared、Reserved 之后才解除 M0。保存后 `prepare_memory_reopen` 仍传 `File`。

- [ ] **Step 1: 写 M0 生命周期顺序失败测试**

在引擎转换测试区加入：

```rust
#[test]
fn backing_conversion_prepares_all_views_and_target_before_releasing_mmap() {
    let p = tmp_csv("convert_view_order", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    let before = conversion_snapshot(&bf);
    let weak = match &bf.backing {
        Backing::Mmap(m) => Arc::downgrade(m),
        _ => panic!("预期 mmap"),
    };
    let observations = std::sync::Mutex::new(Vec::new());
    let cancel = AtomicBool::new(false);
    let status = bf.convert_mmap_to_ram(
        &cancel,
        LoadOptions { chunk_bytes: 16, workers: 2, fault: LoadFault::None },
        |event| observations.lock().unwrap().push((event, weak.upgrade().is_some())),
    ).unwrap();
    assert_eq!(status, ConvertStatus::Converted);
    let events = observations.into_inner().unwrap();
    let prepared = events.iter().position(|(event, _)| matches!(event, LoadEvent::SourcePrepared { .. })).unwrap();
    let reserved = events.iter().position(|(event, _)| matches!(event, LoadEvent::Reserved { .. })).unwrap();
    let first_chunk = events.iter().position(|(event, _)| matches!(event, LoadEvent::ChunkBegin { .. })).unwrap();
    assert!(prepared < reserved && reserved < first_chunk, "{events:?}");
    assert!(events[prepared].1, "块视图准备时原 M0 必须有效");
    assert!(events[reserved].1, "目标预留时原 M0 必须有效");
    assert!(events.iter().filter(|(event, _)| matches!(event, LoadEvent::ChunkBegin { .. })).all(|(_, old_live)| !old_live));
    assert!(weak.upgrade().is_none());
    assert_eq!(conversion_snapshot(&bf), before);
    fs::remove_file(&p).ok();
}
```

把现有 `backing_conversion_reserve_fault_keeps_original_mmap` 中 `convert_for_test` 调用替换为带观察者的直接调用，并增加事件断言：

```rust
let source_prepared = AtomicBool::new(false);
let cancel = AtomicBool::new(false);
let error = bf.convert_mmap_to_ram(
    &cancel,
    LoadOptions { chunk_bytes: 16, workers: 2, fault: LoadFault::None },
    |event| {
        if matches!(event, LoadEvent::SourcePrepared { .. }) {
            assert!(weak.upgrade().is_some());
            source_prepared.store(true, Ordering::Relaxed);
        }
    },
).unwrap_err();
assert!(source_prepared.load(Ordering::Relaxed));
```

该测试原有 `error.contains("预留")`、Weak 仍有效和快照不变断言全部保留。

- [ ] **Step 2: 运行顺序测试并确认事件缺失而 RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml backing_conversion_prepares_all_views_and_target_before_releasing_mmap -- --nocapture`

Expected: 编译 FAIL，包含 `SourcePrepared` 无关联变体；不是测试辅助函数不存在。

- [ ] **Step 3: 新增事件并只替换首次转换源**

在 `ram_load.rs` 的 `LoadEvent` 开头加入：

```rust
SourcePrepared { chunks: usize },
```

在 `engine.rs` 顶部导入 `MappedChunks`。将 `convert_mmap_to_ram` 中从打开文件到 `prepare_load` 的片段替换为：

```rust
let file = fs::File::open(&self.backing_path).map_err(|e| e.to_string())?;
// M0 仍有效时建立覆盖全文件的独立块视图，之后复制不再依赖 File::read_at。
let source = MappedChunks::new(&file, options.chunk_bytes)?;
observer(LoadEvent::SourcePrepared { chunks: source.chunk_count() });
drop(file); // 独立映射在句柄关闭后仍有效，MappedChunks 不保存 File。
#[cfg(test)]
if self::tests::conversion_reserve_fault() {
    return Err("注入预留失败".into());
}
// 全部块视图已建立；目标预留失败时 source 自动清理，原 M0 不动。
let prepared = prepare_load(source, options, &observer)?;
```

其后的 `std::mem::replace` 和 `drop(previous)` 保留，只把注释改为：

```rust
drop(previous); // 块视图和目标 R 均已准备好，复制开始前解除最后一个完整 M0。
```

`prepare_memory_reopen` 的 `prepare_load(file, ...)` 不改；它仍使用 `PositionedRead` adapter。排序估算注释替换为：

```rust
// 保守物理重叠提示：源文件页与匿名 RAM 在复制期可短时接近 2X。
// 预建块视图逐块释放只降低后续驻留，不消除初始重叠；105% 仅约束私有内存。
```

- [ ] **Step 4: 运行引擎转换回归**

Run: `cargo test --manifest-path src-tauri/Cargo.toml backing_conversion -- --nocapture`

Expected: 新顺序测试及现有 reserve/commit/read fault、外部变化、格式矩阵、历史往返全部 PASS。

- [ ] **Step 5: 运行取消、恢复和保存重载回归**

```bash
cargo test --manifest-path src-tauri/Cargo.toml conversion_ -- --nocapture
cargo test --manifest-path src-tauri/Cargo.toml prepare_memory_reopen -- --nocapture
cargo test --manifest-path src-tauri/Cargo.toml save -- --nocapture
```

Expected: 全部 PASS；若某个过滤词没有匹配测试，改用完整 `cargo test --manifest-path src-tauri/Cargo.toml` 验证，不把“0 matched”当通过。现有 ReadAfter 故障现经块复制前注入，继续产生兼容“读取”错误并恢复 mmap。

- [ ] **Step 6: 全量 Rust 与 Node 回归**

```bash
cargo test --manifest-path src-tauri/Cargo.toml
node --test "tests_js/*.test.mjs"
```

Expected: 全部 PASS；记录实际数量和忽略项，不沿用旧测试计数。不提交。

---

### Task 5: 真实私有峰值矩阵与热缓存物理盘对照

**Files:**
- Create outside version control: `src-tauri/target/memory-peak-mmap-$(date +%s)/*.json`
- Create outside version control: `src-tauri/target/physical-read-new-$(date +%s)/*`
- Modify: `progress.md`
- Modify: `findings.md`

**Interfaces:**
- Consumes: Task 1 保存的旧 exe 和基线 JSON；Task 4 的新实现；`PeakReport.pass`、`actual_peak_increment_bytes`、`allowed_peak_increment_bytes`、`working_set_peak_bytes`。
- Produces: 19 个 fresh 峰值报告、旧新物理盘报告和不含推测的测量结论。

- [ ] **Step 1: 构建新诊断器并运行完整 19 场景矩阵**

```bash
cargo build --manifest-path src-tauri/Cargo.toml --release --bin memory_peak
set -euo pipefail
exe="src-tauri/target/release/memory_peak.exe"
base="src-tauri/target/memory-peak-mmap-$(date +%s)"
mkdir -p "$base"
run_case() {
  local scenario="$1"
  local size="$2"
  local workers="$3"
  local preload="$4"
  "$exe" run --scenario "$scenario" --file "$base/fixture-$size.csv" --workers "$workers" --preload "$preload" --report "$base/$scenario-$size-w$workers-preload-$preload.json"
}
for workers in 1 2 4; do
  for preload in false true; do run_case convert 64 "$workers" "$preload"; done
done
run_case convert 256 4 false
run_case convert 1024 4 false
for scenario in cancel commit-fail read-fail cancel-loop; do
  for preload in false true; do run_case "$scenario" 64 4 "$preload"; done
done
for size in 64 256 1024; do run_case save "$size" 4 false; done
export PEAK_BASE="$base"
node --input-type=module <<'NODE'
import fs from "node:fs";
import path from "node:path";
const base = process.env.PEAK_BASE;
const files = fs.readdirSync(base).filter((name) => name.endsWith(".json"));
if (files.length !== 19) throw new Error(`报告数量 ${files.length}，期望 19`);
const rows = files.map((name) => {
  const report = JSON.parse(fs.readFileSync(path.join(base, name), "utf8"));
  if (!report.pass || report.actual_peak_increment_bytes > report.allowed_peak_increment_bytes) {
    throw new Error(`私有峰值失败：${name}`);
  }
  return { name, x: report.file_bytes_x, increment: report.actual_peak_increment_bytes, allowed: report.allowed_peak_increment_bytes, workingSet: report.working_set_peak_bytes };
});
console.table(rows);
fs.writeFileSync(path.join(base, "summary.json"), `${JSON.stringify(rows, null, 2)}\n`, "utf8");
NODE
```

Expected: 19 个场景均满足 105% 私有峰值；工作集只列值，不加入增量判断。任一失败都保留 JSON、停止性能结论并回到根因调查，不放宽阈值。

- [ ] **Step 2: 对新路径执行同一热缓存物理盘探针**

```bash
node scripts/probe-physical-reads.mjs \
  --exe src-tauri/target/release/memory_peak.exe \
  --file 'E:\simpletable_hdd_test\bench_22gb.csv' \
  --workers 1 --seconds 60 \
  --output-dir src-tauri/target/physical-read-new-$(date +%s)
```

Expected: 报告 `covered=true` 且 `status=verified_zero` 才能验证该轮目标盘物理读取为零。`reads_observed` 不能归因给源文件，也不能忽略；记录数值后交替重跑旧新路径并测量无转换时的同盘噪声。`unverified` 必须写明原因。没有 `verified_zero` 就不得宣称首要实测目标已验证。

- [ ] **Step 3: 记录对照而不是只保留成功轮次**

把 Task 1 旧路径和 Step 2 新路径的报告路径、状态、转换耗时、样本数、目标盘读字节和工作集峰值追加到 `findings.md`。至少记录一次旧新同条件对照；所有执行过的失败或噪声轮次也追加，不挑选性删除。物理盘结论固定使用以下判定：

```text
verified_zero：计数器覆盖完整转换，目标 PhysicalDisk 实例读取为零。
reads_observed：目标盘存在读取，但尚不能归因到源文件；需要排查噪声或重测。
unverified：覆盖、实例、样本或协议不完整，不作物理读盘结论。
```

- [ ] **Step 4: 独立检查源文件未改动**

Run: `node -e "const fs=require('fs'); const p='E:/simpletable_hdd_test/bench_22gb.csv'; const s=fs.statSync(p); console.log(JSON.stringify({bytes:s.size,modified:s.mtime.toISOString()})); if(s.size!==2286250041) process.exit(1)"`

Expected: 输出 `bytes:2286250041`；把 modified 与 Task 1 记录比较，不一致则停止并调查，不能继续用该文件做对照。

---

### Task 6: 文档、exe 与 canonical 双构建双冒烟

**Files:**
- Modify: `docs/superpowers/specs/2026-11-12-mmap-chunk-views-design.md:4,94`
- Modify: `README.md`
- Modify: `docs/development.md`
- Modify: `TODO.md`
- Modify: `task_plan.md`
- Modify: `progress.md`
- Modify: `findings.md`

**Interfaces:**
- Consumes: Task 4–5 的实际测试输出和报告路径。
- Produces: 与新行为一致的文档、新 debug/release exe、新 smoke 报告和明确 macOS 未验证状态。

- [ ] **Step 1: 修正文档中的转换顺序和对齐余量**

把规格状态改为“书面规格和实施计划已获用户确认，按计划实施”，并把第 5 节第 3 步的“视图虚拟地址总覆盖量为 X”改为：

```text
逻辑文件覆盖量为 X；实际虚拟映射长度包含起始对齐余量。默认 8 MiB 块通常不重叠，非对齐小测试块可能映射重叠范围，但每个视图只暴露所属逻辑块。
```

README 和 `docs/development.md` 中“预留成功后、任何文件读取开始前，先解除完整源 mmap”的旧描述统一替换为：

```text
首次 mmap 转 RAM 时，完整 M0 仍有效就为全文件预建独立只读块视图，并完成最终连续匿名 RAM 的预检和预留。两者全部成功后才解除 M0；worker 直接从各自块视图复制到最终地址，复制一块立即解除一块。首次转换不再主动整份 read_at，但 mmap 命中缓存与物理读盘不是同一指标，仍需 PhysicalDisk 实测。私有内存保持 floor(X×1.05) 硬门槛，工作集短时可能接近 2X，文件缓存也不要求立即归零。
```

保留 03 单空列保存快路径和既有历史验收快照；旧数据不得标成新实现结果。

- [ ] **Step 2: 追加任务记录和实际验证证据**

在 `TODO.md` 与 `task_plan.md` 追加独立的 Task 13“首次 mmap 预建块视图转换”，列出本计划六项任务及实际完成状态，不重写 Task 1–12。把以下命令的完整 fresh 输出用 `tee -a progress.md` 保存，失败输出同样保留：

```bash
set -o pipefail
cargo test --manifest-path src-tauri/Cargo.toml 2>&1 | tee -a progress.md
node --test "tests_js/*.test.mjs" 2>&1 | tee -a progress.md
```

在 `findings.md` 记录：M0 与块视图独立、File 句柄关闭后映射仍有效、非对齐块的逻辑与虚拟范围差异、105% 与 2X 的不同口径、PhysicalDisk 只能证明整盘而不能在 nonzero 时归因单文件。

- [ ] **Step 3: 格式检查并执行 canonical 双构建双冒烟**

```bash
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
node scripts/verify-local.mjs
```

Expected: 命令退出 0；脚本重新构建 debug/release，确认两个 exe 时间戳前进且不早于前端资产，分别完成 `--smoke`，报告唯一汇总且失败数为 0。保存脚本打印的两个新报告路径和实际合计，不使用本计划之前旧报告作为证据。

- [ ] **Step 4: 检查 diff、工作区和平台状态**

```bash
git diff --check
git status --short
git diff --stat
```

Expected: 没有空白错误；03 文件仍存在其原未提交改动，本任务未修改通用保存和 03 快路径；没有新提交、推送或远程操作。最终报告必须单列：Windows 实测结果、物理盘是否达到 `verified_zero`、macOS 是否尚未进行真机/远程等价测试和 release 峰值矩阵。未执行 macOS 时明确写“未验证”，不得用 Windows 结果替代。

- [ ] **Step 5: 最终检查点**

逐项对照规格第 11 节：源类型与顺序、复制后释放、成功/取消/失败/外部变化清理、19 场景私有峰值、物理盘证据、exe/双 smoke、文档和 macOS 状态。任何一项未满足就保留未完成状态并指出具体证据缺口；不提交，等待用户下一步决定。

---

## Plan Self-Review

- **Spec coverage:** 第 2–7 节由 Tasks 2–4 覆盖；第 8 节由 Task 4 注释和 Task 5 私有/工作集分账覆盖；第 9.1–9.3 节由 Tasks 2–5 覆盖；第 9.4 节由 Tasks 1、5 覆盖；第 9.5、10、11 节由 Task 6 覆盖。通用保存和 03 快路径明确不改，保存后重载仍由 File adapter 回归。
- **Placeholder scan:** 所有代码任务给出完整测试、实现或精确替换片段；运行时唯一目录用 `$(date +%s)` 生成，测量结果只从实际 JSON 和命令输出记录，没有预填通过数据。
- **Type consistency:** `ChunkCopy::copy_chunk` 返回 `Result<(), String>`；`PreparedLoad<R>` 和 `run_prepared_load<R>` 使用同一 `R: ChunkCopy`。`MappedChunks` 缓存 `total: usize`，`LoadOptions.chunk_bytes` 与构造块长一致；`SourcePrepared { chunks: usize }` 与 `chunk_count() -> usize` 一致。`ViewCounters` 仅通过测试句柄跨移动后的 source 观察，不要求 `MappedChunks: Clone`。
