# 内存模式并行物化设计

日期：2026-11-13
状态：已实施并通过本地硬门槛；真实 2 GB 界面验收记录见文末

> 执行说明（2026-11-14）：执行列操作的物化部分已被方案 A 取代——内存模式下插入/删除/重命名/追加列立即提交 Schema、不物化正文，保存时才统一物化；并行物化现仅用于保存路径。以下内容保留为 Task 17 的历史设计与验收记录。

## 1. 背景

内存模式下，删除列后“执行列操作”和保存前整理正文需要对整份文档做两遍序列化（计数一遍、生成一遍）。此前两遍都是顺序单线程：2 GB / 3200 万行夹具上 apply 墙钟约 82–111 s，期间单核占满。用户明确要求“空间换速度”：允许物化绝对峰值达到正文的 2×，换取多核并行。

边界：只改内存模式的物化路径；非内存模式保存仍走原有顺序单线程 `serialize_document + IoSink`，输出字节和代码路径不变。RAM→磁盘写盘保持顺序。

## 2. 目标与非目标

### 2.1 目标

- 内存模式的计数遍与生成遍按逻辑行区间多核并行。
- 生成遍各工作线程直接序列化进新正文的独占切片，**不建立临时块缓冲**。
- 绝对私有内存峰值 = 旧正文 + 新正文 ≈ 2×X；新增增量仍满足 `floor(X×1.05)` 硬门槛（CI/release 口径不变）。
- 输出与顺序物化逐字节一致；统计逐字段一致；进度事件形状与前端协议不变。
- 小文件、单 worker、任何故障注入时回退原顺序路径，旧语义与旧测试不变。

### 2.2 非目标

- 不并行 RAM→磁盘写盘，不改变临时文件、flush/sync、原子换名和失败恢复语义。
- 不并行索引提交（本期仍顺序扫描）。
- 不为非内存模式引入任何并行或缓冲。
- 不新增用户可见设置项；2× 是固定内部策略。

## 3. 核心模型

### 3.1 两遍 + 精确区间直写

```text
旧正文 R0 + 编辑状态
   │
   │ 第一遍：按行区间并行计数（CountingSink + 独立 IndexCounter）
   ▼
每块精确 SerializationStats
   │  前缀和
   ▼
新正文 R1 的 RangeSpan 列表（精确铺满，互不重叠）
   │
   │ 第二遍：各线程把对应行区间序列化进 R1 的独占切片（SliceSink）
   ▼
逐块统计与第一遍硬校验一致 → 合并统计 → R1 完成态
   │
   │ 提交后一次释放 R0
   ▼
当前唯一正文 R1
```

峰值天然为 R0 + R1 ≈ 2×X，未引入额外块缓冲。

### 3.2 行区间切分

- `chunk_spans(total, workers)`：块数 `min(desired, cap)`，`desired ≈ 4×workers`，每块至少 `MIN_ROWS_PER_CHUNK = 8192` 行；余数前置；块长差 ≤1；无空块；首尾相接覆盖 `[0, total)`。
- 块 0 负责 BOM + 表头前缀及其统计。
- 逻辑行区间边界都落在记录/物理行起点，块内引号状态与行计数等价于新建 `IndexCounter` 的初始态。

### 3.3 共享序列化上下文

`SerializationContext` 一次性构建并在所有块间共享：`document`/`schema`/`saved_headers`/`sorted` 引用，加 `edited: HashSet<u32>`、`must_encode_source`、`raw_insert_col`、`total`、`seg_starts` 前缀和。避免每块重建 `HashSet` 撑破内存门槛。

`serialize_row_range(ctx, sink, span, include_prefix, on_rows)` 是唯一入口；`serialize_document` 退化为 `[0, total)` 整区间包装，非内存路径行为不变。

### 3.4 统计可加性

- `bytes`/`rows`/`record_capacity` 直接求和。
- `line_capacity` 求和后减 `(块数 - 1)`：每个新建 `IndexCounter` 都从 `line_capacity = 1`（`line_off[0] = 0`）起步。
- 块 0 额外包含 BOM + 表头行。
- 合并规则由边界矩阵测试覆盖；并行生成还逐块与第一遍统计做**精确相等硬校验**，不一致立即报错，不做静默容差。

### 3.5 `RamRegion::for_each_range_mut`

`RamRegion` 新增预计算变长区间并发写：

```rust
pub fn for_each_range_mut<F, O, P>(
    &mut self,
    ranges: &[RangeSpan],
    workers: usize,
    observer: O,
    writer: F,
    poll: P,
) -> Result<(), String>
where
    F: Fn(usize, &mut [u8]) -> Result<(), String> + Sync,
    O: Fn(RegionEvent) + Sync,
    P: FnMut();
```

- 启动前校验：未 started、`workers > 0`、区间非空、长度 >0、不越界、`offset == cursor` 顺序相接、最终恰好铺满整个区域；任一失败直接拒绝。
- `range_mode` 与 `append`/chunk 写入互斥：区间模式启用后 `append` 返回“已经开始写入”。
- scoped threads（512 KiB 栈）+ 原子下标发放 + 首错停止；`enable_fault_at` 落在 `[offset, offset+len)` 时注入提交失败。
- 只有 writer 成功才累计写入字节与完成区间数；每 worker 无条件发 `WorkerEnd`。
- 调用线程 `poll` 轮询等待，回调不跨线程。

### 3.6 并行选择与回退

`should_parallelize(document, total, workers, fault, sorted)` 同时满足才并行：

1. `workers >= 2`（`min(available_parallelism, 16)`）；
2. `total >= PARALLEL_MIN_ROWS = 65_536`；
3. 没有 `MaterializeFault` 注入；
4. 排序输出，或非排序输出且不存在“断行边界”。

断行边界指：原始文件末条记录没有末尾换行，且其后还存在插入行。此时块边界重新起算的原始运行合并会改变拼接结果，必须回退顺序路径。

### 3.7 进度

- 每块一个独占 `AtomicU64` 记录本地完成行数，无跨线程争用。
- 调用线程等待期间每 10 ms 汇总各块原子值，经现有 `progress_gate`/`PROGRESS_MIN_GAP`（80 ms）门限发 `progress(done, total, phase, done)`。
- 回调始终在调用线程，`progress` 无需 `Send`；中间进度单调；阶段终态无条件补发 `done == total`。

### 3.8 内存记账

- 工作线程栈计入进程私有内存；64 MiB 场景 105% 余量约 2.5–3.0 MB，因此用 `std::thread::Builder::spawn_scoped` 配 512 KiB 栈。
- 新增测试断言并行物化前后线程内预留字节增量**恰为一份新正文**，且没有额外区域。
- `PARALLEL_MATERIALIZATIONS` 使用 `thread_local` 计数器，避免进程内并发测试相互干扰。

## 4. 测试覆盖

- `ram_region`：区间铺满校验、越界/重叠拒绝、故障注入、只记成功区间、与顺序 API 互斥（9 项）。
- `serialize_tests`：整份 vs 区间矩阵（UTF-8/BOM/GBK/引号多行/CRLF/LF/末行无换行/纯文本）、`include_prefix`、中间区间拼接、`SerializationStats` 合并规则。
- `materialize_tests`：
  - `chunk_spans` 覆盖与无空块；
  - 并行选择矩阵（workers/行数/故障/断行边界）；
  - 并行计数与顺序统计逐字段一致（1/2/3/7 块）；
  - 进度单调与终态；
  - 并行生成与顺序物化逐字节一致（格式矩阵、编辑、插入行、列操作、排序、块矩阵）；
  - 并行只新增一份正文（线程内预留字节）；
  - 故障注入回退顺序且状态不变；
  - `prepare_materialization` 接线与进度阶段。

## 5. 门槛与实测

### 5.1 自动门槛

- `cargo test --lib`：401 通过 / 0 失败 / 3 忽略。
- `node --test tests_js/*.test.mjs`：277 通过 / 0 失败。
- 64 MiB release `memory_peak`（workers=4，preload=false）：

| 场景 | X | 105% 上限 | 采用峰值增量 | 余量 | count/generation/index |
| --- | ---: | ---: | ---: | ---: | --- |
| column-apply | 75496455 | 79271277 | 76271616 | 2999661 | 100/88/70 ms |
| column-save | 75496455 | 79271277 | 76251136 | 3020141 | 88/88/71 ms |
| save | 67108864 | 70464307 | 67846144 | 2618163 | 13/14/71 ms |

三场景均 `pass=true`、`commit_order_verified=true`。

### 5.2 真实界面验收

见 `docs/development.md` 的“内存模式并行物化”小节（2026-11-13 实测记录）。

## 6. 风险与对策

| 风险 | 对策 |
| --- | --- |
| 线程栈计入私有内存 | 512 KiB scoped 栈；64 MiB 场景实测余量 2.5 MB 以上 |
| 编辑集合每块重建撑爆内存 | `SerializationContext` 一次性构建并共享 |
| 统计不可加 | `line_capacity` 减块数-1，边界矩阵逐字段比对；生成遍逐块硬校验 |
| 断行边界破坏字节一致性 | `has_line_split_boundary` 检测后回退顺序路径 |
| Windows 页粒度提交重叠 | `enabled_bytes` 允许重叠累计（仅诊断），写入区间互不相交 |
| 进度乱序/风暴 | 工作线程只累加原子值，调用线程 80 ms 门限轮询并补发终态 |
