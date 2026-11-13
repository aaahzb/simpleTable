# 原 M0 直拷与双正文 RAM 物化保存 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 首次转换始终保留原 M0 并直接复制到最终匿名 RAM；需要整理正文时以两遍序列化生成第二份 RAM，再按“RAM 提交 → 磁盘提交”完成列操作、保存、另存为与排序保存。

**Architecture:** `ram_load` 只提供统一的有界并行块复制核心，首次转换以来源借用原 M0 的只读切片，不再建立独立块视图。保存序列化收敛为同一入口的计数写出与区域写出两遍执行；所有可失败的历史、索引容量和新正文准备在 RAM 接管前完成，磁盘写失败不恢复旧正文。旧 03 流式双写快路径只保留行为夹具，不保留第二套提交模型。

**Tech Stack:** Rust 2021、memmap2、windows-sys/libc、Tauri 2、原生 JavaScript、Node.js `node:test`、真实 WebView2 smoke、Windows PhysicalDisk/typeperf。

## Global Constraints

- 工作目录固定为 `D:\SVN_Cursor\simpleTable-public\.claude\worktrees\feat+memory-mode`；不得误改主工作区。
- 保留当前工作区中有意未提交且不相关的内容，不做 reset、裸 stash 或覆盖。
- 未经用户明确要求，不创建 Git 提交、不推送、不创建远程标签或 PR；各 Task 末尾只做检查点，不执行 `git commit`。
- Git 作者与提交者只能使用用户本人身份，不添加 AI 或第三方共同作者。
- 删除文件或目录以及修改既有文件时，遵守用户要求的确认流程。
- 不使用 `VirtualLock`、`mlock`、气球内存或系统内存压力手段；不承诺 mmap 页绝对驻留或所有机器物理读取恒为零。
- 不读取、不修改、不提交真实业务文件 `price_j8888.csv`；物理验收只用授权的 `E:\simpletable_hdd_test\bench_22gb.csv` 或独立合成副本。
- 首次转换不建立独立块视图、不主动整份 `read_at`、不增加显式预热；取消和失败保留原 M0，不运行重映射恢复。
- 新正文完整生成并完成 RAM 提交后才释放旧正文；磁盘提交失败保留新 RAM 和未保存状态，不恢复旧正文。
- 执行列操作只整理 RAM，不写硬盘；保存和另存为只有目标换名成功才清除未保存状态或切换路径。
- 两遍物化必须复用同一序列化入口，禁止分别维护“估算长度”和“实际编码”两套规则。
- 普通 mmap 文档的编辑和非内存保存语义不得改变；不实现完成态 RAM 的原地变长编辑、范围替换或动态扩容。
- 私有内存增量硬门槛均为 `floor(X × 1.05)`；绝对峰值、工作集和系统缓存分别报告，不得混用口径。
- 每次产品改动后编译本地 exe；前端变更必须重新编译嵌入前端的 release exe，再运行真实界面 smoke。
- 旧预热探针只用于其明确的内存或单元测试目的，不能替代真实 release 界面物理读盘验收。
- 所有文本文件以 UTF-8 保存；代码遵循现有 Rust/JavaScript 风格并补充必要文档与关键注释。

---

## File Structure

- `src-tauri/src/ram_load.rs`：通用 `ChunkCopy`、目标区域预留和有界并行复制；新增借用原 M0 的切片来源，移除首次转换对独立块视图的依赖。
- `src-tauri/src/ram_region.rs`：连续匿名 RAM 的预留、并行写入、顺序写入和只读完成态；03 专用渐进释放机制随通用物化迁移收敛。
- `src-tauri/src/engine.rs`：转换闸门、物化计划、历史与固定点、RAM/磁盘两段提交和文件身份。
- `src-tauri/src/engine/serialize.rs`：普通保存与内存物化共用的冻结序列化视图、计数写出器、RAM 区域写出器和输出结构统计。
- `src-tauri/src/engine/serialize_tests.rs`：普通 mmap 字节兼容及 UTF-8/GBK/BOM/引号/多行/CRLF/LF/纯文本矩阵。
- `src-tauri/src/engine/materialize.rs`：历史、固定点和索引容量准备，两遍 RAM 物化及只做状态移动的 RAM 提交。
- `src-tauri/src/engine/materialize_tests.rs`：编码、格式、故障、长度一致性和通用 03 行为迁移测试。
- `src-tauri/src/engine/insert_save.rs`、`insert_save_tests.rs`：旧 03 快路径及其测试；实现迁移后按确认流程删除或收敛，不保留第二套保存语义。
- `src-tauri/src/diagnostics.rs`、`src-tauri/src/bin/memory_peak.rs`：真实生产入口的阶段事件、故障场景和 105% 私有内存硬断言。
- `src-tauri/src/lib.rs`：保存与物化进度事件的现有 IPC 转发，不引入第二套事件通道。
- `frontend/js/bigmodel.js`、`app.js`、`dialogs.js`：区分计算正文、内存整理、写入磁盘与完成状态。
- `frontend/js/smoke.js`：真实 WebView2 的转换、列操作、保存、失败与进度语义回归。
- `scripts/verify-local.mjs`：canonical debug/release 双构建、前端嵌入时间戳与双 smoke 验证。
- `README.md`、`docs/guide.md`、`docs/development.md`、`TODO.md`：当前行为、验证证据、平台边界和任务状态。
- `scripts/probe-ui-physical-reads.mjs`、`tests_js/ui_physical_read_probe.test.mjs`：完整 release 界面的基线/转换窗口采样、报告生成与解析测试；只清理脚本自己启动的进程。
- `.github/workflows/ci.yml`、`.github/workflows/release.yml`：把新物化/保存/重试场景加入现有真实私有内存硬门槛，不改变签名和发布触发规则。

## Baseline Captured on 2026-11-13

- `cargo test --manifest-path .claude/worktrees/feat+memory-mode/src-tauri/Cargo.toml --lib ram_load::tests`：10 passed，0 failed；另有 2 个既有 dead-code warning。
- `node --test tests_js/physical_read_probe.test.mjs tests_js/app-state.test.mjs tests_js/bigmodel.test.mjs`：合计 122 passed，0 failed。
- `cargo test --manifest-path .claude/worktrees/feat+memory-mode/src-tauri/Cargo.toml --lib`：364 passed，0 failed，3 ignored；另有 2 个既有 dead-code warning。
- `node --test tests_js/*.test.mjs`：263 passed，0 failed。
- 上述是当前旧实现的基线，不是新规格验收结果；最终数量以实施后的 fresh 全量输出为准。

---

### Task 1: 增加借用原 M0 的复制来源

**Files:**
- Modify: `src-tauri/src/ram_load.rs:102-127`
- Test: `src-tauri/src/ram_load.rs:445-774`

**Interfaces:**
- Consumes: `ChunkCopy::{len, copy_chunk}`、`load_file`、`LoadOptions`、`LoadResult`。
- Produces: `pub(crate) struct MmapSource<'a>` 与 `pub(crate) fn MmapSource::new(mmap: &'a memmap2::Mmap) -> Self`；不持有 `File`、不建立新映射、不使用裸指针。

- [x] **Step 1: 先写 M0 切片来源的失败测试**

在 `ram_load::tests` 的导入中加入 `MmapSource`，并加入下列测试。测试继续使用现有 `temp_pattern_file`，不新增文件读取路径：

```rust
#[test]
fn mmap_source_copies_single_multi_and_tail_chunks() {
    let (file, _cleanup) = temp_pattern_file("mmap_source_ranges", 70_000);
    let mmap = unsafe { memmap2::Mmap::map(&file).unwrap() };
    let source = MmapSource::new(&mmap);
    assert_eq!(source.len().unwrap(), 70_000);

    for (start, count) in [(0usize, 1000usize), (999, 1001), (69_999, 1)] {
        let mut out = vec![0u8; count];
        source.copy_chunk(start as u64, &mut out).unwrap();
        assert!(out.iter().enumerate().all(|(index, byte)| {
            *byte == ((start + index) % 251) as u8
        }));
    }
}

#[test]
fn mmap_source_parallel_copy_matches_original_bytes() {
    let (file, _cleanup) = temp_pattern_file("mmap_source_parallel", 3 * 1024 * 1024 + 17);
    let mmap = unsafe { memmap2::Mmap::map(&file).unwrap() };
    let cancel = AtomicBool::new(false);
    let result = load_file(
        MmapSource::new(&mmap),
        &cancel,
        LoadOptions {
            chunk_bytes: 64 * 1024,
            workers: 4,
            fault: LoadFault::None,
        },
        |_| {},
    )
    .unwrap();
    let LoadResult::Complete(ram) = result else {
        panic!("unexpected cancel")
    };
    assert_eq!(ram.as_slice(), mmap.as_ref());
}

#[test]
fn mmap_source_rejects_out_of_bounds_copy() {
    let (file, _cleanup) = temp_pattern_file("mmap_source_bounds", 257);
    let mmap = unsafe { memmap2::Mmap::map(&file).unwrap() };
    let source = MmapSource::new(&mmap);
    let mut out = [0u8; 2];
    let error = source.copy_chunk(256, &mut out).unwrap_err();
    assert!(error.contains("超出"), "{error}");
    assert_eq!(out, [0, 0], "越界时不得写入部分目标块");
}
```

- [x] **Step 2: 运行测试并确认 RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib ram_load::tests::mmap_source
```

Expected: 编译失败，提示 `MmapSource` 未定义或无法导入；不得通过先注释测试取得 GREEN。

- [x] **Step 3: 实现最小的只读借用来源**

在 `ChunkCopy` 的 `PositionedRead` blanket impl 后加入：

```rust
/// 借用首次转换前的完整 M0；调用方保证整个复制期间原映射保持有效。
pub(crate) struct MmapSource<'a> {
    mmap: &'a memmap2::Mmap,
}

impl<'a> MmapSource<'a> {
    pub(crate) fn new(mmap: &'a memmap2::Mmap) -> Self {
        Self { mmap }
    }
}

impl ChunkCopy for MmapSource<'_> {
    fn len(&self) -> Result<u64, String> {
        Ok(self.mmap.len() as u64)
    }

    fn copy_chunk(&self, offset: u64, out: &mut [u8]) -> Result<(), String> {
        let start = usize::try_from(offset).map_err(|_| "M0 复制偏移溢出")?;
        let end = start.checked_add(out.len()).ok_or("M0 复制长度溢出")?;
        let source = self
            .mmap
            .get(start..end)
            .ok_or_else(|| format!("M0 复制范围超出源长度：{start}..{end}"))?;
        out.copy_from_slice(source);
        Ok(())
    }
}
```

`memmap2::Mmap` 的只读引用可在线程间共享，现有 scoped worker 负责借用生命周期；这里不添加 `Arc` 克隆或第二份 `Vec<u8>`。

- [x] **Step 4: 运行新测试和既有文件适配器回归**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib ram_load::tests::mmap_source
cargo test --manifest-path src-tauri/Cargo.toml --lib ram_load::tests::load_reads_real_file_with_bounded_parallelism_and_monotonic_progress
```

Expected: 两组均 PASS；真实 `File` 的短读和 `PositionedRead` 路径仍保留。

- [x] **Step 5: 编译检查点，不提交**

Run:

```bash
cargo build --manifest-path src-tauri/Cargo.toml
git diff --check -- src-tauri/src/ram_load.rs
```

Expected: debug exe 编译成功，`git diff --check` 无 whitespace error。此 Task 暂不删除 `MappedChunks`，因为引擎尚未迁移；不执行 Git 提交。

---

### Task 2: 首次转换保留原 M0，取消与失败不再重映射

**Files:**
- Modify: `src-tauri/src/engine.rs:34-37`、`:5503-5531`、`:5565-5890`、`:9837-9938`
- Modify: `src-tauri/src/ram_load.rs:49-61`、`:129-313`、`:358-429`、`:651-774`
- Test: `src-tauri/src/engine.rs:5565-5890`

**Interfaces:**
- Consumes: Task 1 的 `MmapSource::new(&memmap2::Mmap)`，以及未改签名的 `prepare_load` / `run_prepared_load`。
- Produces: `convert_mmap_to_ram` 保持原签名和 `ConvertStatus`；成功时只在安装 `Backing::Ram` 处释放 M0，失败时 `self.backing` 始终持有原 `Arc<Mmap>`。

- [x] **Step 1: 把旧块视图/恢复测试改成原映射身份与生命周期测试**

将 `backing_conversion_prepares_all_views_and_target_before_releasing_mmap` 替换为：

```rust
#[test]
fn backing_conversion_borrows_original_mmap_until_successful_install() {
    let p = tmp_csv("convert_m0_lifetime", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    let before = conversion_snapshot(&bf);
    let weak = match &bf.backing {
        Backing::Mmap(m) => Arc::downgrade(m),
        _ => panic!("预期 mmap"),
    };
    let observations = std::sync::Mutex::new(Vec::new());
    let cancel = AtomicBool::new(false);
    let status = bf
        .convert_mmap_to_ram(
            &cancel,
            LoadOptions {
                chunk_bytes: 16,
                workers: 2,
                fault: LoadFault::None,
            },
            |event| observations.lock().unwrap().push((event, weak.upgrade().is_some())),
        )
        .unwrap();
    assert_eq!(status, ConvertStatus::Converted);
    let events = observations.into_inner().unwrap();
    assert!(events.iter().any(|(event, _)| matches!(event, LoadEvent::Reserved { .. })));
    assert!(events.iter().any(|(event, _)| matches!(event, LoadEvent::ChunkBegin { .. })));
    assert!(events.iter().any(|(event, _)| matches!(event, LoadEvent::Finished { .. })));
    assert!(events.iter().all(|(_, old_live)| *old_live), "{events:?}");
    assert!(weak.upgrade().is_none(), "安装 RAM 后不得遗留其他 M0 Arc");
    assert_eq!(conversion_snapshot(&bf), before);
    fs::remove_file(&p).ok();
}
```

在测试模块加入强引用身份辅助函数，并把预留、提交、复制、取消和外部变化测试都改为 `Arc::ptr_eq` 断言：

```rust
fn original_mmap(bf: &BigFile) -> Arc<memmap2::Mmap> {
    match &bf.backing {
        Backing::Mmap(m) => Arc::clone(m),
        _ => panic!("预期原始 mmap"),
    }
}

fn assert_same_original_mmap(bf: &BigFile, original: &Arc<memmap2::Mmap>) {
    match &bf.backing {
        Backing::Mmap(current) => assert!(Arc::ptr_eq(original, current)),
        _ => panic!("失败后必须保留原 mmap"),
    }
}

#[test]
fn backing_conversion_commit_and_read_faults_keep_original_mmap_without_remap() {
    for (name, fault, expected) in [
        ("commit", LoadFault::CommitAt(32), "提交"),
        ("read", LoadFault::ReadAfter(32), "读取"),
    ] {
        let p = tmp_csv(&format!("convert_{name}_keeps_m0"), &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
        bf.run_filter(&spec_one(0, "Alice")).unwrap();
        let before = conversion_snapshot(&bf);
        let original = original_mmap(&bf);
        let error = convert_for_test(&mut bf, fault).unwrap_err();
        assert!(error.contains(expected), "{name}: {error}");
        assert_same_original_mmap(&bf, &original);
        assert_eq!(conversion_snapshot(&bf), before);
        fs::remove_file(&p).ok();
    }
}

#[test]
fn backing_conversion_cancel_after_progress_keeps_original_mmap_without_finish() {
    let p = tmp_csv("convert_partial_cancel_keeps_m0", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
    let before = conversion_snapshot(&bf);
    let original = original_mmap(&bf);
    let cancel = AtomicBool::new(false);
    let finished = AtomicBool::new(false);
    let status = bf
        .convert_mmap_to_ram(
            &cancel,
            LoadOptions { chunk_bytes: 16, workers: 2, fault: LoadFault::None },
            |event| match event {
                LoadEvent::Progress { done, .. } if done > 0 => {
                    cancel.store(true, Ordering::Relaxed);
                }
                LoadEvent::Finished { .. } => finished.store(true, Ordering::Relaxed),
                _ => {}
            },
        )
        .unwrap();
    assert_eq!(status, ConvertStatus::Cancelled);
    assert!(!finished.load(Ordering::Relaxed));
    assert_same_original_mmap(&bf, &original);
    assert_eq!(conversion_snapshot(&bf), before);
    fs::remove_file(&p).ok();
}

#[test]
fn backing_conversion_repeated_cancel_keeps_same_original_mmap() {
    let p = tmp_csv("convert_repeated_cancel_same_m0", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    let before = conversion_snapshot(&bf);
    let original = original_mmap(&bf);
    for _ in 0..3 {
        let cancel = AtomicBool::new(false);
        let status = bf
            .convert_mmap_to_ram(
                &cancel,
                LoadOptions { chunk_bytes: 16, workers: 2, fault: LoadFault::None },
                |event| {
                    if matches!(event, LoadEvent::Progress { done, .. } if done > 0) {
                        cancel.store(true, Ordering::Relaxed);
                    }
                },
            )
            .unwrap();
        assert_eq!(status, ConvertStatus::Cancelled);
        assert_same_original_mmap(&bf, &original);
        assert_eq!(conversion_snapshot(&bf), before);
    }
    fs::remove_file(&p).ok();
}

#[test]
fn backing_conversion_cancel_after_finished_does_not_roll_back() {
    let p = tmp_csv("convert_finished_no_rollback", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    let weak = match &bf.backing {
        Backing::Mmap(m) => Arc::downgrade(m),
        _ => panic!("预期 mmap"),
    };
    let cancel = AtomicBool::new(false);
    let status = bf
        .convert_mmap_to_ram(
            &cancel,
            LoadOptions { chunk_bytes: 16, workers: 2, fault: LoadFault::None },
            |event| {
                if matches!(event, LoadEvent::Finished { .. }) {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        )
        .unwrap();
    assert_eq!(status, ConvertStatus::Converted);
    assert!(bf.is_ram_backed());
    assert!(weak.upgrade().is_none());
    fs::remove_file(&p).ok();
}
```

预留失败测试保留原名，但使用 `let original = original_mmap(&bf);` 和 `assert_same_original_mmap`，observer 不再等待 `SourcePrepared`。外部变化测试改名为 `backing_conversion_file_stamp_change_keeps_original_mmap_without_reloading_edits`，保留只修改 mtime 的注入方式，并增加同一组身份断言。删除只服务旧恢复模型的 `backing_conversion_restore_fault_reports_both_errors`；不得在复制期间真实截仍被映射的文件。

- [x] **Step 2: 运行转换测试并确认 RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib backing_conversion_borrows_original_mmap_until_successful_install
cargo test --manifest-path src-tauri/Cargo.toml --lib backing_conversion_commit_and_read_faults_keep_original_mmap_without_remap
cargo test --manifest-path src-tauri/Cargo.toml --lib backing_conversion_cancel_after_finished_does_not_roll_back
```

Expected: 前两条分别因复制期间 M0 已释放、失败后 `Arc` 身份不同而 FAIL；第三条应 PASS，用来固定既有“最终复制进度之后不因同一取消标志回滚”的边界。`Finished` 只表示目标复制完成，不表示外部文件状态检查或文档安装已经成功。

- [x] **Step 3: 用借用 M0 的最小引擎实现替换准备、空后备和恢复流程**

导入改为：

```rust
use crate::ram_load::{
    conversion_limit_bytes, prepare_load, run_prepared_load, LoadEvent, LoadOptions, LoadResult,
    MmapSource,
};
```

`convert_mmap_to_ram` 在 `join_index_worker()?` 之后使用以下完整控制流：

```rust
let before = FileStamp::read(&self.backing_path)?;
let Backing::Mmap(m0) = &self.backing else {
    return Ok(ConvertStatus::Cancelled);
};
#[cfg(test)]
if self::tests::conversion_reserve_fault() {
    return Err("注入预留失败".into());
}
let prepared = prepare_load(MmapSource::new(m0), options, &observer)?;

match run_prepared_load(prepared, cancel, &observer) {
    Ok(LoadResult::Complete(ram)) => match FileStamp::read(&self.backing_path) {
        Ok(after) if after == before => {
            // PreparedLoad 已消费并结束借用；此处是唯一正常释放 M0 的位置。
            self.backing = Backing::Ram(RamBuffer::Region(ram));
            Ok(ConvertStatus::Converted)
        }
        Ok(_) => Err("EXTERNAL_CHANGED: 转换期间后备文件发生变化".into()),
        Err(error) => Err(error),
    },
    Ok(LoadResult::Cancelled) => match FileStamp::read(&self.backing_path) {
        Ok(after) if after == before => Ok(ConvertStatus::Cancelled),
        Ok(_) => Err("EXTERNAL_CHANGED: 取消时发现后备文件发生变化".into()),
        Err(error) => Err(error),
    },
    Err(error) => Err(error),
}
```

删除 `restore_after_conversion`。`map_current_backing` 仍供内存模式退出等路径使用，但删除其中的 `conversion_restore_fault` 分支；测试故障设施收敛为：

```rust
thread_local! {
    static CONVERT_RESERVE_FAULT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(super) fn conversion_reserve_fault() -> bool {
    CONVERT_RESERVE_FAULT.with(std::cell::Cell::get)
}

struct ConversionFaultGuard;

impl Drop for ConversionFaultGuard {
    fn drop(&mut self) {
        CONVERT_RESERVE_FAULT.with(|flag| flag.set(false));
    }
}

fn conversion_fault_guard(reserve: bool) -> ConversionFaultGuard {
    CONVERT_RESERVE_FAULT.with(|flag| flag.set(reserve));
    ConversionFaultGuard
}
```

- [x] **Step 4: 收敛旧独立块视图实现**

从 `ram_load.rs` 删除 `MappedViewStats`、`ViewCounters`、`MappedView`、`MappedChunks`、`mapping_granularity` 及其专属测试，保留真实 `File` adapter 和通用取消/故障测试。`LoadEvent` 定义为：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadEvent {
    ReserveBegin { total: u64 },
    Reserved { total: u64 },
    WorkerBegin { worker: usize },
    WorkerEnd { worker: usize },
    ChunkBegin { offset: u64, len: usize },
    ChunkReady { offset: u64, len: usize },
    Progress { done: u64, total: u64 },
    Finished { total: u64 },
}
```

把 `run_prepared_load` 的旧注释改为：

```rust
/// 第二阶段从 PreparedLoad 持有的来源直接复制到最终区域。
/// 对 MmapSource，原 M0 由调用方保留到本函数返回并完成外部状态检查之后。
```

同步把排序估算附近“预建块视图”的注释改为“复制期间保留完整 M0；工作集可能短时重叠，但不锁页”。

- [x] **Step 5: 运行转换与生命周期回归**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib ram_load::tests
cargo test --manifest-path src-tauri/Cargo.toml --lib backing_conversion
cargo test --manifest-path src-tauri/Cargo.toml --lib memory_mode_cancel_keeps_mmap_and_undo_policy
cargo test --manifest-path src-tauri/Cargo.toml --lib progressive_conversion_joins_index_worker_and_releases_its_mmap_arc
```

Expected: 全部 PASS；取消无 `Finished`/伪造 100%，成功和渐进索引完成后无残余 M0 强引用，失败测试均为同一原始 `Arc<Mmap>`。

- [x] **Step 6: 全量 Rust 回归与本地 exe 检查点**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib
cargo build --manifest-path src-tauri/Cargo.toml
cargo build --release --manifest-path src-tauri/Cargo.toml --bin simpletable
cargo build --release --manifest-path src-tauri/Cargo.toml --bin memory_peak
git diff --check -- src-tauri/src/ram_load.rs src-tauri/src/engine.rs
```

Expected: Rust 无新增失败，debug/release exe 和诊断 exe 均编译成功。记录实际测试数和两个既有 dead-code warning 是否仍存在；不执行 Git 提交。

---

### Task 3: 提取唯一文档序列化入口和两类写出器

**Files:**
- Create: `src-tauri/src/engine/serialize.rs`
- Create: `src-tauri/src/engine/serialize_tests.rs`
- Modify: `src-tauri/src/engine.rs`（模块声明、`:10159-10242`、`:14108-14307`、`:14782-14852`）

**Interfaces:**
- Consumes: 既有 `src_span`、`src_fields`、`ins_fields`、`encode_document_record`、`RowOrder`、`RamRegion::append/finish`。
- Produces:

```rust
pub(super) trait DocumentSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), String>;
}

pub(super) struct SerializationView<'a> {
    pub document: &'a BigFile,
    pub schema: &'a Schema,
    pub saved_headers: &'a [String],
    pub sorted: Option<&'a Perm>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SerializationStats {
    pub bytes: u64,
    pub line_capacity: usize,
    pub record_capacity: usize,
    pub rows: u64,
}

pub(super) fn serialize_document<S: DocumentSink>(
    view: SerializationView<'_>,
    sink: &mut S,
    on_rows: impl FnMut(u64, u64),
) -> Result<SerializationStats, String>;

pub(super) struct CountingSink { bytes: usize }
pub(super) struct IoSink<'a, W: std::io::Write> { pub inner: &'a mut W }
pub(super) struct RamRegionSink { region: Option<RamRegion>, written: usize }
```

- [x] **Step 1: 写共享入口和格式矩阵的 RED 测试**

在 `serialize_tests.rs` 建立自有临时文件助手，然后加入以下数据驱动测试。`clean` 文档的输出必须与输入逐字节一致；候选 Schema 测试同时比较真实 RAM 字节：

```rust
#[test]
fn shared_serializer_clean_format_matrix_is_byte_exact() {
    let gbk = encoding_rs::GBK
        .encode("name,note\r\nAlice,北京\r\n")
        .0
        .into_owned();
    let cases: Vec<(&str, &str, Vec<u8>)> = vec![
        ("utf8-crlf", "csv", b"name,note\r\nAlice,x\r\n".to_vec()),
        ("utf8-bom", "csv", b"\xEF\xBB\xBFname,note\nAlice,x\n".to_vec()),
        ("quotes-multiline", "csv", b"name,note\r\nAlice,\"a,b\nline2\"\r\n".to_vec()),
        ("no-final-newline", "csv", b"name,age\r\nAlice,30".to_vec()),
        ("lf", "csv", b"name,age\nAlice,30\n".to_vec()),
        ("gbk", "csv", gbk),
        ("text-lines", "txt", "北京\n\nAlice\n".as_bytes().to_vec()),
    ];
    for (name, extension, expected) in cases {
        let bf = open_fixture(name, extension, &expected);
        let headers = bf.view_headers();
        let mut counting = CountingSink::new();
        let counted = serialize_document(
            SerializationView {
                document: &bf,
                schema: &bf.schema,
                saved_headers: &headers,
                sorted: None,
            },
            &mut counting,
            |_, _| {},
        ).unwrap();
        assert_eq!(counted.bytes, expected.len() as u64, "{name}");

        let mut ram_sink = RamRegionSink::new(RamRegion::reserve(expected.len()).unwrap());
        let written = serialize_document(
            SerializationView {
                document: &bf,
                schema: &bf.schema,
                saved_headers: &headers,
                sorted: None,
            },
            &mut ram_sink,
            |_, _| {},
        ).unwrap();
        let ram = ram_sink.finish().unwrap();
        assert_eq!(written, counted, "{name}");
        assert_eq!(ram.as_slice(), expected.as_slice(), "{name}");
    }
}

#[test]
fn shared_serializer_reencodes_edits_with_candidate_schema() {
    let p = fixture_path("candidate-schema", "csv", b"name,age\r\nAlice,30\r\n");
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_cell(0, 1, "北 京,8".into(), Approval::deny()).unwrap();
    bf.push_col_op(ColOp::Insert { pos: 1, name: "empty".into() }).unwrap();
    let candidate = bf.pending_schema.clone().unwrap();
    let headers = candidate.visible().iter().map(|id| {
        candidate.resolve(*id).unwrap().header.clone()
    }).collect::<Vec<_>>();
    let mut counting = CountingSink::new();
    serialize_document(
        SerializationView {
            document: &bf,
            schema: &candidate,
            saved_headers: &headers,
            sorted: None,
        },
        &mut counting,
        |_, _| {},
    ).unwrap();

    let mut ram_sink = RamRegionSink::new(RamRegion::reserve(counting.bytes()).unwrap());
    serialize_document(
        SerializationView {
            document: &bf,
            schema: &candidate,
            saved_headers: &headers,
            sorted: None,
        },
        &mut ram_sink,
        |_, _| {},
    ).unwrap();
    let ram = ram_sink.finish().unwrap();
    assert_eq!(ram.as_slice(), "name,empty,age\r\nAlice,,\"北 京,8\"\r\n".as_bytes());
}
```

再加入以下真实排序测试。原末行无换行的 `a,1` 被排到中间后必须补一个 CRLF，原 `b,2` 的 CRLF 保留；计数遍与 RAM 遍必须完全一致：

```rust
#[test]
fn shared_serializer_sorted_missing_newline_adds_record_separator() {
    let p = fixture_path("sorted-missing-newline", "csv", b"id,v\r\nb,2\r\na,1");
    let bf = BigFile::open(&p).unwrap();
    let headers = bf.view_headers();
    let sorted = Perm::new(vec![1, 0]).unwrap();
    let expected = b"id,v\r\na,1\r\nb,2\r\n";
    let mut counting = CountingSink::new();
    let counted = serialize_document(
        SerializationView {
            document: &bf,
            schema: &bf.schema,
            saved_headers: &headers,
            sorted: Some(&sorted),
        },
        &mut counting,
        |_, _| {},
    ).unwrap();
    assert_eq!(counted.bytes, expected.len() as u64);

    let mut ram_sink = RamRegionSink::new(RamRegion::reserve(expected.len()).unwrap());
    let written = serialize_document(
        SerializationView {
            document: &bf,
            schema: &bf.schema,
            saved_headers: &headers,
            sorted: Some(&sorted),
        },
        &mut ram_sink,
        |_, _| {},
    ).unwrap();
    assert_eq!(written, counted);
    assert_eq!(ram_sink.finish().unwrap().as_slice(), expected);
}
```

RED 阶段这些测试因模块和类型不存在而编译失败。

- [x] **Step 2: 运行 RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib engine::serialize_tests
```

Expected: 编译失败，提示 `serialize` / `CountingSink` / `SerializationView` 不存在。

- [x] **Step 3: 实现写出器和冻结视图**

`CountingSink`、`IoSink` 与 RAM 写出器的核心代码如下；`RamRegionSink::finish` 同时检查实际长度和 `RamRegion::finish`：

```rust
impl CountingSink {
    pub(super) fn new() -> Self { Self { bytes: 0 } }
    pub(super) fn bytes(&self) -> usize { self.bytes }
}

impl DocumentSink for CountingSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.bytes = self.bytes.checked_add(bytes.len()).ok_or("正文长度溢出")?;
        Ok(())
    }
}

impl<W: std::io::Write> DocumentSink for IoSink<'_, W> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.inner.write_all(bytes).map_err(|error| error.to_string())
    }
}

impl RamRegionSink {
    pub(super) fn new(region: RamRegion) -> Self {
        Self { region: Some(region), written: 0 }
    }

    pub(super) fn finish(mut self) -> Result<FinishedRam, String> {
        let region = self.region.take().ok_or("RAM 写出器已完成")?;
        if self.written != region.len() {
            return Err(format!(
                "第二遍正文长度不一致：预期 {}，实际 {}",
                region.len(), self.written,
            ));
        }
        region.finish()
    }
}

impl DocumentSink for RamRegionSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.written = self.written.checked_add(bytes.len()).ok_or("正文长度溢出")?;
        self.region.as_mut().ok_or("RAM 写出器已完成")?.append(bytes)
    }
}
```

`serialize_document` 把现有 `save_to_observed` 的 BOM、表头、排序遍历、非排序 raw-run、插入行和末行换行逻辑原样移入；所有写出统一走 `DocumentSink`，所有字段映射统一使用 `SerializationView.schema`。新增 `src_fields_with_schema(rec, schema)` / `ins_fields_with_schema(block, row, schema)`，旧 `src_fields` / `ins_fields` 仅以 `self.schema` 委托，避免其他编辑路径改变。

序列化入口内部用同一 `IndexCounter` 观察实际写出的每段字节，得到 `SerializationStats`。CSV 计数规则与现有顺序索引一致：非引号内换行产生物理行，空白物理行不成为 CSV 记录；TextLines 的空白行也是记录。计数遍和 RAM 遍都执行这个入口，不能实现一个只估算字节的简化版本。

原始字节透传增加显式条件 `document.output_encoding == document.backing_encoding`；不一致时必须走 `encode_document_record`，不得把旧编码字节复制到新编码输出。

- [x] **Step 4: 普通 mmap 保存接入共享入口，保持旧磁盘提交不变**

本 Task 只替换 `save_to_observed` 中 BOM/表头/排序/非排序写出循环：

```rust
let mut io = IoSink { inner: &mut w };
let stats = serialize_document(
    SerializationView {
        document: self,
        schema: self.schema.as_ref(),
        saved_headers: &saved_headers,
        sorted: sorted_order.as_deref(),
    },
    &mut io,
    |done, total| {
        if done & 0xF == 0 && progress_gate(&mut last_emit, PROGRESS_MIN_GAP) {
            progress(stream_weighted(done), total, PHASE_STREAM, done);
        }
    },
)?;
done = stats.rows;
```

普通 mmap 仍在原位置执行 `prepare_reopen → replace_with_retry → commit_reopen`；持久内存的旧提交桥接暂时保留到 Task 5，03 分支暂时保留到 Task 7。不得在本 Task 顺手改变文件身份或失败恢复。

- [x] **Step 5: GREEN 与普通保存兼容回归**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib engine::serialize_tests
cargo test --manifest-path src-tauri/Cargo.toml --lib big_save
cargo test --manifest-path src-tauri/Cargo.toml --lib save_sorted
cargo test --manifest-path src-tauri/Cargo.toml --lib text_lines
cargo test --manifest-path src-tauri/Cargo.toml --lib memory_save
cargo test --manifest-path src-tauri/Cargo.toml --lib insert_save
```

Expected: 新测试 PASS，普通 mmap 的现有保存字节、GBK、TextLines、排序、撤销重做和旧内存桥接回归均 PASS；此阶段旧恢复测试仍按旧语义运行，不提前改断言。

- [x] **Step 6: 编译检查点，不提交**

Run:

```bash
cargo build --manifest-path src-tauri/Cargo.toml
cargo build --release --manifest-path src-tauri/Cargo.toml --bin simpletable
git diff --check -- src-tauri/src/engine.rs src-tauri/src/engine/serialize.rs src-tauri/src/engine/serialize_tests.rs
```

Expected: debug/release exe 成功，whitespace 检查无错误；不执行 Git 提交。

---

### Task 4: 两遍 RAM 物化、历史准备与索引容量复用

**Files:**
- Create: `src-tauri/src/engine/materialize.rs`
- Create: `src-tauri/src/engine/materialize_tests.rs`
- Modify: `src-tauri/src/engine.rs:535-690`、`:7270-7327`、`:7848-7867`、`:9015-9086`
- Modify: `src-tauri/src/ram_region.rs:49-258`

**Interfaces:**
- Consumes: Task 3 的 `SerializationView`、`SerializationStats`、`CountingSink`、`RamRegionSink`、`serialize_document`。
- Produces:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RamContentKey {
    pub history_generation: u64,
    pub schema: Arc<Schema>,
    pub order: RowOrder,
    pub output_encoding: Encoding,
    pub format: DocumentFormat,
    pub headerless: bool,
    pub has_bom: bool,
}

pub(super) enum PrepareMaterialization {
    Ready(PreparedMaterialization),
    FixedPoint(FixedPointRequest),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MaterializeFault {
    None,
    CountAfter(u64),
    CommitAt(u64),
    Reserve,
}

enum MaterializationOperation {
    SaveCurrent,
    Columns { before: Arc<Schema>, after: Arc<Schema> },
}

#[derive(Clone)]
struct FrozenSerialization {
    schema: Arc<Schema>,
    saved_headers: Vec<String>,
    sorted: Option<Arc<Perm>>,
}

impl FrozenSerialization {
    fn view<'b>(&'b self, document: &'b BigFile) -> SerializationView<'b>;
}

pub(super) struct MaterializationInput<'a> {
    output: FrozenSerialization,
    approval: Approval,
    operation: MaterializationOperation,
    progress: Option<&'a mut dyn FnMut(u64, u64, u8, u64)>,
    fault: MaterializeFault,
}

pub(super) enum HistoryPreparation {
    Ready(PreparedRamHistory),
    FixedPoint(FixedPointRequest),
}

pub(super) struct PreparedMaterialization {
    pub body: FinishedRam,
    pub stats: SerializationStats,
    pub index: PreparedIndexReuse,
    pub history: PreparedRamHistory,
    pub schema: Arc<Schema>,
    pub base_headers: Vec<String>,
    pub segments: Segments,
    pub order: RowOrder,
    pub next_history_generation: u64,
    pub next_index_epoch: u64,
    pub fixed_point: bool,
    pub content_key: RamContentKey,
}

pub(super) fn prepare_materialization<O>(
    document: &mut BigFile,
    mut input: MaterializationInput<'_>,
    observer: &O,
) -> Result<PrepareMaterialization, String>
where O: Fn(SaveRamEvent) + Sync;

pub(super) fn commit_ram<O>(
    document: &mut BigFile,
    prepared: PreparedMaterialization,
    observer: &O,
) where O: Fn(SaveRamEvent) + Sync;
```

`MaterializationInput` 的构造器和内部方法统一为：

```rust
impl<'a> MaterializationInput<'a> {
    fn save_current(document: &BigFile, approval: Approval) -> Result<Self, String>;
    fn columns(document: &BigFile, approval: Approval) -> Result<Self, String>;
    fn with_progress(
        mut self,
        progress: &'a mut dyn FnMut(u64, u64, u8, u64),
    ) -> Self;
    #[cfg(test)]
    fn with_fault(mut self, fault: MaterializeFault) -> Self;
    fn output(&self) -> FrozenSerialization;
    fn prepare_history(
        &self,
        document: &BigFile,
        new_bytes: u64,
    ) -> Result<HistoryPreparation, String>;
    fn count_progress(&mut self) -> impl FnMut(u64, u64) + '_;
    fn ram_progress(&mut self) -> impl FnMut(u64, u64) + '_;
    fn enable_fault_at(&self) -> Option<usize>;
}
```

`save_current` 从当前 `schema/order/output_encoding` 冻结输入；`columns` 只读取 `pending_schema/pending_cols`，不提前调用 `apply` 或改写 live Schema。`CountAfter(n)` 在第一遍序列化完成且实际完成记录数已达到 n 时、发送 `CountFinished` 前返回“计数故障”，因此小夹具使用 `CountAfter(1)` 必定触发；`CommitAt(n)` 转交 `RamRegion` 的第二遍启用故障，`Reserve` 在第二份区域预留前返回“预留失败”。

- [x] **Step 1: 写两遍一致性、容量顺序和故障原子性 RED 测试**

`materialize_tests.rs` 使用自己的临时文件助手和以下持久内存夹具，不依赖真实业务文件：

```rust
fn edited_memory_document(name: &str) -> (PathBuf, BigFile) {
    let p = fixture_path(
        name,
        "csv",
        b"name,age,city\r\nAlice,30,NY\r\nBob,40,SF\r\n",
    );
    let mut bf = BigFile::open(&p).unwrap();
    assert!(bf.enter_memory_mode_for_test(&AtomicBool::new(false), |_| {}).unwrap());
    bf.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
    (p, bf)
}

#[derive(Debug, PartialEq)]
struct MaterializationSnapshot {
    rows: Vec<Vec<String>>,
    headers: Vec<String>,
    history: HistoryStats,
    revision: i64,
    generation: u64,
    epoch: u64,
    path: PathBuf,
    backing_path: PathBuf,
    base_size: u64,
    base_mtime: std::time::SystemTime,
    filter: Option<FilterSpec>,
    pending_cols: Vec<ColOp>,
}

impl BigFile {
    fn index_capacities(&self) -> (usize, usize) {
        (self.idx.line_off.capacity(), self.idx.rec_lines.capacity())
    }

    fn materialization_snapshot(&self) -> MaterializationSnapshot {
        MaterializationSnapshot {
            rows: self.rows(0, self.seg.vrow_count() as usize),
            headers: self.view_headers(),
            history: self.history_stats(),
            revision: self.revision,
            generation: self.history_generation,
            epoch: self.index_epoch,
            path: self.path.clone(),
            backing_path: self.backing_path.clone(),
            base_size: self.base_size,
            base_mtime: self.base_mtime,
            filter: self.last_spec.clone(),
            pending_cols: self.pending_cols.clone(),
        }
    }
}

fn position_of(
    events: &[SaveRamEvent],
    predicate: impl Fn(&SaveRamEvent) -> bool,
) -> usize {
    events.iter().position(predicate).unwrap()
}
```

加入两遍与容量顺序测试：

```rust
#[test]
fn materialization_two_passes_match_and_reserve_after_history_and_index() {
    let (_p, mut bf) = edited_memory_document("materialize-two-pass");
    let original = bf.backing_identity();
    let old_capacities = bf.index_capacities();
    let events = Mutex::new(Vec::new());
    let input = MaterializationInput::save_current(&bf, Approval::deny()).unwrap();
    let PrepareMaterialization::Ready(prepared) = bf
        .prepare_materialization_for_test(input, |event| {
            events.lock().unwrap().push(event)
        })
        .unwrap()
    else {
        panic!("unexpected fixed point")
    };
    assert_eq!(prepared.body.len() as u64, prepared.stats.bytes);
    assert_eq!(prepared.stats.rows, bf.seg.vrow_count());
    assert!(bf.index_capacities().0 >= prepared.stats.line_capacity);
    assert!(bf.index_capacities().1 >= prepared.stats.record_capacity);
    assert!(old_capacities.0 <= bf.index_capacities().0);
    assert_eq!(bf.backing_identity(), original, "提交前旧 RAM 不得被接管");

    let events = events.into_inner().unwrap();
    let count = position_of(&events, |event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::CountFinished { .. })));
    let history = position_of(&events, |event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::HistoryPrepared)));
    let index = position_of(&events, |event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::IndexPrepared { .. })));
    let reserve = position_of(&events, |event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::NewReserveBegin { .. })));
    assert!(count < history && history < index && index < reserve, "{events:?}");
}
```

故障测试覆盖第一遍和第二遍，快照包含 rows、headers、history stats、`revision`、`history_generation`、`index_epoch`、path/base stamp 和筛选条件：

```rust
#[test]
fn materialization_count_or_second_pass_fault_preserves_complete_old_state() {
    for (name, fault, expected) in [
        ("count", MaterializeFault::CountAfter(1), "计数"),
        // 顺序 RamRegion 只在实际提交水位检查故障；0 必定命中第二遍首次页提交。
        ("commit", MaterializeFault::CommitAt(0), "提交"),
    ] {
        let (_p, mut bf) = edited_memory_document(name);
        let before = bf.materialization_snapshot();
        let original = bf.backing_identity();
        let input = MaterializationInput::save_current(&bf, Approval::deny())
            .unwrap()
            .with_fault(fault);
        let error = bf.prepare_materialization_for_test(input, |_| {}).unwrap_err();
        assert!(error.contains(expected), "{name}: {error}");
        assert_eq!(bf.backing_identity(), original);
        assert_eq!(bf.materialization_snapshot(), before);
    }
}
```

- [x] **Step 2: 运行 RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib engine::materialize_tests
```

Expected: 编译失败，物化类型、事件和测试入口不存在。

- [x] **Step 3: 实现计数、历史/固定点和索引容量准备**

`MaterializationInput` 拥有候选 `Arc<Schema>`、saved headers、可选排序 `Arc<Perm>`、Approval、历史和故障配置；每遍先以 `let output = input.output()` 取得轻量冻结克隆，再用 `output.view(document)` 短暂借用文档。准备顺序固定为：

```rust
observer(SaveRamEvent::Phase(SaveRamPhase::CountBegin));
let counted = {
    let output = input.output();
    let mut sink = CountingSink::new();
    let mut on_rows = input.count_progress();
    serialize_document(output.view(document), &mut sink, &mut on_rows)?
};
if matches!(input.fault, MaterializeFault::CountAfter(limit) if counted.rows >= limit) {
    return Err(format!("注入第一遍计数故障：{} 条记录", counted.rows));
}
observer(SaveRamEvent::Phase(SaveRamPhase::CountFinished {
    new_bytes: counted.bytes,
    line_capacity: counted.line_capacity,
    record_capacity: counted.record_capacity,
}));
let history = match input.prepare_history(document, counted.bytes)? {
    HistoryPreparation::Ready(history) => history,
    HistoryPreparation::FixedPoint(request) => {
        return Ok(PrepareMaterialization::FixedPoint(request));
    }
};
observer(SaveRamEvent::Phase(SaveRamPhase::HistoryPrepared));
let index = document.idx.prepare_reuse(counted)?;
observer(SaveRamEvent::Phase(SaveRamPhase::IndexPrepared {
    line_capacity: counted.line_capacity,
    record_capacity: counted.record_capacity,
}));

// 所有可失败的算术、代际和分配型状态都先于第二份 RAM 与 NewRamReady。
let new_len = usize::try_from(counted.bytes).map_err(|_| "正文超过本机地址空间")?;
let n_data = u32::try_from(counted.rows).map_err(|_| "正文记录数超过 u32")?;
let next_history_generation = document.next_history_generation()?;
let next_index_epoch = document.index_epoch.checked_add(1).ok_or("索引代际溢出")?;
let segments = Segments::new(u32::from(!document.headerless), n_data);
let fixed_point = matches!(&history, PreparedRamHistory::FixedPointClear { .. });
let output = input.output();
let content_key = RamContentKey {
    history_generation: next_history_generation,
    schema: Arc::clone(&output.schema),
    order: RowOrder::Identity,
    output_encoding: document.output_encoding,
    format: document.format,
    headerless: document.headerless,
    has_bom: document.has_bom,
};
observer(SaveRamEvent::Phase(SaveRamPhase::NewReserveBegin {
    new_bytes: counted.bytes,
}));
if input.fault == MaterializeFault::Reserve {
    return Err("注入第二份 RAM 预留失败".into());
}
let region = RamRegion::reserve_with_enable_fault(new_len, input.enable_fault_at())?;
let mut sink = RamRegionSink::new(region);
let written = {
    let mut on_rows = input.ram_progress();
    serialize_document(output.view(document), &mut sink, &mut on_rows)?
};
if written != counted {
    return Err(format!("两遍输出结构不一致：{counted:?} != {written:?}"));
}
let body = sink.finish()?;
observer(SaveRamEvent::Phase(SaveRamPhase::NewRamReady {
    new_bytes: counted.bytes,
}));
Ok(PrepareMaterialization::Ready(PreparedMaterialization {
    body,
    stats: counted,
    index,
    history,
    schema: output.schema,
    base_headers: output.saved_headers,
    segments,
    order: RowOrder::Identity,
    next_history_generation,
    next_index_epoch,
    fixed_point,
    content_key,
}))
```

`RawIndex::prepare_reuse` 对 `line_off` 和 `rec_lines` 分别按 `target.saturating_sub(Vec::capacity())` 调用 `try_reserve`，所有 `u64 → usize/u32` 转换都 checked；不清空旧索引。`PreparedIndexReuse` 只保存目标容量、记录数和格式，不持有借用。

`PreparedRamHistory` 只有两个变体：

```rust
pub(super) enum PreparedRamHistory {
    Rebuild(PreparedSaveHistory),
    FixedPointClear { budget_bytes: u64 },
}
```

历史转换仍在旧正文可用时完成；`new_bytes` 已知后以 `EvictOldest` 重建。固定点请求在任何 RAM 预留之前返回。`PreparedMaterialization.segments` 也提前用 `Segments::new(offset, n_data)` 构造，提交段不再调用分配型构造函数。

`BigFile` 新增 `ram_content_key: Option<RamContentKey>` 和 `disk_body_dirty: bool`，所有构造器分别初始化为 `None` / `false`；Task 5 才把它们接入保存与 `modified()`。这里的“可失败准备”覆盖现有 `Result`、检查算术和 `try_reserve`，不宣称 Rust 全局分配器所有 OOM 都能恢复。

在 `materialize.rs` 增加跨兄弟测试模块可见的后备地址助手；空文档不得用它证明身份：

```rust
#[cfg(test)]
impl BigFile {
    pub(super) fn backing_identity(&self) -> usize {
        self.backing.bytes().as_ptr() as usize
    }

    pub(super) fn prepare_materialization_for_test<O>(
        &mut self,
        input: MaterializationInput<'_>,
        observer: O,
    ) -> Result<PrepareMaterialization, String>
    where
        O: Fn(SaveRamEvent) + Sync,
    {
        prepare_materialization(self, input, &observer)
    }
}
```

- [x] **Step 4: 实现只做状态移动的 RAM 提交**

`commit_ram` 开始先解构 `PreparedMaterialization`，保存旧字节数，再复用旧索引容量。核心移动顺序如下，不创建临时文件、不读盘、不调用 `reclaim_unreferenced_columns`：

```rust
let old_bytes = document.backing.bytes().len() as u64;
let mut index = std::mem::take(&mut document.idx);
index.scan_reusing_capacity(prepared.body.as_slice(), document.format, prepared.index);
let previous = std::mem::replace(
    &mut document.backing,
    Backing::Ram(RamBuffer::Region(prepared.body)),
);
drop(previous);
observer(SaveRamEvent::Phase(SaveRamPhase::OldRamReleased { old_bytes }));
document.idx = index;
document.partial = None;
document.total_bytes = prepared.stats.bytes;
document.seg = prepared.segments;
document.overlay.clear();
document.stable_hits = None;
document.display_hits = None;
document.last_spec = None;
document.schema = prepared.schema;
document.base_ncols = prepared.base_headers.len();
document.base_headers = prepared.base_headers;
document.order = prepared.order;
document.fixed_point = prepared.fixed_point;
document.history_generation = prepared.next_history_generation;
document.index_epoch = prepared.next_index_epoch;
document.backing_encoding = document.output_encoding;
prepared.history.commit_to(document);
document.disk_body_dirty = true;
document.ram_content_key = Some(prepared.content_key);
```

`scan_reusing_capacity` 是无分配、无 `Result` 的内部提交原语：目标容量和结构已在第二遍核对，容量不足或结构不等属于测试必须覆盖的内部不变量违例，不能转成“旧 RAM 已释放后的普通保存错误”。提交期间文档写锁持续持有。

`path`、`backing_path`、`base_size`、`base_mtime`、`revision` 和 `savepoint_detached` 在此函数中均不更新。事件实际顺序必须是 `NewRamReady → OldRamReleased`，不能为了复用旧事件而提前发旧正文释放。

- [x] **Step 5: 增加提交后索引/筛选测试并运行 GREEN**

新增以下完整测试；先建立筛选结果，再提交新 RAM，证明旧显示偏移不会直接套用到新正文：

```rust
#[test]
fn materialization_commit_installs_body_reuses_index_and_rebuilds_filter() {
    let (_p, mut bf) = edited_memory_document("materialize-commit");
    let filter = FilterSpec {
        cols: vec![(1, "31".into(), 0)],
        multi_cols: vec![],
        multi_text: String::new(),
    };
    assert_eq!(bf.run_filter(&filter).unwrap(), 1);
    assert!(bf.last_spec.is_some());
    let before = bf.materialization_snapshot();
    let old_identity = bf.backing_identity();
    let events = Mutex::new(Vec::new());
    let input = MaterializationInput::save_current(&bf, Approval::deny()).unwrap();
    let PrepareMaterialization::Ready(prepared) = bf
        .prepare_materialization_for_test(input, |event| {
            events.lock().unwrap().push(event)
        })
        .unwrap()
    else {
        panic!("unexpected fixed point")
    };
    let expected_key = prepared.content_key.clone();
    let expected_records = prepared.stats.rows;
    let expected_line_capacity = prepared.stats.line_capacity;
    let expected_record_capacity = prepared.stats.record_capacity;
    let expected_bytes = prepared.stats.bytes;

    commit_ram(&mut bf, prepared, &|event| events.lock().unwrap().push(event));
    assert_ne!(bf.backing_identity(), old_identity);
    assert_eq!(bf.rows(0, 1)[0], ["Alice", "31", "NY"]);
    assert_eq!(bf.idx.n_records() as u64, expected_records);
    assert!(bf.idx.line_off.capacity() >= expected_line_capacity);
    assert!(bf.idx.rec_lines.capacity() >= expected_record_capacity);
    assert_eq!(bf.ram_content_key, Some(expected_key));
    assert!(bf.disk_body_dirty);
    assert_eq!(bf.path, before.path);
    assert_eq!(bf.backing_path, before.backing_path);
    assert_eq!(bf.base_size, before.base_size);
    assert_eq!(bf.base_mtime, before.base_mtime);
    assert_eq!(bf.total_bytes, expected_bytes);
    assert!(bf.last_spec.is_none() && bf.stable_hits.is_none() && bf.display_hits.is_none());
    assert_eq!(bf.run_filter(&filter).unwrap(), 1, "必须按新正文重新求值");

    let events = events.into_inner().unwrap();
    let ready = position_of(&events, |event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::NewRamReady { .. })));
    let released = position_of(&events, |event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::OldRamReleased { .. })));
    assert!(ready < released, "{events:?}");
}
```

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib engine::materialize_tests
cargo test --manifest-path src-tauri/Cargo.toml --lib engine::serialize_tests
cargo test --manifest-path src-tauri/Cargo.toml --lib p6_history_never_exceeds_budget_after_interleaved_operations
cargo test --manifest-path src-tauri/Cargo.toml --lib big_filter
```

Expected: 两遍 bytes/stats 完全相等，索引容量在 RAM 预留前完成，第一/第二遍故障保持完整旧状态；筛选条件不被旧显示偏移错误复用，后续按新正文重新求值。

- [x] **Step 6: 编译检查点，不提交**

Run:

```bash
cargo build --manifest-path src-tauri/Cargo.toml
cargo build --release --manifest-path src-tauri/Cargo.toml --bin simpletable
git diff --check -- src-tauri/src/engine.rs src-tauri/src/engine/materialize.rs src-tauri/src/engine/materialize_tests.rs
```

Expected: 编译成功；不执行 Git 提交。

---

### Task 5: RAM/磁盘两段提交、正文缓存键和失败重试

**Files:**
- Modify: `src-tauri/src/engine.rs:7839-7867`、`:9015-9086`、`:10627-10629`、`:13992-14732`
- Modify: `src-tauri/src/engine/materialize.rs`
- Test: `src-tauri/src/engine.rs:15187-15582`（`memory_save_tests`）
- Test: `src-tauri/src/engine/materialize_tests.rs`

**Interfaces:**
- Consumes: Task 4 的 `RamContentKey`、`prepare_materialization`、`commit_ram` 和 `PreparedMaterialization`。
- Produces:

```rust
pub const RAM_COMMITTED_ERROR: &str = "RAM_COMMITTED:";

fn write_completed_ram<F: FnMut(u64, u64)>(
    ram: &FinishedRam,
    tmp: &Path,
    progress: F,
) -> Result<FileStamp, String>;

fn replace_materialized_with_retry(tmp: &Path, dest: &Path) -> Result<(), String>;
fn commit_disk_success(&mut self, dest: &Path, switch_path: bool, stamp: FileStamp);
```

`SaveRamEvent` 只保留 `Phase(SaveRamPhase)`。删除 `OldRamRestored`、`EmergencyMmapFallback`、`Load` 和 `StreamProgress`，不再以事件名暗示旧恢复模型。完整阶段定义为：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum SaveRamPhase {
    CountBegin,
    CountFinished { new_bytes: u64, line_capacity: usize, record_capacity: usize },
    HistoryPrepared,
    IndexPrepared { line_capacity: usize, record_capacity: usize },
    NewReserveBegin { new_bytes: u64 },
    NewRamReady { new_bytes: u64 },
    OldRamReleased { old_bytes: u64 },
    DiskWriteBegin { new_bytes: u64 },
    DiskWriteFinished { new_bytes: u64 },
    DiskCommitted { new_bytes: u64 },
}
```

- [x] **Step 1: 先改写旧恢复测试，建立新语义的 RED**

把 `memory_save_read_fault_restores_old_ram_and_history` 改为第二遍故障保留旧 RAM；把 rename/write 故障测试改为 RAM 提交后的新状态。以下测试直接复用现有 `persistent_file`、`save_people_csv`、`tmp_path_for` 和 `Mutex`，不新增虚构 fixture：

```rust
#[test]
fn memory_save_rename_fault_keeps_new_ram_dirty_and_original_path() {
    let (path, mut file) = persistent_file(
        "rename_keeps_new_ram",
        &save_people_csv(),
        UndoPolicy::default(),
    );
    file.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
    let old_disk = fs::read(&path).unwrap();
    let old_base_size = file.base_size;
    let old_base_mtime = file.base_mtime;
    let target = std::env::temp_dir().join(format!(
        "st_engine_memory_save_rename_target_{}.csv",
        std::process::id(),
    ));
    fs::create_dir(&target).unwrap();
    let events = Mutex::new(Vec::new());
    let error = file.save_to_observed(
        &target,
        true,
        Approval::deny(),
        LoadOptions::default(),
        |_, _, _, _| {},
        |event| events.lock().unwrap().push(event),
    ).unwrap_err();
    assert!(error.starts_with(RAM_COMMITTED_ERROR), "{error}");
    assert!(file.modified() && file.disk_body_dirty);
    assert_eq!(file.path, path);
    assert_eq!(file.backing_path, path);
    assert_eq!(file.base_size, old_base_size);
    assert_eq!(file.base_mtime, old_base_mtime);
    assert_eq!(file.rows(0, 1)[0][1], "31");
    assert_eq!(fs::read(&path).unwrap(), old_disk);
    let events = events.into_inner().unwrap();
    let ready = events.iter().position(|event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::NewRamReady { .. }))).unwrap();
    let released = events.iter().position(|event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::OldRamReleased { .. }))).unwrap();
    assert!(ready < released, "{events:?}");
    assert!(!events.iter().any(|event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::DiskCommitted { .. }))));
    fs::remove_file(tmp_path_for(&target)).ok();
    fs::remove_dir(&target).ok();
    fs::remove_file(&path).ok();
}

#[test]
fn memory_save_unchanged_retry_writes_same_ram_without_materializing() {
    let (path, mut file) = persistent_file(
        "unchanged_retry",
        &save_people_csv(),
        UndoPolicy::default(),
    );
    file.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
    let target = std::env::temp_dir().join(format!(
        "st_engine_memory_save_retry_target_{}.csv",
        std::process::id(),
    ));
    fs::create_dir(&target).unwrap();
    let events = Mutex::new(Vec::new());
    let error = file.save_to_observed(
        &target,
        true,
        Approval::deny(),
        LoadOptions::default(),
        |_, _, _, _| {},
        |event| events.lock().unwrap().push(event),
    ).unwrap_err();
    assert!(error.starts_with(RAM_COMMITTED_ERROR), "{error}");
    let identity = file.backing_identity();
    let first_materializations = events.lock().unwrap().iter().filter(|event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::NewRamReady { .. }))).count();

    // 只移除外部的换名障碍；第二次保存不重写文档正文。
    fs::remove_dir(&target).unwrap();
    let outcome = file.save_to_observed(
        &target,
        true,
        Approval::deny(),
        LoadOptions::default(),
        |_, _, _, _| {},
        |event| events.lock().unwrap().push(event),
    ).unwrap();
    assert!(outcome.saved && !file.modified());
    assert_eq!(file.backing_identity(), identity);
    let events = events.into_inner().unwrap();
    let total_materializations = events.iter().filter(|event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::NewRamReady { .. }))).count();
    assert_eq!(total_materializations, first_materializations, "{events:?}");
    assert_eq!(fs::read(&target).unwrap(), file.backing.bytes());
    fs::remove_file(&target).ok();
    fs::remove_file(&path).ok();
}
```

磁盘创建、写入和 flush/sync 故障通过下列仅测试使用的 RAII 注入，生产编译不包含它：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DiskWriteFault {
    None,
    Create,
    WriteAfter(u64),
    Flush,
    Sync,
}

thread_local! {
    static DISK_WRITE_FAULT: Cell<DiskWriteFault> = const { Cell::new(DiskWriteFault::None) };
}

pub(super) fn disk_write_fault() -> DiskWriteFault {
    DISK_WRITE_FAULT.with(Cell::get)
}

struct DiskWriteFaultGuard;

impl Drop for DiskWriteFaultGuard {
    fn drop(&mut self) {
        DISK_WRITE_FAULT.with(|flag| flag.set(DiskWriteFault::None));
    }
}

fn disk_write_fault_guard(fault: DiskWriteFault) -> DiskWriteFaultGuard {
    DISK_WRITE_FAULT.with(|flag| flag.set(fault));
    DiskWriteFaultGuard
}
```

每个故障都重新创建独立持久内存文档，避免前一用例的 RAM 提交状态掩盖下一故障。加入以下四个真实测试：

```rust
#[test]
fn memory_save_write_and_flush_faults_keep_new_ram_and_temp_error() {
    for (name, fault) in [
        ("create", DiskWriteFault::Create),
        ("write", DiskWriteFault::WriteAfter(1)),
        ("flush", DiskWriteFault::Flush),
        ("sync", DiskWriteFault::Sync),
    ] {
        let (path, mut file) = persistent_file(name, &save_people_csv(), UndoPolicy::default());
        file.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
        let old_identity = file.backing_identity();
        let old_disk = fs::read(&path).unwrap();
        let old_size = file.base_size;
        let old_mtime = file.base_mtime;
        let events = Mutex::new(Vec::new());
        let progress = Mutex::new(Vec::new());
        let _guard = disk_write_fault_guard(fault);
        let error = file.save_to_observed(
            &path,
            false,
            Approval::deny(),
            LoadOptions::default(),
            |done, total, phase, rows| {
                progress.lock().unwrap().push((done, total, phase, rows));
            },
            |event| events.lock().unwrap().push(event),
        ).unwrap_err();
        assert!(error.starts_with(RAM_COMMITTED_ERROR), "{name}: {error}");
        assert_ne!(file.backing_identity(), old_identity, "{name} 必须先提交新 RAM");
        assert!(file.modified() && file.disk_body_dirty, "{name}");
        assert_eq!(file.path, path);
        assert_eq!(file.backing_path, path);
        assert_eq!(file.base_size, old_size);
        assert_eq!(file.base_mtime, old_mtime);
        assert_eq!(file.rows(0, 1)[0][1], "31");
        assert_eq!(fs::read(&path).unwrap(), old_disk, "{name} 不得替换原目标");
        assert!(!events.lock().unwrap().iter().any(|event| matches!(event,
            SaveRamEvent::Phase(SaveRamPhase::DiskCommitted { .. }))), "{name}");
        assert!(!progress.lock().unwrap().iter().any(|(done, total, phase, _)| {
            *total > 0 && *done == *total && *phase == 3
        }), "{name} 不得发送最终 100%");
        fs::remove_file(tmp_path_for(&path)).ok();
        fs::remove_file(&path).ok();
    }
}

#[test]
fn memory_sorted_save_failure_keeps_sorted_ram_fixed_point_and_dirty() {
    let pad = "p".repeat(128);
    let bytes = format!("id,value,note\r\na,1,{pad}\r\nc,3,{pad}\r\nb,2,{pad}\r\n").into_bytes();
    let (path, mut file) = persistent_file(
        "sorted_failure_new_ram",
        &bytes,
        UndoPolicy { budget_bytes: 1, resident_permille: 1000 },
    );
    assert!(file.sort_by_levels(
        &[SortLevel { col: 0, numeric: false, asc: false }],
        Approval::deny(),
    ).unwrap().changed);
    file.set_cell(0, 1, "9".into(), Approval::deny()).unwrap();
    let denied = file.save(Approval::deny(), |_, _, _, _| {}).unwrap();
    let request = denied.fixed_point.unwrap();
    assert_eq!(request.kind, FixedPointKind::SortMaterialization);
    let old_disk = fs::read(&path).unwrap();
    let target = std::env::temp_dir().join(format!(
        "st_sorted_ram_rename_directory_{}", std::process::id()));
    fs::create_dir(&target).unwrap();
    let error = file.save_to_observed(
        &target,
        true,
        Approval::allow(request.preflight_id),
        LoadOptions::default(),
        |_, _, _, _| {},
        |_| {},
    ).unwrap_err();
    assert!(error.starts_with(RAM_COMMITTED_ERROR), "{error}");
    assert_eq!(file.order, RowOrder::Identity);
    assert!(file.fixed_point && file.disk_body_dirty && file.modified());
    assert_eq!(file.revision, 0, "该夹具必须覆盖 revision=0 仍 dirty");
    assert_eq!(file.history_stats().undo_count, 0);
    assert_eq!(file.history_stats().redo_count, 0);
    assert_eq!(file.rows(0, 3).iter().map(|row| row[0].clone()).collect::<Vec<_>>(), ["c", "b", "a"]);
    assert_eq!(file.path, path);
    assert_eq!(file.backing_path, path);
    assert_eq!(fs::read(&path).unwrap(), old_disk);
    fs::remove_file(tmp_path_for(&target)).ok();
    fs::remove_dir(&target).ok();
    fs::remove_file(&path).ok();
}

#[test]
fn memory_save_external_change_cancels_disk_and_keeps_new_ram() {
    let (path, mut file) = persistent_file(
        "external_after_ram_commit",
        &save_people_csv(),
        UndoPolicy::default(),
    );
    file.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
    let old_identity = file.backing_identity();
    let old_disk = fs::read(&path).unwrap();
    let touched = AtomicBool::new(false);
    let events = Mutex::new(Vec::new());
    let error = file.save_to_observed(
        &path,
        false,
        Approval::deny(),
        LoadOptions::default(),
        |_, _, _, _| {},
        |event| {
            let released = matches!(event,
                SaveRamEvent::Phase(SaveRamPhase::OldRamReleased { .. }));
            if released && !touched.swap(true, Ordering::SeqCst) {
                let handle = fs::OpenOptions::new().write(true).open(&path).unwrap();
                handle.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(2))
                    .unwrap();
            }
            events.lock().unwrap().push(event);
        },
    ).unwrap_err();
    assert!(error.starts_with(RAM_COMMITTED_ERROR) && error.contains("EXTERNAL_CHANGED"), "{error}");
    assert_ne!(file.backing_identity(), old_identity);
    assert!(file.modified() && file.disk_body_dirty);
    assert_eq!(file.rows(0, 1)[0][1], "31");
    assert_eq!(file.path, path);
    assert_eq!(fs::read(&path).unwrap(), old_disk, "只改 mtime，不得重写或重载正文");
    assert!(!events.into_inner().unwrap().iter().any(|event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::DiskWriteBegin { .. }))));
    fs::remove_file(&path).ok();
}

#[test]
fn memory_save_success_uses_same_ram_and_never_reloads_temp() {
    let (path, mut file) = persistent_file(
        "success_same_ram",
        &save_people_csv(),
        UndoPolicy::default(),
    );
    file.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
    let events = Mutex::new(Vec::new());
    let first = file.save_to_observed(
        &path,
        false,
        Approval::deny(),
        LoadOptions::default(),
        |_, _, _, _| {},
        |event| events.lock().unwrap().push(event),
    ).unwrap();
    assert!(first.saved && !file.modified());
    let identity = file.backing_identity();
    assert_eq!(fs::read(&path).unwrap(), file.backing.bytes());

    let second = file.save_to_observed(
        &path,
        false,
        Approval::deny(),
        LoadOptions::default(),
        |_, _, _, _| {},
        |event| events.lock().unwrap().push(event),
    ).unwrap();
    assert!(second.saved && file.backing_identity() == identity);
    assert_eq!(fs::read(&path).unwrap(), file.backing.bytes());
    let events = events.into_inner().unwrap();
    let position = |predicate: &dyn Fn(&SaveRamPhase) -> bool| {
        events.iter().position(|event| matches!(event, SaveRamEvent::Phase(phase) if predicate(phase)))
            .unwrap()
    };
    let ready = position(&|phase| matches!(phase, SaveRamPhase::NewRamReady { .. }));
    let released = position(&|phase| matches!(phase, SaveRamPhase::OldRamReleased { .. }));
    let disk = position(&|phase| matches!(phase, SaveRamPhase::DiskWriteBegin { .. }));
    let committed = position(&|phase| matches!(phase, SaveRamPhase::DiskCommitted { .. }));
    assert!(ready < released && released < disk && disk < committed, "{events:?}");
    assert_eq!(events.iter().filter(|event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::NewRamReady { .. }))).count(), 1,
        "第二次保存必须直接复用同一 RAM");
    fs::remove_file(&path).ok();
}
```

前面的另存为换名失败测试已同时断言 `path/backing_path/base_size/base_mtime` 保持原成功磁盘身份，不再保留一个重复的仅路径测试。

- [x] **Step 2: 运行新测试确认 RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib memory_save_rename_fault_keeps_new_ram_dirty_and_original_path
cargo test --manifest-path src-tauri/Cargo.toml --lib memory_save_unchanged_retry_writes_same_ram_without_materializing
cargo test --manifest-path src-tauri/Cargo.toml --lib memory_sorted_save_failure_keeps_sorted_ram_fixed_point_and_dirty
```

Expected: 旧实现因恢复旧 RAM、重新从临时文件载入、失败后视图回滚或 `revision == 0` 被误判干净而 FAIL。

- [x] **Step 3: 建立 RAM 内容键与独立磁盘 dirty**

`modified()` 改为：

```rust
pub fn modified(&self) -> bool {
    self.savepoint_detached || self.revision != 0 || self.disk_body_dirty
}
```

新增：

```rust
fn current_ram_content_key(&self) -> RamContentKey {
    RamContentKey {
        history_generation: self.history_generation,
        schema: Arc::clone(&self.schema),
        order: self.order.clone(),
        output_encoding: self.output_encoding,
        format: self.format,
        headerless: self.headerless,
        has_bom: self.has_bom,
    }
}

fn persistent_ram_is_current(&self) -> bool {
    self.memory_mode
        && matches!(self.backing, Backing::Ram(RamBuffer::Region(_)))
        && self.ram_content_key.as_ref() == Some(&self.current_ram_content_key())
}
```

`revision` 不作为键。`history_generation` 覆盖编辑、撤销重做和 Schema 代际；`RowOrder` 覆盖排序物化；编码、格式、表头和 BOM 防止直接写错误输出。预算调整若也推进 generation，允许多一次保守物化，但不允许复用陈旧正文。

进入持久内存模式并完成 undo policy 提交后，如果 `revision == 0`，为物理恒等正文建立键；有编辑时设为 `None`。临时排序的 `Perm` 与物理 `Identity` 键不相等，因此排序保存仍会物化。RAM 提交时键必须使用提交后的 generation、Schema 和 `RowOrder::Identity`。

- [x] **Step 4: 持久保存改为先 RAM 提交，再创建临时文件**

`save_to_observed` 的持久内存分支固定为：

```rust
if self.memory_mode {
    if !self.persistent_ram_is_current() {
        let input = MaterializationInput::save_current(self, approval)?;
        match self.prepare_materialization(input, &observer)? {
            PrepareMaterialization::Ready(prepared) => self.commit_ram(prepared, &observer),
            PrepareMaterialization::FixedPoint(request) => {
                return Ok(SaveOutcome {
                    saved: false,
                    fixed_point: Some(request),
                    cancelled: false,
                });
            }
        }
    }
    debug_assert!(self.persistent_ram_is_current());
    if !switch_path && self.stale() {
        return Err(format!(
            "{RAM_COMMITTED_ERROR}EXTERNAL_CHANGED: 写盘前发现文件发生变化"
        ));
    }
    return self.commit_persistent_ram_to_disk(dest, switch_path, progress, observer);
}
```

`commit_persistent_ram_to_disk` 只从当前 `FinishedRam` 读取；不得调用 `prepare_memory_reopen`、`prepare_reopen`、`convert_mmap_to_ram` 或旧恢复函数。临时文件创建必须发生在 RAM 提交之后。外部状态在写盘后、换名前再检查一次；检测到变化时保留临时文件并按现有提示返回错误，不替换目标。

- [x] **Step 5: 实现有界 RAM 写盘、换名和磁盘成功提交**

使用 1 MiB 有界缓冲和块：

```rust
const DISK_COPY_CHUNK: usize = 1024 * 1024;
#[cfg(test)]
if self::memory_save_tests::disk_write_fault() == self::memory_save_tests::DiskWriteFault::Create {
    return Err("注入创建临时文件失败".into());
}
let out = fs::File::create(tmp).map_err(|error| format!("创建临时文件失败：{error}"))?;
let mut writer = BufWriter::with_capacity(DISK_COPY_CHUNK, out);
observer(SaveRamEvent::Phase(SaveRamPhase::DiskWriteBegin {
    new_bytes: ram.len() as u64,
}));
let mut copied = 0u64;
for chunk in ram.as_slice().chunks(DISK_COPY_CHUNK) {
    writer.write_all(chunk).map_err(|error| format!("写入临时文件失败：{error}"))?;
    copied = copied.checked_add(chunk.len() as u64).ok_or("写盘长度溢出")?;
    #[cfg(test)]
    if let self::memory_save_tests::DiskWriteFault::WriteAfter(limit) =
        self::memory_save_tests::disk_write_fault()
    {
        if copied >= limit {
            return Err("注入写入临时文件失败".into());
        }
    }
    progress(copied, ram.len() as u64);
}
#[cfg(test)]
if self::memory_save_tests::disk_write_fault() == self::memory_save_tests::DiskWriteFault::Flush {
    return Err("注入刷新临时文件失败".into());
}
writer.flush().map_err(|error| format!("刷新临时文件失败：{error}"))?;
let file = writer.into_inner().map_err(|error| format!("关闭写器失败：{}", error.error()))?;
#[cfg(test)]
if self::memory_save_tests::disk_write_fault() == self::memory_save_tests::DiskWriteFault::Sync {
    return Err("注入同步临时文件失败".into());
}
file.sync_all().map_err(|error| format!("同步临时文件失败：{error}"))?;
drop(file);
let stamp = FileStamp::read(tmp)?;
observer(SaveRamEvent::Phase(SaveRamPhase::DiskWriteFinished {
    new_bytes: ram.len() as u64,
}));
```

`replace_materialized_with_retry` 复用现有换名重试时长和中文占用提示，但失败时只返回错误，绝不 remap/restore。换名成功后的提交是无失败移动：

```rust
fn commit_disk_success(&mut self, dest: &Path, switch_path: bool, stamp: FileStamp) {
    if switch_path {
        self.path = dest.to_path_buf();
    }
    self.backing_path = self.path.clone();
    self.base_size = stamp.len;
    self.base_mtime = stamp.modified;
    self.revision = 0;
    self.savepoint_detached = false;
    self.disk_body_dirty = false;
}
```

`stamp` 在换名前从已关闭临时文件取得，避免换名后再引入一个可失败元数据步骤。只有此函数返回后才发送 `DiskCommitted`、最终 100% 和 `SaveOutcome { saved: true, .. }`。RAM 提交之后的所有错误路径都要加 `RAM_COMMITTED_ERROR` 前缀，供 Task 8 前端区分“需要刷新已提交 RAM”和“提交前旧现场未变”。

- [x] **Step 6: GREEN、排序/固定点/另存为及普通 mmap 回归**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib memory_save
cargo test --manifest-path src-tauri/Cargo.toml --lib memory_sorted
cargo test --manifest-path src-tauri/Cargo.toml --lib save_as
cargo test --manifest-path src-tauri/Cargo.toml --lib save_sorted_history_requires_fixed_point_confirmation
cargo test --manifest-path src-tauri/Cargo.toml --lib big_save
cargo test --manifest-path src-tauri/Cargo.toml --lib temporary_ram_and_new_document_save_legacy_behavior_unchanged
```

Expected: 新 RAM 在写盘/换名失败后保留且 dirty；未变化重试没有第二次 NewRamReady；排序固定点在内存中保留；另存为仅成功切路径；普通 mmap 和临时 RAM 的既有路径不退化。

- [x] **Step 7: 编译检查点，不提交**

Run:

```bash
cargo build --manifest-path src-tauri/Cargo.toml
cargo build --release --manifest-path src-tauri/Cargo.toml --bin simpletable
cargo build --release --manifest-path src-tauri/Cargo.toml --bin memory_peak
git diff --check -- src-tauri/src/engine.rs src-tauri/src/engine/materialize.rs
```

Expected: 编译成功；不执行 Git 提交。

---

### Task 6: 持久内存列操作原子物化与未保存 RAM 退出保护

**Files:**
- Modify: `src-tauri/src/engine.rs:10820-10865`、`:14403-14444`
- Modify: `src-tauri/src/engine/materialize.rs`
- Test: `src-tauri/src/engine/materialize_tests.rs`
- Test: `src-tauri/src/memory_runtime_tests.rs`

**Interfaces:**
- Consumes: `MaterializationInput::columns`、`prepare_materialization`、`commit_ram`、`disk_body_dirty`。
- Produces: `pub fn apply_col_ops_observed<O>(&mut self, approval: Approval, observer: &O) -> Result<EditOutcome, String> where O: Fn(SaveRamEvent) + Sync`；原 `apply_col_ops` 以空 observer 委托，供非 IPC 调用保持兼容。

- [x] **Step 1: 写列操作只改 RAM 和失败保留 pending 的 RED 测试**

```rust
#[test]
fn memory_columns_materialize_ram_without_touching_disk() {
    let (path, mut bf) = edited_memory_document("columns-ram-only");
    let disk_before = fs::read(&path).unwrap();
    let mtime_before = fs::metadata(&path).unwrap().modified().unwrap();
    let old_ram = bf.backing_identity();
    bf.push_col_op(ColOp::Insert { pos: 1, name: "empty".into() }).unwrap();
    let events = Mutex::new(Vec::new());
    let outcome = bf.apply_col_ops_observed(Approval::deny(), &|event| {
        events.lock().unwrap().push(event)
    }).unwrap();
    assert!(outcome.changed && bf.modified() && bf.disk_body_dirty);
    assert!(bf.pending_cols.is_empty() && bf.pending_schema.is_none());
    assert_ne!(bf.backing_identity(), old_ram);
    assert_eq!(bf.view_headers(), ["name", "empty", "age", "city"]);
    assert_eq!(bf.rows(0, 1)[0], ["Alice", "", "31", "NY"]);
    assert_eq!(fs::read(&path).unwrap(), disk_before);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), mtime_before);
    let events = events.into_inner().unwrap();
    assert!(events.iter().any(|event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::NewRamReady { .. }))));
    assert!(!events.iter().any(|event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::DiskWriteBegin { .. })
        | SaveRamEvent::Phase(SaveRamPhase::DiskCommitted { .. }))));
    fs::remove_file(&path).ok();
}

#[test]
fn memory_columns_fault_preserves_live_schema_and_pending_preview() {
    let (_p, mut bf) = edited_memory_document("columns-pending-atomic");
    bf.push_col_op(ColOp::Insert { pos: 1, name: "empty".into() }).unwrap();
    let before = bf.materialization_snapshot();
    let pending_schema = bf.pending_schema.clone();
    let old_ram = bf.backing_identity();
    let input = MaterializationInput::columns(&bf, Approval::deny())
        .unwrap()
        .with_fault(MaterializeFault::CommitAt(16));
    let error = bf.prepare_materialization_for_test(input, |_| {}).unwrap_err();
    assert!(error.contains("提交"), "{error}");
    assert_eq!(bf.materialization_snapshot(), before);
    assert_eq!(bf.pending_schema, pending_schema);
    assert_eq!(bf.backing_identity(), old_ram);
}
```

普通 mmap 分支必须与原入口逐项一致，加入：

```rust
#[test]
fn non_memory_columns_keep_schema_only_behavior() {
    let fixture = b"name,age,city\r\nAlice,30,NY\r\nBob,40,SF\r\n";
    let old_path = fixture_path("columns-old-entry", "csv", fixture);
    let new_path = fixture_path("columns-observed-entry", "csv", fixture);
    let mut expected = BigFile::open(&old_path).unwrap();
    let mut actual = BigFile::open(&new_path).unwrap();
    expected.push_col_op(ColOp::Insert { pos: 1, name: "empty".into() }).unwrap();
    actual.push_col_op(ColOp::Insert { pos: 1, name: "empty".into() }).unwrap();
    let original = actual.backing_identity();
    let events = Mutex::new(Vec::new());

    let expected_outcome = expected.apply_col_ops(Approval::deny()).unwrap();
    let actual_outcome = actual.apply_col_ops_observed(Approval::deny(), &|event| {
        events.lock().unwrap().push(event)
    }).unwrap();
    assert_eq!(actual_outcome, expected_outcome);
    assert!(matches!(actual.backing, Backing::Mmap(_)));
    assert_eq!(actual.backing_identity(), original);
    assert_eq!(actual.view_headers(), expected.view_headers());
    assert_eq!(actual.rows(0, 2), expected.rows(0, 2));
    assert!(events.into_inner().unwrap().iter().all(|event| !matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::NewRamReady { .. })
        | SaveRamEvent::Phase(SaveRamPhase::DiskWriteBegin { .. })
        | SaveRamEvent::Phase(SaveRamPhase::DiskCommitted { .. }))));
}
```

- [x] **Step 2: 运行 RED**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib memory_columns_materialize_ram_without_touching_disk
cargo test --manifest-path src-tauri/Cargo.toml --lib memory_columns_fault_preserves_live_schema_and_pending_preview
```

Expected: 第一条因 `apply_col_ops_observed` 不存在而编译失败；补上入口但尚未接物化时，必须因 RAM 身份不变和 pending 语义不符而 FAIL。

- [x] **Step 3: 以候选 Schema 物化并在成功后才提交列操作**

保留现有格式、排序、索引、pending 和列数校验。持久内存分支使用：

```rust
let input = MaterializationInput::columns(self, approval)?;
match prepare_materialization(self, input, observer)? {
    PrepareMaterialization::FixedPoint(request) => {
        let mut outcome = EditOutcome::unchanged();
        outcome.fixed_point = Some(request);
        Ok(outcome)
    }
    PrepareMaterialization::Ready(prepared) => {
        commit_ram(self, prepared, observer);
        // 此点以后只做无失败状态清理；pending 在准备期间始终可供失败重试。
        self.pending_cols.clear();
        self.pending_schema = None;
        self.last_spec = None;
        Ok(EditOutcome {
            changed: true,
            inserted_rows: 0,
            removed_rows: 0,
            fixed_point: None,
            cancelled: false,
        })
    }
}
```

`MaterializationInput::columns` 准备的历史 op 必须是 `Op::Columns { change: SchemaChange { before, after } }`，其中 `after` 是 pending 候选；`commit_ram` 安装同一 `after`，不得先对 live Schema 调用 `apply` 再尝试物化。非内存分支继续执行原 `preflight_history → apply → finish_history`，不生成 RAM、不创建临时文件。

- [x] **Step 4: 写未保存物化 RAM 的退出保护测试并实现**

在 `memory_runtime_tests.rs` 新增：

```rust
#[test]
fn memory_mode_exit_rejects_materialized_unsaved_ram_until_disk_commit() {
    let path = std::env::temp_dir().join(format!(
        "st_memory_exit_unsaved_ram_{}.csv", std::process::id()
    ));
    fs::write(&path, b"name,age\r\nAlice,30\r\n").unwrap();
    let mut file = BigFile::open(&path).unwrap();
    assert!(file.enter_memory_mode_for_test(&AtomicBool::new(false), |_| {}).unwrap());
    file.push_col_op(ColOp::Insert { pos: 1, name: "empty".into() }).unwrap();
    assert!(file.apply_col_ops(Approval::deny()).unwrap().changed);
    let identity = file.backing_identity();
    let error = file.exit_memory_mode(Approval::deny()).unwrap_err();
    assert!(error.starts_with("UNSAVED_RAM:"), "{error}");
    assert_eq!(file.backing_identity(), identity);
    assert!(file.meta().memory_mode && file.meta().ram_backed && file.meta().modified);
    assert!(file.save(Approval::deny(), |_, _, _, _| {}).unwrap().saved);
    assert!(!file.disk_body_dirty);
    let first = file.exit_memory_mode(Approval::deny()).unwrap();
    if let Some(request) = first.fixed_point {
        assert!(file.exit_memory_mode(Approval::allow(request.preflight_id)).unwrap().exited);
    } else {
        assert!(first.exited);
    }
    fs::remove_file(&path).ok();
}
```

在 `exit_memory_mode` 的任何固定点或 remap 准备之前加入：

```rust
if self.disk_body_dirty {
    return Err(
        "UNSAVED_RAM: 当前正文只存在于内存中，请先保存后再退出内存模式".into()
    );
}
```

仅 `disk_body_dirty` 触发保护；尚未物化的普通覆盖层编辑仍沿用现有退出流程。保存换名成功后 Task 5 清除该位，才允许原有 remap。

- [x] **Step 5: GREEN 与本地 exe 检查点**

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib memory_columns
cargo test --manifest-path src-tauri/Cargo.toml --lib non_memory_columns_keep_schema_only_behavior
cargo test --manifest-path src-tauri/Cargo.toml --lib memory_mode_exit
cargo test --manifest-path src-tauri/Cargo.toml --lib memory_save
cargo build --manifest-path src-tauri/Cargo.toml
cargo build --release --manifest-path src-tauri/Cargo.toml --bin simpletable
git diff --check -- src-tauri/src/engine.rs src-tauri/src/engine/materialize.rs src-tauri/src/engine/materialize_tests.rs src-tauri/src/memory_runtime_tests.rs
```

Expected: 全部 PASS，debug/release exe 编译成功；不执行 Git 提交。

---

### Task 7: 将旧 03 行为迁入通用物化并删除双写模型

**Files:**
- Modify: `src-tauri/src/engine.rs:14224-14345`、`:14447-14620`、`:15584-15587`
- Modify: `src-tauri/src/engine/serialize.rs`
- Modify: `src-tauri/src/engine/materialize_tests.rs`
- Delete after user confirmation: `src-tauri/src/engine/insert_save.rs`、`src-tauri/src/engine/insert_save_tests.rs`、`src-tauri/src/ram_region/streaming_tests.rs`
- Modify: `src-tauri/src/ram_region.rs:264-358`、`:614`

**Interfaces:**
- Consumes: Task 3–6 的唯一序列化、物化和保存入口。
- Produces: `serialize` 内部的规则单空列原始记录投影；不暴露新的保存入口，不保留 `SaveRamEvent::Load/StreamProgress`、`RamSource` 或旧 RAM 恢复函数。

- [x] **Step 1: 将 03 字节夹具改为通用 apply/save 断言并确认 RED**

把 `insert_save_preserves_raw_records_without_temp_reload` 移到 `materialize_tests.rs`，改为：

```rust
#[test]
fn general_single_empty_insert_preserves_raw_records_and_never_reloads_temp() {
    let mut source = vec![0xEF, 0xBB, 0xBF];
    source.extend_from_slice(
        b"name,age,note\r\n\"Alice\",30,\"x,y\"\r\n\"bob\nb\",25,\"say \"\"hi\"\"\"\r\nCarol,35,plain",
    );
    let path = fixture_path("general-raw-insert", "csv", &source);
    let mut file = BigFile::open(&path).unwrap();
    assert!(file.enter_memory_mode_for_test(&AtomicBool::new(false), |_| {}).unwrap());
    file.push_col_op(ColOp::Insert { pos: 1, name: "memo".into() }).unwrap();
    let events = Mutex::new(Vec::new());
    assert!(file.apply_col_ops_observed(Approval::deny(), &|event| {
        events.lock().unwrap().push(event)
    }).unwrap().changed);
    let expected: Vec<u8> = [
        &b"\xEF\xBB\xBF"[..],
        &b"name,memo,age,note\r\n\"Alice\",,30,\"x,y\"\r\n\"bob\nb\",,25,\"say \"\"hi\"\"\"\r\nCarol,,35,plain"[..],
    ].concat();
    assert_eq!(file.backing.bytes(), expected, "列操作后 RAM 必须保持原始字段风格");
    assert_eq!(fs::read(&path).unwrap(), source, "列操作尚未写盘");
    let ram_identity = file.backing_identity();
    assert!(file.save_to_observed(
        &path,
        false,
        Approval::deny(),
        LoadOptions { chunk_bytes: 16, workers: 1, fault: LoadFault::ReadAfter(0) },
        |_, _, _, _| {},
        |event| events.lock().unwrap().push(event),
    ).unwrap().saved);
    assert_eq!(file.backing_identity(), ram_identity);
    assert_eq!(fs::read(&path).unwrap(), expected);
    let events = events.into_inner().unwrap();
    let ready = events.iter().position(|event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::NewRamReady { .. }))).unwrap();
    let released = events.iter().position(|event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::OldRamReleased { .. }))).unwrap();
    let disk = events.iter().position(|event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::DiskWriteBegin { .. }))).unwrap();
    let committed = events.iter().position(|event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::DiskCommitted { .. }))).unwrap();
    assert!(ready < released && released < disk && disk < committed, "{events:?}");
    assert_eq!(events.iter().filter(|event| matches!(event,
        SaveRamEvent::Phase(SaveRamPhase::NewRamReady { .. }))).count(), 1);
    fs::remove_file(&path).ok();
}
```

迁移 `irregular_records_keep_legacy_reload_save_path`，改名 `general_insert_irregular_records_uses_same_serializer_without_temp_reload`，期望磁盘仍为 `a,x,b\r\n1,,\r\n2,,3\r\n`，但没有临时文件重载。`raw_insert_offset_handles_csv_boundaries_and_rejects_irregular_records` 移到 `serialize_tests`，继续逐项验证插入位置和不规则记录回退。

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib general_single_empty_insert_preserves_raw_records_and_never_reloads_temp
cargo test --manifest-path src-tauri/Cargo.toml --lib general_insert_irregular_records_uses_same_serializer_without_temp_reload
```

Expected: 旧双写被通用入口替换前，原始引号夹具或“仅一次物化”断言 FAIL；不得删除字节断言取得 GREEN。

- [x] **Step 2: 在共享序列化内部吸收原始记录投影**

把 `raw_empty_field_insert_offset` 及其引号闭合、字段数、CRLF/LF/无末行换行规则移入 `serialize.rs`。当且仅当操作是规则 CSV 的单个空列插入、输出编码等于后备编码、记录没有私有覆盖时，`serialize_document` 在同一个记录写出分支中按原始偏移依次向当前 `DocumentSink` 写三段字节；不规则记录立即通过同一入口的 `encode_document_record` 回退。该分支服务计数遍和 RAM 遍，不知道临时文件、旧 RAM 释放或换名，不构成第二套保存模型。

- [x] **Step 3: 迁移生成、写盘和换名故障语义**

将两个旧事务测试改成以下新语义，不再断言恢复旧 RAM：

```rust
#[test]
fn general_insert_second_pass_fault_preserves_pending_and_old_ram() {
    let (path, mut bf) = edited_memory_document("general-insert-second-pass-fault");
    bf.push_col_op(ColOp::Insert { pos: 1, name: "empty".into() }).unwrap();
    let before = bf.materialization_snapshot();
    let pending = bf.pending_schema.clone();
    let identity = bf.backing_identity();
    let disk = fs::read(&path).unwrap();
    let input = MaterializationInput::columns(&bf, Approval::deny())
        .unwrap()
        .with_fault(MaterializeFault::CommitAt(64 * 1024));
    let error = bf.prepare_materialization_for_test(input, |_| {}).unwrap_err();
    assert!(error.contains("提交"), "{error}");
    assert_eq!(bf.materialization_snapshot(), before);
    assert_eq!(bf.pending_schema, pending);
    assert_eq!(bf.backing_identity(), identity);
    assert_eq!(fs::read(&path).unwrap(), disk);
    fs::remove_file(&path).ok();
}

#[test]
fn general_insert_rename_fault_keeps_materialized_ram_dirty() {
    let (path, mut bf) = edited_memory_document("general-insert-rename-fault");
    bf.push_col_op(ColOp::Insert { pos: 1, name: "empty".into() }).unwrap();
    bf.apply_col_ops_observed(Approval::deny(), &|_| {}).unwrap();
    let identity = bf.backing_identity();
    let history = bf.history_stats();
    let fixed_point = bf.fixed_point;
    let rows = bf.rows(0, bf.seg.vrow_count() as usize);
    let disk = fs::read(&path).unwrap();
    let target = std::env::temp_dir().join(format!(
        "st_general_insert_rename_directory_{}", std::process::id()));
    fs::create_dir(&target).unwrap();
    let error = bf.save_to_observed(
        &target,
        true,
        Approval::deny(),
        LoadOptions::default(),
        |_, _, _, _| {},
        |_| {},
    ).unwrap_err();
    assert!(error.starts_with(RAM_COMMITTED_ERROR), "{error}");
    assert!(bf.disk_body_dirty && bf.modified());
    assert_eq!(bf.backing_identity(), identity, "重试写盘不得重新物化或读临时文件");
    assert_eq!(bf.history_stats(), history);
    assert_eq!(bf.fixed_point, fixed_point);
    assert_eq!(bf.rows(0, bf.seg.vrow_count() as usize), rows);
    assert_eq!(bf.path, path);
    assert_eq!(bf.backing_path, path);
    assert_eq!(fs::read(&path).unwrap(), disk);
    fs::remove_file(tmp_path_for(&target)).ok();
    fs::remove_dir(&target).ok();
    fs::remove_file(&path).ok();
}
```
- 创建、写入、flush/sync 故障复用 Task 5 的 `DiskWriteFaultGuard`，不得保留 `old_ram_recovery_fault_guard` 或“双失败紧急降级”测试。

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib general_insert
cargo test --manifest-path src-tauri/Cargo.toml --lib memory_save
```

Expected: 所有 03 行为在通用路径通过，失败语义符合 Task 4–6。

- [x] **Step 4: 经用户确认后删除旧实现并收敛模块**

先从 `save_to_observed` 删除 `prepare_insert_save_plan/write_insert_save_plan/finish_insert_save` 分支，删除 `prepare_memory_reopen`、`restore_old_ram_after_save_failure` 及其调用，移除 `mod insert_save` / `mod insert_save_tests`。再从 `ram_region.rs` 删除只被旧快路径使用的 `RamSource` 与平台 `release_before` 支持，移除 `mod streaming_tests`。保留 `RamRegion::append/finish`、并行块写入和既有 `ram_region` 测试。

这些删除实施前先向用户列出上述三个待删文件，取得明确确认；未确认时只完成调用解绑和测试迁移，不擅自删除。

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib ram_region
cargo test --manifest-path src-tauri/Cargo.toml --lib engine::serialize_tests
cargo test --manifest-path src-tauri/Cargo.toml --lib engine::materialize_tests
cargo test --manifest-path src-tauri/Cargo.toml --lib
cargo build --manifest-path src-tauri/Cargo.toml
cargo build --release --manifest-path src-tauri/Cargo.toml --bin simpletable
cargo build --release --manifest-path src-tauri/Cargo.toml --bin memory_peak
```

Expected: 全量 Rust 无新增失败，旧类型引用为零，debug/release 与诊断 exe 编译成功；不执行 Git 提交。

---

### Task 8: 异步列操作 IPC、阶段进度与 RAM 提交后错误重同步

**Files:**
- Modify: `src-tauri/src/lib.rs:1802-1962`
- Modify: `frontend/js/bigmodel.js:193-249`、`:713-726`、`:1742-1817`
- Modify: `frontend/js/app.js:1893-1913`、`:2403-2437`
- Modify: `frontend/js/dialogs.js:291-374`
- Modify: `frontend/js/smoke.js`
- Test: `tests_js/bigmodel.test.mjs`、`tests_js/app-state.test.mjs`、`tests_js/dialogs.test.mjs`

**Interfaces:**
- Consumes: `apply_col_ops_observed`、`SaveRamEvent::Phase`、`RAM_COMMITTED_ERROR`。
- Produces: `big-progress` 的 phase 固定为 `0=计算正文、1=内存整理、2=写入磁盘、3=换名完成`；`big_col_apply` 改为 async command，仍携带 session/lease/approval，不新增事件名。

- [x] **Step 1: 写保存错误和阶段文案的 RED 测试**

在 `bigmodel.test.mjs` 增加自有错误夹具：

```javascript
function makeSaveErrorModel(message) {
  const calls = [];
  const counts = { refreshes: 0, dataEvents: 0 };
  const model = new BigModel({
    invoke: async (cmd) => {
      calls.push(cmd);
      if (cmd === "big_save") throw new Error(message);
      throw new Error("unexpected " + cmd);
    },
    listen: async () => () => {},
  });
  model._sid = 1;
  model._lease = 1;
  model._filterOn = true;
  model._filterCount = 1;
  model._refreshMeta = async () => { counts.refreshes += 1; };
  model._emitData = () => { counts.dataEvents += 1; };
  return { model, calls, counts };
}

test("RAM 提交后磁盘失败刷新新正文视图并保留错误", async () => {
  const { model, counts } = makeSaveErrorModel("RAM_COMMITTED:写入临时文件失败");
  await assert.rejects(model.save(), /RAM_COMMITTED:写入临时文件失败/);
  assert.deepEqual(counts, { refreshes: 1, dataEvents: 1 });
  assert.equal(model.filterActive(), false);
});

test("RAM 提交前保存错误不刷新旧现场", async () => {
  const { model, counts } = makeSaveErrorModel("第二遍正文长度不一致");
  await assert.rejects(model.save(), /第二遍正文长度不一致/);
  assert.deepEqual(counts, { refreshes: 0, dataEvents: 0 });
  assert.equal(model.filterActive(), true);
});
```

在 `dialogs.test.mjs` 从 `dialogs.js` 导出并测试 `BIG_WRITE_PHASES`：

```javascript
assert.deepEqual(BIG_WRITE_PHASES, [
  "正在计算正文…", "正在整理内存…", "正在写入磁盘…", "正在完成原子换名…",
]);
assert.equal(BIG_WRITE_PHASES.some((text) => text.includes("重建行索引")), false);
```

Run:

```bash
node --test tests_js/bigmodel.test.mjs tests_js/dialogs.test.mjs
```

Expected: RAM 提交后错误用例因没有刷新而 FAIL，阶段导出不存在而 FAIL。

- [x] **Step 2: 将列操作命令移到 spawn_blocking 并复用 big-progress**

新增纯映射函数供保存诊断与列操作共用：

```rust
fn save_ram_progress(event: SaveRamEvent) -> Option<(u64, u64, u8, u64)> {
    let SaveRamEvent::Phase(phase) = event;
    let mapped = match phase {
        SaveRamPhase::CountBegin => (0, 1, 0, 0),
        SaveRamPhase::CountFinished { new_bytes, .. } => (new_bytes, new_bytes, 0, 0),
        SaveRamPhase::HistoryPrepared => (0, 1, 1, 0),
        SaveRamPhase::IndexPrepared { record_capacity, .. } => {
            (0, record_capacity as u64, 1, 0)
        }
        SaveRamPhase::NewReserveBegin { new_bytes } => (0, new_bytes, 1, 0),
        SaveRamPhase::NewRamReady { new_bytes } => (new_bytes, new_bytes, 1, 0),
        SaveRamPhase::OldRamReleased { old_bytes } => (old_bytes, old_bytes, 1, 0),
        SaveRamPhase::DiskWriteBegin { new_bytes } => (0, new_bytes, 2, 0),
        SaveRamPhase::DiskWriteFinished { new_bytes } => (new_bytes, new_bytes, 2, 0),
        SaveRamPhase::DiskCommitted { new_bytes } => (new_bytes, new_bytes, 3, 0),
    };
    Some(mapped)
}
```

`big_col_apply` 改为 `async fn`，在与 `run_big_save` 相同的 `spawn_blocking + mpsc + 独立转发线程` 中执行。任务体必须依次 `authorize_big(Write) → bump_gate → lock file → 重新 authorize_big`，然后：

```rust
let result = file.apply_col_ops_observed(approval, &|event| {
    if let Some(update) = save_ram_progress(event) {
        let _ = tx.send(update);
    }
});
drop(tx);
emitter.join().map_err(|error| format!("进度转发线程异常：{error:?}"))?;
result
```

外层 `.await.map_err(|error| format!("列操作任务被取消：{error}"))?`。不得持 `runtime` mutex 等待物化，不得创建 `col-progress` 第二通道；保存已有路径预留和 saved=true 才绑定另存为路径的逻辑保持不变。

- [x] **Step 3: 实现 BigModel 错误重同步和 app dirty/筛选同步**

在 `_invokeSave` 外层捕获两次调用及固定点确认中的错误：

```javascript
async _invokeSave(cmd, args = {}) {
  try {
    const first = await this._invoke(cmd, {
      ...args, approval: { allowFixedPoint: false, preflightId: null },
    });
    if (first.saved) return true;
    if (!first.fixedPoint) return false;
    const confirmed = await this._confirmFixedPoint(first.fixedPoint);
    if (!confirmed) return false;
    const retry = await this._invoke(cmd, {
      ...args,
      approval: { allowFixedPoint: true, preflightId: first.fixedPoint.preflightId },
    });
    if (retry.fixedPoint) throw new Error("固定点预检已过期，请重试");
    return retry.saved === true;
  } catch (error) {
    const message = String(error?.message ?? error);
    if (message.startsWith("RAM_COMMITTED:")) await this._afterWrite();
    throw error;
  }
}
```

`app.js` 的 `runBigWrite` catch 在关闭进度框后执行：

```javascript
const message = String(err?.message ?? err);
setDirty(model.meta()?.modified === true);
if (message.startsWith("RAM_COMMITTED:")) {
  if (hadFilter) fgrid().clearFilters({ anchor: false });
  onCountsChanged(model.rowCount(), model.meta()?.rows ?? model.rowCount());
}
refreshBigUi();
await errorDialog(`${action}失败`, `${message}\n${hint}`);
return false;
```

成功分支仍只在 `saved === true` 后 `dlg.finish()` 和 `setDirty(false)`。`applyColOps` 在 `model.meta()?.memoryMode` 时订阅同一 `onProgress` 并显示“整理内存正文…”，成功文案改为“列操作已应用到内存，尚未保存”；不写“文件已保存”，不调用保存的 dirty 清理。

`dialogs.js` 导出上面的 `BIG_WRITE_PHASES` 并作为默认 `PHASES`，JSDoc 同步为 0–3 四阶段；取消、固定点请求和提交前错误不显示最终 100%。

- [x] **Step 4: 增加真实 WebView2 smoke**

为避免把 64 MiB 夹具经 IPC 整份读入 WebView，在 `lib.rs` 增加与其他 `smoke_*` 夹具命令并列的元数据/4 KiB 前缀探针，并注册到同一 `invoke_handler`：

```rust
#[derive(Serialize)]
struct SmokeFileProbe {
    size: u64,
    mtime_nanos: String,
    prefix: Vec<u8>,
}

/// 冒烟辅助：返回文件大小、精确 mtime 和有限前缀，避免大夹具整份经过 IPC。
#[tauri::command]
fn smoke_file_probe(path: String) -> Result<SmokeFileProbe, String> {
    let metadata = fs::metadata(&path).map_err(|error| io_err(&path, &error))?;
    let mtime_nanos = metadata
        .modified()
        .map_err(|error| io_err(&path, &error))?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos()
        .to_string();
    let mut file = fs::File::open(&path).map_err(|error| io_err(&path, &error))?;
    let mut prefix = vec![0u8; 4096];
    let read = std::io::Read::read(&mut file, &mut prefix)
        .map_err(|error| io_err(&path, &error))?;
    prefix.truncate(read);
    Ok(SmokeFileProbe { size: metadata.len(), mtime_nanos, prefix })
}
```

在现有内存模式 smoke 矩阵中加入完整场景。它通过正常打开、真实确认框、`applyColOps` 和 `saveFile` 走 IPC；RAM 提交后磁盘失败的引擎语义由 Task 5 Rust 注入覆盖，前端 `_afterWrite` 错误重同步由 Step 1 的 BigModel 测试覆盖，不在 WebView2 中虚构平台故障：

```javascript
check("内存模式：列操作只物化 RAM，保存复用新正文", async () => {
  let pending = null;
  try {
    const { path, operationModel } = await openMemoryCsv("columns-ram-only");
    pending = window.__app.enterMemoryMode();
    await confirmMemoryEnterDialog();
    await awaitMemoryEntered(pending);
    pending = null;
    const diskBefore = await invoke("smoke_file_probe", { path });

    await operationModel.insertColumns(1, 1);
    const insertedHeader = operationModel.headers()[1];
    await window.__app.applyColOps();
    await waitFor(() => !operationModel.colOpsPending()
      && operationModel.headers()[1] === insertedHeader
      && operationModel.rawData(0, 1) === "", "列操作后的新 RAM 视图");
    assert(window.__app.isDirty() === true, "列物化后必须保持未保存");
    assert(operationModel.meta()?.modified === true, "引擎也必须报告未保存");
    assert(operationModel.meta()?.ram_backed === true, "列物化不得退回 mmap");
    const diskAfterApply = await invoke("smoke_file_probe", { path });
    assert(JSON.stringify(diskAfterApply) === JSON.stringify(diskBefore),
      "列物化不得改变源文件大小、mtime 或前缀");

    assert(await window.__app.saveFile() === true, "新 RAM 应能直接保存");
    await waitFor(() => !window.__app.isDirty()
      && operationModel.meta()?.modified === false, "保存成功后的干净状态");
    const diskAfterSave = await invoke("smoke_file_probe", { path });
    assert(diskAfterSave.size > diskBefore.size, "插入空列后磁盘正文应增长");
    assert(new TextDecoder().decode(new Uint8Array(diskAfterSave.prefix))
      .includes(insertedHeader), "保存后的磁盘前缀必须包含新列表头");
  } finally {
    await cleanupMemoryScenario(pending);
  }
});
```

Run:

```bash
node --test tests_js/bigmodel.test.mjs tests_js/app-state.test.mjs tests_js/dialogs.test.mjs
cargo test --manifest-path src-tauri/Cargo.toml --lib memory_runtime
cargo build --release --manifest-path src-tauri/Cargo.toml --bin simpletable
.\src-tauri\target\release\simpletable.exe --smoke
```

Expected: Node/Rust 通过，release smoke 无新增失败；报告实际 smoke 数量，不沿用旧 191。前端已嵌入刚编译的 release exe。

验证记录（2026-11-13）：
- 首次真实 smoke 卡在 `memory-mode-save-lifecycle`。根因不是死锁：`prepare_materialization` 的两遍物化把 `serialize_document` 的逐行回调直接接到 `progress`，64 MiB / 4,194,303 行产生 8,388,671 条 `big-progress` 事件；主线程与渲染进程被数百万次 eval 与 DOM 更新淹没，保存响应和后续 IPC 全部排在洪泛之后。
- 修复与回归：`engine/materialize.rs` 的两遍进度改回与磁盘保存路径一致的 `done & 0xF == 0 && progress_gate(last_emit, 80ms)` 节流，并在每个阶段末尾无条件补一条 `done == total` 终态；新增 `materialization_progress_is_throttled_and_reaches_phase_boundaries`（RED 40,000 条 → GREEN）。同一保存的事件数由 8,388,671 降到 69，保存耗时 202 ms。
- 实测：`node --test "tests_js/*.test.mjs"` 268 通过 / 0 失败；`cargo test` 377 通过 + 15 个集成测试通过（4 个忽略）；无探针 release exe 跑完整真实 WebView2 smoke，退出码 0，**合计 192 项，失败 0 项**。

---

### Task 9: 真实私有内存矩阵、阶段计时与 CI/release 硬门槛

**Files:**
- Modify: `src-tauri/src/diagnostics.rs:73-110`
- Modify: `src-tauri/src/bin/memory_peak.rs:52-158`、`:430-627`、`:660-813`
- Modify: `.github/workflows/ci.yml:40-74`、`:114-148`
- Modify: `.github/workflows/release.yml:52-79`

**Interfaces:**
- Consumes: 新 `SaveRamPhase`、真实 `apply_col_ops_observed` / `save_to_observed`。
- Produces: 场景 `save`、`column-apply`、`column-save`、`save-retry`；报告新增 `commit_order_verified`、`phase_timings`、`system_cache_note`，保留 `observed_peak_private_bytes` 和 `working_set_peak_bytes` 独立字段。

- [x] **Step 1: 用新事件顺序替换旧流式断言并写 RED**

把 `save_stream_verified` 改为：

```rust
fn commit_order_verified(
    phases: &[SaveRamPhase],
    expected_materializations: usize,
    require_disk_save: bool,
) -> bool {
    let position = |predicate: &dyn Fn(&SaveRamPhase) -> bool| {
        phases.iter().position(predicate)
    };
    let ready = position(&|phase| matches!(phase, SaveRamPhase::NewRamReady { .. }));
    let released = position(&|phase| matches!(phase, SaveRamPhase::OldRamReleased { .. }));
    let disk = position(&|phase| matches!(phase, SaveRamPhase::DiskWriteBegin { .. }));
    let committed = position(&|phase| matches!(phase, SaveRamPhase::DiskCommitted { .. }));
    let materializations = phases.iter().filter(|phase| {
        matches!(phase, SaveRamPhase::NewRamReady { .. })
    }).count();
    if materializations != expected_materializations
        || !matches!((ready, released), (Some(a), Some(b)) if a < b)
    {
        return false;
    }
    if require_disk_save {
        matches!((ready, released, disk, committed),
            (Some(a), Some(b), Some(c), Some(d)) if a < b && b < c && c < d)
    } else {
        disk.is_none() && committed.is_none()
    }
}
```

加入以下单元测试；`save-retry` 的真实场景对同一文档连续保存两次，期望 `expected_materializations == 1`，并断言第二次磁盘字节与第一次相同：

```rust
#[cfg(test)]
fn ordered_save_phases() -> Vec<SaveRamPhase> {
    vec![
        SaveRamPhase::NewRamReady { new_bytes: 8 },
        SaveRamPhase::OldRamReleased { old_bytes: 8 },
        SaveRamPhase::DiskWriteBegin { new_bytes: 8 },
        SaveRamPhase::DiskCommitted { new_bytes: 8 },
    ]
}

#[test]
fn commit_order_requires_ram_before_disk_and_exact_materialization_count() {
    let valid = ordered_save_phases();
    assert!(commit_order_verified(&valid, 1, true));
    assert!(!commit_order_verified(&valid, 2, true), "重试不得再次物化");

    let mut early_release = ordered_save_phases();
    early_release.swap(0, 1);
    assert!(!commit_order_verified(&early_release, 1, true));
    assert!(!commit_order_verified(&valid[..3], 1, true), "缺 DiskCommitted");
}

#[test]
fn column_apply_commit_order_forbids_all_disk_events() {
    let ram_only = ordered_save_phases()[..2].to_vec();
    assert!(commit_order_verified(&ram_only, 1, false));
    assert!(!commit_order_verified(&ordered_save_phases(), 1, false));
}
```

`Scenario` 的 parse/as_str 增加：

```rust
enum Scenario {
    Convert,
    Cancel,
    CommitFail,
    ReadFail,
    Save,
    ColumnApply,
    ColumnSave,
    SaveRetry,
    CancelLoop,
}

fn requires_memory_mode(scenario: Scenario) -> bool {
    matches!(
        scenario,
        Scenario::Save | Scenario::ColumnApply | Scenario::ColumnSave | Scenario::SaveRetry
    )
}

fn requires_disk_save(scenario: Scenario) -> bool {
    matches!(scenario, Scenario::Save | Scenario::ColumnSave | Scenario::SaveRetry)
}
```

`diagnostics.rs` 增加真实列物化观察入口，普通 `insert_column` 以空 observer 委托它，避免保留绕过新模型的诊断路径：

```rust
pub fn insert_column_with_ram_observer<O>(
    &mut self,
    pos: u32,
    name: String,
    observer: O,
) -> Result<(), String>
where
    O: Fn(SaveRamEvent) + Sync,
{
    self.inner.push_col_op(ColOp::Insert { pos, name })?;
    let outcome = self.inner.apply_col_ops_observed(Approval {
        allow_fixed_point: false,
        preflight_id: None,
    }, &observer)?;
    if outcome.changed { Ok(()) } else { Err("真实插列未产生变化".into()) }
}
```

先定义随真实子进程结果传递的阶段计时；字段全部为 `u128` 毫秒，未发生的阶段保持 0：

```rust
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PhaseTimings {
    pub convert_ms: u128,
    pub count_ms: u128,
    pub ram_generation_ms: u128,
    pub index_prepare_ms: u128,
    pub index_commit_ms: u128,
    pub disk_write_ms: u128,
    pub total_ms: u128,
}
```

把原 `execute_save` 改为 `execute_persistent_operation`，在操作前固定目标路径、源文件状态、首行和复制选项；observer 只收集新 `Phase`，不保留旧流式或重载计数：

```rust
let destination = config.file.with_extension("save.csv");
let source_metadata = std::fs::metadata(&config.file)
    .map_err(|error| format!("读取操作前源文件失败：{error}"))?;
let source_bytes = source_metadata.len();
let source_modified = source_metadata.modified()
    .map_err(|error| format!("读取操作前源文件 mtime 失败：{error}"))?;
let original_first = document.rows(0, 1).into_iter().next()
    .and_then(|row| row.first().cloned())
    .ok_or("持久内存峰值夹具缺少首行首列")?;
let load_options = LoadOptions {
    workers: config.workers,
    fault: LoadFault::None,
    ..LoadOptions::default()
};
let phases = Mutex::new(Vec::with_capacity(32));
let phase_timings = Mutex::new(PhaseTimings::default());
let observe = |event| {
    sampler.sample();
    if let SaveRamEvent::Phase(phase) = event {
        phases.lock().unwrap().push(phase);
    }
};
```

四种场景操作分派固定为：

```rust
let operation_started = std::time::Instant::now();
match config.scenario {
    Scenario::Save => {
        document.set_cell(0, 0, "edited".into())?;
        document.save_with_ram_observer(&destination, load_options, &observe)?;
    }
    Scenario::ColumnApply => {
        document.insert_column_with_ram_observer(1, "note".into(), &observe)?;
    }
    Scenario::ColumnSave => {
        document.insert_column_with_ram_observer(1, "note".into(), &observe)?;
        document.save_with_ram_observer(&destination, load_options, &observe)?;
    }
    Scenario::SaveRetry => {
        document.set_cell(0, 0, "edited".into())?;
        document.save_with_ram_observer(&destination, load_options, &observe)?;
        let first_bytes = std::fs::metadata(&destination)
            .map_err(|error| format!("读取第一次保存失败：{error}"))?.len();
        document.save_with_ram_observer(&destination, load_options, &observe)?;
        let second_bytes = std::fs::metadata(&destination)
            .map_err(|error| format!("读取第二次保存失败：{error}"))?.len();
        if first_bytes != second_bytes {
            return Err(format!("同 RAM 重试文件长度变化：{first_bytes} != {second_bytes}"));
        }
    }
    _ => unreachable!("只有四个持久内存场景进入此函数"),
}
drop(observe);
let phases = phases.into_inner().map_err(|_| "保存事件锁已中毒".to_string())?;
let mut phase_timings = phase_timings.into_inner()
    .map_err(|_| "阶段计时锁已中毒".to_string())?;
phase_timings.total_ms = operation_started.elapsed().as_millis();
let verified = commit_order_verified(
    &phases,
    1,
    requires_disk_save(config.scenario),
);
if !verified {
    return Err(format!("RAM/磁盘提交顺序不符：{phases:?}"));
}
let new_ram_bytes = phases.iter().find_map(|phase| match phase {
    SaveRamPhase::NewRamReady { new_bytes } => Some(*new_bytes),
    _ => None,
}).ok_or("缺少 NewRamReady 字节数")?;
let output_bytes = if requires_disk_save(config.scenario) {
    std::fs::metadata(&destination)
        .map_err(|error| format!("读取磁盘提交后的目标失败：{error}"))?.len()
} else {
    let after = std::fs::metadata(&config.file)
        .map_err(|error| format!("读取列物化后的源文件失败：{error}"))?;
    let after_modified = after.modified()
        .map_err(|error| format!("读取列物化后的源文件 mtime 失败：{error}"))?;
    if after.len() != source_bytes || after_modified != source_modified {
        return Err("执行列操作改变了源文件大小或 mtime".into());
    }
    new_ram_bytes
};
if config.scenario == Scenario::ColumnSave && output_bytes <= source_bytes {
    return Err("真实插列保存后文件未增大".into());
}
let rows = document.rows(0, 1);
let first = rows.first().ok_or("操作后首行消失")?;
match config.scenario {
    Scenario::Save | Scenario::SaveRetry => {
        if first.first().map(String::as_str) != Some("edited") {
            return Err(format!("单元格保存内容错误：{rows:?}"));
        }
    }
    Scenario::ColumnApply | Scenario::ColumnSave => {
        if first.get(..2) != Some(&[original_first, String::new()][..]) {
            return Err(format!("插列后的内容错误：{rows:?}"));
        }
    }
    _ => unreachable!(),
}
Ok(OperationOutcome {
    x_bytes: source_bytes.max(new_ram_bytes).max(output_bytes),
    iterations: 1,
    commit_order_verified: verified,
    phase_timings,
})
```

`execute_operation` 将 `Save | ColumnApply | ColumnSave | SaveRetry` 全部委托给这个函数。`Save` 是单元格编辑后保存；`ColumnApply` 只插列并验证源路径长度和 mtime 不变；`ColumnSave` 插列后立即保存，只允许一次物化；`SaveRetry` 对同一文档连续保存两次，第二次不得再次 `NewRamReady`，并检查保存文件长度不变。磁盘正文与 RAM 的逐字节相等由 Task 5 的真实保存测试验证，本矩阵不把整份文件读入内存制造峰值噪声。`run_child_with_io` 在 READY 前对 `requires_memory_mode(config.scenario)` 的四种场景全部进入内存模式，保证测量的是物化/保存而不是首次转换；只有 `requires_disk_save(config.scenario)` 的三个场景要求磁盘事件，`ColumnApply` 要求零个 `DiskWriteBegin`。

Run:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --bin memory_peak
```

Expected: 新场景和断言尚不存在而 RED；完成接线后 `save-retry` 第二次保存不得再次发出 NewRamReady。

验证记录（2026-11-13）：

- RED：`commit_order_verified` 未定义，新单测编译失败（E0425）。
- GREEN：`cargo test --manifest-path src-tauri/Cargo.toml --bin memory_peak` → 19 passed / 0 failed / 1 ignored。
- 新增真实场景回归：`save_retry_reuses_ram_without_second_materialization`（按 `expected_materializations == 1` 校验，第二次保存重新物化会直接失败）、`column_apply_materializes_ram_without_touching_source_file`（零磁盘事件、源文件大小与 mtime 不变）。
- 修正：`DiagnosticDocument::save_with_ram_observer` 改用另存为语义（`switch_path = true`）。诊断目标与源文件不同，原先 `false` 会让 `commit_disk_success` 把会话保存点记为目标 stamp 而 `path` 仍是源文件，第二次保存被 `stale()` 误判为 `EXTERNAL_CHANGED`；生产 `save()` 的 `false` 只用于 `dest == self.path`，不受影响。
- 语义澄清：夹具首行 `value` 是表头；`save` 场景编辑首个数据行 `0000000`→`edited`，7+LF 与 6+CRLF 等长，故保存文件大小可保持不变，文件增大只由 `column-save` 断言。

- [x] **Step 2: 实现分阶段计时和完整报告**

`OperationOutcome` 去掉 `Copy`，并携带子进程真实测得的计时；`PeakInput` 同步接收，避免报告字段没有数据来源：

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct OperationOutcome {
    x_bytes: u64,
    iterations: u64,
    commit_order_verified: bool,
    phase_timings: PhaseTimings,
}
```

诊断 observer 用 `Instant` 分别记录：`CountBegin→CountFinished`、`NewReserveBegin→NewRamReady`、`HistoryPrepared→IndexPrepared`、`NewRamReady→OldRamReleased`、`DiskWriteBegin→DiskWriteFinished`；转换使用 `ReserveBegin→Finished`，取消/失败时在转换调用返回处结束未闭合区间。同一相位重复出现时累加，不能把总保存耗时填进每个字段。`execute_persistent_operation` 在操作分派前记录 `operation_started = Instant::now()`，得到 `phase_timings` 后执行 `phase_timings.total_ms = operation_started.elapsed().as_millis()`；转换分支同样以整个 `enter_with_optional_cancel` 调用计算 `total_ms`，`CancelLoop` 累加三次。READY 前的持久模式预进入不计入保存场景。

`PeakInput` 和 `PeakReport` 同时增加 `commit_order_verified: bool`、`phase_timings: PhaseTimings`；`PeakInput::test_defaults` 显式设置 `commit_order_verified: true`、`phase_timings: PhaseTimings::default()`。`PeakReport` 另增加：

```rust
pub system_cache_note: String,
```

`run_parent_with_command` 的输入赋值固定为：

```rust
let report = evaluate_peak(PeakInput {
    profile,
    scenario: config.scenario.as_str().into(),
    x_bytes: result.outcome.x_bytes,
    baseline_private_bytes: ready.baseline_private_bytes,
    self_peak_private_bytes: result.self_samples.private_peak_bytes,
    external_peak_private_bytes: external.private_peak_bytes,
    working_set_peak_bytes: result.self_samples.working_set_peak_bytes
        .max(external.working_set_peak_bytes),
    self_sample_count: result.self_samples.count,
    external_sample_count: external.count,
    external_max_interval_ms: external.max_interval_ms,
    commit_order_verified: result.outcome.commit_order_verified,
    phase_timings: result.outcome.phase_timings,
})?;
```

`evaluate_peak` 将最后两个字段原样复制到报告，并写入固定的 `system_cache_note`：“系统文件缓存和备用列表不计入进程私有匿名 RAM；本报告未将其作为应用泄漏”。`observed_peak_private_bytes` 是真实绝对私有峰值，`actual_peak_increment_bytes` 是减基线后的增量，`working_set_peak_bytes` 独立报告；三者不得互相覆盖。

转换类场景只在各分支现有断言全部成功后使用下面的构造器，禁止把原来未检查的 `false` 机械改成 `true`：

```rust
fn checked_conversion_outcome(
    x_bytes: u64,
    iterations: u64,
    phase_timings: PhaseTimings,
) -> OperationOutcome {
    OperationOutcome {
        x_bytes,
        iterations,
        commit_order_verified: true,
        phase_timings,
    }
}
```

`Convert`、`Cancel`、`CommitFail`、`ReadFail` 在各自进入、取消、预期错误和区域无遗留检查之后返回 `Ok(checked_conversion_outcome(x_bytes, 1, phase_timings))`；`CancelLoop` 的三次取消及每次区域检查全部完成后返回 `Ok(checked_conversion_outcome(x_bytes, 3, phase_timings))`。原 M0 地址身份和取消后内容可用性由 Task 2 的引擎测试逐项证明，不用这里的字节采样布尔值替代。这些场景也照常计算 105% 门槛。`pass` 必须同时满足 `actual <= allowed && commit_order_verified`。

- [x] **Step 3: 固定 floor 105% 边界测试**

在 `memory_peak` 测试中加入：

```rust
#[test]
fn peak_gate_uses_floor_105_percent_and_separates_absolute_values() {
    let mut input = PeakInput::test_defaults();
    input.x_bytes = 101;
    input.baseline_private_bytes = 1_000;
    input.self_peak_private_bytes = 1_106;
    input.external_peak_private_bytes = 1_105;
    input.working_set_peak_bytes = Some(9_999);
    let report = evaluate_peak(input).unwrap();
    assert_eq!(report.allowed_peak_increment_bytes, 106);
    assert_eq!(report.observed_peak_private_bytes, 1_106);
    assert_eq!(report.actual_peak_increment_bytes, 106);
    assert_eq!(report.working_set_peak_bytes, Some(9_999));
    assert!(report.pass);

    let mut over = PeakInput::test_defaults();
    over.x_bytes = 101;
    over.baseline_private_bytes = 1_000;
    over.self_peak_private_bytes = 1_107;
    over.external_peak_private_bytes = 1_000;
    let report = evaluate_peak(over).unwrap();
    assert_eq!(report.excess_bytes, 1);
    assert!(!report.pass);
}
```

`commit_order_verified` 明确纳入 `PeakInput`，测试夹具显式设 true；不得把该检查移到报告外人工判断。另加：

```rust
#[test]
fn peak_gate_rejects_bytes_that_pass_without_verified_commit_order() {
    let mut input = PeakInput::test_defaults();
    input.commit_order_verified = false;
    let report = evaluate_peak(input).unwrap();
    assert!(report.actual_peak_increment_bytes <= report.allowed_peak_increment_bytes);
    assert!(!report.commit_order_verified);
    assert!(!report.pass);
}
```

验证记录（2026-11-13）：

- `cargo test --manifest-path src-tauri/Cargo.toml --bin memory_peak` → 21 passed / 0 failed / 1 ignored（含 Step 1 的 19 项）。
- 阶段计时在真实 50 MiB 场景中非零：`save` 的 count/ram_generation/disk_write/total 均 > 0；`column-apply` 的磁盘写入耗时为 0；`convert` 的 `convert_ms > 0` 且 `total_ms >= convert_ms`。
- 计划测试代码中 `actual_peak_increment_bytes`（i128）与 `allowed_peak_increment_bytes`（u64）比较需显式 `as i128`，已按语义修正。

- [x] **Step 4: 本地真实 release 矩阵**

Run:

```bash
set -euo pipefail
cargo build --release --manifest-path src-tauri/Cargo.toml --bin memory_peak
exe="src-tauri/target/release/memory_peak.exe"
base="src-tauri/target/memory-peak-m0-direct"
mkdir -p "$base"
for scenario in save column-apply column-save save-retry; do
  "$exe" run --scenario "$scenario" --file "$base/fixture-64.csv" --workers 4 --preload false --report "$base/$scenario-64-w4.json"
done
```

Expected: 四场景全部 PASS，actual<=floor(X×1.05)、excess=0、事件顺序正确。继续运行既有 convert workers 1/4、preload false/true、cancel、cancel-loop、commit-fail、read-fail，保存报告目录和实际数量；旧十九场景数量不得直接写成新结果。

验证记录（2026-11-13）：

- release 验收器编译通过（21.97s）；报告目录 `src-tauri/target/memory-peak-m0-direct`，共 12 份 JSON，全部 `pass=true`、`commit_order_verified=true`、`excess_bytes=0`。
- 四种持久场景实测：`save` X=67,108,864 / actual=67,235,840；`save-retry` actual=67,239,936；`column-apply` X=75,496,455 / actual=75,653,120；`column-save` actual=75,657,216（allowed 均为 floor(X×1.05)）。
- 阶段计时非零且符合语义：`save` total=350ms（count 100 / ram_generation 126 / index_commit 73 / disk_write 35）；`save-retry` total=383ms（disk_write 70，两次保存累加）；`column-apply` total=715ms 且 `disk_write_ms=0`；`column-save` disk_write=38ms；`convert` 64 MiB w4 preload=false total=9ms、convert_ms=6ms（页缓存已热），w1 为 17ms。
- 既有场景 `cancel/cancel-loop/commit-fail/read-fail` 同样全部 PASS，未出现额外内存占用。

- [x] **Step 5: CI 和 release 阻断**

在 Windows 与 macOS CI 的 `run_case` 后增加：

```bash
run_case save 4 false
run_case column-apply 4 false
run_case column-save 4 false
run_case save-retry 4 false
```

删除原有重复 `run_case save 4 false`。release Windows 同样增加四种场景及 `commit-fail`、`cancel-loop`，所有报告继续 `if: always()` 上传且 `if-no-files-found: error`。macOS CI 使用现有 `sudo -n` 真机采样，不能标记为 Windows 已替代；若本机未运行 macOS，最终文档明确“等待 macOS runner 结果”，不得宣称本地验收通过。

Run:

```bash
git diff --check -- src-tauri/src/diagnostics.rs src-tauri/src/bin/memory_peak.rs .github/workflows/ci.yml .github/workflows/release.yml
node -e "const fs=require('fs'); for (const p of ['.github/workflows/ci.yml','.github/workflows/release.yml']) { const s=fs.readFileSync(p,'utf8'); for (const name of ['save','column-apply','column-save','save-retry']) if (!s.includes('run_case '+name)) throw new Error(p+' 缺少 '+name); }"
```

Expected: 无 whitespace error，工作流包含全部新场景；不执行 Git 提交，不触发远程 release。

验证记录（2026-11-13）：

- `ci.yml`：Windows 与 macOS 两个 job 各自在 `run_case save 4 false` 后追加 `column-apply`/`column-save`/`save-retry`（`replace_all` 一次替换两份相同块，结果每个场景各出现 2 次，无重复 `save`）；macOS 仍走 `sudo -n` 真机采样。
- `release.yml`：test job 增加 `cancel-loop`、`commit-fail`、`column-apply`、`column-save`、`save-retry`；报告上传保持 `if: always()` 与 `if-no-files-found: error`。
- `git diff --check` 通过（仅有 Git 的 LF→CRLF 提示，无 whitespace error）；node 脚本确认 `ci.yml` 每个新场景计数为 2、`release.yml` 为 1。
- 边界：本机未运行 GitHub Actions。macOS 侧的真机采样结果需等待 macOS runner，不得把 Windows 本地矩阵当作 macOS 已验收。

---

### Task 10: 完整 release 界面的无额外预热物理读盘验收

**Files:**
- Create: `scripts/probe-ui-physical-reads.mjs`
- Create: `tests_js/ui_physical_read_probe.test.mjs`
- Test: `tests_js/physical_read_probe.test.mjs`

**Interfaces:**
- Consumes: 当前代码构建的 `src-tauri/target/release/simpletable.exe`、正常 CLI 文件打开、`window.__app.model`、`window.__app.enterMemoryMode()`、现有 `decodeCounterCsv` 和 `parseCsvLine`。
- Produces: `summarizeCounterWindows(text, startAt = null, endAt = null)`、`uiProbeStatus(covered, capacitySufficient, sourceStatus)`；UTF-8 JSON 报告包含独立 baseline/conversion、每盘符 PhysicalDisk、未采集的文件 API/进程 I/O 说明及 `verified_zero | reads_observed | unverified`。

- [x] **Step 1: 先写采样窗口和最终状态的失败测试**

创建 `tests_js/ui_physical_read_probe.test.mjs`：

```javascript
import test from "node:test";
import assert from "node:assert/strict";
import {
  summarizeCounterWindows,
  uiProbeStatus,
} from "../scripts/probe-ui-physical-reads.mjs";

const HEADER = String.raw`"(PDH-CSV 4.0)","\\HOST\PhysicalDisk(2 C:)\Disk Read Bytes/sec","\\HOST\PhysicalDisk(0 E:)\Disk Read Bytes/sec","\\HOST\PhysicalDisk(_Total)\Disk Read Bytes/sec"`;
const at = (value) => new Date(value).getTime();
const startAt = at("2026-11-13T20:00:01.000");
const endAt = at("2026-11-13T20:00:03.000");
const csv = `${HEADER}\n"11/13/2026 20:00:00.000","1.0","0.0","1.0"\n"11/13/2026 20:00:01.000","2.0","0.0","2.0"\n"11/13/2026 20:00:02.000","3.0","0.0","3.0"\n"11/13/2026 20:00:03.000","4.0","0.0","4.0"\n"11/13/2026 20:00:04.000","5.0","0.0","5.0"`;

test("conversion window reports source and other drives separately", () => {
  const result = summarizeCounterWindows(csv, startAt, endAt);
  assert.equal(result.covered, true);
  assert.equal(result.drives["E:"].status, "zero");
  assert.equal(result.drives["E:"].readBytes, 0);
  assert.equal(result.drives["E:"].sampleCount, 5);
  assert.equal(result.drives["C:"].status, "observed");
  assert.equal(result.drives["C:"].readBytes, 15);
  assert.equal(result.drives._Total, undefined);
  assert.equal(result.fileApi.status, "not_collected");
  assert.equal(result.processIo.status, "not_collected");

  const baseline = summarizeCounterWindows(csv);
  assert.equal(baseline.covered, true);
  assert.equal(baseline.drives["E:"].sampleCount, 5);
});

test("conversion window includes boundary buckets and rejects missing coverage", () => {
  const truncated = `${HEADER}\n"11/13/2026 20:00:01.000","0.0","0.0","0.0"\n"11/13/2026 20:00:02.000","0.0","0.0","0.0"`;
  const result = summarizeCounterWindows(truncated, startAt, endAt);
  assert.equal(result.covered, false);
  assert.equal(result.drives["E:"].status, "unverified");
  assert.equal(result.drives["E:"].readBytes, null);
});

test("physical reads remain reads_observed rather than being attributed to the app", () => {
  const nonzero = csv.replaceAll('"0.0","0.0"', '"8.0","8.0"');
  const result = summarizeCounterWindows(nonzero, startAt, endAt);
  assert.equal(result.drives["E:"].status, "observed");
  assert.match(result.drives["E:"].detail, /不能据此归因/);
  assert.equal(uiProbeStatus(true, true, "observed"), "reads_observed");
});

test("final status requires coverage, sufficient available memory and zero source reads", () => {
  assert.equal(uiProbeStatus(true, true, "zero"), "verified_zero");
  assert.equal(uiProbeStatus(false, true, "zero"), "unverified");
  assert.equal(uiProbeStatus(true, false, "zero"), "unverified");
  assert.equal(uiProbeStatus(true, true, "unverified"), "unverified");
});
```

Run:

```bash
node --test tests_js/ui_physical_read_probe.test.mjs tests_js/physical_read_probe.test.mjs
```

Expected: 新测试因 `probe-ui-physical-reads.mjs` 不存在而 RED；旧探针测试仍通过。

- [x] **Step 2: 实现真实 WebView2 自动化、分盘采样和只属于本脚本的清理**

创建 `scripts/probe-ui-physical-reads.mjs`。实现不得调用 `memory_peak`、`preload_source`、`openPathsAsTabs` 或任何文件内容预热接口；产品正常启动参数只携带源文件路径。完整脚本如下：

```javascript
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
```

WebView2 的远程调试端口由脚本随机申请并只连接 `127.0.0.1`；它是验收脚本的外部自动化手段，不进入产品代码或发布配置。基线和转换各写独立 typeperf CSV；转换窗口包含点击前后的完整采样桶，宁可多计边界读取，也不截取较好看的零区间。文件 API 与进程 I/O 明确标为 `not_collected`，不能拿空字段或 PhysicalDisk 数值冒充这两类指标。

- [x] **Step 3: 运行解析测试并确认 GREEN**

Run:

```bash
node --test tests_js/ui_physical_read_probe.test.mjs tests_js/physical_read_probe.test.mjs
```

Expected: 全部通过；测试只使用合成 CSV 文本，不启动应用、不访问授权大文件。

验证记录（2026-11-13）：

- Step 1 RED：`ERR_MODULE_NOT_FOUND`（`scripts/probe-ui-physical-reads.mjs` 不存在），旧 `physical_read_probe.test.mjs` 6 项仍通过。
- 计划给的“有读取”夹具生成方式 `csv.replaceAll('"0.0","0.0"', ...)` 在合成数据中不生效：`_Total` 列与 C: 同值，E: 后面紧跟的是 `"1.0"` 等而非 `"0.0"`。改为精确命中 E: 列 `csv.replaceAll(',"0.0",', ',"8.0",')`，保留“E: 非零必须记为 observed 且不能归因”的原意。
- Step 3 GREEN：`node --test` 共 10 passed / 0 failed（新增 4 项 + 旧探针 6 项）。

- [x] **Step 4: 构建当前完整 release 并执行真实界面验收**

确认本地 exe 属于当前代码，前端改动后必须重编：

```bash
cargo build --release --manifest-path src-tauri/Cargo.toml --bin simpletable
report_dir="src-tauri/target/ui-physical-read-$(date +%s)"
node scripts/probe-ui-physical-reads.mjs \
  --exe src-tauri/target/release/simpletable.exe \
  --file 'E:\simpletable_hdd_test\bench_22gb.csv' \
  --seconds 180 \
  --output-dir "$report_dir"
```

Expected: 脚本通过正常 release 界面打开并等待完整索引，未额外预热；报告包含独立基线、实际转换窗口、源盘和其他盘符。退出码 0/1/2 分别对应 `verified_zero`、`reads_observed`、`unverified`，不得为了得到零而改判状态。若使用独立合成副本，额外传 `--synthetic-copy yes`，且仍不得使用 `price_j8888.csv`。

保存 `physical-read.json` 和两份 typeperf CSV。若为 `reads_observed`，核心读盘目标未通过，回到 systematic-debugging 继续定位；若为 `unverified`，补齐采样/平台证据后重跑；旧 `memory_peak child --preload true` 报告不能替代本步骤。主动保存不在本探针中执行，因此没有磁盘写入混入首次转换窗口。

验证记录（2026-11-13）：

- release `simpletable.exe` 用当前代码重编（21.14s，2026-11-13 17:51）。
- 真实界面探针退出码 0，状态 `verified_zero`；报告目录 `src-tauri/target/ui-physical-read-task10-20261113`（`physical-read.json` + 两份 typeperf CSV）。
- `startup=normal-release-cli-open`、`explicitPreload=false`、`indexingCompleted=true`、`covered=true`、`capacitySufficient=true`、`conversionMs=797`。
- 源盘 E: 在点击→完成窗口内 `readBytes=0`、`sampleCount=2`、`status=zero`；其他盘符独立报告：C: 有读取（`observed`，明确标注不能据此归因于 simpletable），D:/F:/G: 为零。
- `fileApi` 与 `processIo` 保持 `not_collected`；未使用额外预热接口，旧 `memory_peak --preload true` 报告未用于本结论。

---

### Task 11: 全量回归、双构建双 smoke、文档与交付边界

**Files:**
- Modify: `README.md`
- Modify: `docs/guide.md`
- Modify: `docs/development.md`
- Modify: `docs/superpowers/specs/2026-11-12-mmap-chunk-views-design.md`
- Modify: `docs/superpowers/plans/2026-11-12-mmap-chunk-views.md`
- Modify: `docs/superpowers/specs/2026-11-13-m0-direct-copy-ram-materialization-design.md`
- Modify: `TODO.md`

**Interfaces:**
- Consumes: Task 1–10 的实际测试输出、峰值 JSON、canonical smoke 报告和真实界面物理盘 JSON。
- Produces: 与当前实现一致的用户/开发文档和可核对的验收记录；未执行项保持未勾选，不伪造 macOS 或物理盘结论。

- [x] **Step 1: 先运行完整 Rust 与 Node 回归**

Run:

```bash
set -o pipefail
mkdir -p src-tauri/target/m0-direct-final
cargo test --manifest-path src-tauri/Cargo.toml --lib 2>&1 | tee src-tauri/target/m0-direct-final/rust-lib.txt
cargo test --manifest-path src-tauri/Cargo.toml --bin memory_peak 2>&1 | tee src-tauri/target/m0-direct-final/rust-memory-peak.txt
node --test tests_js/*.test.mjs 2>&1 | tee src-tauri/target/m0-direct-final/node-tests.txt
```

Expected: 所有命令退出码 0，失败数均为 0；记录 fresh 输出的 passed/failed/ignored 实际数量。若有失败，先修复并重跑，不沿用旧 `364 + 263` 基线，不把 warning 写成新测试失败。

验证记录（2026-11-13）：

- Rust lib：377 passed / 0 failed / 3 ignored（退出码 0）。
- memory_peak bin：21 passed / 0 failed / 1 ignored（退出码 0）。
- Node：272 passed / 0 failed（退出码 0）。
- 原始输出保存在 `src-tauri/target/m0-direct-final/rust-lib.txt`、`rust-memory-peak.txt`、`node-tests.txt`。

- [x] **Step 2: 执行 canonical debug/release 双构建、嵌入检查和双 smoke**

Run:

```bash
set -o pipefail
node scripts/verify-local.mjs 2>&1 | tee src-tauri/target/m0-direct-final/verify-local.txt
node -e "const fs=require('fs'); const paths=['src-tauri/target/debug/simpletable.exe','src-tauri/target/release/simpletable.exe','src-tauri/target/release/memory_peak.exe']; for (const p of paths) { const s=fs.statSync(p); console.log(p, new Date(s.mtimeMs).toISOString(), s.size); }"
```

Expected: canonical 脚本输出 `dual build, embed timestamps, and dual smoke: PASS`；debug/release exe 时间戳均推进且不早于最新前端文件，debug/release 两份 smoke 报告 ✓/✗ 行数与唯一汇总一致且失败为 0。`memory_peak.exe` 也必须是 Task 9 当前代码构建；若时间戳不属于当前改动，重编后重跑矩阵。记录实际 smoke 数量，不写旧 191。

验证记录（2026-11-13）：

- `node scripts/verify-local.mjs` 输出 `dual build, embed timestamps, and dual smoke: PASS`，退出码 0。
- debug smoke：192 项、失败 0；release smoke：192 项、失败 0（报告见 `src-tauri/target/smoke-debug-*.txt` 与 `smoke-release-*.txt`）。
- exe 时间戳：debug `2026-11-13T09:53:02.836Z`（21,133,824 字节），release `2026-11-13T09:53:24.991Z`（13,811,200 字节）。
- 随后仅对本期的 `memory_peak.rs` 执行 rustfmt 并重编，`memory_peak.exe` 更新为 17:58 本地时间，复测 21 passed / 0 failed / 1 ignored。

- [x] **Step 3: 汇总真实峰值矩阵和界面物理盘结果**

Run:

```bash
node -e "const fs=require('fs'),path=require('path'); const roots=['src-tauri/target/memory-peak-m0-direct','src-tauri/target/m0-direct-final']; for (const root of roots) { if (!fs.existsSync(root)) continue; for (const name of fs.readdirSync(root).filter(n=>n.endsWith('.json'))) { const r=JSON.parse(fs.readFileSync(path.join(root,name),'utf8')); console.log(name, JSON.stringify({scenario:r.scenario,pass:r.pass,allowed:r.allowed_peak_increment_bytes,actual:r.actual_peak_increment_bytes,absolute:r.observed_peak_private_bytes,workingSet:r.working_set_peak_bytes,commitOrder:r.commit_order_verified})); } }"
```

Expected: 所有发布硬门槛场景 `pass=true`、`actual<=floor(X×1.05)`、`excess_bytes=0`、`commit_order_verified=true`；绝对私有峰值、增量、工作集分别列出。Task 10 的界面报告必须来自当前 release exe，`startup=normal-release-cli-open`、`explicitPreload=false`、`indexingCompleted=true`。只有源盘转换窗口 `verified_zero` 才能把核心读盘目标标为通过；`reads_observed` 或 `unverified` 必须在 TODO 和最终交付中保留未完成，不得用旧预热报告替换。

验证记录（2026-11-13）：

- 峰值矩阵 12 个场景全部 `pass=true`、`excess_bytes=0`、`commit_order_verified=true`；最小余量 3,035,955 字节（convert-64-w4-preload-false）。
- 绝对私有峰值与工作集按场景单列（示例：save absolute 240,340,992 / workingSet 242,544,640；column-apply absolute 247,087,104 / workingSet 249,942,016）。
- Task 10 界面报告来自当前 release exe（探针运行时 exe mtime 17:51，之后源码仅文档与 rustfmt 变化），`startup=normal-release-cli-open`、`explicitPreload=false`、`indexingCompleted=true`、状态 `verified_zero`；没有 `reads_observed` 或 `unverified` 需要保留为未完成项。

- [x] **Step 4: 同步用户文档和开发文档**

在 `README.md` 的内存模式说明中写明：首次转换直接借用原 M0 复制，成功后一次释放；持久内存列操作只整理 RAM 并保持未保存；保存先生成并提交新 RAM，再写临时文件、flush/sync 和换名；磁盘失败保留新 RAM 和未保存状态，重试不重复物化未变化正文。将下面的使用边界加入 `docs/guide.md`：

```markdown
### 持久内存模式的保存边界

- “执行列操作”只把新正文整理到内存，不修改源文件；状态栏仍显示未保存。
- 保存成功后才更新磁盘保存点。写盘或换名失败时，界面显示的是已经提交的新内存正文，旧正文不会重新覆盖界面。
- 写盘失败后可直接重试保存；正文未再变化时，不会重新生成整份内存正文。
- 只存在于新 RAM 且尚未写盘的正文会阻止直接退出内存模式，请先保存，避免退回旧磁盘正文。
```

在 `docs/development.md` 加入两段提交顺序和内存口径：

```markdown
首次 mmap → RAM 转换在目标预留及整个复制期间保留原 M0，不建立独立块视图，也不主动整份 read_at；全部复制和文件状态检查成功后才安装 RAM 并一次释放 M0。取消或失败不运行重映射恢复。

正文变化需要重新物化时，同一 serialize_document 入口执行计数遍和 RAM 生成遍。CountFinished 后准备历史、固定点与索引容量，NewRamReady 后才允许 RAM 提交和 OldRamReleased；DiskWriteBegin 只能随后发生。RAM 提交后的创建、写入、flush/sync、关闭、换名和外部变化错误均带 RAM_COMMITTED:，保留新 RAM、未保存状态及原路径。03 单空列不再拥有独立流式双写事务。

私有内存增量硬门槛为 floor(X × 1.05)。observed_peak_private_bytes 是绝对私有峰值，actual_peak_increment_bytes 是减基线增量，working_set_peak_bytes 独立报告；系统文件缓存和备用列表只作口径说明。阶段计时分别记录转换、计数、RAM 生成、索引准备/提交和磁盘写入。
```

更新旧块视图规格/计划的当前状态说明：其 2026-11-12 结果保留为历史验收，但“预建独立块视图”和预热探针已被本期 M0 直拷及真实界面验收取代，不能描述当前实现或证明真实界面零读盘。不要删除或改写旧报告中的历史数值。

验证记录（2026-11-13）：

- `README.md` 内存模式亮点改为 M0 直拷、一次释放、列操作只整理内存、保存先 RAM 提交后写盘、失败保留新 RAM 且重试不重复物化。
- `docs/guide.md` 新增“持久内存模式的保存边界”四条；原“规则插列保存快路径”改写为“列操作与保存”，不再把 03 独立流式双写描述为当前行为。
- `docs/development.md` 用 M0 直拷与两段提交顺序替换旧块视图描述，并补充 105% 硬门槛、绝对峰值/增量/工作集口径与阶段计时。
- 旧规格 `2026-11-12-mmap-chunk-views-design.md` 与旧计划 `2026-11-12-mmap-chunk-views.md` 顶部加入历史状态说明，历史数值未改动。

- [x] **Step 5: 写入本期实际验收记录并核对未完成边界**

在新规格末尾追加“实施验收记录（2026-11-13）”，每项只根据 Step 1–3 的 fresh 结果填写，格式固定为：

```markdown
### 实施验收记录（2026-11-13）

- Rust lib：<实际 passed/failed/ignored>；memory_peak bin：<实际结果>；Node：<实际 passed/failed>。
- canonical debug/release：<实际构建与 smoke 数量>；exe 时间戳：<实际 ISO 时间>。
- 私有峰值：<报告目录、场景总数、最小余量>；commit_order_verified：<实际结果>。
- release 界面物理盘：<报告路径、状态、源盘和其他盘符结果>；未使用额外预热：<true/false>。
- macOS：<已运行时的 runner/结果；未运行时写“未验证，Windows 结果不能替代”>。
- 全仓 cargo fmt --check：<实际执行结果；未执行或既有格式问题须明说，不得写通过>。
```

`TODO.md` 新增本期 Task 14，逐项对应规格第 14 节十项完成判定；仅把证据齐全的项标 `[x]`。若 macOS、真实界面零读盘或任一全量验证未完成，对应项保留 `[ ]`，最终交付明确剩余阻塞，不能宣称本期全部完成。README/开发文档也不保留已删除的 `insert_save` 流式释放顺序作为当前行为。

验证记录（2026-11-13）：

- 新规格末尾已追加“实施验收记录（2026-11-13）”，六项均按 Step 1–3 的 fresh 结果填写；`cargo fmt --check` 明确写为未通过及既有差异文件，未宣称通过。
- `TODO.md` 新增 Task 14 十项：1–7、9 与 8（全量验证）已勾选；第 10 项因 macOS 真机未运行保留 `[ ]`，并注明“Windows 结果不能替代”。
- README/guide/development 已删除旧的 03 流式双写释放顺序作为当前行为的表述。

- [x] **Step 6: 最终范围、文本和工作区检查**

Run:

```bash
git diff --check
git status --short
node --test tests_js/ui_physical_read_probe.test.mjs tests_js/physical_read_probe.test.mjs
```

Expected: `git diff --check` 无 whitespace error；界面探针解析测试通过；工作区只包含本计划明确文件及用户原有未提交内容。逐项核对未误改主工作区、未读取/提交 `price_j8888.csv`、未删除用户进程、未 reset/stash/覆盖无关改动。未经用户明确要求，不提交、不推送、不创建标签或 PR；旧文件删除已单独获得确认，否则保留到获确认后再收敛。

验证记录（2026-11-13）：

- `git diff --check` 退出码 0，仅有 Git 的 LF→CRLF 提示，无 whitespace error。
- `node --test tests_js/ui_physical_read_probe.test.mjs tests_js/physical_read_probe.test.mjs`：10 passed / 0 failed。
- 工作区改动均为本计划文件与用户原有未提交内容；未产生新提交（HEAD 仍为 `da2daba`），未推送、未打标签、未建 PR。
- `TODO.md` 被仓库 `.gitignore` 忽略（`.gitignore:13`），本期 Task 14 仅保存在本地。
- 源码中无残留 `[diag]` 探针；`src-tauri/target/` 下早前计划留下的验证脚本不属于本期诊断探针，未擅自删除。
