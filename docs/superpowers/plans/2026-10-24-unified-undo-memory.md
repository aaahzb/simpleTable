# 统一撤销/重做内存预算 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在每个文档 64 MiB 默认预算内统一内容、行列、排序和表头历史，并用固定点、稳定身份、保存重绑定和有界流式剪贴板覆盖全部已批准场景。

**Architecture:** 保留 `engine.rs` 的文档执行核心，把通用账本、列结构、保存映射、行序和临时文件所有权拆成小型 `pub(crate)` 模块。普通历史只存稀疏变化和共享对象；超限操作先预检、再确认、后暂存提交，保存和粘贴均不在提交前破坏活动文档。

**Tech Stack:** Rust stable、Tauri 2、Serde、无构建原生 JavaScript、Node.js `node:test`、Cargo 单测、debug/release Windows 真机冒烟。

**Approved spec:** `docs/superpowers/specs/2026-10-24-unified-undo-memory-design.md`

**Starting point:** 分支 `feat/unified-undo-memory`，设计提交 `81211c8`。

## Global Constraints

- 默认历史预算是每文档 **64 MiB = 67,108,864 字节**；设置范围为 **1–1,024 MiB**，undo/redo 合计记账。
- 预算固定且可配置，不随系统瞬时可用内存动态变化。
- 预算是确定性历史账目，不是进程 RSS；执行缓冲、排序键、系统页缓存和外部 `readText()` 的完整字符串单独报告。
- 取消固定 200 条主限制，不设置替代性固定条数；磁盘快照仍按逻辑内容计费。
- 应用内大剪贴板仅在当前进程内有效，不承诺跨进程使用，也不在应用重启后恢复。
- 纯界面状态不入历史；筛选、选区、光标和滚动位置不得出现新历史变体。
- 不引入整表前后快照、通用 Composite、Tauri store 或无限磁盘历史。
- 每个行为先写测试并亲见预期 RED；未亲见 RED 时不得写对应生产实现。
- 所有函数保留或补充参数、返回值、异常/错误 docstring 或 Rust doc；关键逻辑写意图注释。
- 文件统一 UTF-8。读取既有 CSV 或报告时先判断编码；自动化只用合成夹具。
- 不读取、不修改、不提交 `price_j8888.csv`。手工大文件基准只用 `E:\simpletable_hdd_test\bench_22gb.csv` 或新生成的测试副本。
- `smoke_debug.txt`、`src-tauri/smoke-debug.txt`、`src-tauri/smoke-release.txt`、`捕获.JPG` 保持未跟踪，不删除、不纳入任何提交。
- 不做推送、远程标签、PR 或其他远程写操作。所有本地提交信息末尾依次带 `Co-Authored-By: Kimi <noreply@moonshot.cn>` 和 `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`。
- 当前 stable 工具链缺 rustfmt 时不得擅自安装，也不得把 rustfmt 写成已运行。
- Node 测试必须显式使用 `node --test tests_js/*.test.mjs`，不得把裸测试目录交给 Node 25。
- 每个代码任务都执行 `node scripts/verify-local.mjs`；它重编 debug/release、检查嵌入时间戳并分别运行新报告的真机冒烟。不得用旧 exe 或旧报告替代。
- 每个任务只暂存该任务列出的目标文件；提交前执行 `git diff --cached --check` 和 `git status --short`。
- 当前基线为 Node 113/113、Rust 138 passed + 2 ignored、debug/release 各 145/145。新增测试后只记录实际输出，不沿用估算。

## File Structure

| 文件 | 责任 |
|---|---|
| `src-tauri/src/history.rs` | 通用双端账本、共享计费、预算调整、固定点请求与编辑结果 |
| `src-tauri/src/schema.rs` | `ColumnId`、不可变 `Schema`、列存储映射与列名字节计量 |
| `src-tauri/src/spool.rs` | 文档会话临时目录、行快照、粘贴导入和暂存 generation 的所有权 |
| `src-tauri/src/baseline.rs` | 普通保存的记录区间映射、稀疏插入行映射和保存预检类型 |
| `src-tauri/src/order.rs` | 恒等/排列行序、筛选命中投影和排序排列字节计量 |
| `src-tauri/src/clipboard.rs` | 进程级应用内大剪贴板；不隶属于来源文档 |
| `src-tauri/src/engine.rs` | 段模型、文档读写、历史操作执行、保存提交及现有 Rust 测试 |
| `src-tauri/src/lib.rs` | Tauri 状态、命令注册、进度、关闭和退出清理 |
| `frontend/js/clipboard.js` | UTF-8 体量探测、分块和十六进制编码的纯函数 |
| `frontend/js/bigmodel.js` | 会话 API、固定点重试、预算下发、结构清除和剪贴板 API |
| `frontend/js/grid.js` | 复制/剪切/粘贴/清除的用户手势和有界数据流 |
| `frontend/js/app.js` | 模型注入、固定点与保存确认、工具栏、页签和脏点 |
| `frontend/js/dialogs.js` | 撤销预算数值对话框和明确确认文案 |
| `frontend/js/prefs.js` | 注入式 localStorage 预算偏好 |
| `frontend/js/smoke.js` | 真机行为、取消/失败、双页签和清理验证 |
| `tests_js/*.test.mjs` | 纯前端与 fake engine 回归 |
| `scripts/verify-local.mjs` | 双构建、时间戳和双真机报告的唯一本地验证入口 |
| `README.md`、`.github/workflows/*.yml` | 用户行为与最终实际测试账目 |

`engine.rs` 保留既有执行方法和同文件测试，避免把依赖大量私有段状态的操作强行搬出。新模块只持有可独立测试的数据结构和所有权逻辑。

## Cross-Task Interface Contract

后续任务必须保持以下外部名称一致。Rust 内部字段使用 snake_case；标记 `rename_all = "camelCase"` 的 IPC 返回值在 JavaScript 使用 camelCase。

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FixedPointKind {
    StepOverBudget,
    SaveTransform,
    SortMaterialization,
}

pub struct Approval {
    pub allow_fixed_point: bool,
    pub preflight_id: Option<u64>,
}

pub struct FixedPointRequest {
    pub kind: FixedPointKind,
    pub operation: String,
    pub required_bytes: u64,
    pub budget_bytes: u64,
    pub preflight_id: u64,
}

pub struct EditOutcome {
    pub changed: bool,
    pub inserted_rows: u64,
    pub removed_rows: u64,
    pub fixed_point: Option<FixedPointRequest>,
    pub cancelled: bool,
}
```

- JavaScript 统一结果字段为 `changed`、`insertedRows`、`removedRows`、`fixedPoint`、`cancelled`。
- 首次编辑调用传 `allowFixedPoint: false`；确认后传 `allowFixedPoint: true` 和返回的 `preflightId`。
- `preflightId` 是文档历史代际；任何成功编辑、undo/redo、保存或预算调整都使其递增，过期确认必须拒绝。
- 取消是 `changed=false`、`cancelled=true`，不是异常，也不刷新缓存或标脏。

## Standard Commands

仓库根：

```bash
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
node scripts/verify-local.mjs
git diff --check
```

Rust：

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib
cargo test -p simpletable --lib engine::tests::
```

---

### Task 1: P0 特征化保存身份风险并建立统一验证入口

**Files:**
- Modify: `src-tauri/src/engine.rs:1025-2144`
- Create: `scripts/verify-local.mjs`
- Modify local only: `progress.md`
- Test: `src-tauri/src/engine.rs` 内 `engine::tests`

**Interfaces:**
- Consumes: 当前 `BigFile::open`、`insert_rows`、`remove_rows`、`set_cell`、`save`、`undo`、`redo`。
- Produces: 三个带 `#[ignore = "P8 ..."]` 的回归测试，Task 8 解除 ignore；所有代码任务共用的双真机验证脚本。

- [x] **Step 1: 写三个预期失败的保存回归测试**

在 `engine::tests` 的 BigFile 行为分区加入以下完整测试。删除最后一行使旧 `rec` 在保存后必然越界；插入行后的源格用例可同时识别“撤销成功但写错行”。

```rust
#[test]
#[ignore = "P8 baseline transform"]
fn p8_save_insert_then_undo_removes_materialized_row() {
    let p = tmp_csv("p8_save_insert", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    let before = bf.rows(0, 5);
    let inserted = vec![vec!["X".to_string(), "Y".to_string(), "Z".to_string()]];
    bf.insert_rows(1, Some(inserted.clone()), 1).unwrap();
    bf.save(|_, _, _, _| {}).unwrap();
    assert!(bf.undo());
    assert_eq!(bf.rows(0, 5), before);
    assert!(bf.redo());
    assert_eq!(bf.rows(1, 1), inserted);
    fs::remove_file(&p).ok();
}

#[test]
#[ignore = "P8 deleted row recovery"]
fn p8_save_delete_last_then_undo_restores_row() {
    let p = tmp_csv("p8_save_delete", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    let before = bf.rows(0, 5);
    assert_eq!(bf.remove_rows(vec![4]).unwrap(), 1);
    bf.save(|_, _, _, _| {}).unwrap();
    assert!(bf.undo());
    assert_eq!(bf.rows(0, 5), before);
    assert!(bf.redo());
    assert_eq!(bf.rows(0, 5), before[..4]);
    fs::remove_file(&p).ok();
}

#[test]
#[ignore = "P8 source cell rebind"]
fn p8_save_undo_source_edit_after_insert_targets_original_row() {
    let p = tmp_csv("p8_save_source_edit", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    let before = bf.rows(0, 5);
    bf.insert_rows(
        1,
        Some(vec![vec!["X".to_string(), "Y".to_string(), "Z".to_string()]]),
        1,
    )
    .unwrap();
    bf.set_cell(2, 0, "changed".to_string()).unwrap();
    bf.save(|_, _, _, _| {}).unwrap();
    assert!(bf.undo());
    assert_eq!(bf.rows(1, 1)[0][0], "X");
    assert_eq!(bf.rows(2, 1)[0][0], before[1][0]);
    assert!(bf.redo());
    assert_eq!(bf.rows(2, 1)[0][0], "changed");
    fs::remove_file(&p).ok();
}
```

- [x] **Step 2: 分别运行并亲见 RED**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib engine::tests::p8_save_insert_then_undo_removes_materialized_row -- --ignored --exact --nocapture
cargo test -p simpletable --lib engine::tests::p8_save_delete_last_then_undo_restores_row -- --ignored --exact --nocapture
cargo test -p simpletable --lib engine::tests::p8_save_undo_source_edit_after_insert_targets_original_row -- --ignored --exact --nocapture
```

预期：三条均 FAIL。第一条是 `undo()` 返回 false；第二条是保存后旧记录越界或内容不等；第三条是插入行被误改或原目标未恢复。若失败来自编译或夹具，而不是这些断言，先修测试并重新取得有效 RED。

- [x] **Step 3: 创建可重复执行的双构建双冒烟脚本**

先创建 `scripts` 目录，再写入以下完整 `scripts/verify-local.mjs`。脚本使用唯一报告路径，不删除既有报告。

```javascript
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
```

- [x] **Step 4: 验证测试基线和脚本**

```bash
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib
cd "D:/SVN_Cursor/simpleTable-public"
node scripts/verify-local.mjs
```

预期：Node 113/113；Rust 138 passed、5 ignored，其中 3 个是本任务的 P8 回归；脚本输出 debug/release 各自实际合计、失败 0 和最终 PASS。

- [x] **Step 5: 记录本地基线并提交测试与验证工具**

在 `progress.md` 追加实际命令结果、三条 RED 的断言原因、双 exe 时间和报告路径。该文件保持本地忽略，不进入提交。

```bash
cd "D:/SVN_Cursor/simpleTable-public"
git add src-tauri/src/engine.rs scripts/verify-local.mjs
git diff --cached --check
git status --short
git commit -m "test: 特征化保存后历史身份风险" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

预期：提交只含 `engine.rs` 的测试变化和新验证脚本；既有四个杂项仍未跟踪。

---

### Task 2: 独立 HistoryLedger、共享计费与预算调整

**Files:**
- Create: `src-tauri/src/history.rs`
- Modify: `src-tauri/src/lib.rs` 顶部模块声明
- Modify local only: `progress.md`
- Test: `src-tauri/src/history.rs` 内 `history::tests`

**Interfaces:**
- Consumes: 无引擎依赖。
- Produces: `HistoryLedger<T>`、`HistoryEntry<T>`、`Prepared<T>`、`SharedCharge`、`Cost`、`HistoryStats`、`Approval`、`FixedPointRequest`、`EditOutcome` 和 `DEFAULT_HISTORY_BUDGET_BYTES`。

- [x] **Step 1: 先写预算、共享引用和 redo 缩容测试**

在 `lib.rs` 先加 `mod history;`，并在 `history.rs` 先只放测试和 `#[cfg(test)]` 辅助函数，运行时会因类型尚不存在而 RED。测试固定以下公开语义。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn item(name: &'static str, bytes: u64) -> HistoryEntry<&'static str> {
        HistoryEntry::new(name, bytes, Vec::new(), 1)
    }

    #[test]
    fn equal_budget_keeps_only_the_new_entry() {
        let mut h = HistoryLedger::new(64);
        h.commit_prepared(h.prepare(item("a", 32)).unwrap());
        h.commit_prepared(h.prepare(item("b", 64)).unwrap());
        assert_eq!(h.stats().bytes, 64);
        assert_eq!(h.stats().undo_count, 1);
        assert_eq!(h.undo_op(), Some(&"b"));
        assert!(h.prepare(item("too big", 65)).unwrap().requires_fixed_point());
    }

    #[test]
    fn shared_allocation_is_charged_once_until_last_reference_leaves() {
        let shared = Arc::new(String::from("shared"));
        let charge = SharedCharge::of(&shared, 40);
        let mut h = HistoryLedger::new(1_000);
        h.commit_prepared(h.prepare(HistoryEntry::new("a", 10, vec![charge], 1)).unwrap());
        h.commit_prepared(h.prepare(HistoryEntry::new("b", 10, vec![charge], 1)).unwrap());
        assert_eq!(h.stats().bytes, 60);
        let entry = h.take_undo().unwrap();
        h.finish_undo(entry);
        assert_eq!(h.stats().bytes, 60);
        let change = h.set_budget(50);
        assert!(!change.fixed_point);
        assert_eq!(h.stats().undo_count, 0);
        assert_eq!(h.stats().redo_count, 1);
        assert_eq!(h.stats().bytes, 50);
        assert_eq!(h.audited_bytes(), h.stats().bytes);
        h.clear();
        assert_eq!(h.stats().bytes, 0);
        assert_eq!(h.audited_bytes(), 0);
    }

    #[test]
    fn shrinking_evicts_farthest_redo_without_breaking_nearest_chain() {
        let mut h = HistoryLedger::new(1_000);
        for name in ["a", "b", "c"] {
            h.commit_prepared(h.prepare(item(name, 100)).unwrap());
        }
        let entry = h.take_undo().unwrap(); // undo c
        h.finish_undo(entry);
        let entry = h.take_undo().unwrap(); // undo b; redo 顺序为 c,b
        h.finish_undo(entry);
        let change = h.set_budget(150);
        assert!(!change.fixed_point);
        assert_eq!(h.stats().undo_count, 0);
        assert_eq!(h.redo_op(), Some(&"b"));
        assert_eq!(h.stats().bytes, 100);
        assert_eq!(h.audited_bytes(), h.stats().bytes);
        let change = h.set_budget(50);
        assert!(change.fixed_point);
        assert_eq!(h.stats().bytes, 0);
    }

    #[test]
    fn cost_accumulation_is_checked() {
        let mut cost = Cost::new();
        cost.add(u64::MAX).unwrap();
        assert_eq!(cost.add(1), Err(HistoryError::CostOverflow));
    }
}
```

- [x] **Step 2: 运行并亲见编译级 RED**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib history::tests -- --nocapture
```

预期：因 `HistoryLedger`、`HistoryEntry`、`Cost` 尚未定义而 FAIL；不得先实现再运行。

- [x] **Step 3: 实现确定性的成本类型和 IPC 类型**

在 `history.rs` 加入以下定义。`Cost::add_string` 计实际 UTF-8 String 分配容量，`add_logical_str` 专供 mmap/磁盘快照的逻辑内容长度；二者不得混用。

```rust
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

pub const DEFAULT_HISTORY_BUDGET_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryError {
    CostOverflow,
    SharedCostConflict,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cost(u64);

impl Cost {
    pub fn new() -> Self { Self(0) }
    pub fn bytes(self) -> u64 { self.0 }
    pub fn add(&mut self, bytes: u64) -> Result<(), HistoryError> {
        self.0 = self.0.checked_add(bytes).ok_or(HistoryError::CostOverflow)?;
        Ok(())
    }
    pub fn add_value<T>(&mut self) -> Result<(), HistoryError> {
        self.add(std::mem::size_of::<T>() as u64)
    }
    pub fn add_string(&mut self, value: &String) -> Result<(), HistoryError> {
        self.add(value.capacity() as u64)
    }
    pub fn add_logical_str(&mut self, value: &str) -> Result<(), HistoryError> {
        self.add(value.len() as u64)
    }
    pub fn add_vec<T>(&mut self, value: &Vec<T>) -> Result<(), HistoryError> {
        let bytes = (value.capacity() as u64)
            .checked_mul(std::mem::size_of::<T>() as u64)
            .ok_or(HistoryError::CostOverflow)?;
        self.add(bytes)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedCharge { id: u64, bytes: u64 }

impl SharedCharge {
    pub fn of<T: Sized>(value: &Arc<T>, bytes: u64) -> Self {
        Self { id: Arc::as_ptr(value) as usize as u64, bytes }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    pub allow_fixed_point: bool,
    pub preflight_id: Option<u64>,
}

impl Approval {
    pub fn deny() -> Self { Self::default() }
    pub fn allow(preflight_id: u64) -> Self {
        Self { allow_fixed_point: true, preflight_id: Some(preflight_id) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FixedPointKind {
    StepOverBudget,
    SaveTransform,
    SortMaterialization,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FixedPointRequest {
    pub kind: FixedPointKind,
    pub operation: String,
    pub required_bytes: u64,
    pub budget_bytes: u64,
    pub preflight_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditOutcome {
    pub changed: bool,
    pub inserted_rows: u64,
    pub removed_rows: u64,
    pub fixed_point: Option<FixedPointRequest>,
    pub cancelled: bool,
}

impl EditOutcome {
    pub fn unchanged() -> Self {
        Self { changed: false, inserted_rows: 0, removed_rows: 0,
            fixed_point: None, cancelled: false }
    }
}
```

- [x] **Step 4: 实现账本准备、提交和无拷贝移动**

一个 entry 的 `own_bytes` 已包含枚举、对齐、Vec/Map 句柄和非共享内容。`entry_bytes` 是该步骤独立保留所需的完整成本。prepare 的局部变量 `incremental_bytes` 只用于预检新总账是否溢出，实际提交由 charge 按当时的共享引用重新入账。即使共享对象已在其他历史项中，是否超过单步预算仍按 `entry_bytes` 判断。

```rust
#[derive(Debug)]
pub struct HistoryEntry<T> {
    pub op: T,
    pub own_bytes: u64,
    pub revision_delta: i64,
    shared: Vec<SharedCharge>,
}

impl<T> HistoryEntry<T> {
    pub fn new(op: T, own_bytes: u64, shared: Vec<SharedCharge>, revision_delta: i64) -> Self {
        Self { op, own_bytes, revision_delta, shared }
    }
}

pub struct Prepared<T> {
    entry: HistoryEntry<T>,
    entry_bytes: u64,
    budget_bytes: u64,
}
impl<T> Prepared<T> {
    pub fn entry(&self) -> &HistoryEntry<T> { &self.entry }
    pub fn entry_bytes(&self) -> u64 { self.entry_bytes }
    pub fn requires_fixed_point(&self) -> bool { self.entry_bytes > self.budget_bytes }
    pub fn into_entry(self) -> HistoryEntry<T> { self.entry }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HistoryStats {
    pub undo_count: usize,
    pub redo_count: usize,
    pub bytes: u64,
    pub budget_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommitStats { pub evicted_undo: usize, pub cleared_redo: usize }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BudgetChange { pub evicted: usize, pub fixed_point: bool }

pub struct HistoryLedger<T> {
    undo: VecDeque<HistoryEntry<T>>,
    redo: VecDeque<HistoryEntry<T>>,
    budget_bytes: u64,
    total_bytes: u64,
    shared: HashMap<u64, (u64, u64)>, // id -> (bytes, entry_refs)
}
```

`Prepared::requires_fixed_point` 比较该条目的完整保留成本与当前预算；prepare 不保存增量字段，也不能用共享抵扣放宽单步固定点判断。最终实现以以下方法为准。

```rust
impl<T> HistoryLedger<T> {
    pub fn new(budget_bytes: u64) -> Self {
        Self { undo: VecDeque::new(), redo: VecDeque::new(), budget_bytes,
            total_bytes: 0, shared: HashMap::new() }
    }

    pub fn prepare(&self, entry: HistoryEntry<T>) -> Result<Prepared<T>, HistoryError> {
        let mut unique = BTreeMap::<u64, u64>::new();
        for charge in &entry.shared {
            if unique.insert(charge.id, charge.bytes)
                .is_some_and(|old| old != charge.bytes) {
                return Err(HistoryError::SharedCostConflict);
            }
        }
        let mut entry_bytes = entry.own_bytes;
        let mut incremental_bytes = entry.own_bytes;
        for (id, bytes) in unique {
            entry_bytes = entry_bytes.checked_add(bytes).ok_or(HistoryError::CostOverflow)?;
            if let Some((old, _)) = self.shared.get(&id) {
                if *old != bytes { return Err(HistoryError::SharedCostConflict); }
            } else {
                incremental_bytes = incremental_bytes
                    .checked_add(bytes).ok_or(HistoryError::CostOverflow)?;
            }
        }
        self.total_bytes.checked_add(incremental_bytes)
            .ok_or(HistoryError::CostOverflow)?;
        Ok(Prepared { entry, entry_bytes, budget_bytes: self.budget_bytes })
    }

    pub fn undo_op(&self) -> Option<&T> { self.undo.back().map(|e| &e.op) }
    pub fn redo_op(&self) -> Option<&T> { self.redo.back().map(|e| &e.op) }
    pub fn stats(&self) -> HistoryStats {
        HistoryStats { undo_count: self.undo.len(), redo_count: self.redo.len(),
            bytes: self.total_bytes, budget_bytes: self.budget_bytes }
    }
}
```

charge/release 的引用计数按 entry 而非 Arc clone 次数计算，同一 entry 内重复 SharedCharge 先按 id 去重。prepare 已检查现有共享成本冲突和 total+incremental 溢出，所以提交为无失败移动：

```rust
impl<T> HistoryLedger<T> {
fn charge(&mut self, entry: &HistoryEntry<T>) {
    self.total_bytes += entry.own_bytes;
    let mut seen = BTreeSet::new();
    for charge in &entry.shared {
        if !seen.insert(charge.id) { continue; }
        let item = self.shared.entry(charge.id).or_insert((charge.bytes, 0));
        debug_assert_eq!(item.0, charge.bytes);
        if item.1 == 0 { self.total_bytes += charge.bytes; }
        item.1 += 1;
    }
}

fn release(&mut self, entry: &HistoryEntry<T>) {
    self.total_bytes -= entry.own_bytes;
    let mut seen = BTreeSet::new();
    for charge in &entry.shared {
        if !seen.insert(charge.id) { continue; }
        let item = self.shared.get_mut(&charge.id).unwrap();
        item.1 -= 1;
        if item.1 == 0 {
            self.total_bytes -= item.0;
            self.shared.remove(&charge.id);
        }
    }
}

pub fn commit_prepared(&mut self, prepared: Prepared<T>) -> CommitStats {
    debug_assert!(!prepared.requires_fixed_point());
    let mut stats = CommitStats::default();
    self.charge(&prepared.entry);
    self.undo.push_back(prepared.entry);
    while let Some(entry) = self.redo.pop_front() {
        self.release(&entry);
        stats.cleared_redo += 1;
    }
    while self.total_bytes > self.budget_bytes {
        let entry = self.undo.pop_front().unwrap();
        self.release(&entry);
        stats.evicted_undo += 1;
    }
    stats
}

pub fn take_undo(&mut self) -> Option<HistoryEntry<T>> {
    let entry = self.undo.pop_back()?;
    self.release(&entry);
    Some(entry)
}
pub fn take_redo(&mut self) -> Option<HistoryEntry<T>> {
    let entry = self.redo.pop_back()?;
    self.release(&entry);
    Some(entry)
}
pub fn finish_undo(&mut self, entry: HistoryEntry<T>) {
    self.charge(&entry);
    self.redo.push_back(entry);
}
pub fn finish_redo(&mut self, entry: HistoryEntry<T>) {
    self.charge(&entry);
    self.undo.push_back(entry);
}
pub fn restore_undo(&mut self, entry: HistoryEntry<T>) { self.finish_redo(entry); }
pub fn restore_redo(&mut self, entry: HistoryEntry<T>) { self.finish_undo(entry); }
}
```

提交必须先登记新项，再清 redo，再从 undo 头部淘汰，避免共享对象短暂误释放。take 取出并暂时降账，finish 放到另一栈恢复同一账目，restore 用于逆操作失败的原位恢复。所有路径只移动 entry，不 clone `T`。测试用 `audited_bytes()` 独立遍历双栈重算，不能用返回 total_bytes 的 stats() 自证。

- [x] **Step 5: 实现缩容不动点并跑 GREEN**

`set_budget` 的循环顺序固定为 undo 头部、redo 头部。进入循环时若双栈合计只剩一项且仍超预算，调用 `clear()` 并返回 `fixed_point=true`；不得从 redo 中间删除。正常提交不得调用这个固定点分支。

```rust
impl<T> HistoryLedger<T> {
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.shared.clear();
        self.total_bytes = 0;
    }

    #[cfg(test)]
    fn audited_bytes(&self) -> u64 {
        let mut expected = 0;
        let mut refs = BTreeMap::<u64, (u64, u64)>::new();
        for entry in self.undo.iter().chain(self.redo.iter()) {
            expected += entry.own_bytes;
            let mut seen = BTreeSet::new();
            for charge in &entry.shared {
                if !seen.insert(charge.id) { continue; }
                let item = refs.entry(charge.id).or_insert((charge.bytes, 0));
                assert_eq!(item.0, charge.bytes);
                item.1 += 1;
            }
        }
        assert_eq!(refs.len(), self.shared.len());
        for (id, item) in &refs { assert_eq!(self.shared.get(id), Some(item)); }
        expected + refs.values().map(|(bytes, _)| *bytes).sum::<u64>()
    }

    pub fn set_budget(&mut self, budget_bytes: u64) -> BudgetChange {
    self.budget_bytes = budget_bytes;
    let mut change = BudgetChange::default();
    while self.total_bytes > self.budget_bytes {
        if self.undo.len() + self.redo.len() == 1 {
            self.clear();
            change.fixed_point = true;
            break;
        }
        let entry = if self.undo.is_empty() { self.redo.pop_front() }
            else { self.undo.pop_front() };
        let Some(entry) = entry else { break };
        self.release(&entry);
        change.evicted += 1;
    }
    change
}
}
```

运行：

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib history::tests -- --nocapture
```

预期：4 个新测试 PASS，且共享计费、等值预算和 redo 缩容断言均真实执行。

- [x] **Step 6: 全量验证并提交**

```bash
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
node scripts/verify-local.mjs
git add src-tauri/src/history.rs src-tauri/src/lib.rs
git diff --cached --check
git commit -m "feat: 添加撤销历史字节账本" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

在 `progress.md` 记录 RED/GREEN 和实际测试数。若出现 Rust 编译警告，先消除再提交。Task 2 的接口尚未接入时，只允许在 `lib.rs` 的 `mod history;` 上使用带注释的临时 `#[allow(dead_code)]`；不得扩大到全 crate，Task 15 必须删除并复验。

---

### Task 3: 现有编辑接入账本、固定点与有符号脏点

**Files:**
- Modify: `src-tauri/src/engine.rs:4011-4036,4972-5060,6205-6660`
- Modify: `src-tauri/src/lib.rs` 的编辑命令
- Modify: `frontend/js/bigmodel.js:35-250,845-1116`
- Test: `src-tauri/src/engine.rs`、`tests_js/bigmodel.test.mjs`
- Modify local only: `progress.md`

**Interfaces:**
- Consumes: Task 2 的 `Approval`、`EditOutcome`、`HistoryLedger<Op>`、`Prepared<Op>`。
- Produces: 所有现有编辑命令返回 `EditOutcome`；`BigModel._invokeEdit(cmd,args)`；`BigFile.history_stats()`、`set_history_budget(u64)`；`revision: i64`。

- [x] **Step 1: 写固定点、取消、过期预检和保存点脏态测试**

以下测试使用新签名，先在引擎和测试中写出调用，亲见“参数数量不符/类型不存在”的 RED，再改生产实现。

```rust
#[test]
fn p1_fixed_point_requires_confirmation_and_bounds_later_undo() {
    let p = tmp_csv("p1_fixed_point", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_history_budget(4_096).unwrap();
    let large = "x".repeat(128 * 1024);
    let first = bf.set_cell(0, 0, large.clone(), Approval::deny()).unwrap();
    assert!(!first.changed);
    let request = first.fixed_point.unwrap();
    assert_eq!(request.budget_bytes, 4_096);
    assert!(request.required_bytes > 4_096);
    assert_eq!(bf.rows(0, 1)[0][0], "alice");
    let done = bf.set_cell(0, 0, large.clone(), Approval::allow(request.preflight_id)).unwrap();
    assert!(done.changed);
    assert_eq!(bf.history_stats().undo_count, 0);
    assert!(bf.modified());
    bf.set_cell(1, 0, "after".into(), Approval::deny()).unwrap();
    assert!(bf.undo());
    assert_eq!(bf.rows(0, 1)[0][0], large);
    assert!(!bf.undo());
    fs::remove_file(&p).ok();
}

#[test]
fn p1_fixed_point_preflight_expires_after_another_edit() {
    let p = tmp_csv("p1_preflight", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_history_budget(4_096).unwrap();
    let large = "x".repeat(128 * 1024);
    let request = bf.set_cell(0, 0, large.clone(), Approval::deny())
        .unwrap().fixed_point.unwrap();
    bf.set_cell(1, 0, "newer".into(), Approval::deny()).unwrap();
    let err = bf.set_cell(0, 0, large, Approval::allow(request.preflight_id)).unwrap_err();
    assert!(err.contains("预检已过期"));
    assert_eq!(bf.rows(0, 1)[0][0], "alice");
    fs::remove_file(&p).ok();
}

#[test]
fn p1_undo_before_savepoint_is_dirty_and_redo_returns_clean() {
    let p = tmp_csv("p1_signed_revision", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_cell(0, 0, "changed".into(), Approval::deny()).unwrap();
    bf.save(|_, _, _, _| {}).unwrap();
    assert!(!bf.modified());
    assert!(bf.undo());
    assert!(bf.modified());
    assert!(bf.redo());
    assert!(!bf.modified());
    fs::remove_file(&p).ok();
}

#[test]
fn p1_branch_after_undo_past_savepoint_stays_dirty() {
    let p = tmp_csv("p1_detached_savepoint", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_cell(0, 0, "saved".into(), Approval::deny()).unwrap();
    bf.save(|_, _, _, _| {}).unwrap();
    assert!(bf.undo());
    bf.set_cell(0, 0, "branch".into(), Approval::deny()).unwrap();
    assert!(bf.modified());
    assert!(bf.undo());
    assert!(bf.modified());
    fs::remove_file(&p).ok();
}

#[test]
fn p1_fixed_point_after_undo_past_savepoint_stays_dirty_at_zero_revision() {
    let p = tmp_csv("p1_fixed_detached", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_cell(0, 0, "saved".into(), Approval::deny()).unwrap();
    bf.save(|_, _, _, _| {}).unwrap();
    assert!(bf.undo());
    bf.set_history_budget(4_096).unwrap();
    let large = "x".repeat(128 * 1024);
    let request = bf.set_cell(1, 0, large.clone(), Approval::deny())
        .unwrap().fixed_point.unwrap();
    let done = bf.set_cell(1, 0, large, Approval::allow(request.preflight_id)).unwrap();
    assert!(done.changed);
    assert!(bf.modified(), "保存点已在被清除的 redo 分支上，revision=0 也不能假干净");
    fs::remove_file(&p).ok();
}

#[test]
fn p1_small_operations_are_not_capped_at_200() {
    let p = tmp_csv("p1_no_step_cap", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    for i in 0..201 {
        bf.set_cell(0, 0, i.to_string(), Approval::deny()).unwrap();
    }
    assert_eq!(bf.history_stats().undo_count, 201);
    assert!(bf.history_stats().bytes <= DEFAULT_HISTORY_BUDGET_BYTES);
    fs::remove_file(&p).ok();
}
```

在 `bigmodel.test.mjs` 追加以下轻量夹具和三条测试。夹具不分配文档二维数组，只记录真实 [cmd,args] 元组。

```javascript
function makeFixedPointModel(confirm, retry) {
  const calls = [];
  let refreshes = 0;
  let dataEvents = 0;
  const fixedPoint = {
    kind: "stepOverBudget", operation: "setCells", requiredBytes: 10000,
    budgetBytes: 4096, preflightId: 7,
  };
  const model = new BigModel({
    invoke: async (cmd, args = {}) => {
      calls.push([cmd, args]);
      if (cmd !== "big_set_cells") throw new Error("unexpected " + cmd);
      if (calls.length === 1) {
        return { changed: false, insertedRows: 0, removedRows: 0,
          fixedPoint, cancelled: false };
      }
      return retry;
    },
    listen: async () => () => {},
    confirmFixedPoint: confirm,
  });
  model._refreshMeta = async () => { refreshes += 1; };
  model._emitData = () => { dataEvents += 1; };
  model._emitUndo = () => {};
  return { model, calls, counts: () => ({ refreshes, dataEvents }) };
}

test("固定点取消不重试、不刷新、不发数据通知", async () => {
  const { model, calls, counts } = makeFixedPointModel(async () => false, null);
  const outcome = await model.setCells([[0, 0, "x"]]);
  assert.equal(outcome.cancelled, true);
  assert.equal(outcome.changed, false);
  assert.equal(calls.length, 1);
  assert.deepEqual(counts(), { refreshes: 0, dataEvents: 0 });
});

test("固定点确认只带原 preflightId 重试一次", async () => {
  const retry = { changed: true, insertedRows: 0, removedRows: 0,
    fixedPoint: null, cancelled: false };
  const { model, calls, counts } = makeFixedPointModel(async () => true, retry);
  const outcome = await model.setCells([[0, 0, "x"]]);
  assert.equal(outcome.changed, true);
  assert.equal(calls.length, 2);
  assert.deepEqual(calls[1][1].approval, { allowFixedPoint: true, preflightId: 7 });
  assert.deepEqual(counts(), { refreshes: 1, dataEvents: 1 });
});

test("固定点过期重试抛错且不刷新", async () => {
  const retry = { changed: false, insertedRows: 0, removedRows: 0,
    fixedPoint: { preflightId: 8 }, cancelled: false };
  const { model, calls, counts } = makeFixedPointModel(async () => true, retry);
  await assert.rejects(model.setCells([[0, 0, "x"]]), /预检已过期/);
  assert.equal(calls.length, 2);
  assert.deepEqual(counts(), { refreshes: 0, dataEvents: 0 });
});
```

- [x] **Step 2: 运行 RED**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib engine::tests::p1_ -- --nocapture
cd "D:/SVN_Cursor/simpleTable-public"
node --test --test-name-pattern="fixedPoint|固定点" tests_js/bigmodel.test.mjs
```

预期：Rust 因新签名未实现失败；Node 因 `_invokeEdit` 和取消语义不存在失败。

- [x] **Step 3: 用统一预检/提交替换固定条数与字符上限**

删除 `UNDO_CAP`、`UNDO_CELL_CHAR_CAP` 和旧 `push_undo`，把双栈字段替换为 `history: HistoryLedger<Op>`，新增 `history_generation: u64`、`fixed_point: bool`、`savepoint_detached: bool`，把 `revision` 改为 `i64`。`modified()` 必须是 `self.savepoint_detached || self.revision != 0`。新文档操作在原 revision<0 时会清除包含保存点的 redo 分支，先设 savepoint_detached=true；固定点提交在原 revision!=0 时保存点已不可达，也设 true。undo/redo 只改 revision，不清 detached；每次成功保存才 revision=0 且 detached=false。普通淘汰保持 revision 不动，使不可达保存点不会因删历史而假 clean。

```rust
enum HistoryDecision {
    Normal(Prepared<Op>),
    FixedPoint(Prepared<Op>),
    Confirm(EditOutcome),
}

fn preflight_history(
    &self,
    op: Op,
    operation: &str,
    approval: Approval,
) -> Result<HistoryDecision, String> {
    if approval.allow_fixed_point && approval.preflight_id != Some(self.history_generation) {
        return Err("固定点预检已过期，请重试本次操作".into());
    }
    let entry = op.history_entry().map_err(|e| format!("历史成本计算失败：{e}"))?;
    self.revision.checked_add(entry.revision_delta)
        .ok_or_else(|| "内容版本号溢出".to_string())?;
    self.history_generation.checked_add(1).ok_or_else(|| "历史代际溢出".to_string())?;
    let prepared = self.history.prepare(entry).map_err(|e| format!("历史记账失败：{e:?}"))?;
    if prepared.requires_fixed_point() {
        let request = FixedPointRequest {
            kind: FixedPointKind::StepOverBudget,
            operation: operation.into(),
            required_bytes: prepared.entry_bytes(),
            budget_bytes: self.history.stats().budget_bytes,
            preflight_id: self.history_generation,
        };
        if !approval.allow_fixed_point {
            let mut outcome = EditOutcome::unchanged();
            outcome.fixed_point = Some(request);
            return Ok(HistoryDecision::Confirm(outcome));
        }
        Ok(HistoryDecision::FixedPoint(prepared))
    } else {
        Ok(HistoryDecision::Normal(prepared))
    }
}
```

`Op::history_entry() -> Result<HistoryEntry<Op>, String>` 用 Task 2 的 `Cost` 计算 `size_of::<Op>()`、所有 Vec/Map 容量、枚举判别和对齐后的结构尺寸、字符串容量；新项 `revision_delta=1`。普通提交只在 `apply` 成功后按上述分支规则更新 detached、清 redo、压 undo，并按 `entry.revision_delta` 对 revision 做 checked_add，再增加代际；固定点提交同样在成功后按该 entry 的 revision_delta 更新 revision（现有编辑均为 +1），随后清双栈并设 `fixed_point=true`，绝不把 revision 归零。成功 undo/redo、预算调整和保存也增加 history_generation，使旧确认必然过期。任何 prepare/apply 错误都不动 redo、revision、detached 和数据。

- [x] **Step 4: 迁移五个编辑入口和 Tauri/JS 返回值**

新 Rust 签名为：

```rust
pub fn set_cell(&mut self, vrow: u64, col: u32, value: String, approval: Approval)
    -> Result<EditOutcome, String>;
pub fn set_cells(&mut self, cells: Vec<(u64, u32, String)>, approval: Approval)
    -> Result<EditOutcome, String>;
pub fn paste_rows(&mut self, vrow: u64, col: u32, rows: Vec<Vec<String>>, approval: Approval)
    -> Result<EditOutcome, String>;
pub fn insert_rows(&mut self, vpos: u64, rows: Option<Vec<Vec<String>>>, count: u64,
    approval: Approval) -> Result<EditOutcome, String>;
pub fn remove_rows(&mut self, vrows: Vec<u64>, approval: Approval)
    -> Result<EditOutcome, String>;
```

既有 Rust 调用全部传 `Approval::deny()`；只关心旧 bool/count 的断言改读 `.changed`、`.inserted_rows`、`.removed_rows`。删除 `PasteOutcome`，`lib.rs` 五个命令都接收 `approval: Approval` 并返回 `EditOutcome`。空改动在任何固定点检查前返回 `EditOutcome::unchanged()`。

BigModel 增加统一重试和统一收尾；引擎首次未写入，所以取消不能刷新缓存。

```javascript
async _invokeEdit(cmd, args = {}) {
  const first = await this._invoke(cmd, {
    ...args,
    approval: { allowFixedPoint: false, preflightId: null },
  });
  if (!first.fixedPoint) return first;
  const confirmed = await this._confirmFixedPoint(first.fixedPoint);
  if (!confirmed) {
    return { changed: false, insertedRows: 0, removedRows: 0,
      fixedPoint: null, cancelled: true };
  }
  const retry = await this._invoke(cmd, {
    ...args,
    approval: { allowFixedPoint: true, preflightId: first.fixedPoint.preflightId },
  });
  if (retry.fixedPoint) throw new Error("固定点预检已过期，请重试");
  return retry;
}

async _afterEdit(outcome, origin = "edit") {
  if (!outcome?.changed) return outcome;
  this._invalidate();
  // _refreshMeta 已发 onUndoStack，不能重复通知。
  await this._refreshMeta();
  this._emitData(outcome.insertedRows > 0 || outcome.removedRows > 0, origin);
  return outcome;
}
```

构造函数把 `io.confirmFixedPoint` 保存为 `this._confirmFixedPoint`，缺省 `async () => false`。`setCells` 和 `pasteRows` 改走 `_invokeEdit` + `_afterEdit`；`setDataMerged` 保留现有乐观缓存，但只要结果不是 `changed=true`，先恢复旧缓存，取消不刷新。undo/redo 改发 `_emitData(true, "history")`，空操作仍不通知。

- [x] **Step 5: 运行 GREEN 和完整验证**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib engine::tests::p1_ -- --nocapture
cargo test -p simpletable --lib
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
node scripts/verify-local.mjs
```

预期：6 个新 Rust 测试和固定点 Node 测试通过；3 个 P8 测试仍 ignored；普通编辑与既有双冒烟不退化。

- [x] **Step 6: 提交**

```bash
cd "D:/SVN_Cursor/simpleTable-public"
git add src-tauri/src/engine.rs src-tauri/src/history.rs src-tauri/src/lib.rs frontend/js/bigmodel.js tests_js/bigmodel.test.mjs
git diff --cached --check
git commit -m "feat: 统一编辑历史预算与固定点" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: 64 MiB 设置、固定点对话框和全页签脏点同步

**Files:**
- Modify: `frontend/js/prefs.js`
- Modify: `frontend/js/dialogs.js`
- Modify: `frontend/js/bigmodel.js`
- Modify: `frontend/js/app.js:13-28,517-526,1374-1494`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/engine.rs` 预算缩容保存点测试
- Test: `tests_js/prefs.test.mjs`、`tests_js/dialogs.test.mjs`、`tests_js/bigmodel.test.mjs`、`frontend/js/smoke.js`
- Modify local only: `progress.md`

**Interfaces:**
- Consumes: Task 3 的 fixedPoint 请求、`_invokeEdit` 和 `BigMeta.modified`。
- Produces: `UNDO_BUDGET_PREF_KEY = "simpletable.undo_budget.v1"`、`normalizeUndoBudgetMiB`、`createUndoBudgetPrefs`、`setDefaultUndoBudgetMiB`、`BigModel.setUndoBudget`、`fixedPointMessage`、`confirmFixedPointDialog`。

- [x] **Step 1: 写偏好和对话框 RED 测试**

在 `prefs.test.mjs` 追加：

```javascript
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
  assert.equal(createUndoBudgetPrefs(null).get(), 64);
});
```

`dialogs.test.mjs` 保留现有导入，再增加纯文案函数导入和三类请求测试；无须构造 DOM。

```javascript
import { fixedPointMessage } from "../frontend/js/dialogs.js";

const fixedBase = {
  operation: "粘贴", requiredBytes: 9 * 1024 * 1024,
  budgetBytes: 64 * 1024 * 1024, preflightId: 1,
};

test("固定点：单步超预算文案包含体量与撤销边界", () => {
  const text = fixedPointMessage({ ...fixedBase, kind: "stepOverBudget" });
  assert.match(text, /操作：粘贴/);
  assert.match(text, /所需历史体量：9\.0 MB/);
  assert.match(text, /当前每文档预算：64\.0 MB/);
  assert.match(text, /此前历史与本次操作不可撤销/);
  assert.match(text, /之后的新操作可以撤销到固定点/);
});

test("固定点：保存转换显示具体原因", () => {
  const text = fixedPointMessage({
    ...fixedBase, kind: "saveTransform", operation: "Source 列 age 仍需由历史恢复",
  });
  assert.match(text, /Source 列 age 仍需由历史恢复/);
  assert.match(text, /此次保存将建立固定点/);
  assert.match(text, /之后的新操作可以撤销到固定点/);
});

test("固定点：排序保存使用物化专用文案", () => {
  const text = fixedPointMessage({ ...fixedBase, kind: "sortMaterialization" });
  assert.match(text, /按当前排序保存会物化当前行序/);
  assert.doesNotMatch(text, /列历史清空/);
  assert.match(text, /此前历史与本次操作不可撤销/);
});
```

预算对话框说明每页签独立、undo/redo 共用和降预算可能淘汰历史或形成固定点。`bigmodel.test.mjs` 新增预算 payload 测试；open/create 的真实会话隔离由本任务 smoke 两页签覆盖。

```javascript
test("undo budget 按 MiB 转为字节下发", async () => {
  const calls = [];
  const model = new BigModel({
    invoke: async (cmd, args = {}) => {
      calls.push([cmd, args]);
      if (cmd === "big_set_undo_budget") return { rows: 1, cols: 1, modified: false };
      throw new Error("unexpected " + cmd);
    },
    listen: async () => () => {},
  });
  model._meta = { rows: 1, cols: 1 };
  model._emitUndo = () => {};
  await model.setUndoBudget(128);
  assert.deepEqual(calls.map(([cmd, args]) => [cmd, args.budgetBytes]),
    [["big_set_undo_budget", 128 * 1024 * 1024]]);
});
```

`engine.rs` 增加预算缩容固定点的保存点分支测试。这里先撤销到保存点之前，再缩容清掉 redo；之后把预算调回足够大，确保普通编辑不会因 revision 回到 0 而假干净。

```rust
#[test]
fn p1_budget_fixed_point_detaches_unreachable_savepoint() {
    let p = tmp_csv("p1_budget_detached", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_cell(0, 0, "saved".into(), Approval::deny()).unwrap();
    bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
    assert!(bf.undo());
    let change = bf.set_history_budget(1).unwrap();
    assert!(change.fixed_point);
    assert!(bf.modified());
    bf.set_history_budget(4_096).unwrap();
    bf.set_cell(0, 0, "branch".into(), Approval::deny()).unwrap();
    assert!(bf.modified(), "revision=0 也不能回到已不可达的保存点");
    assert!(bf.undo());
    assert!(bf.modified());
    fs::remove_file(&p).ok();
}
```

- [x] **Step 2: 运行 RED**

```bash
cd "D:/SVN_Cursor/simpleTable-public"
node --test --test-name-pattern="撤销预算|固定点|undo budget" tests_js/prefs.test.mjs tests_js/dialogs.test.mjs tests_js/bigmodel.test.mjs
```

预期：新导出和预算命令不存在而 FAIL。

- [x] **Step 3: 实现偏好归一化、会话默认与确认对话框**

在 `prefs.js` 增加：

```javascript
export const UNDO_BUDGET_PREF_KEY = "simpletable.undo_budget.v1";
export const DEFAULT_UNDO_BUDGET_MIB = 64;

export function normalizeUndoBudgetMiB(raw) {
  const n = Number(raw);
  return Number.isInteger(n) && n >= 1 && n <= 1024 ? n : DEFAULT_UNDO_BUDGET_MIB;
}

export function undoBudgetBytes(raw) {
  return normalizeUndoBudgetMiB(raw) * 1024 * 1024;
}

export function createUndoBudgetPrefs(storage) {
  return {
    get() {
      try { return normalizeUndoBudgetMiB(storage?.getItem(UNDO_BUDGET_PREF_KEY)); }
      catch { return DEFAULT_UNDO_BUDGET_MIB; }
    },
    set(value) {
      const normalized = normalizeUndoBudgetMiB(value);
      try { storage?.setItem(UNDO_BUDGET_PREF_KEY, String(normalized)); } catch { /* 偏好不可写 */ }
      return normalized;
    },
  };
}
```

BigModel 模块级默认从 64 MiB 开始，`setDefaultUndoBudgetMiB` 只更新之后 open/create 的默认值；`open/create` 在引擎会话创建后立刻调用 `big_set_undo_budget {budgetBytes}`，再发 reset/data。`BigModel.setUndoBudget(mib)` 使用同一 checked 字节值，成功后保存返回的 BigMeta 并发 onUndoStack。Rust `big_set_undo_budget` 调用 `bf.set_history_budget` 后返回 `bf.meta()`；`BigFile.set_history_budget(u64) -> Result<BudgetChange,String>` 返回实际淘汰和固定点结果。预算调整增加 history generation，若账本最后一条仍过大则设置固定点，但不改内容和 revision；若原 revision 非 0，保存点已不可达，必须同时设置 detached：

```rust
if change.fixed_point && self.revision != 0 {
    self.savepoint_detached = true;
}
```

在 `dialogs.js` 把文案与 DOM 模态分开，便于纯逻辑测试。三类请求共用同一撤销边界，专用前缀不得混用。

```javascript
export function fixedPointMessage(request) {
  const tail = "\n继续后，此前历史与本次操作不可撤销；之后的新操作可以撤销到固定点。\n是否继续？";
  if (request.kind === "stepOverBudget") {
    return "操作：" + request.operation
      + "\n所需历史体量：" + fmtBytes(request.requiredBytes)
      + "\n当前每文档预算：" + fmtBytes(request.budgetBytes) + tail;
  }
  if (request.kind === "saveTransform") {
    return "保存无法安全保留全部历史：" + request.operation
      + "\n此次保存将建立固定点，保留的此前历史将清空。" + tail;
  }
  if (request.kind === "sortMaterialization") {
    return "按当前排序保存会物化当前行序。" + tail;
  }
  throw new Error("未知固定点类型");
}

export async function confirmFixedPointDialog(request) {
  const body = document.createElement("div");
  body.textContent = fixedPointMessage(request);
  return await modal("确认固定点", body, [
    { text: "取消", value: false },
    { text: "继续", value: true, primary: true },
  ]) === true;
}
```

所有 kind 都保留取消，取消不写文件、不改会话。

新增 `undoBudgetDialog(def)`，用 `modal` 创建 number input（1–1024）和上述说明；确认值经 `normalizeUndoBudgetMiB`，取消返回 `null`，非法输入回退 64，不依赖 `intDialog` 的夹取行为代替读取侧归一化。

- [x] **Step 4: 接入五个模型构造点、工具栏和脏点**

app 顶部创建偏好存取器并调用 `setDefaultUndoBudgetMiB(prefs.get())`。保留启动、磁盘打开、resetToEmpty、gridB 和新建五个构造点的现有 io 实参，只统一经过下面的工厂注入确认回调；保持 `bindModelEvents` 先于 open/create。

```javascript
function makeDocumentModel(io) {
  return new BigModel({ ...io, confirmFixedPoint: confirmFixedPointDialog });
}
```

在“快捷键”前插入 id 为 `btn-undo-budget` 的“撤销内存…”按钮。点击后取消立即返回；确认后遍历唯一模型集合并等待全部下发，再保存全局默认：

```javascript
async function changeUndoBudget() {
  const value = await undoBudgetDialog(undoBudgetMiB);
  if (value === null) return;
  const models = new Set([model, ...tabs.map((t) => t.model), gridB.model]);
  try {
    for (const m of models) if (m?.meta()) await m.setUndoBudget(value);
    undoBudgetMiB = value;
    undoPrefs.set(value);
    setDefaultUndoBudgetMiB(value);
    statusMsg(`撤销/重做预算已设为每文档 ${value} MiB`, 3000);
  } catch (err) {
    await errorDialog("设置撤销内存失败", String(err?.message ?? err));
  }
}
```

undo/redo 成功后不再 `markDirty()`，改为 `setDirty(model.modified()); updateStatusText();`。`bindModelEvents` 对 `origin="history"` 不标脏，最终脏点由 wrapper 按引擎 meta 同步；这样保存点之前为负 revision 时仍是脏，redo 回到保存点恢复干净。

- [x] **Step 5: 真机覆盖设置与固定点两分支**

在 `smoke.js` 增加独立合成文档：把预算设为 1 MiB，写入大于 1 MiB 的新值，等待固定点模态并取消，断言旧值、脏点、undo/redo 不变；再次操作并确认，断言新值生效、双栈为空、后续一次普通编辑可撤销到固定点。另开第二个页签设置不同历史长度，改全局预算后两模型都收到新字节数，但各自淘汰结果独立。最后恢复 64 MiB。

运行：

```bash
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib
cd "D:/SVN_Cursor/simpleTable-public"
node scripts/verify-local.mjs
```

预期：Node/Rust 全绿；debug/release 均实际执行取消、确认、双页签下发和保存点脏点往返。

- [x] **Step 6: 提交**

```bash
git add frontend/js/prefs.js frontend/js/dialogs.js frontend/js/bigmodel.js frontend/js/app.js frontend/js/smoke.js src-tauri/src/lib.rs src-tauri/src/engine.rs tests_js/prefs.test.mjs tests_js/dialogs.test.mjs tests_js/bigmodel.test.mjs
git diff --cached --check
git commit -m "feat: 添加每文档撤销内存设置" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: 结构化范围清除，消除前端行列叉积

**Files:**
- Modify: `src-tauri/src/engine.rs` 的编辑准备与 `Op` 分区
- Modify: `src-tauri/src/lib.rs`
- Modify: `frontend/js/bigmodel.js:947-1003`
- Modify: `frontend/js/grid.js:991-1011,1582-1601`
- Test: `src-tauri/src/engine.rs`、`tests_js/bigmodel.test.mjs`、`tests_js/grid.test.mjs`
- Modify local only: `progress.md`

**Interfaces:**
- Consumes: Task 3 的 `Approval`、`EditOutcome` 和普通/固定点提交。
- Produces: `ClearTarget`、`big_clear`、`BigModel.clearRect`、`BigModel.clearRows/Columns` 的结构化 payload。

- [x] **Step 1: 写结构清除和零叉积 RED 测试**

Rust 增加三类结构清除和固定点测试：

```rust
#[test]
fn p1_clear_structured_targets_roundtrip() {
    let p = tmp_csv("p1_clear_targets", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    let targets = [
        ClearTarget::Rows { rows: vec![1] },
        ClearTarget::Columns { cols: vec![1] },
        ClearTarget::Rect { row: 0, col: 0, rows: 2, cols: 2 },
    ];
    for target in targets {
        let before = bf.rows(0, bf.meta().rows);
        assert!(bf.clear(target, Approval::deny()).unwrap().changed);
        let changed = bf.rows(0, bf.meta().rows);
        assert_ne!(changed, before);
        assert!(bf.undo());
        assert_eq!(bf.rows(0, bf.meta().rows), before);
        assert!(bf.redo());
        assert_eq!(bf.rows(0, bf.meta().rows), changed);
    }
    fs::remove_file(&p).ok();
}

#[test]
fn p1_clear_over_budget_preserves_data_and_redo() {
    let p = tmp_csv("p1_clear_fixed", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_cell(0, 0, "redo-base".into(), Approval::deny()).unwrap();
    assert!(bf.undo());
    let before = bf.rows(0, bf.meta().rows);
    let history = bf.history_stats();
    assert_eq!(history.redo_count, 1);
    bf.set_history_budget(history.bytes).unwrap();
    let history = bf.history_stats();
    let first = bf.clear(ClearTarget::Columns { cols: vec![0, 1, 2] },
        Approval::deny()).unwrap();
    assert!(!first.changed);
    assert!(first.fixed_point.is_some());
    assert_eq!(bf.rows(0, bf.meta().rows), before);
    assert_eq!(bf.history_stats(), history);
    fs::remove_file(&p).ok();
}
```

Node 测试使用真实的 [cmd,args] 调用记录。新夹具直接注入 meta，不创建百万行数组。

```javascript
function makeModelWithMeta({ rows, cols }) {
  const calls = [];
  const model = new BigModel({
    invoke: async (cmd, args = {}) => {
      calls.push([cmd, args]);
      if (cmd === "big_clear") {
        return { changed: true, insertedRows: 0, removedRows: 0,
          fixedPoint: null, cancelled: false };
      }
      throw new Error("unexpected " + cmd);
    },
    listen: async () => () => {},
  });
  model._meta = { rows, cols };
  model._refreshMeta = async () => {};
  model._emitData = () => {};
  model._emitUndo = () => {};
  return { model, calls };
}

test("整行整列清除只发送结构，不生成行列叉积", async () => {
  const { model, calls } = makeModelWithMeta({ rows: 1_000_000, cols: 200 });
  await model.clearRows([9, 3, 9]);
  await model.clearColumns([7, 2, 7]);
  await model.clearRect(10, 4, 1000, 20);
  assert.deepEqual(calls.slice(-3).map(([cmd, args]) => [cmd, args.target]), [
    ["big_clear", { kind: "rows", rows: [9, 3, 9] }],
    ["big_clear", { kind: "columns", cols: [7, 2, 7] }],
    ["big_clear", { kind: "rect", row: 10, col: 4, rows: 1000, cols: 20 }],
  ]);
});
```

Grid 使用现有 `makeEditGrid` 原型夹具，追加三个完整测试：

```javascript
test("Delete 单块反向矩形直接发 clearRect", async () => {
  const calls = [];
  const model = {
    clearRect: async (...args) => { calls.push(args); return { changed: true }; },
    clearCells: async () => { throw new Error("不得展开为单元格"); },
  };
  const { g } = makeEditGrid(model);
  g.sel = { blocks: [[4, 2, 1, 0]] };
  g.selectedSet = new Set([0]);
  g.selectedCells = () => { throw new Error("不得调用 selectedCells"); };
  assert.equal(await g.deleteSelection(), true);
  assert.deepEqual(calls, [[1, 0, 4, 3]]);
});

test("Delete 多块选区保留稀疏坐标", async () => {
  const cells = [[0, 0], [2, 2]];
  const calls = [];
  const model = {
    clearCells: async (value) => { calls.push(value); return { changed: true }; },
    clearRect: async () => { throw new Error("多块不得合并成大矩形"); },
  };
  const { g } = makeEditGrid(model);
  g.sel = { blocks: [[0, 0, 0, 0], [2, 2, 2, 2]] };
  g.selectedSet = new Set([0, 1]);
  g.selectedCells = () => cells;
  await g.deleteSelection();
  assert.deepEqual(calls, [cells]);
});

test("剪切复制拒绝时不得清除", async () => {
  const { g } = makeEditGrid({});
  g.selectedSet = new Set([0]);
  let clears = 0;
  g.copySelection = async () => { throw new Error("clipboard denied"); };
  g.deleteSelection = async () => { clears += 1; return true; };
  await assert.rejects(g.cutSelection(), /clipboard denied/);
  assert.equal(clears, 0);
});
```

- [x] **Step 2: 运行 RED**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib engine::tests::p1_clear -- --nocapture
cd "D:/SVN_Cursor/simpleTable-public"
node --test --test-name-pattern="结构|叉积|矩形|剪切" tests_js/bigmodel.test.mjs tests_js/grid.test.mjs
```

预期：Rust 无 `ClearTarget/big_clear`；Node 仍调用 `setCells` 并产生叉积；剪切仍先清后等复制。

- [x] **Step 3: 实现两遍引擎预检和固定点清除**

```rust
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ClearTarget {
    Cells { cells: Vec<(u64, u32)> },
    Rows { rows: Vec<u64> },
    Columns { cols: Vec<u32> },
    Rect { row: u64, col: u32, rows: u64, cols: u32 },
}

pub fn clear(&mut self, target: ClearTarget, approval: Approval)
    -> Result<EditOutcome, String>;
```

`NormalizedClear` 对行、列排序去重并校验 checked 边界；`Cells` 保留稀疏目标。第一遍只迭代实际非空旧值，累计变化数和确定性成本，不创建 `Vec<CellChange>`。空改动立即返回。若估算已超过预算且未确认，返回固定点请求；确认后直接按 normalized target 流式写空串并清历史，不构造全量旧值。估算未超限时第二遍构建稀疏 `Op::SetCells`，再按实际 Vec/String 容量复核成本后提交。两遍之间持有同一文档锁，输入和代际不能变化。

- [x] **Step 4: 前端只发送结构，复制成功后才剪切**

注册 `big_clear(target,approval)`。BigModel 的 `clearCells` 发送 `{kind:"cells",cells}`，`clearRows/Columns` 原样发送去重前的行列数组（引擎统一去重），`clearRect` 发送范围。Grid 增加：

```javascript
_singleSelectionRect() {
  if (this.sel.blocks.length !== 1) return null;
  const [r1, c1, r2, c2] = this.sel.blocks[0];
  return {
    row: Math.min(r1, r2), col: Math.min(c1, c2),
    rows: Math.abs(r2 - r1) + 1, cols: Math.abs(c2 - c1) + 1,
  };
}

async deleteSelection(say = true) {
  const n = this.selectedSet.size;
  if (!n) return false;
  const rect = this._singleSelectionRect();
  const outcome = rect
    ? await this.model.clearRect(rect.row, rect.col, rect.rows, rect.cols)
    : await this.model.clearCells(this.selectedCells());
  if (outcome?.changed && say) this.hooks.statusMsg(`已清除 ${n} 个单元格的内容`, 2000);
  return outcome?.changed === true;
}

async cutSelection() {
  if (!this.selectedSet.size) return false;
  const copied = await this.copySelection();
  if (!copied) return false;
  return this.deleteSelection(false);
}
```

取消 fixedPoint 不显示“已清除”。新增并实际接线统一入口，不能只依赖调用方自觉：

```javascript
_runClipboard(promise) {
  Promise.resolve(promise).catch((err) => {
    this.hooks.statusMsg(String(err?.message ?? err), 3000);
  });
}
```

Ctrl+C 改为 `this._runClipboard(this.copySelection())`，Ctrl+X 改为 `this._runClipboard(this.cutSelection())`，Delete 改为 `this._runClipboard(this.deleteSelection())`；三者都先 preventDefault 并 return。cut 内部 await 复制和清除，清除拒绝会传回同一 catch。本任务先把 `copySelection` 的现有文本路径改为返回 `Promise<boolean>`：空选区 false，系统剪贴板写入 resolve 后 true，拒绝 false；Task 12 再替换大复制实现。

- [x] **Step 5: GREEN、双真机并提交**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib engine::tests::p1_clear -- --nocapture
cargo test -p simpletable --lib
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
node scripts/verify-local.mjs
git add src-tauri/src/engine.rs src-tauri/src/lib.rs frontend/js/bigmodel.js frontend/js/grid.js tests_js/bigmodel.test.mjs tests_js/grid.test.mjs
git diff --cached --check
git commit -m "feat: 使用结构化范围清除历史" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

预期：整行/整列/矩形仍一步撤销，固定点取消不清除；剪切复制失败不清选区；双冒烟通过。

---

### Task 6: 稳定 ColumnId 与共享 Schema 机械迁移

**Files:**
- Create: `src-tauri/src/schema.rs`
- Modify: `src-tauri/src/lib.rs` 模块声明
- Modify: `src-tauri/src/engine.rs` 的 `CellId`、读写字段、筛选/排序列解析
- Test: `src-tauri/src/schema.rs`、既有 `engine::tests`
- Modify local only: `progress.md`

**Interfaces:**
- Consumes: Task 2 的 `Cost` 和 `SharedCharge`。
- Produces: `ColumnId`、`ColumnStorage`、`Column`、`Schema`、`Arc<Schema>`；后续所有 `CellId` 的 `col` 均为 `ColumnId`。

- [x] **Step 1: 写稳定列身份 RED 测试**

```rust
use crate::history::SharedCharge;
use std::sync::Arc;

#[test]
fn schema_ids_survive_insert_delete_rename_and_reorder() {
    let mut schema = Schema::from_headers(vec!["A".into(), "B".into(), "C".into()]).unwrap();
    let a = schema.visible()[0];
    let b = schema.visible()[1];
    let inserted = schema.insert(1, "X".into()).unwrap();
    assert_eq!(schema.visible(), &[a, inserted, b, schema.visible()[3]]);
    schema.rename(b, "Bee".into()).unwrap();
    schema.delete(a).unwrap();
    assert_eq!(schema.resolve(b).unwrap().header, "Bee");
    assert_eq!(schema.storage(b), Some(ColumnStorage::Source(1)));
    assert!(!schema.visible().contains(&a));
    assert!(schema.resolve(a).is_some()); // 隐藏身份仍可供历史恢复
}

#[test]
fn schema_history_charge_does_not_depend_on_document_rows() {
    let schema = Arc::new(Schema::from_headers(vec!["A".into(), "B".into()]).unwrap());
    let before = schema.history_bytes();
    let same = schema.clone();
    assert_eq!(before, same.history_bytes());
    assert_eq!(SharedCharge::of(&schema, before), SharedCharge::of(&same, before));
}
```

- [x] **Step 2: 运行 RED**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib schema::tests -- --nocapture
```

预期：`Schema` 和 `ColumnId` 未定义而 FAIL。

- [x] **Step 3: 实现 Schema 值对象**

```rust
use crate::history::Cost;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ColumnId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnStorage {
    Source(u32),
    Inserted(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub storage: ColumnStorage,
    pub header: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schema {
    visible: Vec<ColumnId>,
    columns: BTreeMap<ColumnId, Column>,
    next_id: u32,
    next_inserted_storage: u32,
    history_bytes: u64,
}

impl Schema {
    pub fn from_headers(headers: Vec<String>) -> Result<Self, String> {
        let n = u32::try_from(headers.len()).map_err(|_| "列数超过 u32".to_string())?;
        let mut visible = Vec::with_capacity(headers.len());
        let mut columns = BTreeMap::new();
        for (i, header) in headers.into_iter().enumerate() {
            let id = ColumnId(i as u32);
            visible.push(id);
            columns.insert(id, Column { storage: ColumnStorage::Source(i as u32), header });
        }
        let mut schema = Self { visible, columns, next_id: n,
            next_inserted_storage: 0, history_bytes: 0 };
        schema.history_bytes = schema.compute_history_bytes()?;
        Ok(schema)
    }

    pub fn visible(&self) -> &[ColumnId] { &self.visible }
    pub fn resolve(&self, id: ColumnId) -> Option<&Column> { self.columns.get(&id) }
    pub fn storage(&self, id: ColumnId) -> Option<ColumnStorage> {
        self.resolve(id).map(|c| c.storage)
    }
    pub fn view_id(&self, view_col: u32) -> Option<ColumnId> {
        self.visible.get(view_col as usize).copied()
    }

    fn compute_history_bytes(&self) -> Result<u64, String> {
        let mut cost = Cost::new();
        cost.add_value::<Self>().map_err(|e| format!("{e:?}"))?;
        cost.add_vec(&self.visible).map_err(|e| format!("{e:?}"))?;
        for column in self.columns.values() {
            cost.add_value::<(ColumnId, Column)>().map_err(|e| format!("{e:?}"))?;
            cost.add((6 * std::mem::size_of::<usize>()) as u64)
                .map_err(|e| format!("{e:?}"))?;
            cost.add_string(&column.header).map_err(|e| format!("{e:?}"))?;
        }
        Ok(cost.bytes())
    }

    fn finish_candidate(mut candidate: Self) -> Result<Self, String> {
        candidate.history_bytes = candidate.compute_history_bytes()?;
        Ok(candidate)
    }

    pub fn insert(&mut self, pos: u32, header: String) -> Result<ColumnId, String> {
        let pos = usize::try_from(pos).map_err(|_| "插入列位置无效".to_string())?;
        if pos > self.visible.len() { return Err("插入列位置越界".into()); }
        let mut next = self.clone();
        let id = ColumnId(next.next_id);
        next.next_id = next.next_id.checked_add(1).ok_or("ColumnId 溢出")?;
        let storage = next.next_inserted_storage;
        next.next_inserted_storage = next.next_inserted_storage.checked_add(1)
            .ok_or("插入列存储编号溢出")?;
        next.visible.insert(pos, id);
        next.columns.insert(id, Column { storage: ColumnStorage::Inserted(storage), header });
        *self = Self::finish_candidate(next)?;
        Ok(id)
    }

    pub fn delete(&mut self, id: ColumnId) -> Result<(), String> {
        if !self.visible.contains(&id) { return Err("列不在当前可见结构中".into()); }
        if self.visible.len() == 1 { return Err("至少保留一列".into()); }
        let mut next = self.clone();
        next.visible.retain(|current| *current != id);
        *self = Self::finish_candidate(next)?;
        Ok(())
    }

    pub fn rename(&mut self, id: ColumnId, header: String) -> Result<(), String> {
        if !self.columns.contains_key(&id) { return Err("列身份不存在".into()); }
        let mut next = self.clone();
        next.columns.get_mut(&id).unwrap().header = header;
        *self = Self::finish_candidate(next)?;
        Ok(())
    }

    pub fn history_bytes(&self) -> u64 { self.history_bytes }
}
```

`from_headers(headers) -> Result<Self,String>` 用 `u32::try_from(headers.len())` 检查列数，分配 `Source(0..n)`、next_id=n、next_inserted_storage=0。`insert/delete/rename` 先 clone 仅列级候选 Schema，在候选上用 checked add 分配 id/storage 并重新计算 history_bytes，全部成功后才 `*self=candidate`；删除只从 `visible` 移除，不擦除 `columns` 中的存储身份，id 禁止重用。

`compute_history_bytes` 用 `Cost::add` 逐项 checked 累加：`size_of::<Schema>()`、`visible.capacity()*size_of::<ColumnId>()`、每节点 `size_of::<(ColumnId,Column)>() + 6*size_of::<usize>() + header.capacity()`。乘法也先 checked_mul；成功值存入 `history_bytes` 字段，getter 只返回已核算值。候选 clone 只是执行期列级临时对象，与文档行数无关，不进入历史。

- [x] **Step 4: 把物理列号改成稳定身份**

`CellId::Src` 和 `CellId::Ins` 的 `col` 都改为 `ColumnId`。BigFile 增加 `schema: Arc<Schema>` 和 `pending_schema: Option<Arc<Schema>>`，原 `base_ncols` 只表示物理源列数，不再充当历史身份。源覆盖层键改为 `(u32, ColumnId)`；插入块中的物理槽由 `ColumnStorage` 解析，新增插入列缺值即为空，不为每行扩容。

读路径先做 pending 预览投影，再用 committed schema 得到 ColumnId 和 storage；写路径因 pending 禁止编辑，只需 committed schema。筛选和排序开始时把视图列一次性解析成 ColumnId，后续不再拿物理列号当身份。粘贴起始列也在预检开始解析。既有 ColOp IPC 格式本期不变。

- [x] **Step 5: 全量 GREEN 后提交**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib schema::tests
cargo test -p simpletable --lib
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
node scripts/verify-local.mjs
git add src-tauri/src/schema.rs src-tauri/src/engine.rs src-tauri/src/lib.rs
git diff --cached --check
git commit -m "refactor: 使用稳定列身份与共享结构" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

预期：新 schema 测试通过；既有内容、筛选、排序和 pending 预览行为不变；P8 三项仍 ignored。

---

### Task 7: 自包含行快照和文档会话 spool

**Files:**
- Create: `src-tauri/src/spool.rs`
- Modify: `src-tauri/Cargo.toml` 增加 `fs2 = "0.4"`
- Modify: `src-tauri/src/lib.rs` 模块声明与 `big_close`
- Modify: `src-tauri/src/engine.rs` 的 `DeletedRow`、插删行和 BigFile 所有权
- Test: `src-tauri/src/spool.rs`、`src-tauri/src/engine.rs`
- Modify local only: `progress.md`

**Interfaces:**
- Consumes: Task 2 的共享计费、Task 6 的稳定 CellId。
- Produces: `SessionSpool`、`RowSnapshot`、`SnapshotIndex`、`history_text_bytes`；`backing_path` 与用户 `path` 分离。

- [x] **Step 1: 写快照和所有权 RED 测试**

```rust
#[test]
fn spool_snapshot_roundtrip_and_drop_cleans_only_owned_dir() {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!(
        "simpletable-spool-test-{}-{nonce}", std::process::id()));
    let spool = SessionSpool::create(&root).unwrap();
    let owned = spool.root().to_path_buf();
    let rows = vec![vec!["a".to_string(), "中文".to_string()], vec!["b".to_string()]];
    let snapshot = RowSnapshot::write(&spool, rows.clone().into_iter()).unwrap();
    assert_eq!(snapshot.read_row(0).unwrap(), rows[0]);
    assert_eq!(snapshot.read_row(1).unwrap(), rows[1]);
    assert!(snapshot.history_bytes() >= "a中文b".len() as u64);
    drop(snapshot);
    drop(spool);
    assert!(!owned.exists());
}

#[test]
fn deleted_source_row_has_content_before_any_save() {
    let p = tmp_csv("p3_snapshot", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    let before = bf.rows(0, 5);
    bf.remove_rows(vec![4], Approval::deny()).unwrap();
    assert!(bf.undo());
    assert_eq!(bf.rows(0, 5), before);
    assert!(bf.redo());
    assert_eq!(bf.rows(0, 5), before[..4]);
    fs::remove_file(&p).ok();
}
```

增加大快照阈值和 owned root 测试；BigFile 关签清理由 Task 14 的真机 CLEANUP_PATH 探针覆盖。

```rust
#[test]
fn large_snapshot_spills_to_disk_without_losing_logical_charge() {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!(
        "simpletable-large-snapshot-{}-{nonce}", std::process::id()));
    let spool = SessionSpool::create(&root).unwrap();
    let rows = vec![vec!["x".repeat(MEM_SNAPSHOT_MAX_BYTES as usize + 1)]];
    let snapshot = RowSnapshot::write(&spool, rows.into_iter()).unwrap();
    assert!(matches!(snapshot, RowSnapshot::Disk(_)));
    assert!(snapshot.history_bytes() > MEM_SNAPSHOT_MAX_BYTES);
    drop(snapshot);
    drop(spool);
    assert!(!root.exists());
}
```

- [x] **Step 2: 运行 RED**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib spool::tests -- --nocapture
cargo test -p simpletable --lib engine::tests::deleted_source_row_has_content_before_any_save -- --exact --nocapture
```

预期：快照类型不存在；旧 DeletedRow::Src 只有 rec 的断言结构无法满足。

- [x] **Step 3: 实现 SessionSpool 与空间/权限预检**

`SessionSpool::create(parent)` 用 `create_dir` 创建调用方给出的全新路径，成功后把 `owned=true`；已存在路径返回错误，绝不复用或清空。BigFile 用 `temp_dir()/simpletable-{pid}-{atomic_session}`，每个会话独立。`fs2::available_space(root)` 加 8 MiB 余量做 `ensure_capacity(required)`；创建时写一个 0 字节 probe 验证权限，写失败即返回且不动文档。

```rust
use crate::history::{Cost, HistoryError, SharedCharge};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub const MEM_SNAPSHOT_MAX_BYTES: u64 = 1024 * 1024;
pub const SPOOL_RESERVE_BYTES: u64 = 8 * 1024 * 1024;

pub struct SessionSpool {
    root: PathBuf,
    owned: bool,
    next_file: AtomicU64,
}

impl SessionSpool {
    pub fn create(root: &Path) -> Result<Self, String> {
        if root.exists() { return Err(format!("spool 路径已存在：{}", root.display())); }
        fs::create_dir(root).map_err(|e| format!("创建 spool 失败：{e}"))?;
        let spool = Self { root: root.to_path_buf(), owned: true, next_file: AtomicU64::new(0) };
        let probe = root.join("write-probe");
        File::create(&probe).map_err(|e| format!("spool 不可写：{e}"))?;
        fs::remove_file(&probe).map_err(|e| format!("清理 spool 探针失败：{e}"))?;
        Ok(spool)
    }

    pub fn root(&self) -> &Path { &self.root }

    pub fn ensure_capacity(&self, required: u64) -> Result<(), String> {
        let needed = required.checked_add(SPOOL_RESERVE_BYTES).ok_or("spool 空间需求溢出")?;
        let available = fs2::available_space(&self.root)
            .map_err(|e| format!("读取 spool 可用空间失败：{e}"))?;
        if available < needed {
            return Err(format!("临时空间不足：需要 {needed} 字节，可用 {available} 字节"));
        }
        Ok(())
    }

    pub fn new_file(&self, kind: &str) -> Result<PathBuf, String> {
        if kind.is_empty() || !kind.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err("spool 文件种类只能含 ASCII 字母、数字和连字符".into());
        }
        let id = self.next_file.fetch_add(1, Ordering::Relaxed);
        let path = self.root.join(format!("{id:016x}-{kind}.tmp"));
        OpenOptions::new().write(true).create_new(true).open(&path)
            .map_err(|e| format!("创建 spool 文件失败：{e}"))?;
        Ok(path)
    }
}

impl Drop for SessionSpool {
    fn drop(&mut self) {
        if self.owned { let _ = fs::remove_dir_all(&self.root); }
    }
}
```

`big_close` 移除 BigEntry 后等待文件锁释放并 drop BigFile；不删除其他会话目录。`backing_path` 初始等于磁盘打开路径，新建内存档为空；`path` 始终只是用户保存身份。后续固定点 generation 只改 backing_path。

- [x] **Step 4: 实现自包含 RowSnapshot 并接入删除历史**

```rust
#[derive(Clone)]
pub enum RowSnapshot {
    Memory(Arc<MemorySnapshot>),
    Disk(Arc<DiskSnapshot>),
}

pub struct MemorySnapshot { rows: Vec<Vec<String>>, history_bytes: u64 }
pub struct DiskSnapshot {
    path: PathBuf,
    index: Vec<SnapshotIndex>,
    logical_bytes: u64,
    history_bytes: u64,
}
pub struct SnapshotIndex { pub offset: u64, pub length: u64 }

fn memory_history_bytes(rows: &Vec<Vec<String>>) -> Result<u64, String> {
    let mut cost = Cost::new();
    cost.add_value::<MemorySnapshot>().map_err(|e| format!("{e:?}"))?;
    cost.add((4 * std::mem::size_of::<usize>()) as u64).map_err(|e| format!("{e:?}"))?;
    cost.add_vec(rows).map_err(|e| format!("{e:?}"))?;
    for row in rows {
        cost.add_vec(row).map_err(|e| format!("{e:?}"))?;
        for value in row { cost.add_string(value).map_err(|e| format!("{e:?}"))?; }
    }
    Ok(cost.bytes())
}

fn disk_history_bytes(path: &Path, index: &Vec<SnapshotIndex>, logical: u64)
    -> Result<u64, String> {
    let mut cost = Cost::new();
    cost.add_value::<DiskSnapshot>().map_err(|e| format!("{e:?}"))?;
    cost.add((4 * std::mem::size_of::<usize>()) as u64).map_err(|e| format!("{e:?}"))?;
    cost.add(logical).map_err(|e| format!("{e:?}"))?;
    cost.add_vec(index).map_err(|e| format!("{e:?}"))?;
    cost.add(path.as_os_str().len() as u64).map_err(|e| format!("{e:?}"))?;
    Ok(cost.bytes())
}

fn write_disk_row(file: &mut File, row: &[String], offset: &mut u64,
    index: &mut Vec<SnapshotIndex>) -> Result<(), String> {
    let cols = u32::try_from(row.len()).map_err(|_| "快照列数超过 u32")?;
    let mut record_bytes = 4u64;
    for value in row {
        record_bytes = record_bytes.checked_add(8).and_then(|n| n.checked_add(value.len() as u64))
            .ok_or("快照行长度溢出")?;
    }
    let start = *offset;
    *offset = offset.checked_add(record_bytes).ok_or("快照偏移溢出")?;
    file.write_all(&cols.to_le_bytes()).map_err(|e| e.to_string())?;
    for value in row {
        file.write_all(&(value.len() as u64).to_le_bytes()).map_err(|e| e.to_string())?;
        file.write_all(value.as_bytes()).map_err(|e| e.to_string())?;
    }
    index.push(SnapshotIndex { offset: start, length: record_bytes });
    Ok(())
}

fn take<'a>(buf: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8], String> {
    let end = pos.checked_add(len).ok_or("快照读取偏移溢出")?;
    if end > buf.len() { return Err("快照记录截断".into()); }
    let out = &buf[*pos..end];
    *pos = end;
    Ok(out)
}

impl RowSnapshot {
    pub fn write<I>(spool: &SessionSpool, rows: I) -> Result<Self, String>
    where I: Iterator<Item = Vec<String>> {
        let mut buffered = Vec::<Vec<String>>::new();
        let mut logical_bytes = 0u64;
        let mut disk: Option<(PathBuf, File, Vec<SnapshotIndex>, u64)> = None;
        for row in rows {
            for value in &row {
                logical_bytes = logical_bytes.checked_add(value.len() as u64)
                    .ok_or("快照逻辑内容长度溢出")?;
            }
            if disk.is_none() && logical_bytes <= MEM_SNAPSHOT_MAX_BYTES {
                buffered.push(row);
                continue;
            }
            if disk.is_none() {
                let path = spool.new_file("rows")?;
                spool.ensure_capacity(logical_bytes)?;
                let mut file = OpenOptions::new().write(true).open(&path)
                    .map_err(|e| e.to_string())?;
                file.write_all(b"STSP").and_then(|_| file.write_all(&0u64.to_le_bytes()))
                    .map_err(|e| e.to_string())?;
                let mut index = Vec::new();
                let mut offset = 12u64;
                for old in buffered.drain(..) {
                    write_disk_row(&mut file, &old, &mut offset, &mut index)?;
                }
                disk = Some((path, file, index, offset));
            }
            let (_, file, index, offset) = disk.as_mut().unwrap();
            write_disk_row(file, &row, offset, index)?;
        }
        if let Some((path, mut file, index, _)) = disk {
            let count = u64::try_from(index.len()).map_err(|_| "快照行数超过 u64")?;
            file.seek(SeekFrom::Start(4)).and_then(|_| file.write_all(&count.to_le_bytes()))
                .map_err(|e| e.to_string())?;
            file.flush().map_err(|e| e.to_string())?;
            let history_bytes = disk_history_bytes(&path, &index, logical_bytes)?;
            return Ok(Self::Disk(Arc::new(DiskSnapshot {
                path, index, logical_bytes, history_bytes,
            })));
        }
        let history_bytes = memory_history_bytes(&buffered)?;
        Ok(Self::Memory(Arc::new(MemorySnapshot { rows: buffered, history_bytes })))
    }

    pub fn read_row(&self, row: usize) -> Result<Vec<String>, String> {
        match self {
            Self::Memory(value) => value.rows.get(row).cloned().ok_or("快照行越界".into()),
            Self::Disk(value) => {
                let item = value.index.get(row).ok_or("快照行越界")?;
                let mut file = File::open(&value.path).map_err(|e| e.to_string())?;
                let mut header = [0u8; 12];
                file.read_exact(&mut header).map_err(|e| e.to_string())?;
                if &header[..4] != b"STSP" { return Err("快照 magic 错误".into()); }
                let count = u64::from_le_bytes(header[4..12].try_into().unwrap());
                if count != value.index.len() as u64 { return Err("快照记录数不一致".into()); }
                file.seek(SeekFrom::Start(item.offset)).map_err(|e| e.to_string())?;
                let len = usize::try_from(item.length).map_err(|_| "快照行超出内存地址范围")?;
                let mut buf = vec![0u8; len];
                file.read_exact(&mut buf).map_err(|e| e.to_string())?;
                let mut pos = 0usize;
                let cols = u32::from_le_bytes(take(&buf, &mut pos, 4)?.try_into().unwrap());
                let mut out = Vec::with_capacity(cols as usize);
                for _ in 0..cols {
                    let len = u64::from_le_bytes(take(&buf, &mut pos, 8)?.try_into().unwrap());
                    let len = usize::try_from(len).map_err(|_| "快照字段超出地址范围")?;
                    out.push(String::from_utf8(take(&buf, &mut pos, len)?.to_vec())
                        .map_err(|_| "快照字段不是 UTF-8")?);
                }
                if pos != buf.len() { return Err("快照记录存在多余字节".into()); }
                Ok(out)
            }
        }
    }

    pub fn history_bytes(&self) -> u64 {
        match self { Self::Memory(v) => v.history_bytes, Self::Disk(v) => v.history_bytes }
    }
    pub fn shared_charge(&self) -> SharedCharge {
        match self {
            Self::Memory(value) => SharedCharge::of(value, self.history_bytes()),
            Self::Disk(value) => SharedCharge::of(value, self.history_bytes()),
        }
    }
}

pub fn history_text_bytes(utf8_len: u64) -> Result<u64, HistoryError> {
    utf8_len.checked_add((4 * std::mem::size_of::<usize>()) as u64)
        .ok_or(HistoryError::CostOverflow)
}
```

磁盘格式为 magic、记录数、逐行 `u32 列数 + 逐格 u64 UTF-8 长度 + 原始 UTF-8 字节`，index 指向每行起点；所有长度和偏移 checked。小快照才保留 Memory，大快照写 Disk，但两者都按内容、Vec/Arc/index 结构计历史。`DeletedRow` 改为保存原 `CellId` 行身份、原 vpos 和 `RowSnapshot` 行引用，撤销恢复读取快照而非假定源 rec 仍有字节。

- [x] **Step 5: GREEN、双真机并提交**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib spool::tests
cargo test -p simpletable --lib
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
node scripts/verify-local.mjs
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/spool.rs src-tauri/src/engine.rs src-tauri/src/lib.rs
git diff --cached --check
git commit -m "feat: 为删除历史添加自包含行快照" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

预期：快照 roundtrip、批量删除、关签清理通过；Cargo.lock 只出现 fs2 及其必要锁定变化。

---

### Task 8: BaselineTransform 与普通保存历史重绑定

**Files:**
- Create: `src-tauri/src/baseline.rs`
- Modify: `src-tauri/src/history.rs` 增加只读遍历和预建双栈提交
- Modify: `src-tauri/src/engine.rs:7184-7632` 及行/格身份
- Modify: `src-tauri/src/lib.rs` 保存命令
- Modify: `frontend/js/bigmodel.js`、`frontend/js/app.js` 保存收尾
- Test: `src-tauri/src/baseline.rs`、`src-tauri/src/engine.rs`、P8 三项解除 ignored
- Modify local only: `progress.md`

**Interfaces:**
- Consumes: Task 6 ColumnId、Task 7 RowSnapshot/backing_path。
- Produces: `RowId`、`BaselineTransform`、`PreparedHistory`、`SaveOutcome`；普通保存保留可证明安全的历史。

- [x] **Step 1: 运行 P8 RED 并增加交错保存测试**

先运行 Task 1 三条 ignored，确认仍是原失败。随后新增：

```rust
#[test]
fn p3_save_interleaved_delete_cell_history_rebinds_in_order() {
    let p = tmp_csv("p3_interleaved", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    let before = bf.rows(0, 5);
    bf.set_cell(4, 0, "last-before-delete".into(), Approval::deny()).unwrap();
    bf.remove_rows(vec![4], Approval::deny()).unwrap();
    bf.insert_rows(1, Some(vec![vec!["X".into(), "Y".into(), "Z".into()]]), 1,
        Approval::deny()).unwrap();
    bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
    assert!(bf.undo()); // 撤销插入
    assert!(bf.undo()); // 恢复最后行及快照
    assert!(bf.undo()); // 撤销更早的末行编辑
    assert_eq!(bf.rows(0, 5), before);
    assert!(bf.redo());
    assert!(bf.redo());
    assert!(bf.redo());
    fs::remove_file(&p).ok();
}

```

将 Task 1 三条测试更新为新签名并移除 `#[ignore]`。此时它们及交错用例必须仍 RED；保存点分支只由 Task 3 的测试覆盖，本任务不重复添加可能立即 GREEN 的用例。

- [x] **Step 2: 实现 RowId 与区间转换**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RowId {
    Source(u32),
    Inserted { block: u32, row: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordRange {
    pub old_start: u32,
    pub old_end: u32, // 半开区间
    pub new_start: u32,
}

#[derive(Clone, Debug)]
pub struct BaselineTransform {
    pub ranges: Vec<RecordRange>,
    pub inserted_rows: BTreeMap<(u32, u32), u32>,
    pub deleted_sources: BTreeSet<u32>,
    /// 历史仍引用的已删除 Source rec -> 预留 Inserted block（row=0）。
    pub suspended_rows: BTreeMap<u32, u32>,
    pub columns: BTreeMap<ColumnId, u32>,
}
```

`CellId` 改为 `{row: RowId, col: ColumnId}`。连续幸存源行只在 old/new 同时连续时合并 range；100,000 个连续行只能有一项。插入行用稀疏 map，删除源行在保存转换时分配唯一空闲 Inserted block（row=0）作为 suspended 身份，CellId 与删除项同步重绑定；撤销恢复为可见 Inserted 后，再次保存可物化回 Source，禁止写到其他 rec。

- [x] **Step 3: 预建可失败的转换，提交只做无失败交换**

先在引擎定义保存转换所需的共享值：

```rust
pub struct SharedText { text: Box<str> }

pub enum ValueRef {
    Source {
        row: RowId,
        col: ColumnId,
        offset: u64,
        raw_len: u64,
        utf8_len: u64,
        index_epoch: u64,
    },
    Text(Arc<SharedText>),
}
```

HistoryLedger 增加 `iter_undo/iter_redo` 和 `prepare_rebuild(undo,redo,budget) -> Result<PreparedHistory,HistoryError>`。保存前遍历全部 Op，生成转换后的新 entries；ValueRef 使用 Arc/源引用，clone 只复制指针，不复制整表字符串。仍指向旧 backing 的 Source 在临时文件阶段迁移为 Text，双方都用 Task 7 的 history_text_bytes，因此总账不变。若任何行、列、块或快照无法转换，返回带具体 operation 的 SaveTransform 请求，不得静默保留旧身份。

保存接口统一为：

```rust
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveOutcome {
    pub saved: bool,
    pub fixed_point: Option<FixedPointRequest>,
    pub cancelled: bool,
}

pub fn save(&mut self, approval: Approval, progress: impl FnMut(u64, u64, u32, u64))
    -> Result<SaveOutcome, String>;
pub fn save_to(&mut self, dest: &Path, switch_path: bool, approval: Approval,
    progress: impl FnMut(u64, u64, u32, u64)) -> Result<SaveOutcome, String>;
```

预检顺序固定为：生成 transform、扫描全部历史、迁移仍需要的源值/快照、预留转换内存、写目标同目录临时文件、用临时路径验证可重开和预期 meta、准备 `PreparedHistory`。保存入口先校验确认代际；转换失败用下面的完整构造，未确认时在写临时文件前返回 `saved=false + fixedPoint`。

```rust
if approval.allow_fixed_point
    && approval.preflight_id != Some(self.history_generation) {
    return Err("固定点预检已过期，请重试本次保存".into());
}

fn save_fixed_point_request(&self, kind: FixedPointKind, operation: String)
    -> FixedPointRequest {
    FixedPointRequest {
        kind,
        operation,
        required_bytes: self.history.stats().bytes,
        budget_bytes: self.history.stats().budget_bytes,
        preflight_id: self.history_generation,
    }
}

// 转换失败的分支；operation 与 row_or_column_reason 均取刚失败 Op 的错误上下文。
let request = self.save_fixed_point_request(
    FixedPointKind::SaveTransform,
    format!("{operation}：{row_or_column_reason}"),
);
if !approval.allow_fixed_point {
    return Ok(SaveOutcome { saved: false, fixed_point: Some(request), cancelled: false });
}
```

确认重试必须仍持有同一 `history_generation`；保存与排序保存都在入口执行相同检查，过期时返回错误，不重新确认、不写文件。目标替换是唯一提交点；之后只采用已准备 backing/index、交换 PreparedHistory、更新 path/backing_path、revision=0、savepoint_detached=false、history_generation+1 和 index_epoch+1，不再做可失败转换。

- [x] **Step 4: 接入保存取消和固定点重试**

BigModel 增加 `_invokeSave(cmd,args)`，首次传 deny；收到 fixedPoint 时用同一 `confirmFixedPointDialog`，取消返回 false 且调用方不得 setDirty(false)，确认后带 preflightId 重试。`save/saveAs` 只有 `saved=true` 才执行 `_afterWrite()`。`runBigWrite` 检查这个 false：关闭进度框并直接返回 false，不能显示“已保存”。

普通保存成功后双栈保留、revision=0；确认固定点保存成功后双栈清空、revision=0。取消、临时写失败、重开校验失败和换名失败均保持活动数据、原历史、redo、path 和 dirty；既有另存/占用恢复测试不得弱化。

- [x] **Step 5: 解除 P8 ignore 并完整验证**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib engine::tests::p8_ -- --nocapture
cargo test -p simpletable --lib engine::tests::p3_ -- --nocapture
cargo test -p simpletable --lib
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
node scripts/verify-local.mjs
```

预期：P8 三项和交错保存全部通过；保存后连续 undo/redo 位置精确；取消与失败锚点保持；Rust 只剩原来的 2 个 bench ignored。

- [x] **Step 6: 提交**

```bash
git add src-tauri/src/baseline.rs src-tauri/src/history.rs src-tauri/src/spool.rs src-tauri/src/engine.rs src-tauri/src/lib.rs frontend/js/bigmodel.js frontend/js/app.js tests_js/bigmodel.test.mjs
git diff --cached --check
git commit -m "feat: 保存后重绑定撤销历史身份" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 9: 会话内列执行、粘贴扩列合并与删除列保存固定点

**Files:**
- Modify: `src-tauri/src/schema.rs`
- Modify: `src-tauri/src/baseline.rs`
- Modify: `src-tauri/src/engine.rs:7349-7534` 及粘贴/保存
- Modify: `src-tauri/src/lib.rs`
- Modify: `frontend/js/bigmodel.js`、`frontend/js/grid.js`、`frontend/js/app.js`
- Test: Rust schema/engine、Node bigmodel/grid、`frontend/js/smoke.js`
- Modify local only: `progress.md`

**Interfaces:**
- Consumes: Task 6 Schema、Task 8 BaselineTransform/SaveOutcome。
- Produces: `Op::Columns`、扩展的 `Op::Paste { schema_change }`；`big_col_apply(approval) -> EditOutcome`。

- [x] **Step 1: 写列往返和扩列原子性 RED**

```rust
#[test]
fn p2_column_execute_is_undoable_without_rewriting_source() {
    let p = tmp_csv("p2_columns", &people_csv());
    let original = fs::read(&p).unwrap();
    let mut bf = BigFile::open(&p).unwrap();
    bf.col_push(ColOp::Insert { pos: 1, name: "X".into() }).unwrap();
    bf.col_push(ColOp::Rename { col: 0, name: "AA".into() }).unwrap();
    let outcome = bf.apply_col_ops(Approval::deny()).unwrap();
    assert!(outcome.changed);
    assert!(bf.modified());
    assert_eq!(bf.meta().headers, vec!["AA", "X", "age", "city"]);
    assert_eq!(fs::read(&p).unwrap(), original);
    assert!(bf.undo());
    assert_eq!(bf.meta().headers, vec!["name", "age", "city"]);
    assert!(bf.redo());
    assert_eq!(bf.meta().headers, vec!["AA", "X", "age", "city"]);
    fs::remove_file(&p).ok();
}

#[test]
fn p2_save_interleaved_columns_rows_cells_roundtrip() {
    let p = tmp_csv("p2_save_all_ops", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    let before = bf.rows(0, bf.meta().rows);
    let before_headers = bf.meta().headers;
    bf.set_cell(4, 0, "last-before-structure".into(), Approval::deny()).unwrap();
    bf.insert_rows(1, Some(vec![vec!["X".into(), "Y".into(), "Z".into()]]), 1,
        Approval::deny()).unwrap();
    bf.col_push(ColOp::Insert { pos: 1, name: "extra".into() }).unwrap();
    bf.apply_col_ops(Approval::deny()).unwrap();
    let after = bf.rows(0, bf.meta().rows);
    let after_headers = bf.meta().headers;
    let saved = bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
    assert!(saved.saved, "插入列仍在可见 schema 中，不应要求固定点");
    assert_eq!(bf.rows(0, bf.meta().rows), after);
    assert!(bf.undo());
    assert!(bf.undo());
    assert!(bf.undo());
    assert_eq!(bf.rows(0, bf.meta().rows), before);
    assert_eq!(bf.meta().headers, before_headers);
    assert!(bf.redo());
    assert!(bf.redo());
    assert!(bf.redo());
    assert_eq!(bf.rows(0, bf.meta().rows), after);
    assert_eq!(bf.meta().headers, after_headers);
    fs::remove_file(&p).ok();
}
```

普通扩列粘贴与超预算固定点是两条不同路径：前者一次 undo/redo，后者确认后本次操作本身不可撤销。追加以下测试，禁止把两种语义混在一条断言里。

```rust
#[test]
fn p2_paste_expand_undo_redo_is_atomic() {
    let p = tmp_csv("p2_paste_expand", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    let before = bf.rows(0, bf.meta().rows);
    let headers = bf.meta().headers;
    let history = bf.history_stats();
    let rows = vec![vec!["X".to_string(); 4]; 6];
    assert!(bf.paste_rows(u64::MAX, 0, rows.clone(), Approval::deny()).is_err());
    assert_eq!(bf.rows(0, bf.meta().rows), before);
    assert_eq!(bf.history_stats(), history);
    assert!(bf.paste_rows(3, 1, rows, Approval::deny()).unwrap().changed);
    assert_eq!(bf.meta().rows, 9);
    assert_eq!(bf.meta().cols, 5);
    let after = bf.rows(0, bf.meta().rows);
    let after_headers = bf.meta().headers;
    assert!(bf.undo());
    assert_eq!(bf.rows(0, bf.meta().rows), before);
    assert_eq!(bf.meta().headers, headers);
    assert!(bf.redo());
    assert_eq!(bf.rows(0, bf.meta().rows), after);
    assert_eq!(bf.meta().headers, after_headers);
    fs::remove_file(&p).ok();
}

#[test]
fn p2_over_budget_paste_cancel_then_fixed_point() {
    let p = tmp_csv("p2_paste_fixed", &people_csv());
    let original = fs::read(&p).unwrap();
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_history_budget(4_096).unwrap();
    let before = bf.rows(0, bf.meta().rows);
    let rows = vec![vec!["x".repeat(128 * 1024)]];
    let first = bf.paste_rows(0, 3, rows.clone(), Approval::deny()).unwrap();
    assert!(!first.changed);
    let request = first.fixed_point.unwrap();
    assert_eq!(bf.rows(0, bf.meta().rows), before);
    let done = bf.paste_rows(0, 3, rows,
        Approval::allow(request.preflight_id)).unwrap();
    assert!(done.changed);
    assert_eq!(bf.history_stats().undo_count, 0);
    assert!(!bf.undo());
    assert_eq!(fs::read(&p).unwrap(), original);
    fs::remove_file(&p).ok();
}

#[test]
fn p2_delete_source_column_save_requires_fixed_point() {
    let p = tmp_csv("p2_delete_source_save", &people_csv());
    let original = fs::read(&p).unwrap();
    let mut bf = BigFile::open(&p).unwrap();
    bf.col_push(ColOp::Delete { col: 1 }).unwrap();
    bf.apply_col_ops(Approval::deny()).unwrap();
    let visible_headers = bf.meta().headers;
    let visible_rows = bf.rows(0, bf.meta().rows);
    assert!(bf.meta().can_undo, "必须保留能恢复被删 Source 列的历史");
    let first = bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
    assert!(!first.saved);
    let request = first.fixed_point.unwrap();
    assert_eq!(request.kind, FixedPointKind::SaveTransform);
    assert_eq!(fs::read(&p).unwrap(), original);
    let done = bf.save(Approval::allow(request.preflight_id), |_, _, _, _| {}).unwrap();
    assert!(done.saved);
    assert_eq!(bf.history_stats().undo_count, 0);
    assert_eq!(bf.history_stats().redo_count, 0);
    let reopened = BigFile::open(&p).unwrap();
    assert_eq!(reopened.meta().headers, visible_headers);
    assert_eq!(reopened.rows(0, reopened.meta().rows), visible_rows);
    fs::remove_file(&p).ok();
}
```

- [x] **Step 2: 运行 RED**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib engine::tests::p2_ -- --nocapture
cd "D:/SVN_Cursor/simpleTable-public"
node --test --test-name-pattern="扩列|列操作|fixedPoint" tests_js/bigmodel.test.mjs tests_js/grid.test.mjs
```

预期：旧 apply_col_ops 物理重写并清栈；Grid 仍先 insertColumns/applyColOps，无法满足一次 undo。

- [x] **Step 3: 用 SchemaChange 替代物理执行**

```rust
#[derive(Clone)]
pub struct SchemaChange {
    pub before: Arc<Schema>,
    pub after: Arc<Schema>,
}

enum Op {
    // 既有变体保留
    Columns { change: SchemaChange },
    Paste {
        changes: Vec<CellChange>,
        inserted: Option<PasteInsert>,
        schema_change: Option<SchemaChange>,
    },
}
```

`pending_schema` 继续支持预览。执行时只构造一条 Columns，前后 Arc 共享列存储；成功后 `schema=after`、清 pending、revision+1。undo/redo 只切 Arc，不重读文件、不物理重写、不重建行索引。app 删除“历史丢失”确认和物理进度条，按钮 title 改为“应用列操作（可撤销）”，成功后保持 dirty=true。

- [x] **Step 4: 粘贴扩列并入专用 Paste**

`paste_rows` 在预检开头同时解析起始 ColumnId、目标宽高、末尾扩行和所需新列。需要扩列时从当前 schema clone 列级结构并追加插入列，磁盘档和内存档走同一路径。普通提交一次写入 schema+cells+rows；固定点提交也必须在同一代际提交点一次切换。任何阶段失败都不先执行列操作。

Grid 删除 `insertColumns + applyColOps + confirmColOps` 分支，直接 `await model.pasteRows(...)`。列名沿用既有自动命名规则。取消 fixedPoint 时保持原列数和内容，状态提示“已取消粘贴”。

保存预检扫描所有保留历史需要的 ColumnId：凡是不在当前 visible schema 中、保存后来源会消失的列，返回带具体原因的 fixedPoint；相关历史已淘汰时正常转换。确认固定点保存不复制旧整列，成功后历史清空。

- [x] **Step 5: 真机 GREEN 并提交**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
node scripts/verify-local.mjs
git add src-tauri/src/schema.rs src-tauri/src/baseline.rs src-tauri/src/engine.rs src-tauri/src/lib.rs frontend/js/bigmodel.js frontend/js/grid.js frontend/js/app.js frontend/js/smoke.js tests_js/bigmodel.test.mjs tests_js/grid.test.mjs
git diff --cached --check
git commit -m "feat: 实现列结构与扩列粘贴撤销" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

预期：真机验证列插删改名往返、原 CSV 未提前改写、超宽粘贴一步往返、删除列保存取消/确认两分支。

---

### Task 10: 共享排序排列、筛选投影与排序保存固定点

**Files:**
- Create: `src-tauri/src/order.rs`
- Modify: `src-tauri/src/engine.rs:5542-5705,7184-7348`
- Modify: `src-tauri/src/baseline.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `frontend/js/bigmodel.js`、`frontend/js/app.js`
- Test: `src-tauri/src/order.rs`、Rust sort/save、Node bigmodel、`frontend/js/smoke.js`
- Modify local only: `progress.md`

**Interfaces:**
- Consumes: Task 6 稳定列、Task 8 SaveOutcome、Task 2 共享计费。
- Produces: `RowOrder::{Identity,Perm}`、`Perm`、`Op::Sort`、稳定筛选命中投影。

- [x] **Step 1: 写排序历史与排列成本 RED**

```rust
#[test]
fn p4_sort_and_exit_are_exact_history_without_content_revision() {
    let p = tmp_csv("p4_sort_history", &sort_fixture());
    let mut bf = load_ram(&p);
    let original = bf.rows(0, 5);
    let modified = bf.modified();
    bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: true }],
        Approval::deny()).unwrap();
    let sorted = bf.rows(0, 5);
    assert_ne!(sorted, original);
    assert_eq!(bf.modified(), modified);
    assert!(bf.undo());
    assert_eq!(bf.rows(0, 5), original);
    assert!(bf.redo());
    assert_eq!(bf.rows(0, 5), sorted);
    bf.exit_sort_mode(true, Approval::deny()).unwrap();
    assert_eq!(bf.rows(0, 5), original);
    assert!(bf.undo());
    assert_eq!(bf.rows(0, 5), sorted);
    fs::remove_file(&p).ok();
}

#[test]
fn identity_order_has_no_permutation_allocation() {
    let identity = RowOrder::Identity;
    assert_eq!(identity.history_bytes(), 0);
    assert_eq!(identity.display_to_orig(123), 123);
}
```

追加筛选成员与排序保存测试。with_redo=true 时先形成两条排序历史再 undo 一次，因此当前仍是 Perm 且 redo 非空。

```rust
#[test]
fn p4_sort_preserves_filter_members_and_history_flags() {
    let p = tmp_csv("p4_sort_filter", &sort_fixture());
    let mut bf = load_ram(&p);
    bf.run_filter(&spec_one(2, "foo")).unwrap();
    let mut before = bf.rows(0, bf.filter_len() as u64);
    assert_eq!(before.len(), 3, "筛选夹具必须有非空且确定的命中");
    bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: true }],
        Approval::deny()).unwrap();
    let mut after = bf.rows(0, bf.filter_len() as u64);
    assert_eq!(after.len(), 3);
    before.sort(); after.sort();
    assert_eq!(after, before);
    assert!(bf.meta().can_undo);
    assert!(bf.undo());
    assert!(bf.meta().can_redo);
    assert!(bf.redo());
    fs::remove_file(&p).ok();
}

fn assert_sorted_save_fixed_point(with_redo: bool) {
    let name = if with_redo { "p4_sort_save_redo" } else { "p4_sort_save_undo" };
    let p = tmp_csv(name, &sort_fixture());
    let original = fs::read(&p).unwrap();
    let mut bf = load_ram(&p);
    bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: true }],
        Approval::deny()).unwrap();
    if with_redo {
        bf.sort_by_levels(&[SortLevel { col: 1, numeric: false, asc: true }],
            Approval::deny()).unwrap();
        assert!(bf.undo());
    }
    let sorted = bf.rows(0, bf.meta().rows);
    let first = bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
    assert!(!first.saved);
    let request = first.fixed_point.unwrap();
    assert_eq!(request.kind, FixedPointKind::SortMaterialization);
    assert_eq!(fs::read(&p).unwrap(), original);
    let done = bf.save(Approval::allow(request.preflight_id), |_, _, _, _| {}).unwrap();
    assert!(done.saved);
    assert_eq!(bf.history_stats().undo_count, 0);
    assert_eq!(bf.history_stats().redo_count, 0);
    assert!(!bf.modified());
    let reopened = BigFile::open(&p).unwrap();
    assert_eq!(reopened.rows(0, reopened.meta().rows), sorted);
    fs::remove_file(&p).ok();
}

#[test]
fn p4_sorted_save_with_undo_or_redo_requires_fixed_point() {
    assert_sorted_save_fixed_point(false);
    assert_sorted_save_fixed_point(true);
}
```

旧 `save_sorted_clears_undo_history` 改为明确确认路径，不删除内容断言。

- [x] **Step 2: 运行 RED**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib order::tests -- --nocapture
cargo test -p simpletable --lib engine::tests::p4_ -- --nocapture
```

预期：旧 sort 直接清双栈、order 是 Option<Vec<u32>>，新接口和断言失败。

- [x] **Step 3: 实现恒等/共享排列与 Op::Sort**

```rust
use crate::history::{Cost, HistoryError, SharedCharge};
use std::sync::Arc;

#[derive(Clone)]
pub enum RowOrder {
    Identity,
    Perm(Arc<Perm>),
}

pub struct Perm {
    display_to_orig: Vec<u32>,
    history_bytes: u64, // 构造时用 Cost checked 计算 Vec 容量和结构开销
}

impl Perm {
    pub fn new(display_to_orig: Vec<u32>) -> Result<Self, HistoryError> {
        let mut cost = Cost::new();
        cost.add_value::<Self>()?;
        cost.add_vec(&display_to_orig)?;
        Ok(Self { display_to_orig, history_bytes: cost.bytes() })
    }
}

impl RowOrder {
    pub fn display_to_orig(&self, display: u64) -> u64 {
        match self {
            Self::Identity => display,
            Self::Perm(perm) => {
                let index = usize::try_from(display).expect("显示行号超出地址范围");
                u64::from(*perm.display_to_orig.get(index).expect("显示行号超出排列"))
            }
        }
    }
    pub fn history_bytes(&self) -> u64 {
        match self { Self::Identity => 0, Self::Perm(perm) => perm.history_bytes }
    }
    pub fn shared_charge(&self) -> Option<SharedCharge> {
        match self {
            Self::Identity => None,
            Self::Perm(perm) => Some(SharedCharge::of(perm, perm.history_bytes)),
        }
    }
}

enum Op {
    // 既有变体保留
    Sort { before: RowOrder, after: RowOrder, levels: Vec<StableSortLevel> },
}
```

排序命令仍接收视图列号，开始时解析为 `StableSortLevel {col:ColumnId,numeric,asc}`。新排列只生成一次，Op 与活动 RowOrder 共享 Arc；undo/redo 直接恢复 before/after，不重算键、不复制行内容。Identity 不分配 Vec。rank 只在筛选投影需要时临时创建并释放。

BigFile 筛选状态改为 `stable_hits`（原始行身份升序）加 `display_hits` 缓存；排序/撤销/重做由 `order.rs::project_filter(stable_hits,&new_order)` 重建 display_hits。筛选本身不进入 Op，也不在撤销时恢复旧筛选快照。

- [x] **Step 4: 排序只作视图操作，排序保存明确物化**

Sort entry 的 `revision_delta=0`。BigModel.sort 发 `origin="view"`；退出排序不再弹“保存/放弃”对话框，直接形成从当前 Perm 到 Identity 的可撤销操作。releaseRam 只释放执行缓存，不删除历史排列；之后 undo 可在 mmap 上恢复精确 Perm。

排序保存预检在写临时文件前检查双栈：任一非空即返回 `kind=SortMaterialization`、operation 为“按当前排序保存”的 fixedPoint，required_bytes 取当前双栈总账。取消不动文件和会话；确认并成功物化后新物理顺序成为 Identity 固定点、双栈清空、revision=0。双栈为空保持直接保存。普通未排序保存仍走 Task 8 transform。

- [x] **Step 5: 全量 GREEN、真机并提交**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
node scripts/verify-local.mjs
git add src-tauri/src/order.rs src-tauri/src/engine.rs src-tauri/src/baseline.rs src-tauri/src/lib.rs frontend/js/bigmodel.js frontend/js/app.js frontend/js/smoke.js tests_js/bigmodel.test.mjs
git diff --cached --check
git commit -m "feat: 将排序与排序保存纳入统一历史" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

预期：排序/退出/undo/redo 不额外标脏，筛选成员稳定；排序保存取消与确认均真机通过。

---

### Task 11: HeaderMode 可逆历史与渐进索引

**Files:**
- Modify: `src-tauri/src/engine.rs:7101-7160` 及测试
- Modify: `src-tauri/src/schema.rs`、`src-tauri/src/baseline.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `frontend/js/bigmodel.js`、`frontend/js/app.js`
- Test: `src-tauri/src/engine.rs`、`tests_js/bigmodel.test.mjs`、`frontend/js/smoke.js`
- Modify local only: `progress.md`

**Interfaces:**
- Consumes: Task 6 Schema、Task 7 RowSnapshot、Task 8 RowId、Task 10 的零内容 revision。
- Produces: `Op::HeaderMode`、`HeaderModeChange`、`HeaderTop`、`BigFile.index_epoch`；`set_headerless(on,approval) -> EditOutcome`。

- [ ] **Step 1: 写完整索引和渐进索引往返 RED**

```rust
#[test]
fn p4_header_mode_roundtrip_without_renumbering_records() {
    let p = tmp_csv("p4_header_history", &people_csv());
    let original = fs::read(&p).unwrap();
    let mut bf = BigFile::open(&p).unwrap();
    let before = bf.rows(0, bf.meta().rows);
    let dirty = bf.modified();
    let out = bf.set_headerless(true, Approval::deny()).unwrap();
    assert!(out.changed && out.inserted_rows == 1 && out.removed_rows == 0);
    assert_eq!(bf.rows(0, 1)[0], vec!["name", "age", "city"]);
    assert_eq!(bf.modified(), dirty);
    assert!(bf.undo());
    assert_eq!(bf.rows(0, bf.meta().rows), before);
    assert!(bf.redo());
    assert_eq!(bf.rows(0, 1)[0], vec!["name", "age", "city"]);
    assert!(bf.undo());
    bf.set_headerless(true, Approval::deny()).unwrap();
    bf.set_headerless(false, Approval::deny()).unwrap();
    assert_eq!(bf.meta().headers, vec!["name", "age", "city"]);
    assert!(bf.undo());
    assert!(bf.meta().headerless);
    assert!(bf.redo());
    assert!(!bf.meta().headerless);
    assert_eq!(fs::read(&p).unwrap(), original);
    fs::remove_file(&p).ok();
}
```

既有 `big_headerless_progressive_open_and_toggle` 保留构造与内容断言，两次 `set_headerless` 只增加 `Approval::deny()` 实参；Task 12 再统一更新 copy_tsv 签名。追加两个真实测试：

```rust
#[test]
fn p4_header_mode_progressive_history_survives_index_completion() {
    let mut csv = String::from("a,b\r\n");
    for i in 0..20 { csv.push_str(&format!("{i:04},x\r\n")); }
    let p = tmp_csv("p4_header_progressive_history", csv.as_bytes());
    let mut bf = BigFile::open_progressive_full(&p, 29, 1000, 8, false, true).unwrap();
    assert!(bf.partial.is_some());
    let epoch = bf.index_epoch;
    let out = bf.set_headerless(false, Approval::deny()).unwrap();
    assert!(out.changed && out.removed_rows == 1 && out.inserted_rows == 0);
    assert_eq!(bf.meta().rows, 3);
    assert_eq!(bf.meta().headers, vec!["a", "b"]);
    assert_eq!(bf.rows(0, 1)[0], vec!["0000", "x"]);
    assert!(bf.undo());
    assert!(bf.meta().headerless);
    assert_eq!(bf.meta().rows, 4);
    assert_eq!(bf.rows(0, 1)[0], vec!["a", "b"]);
    assert!(bf.redo());
    assert!(!bf.meta().headerless);
    assert_eq!(bf.rows(0, 1)[0], vec!["0000", "x"]);
    bf.set_headerless(true, Approval::deny()).unwrap();
    assert_eq!(bf.rows(0, 1)[0], vec!["a", "b"]);
    bf.drive_index_for_test();
    bf.refresh_indexed();
    assert!(bf.partial.is_none());
    assert_eq!(bf.index_epoch, epoch, "后台索引不得更换 backing 代际");
    assert_eq!(bf.meta().rows, 21);
    assert_eq!(bf.rows(20, 1)[0], vec!["0019", "x"]);
    assert!(bf.undo(), "索引完成后仍撤销同一条 Source(0) 表头历史");
    assert!(!bf.meta().headerless);
    assert_eq!(bf.meta().rows, 20);
    assert_eq!(bf.meta().headers, vec!["a", "b"]);
    assert_eq!(bf.rows(0, 1)[0], vec!["0000", "x"]);
    assert_eq!(bf.rows(19, 1)[0], vec!["0019", "x"]);
    assert!(bf.redo());
    assert!(bf.meta().headerless);
    assert_eq!(bf.rows(0, 1)[0], vec!["a", "b"]);
    fs::remove_file(&p).ok();
}

#[test]
fn p4_header_over_budget_cancel_preserves_filter_redo_and_dirty() {
    let p = tmp_csv("p4_header_fixed", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_cell(0, 0, "redo-base".into(), Approval::deny()).unwrap();
    assert!(bf.undo());
    let old = bf.history_stats();
    bf.set_history_budget(old.bytes).unwrap();
    let history = bf.history_stats();
    bf.run_filter(&spec_one(2, "beijing")).unwrap();
    let hits = bf.rows(0, bf.filter_len() as u64);
    assert_eq!(hits.len(), 2);
    let before = bf.rows(0, bf.meta().rows);
    let headers = bf.meta().headers;
    let dirty = bf.modified();
    let first = bf.set_headerless(true, Approval::deny()).unwrap();
    assert!(!first.changed);
    let request = first.fixed_point.unwrap();
    assert_eq!(bf.rows(0, bf.meta().rows), before);
    assert_eq!(bf.rows(0, bf.filter_len() as u64), hits);
    assert_eq!(bf.meta().headers, headers);
    assert_eq!(bf.modified(), dirty);
    assert_eq!(bf.history_stats(), history);
    let done = bf.set_headerless(true, Approval::allow(request.preflight_id)).unwrap();
    assert!(done.changed);
    assert_eq!(bf.history_stats().undo_count, 0);
    assert_eq!(bf.history_stats().redo_count, 0);
    assert!(!bf.undo());
    fs::remove_file(&p).ok();
}
```

- [ ] **Step 2: 运行 RED**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib engine::tests::p4_header -- --nocapture
```

预期：旧方法无 approval 参数并直接清双栈，新测试编译失败或 undo=false。

- [ ] **Step 3: 用稳定顶部行实现 HeaderModeChange**

```rust
#[derive(Clone)]
pub struct HeaderTop {
    pub row: RowId,
    pub vpos: u64,
    pub snapshot: RowSnapshot,
    pub source_record: Option<u32>,
}

#[derive(Clone)]
pub struct HeaderModeChange {
    pub before_headerless: bool,
    pub after_headerless: bool,
    pub schema_change: SchemaChange,
    pub top: Option<HeaderTop>,
    pub index_epoch: u64,
}

enum Op {
    // 其他专用变体保留
    HeaderMode(HeaderModeChange),
}
```

BigFile 在 open/create 时分配 `index_epoch`，只有更换 backing generation 才递增；后台每次多索引一批记录不递增。完整索引的 false→true 沿用 O(1) 顶部 Ins 行，HeaderTop 记录该 Inserted 身份和原表头快照；渐进索引用 Source(0) 与 source_record=Some(0)，模式偏移改变但源记录号不平移。索引在操作后完成时，undo/redo 仍按 HeaderTop.row 恢复/移除顶部段，并复用同一 index_epoch 校验 backing。

HeaderMode 的 `revision_delta=0`；outcome 用 inserted_rows/removed_rows 各 0 或 1 触发行数通知，但不增加内容 revision。`schema_change` 保存自动生成列名、原表头和目标表头；`top.snapshot` 保存恢复数据行所需内容。成本包括两个共享 Schema、RowSnapshot 和结构开销，顶部行超预算走同一固定点预检。每次正向、逆向应用只清当前 filter/last_spec，不把旧筛选放进历史。

- [ ] **Step 4: 命令、保存转换与前端视图通知**

`big_set_headerless(on,approval)` 返回 EditOutcome。BigModel 用 `_invokeEdit("big_set_headerless",{on})` 和 `_afterEdit(outcome,"view")`；app 不因 view 标脏，undo/redo 仍从引擎 modified 更新真实脏点。排序和 pending 守卫先于任何预检，on 等于当前值返回 unchanged，不清 redo。

BaselineTransform 对 HeaderTop.row、RowSnapshot 和前后 Schema 一并重绑定。普通保存不能留下指向旧顶部 Ins/Source 的 HeaderMode；无法证明时走 Task 8 的保存固定点原因，不静默清栈。

- [ ] **Step 5: 全量 GREEN、真机并提交**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
node scripts/verify-local.mjs
git add src-tauri/src/engine.rs src-tauri/src/schema.rs src-tauri/src/baseline.rs src-tauri/src/lib.rs frontend/js/bigmodel.js frontend/js/app.js frontend/js/smoke.js tests_js/bigmodel.test.mjs
git diff --cached --check
git commit -m "feat: 将首行模式切换纳入撤销历史" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

预期：完整/渐进索引切换均可 undo/redo，无全量 rec 平移，不额外标脏；排序守卫和旧保存内容断言不退化。

---

### Task 12: 8 MiB 流式复制与进程级应用内剪贴板

**Files:**
- Create: `src-tauri/src/clipboard.rs`
- Modify: `src-tauri/src/lib.rs` setup、命令注册与 quit_app
- Modify: `src-tauri/src/engine.rs:7023-7070`
- Modify: `frontend/js/bigmodel.js`、`frontend/js/grid.js`
- Test: `src-tauri/src/clipboard.rs`、Rust copy、Node grid
- Modify local only: `progress.md`

**Interfaces:**
- Consumes: Task 7 临时空间检查、Task 5 的剪切等待。
- Produces: `LARGE_COPY_BYTES=8*1024*1024`、`ClipboardStore`、`CopyOutcome`、`ClipboardMeta`、`big_copy`、`big_clipboard_meta/clear`、`BigModel.clipboardMeta()/clipboardClear()`。

- [ ] **Step 1: 写阈值、所有权和前端不取全文 RED**

```rust
#[test]
fn copy_over_8_mib_returns_token_not_tsv() {
    let p = tmp_csv("p5_large_copy", &wide_9_mib_csv());
    let bf = BigFile::open(&p).unwrap();
    let store = ClipboardStore::create_unique().unwrap();
    let outcome = bf.copy_tsv(0, bf.meta().rows, 0, bf.meta().cols, &store).unwrap();
    let CopyOutcome::App { meta } = outcome else { panic!("必须应用内复制") };
    assert!(meta.bytes > LARGE_COPY_BYTES);
    assert_eq!(meta.version, 1);
    let entry = store.get(&meta.token).unwrap();
    assert_eq!(entry.bytes, meta.bytes);
    assert!(entry.path.exists());
    drop(bf);
    assert!(store.get(&meta.token).unwrap().path.exists());
    store.cleanup();
}

#[test]
fn copy_at_8_mib_stays_text() {
    let bytes = format!("value\n{}\n", "x".repeat(LARGE_COPY_BYTES as usize - 2))
        .into_bytes();
    let p = tmp_csv("p5_copy_equal_limit", &bytes);
    let bf = BigFile::open(&p).unwrap();
    let store = ClipboardStore::create_unique().unwrap();
    let outcome = bf.copy_tsv(0, 1, 0, 1, &store).unwrap();
    let CopyOutcome::Text { text, bytes } = outcome else { panic!("等值必须仍为 Text") };
    assert_eq!(bytes, LARGE_COPY_BYTES);
    assert_eq!(text.len(), LARGE_COPY_BYTES as usize);
    store.cleanup();
    fs::remove_file(&p).ok();
}

#[test]
fn replacing_copy_removes_only_previous_owned_file() {
    let store = ClipboardStore::create_unique().unwrap();
    let first = store.put_test_bytes(b"first").unwrap();
    let old = store.get(&first).unwrap().path.clone();
    let second = store.put_test_bytes(b"second").unwrap();
    assert_ne!(first, second);
    assert!(!old.exists());
    assert!(store.get(&second).unwrap().path.exists());
    store.cleanup();
}
```

`wide_9_mib_csv()` 只生成合成宽行并写入 `tmp_csv` 目标；`put_test_bytes` 位于 `#[cfg(test)]` impl，不成为生产命令。

```rust
fn wide_9_mib_csv() -> Vec<u8> {
    format!("value\n{}\n", "x".repeat(9 * 1024 * 1024)).into_bytes()
}
```

`grid.test.mjs` 复用现有 `makeEditGrid`，追加写剪贴板夹具和三条测试。大结果没有 text 字段，测试同时证明小矩形也走引擎以及剪切失败不清除。

```javascript
function stubClipboardWriter(writes) {
  const old = Object.getOwnPropertyDescriptor(globalThis.navigator, "clipboard");
  Object.defineProperty(globalThis.navigator, "clipboard", {
    value: { writeText: async (text) => { writes.push(text); } }, configurable: true,
  });
  return () => {
    if (old) Object.defineProperty(globalThis.navigator, "clipboard", old);
    else delete globalThis.navigator.clipboard;
  };
}

test("大复制完整矩形只拿 token，不写系统剪贴板", async () => {
  const writes = [];
  const calls = [];
  const model = {
    copyRows: async (...args) => {
      calls.push(args);
      return { kind: "app", meta: { token: "7", rows: 4, cols: 3,
        bytes: 9_000_000, version: 1 } };
    },
    clipboardClear: async () => { throw new Error("App 结果不得清 current"); },
  };
  const { g, messages } = makeEditGrid(model);
  g.sel = { current: [1, 0], blocks: [[4, 2, 1, 0]] };
  g.selectedSet = { size: 200_001 };
  g.selectedCells = () => { throw new Error("完整矩形不得展开"); };
  g.lastCopiedText = null;
  const restore = stubClipboardWriter(writes);
  try { assert.equal(await g.copySelection(), true); } finally { restore(); }
  assert.deepEqual(calls, [[1, 5, 0, 3]]);
  assert.deepEqual(writes, []);
  assert.equal(g.lastCopiedText, null);
  assert.ok(messages.some((x) => x.includes("已复制大区域，仅可在本应用内粘贴")));
});

test("小复制 Text 写系统后才清旧应用内 token", async () => {
  const writes = [];
  const events = [];
  const model = {
    copyRows: async () => ({ kind: "text", text: "A", bytes: 1 }),
    clipboardClear: async () => { events.push("clear"); },
  };
  const { g } = makeEditGrid(model);
  g.sel = { current: [0, 0], blocks: [[0, 0, 0, 0]] };
  g.selectedSet = new Set([0]);
  g.lastCopiedText = null;
  const restore = stubClipboardWriter(writes);
  try { assert.equal(await g.copySelection(), true); } finally { restore(); }
  assert.deepEqual(writes, ["A"]);
  assert.deepEqual(events, ["clear"]);
  assert.equal(g.lastCopiedText, "A");
});

test("剪切复制失败返回 false 且不清除", async () => {
  const writes = [];
  let clears = 0;
  const model = {
    copyRows: async () => { throw new Error("copy failed"); },
    clearRect: async () => { clears += 1; return { changed: true }; },
    clearCells: async () => { clears += 1; return { changed: true }; },
  };
  const { g } = makeEditGrid(model);
  g.sel = { current: [0, 0], blocks: [[0, 0, 0, 0]] };
  g.selectedSet = new Set([0]);
  const restore = stubClipboardWriter(writes);
  try { assert.equal(await g.cutSelection(), false); } finally { restore(); }
  assert.equal(clears, 0);
  assert.deepEqual(writes, []);
});
```

- [ ] **Step 2: 运行 RED**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib clipboard::tests -- --nocapture
cargo test -p simpletable --lib engine::tests::copy_ -- --nocapture
cd "D:/SVN_Cursor/simpleTable-public"
node --test --test-name-pattern="大复制|应用内|剪切" tests_js/grid.test.mjs
```

预期：ClipboardStore/新返回类型不存在，旧 Grid 大复制仍把完整 text 交给系统剪贴板。

- [ ] **Step 3: 实现进程级剪贴板和 CopyBuilder**

```rust
pub const LARGE_COPY_BYTES: u64 = 8 * 1024 * 1024;
pub const CLIPBOARD_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardMeta {
    pub token: String,
    pub rows: u64,
    pub cols: u32,
    pub bytes: u64,
    pub version: u32,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CopyOutcome {
    Text { text: String, bytes: u64 },
    App { meta: ClipboardMeta },
}

pub struct ClipboardEntry {
    pub token: String,
    pub path: PathBuf,
    pub rows: u64,
    pub cols: u32,
    pub bytes: u64,
}

impl Drop for ClipboardEntry {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.path); }
}

pub struct ClipboardStore {
    root: PathBuf,
    current: Mutex<Option<Arc<ClipboardEntry>>>,
    next_token: AtomicU64,
}
```

`ClipboardStore::create_unique` 在系统 temp 下创建本进程专属目录并检查空间/权限。CopyBuilder 小结果最多保留 8 MiB String；流式枚举单元格并累计实际 UTF-8 字节，跨过阈值时创建临时文件、写入已有前缀，之后只用 64 KiB 缓冲继续写。大结果不构造完整 String。格式仍是版本 1 TSV，引号、CRLF、短选区语义与现有序列化一致。

新文件写完并 flush 后才替换 current；旧 token 立即失效，旧文件在该 Arc 的最后一个读取者释放时删除。`get(token)` clone Arc 后释放 Mutex，替换或关签不会使进行中的内部粘贴读到半文件。`clear_current()` 只使 current token 失效并保留可复用 root，供小复制和 big_clipboard_clear 使用；`cleanup()` 才取走 current 并清理自己拥有的 root，供测试结束和进程退出使用。两者都不扫描或删除其他实例目录。

- [ ] **Step 4: 注册进程所有者并改 Grid 复制**

Tauri setup 在 BigShared 后 `app.manage(ClipboardStore::create_unique()?)`。`big_copy(session,r0,r1,c0,c1)` 从进程 State 取 ClipboardStore 并调用 BigFile.copy_tsv；`big_clipboard_meta()` 只返回当前 metadata，`big_clipboard_clear()` 调 clear_current 使 token 失效但保留 store root。quit_app 先调用统一的 `cleanup_process_state(&app)`，再 `app.exit(0)`；RunEvent 和 smoke 直接退出由 Task 14 补齐。

Grid 对单个完整矩形直接根据 `sel.blocks` 计算范围，不因 selectedSet 的 200,000 格缓存上限误退化为非矩形。`所有完整矩形一律调用 BigModel.copyRows，包括只有一格但文本超过 8 MiB 的选区`；格数不再充当体量判据，由引擎按实际 UTF-8 字节返回 Text 或 App。收到 Text 才写系统剪贴板，收到 App 不写系统剪贴板；Text 在系统写入成功后调用并等待 `BigModel.clipboardClear()`，由它发送 `big_clipboard_clear`，保证旧大 token 不覆盖新复制。`lastCopiedText` 只保存 Text，大结果不保存全文。非矩形多选区仍走现有小文本序列化和既有大选区拒绝，不为绕过限制拼接全文。

`copySelection()` 所有分支都返回 Promise<boolean>；`BigModel.copyRows` 直接返回 CopyOutcome，`clipboardMeta()` 读取当前 App 元数据，`clipboardClear()` 只清进程 current。系统写入或引擎失败时 catch 显示错误并返回 false。键盘 Ctrl+C/Ctrl+X/Delete 统一改为 `this._runClipboard(this.copySelection())` 等形式，`_runClipboard` 只负责 `Promise.resolve(p).catch(statusMsg)`，不吞掉 cut 内部的“复制失败不得清除”判断。

- [ ] **Step 5: GREEN、双真机并提交**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
node scripts/verify-local.mjs
git add src-tauri/src/clipboard.rs src-tauri/src/engine.rs src-tauri/src/lib.rs frontend/js/bigmodel.js frontend/js/grid.js frontend/js/smoke.js tests_js/grid.test.mjs
git diff --cached --check
git commit -m "feat: 实现应用内流式大复制" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

预期：8 MiB 等值仍 Text，超过即 App；JS 不收到大 TSV，关闭来源页签后 token 可用，新复制替换旧 token。

---

### Task 13: 外部大粘贴 UTF-8 分块和流式导入

**Files:**
- Create: `frontend/js/clipboard.js`
- Create: `tests_js/clipboard.test.mjs`
- Modify: `src-tauri/src/spool.rs`
- Modify: `src-tauri/src/engine.rs` 粘贴预检与 RecordReader
- Modify: `src-tauri/src/lib.rs` 导入命令
- Modify: `frontend/js/clipboard.js`、`frontend/js/bigmodel.js`、`frontend/js/grid.js`
- Test: `src-tauri/src/spool.rs`、Rust paste、`tests_js/clipboard.test.mjs`、bigmodel/grid
- Modify local only: `progress.md`

**Interfaces:**
- Consumes: Task 2 成本、Task 7 spool、Task 9 Paste schema_change。
- Produces: `PASTE_CHUNK_BYTES=512*1024`、`LARGE_TEXT_BYTES=8*1024*1024`、`PasteDestination`、`PasteImport`、`big_paste_import_begin/chunk/finish/cancel`。

- [ ] **Step 1: 写 UTF-8 边界和 IPC 上限 RED**

```javascript
import test from "node:test";
import assert from "node:assert/strict";
import {
  LARGE_TEXT_BYTES, PASTE_CHUNK_BYTES,
  utf8ByteLength, utf8Chunks, bytesToHex, isSingleValueText,
} from "../frontend/js/clipboard.js";

test("UTF-8 分块不拆代理对且 hex 字符串不超过 1 MiB", () => {
  const text = `a中😀b\r\n${"😀".repeat(200000)}`;
  let joined = "";
  let count = 0;
  for (const chunk of utf8Chunks(text, PASTE_CHUNK_BYTES)) {
    assert.ok(chunk.bytes.length <= PASTE_CHUNK_BYTES);
    assert.ok(bytesToHex(chunk.bytes).length <= 1024 * 1024);
    assert.equal(new TextDecoder().decode(chunk.bytes), text.slice(chunk.start, chunk.end));
    joined += text.slice(chunk.start, chunk.end);
    count += 1;
  }
  assert.equal(joined, text);
  assert.equal(utf8ByteLength(text), new TextEncoder().encode(text).length);
  assert.ok(count > 1);
});

test("大体量探测不构造完整二维数组", () => {
  const text = "中".repeat(3 * 1024 * 1024);
  assert.ok(utf8ByteLength(text, LARGE_TEXT_BYTES + 1) > LARGE_TEXT_BYTES);
  assert.equal(isSingleValueText("abc\r\n"), true);
  assert.equal(isSingleValueText("a\tb"), false);
  assert.equal(isSingleValueText("a\nb"), false);
});
```

Rust 追加记录语义和坏块测试。第 2 块坏 hex、last 后追加都必须失败；测试直接调用命令共用的 PasteImport 方法，不构造假导入器。

```rust
#[test]
fn p5_record_reader_matches_parse_csv_tsv_semantics() {
    let p = tmp_csv("p5_tsv_reader", b"a\t\"b\"\"q\"\r\n\"x\r\ny\"\tz");
    let mut reader = RecordReader::open(&p, b'\t').unwrap();
    assert_eq!(reader.next_record().unwrap(),
        Some(vec!["a".to_string(), "b\"q".to_string()]));
    assert_eq!(reader.next_record().unwrap(),
        Some(vec!["x\r\ny".to_string(), "z".to_string()]));
    assert!(reader.next_record().unwrap().is_none());
    fs::remove_file(&p).ok();
}

#[test]
fn p5_bad_import_chunk_and_post_last_preserve_document() {
    let p = tmp_csv("p5_bad_import", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_cell(0, 0, "redo-base".into(), Approval::deny()).unwrap();
    assert!(bf.undo());
    let before = bf.rows(0, bf.meta().rows);
    let headers = bf.meta().headers;
    let history = bf.history_stats();
    let revision = bf.revision;
    let backing = bf.backing_path().to_path_buf();
    let mut bad = PasteImport::begin(&bf.spool, "bad-hex".into()).unwrap();
    let bad_path = bad.path.clone();
    assert_eq!(bad.append_hex(&bf.spool, "41", false).unwrap(), 1);
    assert!(bad.append_hex(&bf.spool, "0g", false).is_err());
    bad.cancel().unwrap();
    assert!(!bad_path.exists());
    let mut closed = PasteImport::begin(&bf.spool, "post-last".into()).unwrap();
    let closed_path = closed.path.clone();
    assert_eq!(closed.append_hex(&bf.spool, "41", true).unwrap(), 1);
    assert!(closed.append_hex(&bf.spool, "42", false).is_err());
    closed.cancel().unwrap();
    assert!(!closed_path.exists());
    assert_eq!(bf.rows(0, bf.meta().rows), before);
    assert_eq!(bf.meta().headers, headers);
    assert_eq!(bf.history_stats(), history);
    assert_eq!(history.redo_count, 1);
    assert_eq!(bf.revision, revision);
    assert_eq!(bf.backing_path(), backing);
    fs::remove_file(&p).ok();
}
```

`bigmodel.test.mjs` 追加导入协议夹具和固定点两条路径。测试文本很小，因为 Grid 已负责大体量分流；这里验证的是 begin/chunk/finish/cancel 所有权，而不是重复分配大字符串。

```javascript
function makeImportModel(confirm, finish) {
  const calls = [];
  let refreshes = 0;
  let dataEvents = 0;
  let finishes = 0;
  const model = new BigModel({
    invoke: async (cmd, args = {}) => {
      calls.push([cmd, args]);
      if (cmd === "big_paste_import_begin") return { importId: "i1" };
      if (cmd === "big_paste_import_chunk") return { bytes: 3 };
      if (cmd === "big_paste_import_finish") return finish(++finishes);
      if (cmd === "big_paste_import_cancel") return null;
      throw new Error("unexpected " + cmd);
    },
    listen: async () => () => {},
    confirmFixedPoint: confirm,
  });
  model._meta = { rows: 1, cols: 1 };
  model._refreshMeta = async () => { refreshes += 1; };
  model._emitData = () => { dataEvents += 1; };
  model._emitUndo = () => {};
  return { model, calls, counts: () => ({ refreshes, dataEvents }) };
}

const importFixed = {
  kind: "stepOverBudget", operation: "外部粘贴", requiredBytes: 9_000_000,
  budgetBytes: 4096, preflightId: 7,
};
const importRequest = { changed: false, insertedRows: 0, removedRows: 0,
  fixedPoint: importFixed, cancelled: false };

test("外部大粘贴固定点取消会 cancel 且不刷新", async () => {
  const { model, calls, counts } = makeImportModel(async () => false, () => importRequest);
  const out = await model.pasteExternalText("A\tB", { kind: "rect", row: 0, col: 0 });
  assert.equal(out.cancelled, true);
  assert.equal(out.changed, false);
  assert.deepEqual(calls.map(([cmd]) => cmd), [
    "big_paste_import_begin", "big_paste_import_chunk",
    "big_paste_import_finish", "big_paste_import_cancel",
  ]);
  assert.deepEqual(counts(), { refreshes: 0, dataEvents: 0 });
});

test("外部大粘贴确认复用 importId 重试且不重传", async () => {
  const done = { changed: true, insertedRows: 0, removedRows: 0,
    fixedPoint: null, cancelled: false };
  const { model, calls, counts } = makeImportModel(async () => true,
    (attempt) => attempt === 1 ? importRequest : done);
  const out = await model.pasteExternalText("A\tB", { kind: "rect", row: 0, col: 0 });
  assert.equal(out.changed, true);
  assert.deepEqual(calls.map(([cmd]) => cmd), [
    "big_paste_import_begin", "big_paste_import_chunk",
    "big_paste_import_finish", "big_paste_import_finish",
  ]);
  const finishes = calls.filter(([cmd]) => cmd === "big_paste_import_finish");
  assert.equal(finishes[0][1].importId, "i1");
  assert.equal(finishes[1][1].importId, "i1");
  assert.deepEqual(finishes[1][1].approval, { allowFixedPoint: true, preflightId: 7 });
  assert.equal(calls.filter(([cmd]) => cmd === "big_paste_import_chunk")[0][1].last, true);
  assert.deepEqual(counts(), { refreshes: 1, dataEvents: 1 });
});
```

- [ ] **Step 2: 运行 RED**

```bash
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/clipboard.test.mjs
node --test --test-name-pattern="外部大粘贴" tests_js/bigmodel.test.mjs
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib spool::tests::paste -- --nocapture
cargo test -p simpletable --lib engine::tests::p5_ -- --nocapture
```

预期：分块 helper 和导入命令尚不存在；不能先改生产实现再运行本步骤。

- [ ] **Step 3: 实现无全文再编码的 JS 分块**

```javascript
export const LARGE_TEXT_BYTES = 8 * 1024 * 1024;
export const PASTE_CHUNK_BYTES = 512 * 1024;

export function utf8ByteLength(text, stopAfter = Number.POSITIVE_INFINITY) {
  let bytes = 0;
  for (let i = 0; i < text.length;) {
    const cp = text.codePointAt(i);
    i += cp > 0xffff ? 2 : 1;
    bytes += cp <= 0x7f ? 1 : cp <= 0x7ff ? 2 : cp <= 0xffff ? 3 : 4;
    if (bytes >= stopAfter) return bytes;
  }
  return bytes;
}

export function* utf8Chunks(text, maxBytes = PASTE_CHUNK_BYTES) {
  if (!Number.isInteger(maxBytes) || maxBytes < 4) throw new RangeError("maxBytes 至少为 4");
  let start = 0;
  let end = 0;
  let bytes = 0;
  const flush = function* () {
    if (end === start) return;
    const slice = text.slice(start, end);
    yield { start, end, bytes: new TextEncoder().encode(slice) };
  };
  while (end < text.length) {
    const cp = text.codePointAt(end);
    const units = cp > 0xffff ? 2 : 1;
    const n = cp <= 0x7f ? 1 : cp <= 0x7ff ? 2 : cp <= 0xffff ? 3 : 4;
    if (bytes + n > maxBytes) {
      yield* flush();
      start = end;
      bytes = 0;
    }
    end += units;
    bytes += n;
  }
  yield* flush();
}

export function bytesToHex(bytes) {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

export function isSingleValueText(text) {
  let end = text.length;
  while (end > 0 && (text[end - 1] === "\r" || text[end - 1] === "\n")) end -= 1;
  if (!end) return false;
  for (let i = 0; i < end; i += 1) {
    if (text[i] === "\t" || text[i] === "\r" || text[i] === "\n") return false;
  }
  return true;
}
```

`utf8Chunks` 每次只对当前 slice 调 TextEncoder，不 `encode(text)` 全文；`bytesToHex` 最长 1,048,576 字符。`isSingleValueText` 不创建 trim/replace 后的大字符串。

- [ ] **Step 4: 实现 PasteImport 与目标模式**

```rust
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PasteDestination {
    Rect { row: u64, col: u32 },
    SingleCell { row: u64, col: u32 },
    Fill { target: ClearTarget },
}

pub struct PasteImport {
    pub id: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub finished_upload: bool,
    pub row_index: Vec<u64>,
}
```

`PasteImport::begin(&SessionSpool, String)`、`append_hex(&SessionSpool, &str, bool)`、`cancel(self)` 是 Tauri 命令与测试共用的实际方法；append 在写文件前完成 hex、512 KiB、finished_upload 和空间检查，cancel 消费对象并只删除自己的 path。

命令固定为 `big_paste_import_begin(session)->{importId}`、`big_paste_import_chunk(session,importId,hex,last)->{bytes}`、`big_paste_import_finish(session,importId,destination,approval)->EditOutcome`、`big_paste_import_cancel(session,importId)->()`。begin 在目标会话 spool 新建唯一文件；chunk 校验 hex 为偶数长度、仅 ASCII 十六进制、解码后不超过 512 KiB。每次 append 之前必须调用目标会话 `spool.ensure_capacity(decoded_len as u64)`，空间不足与坏 hex 一样在写入前失败；随后顺序 append 并 checked 累计，last 后拒绝再写。失败由统一 cancel 删除本次 import，cancel 不删除其他临时文件。

finish 对 TSV 用流式 RecordReader 两遍扫描：第一遍得到行数、最大宽度、目标地址、扩行/扩列和精确历史成本；只有成本可保留时第二遍逐记录构建稀疏 CellChange/插入块，不产生完整二维输入。SingleCell 在引擎流式做 CRLF/CR→LF 和末尾换行剥离；Fill 只保留一个共享新值，再按结构化 target 计算变化，禁止为每格 clone 大字符串。所有目标先校验，排序态扩行/扩列仍拒绝。

- [ ] **Step 5: Grid 接线、确认重试和取消清理**

BigModel.pasteExternalText(text,destination) 先 begin，再逐 chunk 调 invoke；最后一个块带 `last=true`。finish 走与 `_invokeEdit` 相同的固定点确认，但重试继续使用同一 importId 和 destination，不重传文本。固定点取消时显式调用 cancel 并返回 unchanged/cancelled；chunk、finish、确认过期或确认对话框抛错都在 finally 中 cancel 尚未消费的 import。成功或固定点提交由引擎消费 import，JS 不重复删除。

Grid 在 `readText()` 后先计算体量，不对大文本做 replace 全文副本。小文本维持原分支；大文本只构造 PasteDestination：单值多选区 Fill、外部无 Tab 单格 SingleCell、其余 Rect。forceSplit=true 时一律使用 Rect，覆盖 SingleCell/Fill 判据。大体量一律走导入，不调用 `parseCsv`、不构造 rows、不调用一次 `big_paste` 整包。SingleCell/Fill 的固定点确认后直接写共享新值并清历史；Rect 的固定点由 Task 14 generation 执行。保留 `readText()` 会产生一份完整 JS 字符串的注释和 README 限制说明。

- [ ] **Step 6: GREEN、双真机并提交**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
node scripts/verify-local.mjs
git add src-tauri/src/spool.rs src-tauri/src/engine.rs src-tauri/src/lib.rs frontend/js/clipboard.js frontend/js/bigmodel.js frontend/js/grid.js frontend/js/smoke.js tests_js/clipboard.test.mjs tests_js/bigmodel.test.mjs tests_js/grid.test.mjs
git diff --cached --check
git commit -m "feat: 分块导入外部大粘贴" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

预期：所有 IPC hex 不超过 1 MiB；坏块、取消和过期均零写入；普通可撤销导入仍一步 undo/redo。

---

### Task 14: 内部粘贴、固定点 generation 与完整清理链

**Files:**
- Modify: `src-tauri/src/clipboard.rs`
- Modify: `src-tauri/src/spool.rs`
- Modify: `src-tauri/src/engine.rs` 粘贴与 backing 提交
- Modify: `src-tauri/src/lib.rs:84-92,876-960`
- Modify: `frontend/js/bigmodel.js`、`frontend/js/grid.js`、`frontend/js/smoke.js`
- Modify: `scripts/verify-local.mjs`
- Test: Rust clipboard/spool/engine、Node bigmodel/grid、双 exe smoke
- Modify local only: `progress.md`

**Interfaces:**
- Consumes: Task 12 ClipboardEntry Arc、Task 13 PasteImport/Destination、Task 8 backing_path。
- Produces: `PreparedGeneration`、`backing_encoding/output_encoding`、`cleanup_process_state`、报告 `CLEANUP_PATH=` 探针。

- [ ] **Step 1: 写内部直接粘贴和固定点原子性 RED**

```rust
#[test]
fn p5_internal_fixed_point_paste_preserves_user_file_and_history_on_cancel() {
    let p = tmp_csv("p5_internal_fixed", &people_csv());
    let original = fs::read(&p).unwrap();
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_history_budget(4_096).unwrap();
    bf.set_cell(0, 0, "redo-base".into(), Approval::deny()).unwrap();
    assert!(bf.undo());
    let before = bf.rows(0, bf.meta().rows);
    let before_history = bf.history_stats();
    let store = ClipboardStore::create_unique().unwrap();
    let token = store.put_test_bytes(&large_tsv_128_kib()).unwrap();
    let first = bf.paste_clipboard(&store, &token,
        PasteDestination::Rect { row: 0, col: 0 }, Approval::deny()).unwrap();
    assert!(first.fixed_point.is_some());
    assert_eq!(bf.rows(0, bf.meta().rows), before);
    assert_eq!(bf.history_stats().bytes, before_history.bytes);
    assert_eq!(bf.history_stats().redo_count, before_history.redo_count);
    let request = first.fixed_point.unwrap();
    let done = bf.paste_clipboard(&store, &token,
        PasteDestination::Rect { row: 0, col: 0 },
        Approval::allow(request.preflight_id)).unwrap();
    assert!(done.changed);
    assert_eq!(bf.history_stats().undo_count, 0);
    assert_eq!(bf.history_stats().redo_count, 0);
    assert!(bf.modified());
    assert_eq!(fs::read(&p).unwrap(), original);
    assert_ne!(bf.backing_path(), p);
    assert!(bf.backing_path().exists());
    assert!(!bf.undo());
    let fixed = bf.rows(0, bf.meta().rows);
    bf.set_cell(0, 1, "after-fixed".into(), Approval::deny()).unwrap();
    assert!(bf.undo(), "固定点之后的新操作必须可撤销");
    assert_eq!(bf.rows(0, bf.meta().rows), fixed);
    assert!(!bf.undo(), "不得越过固定点");
    fs::remove_file(&p).ok();
}
```

无效 token、截断 UTF-8 文件和 generation 重开校验失败均必须保留 before/backing/history/redo。版本号是当前进程元数据常量，不能伪造不存在的“错误版本 token”分支。新操作可撤销到固定点但不能越过；排序态只允许不扩行列的固定点粘贴，原 Perm 原样保留。无效 token 与截断输入使用下面的真实错误路径：

```rust
#[test]
fn p5_invalid_or_truncated_clipboard_keeps_generation_and_history() {
    let p = tmp_csv("p5_bad_clipboard", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_cell(0, 0, "redo-base".into(), Approval::deny()).unwrap();
    assert!(bf.undo());
    let before = bf.rows(0, bf.meta().rows);
    let history = bf.history_stats();
    let backing = bf.backing_path().to_path_buf();
    let store = ClipboardStore::create_unique().unwrap();
    let destination = PasteDestination::Rect { row: 0, col: 0 };
    assert!(bf.paste_clipboard(&store, "missing", destination.clone(),
        Approval::deny()).is_err());
    let token = store.put_test_bytes(&[0xf0, 0x9f, 0x98]).unwrap();
    assert!(bf.paste_clipboard(&store, &token, destination, Approval::deny()).is_err());
    assert_eq!(bf.rows(0, bf.meta().rows), before);
    assert_eq!(bf.history_stats(), history);
    assert_eq!(bf.backing_path(), backing);
    store.cleanup();
    fs::remove_file(&p).ok();
}

#[test]
fn p5_generation_reopen_failure_is_atomic() {
    let p = tmp_csv("p5_generation_bad", &people_csv());
    let original = fs::read(&p).unwrap();
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_cell(0, 0, "redo-base".into(), Approval::deny()).unwrap();
    assert!(bf.undo());
    let before = bf.rows(0, bf.meta().rows);
    let history = bf.history_stats();
    let revision = bf.revision;
    let backing = bf.backing_path().to_path_buf();
    let store = ClipboardStore::create_unique().unwrap();
    let token = store.put_test_bytes(&large_tsv_128_kib()).unwrap();
    let destination = PasteDestination::Rect { row: 0, col: 0 };
    let prepared = bf.prepare_paste_generation(store.get(&token).unwrap(), &destination).unwrap();
    let generation = prepared.backing_path.clone();
    fs::write(&generation, [0xf0, 0x9f, 0x98]).unwrap();
    assert!(bf.commit_prepared_generation(prepared).is_err());
    assert!(!generation.exists(), "失败的本次 generation 必须自清理");
    assert_eq!(bf.rows(0, bf.meta().rows), before);
    assert_eq!(bf.history_stats(), history);
    assert_eq!(bf.revision, revision);
    assert_eq!(bf.backing_path(), backing);
    assert_eq!(fs::read(&p).unwrap(), original);
    store.cleanup();
    fs::remove_file(&p).ok();
}

#[test]
fn p5_sorted_fixed_point_paste_keeps_permutation() {
    let p = tmp_csv("p5_sorted_fixed_paste", &sort_fixture());
    let original = fs::read(&p).unwrap();
    let mut bf = load_ram(&p);
    bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: true }],
        Approval::deny()).unwrap();
    let old = bf.history_stats();
    bf.set_history_budget(old.bytes).unwrap();
    let store = ClipboardStore::create_unique().unwrap();
    let token = store.put_test_bytes(&large_tsv_128_kib()).unwrap();
    let destination = PasteDestination::SingleCell { row: 0, col: 0 };
    let first = bf.paste_clipboard(&store, &token, destination.clone(),
        Approval::deny()).unwrap();
    let request = first.fixed_point.unwrap();
    let before_order = (0..bf.meta().rows).map(|r| bf.order.display_to_orig(r))
        .collect::<Vec<_>>();
    let done = bf.paste_clipboard(&store, &token, destination,
        Approval::allow(request.preflight_id)).unwrap();
    assert!(done.changed);
    let after_order = (0..bf.meta().rows).map(|r| bf.order.display_to_orig(r))
        .collect::<Vec<_>>();
    assert_eq!(after_order, before_order);
    assert!(bf.meta().sorted);
    assert_eq!(fs::read(&p).unwrap(), original);
    store.cleanup();
    fs::remove_file(&p).ok();
}
```

`large_tsv_128_kib` 和 `put_test_bytes` 都是合成测试 helper，不接触业务 CSV：

```rust
fn large_tsv_128_kib() -> Vec<u8> {
    vec![b'x'; 128 * 1024]
}
```

- [ ] **Step 2: 运行 RED**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib engine::tests::p5_ -- --nocapture
```

预期：内部 paste_clipboard、PreparedGeneration 和 backing/output 编码分离尚不存在。

- [ ] **Step 3: 流式合并并只切换会话 backing**

```rust
pub struct PreparedGeneration {
    pub backing_path: PathBuf,
    pub rows: u64,
    pub cols: u32,
    pub bytes: u64,
    pub schema: Arc<Schema>,
    pub order: RowOrder,
    pub headerless: bool,
}
```

BigFile 增加 `backing_encoding` 和 `output_encoding`：初始两者均为源文件编码；内部 generation 一律 UTF-8，用户保存仍按 output_encoding；普通保存成功后两者才统一为目标编码。`path` 始终是用户身份，`backing_path` 可以是本会话 UTF-8 spool，固定点绝不提前写 path。

固定点粘贴收到确认后才创建 PreparedGeneration。先按当前逻辑字节、输入字节和索引估算调 ensure_capacity，再流式遍历当前 identity 行；目标矩形输入通过 PasteImport.row_index 定位临时文件记录并按行读取合流，内存只保留当前输出行、当前输入行、8 字节/行的 offset 索引和 64 KiB I/O 缓冲。未排序扩行在末尾追加；排序态因禁止扩行列，临时建立 orig→display 反向 rank 来选取输入记录，提交后立即释放并保持原 Perm 长度和值。`prepare_paste_generation(&self, Arc<ClipboardEntry>, &PasteDestination)` 写完并 flush 后只返回 PreparedGeneration，不改活动状态。`commit_prepared_generation(&mut self, PreparedGeneration)` 重新打开 UTF-8 CSV generation，校验编码、CSV 结构、行列数、schema 宽度和提交范围；全部成功才交换 backing/index/schema/order，随后清双栈、fixed_point=true、history_generation+1、revision checked_add(1)。savepoint_detached 按统一规则更新，原 path 和 output_encoding 不变。

失败时只删除本次 PreparedGeneration，活动 backing 不变。导入文件由 Arc/PasteImport 所有权收尾；提交前不得清 redo 或旧历史。

- [ ] **Step 4: 内部粘贴先于 readText，关签与退出清理**

Grid.pasteClipboard 开始先 `await model.clipboardMeta()`；有 token 就直接 `model.pasteClipboardToken(token,destination)`，不调用 navigator.readText。目标模式仍按当前选区决定：矩形粘贴到 current，单值 Fill 只有 metadata rows=1、cols=1 且非 forceSplit 时适用。无 token 才走 Task 13 外部文本。token 在读取后失效时显示错误，不回退粘贴一份旧 JS 文本。

实现统一清理：

```rust
fn cleanup_process_state(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<BigShared>() {
        state.lock().unwrap().clear();
    }
    if let Some(state) = app.try_state::<ClipboardStore>() {
        state.cleanup();
    }
}
```

`quit_app` 先 cleanup 再 exit；`.run(|app,event| if matches!(event,RunEvent::Exit) {cleanup})` 覆盖事件循环退出；`finish_smoke(app,report,code)` 写报告后 cleanup，再 process::exit。big_close 仍只 drop 对应会话，绝不清 ClipboardStore。清理函数可重复调用，第二次不得报错或删除新实例目录。

- [ ] **Step 5: 真机报告验证进程退出后临时目录消失**

smoke 在创建一个大复制和一个会话 spool 后，把二者 owned root 以每行 `CLEANUP_PATH=<绝对路径>` 附加到报告；close tab 断言文档 root 已消失，再故意保留进程级大剪贴板到 finish_smoke，验证直接 process::exit 前也 cleanup。报告仍保留原“合计 N 项，失败 0 项”。

verify-local.mjs 在每次 exe 退出后新增：

```javascript
const cleanupPaths = [...reportText.matchAll(/^CLEANUP_PATH=(.+)$/gm)]
  .map((m) => m[1].trim());
assert.ok(cleanupPaths.length >= 1, "冒烟未报告清理探针");
for (const path of cleanupPaths) {
  assert.equal(fs.existsSync(path), false, `退出后临时目录仍存在：${path}`);
}
```

不删除旧报告，不由脚本代替产品清理；若产品未清理，断言直接失败。

- [ ] **Step 6: 全量 GREEN 并提交**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
node scripts/verify-local.mjs
git add src-tauri/src/clipboard.rs src-tauri/src/spool.rs src-tauri/src/engine.rs src-tauri/src/lib.rs frontend/js/bigmodel.js frontend/js/grid.js frontend/js/smoke.js scripts/verify-local.mjs tests_js/bigmodel.test.mjs tests_js/grid.test.mjs
git diff --cached --check
git commit -m "feat: 完成内部大粘贴与临时文件清理" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

预期：内部粘贴不经过 JS 全文；固定点不改用户 CSV；关签、quit、RunEvent、smoke 直接退出均清理 owned 临时目录。

---

### Task 15: 共享值内存优化、完整验收、文档与发布审计

**Files:**
- Modify: `src-tauri/src/history.rs`、`src-tauri/src/engine.rs`、`src-tauri/src/schema.rs`、`src-tauri/src/order.rs`、`src-tauri/src/lib.rs`
- Test: Rust memory/accounting、Node 全量、`frontend/js/smoke.js`
- Modify: `README.md`、`TODO.md`、`task_plan.md`、`findings.md`、`progress.md`
- Modify: `.github/workflows/` 中实际存在旧测试数目的文件
- Modify local only: `progress.md` 先记录实测，再随文档提交
- No product data: 不读取、修改或提交 `price_j8888.csv`；四个既有未跟踪杂项不动

**Interfaces:**
- Consumes: 全部前序接口。
- Produces: 活动覆盖层和行块接入 Task 8 已有 `SharedText/ValueRef` 的共享形式、内存/耗时实测记录、最终测试账目；不改变已定义 IPC 字段。

- [ ] **Step 1: 写共享分配、账目稳定和内存回归 RED**

```rust
#[test]
fn p6_value_and_block_are_shared_not_deep_copied() {
    let p = tmp_csv("p6_shared_values", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    let value = "new-value".repeat(1000);
    bf.set_cell(0, 0, value, Approval::deny()).unwrap();
    let active = bf.test_active_value_ptr(RowId::Source(1), ColumnId(0));
    let historical = bf.test_history_value_ptr();
    assert_eq!(active, historical);
    let bytes = bf.history_stats().bytes;
    assert!(bf.undo());
    assert_eq!(bf.history_stats().bytes, bytes);
    assert!(bf.redo());
    assert_eq!(bf.history_stats().bytes, bytes);
    assert_eq!(bf.test_active_value_ptr(RowId::Source(1), ColumnId(0)), historical);
    fs::remove_file(&p).ok();
}

#[test]
fn p6_history_never_exceeds_budget_after_interleaved_operations() {
    let p = tmp_csv("p6_history_budget", &sort_fixture());
    let mut bf = load_ram(&p);
    bf.set_history_budget(64 * 1024).unwrap();
    for i in 0..100 {
        bf.set_cell(0, 0, format!("{i}:{}", "x".repeat(2048)), Approval::deny()).unwrap();
        if i % 7 == 0 { assert!(bf.undo()); }
        if i % 11 == 0 { assert!(bf.redo()); }
        assert!(bf.history_stats().bytes <= bf.history_stats().budget_bytes);
    }
    fs::remove_file(&p).ok();
}
```

追加 Source 重绑定、插入块和空串共享测试。`test_history_source_is_borrowed`、`test_last_insert_block_ptrs` 只在 `#[cfg(test)]` 读取既有 Op/Segments 字段；RowOrder/StableSortLevel 的类型本身不含行内容或排序键。

```rust
#[test]
fn p6_save_rebinds_source_without_changing_history_bytes() {
    let p = tmp_csv("p6_source_rebind", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_cell(0, 0, "changed".into(), Approval::deny()).unwrap();
    assert!(bf.test_history_source_is_borrowed());
    let bytes = bf.history_stats().bytes;
    assert!(bf.save(Approval::deny(), |_, _, _, _| {}).unwrap().saved);
    assert!(!bf.test_history_source_is_borrowed());
    assert_eq!(bf.history_stats().bytes, bytes);
    assert!(bf.meta().can_undo);
    fs::remove_file(&p).ok();
}

#[test]
fn p6_insert_block_and_empty_values_share_allocations() {
    let p = tmp_csv("p6_block_sharing", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    bf.paste_rows(bf.meta().rows, 0, vec![vec!["X".into(), "Y".into(), "Z".into()]],
        Approval::deny()).unwrap();
    let (active, historical) = bf.test_last_insert_block_ptrs();
    assert_ne!(active, 0);
    assert_eq!(active, historical);
    bf.set_cell(0, 0, String::new(), Approval::deny()).unwrap();
    bf.set_cell(1, 0, String::new(), Approval::deny()).unwrap();
    let first = bf.test_active_value_ptr(RowId::Source(1), ColumnId(0));
    let second = bf.test_active_value_ptr(RowId::Source(2), ColumnId(0));
    assert_eq!(first, second);
    fs::remove_file(&p).ok();
}
```

- [ ] **Step 2: 运行 RED 并审计真实克隆点**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib engine::tests::p6_ -- --nocapture
grep -n -E '\.clone\(\)|to_string\(\)|Vec<Vec<String>>' src/engine.rs src/baseline.rs
```

预期：当前覆盖层/插入块或保存 rebuild 仍有深拷贝，指针和账目稳定断言失败。grep 输出逐项分类为“执行必需单行/列级小对象/应避免的历史深拷贝”，不只删 clone 表象。

- [ ] **Step 3: 实现共享值、行块和淘汰后列回收**

```rust
pub struct RowBlock {
    rows: Vec<Vec<Arc<SharedText>>>,
}
```

Task 8 已定义 SharedText/ValueRef，本步只把活动覆盖层和行块接到同一 Arc，不重复定义类型。

同时删除 `lib.rs` 中 `mod history;` 上方为 Task 2 暂设的 `#[allow(dead_code)]` 及注释。此时所有保留接口必须已有真实消费方；完整 `cargo test` 和双构建不得出现 dead_code 警告，不得把临时豁免留到发布。

`history_text_bytes(utf8_len)` 对 Source 和未来 Text 都预留相同账目：`4 * size_of::<usize>() + utf8_len`，分别覆盖 Arc strong/weak、Box<str> 指针/长度和内容；CellChange/Vec/枚举自身另计。Box<str> 不用 String 的额外 capacity，因此 Source→Text 保存迁移只改变 own/shared 的归属，不改变总账。SharedText 进入活动覆盖层和历史时 clone Arc，不 clone 内容；空字符串用 OnceLock<Arc<SharedText>>。

Segments.blocks 改为持有 Arc<RowBlock>，InsertRows/Paste 历史与活动块共享。undo 摘除活动 Arc，redo 重新挂同一 Arc；保存 PreparedHistory 也只 clone Arc。Schema/Perm 已在 Task 6/10 共享，不另建包装层。历史淘汰后收集活动 Schema 和双栈可见的 ColumnId 集合，只删除“隐藏且不再被任何历史引用”的插入列 overlay/storage；Source 字节不因回收而修改。

`_fillSelection` 的结构化 Fill 已在 Task 13 共用一个新值 Arc；普通小粘贴继续稀疏记录。不得把范围填充改回前端 rows×cols 数组，也不得宣称排序键或 readText 全文已消除。

- [ ] **Step 4: 全量自动化与忽略基准的明确运行**

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
cargo test -p simpletable --lib
cd "D:/SVN_Cursor/simpleTable-public"
node --test tests_js/*.test.mjs
node scripts/verify-local.mjs
```

新增 `#[ignore] bench_history_large` 只读 `SIMPLETABLE_BENCH_CSV` 指向的合成大文件，执行打开、渐进索引、筛选、排序/撤销、小编辑、固定点粘贴和关签；绝不 save。每阶段输出一行 `BENCH label=... elapsed_ms=... history_bytes=... budget_bytes=... spool_bytes=... peak_working_set=...`。Windows 用 `std::process::Command` 调 `powershell -NoProfile -Command "(Get-Process -Id <当前PID>).PeakWorkingSet64"` 并解析数字，非 Windows 无可靠实现时输出 `peak_working_set=unmeasured`，不得伪造 0。

基准命令固定为：

```bash
cd "D:/SVN_Cursor/simpleTable-public/src-tauri"
SIMPLETABLE_BENCH_CSV="E:/simpletable_hdd_test/bench_22gb.csv" BENCH_CACHE_LABEL=cold \
  cargo test -p simpletable --lib engine::tests::bench_history_large --release -- --ignored --nocapture
SIMPLETABLE_BENCH_CSV="E:/simpletable_hdd_test/bench_22gb.csv" BENCH_CACHE_LABEL=hot \
  cargo test -p simpletable --lib engine::tests::bench_history_large --release -- --ignored --nocapture
```

cold 只表示本次环境首次打开并如实记录是否重启过系统，hot 表示紧随其后的第二次；不声称清空操作系统缓存。若 E 盘夹具不存在，明确记“未运行大文件基准”，其余验证继续，不得改用 price_j8888.csv。

预期：常规 Rust 只保留 bench_gen_big_csv、bench_big_filter、bench_history_large 三个明确 ignored；完整测试无失败；Node/双 exe 以实际新数量为准。

- [ ] **Step 5: 文档、CI 账目与发布审计**

README 用中文更新：64 MiB 默认与 1–1,024 MiB 设置、每页签独立、最旧淘汰、固定点提示、列/排序/表头历史、保存例外、大复制仅限本应用当前进程、不承诺跨进程或重启恢复、外部 readText 固有限制、临时文件正常清理、重启不恢复历史。写入实际 Node/Rust/debug/release 数量和基准表，不把计划数字写成结果。

同步 TODO/task_plan/findings/progress：记录本阶段真实 RED/GREEN、失败尝试、未运行项、基准 cold/hot 条件、exe 时间戳和临时清理探针。用 `grep -R -n -E '113|138|145|200 条|UNDO_CAP' README.md .github scripts tests_js src-tauri/src` 找旧账目；只修改确实对应当前测试数量的 CI/文档，不断言所有 145 都是测试数。

```bash
cd "D:/SVN_Cursor/simpleTable-public"
git diff --check
git status --short
git -c core.quotepath=false status --porcelain=v1 -z
```

预期：tracked diff 只含本设计目标文件；`smoke_debug.txt`、`src-tauri/smoke-debug.txt`、`src-tauri/smoke-release.txt`、`捕获.JPG` 仍只是未跟踪杂项，不删除、不 stage；业务 CSV 不在变更集合。前端最后变更后 verify-local 已重新编译双 exe并验证时间戳。

- [ ] **Step 6: 分两次本地提交并复核，不做远程操作**

```bash
cd "D:/SVN_Cursor/simpleTable-public"
git add src-tauri/src/history.rs src-tauri/src/engine.rs src-tauri/src/schema.rs src-tauri/src/order.rs src-tauri/src/baseline.rs src-tauri/src/lib.rs frontend/js/smoke.js
git diff --cached --check
git commit -m "perf: 共享历史值与插入块降低内存" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
git add README.md TODO.md task_plan.md findings.md progress.md .github/workflows scripts/verify-local.mjs
git diff --cached --check
git commit -m "docs: 完成统一撤销内存预算验收" -m "Co-Authored-By: Kimi <noreply@moonshot.cn>" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
git show --stat --oneline HEAD
git status --short
```

预期：两次提交均只含实际变更目标；若 `.github/workflows` 或 verify-local 无需变化，不用空路径强制提交。最终明确报告实际通过数量、三个 ignored 中哪些基准已运行、未运行项、未做远程操作。

---

## Final Self-Review Checklist for the Executor

- [ ] 每一行为都保留预期 RED 命令和实际失败输出；若测试因写错而 error，先修测试再重新亲见正确 RED。
- [ ] 每任务结束后 history_stats.bytes 不超过预算；取消/失败同时核对数据、redo、dirty、backing_path，而非只看 canUndo。
- [ ] 前端最后一次变更后必须重新运行 verify-local，不能用之前编译的 exe 代替。
- [ ] 普通保存、排序保存、删除列保存分别验证取消和确认；普通保存可转换时不得清栈。
- [ ] 外部 readText 的完整字符串、固定点 generation 的执行磁盘/内存、进程 RSS 与 64 MiB 历史账目不混为一谈。
- [ ] 所有提交都是本地提交，四个既有未跟踪杂项和业务 CSV 不进提交，不执行远程操作。
