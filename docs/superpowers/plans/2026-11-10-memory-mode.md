# 大文件内存模式 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为大文件增加可持久保持、可取消退出的私有内存模式，以有界多核转换和真实进程私有内存硬验收证明转换增量不超过 `floor(X × 1.05)`。

**Architecture:** 新增连续私有区域和两阶段分块载入器。Windows 先预留再逐块提交，macOS 使用匿名映射；引擎在预留成功后、读取开始前解除源映射，保留索引和 `index_epoch`。独立 `memory_mode` 管理持久生命周期，保存时先物化旧 RAM 依赖再释放旧 RAM，前端使用文档级非模态进度。

**Tech Stack:** Rust 2021、Tauri 2、memmap2、scoped threads、windows-sys、libc、原生 JavaScript、Node.js `node:test`、Windows WebView2、macOS 15 arm64 CI、JSON 峰值报告。

## Global Constraints

- 执行时从 `main` 创建 `feat/memory-mode`，不在 `main` 直接实施；不推送、不建 PR 或标签。
- 严格遵循 `docs/superpowers/specs/2026-11-10-memory-mode-design.md`；第一阶段不改变插入列、删除列、执行列操作和 Ctrl+Z 语义。
- 初次转换硬上限为 `floor(X × 1.05)`；保存路径的 X 取保存前后文件大小的较大者。真实进程私有字节超出即失败，禁止改成 106% 或 110%。
- 转换块默认 8 MiB，实际并行度为 `min(available_parallelism, 4)`，内部余量固定 50‰；这些不是用户设置。
- 不调用 `VirtualLock` 或 `mlock`，不把工作集、系统可用内存或 macOS `phys_footprint` 当作私有字节替代证据。
- 普通撤销预算保留 1–1024 MiB、默认 64 MiB；驻留比例默认 100%，范围 0%–1000%，步长 0.1%，内部 100% = 1000‰、最大值 10000‰。
- 设置页直接显示两条有效预算公式和当前文档实时试算；预算只是历史上限，不预分配。
- 当前文档转换时其他页签和窗口保持可编辑；禁止覆盖整窗的模态进度框。
- 只用合成夹具。大文件基准只能只读使用 `E:\simpletable_hdd_test\bench_22gb.csv`；不得读取、修改、提交或试验 `price_j8888.csv`，不得运行会打开它的 park 模式。
- 每次产品改动后重编 exe；前端嵌入 release，最终必须运行 `node scripts/verify-local.mjs` 和 release 真实峰值验收。
- Git 作者和提交者只能使用用户本人现有 `user.name` / `user.email`；禁止 AI、第三方和任何 `Co-Authored-By` 尾注。
- 不删除或提交既有未跟踪计划、报告和调试杂项；删除文件须先获用户确认。执行本计划列出的文件修改前仍遵循用户的确认规则。
- 新函数使用 Rust doc comment 或 JavaScript JSDoc 写明参数、返回值和错误；文件统一 UTF-8，保留原换行风格。
- 每项测试先亲见功能缺失导致的预期 RED；测试编译错误先修测试，不算有效 RED。

## File Map

- Create `src-tauri/src/ram_region.rs`：连续私有区域、逐块启用、不相交并行写入、类型状态完成态。
- Create `src-tauri/src/ram_load.rs`：两阶段定位读取、取消、平台中性事件、进度和受控故障注入。
- Create `src-tauri/src/process_memory.rs`：Windows/macOS 私有字节、工作集、可用内存和匿名区域审计。
- Create `src-tauri/src/diagnostics.rs`：仅供独立验收二进制调用的隐藏引擎入口，不复制转换算法。
- Create `src-tauri/src/bin/memory_peak.rs`：独立子进程、5 ms 外部监测、覆盖矩阵和 JSON 硬断言。
- Modify `src-tauri/src/engine.rs`：双 RAM 后备、索引线程完成确认、持久状态、动态预算、保存和排序生命周期。
- Modify `src-tauri/src/history.rs`：无副作用缩容预检、自动淘汰重建和 `MemoryModeExit` 固定点。
- Modify `src-tauri/src/lib.rs`：运行时双设置、原子准备/提交、IPC、取消和定向事件。
- Modify `src-tauri/Cargo.toml` / `Cargo.lock`：加入 Windows FFI 与 macOS libc 直接依赖。
- Modify `frontend/js/prefs.js`、`bigmodel.js`、`dialogs.js`：版本化设置、模型 IPC、公式与实时试算。
- Modify `frontend/js/app.js`、`windowlifecycle.js`：工具栏、文档级任务、状态栏、排序提示和关闭等待。
- Modify `tests_js/helpers/shared_bigmodel_fake.mjs`、`tests_js/*.test.mjs`：共享 fake 和逻辑回归。
- Modify `frontend/js/smoke.js`：独立大文件夹具及真实 WebView2 生命周期测试。
- Modify `scripts/verify-local.mjs`：仅在新增 smoke 需要时提高超时，保留完整报告校验。
- Modify `.github/workflows/ci.yml`、`.github/workflows/release.yml`：真实内存硬门槛和报告 artifact。
- Modify `README.md`、`docs/guide.md`、`docs/development.md`、`TODO.md`、`task_plan.md`、`progress.md`、`findings.md`：用法、设计、验证命令和实测结果。

---

### Task 1: 连续私有内存区域与安全完成态

**Files:**
- Create: `src-tauri/src/ram_region.rs`
- Modify: `src-tauri/src/lib.rs` 模块声明区
- Modify: `src-tauri/Cargo.toml`、`src-tauri/Cargo.lock`
- Test: `src-tauri/src/ram_region.rs` 内置 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: Windows `VirtualAlloc/VirtualFree/VirtualQuery`；macOS `libc::mmap/munmap`。
- Produces: `RamRegion::reserve`、`reserve_with_enable_fault`、`for_each_chunk_mut`、`finish`；`FinishedRam::{as_slice,len,as_ptr}`；`RegionEvent`；`live_reserved_bytes()`。
- Safety: 只有所有不相交块写入成功的 `FinishedRam` 能暴露 `&[u8]`；写入中区域没有安全只读切片。

- [ ] **Step 1: 先写不相交写入、完成态、故障和真实提交测试**

```rust
#[test]
fn parallel_chunks_cover_every_byte_and_finish() {
    let len = 4 * 1024 * 1024 + 123;
    let mut region = RamRegion::reserve(len).unwrap();
    region.for_each_chunk_mut(64 * 1024, 4, |_| {}, |start, out| {
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = ((start + i) % 251) as u8;
        }
        Ok(())
    }).unwrap();
    let finished = region.finish().unwrap();
    assert_eq!(finished.len(), len);
    assert!(finished.as_slice().iter().enumerate()
        .all(|(i, b)| *b == (i % 251) as u8));
}

#[test]
fn failed_chunk_cannot_finish() {
    let mut region = RamRegion::reserve(1024 * 1024).unwrap();
    let error = region.for_each_chunk_mut(64 * 1024, 2, |_| {}, |start, _| {
        if start == 128 * 1024 { Err("injected write".into()) } else { Ok(()) }
    }).unwrap_err();
    assert!(error.contains("injected write"));
    assert!(region.finish().unwrap_err().contains("尚未完整写入"));
}

#[cfg(windows)]
#[test]
fn windows_reserve_is_not_committed_and_chunks_commit_monotonically() {
    use windows_sys::Win32::System::Memory::{VirtualQuery, MEMORY_BASIC_INFORMATION,
        MEM_COMMIT, MEM_FREE};
    fn committed(mut address: usize, end: usize) -> usize {
        let mut total = 0;
        while address < end {
            let mut info = MEMORY_BASIC_INFORMATION::default();
            let size = unsafe { VirtualQuery(address as _, &mut info,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
            assert_ne!(size, 0);
            if info.State == MEM_COMMIT { total += info.RegionSize; }
            address += info.RegionSize;
        }
        total
    }
    let len = 2 * 1024 * 1024 + 17;
    let mut region = RamRegion::reserve(len).unwrap();
    let base = region.as_ptr() as usize;
    assert_eq!(committed(base, base + len), 0);
    region.for_each_chunk_mut(64 * 1024, 1, |_| {}, |_, out| {
        out.fill(7);
        Ok(())
    }).unwrap();
    let finished = region.finish().unwrap();
    assert!(committed(base, base + len) >= len);
    let base = finished.as_ptr();
    drop(finished);
    let mut info = MEMORY_BASIC_INFORMATION::default();
    unsafe { VirtualQuery(base as _, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()); }
    assert_eq!(info.State, MEM_FREE);
}
```

全局 `live_reserved_bytes()` 不在并行 `cargo test` 中做绝对前后相等断言；精确释放断言由这里的 `VirtualQuery` 和 Task 10 的独立子进程完成，避免其他测试并发分配造成假失败。

- [ ] **Step 2: 确认 RED**

Run: `cd src-tauri && cargo test ram_region -- --nocapture`

Expected: 先因缺少模块和类型失败；加入空模块后，失败点必须是缺少 `RamRegion`、`FinishedRam` 和 `RegionEvent`。

- [ ] **Step 3: 加入平台依赖并实现类型状态区域**

在 `Cargo.toml` 增加：

```toml
[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.59", features = [
  "Win32_Foundation",
  "Win32_System_Memory",
  "Win32_System_ProcessStatus",
  "Win32_System_SystemInformation",
  "Win32_System_Threading",
] }

[target.'cfg(target_os = "macos")'.dependencies]
libc = "0.2"
```

核心类型和调度接口如下；`reserve_with_enable_fault` 是隐藏诊断入口，生产 UI 永远传 `None`，用于 release 失败恢复验收，不替代真实分配。

```rust
use std::fmt;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

static LIVE_RESERVED_BYTES: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionEvent {
    WorkerBegin { worker: usize },
    WorkerEnd { worker: usize },
    BeforeEnable { offset: usize, len: usize },
    Enabled { offset: usize, len: usize },
}

pub struct RamRegion {
    base: NonNull<u8>,
    len: usize,
    enabled_bytes: AtomicUsize,
    written_bytes: AtomicUsize,
    started: bool,
    enable_fault_at: Option<usize>,
}

pub struct FinishedRam(RamRegion);

impl fmt::Debug for FinishedRam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FinishedRam").field("len", &self.len()).finish()
    }
}

// SAFETY: 安全并行入口只发放唯一原子下标对应的不相交范围，所有线程在返回前 joined。
unsafe impl Send for RamRegion {}
unsafe impl Sync for RamRegion {}

/// 返回本模块当前持有的预留字节数；只用于隔离诊断，不作并行单测的全局断言。
pub fn live_reserved_bytes() -> usize { LIVE_RESERVED_BYTES.load(Ordering::Relaxed) }

impl RamRegion {
    /// 预留连续地址；失败不触碰调用方现有后备。
    pub fn reserve(len: usize) -> Result<Self, String> {
        Self::reserve_with_enable_fault(len, None)
    }

    /// 在指定偏移注入一次提交失败；仅供受控诊断和恢复测试。
    #[doc(hidden)]
    pub fn reserve_with_enable_fault(len: usize, enable_fault_at: Option<usize>)
        -> Result<Self, String>
    {
        if len == 0 { return Err("不能预留 0 字节内存区域".into()); }
        let base = os::reserve(len)?;
        LIVE_RESERVED_BYTES.fetch_add(len, Ordering::Relaxed);
        Ok(Self { base, len, enabled_bytes: AtomicUsize::new(0),
            written_bytes: AtomicUsize::new(0), started: false, enable_fault_at })
    }

    pub fn len(&self) -> usize { self.len }
    pub fn as_ptr(&self) -> *const u8 { self.base.as_ptr() }
    pub fn enabled_bytes(&self) -> usize { self.enabled_bytes.load(Ordering::Relaxed) }

    /// 用有界 scoped threads 写不相交块；错误停止调度并等待在途写入退出。
    pub fn for_each_chunk_mut<F, O>(&mut self, chunk_bytes: usize, workers: usize,
        observer: O, writer: F) -> Result<(), String>
    where
        F: Fn(usize, &mut [u8]) -> Result<(), String> + Sync,
        O: Fn(RegionEvent) + Sync,
    {
        if self.started { return Err("内存区域已经开始写入".into()); }
        if chunk_bytes == 0 || workers == 0 { return Err("块大小和并行度必须大于 0".into()); }
        self.started = true;
        let shared = &*self;
        let next = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let error = Mutex::new(None::<String>);
        let count = workers.min(self.len.div_ceil(chunk_bytes)).max(1);
        let record = |text: String| {
            stop.store(true, Ordering::Relaxed);
            let mut slot = error.lock().unwrap();
            if slot.is_none() { *slot = Some(text); }
        };
        std::thread::scope(|scope| {
            for worker in 0..count {
                let writer = &writer;
                let observer = &observer;
                let record = &record;
                let next = &next;
                let stop = &stop;
                scope.spawn(move || {
                    observer(RegionEvent::WorkerBegin { worker });
                    loop {
                        if stop.load(Ordering::Relaxed) { break; }
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(start) = index.checked_mul(chunk_bytes) else {
                            record("内存块偏移溢出".into()); break;
                        };
                        if start >= shared.len { break; }
                        let len = chunk_bytes.min(shared.len - start);
                        observer(RegionEvent::BeforeEnable { offset: start, len });
                        if shared.enable_fault_at == Some(start) {
                            record(format!("注入提交失败，偏移 {start}")); break;
                        }
                        if let Err(text) = os::enable(shared.base, start, len) {
                            record(text); break;
                        }
                        shared.enabled_bytes.fetch_add(len, Ordering::Relaxed);
                        observer(RegionEvent::Enabled { offset: start, len });
                        // SAFETY: start 只发放一次，范围有效且与其他 worker 不相交。
                        let out = unsafe { std::slice::from_raw_parts_mut(
                            shared.base.as_ptr().add(start), len) };
                        if let Err(text) = writer(start, out) { record(text); break; }
                        shared.written_bytes.fetch_add(len, Ordering::Relaxed);
                    }
                    observer(RegionEvent::WorkerEnd { worker });
                });
            }
        });
        if let Some(text) = error.into_inner().unwrap() { Err(text) } else { Ok(()) }
    }

    /// 只有完整写入才产生只读完成态；Err 会消耗 self 并由 Drop 释放区域。
    pub fn finish(self) -> Result<FinishedRam, String> {
        if self.written_bytes.load(Ordering::Relaxed) != self.len {
            return Err("内存区域尚未完整写入".into());
        }
        Ok(FinishedRam(self))
    }
}

impl FinishedRam {
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: 完成态保证所有写入线程退出且 written_bytes == len。
        unsafe { std::slice::from_raw_parts(self.0.base.as_ptr(), self.0.len) }
    }
    pub fn len(&self) -> usize { self.0.len }
    pub fn as_ptr(&self) -> *const u8 { self.0.base.as_ptr() }
}

impl Drop for RamRegion {
    fn drop(&mut self) {
        os::free(self.base, self.len);
        LIVE_RESERVED_BYTES.fetch_sub(self.len, Ordering::Relaxed);
    }
}
```

Windows `os` 只用 `MEM_RESERVE` 预留，用 `MEM_COMMIT` 启用每块，用 `VirtualFree(base, 0, MEM_RELEASE)` 释放。macOS 实现完整如下：

```rust
#[cfg(target_os = "macos")]
mod os {
    use super::*;
    pub fn reserve(len: usize) -> Result<NonNull<u8>, String> {
        // SAFETY: 返回新的匿名私有映射，成功时由 RamRegion::drop 释放。
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON, -1, 0) };
        if ptr == libc::MAP_FAILED {
            Err(format!("匿名映射 {} 字节失败：{}", len, std::io::Error::last_os_error()))
        } else {
            NonNull::new(ptr.cast()).ok_or_else(|| "匿名映射返回空地址".into())
        }
    }
    pub fn enable(_base: NonNull<u8>, start: usize, len: usize) -> Result<(), String> {
        start.checked_add(len).ok_or("匿名块范围溢出")?;
        Ok(()) // macOS 在写入时按需分配物理页，不伪造 Windows commit。
    }
    pub fn free(base: NonNull<u8>, len: usize) {
        // SAFETY: base/len 与成功 mmap 完全一致，Drop 只执行一次。
        let result = unsafe { libc::munmap(base.as_ptr().cast(), len) };
        debug_assert_eq!(result, 0, "munmap 失败：{}", std::io::Error::last_os_error());
    }
}
```

- [ ] **Step 4: 运行区域测试与完整 Rust 回归**

Run: `cd src-tauri && cargo test ram_region -- --nocapture && cargo test`

Expected: 新测试全绿，Windows 预留后提交量为 0，既有测试无回归。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/ram_region.rs src-tauri/src/lib.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat: 增加连续私有内存区域"
```

---

### Task 2: 两阶段有界并行载入、平台中性事件与取消

**Files:**
- Create: `src-tauri/src/ram_load.rs`
- Modify: `src-tauri/src/lib.rs` 模块声明区
- Test: `src-tauri/src/ram_load.rs` 内置测试

**Interfaces:**
- Consumes: Task 1 的 `RamRegion`、`FinishedRam`、`RegionEvent`。
- Produces: `PositionedRead`、`LoadOptions`、`LoadFault`、`LoadEvent`、`LoadResult`、`PreparedLoad<R>`、`prepare_load`、`run_prepared_load`、`load_file`、`conversion_limit_bytes`。
- 两阶段约束: `prepare_load` 在源 mmap 仍有效时只预留；调用方解除源 mmap 后才执行 `run_prepared_load`。

- [ ] **Step 1: 写预算、并行读取、取消、提交失败和短读测试**

```rust
#[test]
fn conversion_limit_is_exactly_five_percent_and_checked() {
    assert_eq!(conversion_limit_bytes(0).unwrap(), 0);
    assert_eq!(conversion_limit_bytes(100).unwrap(), 105);
    assert_eq!(conversion_limit_bytes(101).unwrap(), 106);
    assert_eq!(conversion_limit_bytes(20 << 30).unwrap(), (20 << 30) + (1 << 30));
    assert!(conversion_limit_bytes(u64::MAX).is_err());
}

#[test]
fn load_reads_real_file_with_bounded_parallelism_and_monotonic_progress() {
    let file = temp_pattern_file("ram_load_ok", 3 * 1024 * 1024 + 17);
    let cancel = AtomicBool::new(false);
    let events = Mutex::new(Vec::new());
    let result = load_file(file, &cancel,
        LoadOptions { chunk_bytes: 64 * 1024, workers: 4, fault: LoadFault::None },
        |event| events.lock().unwrap().push(event)).unwrap();
    let LoadResult::Complete(ram) = result else { panic!("unexpected cancel") };
    assert!(ram.as_slice().iter().enumerate().all(|(i, b)| *b == (i % 251) as u8));
    let events = events.into_inner().unwrap();
    let progress: Vec<u64> = events.iter().filter_map(|e| match e {
        LoadEvent::Progress { done, .. } => Some(*done), _ => None,
    }).collect();
    assert!(progress.windows(2).all(|w| w[0] <= w[1]));
    assert_eq!(progress.last().copied(), Some(3 * 1024 * 1024 + 17));
    assert!(matches!(events.last(), Some(LoadEvent::Finished { .. })));
    assert!(events.iter().any(|e| matches!(e, LoadEvent::ChunkBegin { .. })));
    assert!(events.iter().any(|e| matches!(e, LoadEvent::ChunkReady { .. })));
}

#[test]
fn cancel_drops_partial_region_without_finished_or_fake_hundred_percent() {
    let file = temp_pattern_file("ram_load_cancel", 8 * 1024 * 1024);
    let cancel = AtomicBool::new(false);
    let events = Mutex::new(Vec::new());
    let result = load_file(file, &cancel,
        LoadOptions { chunk_bytes: 64 * 1024, workers: 2, fault: LoadFault::None },
        |event| {
            events.lock().unwrap().push(event);
            if matches!(event, LoadEvent::Progress { done, .. } if done > 0) {
                cancel.store(true, Ordering::Relaxed);
            }
        }).unwrap();
    assert!(matches!(result, LoadResult::Cancelled));
    let events = events.into_inner().unwrap();
    assert!(!events.iter().any(|e| matches!(e, LoadEvent::Finished { .. })));
    assert!(!events.iter().any(|e| matches!(e, LoadEvent::Progress { done, total }
        if done == total)));
}

#[test]
fn commit_and_short_read_faults_are_real_errors() {
    for (name, fault, expected) in [
        ("commit", LoadFault::CommitAt(128 * 1024), "提交"),
        ("short", LoadFault::ReadAfter(4096), "读取"),
    ] {
        let file = temp_pattern_file(name, 1024 * 1024);
        let cancel = AtomicBool::new(false);
        let error = load_file(file, &cancel,
            LoadOptions { chunk_bytes: 64 * 1024, workers: 2, fault }, |_| {})
            .unwrap_err();
        assert!(error.contains(expected), "{name}: {error}");
    }
}
```

`temp_pattern_file` 使用 `std::env::temp_dir()`、进程 ID 和原子序号生成唯一路径，以复用的 1 MiB 缓冲写入 `(offset % 251) as u8`；测试清理自己创建的夹具，不触碰业务或基准文件。

- [ ] **Step 2: 确认 RED**

Run: `cd src-tauri && cargo test ram_load -- --nocapture`

Expected: 缺少两阶段类型和函数导致失败；先修测试自身导入，再接受功能缺失 RED。

- [ ] **Step 3: 实现两阶段载入器和定位读取**

```rust
use crate::ram_region::{FinishedRam, RamRegion, RegionEvent};
use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

pub const CONVERSION_MARGIN_PERMILLE: u64 = 50;
pub const COPY_CHUNK_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_LOAD_WORKERS: usize = 4;

/// 计算 floor(X × 1.05)，任一乘法或加法溢出都拒绝转换。
pub fn conversion_limit_bytes(file_bytes: u64) -> Result<u64, String> {
    file_bytes.checked_mul(1050).and_then(|v| v.checked_div(1000))
        .ok_or_else(|| "转换内存上限计算溢出".into())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoadFault {
    #[default]
    None,
    CommitAt(usize),
    ReadAfter(u64),
}

#[derive(Clone, Copy, Debug)]
pub struct LoadOptions {
    pub chunk_bytes: usize,
    pub workers: usize,
    #[doc(hidden)]
    pub fault: LoadFault,
}

impl Default for LoadOptions {
    fn default() -> Self {
        let workers = std::thread::available_parallelism().map_or(1, usize::from)
            .min(MAX_LOAD_WORKERS);
        Self { chunk_bytes: COPY_CHUNK_BYTES, workers, fault: LoadFault::None }
    }
}

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

#[derive(Debug)]
pub enum LoadResult { Complete(FinishedRam), Cancelled }

/// 线程安全定位读取；实现不得改变共享文件游标。
pub trait PositionedRead: Sync {
    fn len(&self) -> Result<u64, String>;
    fn read_at(&self, offset: u64, out: &mut [u8]) -> Result<usize, String>;
}

#[cfg(windows)]
impl PositionedRead for File {
    fn len(&self) -> Result<u64, String> { Ok(self.metadata().map_err(|e| e.to_string())?.len()) }
    fn read_at(&self, offset: u64, out: &mut [u8]) -> Result<usize, String> {
        use std::os::windows::fs::FileExt;
        self.seek_read(out, offset).map_err(|e| format!("定位读取失败，偏移 {offset}：{e}"))
    }
}

#[cfg(unix)]
impl PositionedRead for File {
    fn len(&self) -> Result<u64, String> { Ok(self.metadata().map_err(|e| e.to_string())?.len()) }
    fn read_at(&self, offset: u64, out: &mut [u8]) -> Result<usize, String> {
        use std::os::unix::fs::FileExt;
        FileExt::read_at(self, out, offset)
            .map_err(|e| format!("定位读取失败，偏移 {offset}：{e}"))
    }
}

pub struct PreparedLoad<R> {
    source: R,
    region: RamRegion,
    total: usize,
    options: LoadOptions,
}

/// 第一阶段只取得长度并预留目标；源 mmap 由调用方继续保持。
pub fn prepare_load<R, O>(source: R, options: LoadOptions, observer: &O)
    -> Result<PreparedLoad<R>, String>
where R: PositionedRead, O: Fn(LoadEvent) + Sync
{
    let total = usize::try_from(source.len()?).map_err(|_| "文件大小超出本机地址范围".to_string())?;
    observer(LoadEvent::ReserveBegin { total: total as u64 });
    let fault = match options.fault { LoadFault::CommitAt(at) => Some(at), _ => None };
    let region = RamRegion::reserve_with_enable_fault(total, fault)?;
    observer(LoadEvent::Reserved { total: total as u64 });
    observer(LoadEvent::Progress { done: 0, total: total as u64 });
    Ok(PreparedLoad { source, region, total, options })
}

/// 第二阶段直接读入最终区域；调用此函数前必须已解除完整源映射。
pub fn run_prepared_load<R, O>(prepared: PreparedLoad<R>, cancel: &AtomicBool, observer: O)
    -> Result<LoadResult, String>
where R: PositionedRead, O: Fn(LoadEvent) + Sync
{
    let PreparedLoad { source, mut region, total, options } = prepared;
    if cancel.load(Ordering::Relaxed) { return Ok(LoadResult::Cancelled); }
    let progress = Mutex::new(0u64);
    let observer = &observer;
    let result = region.for_each_chunk_mut(options.chunk_bytes, options.workers,
        |event| observer(match event {
            RegionEvent::WorkerBegin { worker } => LoadEvent::WorkerBegin { worker },
            RegionEvent::WorkerEnd { worker } => LoadEvent::WorkerEnd { worker },
            RegionEvent::BeforeEnable { offset, len } =>
                LoadEvent::ChunkBegin { offset: offset as u64, len },
            RegionEvent::Enabled { offset, len } =>
                LoadEvent::ChunkReady { offset: offset as u64, len },
        }),
        |start, out| {
            let mut read = 0usize;
            while read < out.len() {
                if cancel.load(Ordering::Relaxed) { return Err("用户取消载入".into()); }
                let offset = start.checked_add(read).ok_or("读取偏移溢出")?;
                if matches!(options.fault, LoadFault::ReadAfter(limit) if offset as u64 >= limit) {
                    return Err(format!("注入读取失败，偏移 {offset}"));
                }
                let n = source.read_at(offset as u64, &mut out[read..])?;
                if n == 0 { return Err(format!("文件读取提前结束，偏移 {offset}")); }
                read += n;
            }
            let mut done = progress.lock().unwrap();
            *done = done.checked_add(out.len() as u64).ok_or("读取进度溢出")?;
            observer(LoadEvent::Progress { done: *done, total: total as u64 });
            if cancel.load(Ordering::Relaxed) { return Err("用户取消载入".into()); }
            Ok(())
        });
    if let Err(error) = result {
        if cancel.load(Ordering::Relaxed) { return Ok(LoadResult::Cancelled); }
        return Err(error);
    }
    let finished = region.finish()?;
    observer(LoadEvent::Finished { total: total as u64 });
    Ok(LoadResult::Complete(finished))
}

/// 小测试和不需要解除源映射的调用方使用两阶段核心，不另写复制算法。
pub fn load_file<R, O>(source: R, cancel: &AtomicBool, options: LoadOptions, observer: O)
    -> Result<LoadResult, String>
where R: PositionedRead, O: Fn(LoadEvent) + Sync
{
    let prepared = prepare_load(source, options, &observer)?;
    run_prepared_load(prepared, cancel, observer)
}
```

macOS 的 `ChunkReady` 表示目标范围已可写，不宣称发生 Windows 式逐块 commit；预留、启用和 worker 边界事件全部保留，供 Task 10 在这些分配点采样真实进程内存。

- [ ] **Step 4: 运行载入和完整 Rust 测试**

Run: `cd src-tauri && cargo test ram_load -- --nocapture && cargo test`

Expected: 正常读取字节完全一致；取消无伪 100%；提交和短读故障释放半成品；既有测试无回归。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/ram_load.rs src-tauri/src/lib.rs
git commit -m "feat: 实现两阶段有界多核文件载入"
```

---

### Task 3: 引擎双 RAM 后备、索引线程收尾与纯后备转换

**Files:**
- Modify: `src-tauri/src/engine.rs` 的 `Backing`、`BigFile`、渐进索引、RAM 载入和测试区
- Test: `src-tauri/src/engine.rs` 内置测试

**Interfaces:**
- Consumes: `prepare_load`、`run_prepared_load`、`LoadOptions`、`LoadEvent`、`LoadResult`。
- Produces: `RamBuffer::{Vec,Region}`、`Backing::Ram(RamBuffer)`、`FileStamp`、`ConvertStatus`、`BigFile::convert_mmap_to_ram`、`join_index_worker`、`map_current_backing`。
- 保留: `load_into_ram(&AtomicBool, &mut FnMut(u64,u64)) -> Result<bool,String>` 的现有排序调用语义。

- [ ] **Step 1: 写字节、历史、索引代际、源映射释放和恢复测试**

```rust
#[test]
fn backing_conversion_preserves_bytes_history_filter_and_index_epoch() {
    let p = tmp_csv("convert_preserve", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
    bf.run_filter(&spec_one(0, "Alice")).unwrap();
    let before_rows = bf.filter_page(0, 10).rows;
    let before_history = bf.history_stats();
    let before_epoch = bf.index_epoch;
    let weak = match &bf.backing {
        Backing::Mmap(m) => Arc::downgrade(m), _ => panic!("预期 mmap"),
    };
    let cancel = AtomicBool::new(false);
    assert!(bf.load_into_ram(&cancel, &mut |_, _| {}).unwrap());
    assert!(matches!(bf.backing, Backing::Ram(RamBuffer::Region(_))));
    assert!(weak.upgrade().is_none(), "读取完成前旧 Arc<Mmap> 必须全部释放");
    assert_eq!(bf.index_epoch, before_epoch);
    assert_eq!(bf.history_stats(), before_history);
    assert_eq!(bf.filter_page(0, 10).rows, before_rows);
    assert!(bf.undo());
    assert_eq!(bf.rows(0, 1)[0][1], "30");
    fs::remove_file(&p).ok();
}

#[test]
fn progressive_conversion_joins_index_worker_and_releases_its_mmap_arc() {
    let p = tmp_csv("convert_join", &people_csv());
    let mut bf = BigFile::open_progressive_with(&p, 8, 1, 16, false).unwrap();
    while bf.is_indexing() {
        bf.refresh_indexed();
        std::thread::yield_now();
    }
    let cancel = AtomicBool::new(false);
    bf.load_into_ram(&cancel, &mut |_, _| {}).unwrap();
    assert!(bf.index_thread.is_none());
    assert_eq!(bf.rows(0, 1)[0][0], "Alice");
    fs::remove_file(&p).ok();
}

#[test]
fn conversion_read_fault_restores_mmap_without_changing_revision_or_history() {
    let p = tmp_csv("convert_fault", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_cell(0, 0, "unsaved".into(), Approval::deny()).unwrap();
    let before = (bf.revision, bf.history_stats(), bf.rows(0, 1));
    let cancel = AtomicBool::new(false);
    let error = bf.convert_mmap_to_ram(&cancel,
        LoadOptions { chunk_bytes: 4096, workers: 2, fault: LoadFault::ReadAfter(32) },
        &mut |_| {}).unwrap_err();
    assert!(error.contains("读取"));
    assert!(!bf.is_ram_backed());
    assert_eq!((bf.revision, bf.history_stats(), bf.rows(0, 1)), before);
    fs::remove_file(&p).ok();
}
```

另以同一转换核心逐项建立矩阵断言：UTF-8、GBK、带引号换行、末行无换行、纯文本逐行；插入行、删除行、共享 Schema 列历史的 undo/redo；预留失败、指定块提交失败、指定偏移读取失败、完成时文件戳变化、恢复映射再失败。每个用例都检查内容、筛选、`revision`、历史、`index_epoch` 和临时区域释放。恢复再失败的错误必须同时包含原始错误和恢复错误，不能返回“已恢复”。

- [ ] **Step 2: 确认 RED**

Run: `cd src-tauri && cargo test backing_conversion -- --nocapture && cargo test progressive_conversion -- --nocapture`

Expected: `RamBuffer`、`index_thread` 和 `convert_mmap_to_ram` 尚不存在；既有 RAM 测试仍应通过，不能用删旧测试制造 GREEN。

- [ ] **Step 3: 接入 RamBuffer 并完成索引线程所有权**

```rust
enum RamBuffer {
    Vec(Vec<u8>),
    Region(FinishedRam),
}

enum Backing {
    Mmap(Arc<memmap2::Mmap>),
    Ram(RamBuffer),
}

impl Backing {
    fn bytes(&self) -> &[u8] {
        match self {
            Backing::Mmap(m) => m,
            Backing::Ram(RamBuffer::Vec(v)) => v,
            Backing::Ram(RamBuffer::Region(r)) => r.as_slice(),
        }
    }
    fn is_ram(&self) -> bool { matches!(self, Backing::Ram(_)) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp { len: u64, modified: std::time::SystemTime }

impl FileStamp {
    fn read(path: &Path) -> Result<Self, String> {
        let meta = fs::metadata(path).map_err(|e| format!("读取后备文件状态失败：{e}"))?;
        Ok(Self { len: meta.len(),
            modified: meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH) })
    }
}
```

`BigFile` 增加 `index_thread: Option<std::thread::JoinHandle<()>>`。所有构造点显式初始化；`csv-index` 的 handle 存入字段。`join_index_worker()` 在确认 `!is_indexing()` 后 take 并 join，worker panic 转成错误；`Drop` 先设置现有取消标志，再 take/join，禁止后台线程遗留 `Arc<Mmap>`。不要在每次 `meta()` 中无条件 join 阻塞 UI。

把当前 `#[cfg(test)] BigFile::open/open_with` 的构造体提取为 `pub(crate) fn open_complete(path, headerless)`，现有测试 helper 保持原名并转发；Task 10 的隐藏诊断模块也调用这一同一份引擎构造，不复制解析或转换逻辑。

- [ ] **Step 4: 实现两阶段后备转换与恢复**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConvertStatus { Converted, Cancelled }

impl BigFile {
    /// 把 mmap 转为 RAM；observer 可被多个 worker 并发调用。
    fn convert_mmap_to_ram<O>(&mut self, cancel: &AtomicBool,
        options: LoadOptions, observer: O) -> Result<ConvertStatus, String>
    where O: Fn(LoadEvent) + Sync
    {
        if self.is_ram_backed() { return Ok(ConvertStatus::Cancelled); }
        self.refresh_indexed();
        if self.is_indexing() { return Err("文件仍在索引中".into()); }
        self.join_index_worker()?;
        let before = FileStamp::read(&self.backing_path)?;
        let file = fs::File::open(&self.backing_path).map_err(|e| e.to_string())?;
        // 原 mmap 在预留期间保持有效；预留失败直接返回，现场不变。
        let prepared = prepare_load(file, options, &observer)?;
        let previous = std::mem::replace(&mut self.backing,
            Backing::Ram(RamBuffer::Vec(Vec::new())));
        debug_assert!(matches!(previous, Backing::Mmap(_)));
        drop(previous); // 读取开始前释放最后一个完整源 Arc<Mmap>。
        match run_prepared_load(prepared, cancel, &observer) {
            Ok(LoadResult::Complete(ram)) => match FileStamp::read(&self.backing_path) {
                Ok(after) if after == before => {
                    self.backing = Backing::Ram(RamBuffer::Region(ram));
                    Ok(ConvertStatus::Converted)
                }
                Ok(_) => {
                    drop(ram);
                    self.restore_after_conversion(
                        "EXTERNAL_CHANGED: 转换期间后备文件发生变化".into(), false)
                }
                Err(error) => {
                    drop(ram);
                    self.restore_after_conversion(error, false)
                }
            },
            Ok(LoadResult::Cancelled) => match FileStamp::read(&self.backing_path) {
                Ok(after) if after == before =>
                    self.restore_after_conversion("用户取消".into(), true),
                Ok(_) => self.restore_after_conversion(
                    "EXTERNAL_CHANGED: 取消时发现后备文件发生变化".into(), false),
                Err(error) => self.restore_after_conversion(error, false),
            },
            Err(error) => self.restore_after_conversion(error, false),
        }
    }

    /// 取消/失败统一按 backing_path 重建 mmap，不改索引代际或历史。
    fn restore_after_conversion(&mut self, error: String, cancelled: bool)
        -> Result<ConvertStatus, String>
    {
        match self.map_current_backing() {
            Ok(backing) => {
                self.backing = backing;
                if cancelled { Ok(ConvertStatus::Cancelled) } else { Err(error) }
            }
            Err(recovery) => Err(format!("转换失败：{error}\n恢复 mmap 也失败：{recovery}")),
        }
    }

    fn map_current_backing(&self) -> Result<Backing, String> {
        let file = fs::File::open(&self.backing_path).map_err(|e| e.to_string())?;
        let mmap = unsafe { memmap2::MmapOptions::new().map(&file).map_err(|e| e.to_string())? };
        Ok(Backing::Mmap(Arc::new(mmap)))
    }

    /// 保留现有临时排序 RAM 接口；Mutex 只序列化 UI 回调，不串行化读取。
    pub fn load_into_ram<F>(&mut self, cancel: &AtomicBool, progress: &mut F)
        -> Result<bool, String>
    where F: FnMut(u64, u64) + Send
    {
        if self.is_ram_backed() {
            let total = self.bytes().len() as u64;
            progress(total, total);
            return Ok(true);
        }
        let progress = std::sync::Mutex::new(progress);
        let observer = |event| {
            if let LoadEvent::Progress { done, total } = event {
                let mut callback = progress.lock().unwrap();
                (*callback)(done, total);
            }
        };
        match self.convert_mmap_to_ram(cancel, LoadOptions::default(), observer)? {
            ConvertStatus::Converted => Ok(true),
            ConvertStatus::Cancelled => Ok(false),
        }
    }
}
```

所有转换 observer 统一为 `Fn(LoadEvent) + Sync`。保留 `load_into_ram` 对调用方的 `FnMut(u64,u64)` 进度接口，用 `Mutex<&mut F>` 适配并发 worker；因此增加 `F: Send` 约束，现有排序进度闭包仍可跨后台任务发送。已经 RAM 时维持现有行为：发送一次 `total/total` 并返回 `Ok(true)`，不进入转换器。

外部文件戳变化恢复 mmap 后返回 `EXTERNAL_CHANGED` 前缀，由后续 IPC/App 进入现有外部变化提示；不自动重载、不清编辑。`path` 仍是用户保存身份，`backing_path` 是映射和恢复身份，二者不得替换使用。恢复失败时错误同时包含转换和恢复错误，绝不宣称现场已恢复。- [ ] **Step 5: 运行新测试和完整 Rust 回归**

Run: `cd src-tauri && cargo test conversion -- --nocapture && cargo test ram_load -- --nocapture && cargo test`

Expected: 新旧转换、取消、筛选、历史和保存测试全绿；取消不发送最终 100%；旧 mmap weak 不可再升级。

- [ ] **Step 6: 提交**

```bash
git add src-tauri/src/engine.rs
git commit -m "feat: 保留索引切换连续 RAM 后备"
```

---

### Task 4: Windows/macOS 真实私有内存采样与匿名区域审计

**Files:**
- Create: `src-tauri/src/process_memory.rs`
- Modify: `src-tauri/src/lib.rs` 模块声明区
- Test: `src-tauri/src/process_memory.rs` 内置测试

**Interfaces:**
- Produces: `ProcessMemorySample`、`SystemMemory`、`AnonymousRegionAudit`、`sample_process(pid)`、`sample_current_process()`、`available_system_memory()`、`audit_task_regions()`。
- 硬指标: Windows `PROCESS_MEMORY_COUNTERS_EX.PrivateUsage`；macOS `task_vm_info.internal + task_vm_info.compressed`，并做匿名私有区域审计。
- 禁止替代: 工作集、`resident_size`、系统可用内存和 `phys_footprint` 只作诊断，不能进入硬断言的 `private_bytes` 字段。

- [ ] **Step 1: 先写当前进程、外部进程入口和区域审计测试**

```rust
#[test]
fn current_process_reports_real_private_and_available_memory() {
    let sample = sample_current_process().unwrap();
    assert!(sample.private_bytes > 0);
    assert!(sample.working_set_bytes.unwrap_or(1) > 0);
    let system = available_system_memory().unwrap();
    assert!(system.available_physical_bytes > 0);
    #[cfg(windows)]
    assert!(system.available_commit_bytes.unwrap() > 0);
    #[cfg(target_os = "macos")]
    assert!(system.available_commit_bytes.is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_anonymous_region_audit_covers_ram_and_detects_release() {
    let mut region = RamRegion::reserve(16 * 1024 * 1024).unwrap();
    region.for_each_chunk_mut(1024 * 1024, 2, |_| {}, |_, out| {
        out.fill(0x5a);
        Ok(())
    }).unwrap();
    let ram = region.finish().unwrap();
    let base = ram.as_ptr() as u64;
    let audit = audit_task_regions(unsafe { mach_task_self() }, Some((base, ram.len() as u64))).unwrap();
    assert!(audit.matched_private_bytes >= ram.len() as u64);
    drop(ram);
    let after = audit_task_regions(unsafe { mach_task_self() }, Some((base, 16 * 1024 * 1024)));
    assert!(after.is_err() || after.unwrap().matched_private_bytes == 0);
}
```

Windows 的逐块真实提交和释放由 Task 1 的 `VirtualQuery` 已覆盖；这里增加 `sample_process(Some(std::process::id()))` 走 `OpenProcess/task_for_pid` 同一外部入口的测试。macOS 若 `task_for_pid` 权限失败，测试和 CI 必须失败并打印 kern_return，不得改成 `resident_size` 或跳过。

- [ ] **Step 2: 确认 RED**

Run: `cd src-tauri && cargo test process_memory -- --nocapture`

Expected: 缺少采样模块和返回类型；macOS 结构体计数或权限问题必须作为真实失败暴露，不允许静默 fallback。

- [ ] **Step 3: 实现跨平台返回类型与 Windows 私有字节**

```rust
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct ProcessMemorySample {
    pub private_bytes: u64,
    pub working_set_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct SystemMemory {
    pub available_physical_bytes: u64,
    pub available_commit_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct AnonymousRegionAudit {
    pub matched_private_bytes: u64,
    pub region_count: u64,
}

#[cfg(windows)]
pub fn sample_process(pid: Option<u32>) -> Result<ProcessMemorySample, String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo,
        PROCESS_MEMORY_COUNTERS_EX};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcess,
        PROCESS_QUERY_LIMITED_INFORMATION};
    let (handle, owned) = match pid {
        Some(pid) => {
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            if handle == 0 { return Err(format!("OpenProcess({pid}) 失败：{}",
                std::io::Error::last_os_error())); }
            (handle, true)
        }
        None => unsafe { (GetCurrentProcess(), false) },
    };
    let result = (|| {
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let ok = unsafe { GetProcessMemoryInfo(handle, &mut counters, counters.cb) };
        if ok == 0 { return Err(format!("GetProcessMemoryInfo 失败：{}",
            std::io::Error::last_os_error())); }
        Ok(ProcessMemorySample {
            private_bytes: counters.PrivateUsage as u64,
            working_set_bytes: Some(counters.WorkingSetSize as u64),
        })
    })();
    if owned { unsafe { CloseHandle(handle); } }
    result
}

#[cfg(windows)]
pub fn available_system_memory() -> Result<SystemMemory, String> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx,
        MEMORYSTATUSEX};
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return Err(format!("GlobalMemoryStatusEx 失败：{}", std::io::Error::last_os_error()));
    }
    Ok(SystemMemory {
        available_physical_bytes: status.ullAvailPhys,
        available_commit_bytes: Some(status.ullAvailPageFile),
    })
}
```

`sample_current_process()` 直接调用 `sample_process(None)`。外部 monitor 在子进程 GO 前先调用一次 `sample_process(child_pid)` 验证权限并预热；之后每 5 ms 走同一采样入口，任何打开、查询或 task_for_pid 错误都立即终止验收。

- [ ] **Step 4: 实现 macOS task_vm_info、可用内存与区域审计**

在本模块直接声明 C ABI，不依赖未经核实的 mach2 名称。首条 `task_info` 单测同时验证 `size_of::<TaskVmInfo>()`、count 和返回值；声明不一致必须在 macOS CI 立即失败。

```rust
#[cfg(target_os = "macos")]
mod os {
    use super::*;

    pub type KernReturn = i32;
    pub type MachPort = u32;
    pub const KERN_SUCCESS: KernReturn = 0;
    pub const KERN_INVALID_ADDRESS: KernReturn = 1;
    pub const TASK_VM_INFO: u32 = 22;
    pub const VM_REGION_EXTENDED_INFO: i32 = 13;
    pub const HOST_VM_INFO64: u32 = 4;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct TaskVmInfo {
        pub virtual_size: u64,
        pub region_count: i32,
        pub page_size: i32,
        pub resident_size: u64,
        pub resident_size_peak: u64,
        pub device: u64,
        pub device_peak: u64,
        pub internal: u64,
        pub internal_peak: u64,
        pub external: u64,
        pub external_peak: u64,
        pub reusable: u64,
        pub reusable_peak: u64,
        pub purgeable_volatile_pmap: u64,
        pub purgeable_volatile_resident: u64,
        pub purgeable_volatile_virtual: u64,
        pub compressed: u64,
        pub compressed_peak: u64,
        pub compressed_lifetime: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct VmRegionExtendedInfo {
        pub protection: i32,
        pub user_tag: u32,
        pub pages_resident: u32,
        pub pages_shared_now_private: u32,
        pub pages_swapped_out: u32,
        pub pages_dirtied: u32,
        pub ref_count: u32,
        pub shadow_depth: u16,
        pub external_pager: u8,
        pub share_mode: u8,
        pub pages_reusable: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct VmStatistics64 {
        pub free_count: u32,
        pub active_count: u32,
        pub inactive_count: u32,
        pub wire_count: u32,
        pub zero_fill_count: u64,
        pub reactivations: u64,
        pub pageins: u64,
        pub pageouts: u64,
        pub faults: u64,
        pub cow_faults: u64,
        pub lookups: u64,
        pub hits: u64,
        pub purges: u64,
        pub purgeable_count: u32,
        pub speculative_count: u32,
        pub decompressions: u64,
        pub compressions: u64,
        pub swapins: u64,
        pub swapouts: u64,
        pub compressor_page_count: u32,
        pub throttled_count: u32,
        pub external_page_count: u32,
        pub internal_page_count: u32,
        pub total_uncompressed_pages_in_compressor: u64,
    }

    extern "C" {
        pub static mach_task_self_: MachPort;
        pub fn mach_host_self() -> MachPort;
        pub fn mach_port_deallocate(task: MachPort, name: MachPort) -> KernReturn;
        pub fn task_for_pid(task: MachPort, pid: i32, target: *mut MachPort) -> KernReturn;
        pub fn task_info(task: MachPort, flavor: u32, out: *mut i32,
            count: *mut u32) -> KernReturn;
        pub fn mach_vm_region(task: MachPort, address: *mut u64, size: *mut u64,
            flavor: i32, info: *mut i32, count: *mut u32,
            object_name: *mut MachPort) -> KernReturn;
        pub fn host_statistics64(host: MachPort, flavor: u32, out: *mut i32,
            count: *mut u32) -> KernReturn;
    }

    pub unsafe fn mach_task_self() -> MachPort { mach_task_self_ }
}
#[cfg(target_os = "macos")]
use os::*;

#[cfg(target_os = "macos")]
fn sample_task(task: MachPort) -> Result<ProcessMemorySample, String> {
    let mut info = TaskVmInfo::default();
    let mut count = (std::mem::size_of::<TaskVmInfo>() / 4) as u32;
    let kr = unsafe { task_info(task, TASK_VM_INFO,
        (&mut info as *mut TaskVmInfo).cast(), &mut count) };
    if kr != KERN_SUCCESS { return Err(format!("task_info(TASK_VM_INFO) 失败：{kr}")); }
    let private = info.internal.checked_add(info.compressed)
        .ok_or_else(|| "macOS 私有字节相加溢出".to_string())?;
    Ok(ProcessMemorySample { private_bytes: private,
        working_set_bytes: Some(info.resident_size) })
}

#[cfg(target_os = "macos")]
pub fn sample_process(pid: Option<u32>) -> Result<ProcessMemorySample, String> {
    let self_task = unsafe { mach_task_self() };
    let Some(pid) = pid else { return sample_task(self_task); };
    let mut task = 0;
    let kr = unsafe { task_for_pid(self_task, pid as i32, &mut task) };
    if kr != KERN_SUCCESS {
        return Err(format!("task_for_pid({pid}) 失败：{kr}；禁止降级为工作集"));
    }
    let result = sample_task(task);
    unsafe { mach_port_deallocate(self_task, task); }
    result
}
```

`available_system_memory()` 在 macOS 调 `host_statistics64(mach_host_self(), HOST_VM_INFO64)`，可用物理字节按 `(free_count + inactive_count + speculative_count + purgeable_count) × page_size` 使用 checked u64 计算；`available_commit_bytes=None`。这不是转换硬指标，只用于确认框。

`audit_task_regions(task, range)` 用 `mach_vm_region` 和 `VmRegionExtendedInfo::count = size_of/4` 迭代地址。只累计 `external_pager == 0` 且 `share_mode` 为 `SM_COW=1`、`SM_PRIVATE=2` 或 `SM_PRIVATE_ALIASED=6` 的区域；传入 range 时累计交集字节。`KERN_INVALID_ADDRESS` 表示到达地址空间末尾或目标已释放，其他 kern_return 必须报错。区域审计证明匿名私有身份和释放，不把虚拟区域总字节当私有 commit。

- [ ] **Step 5: 运行当前平台测试和完整 Rust 回归**

Run: `cd src-tauri && cargo test process_memory -- --nocapture && cargo test`

Expected: Windows 私有字节来自 PrivateUsage；macOS task_info 与区域审计真实执行；无采样失败降级。

- [ ] **Step 6: 提交**

```bash
git add src-tauri/src/process_memory.rs src-tauri/src/lib.rs
git commit -m "test: 增加跨平台真实进程内存采样"
```

---

### Task 5: memory_mode 生命周期、动态撤销预算与退出预检

**Files:**
- Modify: `src-tauri/src/history.rs`
- Modify: `src-tauri/src/engine.rs`
- Test: 两个模块的内置测试

**Interfaces:**
- Produces: `UndoPolicy`、`PreparedUndoPolicy`、`prepare_undo_policy_for_mode`、`commit_undo_policy`、`MemoryEstimate`、`MemoryExitOutcome`、`enter_memory_mode_with_options`、`FixedPointKind::MemoryModeExit`。
- Consumes: Task 3 的同一转换核心；Task 4 的真实系统内存采样。临时 RAM 提升不得再次读取文件。

- [ ] **Step 1: 先写整数预算、状态矩阵、临时 RAM 提升和退出取消测试**

```rust
#[test]
fn resident_budget_uses_ceiling_checked_integer_math() {
    let policy = UndoPolicy { budget_bytes: 64, resident_permille: 1 };
    assert_eq!(effective_undo_budget_bytes(0, false, policy).unwrap(), 64);
    assert_eq!(effective_undo_budget_bytes(1, true, policy).unwrap(), 64);
    assert_eq!(effective_undo_budget_bytes(1001, true,
        UndoPolicy { budget_bytes: 0, resident_permille: 1 }).unwrap(), 2);
    assert_eq!(effective_undo_budget_bytes(1000, true,
        UndoPolicy { budget_bytes: 64, resident_permille: 1000 }).unwrap(), 1000);
    assert_eq!(effective_undo_budget_bytes(1000, true,
        UndoPolicy { budget_bytes: 64, resident_permille: 0 }).unwrap(), 64);
    assert!(effective_undo_budget_bytes(u64::MAX, true,
        UndoPolicy { budget_bytes: 1, resident_permille: 10_000 }).is_err());
}

#[test]
fn temporary_ram_promotion_does_not_copy_or_change_history() {
    let p = tmp_csv("memory_promote", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    let cancel = AtomicBool::new(false);
    bf.load_into_ram(&cancel, &mut |_, _| {}).unwrap();
    bf.set_cell(0, 0, "kept".into(), Approval::deny()).unwrap();
    let before = (bf.history_stats(), bf.index_epoch, bf.revision);
    let events = AtomicUsize::new(0);
    assert!(bf.enter_memory_mode_for_test(&cancel, |_| {
        events.fetch_add(1, Ordering::Relaxed);
    }).unwrap());
    assert!(bf.meta().memory_mode && bf.meta().ram_backed);
    assert_eq!((bf.history_stats(), bf.index_epoch, bf.revision), before);
    assert_eq!(events.load(Ordering::Relaxed), 0, "已有 RAM 提升不得产生复制事件");
    fs::remove_file(&p).ok();
}

#[test]
fn exit_shrink_cancel_keeps_ram_mode_history_and_dirty_state() {
    let p = tmp_csv("memory_exit_cancel", &people_csv());
    let mut bf = BigFile::open(&p).unwrap();
    let cancel = AtomicBool::new(false);
    bf.enter_memory_mode_for_test(&cancel, |_| {}).unwrap();
    bf.set_undo_policy(UndoPolicy { budget_bytes: 1, resident_permille: 1000 }).unwrap();
    bf.set_cell(0, 0, "x".repeat(4096), Approval::deny()).unwrap();
    let before = (bf.history_stats(), bf.meta().modified, bf.rows(0, 1));
    let outcome = bf.exit_memory_mode(Approval::deny()).unwrap();
    assert!(!outcome.exited);
    assert_eq!(outcome.fixed_point.unwrap().kind, FixedPointKind::MemoryModeExit);
    assert!(bf.meta().memory_mode && bf.meta().ram_backed);
    assert_eq!((bf.history_stats(), bf.meta().modified, bf.rows(0, 1)), before);
    fs::remove_file(&p).ok();
}
```

补齐四态表断言：普通 mmap、临时排序 RAM、新建无路径 RAM、持久内存模式的 `ram_backed/memory_mode` 组合严格分离；无路径新建档和低于 50 MiB 的生产入口拒绝；临时 RAM 和新建档不自动套用驻留比例。

- [ ] **Step 2: 确认 RED**

Run: `cd src-tauri && cargo test resident_budget -- --nocapture && cargo test memory_ -- --nocapture`

Expected: 缺少 `UndoPolicy`、`memory_mode` 和专用固定点类型；不得借用 `StepOverBudget` 代替退出语义。

- [ ] **Step 3: 实现纯预算函数和可提交预检**

```rust
pub const DEFAULT_RESIDENT_PERMILLE: u32 = 1000;
pub const MAX_RESIDENT_PERMILLE: u32 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UndoPolicy {
    pub budget_bytes: u64,
    pub resident_permille: u32,
}

impl Default for UndoPolicy {
    fn default() -> Self {
        Self { budget_bytes: DEFAULT_HISTORY_BUDGET_BYTES,
            resident_permille: DEFAULT_RESIDENT_PERMILLE }
    }
}

/// 内存模式有效预算 = max(普通预算, ceil(file_bytes × permille / 1000))。
pub fn effective_undo_budget_bytes(file_bytes: u64, memory_mode: bool,
    policy: UndoPolicy) -> Result<u64, String>
{
    if policy.resident_permille > MAX_RESIDENT_PERMILLE {
        return Err("驻留撤销比例超出 0%–1000% 范围".into());
    }
    if !memory_mode { return Ok(policy.budget_bytes); }
    let scaled = file_bytes.checked_mul(policy.resident_permille as u64)
        .and_then(|v| v.checked_add(999))
        .map(|v| v / 1000)
        .ok_or_else(|| "驻留撤销预算计算溢出".to_string())?;
    Ok(policy.budget_bytes.max(scaled))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BudgetEffect {
    pub evicted_undo: usize,
    pub evicted_redo: usize,
    pub establishes_fixed_point: bool,
}

pub struct PreparedBudget<T> {
    ledger: HistoryLedger<T>,
    pub effect: BudgetEffect,
}

impl<T: Clone> HistoryLedger<T> {
    /// 在克隆账本上执行现有 set_budget 规则，原账本和文档现场不变。
    pub fn prepare_budget_change(&self, budget_bytes: u64)
        -> Result<PreparedBudget<T>, HistoryError>
    {
        let before = self.stats();
        let mut ledger = self.clone();
        let effect = ledger.apply_budget_for_preview(budget_bytes)?;
        Ok(PreparedBudget { ledger, effect })
    }

    /// 提交只移动已准备账本，不再分配、IO 或返回错误。
    pub fn commit_budget_change(&mut self, prepared: PreparedBudget<T>) {
        *self = prepared.ledger;
    }
}

pub struct PreparedUndoPolicy {
    policy: UndoPolicy,
    effective_budget_bytes: u64,
    ledger: PreparedBudget<Op>,
    pub effect: BudgetEffect,
    fixed_point: bool,
    savepoint_detached: bool,
    next_history_generation: u64,
}

impl BigFile {
    pub fn undo_policy(&self) -> UndoPolicy { self.undo_policy }

    pub fn prepare_undo_policy(&self, policy: UndoPolicy)
        -> Result<PreparedUndoPolicy, String>
    {
        self.prepare_undo_policy_for_mode(policy, self.memory_mode)
    }

    fn prepare_undo_policy_for_mode(&self, policy: UndoPolicy, memory_mode: bool)
        -> Result<PreparedUndoPolicy, String>
    {
        let effective = effective_undo_budget_bytes(self.total_bytes, memory_mode, policy)?;
        let ledger = self.history.prepare_budget_change(effective)
            .map_err(history_cost_error)?;
        let next_history_generation = self.next_history_generation()?;
        let fixed_point = self.fixed_point || ledger.effect.establishes_fixed_point;
        let savepoint_detached = self.savepoint_detached
            || (ledger.effect.establishes_fixed_point && self.revision != 0);
        Ok(PreparedUndoPolicy { policy, effective_budget_bytes: effective,
            effect: ledger.effect, ledger, fixed_point, savepoint_detached,
            next_history_generation })
    }

    pub fn commit_undo_policy(&mut self, prepared: PreparedUndoPolicy) {
        self.history.commit_budget_change(prepared.ledger);
        self.undo_policy = prepared.policy;
        self.fixed_point = prepared.fixed_point;
        self.savepoint_detached = prepared.savepoint_detached;
        self.history_generation = prepared.next_history_generation;
        if prepared.effect.evicted_undo + prepared.effect.evicted_redo > 0 {
            self.reclaim_unreferenced_columns();
        }
    }

    fn set_undo_policy(&mut self, policy: UndoPolicy) -> Result<BudgetEffect, String> {
        let prepared = self.prepare_undo_policy(policy)?;
        let effect = prepared.effect;
        self.commit_undo_policy(prepared);
        Ok(effect)
    }
}
```

把现有 `set_budget` 的淘汰和固定点逻辑原样提取到 `apply_budget_for_preview`，让旧 `set_budget` 也调用它，确保 preview 与实际提交一致。`HistoryLedger<T>` 和条目在 `T: Clone` 时可克隆；prepared 的临时内存只来自现有历史上限，不是文件转换缓冲。

`FixedPointKind` 增加 `MemoryModeExit`。`BigFile` 增加 `memory_mode: bool`、`undo_policy: UndoPolicy`；`BigMeta` 增加：

```rust
pub memory_mode: bool,
pub effective_undo_budget_bytes: u64,
pub undo_budget_bytes: u64,
pub resident_permille: u32,
pub backing_file_bytes: u64,
```

`commit_undo_policy` 同步账本、`fixed_point`、保存点脱离状态、脏点和 `history_generation`，完全复用当前 `set_history_budget` 的副作用规则；预检取消不得调用它。

- [ ] **Step 4: 实现真实估算、统一开启入口、排序保持和退出**

```rust
pub const MEMORY_MODE_MIN_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Clone, Debug, serde::Serialize)]
pub struct MemoryEstimate {
    pub file_bytes: u64,
    pub limit_bytes: u64,
    pub available_physical_bytes: u64,
    pub available_commit_bytes: Option<u64>,
    pub already_ram: bool,
    pub can_enter: bool,
    pub reason: Option<String>,
}

#[derive(Debug)]
pub struct MemoryExitOutcome {
    pub exited: bool,
    pub fixed_point: Option<FixedPointRequest>,
}

impl BigFile {
    pub fn memory_estimate(&self) -> Result<MemoryEstimate, String> {
        let system = crate::process_memory::available_system_memory()?;
        let file_bytes = if self.has_path() { FileStamp::read(&self.backing_path)?.len } else { 0 };
        let reason = if !self.has_path() {
            Some("新建且从未保存的文档没有可靠磁盘后备".to_string())
        } else if file_bytes < MEMORY_MODE_MIN_BYTES {
            Some("内存模式只用于至少 50 MiB 的大文件".to_string())
        } else if self.is_indexing() {
            Some("文件仍在索引中".to_string())
        } else if system.available_commit_bytes.is_some_and(|available| available < file_bytes) {
            Some("Windows 可用 commit 不足以支持目标私有内存".to_string())
        } else {
            None
        };
        Ok(MemoryEstimate {
            file_bytes, limit_bytes: conversion_limit_bytes(file_bytes)?,
            available_physical_bytes: system.available_physical_bytes,
            available_commit_bytes: system.available_commit_bytes,
            already_ram: self.is_ram_backed(), can_enter: reason.is_none(), reason,
        })
    }

    pub fn enter_memory_mode<O>(&mut self, cancel: &AtomicBool, observer: O)
        -> Result<bool, String>
    where O: Fn(LoadEvent) + Sync
    {
        self.enter_memory_mode_with_options(cancel, MEMORY_MODE_MIN_BYTES,
            LoadOptions::default(), observer)
    }

    #[cfg(test)]
    fn enter_memory_mode_for_test<O>(&mut self, cancel: &AtomicBool, observer: O)
        -> Result<bool, String>
    where O: Fn(LoadEvent) + Sync
    {
        self.enter_memory_mode_with_options(cancel, 1, LoadOptions::default(), observer)
    }

    #[doc(hidden)]
    pub(crate) fn enter_memory_mode_with_options<O>(&mut self, cancel: &AtomicBool,
        minimum_bytes: u64, options: LoadOptions, observer: O) -> Result<bool, String>
    where O: Fn(LoadEvent) + Sync
    {
        if self.memory_mode { return Ok(false); }
        if !self.has_path() { return Err("没有可靠磁盘后备，不能开启内存模式".into()); }
        let estimate = self.memory_estimate()?;
        if estimate.file_bytes < minimum_bytes || !estimate.can_enter {
            return Err(estimate.reason.unwrap_or_else(|| "文件小于本入口允许的尺寸".into()));
        }
        let prepared = self.prepare_undo_policy_for_mode(self.undo_policy, true)?;
        if !self.is_ram_backed() {
            match self.convert_mmap_to_ram(cancel, options, observer)? {
                ConvertStatus::Converted => {}
                ConvertStatus::Cancelled => return Ok(false),
            }
        }
        self.commit_undo_policy(prepared);
        self.memory_mode = true;
        Ok(true)
    }
}
```

退出先按 `memory_mode=false` 准备普通预算。若会淘汰或建立固定点且未获批准，返回 `MemoryExitOutcome { exited:false, fixed_point:Some(request) }`，不得先映射或释放 RAM；批准时校验 `preflight_id == history_generation`。获批准或无副作用时，先用 `backing_path` 创建新 mmap，再无失败提交 mmap、prepared 普通预算和 `memory_mode=false`，保留原始字节、索引、`index_epoch`、编辑和保留下的历史。

排序退出条件精确改为：

```rust
if release_ram && self.is_ram_backed() && self.has_path() && !self.memory_mode {
    self.remap()?;
}
```

普通临时排序保存/退出仍回 mmap，无路径新建档仍保持 RAM，持久内存模式退出排序仍保持 RAM。现有“撤销退出排序可恢复精确排序视图”的测试不得改成强制 RAM 重载。

- [ ] **Step 5: 运行预算、生命周期和完整 Rust 测试**

Run: `cd src-tauri && cargo test resident_budget -- --nocapture && cargo test memory_ -- --nocapture && cargo test`

Expected: 默认、0%、0.1%、1000%、向上取整和溢出全过；临时 RAM 无二次复制；退出取消现场不变；旧排序/新建测试无回归。

- [ ] **Step 6: 提交**

```bash
git add src-tauri/src/engine.rs src-tauri/src/history.rs
git commit -m "feat: 增加持久内存模式和动态撤销预算"
```

---

### Task 6: 内存模式保存、另存为、排序保存与双重失败恢复

**Files:**
- Modify: `src-tauri/src/engine.rs` 的保存、历史转换、`PreparedReopen`、换名恢复
- Modify: `src-tauri/src/history.rs` 的保存重建策略
- Test: `src-tauri/src/engine.rs` 保存测试区

**Interfaces:**
- Produces: `TransformedSaveHistory`、`SaveRebuildPolicy::{Strict,EvictOldest}`、`prepare_memory_reopen`、`SaveRamEvent`、`save_to_observed`、`restore_old_ram_after_save_failure`。
- 不变: 普通 mmap、临时排序 RAM、无路径新建文档的既有保存行为和固定点协议。

- [ ] **Step 1: 先写保存生命周期、预算时机、区域顺序和失败恢复测试**

建立以下具名用例，每个都同时断言文件内容、会话内容、脏点、undo/redo、`memory_mode`、`ram_backed` 和有效预算：

1. `memory_save_and_save_as_stay_in_mode_and_use_new_file_budget`：保存及另存为后仍为 Region RAM，路径和 `backing_path` 同步，预算按新文件大小计算。
2. `memory_sorted_save_stays_in_mode_and_materializes_order`：先拒绝未批准固定点，再批准保存；物理顺序正确，排序退出但内存模式保留。
3. `memory_save_shrinks_budget_without_extra_confirmation`：新文件变小导致淘汰或固定点时保存仍成功，meta、脏点和撤销按钮数据同步。
4. `temporary_ram_and_new_document_save_legacy_behavior_unchanged`：保留现有 `save_after_ram_load_roundtrip`、临时排序回 mmap、无路径另存为回 mmap 的断言。
5. `memory_save_read_fault_restores_old_ram_and_history`：新临时文件读取故障后，原编辑、历史、路径和内存模式不变，临时文件错误路径按现有协议保留。
6. `memory_save_rename_fault_restores_old_ram_and_history`：换名失败后同样恢复；不能把新 RAM 当唯一副本而丢弃旧恢复路径。
7. `memory_save_double_failure_reports_both_and_degrades_explicitly`：恢复旧 RAM 也失败时，错误同时包含保存错误和恢复错误；若只能保留 mmap，则明确提交 `memory_mode=false` 和预先准备的普通预算，meta 与错误说明淘汰数，不伪称历史完整。
8. `memory_save_releases_old_region_before_new_reservation`：通过诊断事件断言 `OldRamReleased { old_bytes }` 在 `ReserveBegin { total:new_bytes }` 之前，且期间只有旧文件 mmap 作为恢复读路径。
9. `memory_save_repeated_cancel_recovery_has_no_region_growth`：独立子进程中重复取消/失败恢复，`live_reserved_bytes()` 每次都回到本次操作前数值；该绝对断言只在隔离进程执行。

- [ ] **Step 2: 确认 RED**

Run: `cd src-tauri && cargo test memory_save -- --nocapture`

Expected: 当前保存后回 mmap、旧预算过早重建或缺少新恢复路径而失败；旧普通保存测试仍应通过。

- [ ] **Step 3: 拆分旧 RAM 历史转换与新文件预算重建**

```rust
struct TransformedSaveHistory {
    undo: std::collections::VecDeque<HistoryEntry<Op>>,
    redo: std::collections::VecDeque<HistoryEntry<Op>>,
    blocks: Vec<Option<Arc<RowBlock>>>,
    schema: Arc<Schema>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveRebuildPolicy { Strict, EvictOldest }

impl TransformedSaveHistory {
    /// 新临时文件大小已知后才按新预算计费；提交前原账本不变。
    fn prepare_rebuild(self, budget_bytes: u64, policy: SaveRebuildPolicy)
        -> Result<PreparedSaveHistory, String>
    {
        let history = match policy {
            SaveRebuildPolicy::Strict => HistoryLedger::prepare_rebuild(
                self.undo, self.redo, budget_bytes).map_err(history_cost_error)?,
            SaveRebuildPolicy::EvictOldest => HistoryLedger::prepare_rebuild_with_eviction(
                self.undo, self.redo, budget_bytes).map_err(history_cost_error)?,
        };
        Ok(PreparedSaveHistory { history, blocks: self.blocks, schema: self.schema })
    }
}
```

把现有 `prepare_history_for_save` 拆成 `transform_history_for_save()` 和 `TransformedSaveHistory::prepare_rebuild()`。普通 mmap 保存仍在原时机以原预算 `Strict` 重建；只有 `memory_mode=true` 走新顺序。`prepare_rebuild_with_eviction` 必须复用缩容的最旧淘汰和固定点规则，不能在临时文件大小未知时提前淘汰。

内存保存顺序固定为：

1. 旧 RAM 仍有效时完成 `transform_history_for_save`、临时文件写出、完整校验、临时文件 mmap 和新索引。
2. 以新临时文件大小计算有效预算，用 `EvictOldest` 准备新历史；保存自动缩容不额外弹窗。
3. 预准备紧急退出内存模式所需的普通预算，确保双重失败时提交不再分配。
4. 先建立旧 `backing_path` 的恢复 mmap，再发出 `OldRamReleased` 并替换 self.backing，真正 drop 旧 Region。
5. 对已校验临时文件执行同一个两阶段 RAM 核心，保留刚建好的索引；临时 mmap 在读取前解除。
6. 换名成功后只做无失败提交：新 RAM、新索引、新保存身份、转换后的历史和新有效预算。
7. 任一新 RAM 或换名失败，丢弃新半成品，从旧 `backing_path` 恢复旧 RAM；成功时原历史和 `memory_mode` 完全不变。

排序保存共用第 1、2、4–7 步的资源状态机，但历史仍按现有排序物化规则清空并建立固定点；不能复制一份不同的内存写入算法。无路径新建档不得进入“丢弃唯一 RAM”的分支。

- [ ] **Step 4: 实现恢复状态机和诊断阶段事件**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum SaveRamPhase {
    OldRamReleased { old_bytes: u64 },
    NewReserveBegin { new_bytes: u64 },
    NewRamReady { new_bytes: u64 },
    OldRamRestored { old_bytes: u64 },
    EmergencyMmapFallback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum SaveRamEvent {
    Phase(SaveRamPhase),
    Load(LoadEvent),
}

impl BigFile {
    /// 生产保存保留原签名；默认 RAM 参数和无操作诊断 observer 不改变 UI 行为。
    pub fn save_to<F: FnMut(u64, u64, u8, u64)>(&mut self, dest: &Path,
        switch_path: bool, approval: Approval, progress: F) -> Result<SaveOutcome, String>
    {
        self.save_to_observed(dest, switch_path, approval, LoadOptions::default(),
            progress, |_| {})
    }

    /// 诊断与生产共用同一保存状态机，observer 同时收到阶段和真实载入事件。
    #[doc(hidden)]
    pub(crate) fn save_to_observed<F, O>(&mut self, dest: &Path, switch_path: bool,
        approval: Approval, load_options: LoadOptions, progress: F, observer: O)
        -> Result<SaveOutcome, String>
    where
        F: FnMut(u64, u64, u8, u64),
        O: Fn(SaveRamEvent) + Sync,
    {
        // 现有保存主体移入此函数；所有新 RAM LoadEvent 包装为 SaveRamEvent::Load。
        // 在每个 SaveRamPhase 的实际状态变更后发送 SaveRamEvent::Phase。
        self.save_to_memory_impl(dest, switch_path, approval, load_options,
            progress, observer)
    }

    /// 保存失败后先尝试恢复旧 RAM；双重失败才使用已准备的普通预算紧急回退。
    fn restore_old_ram_after_save_failure(&mut self, save_error: String,
        emergency_budget: PreparedUndoPolicy) -> String
    {
        match self.reload_old_ram_for_save_recovery() {
            Ok(()) => save_error,
            Err(ram_recovery) => {
                let effect = emergency_budget.effect;
                self.commit_undo_policy(emergency_budget);
                self.memory_mode = false;
                format!(
                    "{save_error}\n恢复旧内存后备也失败：{ram_recovery}\n\
                     已紧急回退到 mmap 并退出内存模式；淘汰 undo {} 条、redo {} 条，固定点：{}",
                    effect.evicted_undo, effect.evicted_redo, effect.establishes_fixed_point)
            }
        }
    }
}
```

`save_to_memory_impl` 是现有保存主体的新名称，不是第二套保存算法。`OldRamReleased` 必须在旧 Region 实际 drop 后发送，`NewReserveBegin` 必须在新区域预留前发送；新 RAM 的每个 `LoadEvent` 都原样包装给 observer。`reload_old_ram_for_save_recovery` 只能从旧 `backing_path` 读取，保留旧 `idx/index_epoch/seg/overlay/history`，并复用 Task 3 转换核心。紧急回退只用于“保存失败 + 旧 RAM 恢复失败”的双失败，不是普通取消退出的捷径。若连恢复 mmap 也不可用，错误继续追加该错误，文档不得宣称可安全读取。

`PreparedReopen.backing` 保持 `Backing` 类型以支持 RAM；`commit_reopen` 不强制改回 mmap，也不重置 `memory_mode`。另存为仍沿用现有路径预留、迁移和 owner/lease 协议。

- [ ] **Step 5: 运行保存矩阵和完整 Rust 回归**

Run: `cd src-tauri && cargo test memory_save -- --nocapture && cargo test save_after_ram_load_roundtrip -- --nocapture && cargo test`

Expected: 内存保存两个分支均保持模式；旧临时 RAM/新建行为不变；所有故障路径可读且错误如实描述；无双重完整匿名目标。

- [ ] **Step 6: 提交**

```bash
git add src-tauri/src/engine.rs src-tauri/src/history.rs
git commit -m "feat: 保存后保持内存模式并可恢复失败"
```

---

### Task 7: Runtime 双设置原子更新、内存 IPC、取消与定向事件

**Files:**
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/document_registry.rs` 仅在任务状态需要独立字段时修改
- Test: `src-tauri/src/lib.rs` 运行时测试区

**Interfaces:**
- Produces: `UndoSettings`、`set_runtime_undo_policy`、四个内存命令、`big-memory-progress`、`MemoryModeOutcome`。
- IPC: `big_memory_mode_estimate`、`big_memory_mode_enter`、`big_memory_mode_cancel`、`big_memory_mode_exit`。

- [ ] **Step 1: 先写双策略、在途注册、预检失败和取消路由测试**

```rust
#[test]
fn process_policy_prepare_failure_changes_no_document_or_future_default() {
    let runtime = SharedRuntime::default();
    let a = register_memory(&runtime, "main");
    edit_twice(&runtime, "main", &a);
    let before = history_stats(&runtime, "main", a.session, a.lease);
    runtime.lock().unwrap().docs.get(a.session).unwrap()
        .file.lock().unwrap().set_backing_bytes_for_test(u64::MAX);
    let error = set_runtime_undo_policy(&runtime, 1024 * 1024, 10_000).unwrap_err();
    assert!(error.contains("溢出"));
    assert_eq!(history_stats(&runtime, "main", a.session, a.lease), before);
    assert_eq!(runtime.lock().unwrap().resident_permille, 1000);
}

#[test]
fn policy_update_and_inflight_registration_never_expose_half_a_policy() {
    let runtime = SharedRuntime::default();
    let a = register_memory(&runtime, "main");
    let b = register_memory(&runtime, "doc-1");
    set_runtime_undo_policy(&runtime, 1024 * 1024, 250).unwrap();
    for (owner, session) in [("main", a), ("doc-1", b)] {
        let file = authorize_big(&runtime, owner, session.session, session.lease,
            Access::Read).unwrap();
        let file = file.file.lock().unwrap();
        assert_eq!(file.undo_policy(), UndoPolicy {
            budget_bytes: 1024 * 1024, resident_permille: 250,
        });
    }
    let c = register_memory(&runtime, "main");
    assert_eq!(history_stats(&runtime, "main", c.session, c.lease).budget_bytes,
        1024 * 1024);
}
```

保留并更新现有“设置期间新开文档”“设置前在途 open”“不同 owner 独立淘汰”测试，使其同时断言普通预算和驻留千分比。取消测试确认 `Access::Cancel` 只设置原子标志，不尝试获取写锁；enter 的 worker 仍持写锁时 cancel 能立即返回，enter 最终恢复并返回 `cancelled=true`。

- [ ] **Step 2: 确认 RED**

Run: `cd src-tauri && cargo test process_policy -- --nocapture && cargo test memory_mode_command -- --nocapture`

Expected: 缺少驻留策略字段、双参数更新和内存命令；不得以先写全局值再逐文档失败的方式通过。

- [ ] **Step 3: 实现全文档准备后无失败提交**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct UndoSettings {
    pub budget_bytes: u64,
    pub resident_permille: u32,
}

struct RuntimeState {
    // 保留现有字段。
    docs: DocumentRegistry,
    opens: OpenRequestQueue,
    windows: WindowRegistry,
    drags: DragRegistry,
    undo_budget_bytes: u64,
    resident_permille: u32,
}

fn set_runtime_undo_policy(runtime: &SharedRuntime, budget_bytes: u64,
    resident_permille: u32) -> Result<Vec<SessionInfo>, String>
{
    validate_undo_policy(budget_bytes, resident_permille)?;
    let mut guard = runtime.lock().unwrap();
    let entries = guard.docs.entries_in_session_order();
    let mut prepared = Vec::with_capacity(entries.len());
    // 唯一锁顺序：runtime -> document。准备完所有文档前不写全局默认。
    for (_session, entry) in entries {
        let file = entry.file.lock().unwrap();
        prepared.push((Arc::clone(&entry.file), file.prepare_undo_policy(UndoPolicy {
            budget_bytes, resident_permille,
        })?));
    }
    for (file, change) in prepared {
        file.lock().unwrap().commit_undo_policy(change);
    }
    guard.undo_budget_bytes = budget_bytes;
    guard.resident_permille = resident_permille;
    Ok(guard.docs.session_infos())
}
```

在 `DocumentRegistry` 内直接迭代现有 `sessions` map，新增确定顺序快照；不新增第二份注册表：

```rust
impl DocumentRegistry {
    pub fn entries_in_session_order(&self) -> Vec<(u64, BigEntry)> {
        let mut entries: Vec<_> = self.sessions.iter()
            .map(|(session, entry)| (*session, entry.clone())).collect();
        entries.sort_by_key(|(session, _)| *session);
        entries
    }
}
```

`BigEntry` 已可 Clone，快照持有同一 `Arc<Mutex<BigFile>>`。`BigFile::undo_policy()` 返回当前 `UndoPolicy`；`set_backing_bytes_for_test` 仅修改测试文档的 `total_bytes`，不得改真实文件。`register_new_document` 在同一个 runtime 锁内读取完整 `UndoPolicy` 并准备/提交新文档，防止在途 open/create 看到半套设置。任何路径都不得持文档锁再获取 runtime 锁；事件路由先复制 owner/lease，再释放 runtime 锁。保留 `set_runtime_undo_budget` 作为现有测试兼容 wrapper，它读取当前驻留值后调用新函数。

`set_undo_budget` Tauri 命令保留名称但参数变为 `{ budgetBytes, residentPermille }`，返回现有 session 列表。启动和设置页都一次上发两个值。

- [ ] **Step 4: 实现命令、进度事件和关闭等待**

```rust
#[derive(Debug, serde::Serialize)]
pub struct MemoryModeOutcome {
    pub changed: bool,
    pub cancelled: bool,
    pub fixed_point: Option<FixedPointRequest>,
    pub meta: BigMeta,
}

#[derive(Clone, Debug, serde::Serialize)]
struct MemoryProgress {
    session: u64,
    lease: u64,
    done: u64,
    total: u64,
}
```

- estimate 使用 `Access::Read`，返回 Task 5 的 X、105% 上限、可用物理内存、Windows 可用 commit 和是否已 RAM。
- enter 复用 `big_sort_load` 的 `spawn_blocking`、后台线程降优先级和 owner/lease 定向事件模式，但调用 `enter_memory_mode`，绝不调用 `big_sort_load`。事件名固定 `big-memory-progress`，payload 使用 `session/lease/done/total`。
- enter 开始时 `entry.load_cancel.store(false)`，持文档写锁；已经是临时 RAM 时只提交状态并立即返回，不发送复制进度。
- cancel 使用 `Access::Cancel`，只设置 `entry.load_cancel=true`，返回后不等待写锁。
- exit 使用 `Access::Write`，返回固定点请求或最终 meta；成功只改变后备和预算，不发表格结构 data 事件。
- 四个命令注册进 `invoke_handler`。`big_close` 继续先 bump gate、设置取消，再等待文件锁；owner/lease 事件不得发给迁移后的旧窗口。

- [ ] **Step 5: 运行 Runtime 和完整 Rust 回归**

Run: `cd src-tauri && cargo test process_policy -- --nocapture && cargo test memory_mode_command -- --nocapture && cargo test`

Expected: 预检失败零提交；在途注册看到完整最新策略；取消不阻塞；迁移和关闭事件路由无回归。

- [ ] **Step 6: 提交**

```bash
git add src-tauri/src/lib.rs src-tauri/src/document_registry.rs
git commit -m "feat: 增加内存模式 IPC 和原子撤销设置"
```

---

### Task 8: BigModel、版本化撤销设置、公式实时试算与 shared fake

**Files:**
- Modify: `frontend/js/prefs.js`
- Modify: `frontend/js/dialogs.js`
- Modify: `frontend/js/bigmodel.js`
- Modify: `tests_js/helpers/shared_bigmodel_fake.mjs`
- Modify: `tests_js/prefs.test.mjs`、`tests_js/dialogs.test.mjs`、`tests_js/bigmodel.test.mjs`、`tests_js/window-transfer.test.mjs`
- Test: 上述 Node 测试；不另建简化 fake

**Interfaces:**
- Produces: `createUndoSettingsPrefs`、`parseResidentPercent`、`normalizeResidentPermille`、`effectiveUndoBudgetBytes`、`undoBudgetDialog`、`BigModel` 内存方法和事件。
- IPC 属性: JS 方法 `memoryMode()` 读取 `meta.memory_mode`；有效预算读取 `meta.effective_undo_budget_bytes`；进度事件固定 `big-memory-progress`。

- [ ] **Step 1: 先写偏好迁移、0.1% 解析、BigInt 公式和模型命令测试**

```javascript
test("撤销设置从旧 v1 迁移且双值持久化", () => {
  const storage = new MemoryStorage();
  storage.setItem("simpletable.undo_budget.v1", "32");
  const prefs = createUndoSettingsPrefs(storage);
  assert.deepEqual(prefs.get(), { budgetMiB: 32, residentPermille: 1000 });
  prefs.set({ budgetMiB: 128, residentPermille: 250 });
  assert.deepEqual(prefs.get(), { budgetMiB: 128, residentPermille: 250 });
  assert.equal(storage.getItem("simpletable.undo_budget.v1"), "32",
    "迁移不删除旧设置，便于回滚");
});

test("驻留百分比按十进制文本解析且只允许 0.1% 步长", () => {
  for (const [raw, expected] of [["0", 0], ["0.1", 1], ["100", 1000],
    ["999.9", 9999], ["1000.0", 10000]]) {
    assert.equal(parseResidentPercent(raw), expected);
  }
  for (const raw of ["-1", "1000.1", "1.23", "abc", "", "NaN"]) {
    assert.equal(parseResidentPercent(raw), null);
  }
});

test("有效预算使用整数向上取整和 BigInt 溢出边界", () => {
  assert.equal(effectiveUndoBudgetBytes({ budgetMiB: 1, residentPermille: 1,
    fileBytes: 1001n, memoryMode: true }), 1024n * 1024n);
  assert.equal(effectiveUndoBudgetBytes({ budgetMiB: 0, residentPermille: 1,
    fileBytes: 1001n, memoryMode: true }), 2n);
  assert.equal(effectiveUndoBudgetBytes({ budgetMiB: 1, residentPermille: 10000,
    fileBytes: 1001n, memoryMode: false }), 1024n * 1024n);
});

test("已有临时 RAM 提升仍调用 enter 但不调用 sort_load", async () => {
  const model = await createSharedFakeModel({ hasPath: true,
    totalBytes: 64 * 1024 * 1024, ramBacked: true });
  await model.enterMemoryMode();
  assert.equal(model.memoryMode(), true);
  assert.equal(model.calls().filter(([cmd]) => cmd === "big_sort_load").length, 0);
  assert.equal(model.calls().filter(([cmd]) => cmd === "big_memory_mode_enter").length, 1);
});
```

模型测试还覆盖：estimate/cancel/exit 参数；owner/lease 不匹配的进度被忽略；取消结果不刷新为 100%；exit 固定点请求原样返回；保存后刷新新预算；enter/exit 在 `TRANSFER_BLOCK_COMMANDS`，estimate 在 `TRANSFER_READ_COMMANDS`，cancel 可在迁移取消阶段执行。

- [ ] **Step 2: 确认 RED**

Run: `node --test tests_js/prefs.test.mjs tests_js/dialogs.test.mjs tests_js/bigmodel.test.mjs tests_js/window-transfer.test.mjs`

Expected: 新导出、命令和 fake 状态缺失导致失败；既有旧预算测试先保留，确认迁移兼容而不是删除断言。

- [ ] **Step 3: 实现版本化偏好与精确预算函数**

保留 `UNDO_BUDGET_PREF_KEY` 和旧导出供迁移/旧测试使用，新增：

```javascript
export const UNDO_SETTINGS_PREF_KEY = "simpletable.undo_settings.v2";
export const DEFAULT_RESIDENT_PERMILLE = 1000;
export const MAX_RESIDENT_PERMILLE = 10000;

/** 把用户输入的 0–1000.0 百分比精确转为整数千分比；非法输入返回 null。 */
export function parseResidentPercent(raw) {
  const text = String(raw).trim();
  const match = /^(\d{1,4})(?:\.(\d))?$/.exec(text);
  if (!match) return null;
  const value = Number(match[1]) * 10 + Number(match[2] || "0");
  return value <= MAX_RESIDENT_PERMILLE ? value : null;
}

/** 规范化内部存储的整数千分比，非法值回退 100%。 */
export function normalizeResidentPermille(raw) {
  const value = Number(raw);
  return Number.isInteger(value) && value >= 0 && value <= MAX_RESIDENT_PERMILLE
    ? value : DEFAULT_RESIDENT_PERMILLE;
}

/** 返回 BigInt 字节数；不在比例计算中使用浮点乘法。 */
export function effectiveUndoBudgetBytes({ budgetMiB, residentPermille,
  fileBytes, memoryMode }) {
  const budget = BigInt(budgetMiB) * 1024n * 1024n;
  if (!memoryMode) return budget;
  const bytes = typeof fileBytes === "bigint" ? fileBytes : BigInt(fileBytes);
  const permille = BigInt(normalizeResidentPermille(residentPermille));
  const scaled = (bytes * permille + 999n) / 1000n;
  return scaled > budget ? scaled : budget;
}

/** 格式化任意精度字节数；单位换算保留两位整数小数，不先转成 Number。 */
export function formatByteQuantity(numberOrBigInt) {
  const bytes = BigInt(numberOrBigInt);
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let whole = bytes;
  let fraction = 0n;
  let unit = 0;
  while (whole >= 1024n && unit < units.length - 1) {
    fraction = ((whole % 1024n) * 100n) / 1024n;
    whole /= 1024n;
    unit += 1;
  }
  return unit === 0 ? `${bytes} B`
    : `${whole}.${fraction.toString().padStart(2, "0")} ${units[unit]}`;
}

/** v2 单值保存 { budgetMiB, residentPermille }，旧 v1 仅提供普通预算。 */
export function createUndoSettingsPrefs(storage) {
  return {
    get() {
      try {
        const saved = JSON.parse(storage.getItem(UNDO_SETTINGS_PREF_KEY));
        if (saved) return {
          budgetMiB: normalizeUndoBudgetMiB(saved.budgetMiB),
          residentPermille: normalizeResidentPermille(saved.residentPermille),
        };
      } catch { /* 继续读取旧 v1。 */ }
      return { budgetMiB: createUndoBudgetPrefs(storage).get(),
        residentPermille: DEFAULT_RESIDENT_PERMILLE };
    },
    set({ budgetMiB, residentPermille }) {
      const value = { budgetMiB: normalizeUndoBudgetMiB(budgetMiB),
        residentPermille: normalizeResidentPermille(residentPermille) };
      storage.setItem(UNDO_SETTINGS_PREF_KEY, JSON.stringify(value));
      return value;
    },
  };
}
```

- [ ] **Step 4: 实现撤销设置对话框和 BigModel API**

`undoBudgetDialog(def, preview)` 接收 `{ budgetMiB, residentPermille }` 与 `{ hasPath, totalBytes, memoryMode }`，取消返回 `null`，确定返回两个设置值。对话框使用 `#undo-budget-mib` 和 `#undo-resident-percent` 两个输入，step 分别为 1 和 0.1，并直接显示：

```text
普通文档有效预算 = 普通预算

内存模式文档有效预算 =
    max（普通预算，当前后备文件总字节数 × 驻留撤销百分比）
```

两个 input 的 `input` 事件都调用同一个 `renderPreview()`，实时显示普通预算、当前文件大小、输入比例、当前有效预算、若开启后的有效预算；提高预算不预分配，降低预算可能淘汰最旧历史或建立固定点。无当前文件时显示“当前文档无磁盘后备，内存模式不可开启”，不伪造文件大小。

BigModel 增加以下完整实现。`_refreshMeta()` 已同时刷新 meta 并触发 `_emitUndo()`，所以成功退出只调用一次该方法；固定点取消不刷新，也不发表格 data 事件。

```javascript
async memoryEstimate() { return this._invoke("big_memory_mode_estimate"); }

async enterMemoryMode() {
  const outcome = await this._invoke("big_memory_mode_enter");
  if (!outcome.cancelled) await this._refreshMeta();
  return outcome;
}

async cancelMemoryModeLoad() { return this._invoke("big_memory_mode_cancel"); }

async exitMemoryMode({ allowFixedPoint = false, preflightId = null } = {}) {
  const outcome = await this._invoke("big_memory_mode_exit", {
    approval: { allowFixedPoint, preflightId },
  });
  if (!outcome.fixed_point) await this._refreshMeta();
  return outcome;
}

memoryMode() { return Boolean(this._meta?.memory_mode); }

/**
 * 订阅内存转换进度，只接收本 model 当前 session/lease。
 * @param {(done: number, total: number) => void} cb - 字节进度回调。
 * @returns {Promise<Function>} 取消监听函数。
 */
async onMemoryProgress(cb) {
  return this._listen("big-memory-progress", (e) => {
    const { session, lease, done, total } = e.payload;
    if (session === this._sid && lease === this._lease) cb(done, total);
  });
}
```

`enterMemoryMode` 即使 `ram_backed=true` 也必须调用后端，由后端只提升生命周期；已有 RAM 时后端不发送进度，前端只刷新一次 meta。保存仍沿用现有 `_afterEdit`/`_refreshMeta` 路径，以获得新有效预算、dirty 和撤销按钮状态。

- [ ] **Step 5: 扩展 shared fake 并运行 Node 回归**

shared fake 增加 `memoryMode=false`、`undoPolicy`、`effectiveUndoBudgetBytes`、四个命令和 `big-memory-progress` 定向事件。默认仍为 `total_bytes=1024`、`hasPath=false`，避免既有测试突然出现大文件入口。`big_memory_mode_enter` 在已有 RAM 时只翻转持久标志；`big_sort_exit` 在持久模式保留 RAM；save/saveAs 仅对持久模式保留 RAM 并按新大小重算；退出缩容返回 `memoryModeExit` 固定点。

`fixedPointMessage(request)` 在未知类型抛错前增加现有 camelCase 协议分支，继续复用文件尾部的固定点后果说明 `tail`：

```javascript
if (request.kind === "memoryModeExit") {
  return "退出内存模式将恢复普通撤销预算，可能淘汰最旧历史。"
    + "
取消则保持内存模式、RAM 后备和当前编辑现场。" + tail;
}
```

Run: `node --test tests_js/*.test.mjs`

Expected: 新旧偏好、公式、模型、迁移和 fake 测试全绿；没有第二套简化 fake。

- [ ] **Step 6: 提交**

```bash
git add frontend/js/prefs.js frontend/js/dialogs.js frontend/js/bigmodel.js tests_js
git commit -m "feat: 增加内存模式模型和撤销设置试算"
```

---

### Task 9: App 工具栏、非模态文档进度、状态栏与安全关闭

**Files:**
- Modify: `frontend/js/app.js`
- Modify: `frontend/js/windowlifecycle.js`
- Modify: `frontend/index.html` 的内联 `<style>` 和 `#grid-area`，只放文档内进度元素与样式
- Modify: `tests_js/window-close.test.mjs`、`tests_js/app-state.test.mjs`
- Test: Node 状态/关闭测试；真实 WebView2 在 Task 11

**Interfaces:**
- Consumes: Task 8 的模型方法、设置返回和固定点请求。
- Produces: `memoryOperations: WeakMap<BigModel, MemoryOperation>`、`settleMemoryOperation(model)`、文档级进度 UI、窗口关闭前 settle hook。

- [ ] **Step 1: 先写其他页签可编辑、任务归属和关闭顺序测试**

```javascript
test("窗口关闭确认后先取消并等待全部文档任务再 close_window", async () => {
  const calls = [];
  const coordinator = createWindowCloseCoordinator({
    tabs: () => [{ id: 1, dirty: true }],
    activateTab: async (id) => calls.push(["activate", id]),
    confirmDiscard: async () => true,
    settleDocumentTasks: async () => calls.push(["settle"]),
    closeWindow: async () => calls.push(["close_window"]),
  });
  assert.equal(await coordinator.requestClose(), true);
  assert.deepEqual(calls, [["activate", 1], ["settle"], ["close_window"]]);
});

test("取消关闭不取消文档任务", async () => {
  const calls = [];
  const coordinator = createWindowCloseCoordinator({
    tabs: () => [{ id: 1, dirty: true }],
    activateTab: async () => {},
    confirmDiscard: async () => false,
    settleDocumentTasks: async () => calls.push(["settle"]),
    closeWindow: async () => calls.push(["close"]),
  });
  assert.equal(await coordinator.requestClose(), false);
  assert.deepEqual(calls, []);
});
```

App 状态测试用两个 shared fake model：A 的 enter 被 `deferModelCommand` 阻塞时，B 的 `setData` 仍完成；A 的进度和完成只更新 A 的页签，不切回 A、不写 B 的 dirty/meta；关闭 A 时先 cancel，再等待 enter Promise，最后 close engine。

- [ ] **Step 2: 确认 RED**

Run: `node --test tests_js/window-close.test.mjs tests_js/app-state.test.mjs`

Expected: 缺少 `settleDocumentTasks` 和按 model 隔离的任务状态；旧“确认后直接 close_window”断言应按新安全顺序更新，不能删除取消关闭断言。

- [ ] **Step 3: 实现双设置启动/修改和工具栏状态**

启动仍在 `openPump.start()` 前恢复设置，但一次发送：

```javascript
const undoPrefs = createUndoSettingsPrefs(window.localStorage);
let undoSettings = undoPrefs.get();

async function applyUndoSettings(value) {
  await invoke("set_undo_budget", {
    budgetBytes: undoBudgetBytes(value.budgetMiB),
    residentPermille: value.residentPermille,
  });
  undoSettings = undoPrefs.set(value);
  await Promise.all(tabs.map((tab) => tab.model?._refreshMeta?.()));
}
```

`changeUndoBudget()` 传入当前 model 的 `hasPath/totalBytes/memoryMode`，等待对话框后只调用一次 `applyUndoSettings`，按钮在途禁用，成功后才持久化。状态栏保留“每文档 N MiB”并追加“驻留 P%”。

工具栏新增 `btn-memory-mode`，文案在“转为内存模式…”和“退出内存模式”间切换。`refreshBigUi()` 统一决定显隐和 disabled：仅 `hasPath && total_bytes >= BIG_THRESHOLD` 显示；索引中、保存、排序载入、结构生成或其他互斥任务禁用。小于 50 MiB 和无路径新建档不显示，不能用 `isBig` 代替实际大小和路径判断。

在现有工具栏构建函数中创建按钮，不在构建后重复追加：

```javascript
function createMemoryModeButton() {
  const button = document.createElement("button");
  button.id = "btn-memory-mode";
  button.type = "button";
  button.className = "hidden";
  button.textContent = "转为内存模式…";
  button.addEventListener("click", () => {
    if (model) void runMemoryModeCommand(model);
  });
  return button;
}
```

`index.html` 在 `#grid-area` 内、`#grid-scroll` 前插入唯一文档进度条；切换到其他 model 时它只反映当前页签，后台页签状态仍保存在 WeakMap：

```html
<div id="memory-progress" class="memory-progress hidden" role="status" aria-live="polite">
  <span id="memory-progress-text">准备内存模式</span>
  <progress id="memory-progress-bar" max="100" value="0"></progress>
  <button id="btn-memory-cancel" type="button">取消</button>
</div>
```

在同一文件的 `<style>` 中加入：

```css
#grid-area { position: relative; }
.memory-progress {
  position: absolute; z-index: 40; top: 8px; left: 50%;
  transform: translateX(-50%); display: flex; align-items: center; gap: 8px;
  max-width: calc(100% - 24px); padding: 7px 10px;
  color: var(--text); background: #fff; border: 1px solid var(--border);
  box-shadow: 0 2px 8px rgba(0,0,0,.16); font-size: 12px;
}
.memory-progress.hidden { display: none; }
.memory-progress progress { width: 180px; }
.memory-progress button { flex: none; }
```

- [ ] **Step 4: 实现按 model 隔离的非模态任务**

App 从 `prefs.js` 引入 Task 8 的 `formatByteQuantity`，并使用现有 `confirmDialog`、`confirmFixedPointDialog`。按钮命令在异步操作前固定捕获发起文档：

```javascript
async function runMemoryModeCommand(operationModel) {
  const kind = operationModel.memoryMode() ? "exit" : "enter";
  return runMemoryOperation(operationModel, kind, async () => {
    if (kind === "exit") {
      const first = await operationModel.exitMemoryMode();
      if (!first.fixed_point) return first;
      if (!(await confirmFixedPointDialog(first.fixed_point))) return first;
      return operationModel.exitMemoryMode({
        allowFixedPoint: true,
        preflightId: first.fixed_point.preflightId
          ?? first.fixed_point.preflight_id,
      });
    }
    const estimate = await operationModel.memoryEstimate();
    if (!estimate.can_enter) {
      throw new Error(estimate.reason || "当前文档不能开启内存模式");
    }
    const text = [
      "文件大小：" + formatByteQuantity(estimate.file_bytes),
      "转换私有内存上限：" + formatByteQuantity(estimate.limit_bytes),
      "当前可用物理内存：" + formatByteQuantity(estimate.available_physical_bytes),
      estimate.available_commit_bytes == null ? null
        : "Windows 可用 commit：" + formatByteQuantity(estimate.available_commit_bytes),
      estimate.available_physical_bytes < estimate.file_bytes
        ? "可用物理内存不足，可能发生换页。仍要继续吗？" : "是否继续？",
    ].filter((line) => line !== null).join("
");
    if (!(await confirmDialog(text))) return null;
    return operationModel.enterMemoryMode();
  });
}

const memoryOperations = new WeakMap();

async function runMemoryOperation(model, kind, action) {
  if (memoryOperations.has(model)) throw new Error("该文档已有内存模式任务");
  const tab = tabs.find((item) => item.model === model);
  const operation = { kind, promise: null, tabId: tab?.id };
  operation.promise = (async () => {
    try { return await action(); }
    finally {
      memoryOperations.delete(model);
      if (tab) refreshTabMemoryUi(tab);
    }
  })();
  memoryOperations.set(model, operation);
  if (tab) refreshTabMemoryUi(tab);
  return operation.promise;
}

async function settleMemoryOperation(model) {
  const operation = memoryOperations.get(model);
  if (!operation) return;
  try { await model.cancelMemoryModeLoad(); } catch { /* 会话可能已关闭。 */ }
  try { await operation.promise; } catch { /* 保留原任务错误，关闭继续。 */ }
}
```

所有 enter/cancel/exit 闭包在点击时把当前 model 固定为 `operationModel`，并记录 tab id；`await` 后只更新该 model/tab，不用可能已切换的全局 model。进度放在当前文档容器内的非模态条，含已读/总量和取消按钮；不调用 `progressDialog`，不覆盖工具栏、其他页签或其他窗口。发起文档的编辑命令由文档级 busy guard 拒绝，其他 model 完全不受影响。

开启确认框显示 X、`floor(X × 1.05)`、可用物理内存和 Windows 可用 commit。可用 commit 不足由后端拒绝；物理内存不足只明确警告换页风险，由用户决定。`EXTERNAL_CHANGED` 错误进入现有外部文件变化提示，不自动重载。

- [ ] **Step 5: 实现排序提示、退出确认与页签/窗口关闭**

退出内存模式复用固定点确认：`kind === "memoryModeExit"` 时文案明确“退出会恢复普通撤销预算，可能淘汰历史；取消则保持内存模式和现场”。取消返回不得刷新 data、清 dirty 或释放 RAM；确认重试携带同一 preflight id，成功后只刷新 meta、dirty 和撤销按钮。

状态栏分别显示“已排序”“临时排序 RAM”“内存模式”；新建无路径 RAM 不显示为临时排序或持久模式。`confirmExitSortDialog()` 和 `exitSortMode()` 在持久模式使用“恢复排序前行序，保留内存模式”，不得再统一宣称释放内存或回到 mmap。

`detachTab(id,{closeEngine:true})` 在 `model.close()` 前调用 `await settleMemoryOperation(model)`。窗口协调器新增 `settleDocumentTasks` 回调，用户确认关闭后遍历本窗口 tabs，逐个 cancel 并等待任务，再调用 `close_window`；后端 `big_close` 继续作为第二道锁等待保障。

`smokeSnapshot` 和 `window.__app` 增加只读内存状态及测试入口：`enterMemoryMode`、`cancelMemoryModeLoad`、`exitMemoryMode`、`settleMemoryOperation`，以及当前 model 的 operation kind。入口调用生产同一函数，不复制状态转换。

- [ ] **Step 6: 运行 Node 回归并提交**

Run: `node --test tests_js/*.test.mjs`

Expected: 设置双值一次上发；A 任务不阻塞 B；关闭顺序正确；排序提示和固定点取消语义全绿。

```bash
git add frontend/js/app.js frontend/js/windowlifecycle.js frontend/index.html tests_js
git commit -m "feat: 增加非模态内存模式界面和安全关闭"
```

---

### Task 10: 独立 release 峰值二进制、105% 硬断言与 CI/release 阻断

**Files:**
- Create: `src-tauri/src/diagnostics.rs`
- Create: `src-tauri/src/bin/memory_peak.rs`
- Modify: `src-tauri/src/lib.rs` 隐藏 diagnostics 导出
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/workflows/release.yml`
- Test: `memory_peak` 纯报告测试、独立子进程真实矩阵

**Interfaces:**
- Consumes: 真实 `BigFile` 打开/转换/保存、`SaveRamEvent`（含 `LoadEvent`）、`sample_process`。
- Produces: `memory_peak run|child`、`PeakReport`、`evaluate_peak`、每场景 JSON 报告。
- 禁止: 复制一套测试转换器、模拟分配器代替真实验收、只用内部 `live_reserved_bytes` 证明通过。

- [ ] **Step 1: 先写超 1 字节也失败和缺失采样也失败的报告测试**

```rust
#[test]
fn peak_hard_limit_rejects_one_byte_excess() {
    let x = 64 * 1024 * 1024;
    let allowed = x * 105 / 100;
    let base = PeakInput {
        x_bytes: x, baseline_private_bytes: 100,
        self_peak_private_bytes: 100 + allowed,
        external_peak_private_bytes: 100,
        self_sample_count: 2, external_sample_count: 2,
        ..PeakInput::test_defaults()
    };
    let pass = evaluate_peak(base.clone()).unwrap();
    assert!(pass.pass);
    assert_eq!(pass.remaining_bytes, 0);

    let fail = evaluate_peak(PeakInput {
        external_peak_private_bytes: 101 + allowed,
        ..base
    }).unwrap();
    assert!(!fail.pass);
    assert_eq!(fail.excess_bytes, 1);
}

#[test]
fn missing_external_samples_is_failure_not_a_pass_by_default() {
    let error = evaluate_peak(PeakInput {
        external_sample_count: 0,
        ..PeakInput::test_defaults()
    }).unwrap_err();
    assert!(error.contains("外部采样"));
}
```

- [ ] **Step 2: 确认 RED**

Run: `cd src-tauri && cargo test --bin memory_peak`

Expected: 缺少 binary 和 `evaluate_peak`；先修测试编译问题，再接受功能缺失 RED。

- [ ] **Step 3: 实现隐藏诊断入口，不复用 cfg(test) helper**

`lib.rs` 使用 `#[doc(hidden)] pub mod diagnostics;`，其他引擎模块仍保持私有。diagnostics 只暴露薄 wrapper：

```rust
#[doc(hidden)]
pub struct DiagnosticDocument { inner: BigFile }

impl DiagnosticDocument {
    pub fn open_complete(path: &Path) -> Result<Self, String> {
        Ok(Self { inner: BigFile::open_complete(path, false)? })
    }
    pub fn preload_source(&self) {
        let checksum = self.inner.backing.bytes().iter().fold(0u64, |sum, byte| {
            sum.wrapping_add(*byte as u64)
        });
        std::hint::black_box(checksum);
    }
    pub fn enter_memory_mode<O>(&mut self, cancel: &AtomicBool,
        options: LoadOptions, observer: O) -> Result<bool, String>
    where O: Fn(LoadEvent) + Sync {
        self.inner.enter_memory_mode_with_options(cancel, MEMORY_MODE_MIN_BYTES, options, observer)
    }
    pub fn live_reserved_bytes() -> usize { crate::ram_region::live_reserved_bytes() }
    pub fn backing_file_bytes(&self) -> u64 { self.inner.total_bytes }
    pub fn set_cell(&mut self, row: u64, col: u32, value: String) -> Result<(), String> {
        self.inner.set_cell(row, col, value, Approval {
            allow_fixed_point: false, preflight_id: None,
        }).map(|_| ())
    }
    pub fn save_with_ram_observer<O>(&mut self, dest: &Path, options: LoadOptions,
        observer: O) -> Result<(), String>
    where O: Fn(SaveRamEvent) + Sync {
        self.inner.save_to_observed(dest, false, Approval {
            allow_fixed_point: false, preflight_id: None,
        }, options, |_, _, _, _| {}, observer).map(|_| ())
    }
}
```

再暴露 `rows()` 和取消场景所需 wrapper，全部直接调用 Task 3–6 的生产内部函数。`open_complete` 来自 Task 3 提取的真实构造，不再依赖 `#[cfg(test)] open`；渐进索引 worker 的 Arc 释放仍由 Task 3 单测证明。

- [ ] **Step 4: 实现父子协议、5 ms 外部监测和每分配点自采样**

CLI 固定为：

```text
memory_peak run --scenario convert|cancel|commit-fail|read-fail|save|cancel-loop
                --file PATH --workers 1|2|4 --preload true|false
                --report PATH
memory_peak child --scenario SCENARIO --file PATH --workers N --preload BOOL
```

父进程协议：

1. 生成或校验合成夹具，spawn 当前 exe 的 `child`，stdin/stdout 均管道。
2. child 用真实引擎打开并等待构造完成，按矩阵决定是否 `preload_source`；预热采样器、进度 Vec 和报告分配。
3. child 记录转换前 `private_bytes` 基线，打印一行 `READY <json>` 并 flush，然后阻塞等父进程 GO；READY 前不算转换峰值。
4. parent 读 READY，先调用 `sample_process(child_pid)` 验证权限并预热，启动 monitor，再写 `GO\n`。monitor 每 5 ms 调 `sample_process(child_pid)`，记录私有字节、工作集、采样数、最大采样间隔和任何错误。
5. child 的 observer 在每个 `ReserveBegin/Reserved/WorkerBegin/ChunkBegin/ChunkReady/Progress/Finished` 和每个 `SaveRamEvent` 调用 `sample_current_process()`，保留全部采样点计数及最大值。
6. child 完成或按预期取消/恢复后打印 `RESULT <json>`；parent wait、停止并 join monitor。外部或自身采样任一错误都使场景失败。
7. 报告取 `max(self_peak, external_peak)` 作为实测峰值，不能只取较有利的一侧。

取消场景在第一个超过总量 25% 的 Progress 设置原子 cancel；commit/read-fail 使用 Task 2 隐藏 fault，但分配、读取、清理和恢复均走真实实现。cancel-loop 至少重复三次，并逐次记录 `live_reserved_bytes` 回到操作前数值。save 场景先真实进入内存模式、完成预热并记录含旧 RAM 的基线，再改一个单元格并保存；X 取保存前后文件大小较大者，同时断言 `OldRamReleased` 先于 `NewReserveBegin`。

- [ ] **Step 5: 实现完整数值报告与硬断言**

```rust
#[derive(Clone, Debug)]
pub struct PeakInput {
    pub profile: String,
    pub scenario: String,
    pub x_bytes: u64,
    pub baseline_private_bytes: u64,
    pub self_peak_private_bytes: u64,
    pub external_peak_private_bytes: u64,
    pub working_set_peak_bytes: Option<u64>,
    pub self_sample_count: u64,
    pub external_sample_count: u64,
}

#[cfg(test)]
impl PeakInput {
    fn test_defaults() -> Self {
        Self { profile: "test".into(), scenario: "convert".into(),
            x_bytes: 64 * 1024 * 1024, baseline_private_bytes: 100,
            self_peak_private_bytes: 100, external_peak_private_bytes: 100,
            working_set_peak_bytes: Some(200), self_sample_count: 2,
            external_sample_count: 2 }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PeakReport {
    pub platform: String,
    pub profile: String,
    pub scenario: String,
    pub file_bytes_x: u64,
    pub allowed_peak_increment_bytes: u64,
    pub baseline_private_bytes: u64,
    pub self_peak_private_bytes: u64,
    pub external_peak_private_bytes: u64,
    pub observed_peak_private_bytes: u64,
    pub actual_peak_increment_bytes: i128,
    pub remaining_bytes: i128,
    pub excess_bytes: i128,
    pub working_set_peak_bytes: Option<u64>,
    pub self_sample_count: u64,
    pub external_sample_count: u64,
    pub monitor_interval_ms: u32,
    pub pass: bool,
}

pub fn evaluate_peak(input: PeakInput) -> Result<PeakReport, String> {
    if input.self_sample_count == 0 { return Err("缺少转换器自身采样".into()); }
    if input.external_sample_count == 0 { return Err("缺少外部采样".into()); }
    let allowed_u128 = (input.x_bytes as u128).checked_mul(1050)
        .and_then(|v| v.checked_div(1000))
        .ok_or_else(|| "允许峰值计算溢出".to_string())?;
    let allowed_u64 = u64::try_from(allowed_u128)
        .map_err(|_| "允许峰值超出 u64 报告范围".to_string())?;
    let allowed = allowed_u128 as i128;
    let baseline = input.baseline_private_bytes as i128;
    let observed = input.self_peak_private_bytes.max(input.external_peak_private_bytes) as i128;
    let actual = observed - baseline;
    let remaining = allowed - actual;
    Ok(PeakReport {
        platform: std::env::consts::OS.into(), profile: input.profile,
        scenario: input.scenario, file_bytes_x: input.x_bytes,
        allowed_peak_increment_bytes: allowed_u64,
        baseline_private_bytes: input.baseline_private_bytes,
        self_peak_private_bytes: input.self_peak_private_bytes,
        external_peak_private_bytes: input.external_peak_private_bytes,
        observed_peak_private_bytes: observed as u64,
        actual_peak_increment_bytes: actual,
        remaining_bytes: remaining.max(0), excess_bytes: (-remaining).max(0),
        working_set_peak_bytes: input.working_set_peak_bytes,
        self_sample_count: input.self_sample_count,
        external_sample_count: input.external_sample_count,
        monitor_interval_ms: 5, pass: actual <= allowed,
    })
}
```

每行控制台摘要和 JSON 必须同时列出 X、允许峰值、基线、self/external/observed 峰值、实际增量、剩余余量或超出量、PASS/FAIL。`actual` 为负时如实保留，不用 saturating_sub 隐藏；`pass=false` 时进程非零退出。

本地 release 矩阵：

| 场景 | 夹具 | workers | preload |
| --- | ---: | ---: | --- |
| convert | 64、256、1024 MiB | 1、2、4 均跑 64 MiB；256/1024 跑默认 4 | false、true 均跑 64 MiB；大夹具 false |
| cancel、commit-fail、read-fail、cancel-loop | 64 MiB | 4 | 两种 |
| save | 64、256、1024 MiB | 4 | false |

Run: `cd src-tauri && cargo build --release --bin memory_peak && ./target/release/memory_peak run --scenario convert --file target/memory-peak/fixture-64.csv --workers 4 --preload false --report target/memory-peak/convert-64.json`

Expected: 进程退出 0，JSON `pass=true`，`actual_peak_increment_bytes <= allowed_peak_increment_bytes`。任何超出先修实现再重跑，不能调整硬上限。

- [ ] **Step 6: 把真实峰值加入 CI 和发布阻断**

CI Windows 与 macOS 都新增 release `memory_peak` 构建和 64 MiB 真实矩阵：convert workers 1/4 + preload false/true、cancel、commit-fail、read-fail、save。上传各自 JSON artifact，`if-no-files-found: error`。macOS 外部 `task_for_pid` 若需要更高权限，CI 用 runner 已有的非交互 `sudo -n` 运行同一 parent/child 验收；权限仍失败则 job 失败并保留 kern_return，禁止改为工作集。

`release.yml` 的 `test` job 在构建发布产物前执行 Windows release 64 MiB convert/cancel/read-fail/save 硬断言并上传报告。该步骤失败时 Windows/macOS 发布构建都不得开始。顶部过时测试数量注释改为实际数量，不预先写死本计划数字。

本任务只修改 workflow，不推送、不打 tag、不触发远程 release。

- [ ] **Step 7: 提交**

```bash
git add src-tauri/src/diagnostics.rs src-tauri/src/bin/memory_peak.rs src-tauri/src/lib.rs .github/workflows/ci.yml .github/workflows/release.yml
git commit -m "test: 将真实内存峰值设为发布硬门槛"
```

---

### Task 11: 真实 WebView2 大文件矩阵、文档和最终双构建验收

**Files:**
- Modify: `frontend/js/smoke.js`
- Modify: `scripts/verify-local.mjs` 仅在完整新增 smoke 需要时提高超时
- Modify: `README.md`、`docs/guide.md`、`docs/development.md`
- Modify: `TODO.md`、`task_plan.md`、`progress.md`、`findings.md`
- Test: 全量 Node、Rust、release 峰值、debug/release WebView2

**Interfaces:**
- Consumes: 所有前序任务；不新增产品状态或另一套测试入口。
- Produces: 可复查的完整测试输出、实际峰值 JSON、双 exe 时间戳和同步文档。

- [ ] **Step 1: 先增加独立大文件 smoke 并确认旧行为未被改写**

新建每次运行唯一的 64 MiB 合成 CSV，复用固定小缓冲分块写入；不扩大现有 `BIG_CSV`，不触碰 5000 行小文件统一 BigModel 测试。新增以下具名 smoke：

1. 小文件和无路径新建档不显示入口；大文件索引中/互斥任务中禁用。
2. 确认框准确显示 X、105% 上限、可用物理内存和 Windows 可用 commit。
3. 开启后编辑、撤销、重做；状态栏分别显示内存模式、排序和临时 RAM。
4. 中途取消不出现伪 100%，内容、dirty、undo/redo 不变。
5. 临时排序 RAM 提升不调用第二次 load，不显示第二次复制进度。
6. 内存模式排序和退出排序保留 RAM，提示不再宣称释放。
7. 保存、另存为和排序保存保持模式，并按新文件大小更新有效预算、dirty 和撤销按钮。
8. 退出缩容先取消：现场不变；再确认：回 mmap，保留允许的编辑和历史。
9. 设置双值只上发一次，公式、0.1% 步长和当前文档实时试算正确。
10. A 转换时切换 B 并编辑成功，A 完成后不错误刷新 B。
11. 外部文件变化进入既有提示，不自动重载或丢弃修改。
12. 关闭页签和关闭窗口都先 cancel 并等待对应 Promise，后端 close 随后完成。

保留临时排序保存回 mmap、退出排序释放临时 RAM、无路径新建保存回 mmap 等旧 smoke 原断言。若完整新增用例超过 180 秒，只提高 `verify-local.mjs` 超时并继续校验所有 ✓/✗ 行、唯一合计和 CLEANUP_PATH，不缩短断言迎合超时。

- [ ] **Step 2: 重编 debug/release 并运行真实 WebView2**

前端嵌入 exe，不能用旧二进制验证新 JS。运行 canonical gate：

Run: `node scripts/verify-local.mjs`

Expected: debug 和 release exe 都重新构建，exe 时间戳前进且不早于前端资源；两次完整 smoke 均 0 失败，报告行数和 CLEANUP_PATH 校验通过。若失败，保留实际输出并修复后重跑完整 gate。

- [ ] **Step 3: 运行全量逻辑、Rust 和 release 真实峰值**

Run: `node --test tests_js/*.test.mjs`

Run: `cd src-tauri && cargo test`

Run: `cd src-tauri && cargo test --bin memory_peak && cargo build --release --bin memory_peak`

随后执行 Task 10 的完整本地 64/256/1024 MiB release 矩阵，把所有 JSON 保存在 `src-tauri/target/memory-peak/`。报告必须逐场景满足硬公式；内部记账、工作集和可用内存只能作为附加信息。

Expected: 所有命令退出 0；Node/Rust 无失败；每个峰值 JSON 都有真实 self 和 external 样本且 `pass=true`。本计划写作阶段不声称这些测试已运行，执行者必须记录 fresh 实际数量。

- [ ] **Step 4: 同步使用与开发文档**

- `README.md` 简洁增加“内存模式”入口和与临时排序的区别，保持现有文档测试要求的最多 130 行；详细公式放 guide。
- `docs/guide.md` 删除“只有排序会把文件载入内存”和“退出排序总是释放内存”的过时描述，说明开启、取消、保存、另存为、退出排序和明确退出生命周期。
- `docs/development.md` 记录 reserve/commit、两阶段解除 mmap、私有字节口径、105% 公式、5 ms 外部采样、运行命令和 CI 权限失败不可降级。
- 设置文档写两条公式、默认 100%、0%–1000%、0.1% 步长、实时试算、提高不预分配和缩容影响。
- 明确第一阶段不改变列操作和 Ctrl+Z，不锁定物理页，不承诺系统总可用内存固定变化。
- `TODO.md`、`task_plan.md`、`progress.md`、`findings.md` 记录实际改动、真实测试数量、双 smoke 合计、每个尺寸的实测峰值/余量、CI 结果和未做的 macOS WKWebView 手测；没有证据的项目明确写未验证，不能补写“通过”。

- [ ] **Step 5: 最终自审与提交**

Run: `git status --short && git diff --check && git diff --stat`

检查只包含本任务文件，不包含业务 CSV、基准文件、旧 smoke 报告或无关未跟踪计划。核对最近一次提交的作者、提交者和完整消息，只显示用户本人且无共同作者尾注。

```bash
git add frontend/js/smoke.js scripts/verify-local.mjs README.md docs/guide.md docs/development.md TODO.md task_plan.md progress.md findings.md
git commit -m "docs: 记录内存模式用法和真实峰值验收"
```

最终报告列出 fresh Node/Rust 数量、debug/release smoke 数量、每个 release 峰值场景的 X/上限/基线/峰值/增量/余量、exe 时间戳和文档位置。没有实际运行的 macOS WKWebView 交互不得称为已验收。

---

## 规格覆盖自检（执行者完成后再次核对）

- 连续区域、Windows 逐块提交、macOS 匿名映射：Task 1。
- 有界多核、直接读最终区域、预留后解除 mmap、取消和失败：Task 2–3。
- `memory_mode` 与 `ram_backed` 分离、排序兼容、动态预算和退出确认：Task 5。
- 保存/另存为/排序保存、预算按新文件、旧 RAM 先释放和恢复：Task 6。
- 双设置一次更新所有及未来文档、IPC、owner/lease 事件和关闭取消：Task 7、9。
- 设置页公式、实时试算、0.1% 和整数运算：Task 5、8。
- 文档级非模态进度、其他页签可编辑、状态提示：Task 9。
- Windows/macOS 真实私有内存、分配点采样、外部 5 ms、105% 硬门槛：Task 4、Task 10。
- Node/Rust/WebView2、debug/release 构建、CI/release 阻断和文档：Task 10–11。
