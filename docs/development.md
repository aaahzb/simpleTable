# simpleTable 开发文档

[返回项目首页](../README.md)

## 开发

Rust + Tauri 2 外壳，前端是原生 HTML/CSS/JS，无构建步骤。需要 Rust 工具链（[rustup](https://rustup.rs/)）和 Node.js（仅跑前端单测）。

```bash
# 前端逻辑单测
node --test tests_js/*.test.mjs

# Rust 引擎单测（含默认忽略基准；最终数量以当次输出为准）
cd src-tauri && cargo test

# 编译并运行（前端资产编译时嵌入 exe，改完前端需重新 build）
cargo build
./target/debug/simpletable.exe

# 真机冒烟自检：真实 WebView2，完整对抗断言以当次报告合计为准
./target/debug/simpletable.exe --smoke   # 报告写 %TEMP%\simpletable_smoke_report.txt

# 双构建、前端嵌入时间检查和 debug/release 双真机冒烟
cd .. && node scripts/verify-local.mjs
```

`--smoke` 和 `--park` 是隔离诊断模式，不注册单实例，也不会把测试参数转交给正常窗口；迁移产生的子窗口不会重复运行启动路径或诊断套件。

平台验收边界：Windows 默认打开方式、真实鼠标/RDP 拖放和最后一个窗口由用户关闭仍需在 release exe 上验收。macOS 需在新的 `.app` 上验证 Finder 冷启动/已运行双击、同路径激活、跨窗口真实拖放与拖出，并确认 TXT/LOG 默认打开其他程序不受影响；Windows 环境的配置检查和编译通过不等于 macOS 实机验收。

推 `main` / PR 触发 CI（Windows：node + cargo test + 真实私有内存峰值硬门禁 + WebView2 冒烟；macOS arm64：编译 + cargo test + 经 `sudo -n` 的真实私有内存峰值硬门禁）；推 `v*` tag 先在 Windows 执行真实峰值发布硬门禁，再自动发 GitHub Release。Release 附件固定只有 Windows exe 与 macOS dmg 两项；峰值 JSON 仅上传为 Actions 构件，供排查时下载，不进入 Release。

## 内存模式实现与真实峰值门禁

- Windows 使用 `VirtualAlloc` 先 `MEM_RESERVE` 预留连续地址，再按 8 MiB 块 `MEM_COMMIT`；释放必须整段 `VirtualFree(base, 0, MEM_RELEASE)`。macOS 使用 `MAP_PRIVATE | MAP_ANON` 的匿名 `mmap/munmap`，不伪造 Windows commit。两阶段读取最多 4 个并行任务。
- 首次 mmap → RAM 转换在目标预留及整个复制期间保留原 M0，不建立独立块视图，也不主动整份 read_at；全部复制和文件状态检查成功后才安装 RAM 并一次释放 M0。取消或失败不运行重映射恢复。mmap 命中缓存与物理读盘不是同一指标，仍需 PhysicalDisk 实测；私有内存保持 floor(X×1.05) 硬门槛，工作集短时可能接近 2X，文件缓存也不要求立即归零。
- 正文变化需要重新物化时，同一 `SerializationContext` 按逻辑行分块并行执行计数遍和 RAM 生成遍：第一遍用 `CountingSink` 得到每块精确字节数，前缀和切出新正文的 `RangeSpan`；第二遍各 worker 用 `SliceSink` 直写自己的独占区间，不建立临时块缓冲，绝对私有峰值恒为旧正文 + 新正文 ≈ 2×X。块数 `min(4×workers, total/8192)`，每块至少 8192 行，worker 数 `min(available_parallelism, 16)`，scoped 线程栈 512 KiB。统计按 `bytes/rows/record_capacity` 直接求和、`line_capacity` 求和减 `(块数-1)` 合并，第二遍逐块与第一遍精确相等硬校验。`workers < 2`、总行数 `< 65536`、任何 `MaterializeFault` 注入，或非排序输出存在“原始末条无换行且其后还有插入行”的断行边界时回退原顺序路径。进度由调用线程每 10 ms 汇总各块原子完成行数，经 80 ms 门限发事件，阶段终态无条件补发 `done == total`。CountFinished 后准备历史、固定点与索引容量，NewRamReady 后才允许 RAM 提交和 OldRamReleased；DiskWriteBegin 只能随后发生。RAM 提交后的创建、写入、flush/sync、关闭、换名和外部变化错误均带 RAM_COMMITTED:，保留新 RAM、未保存状态及原路径。非内存保存仍只调用顺序 `serialize_document`，不经过上述并行路径。03 单空列不再拥有独立流式双写事务。
- 保存进度统一映射到 `MEMORY_SAVE_PROGRESS_TOTAL = 1000`：需要重新物化时计数 0–100、RAM 生成 100–700、写盘 700–990，无需物化时写盘 0–990，`PHASE_COMMIT` 固定 1000。`memory_save_weight` 与 `save_ram_progress` 共用该刻度并保证单调不减，任何阶段都不会提前到 100%；列操作 observer 通道在成功后才走完成帧，取消或失败只关闭对话框。
- 前端 `BigModel` 的 pending 列操作（插入/删除/重命名）会改变视图列映射，`_pushColOp`/`popColOp`/`clearColOps` 在 `_refreshMeta()` 前必须 `_invalidate()`；否则页缓存仍按旧源列序号排列，网格只更新表头而正文错位。真实界面探针要求插入列后表头含新列名、首行对应位置为空且旧列数据右移。
- 所有需要文档锁的产品命令（`big_meta`、`big_rows`、编辑、筛选读、列操作 pending、粘贴导入、`big_close` 等，冒烟诊断命令除外）必须声明为 `async` 并通过 `run_big_offthread` 进入 `spawn_blocking`：锁等待和长操作都不能发生在主线程。`with_big_owned` 是后台线程按 owner 定位文档的同一套两阶段授权。判据测试 `document_commands_do_not_block_the_calling_thread_on_the_file_lock` 让辅助线程持锁 400 ms，再手动 poll 命令 future 一次，要求立即 `Pending` 且耗时 <100 ms；若退回主线程原地等锁，该测试会以“实际已就绪”失败。内存模式物化按行分块多核并行（非内存保存仍顺序单线程），写盘仍顺序复制。前端层：`applyColOps()` 用 `columnApplyTask` 记录在途列操作整体（含完成后元数据刷新），`saveFile`/`saveFileAs` 开始前必须 `await waitForColumnApply()`；`app-state.test.mjs` 的“列操作在途时保存必须等它落定再开始”以事件顺序断言 `dirty:true → save-start → dirty:false`。
- Windows 硬口径是 `PROCESS_MEMORY_COUNTERS_EX.PrivateUsage`，macOS 是 `task_vm_info.internal + compressed`。工作集、`resident_size`、系统可用内存和 `phys_footprint` 只作诊断，不能替代 `private_bytes` 进入硬断言。
- 初次转换的 X 是文件字节数；保存路径的 X 取保存前后文件大小较大者。允许私有内存增量为 `floor(X × 1.05)`，实际增量为 `max(self_peak, external_peak) - baseline`，超过上限即 `pass=false` 和进程失败。
- 独立子进程在 READY 前打开真实文档、按需 preload 并预热分配；父进程先验证 PID 采样，再以 5 ms 外部监测执行 GO → RESULT → DONE。自身采样和外部采样任一缺失都失败，不能降级为只报工作集。
- 私有内存增量硬门槛为 floor(X × 1.05)。observed_peak_private_bytes 是绝对私有峰值，actual_peak_increment_bytes 是减基线增量，working_set_peak_bytes 独立报告；系统文件缓存和备用列表只作口径说明。阶段计时分别记录转换、计数、RAM 生成、索引准备/提交和磁盘写入。

```bash
# memory_peak 自身单元测试与 release 诊断二进制
cargo test --manifest-path src-tauri/Cargo.toml --bin memory_peak
cargo build --manifest-path src-tauri/Cargo.toml --release --bin memory_peak

# 单个真实 64 MiB convert 场景；夹具只在不存在时创建，绝不覆盖既有文件
src-tauri/target/release/memory_peak.exe run \
  --scenario convert \
  --file src-tauri/target/memory-peak/fixture-64.csv \
  --workers 4 --preload false \
  --report src-tauri/target/memory-peak/convert-64-w4-preload-false.json
```

完整矩阵覆盖 convert 的 64 MiB workers=1/2/4、preload=false/true、256 MiB 和 1 GiB，cancel/commit-fail/read-fail/cancel-loop 的两种 preload，以及 save 的 64 MiB、256 MiB 和 1 GiB。逐份 JSON 必须确认 self/external 样本非零、`observed=max(self,external)`、上限公式和 `monitor_interval_ms=5`；macOS 命令去掉 `.exe` 后缀。

CI 和 release 必须执行真实 release 硬断言并上传 JSON。Windows/macOS 的权限、PID 打开或采样失败都直接阻断，不能回退到工作集或其他诊断口径通过。Windows 本地通过不替代 macOS 真机验证；当前 macOS 匿名 mmap、真实私有字节采样及 WKWebView 内存模式交互仍需在 macOS 真机验收。

## 2026-11-12 Windows 本地验收快照

- 全量 Node：257 项通过、0 失败。Rust lib：357 项通过、0 失败、3 项忽略；显式 memory_peak：15 项通过、0 失败、1 项忽略。
- canonical 双构建双 smoke 退出码 0，debug/release 各 191 项通过、0 失败；每份报告的三个 CLEANUP_PATH 均已清理。debug/release simpletable.exe 和 release memory_peak.exe 均晚于最新 Rust 源码与前端资产。
- 当前 release 十九场景真实峰值矩阵全部通过并经独立脚本复核，self/external 均有样本，公式、5 ms 监测配置和 pass=true 均有效。保存场景的 X 已取保存前后较大文件大小。

下表单位全部为字节。采用峰值是自身与外部峰值的最大值，增量是采用峰值减基线，余量是 105% 上限减增量。

| 场景 | X | 105% 上限 | 基线 | 自身峰值 | 外部峰值 | 采用峰值 | 增量 | 余量 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| convert-64-w4-preload-false | 67108864 | 70464307 | 103907328 | 171220992 | 171220992 | 171220992 | 67313664 | 3150643 |
| convert-64-w1-preload-false | 67108864 | 70464307 | 104144896 | 171302912 | 171302912 | 171302912 | 67158016 | 3306291 |
| convert-64-w1-preload-true | 67108864 | 70464307 | 103653376 | 170835968 | 170835968 | 170835968 | 67182592 | 3281715 |
| convert-64-w2-preload-false | 67108864 | 70464307 | 104353792 | 171565056 | 154755072 | 171565056 | 67211264 | 3253043 |
| convert-64-w2-preload-true | 67108864 | 70464307 | 105320448 | 172548096 | 155738112 | 172548096 | 67227648 | 3236659 |
| convert-64-w4-preload-true | 67108864 | 70464307 | 104185856 | 171503616 | 171503616 | 171503616 | 67317760 | 3146547 |
| convert-256-w4-preload-false | 268435456 | 281857228 | 410402816 | 679047168 | 679047168 | 679047168 | 268644352 | 13212876 |
| convert-1024-w4-preload-false | 1073741824 | 1127428915 | 1624928256 | 2698883072 | 2698838016 | 2698883072 | 1073954816 | 53474099 |
| cancel-64-w4-preload-false | 67108864 | 70464307 | 105816064 | 156319744 | 156278784 | 156319744 | 50503680 | 19960627 |
| cancel-64-w4-preload-true | 67108864 | 70464307 | 104472576 | 154976256 | 154894336 | 154976256 | 50503680 | 19960627 |
| commit-fail-64-w4-preload-false | 67108864 | 70464307 | 105021440 | 105025536 | 105025536 | 105025536 | 4096 | 70460211 |
| commit-fail-64-w4-preload-true | 67108864 | 70464307 | 104566784 | 104570880 | 104570880 | 104570880 | 4096 | 70460211 |
| read-fail-64-w4-preload-false | 67108864 | 70464307 | 104448000 | 112812032 | 104452096 | 112812032 | 8364032 | 62100275 |
| read-fail-64-w4-preload-true | 67108864 | 70464307 | 103923712 | 112275456 | 103927808 | 112275456 | 8351744 | 62112563 |
| cancel-loop-64-w4-preload-false | 67108864 | 70464307 | 104939520 | 155443200 | 155357184 | 155443200 | 50503680 | 19960627 |
| cancel-loop-64-w4-preload-true | 67108864 | 70464307 | 105390080 | 155873280 | 155783168 | 155873280 | 50483200 | 19981107 |
| save-64-w4-preload-false | 75496455 | 79271277 | 171679744 | 180805632 | 180543488 | 180805632 | 9125888 | 70145389 |
| save-256-w4-preload-false | 301988871 | 317088314 | 678141952 | 713867264 | 714985472 | 714985472 | 36843520 | 280244794 |
| save-1024-w4-preload-false | 1207958535 | 1268356461 | 2699055104 | 2841763840 | 2844913664 | 2844913664 | 145858560 | 1122497901 |

最小余量出现在 convert-64-w4-preload-true，为 3146547 字节。此次只验证本地 Windows 自动门禁；未推送、未运行远程 CI/release，macOS 匿名 mmap、真实私有字节采样和 WKWebView 内存模式交互仍未做真机验收。

## 2026-11-13 内存模式并行物化本地验收

设计说明见 `docs/superpowers/specs/2026-11-13-parallel-memory-materialization-design.md`。

- 范围：仅内存模式的 `prepare_materialization`（执行列操作、保存前整理正文）。非内存保存仍只调用顺序 `serialize_document + IoSink`，字节输出与代码路径不变；RAM→磁盘写盘保持顺序。
- 并行模型：按逻辑行分块（`CHUNKS_PER_WORKER = 4`、`MIN_ROWS_PER_CHUNK = 8192`、worker 上限 16）。第一遍并行计数得到每块精确字节数，前缀和切出新正文 `RangeSpan`；第二遍各线程用 `SliceSink` 直写独占区间，无临时块缓冲，绝对峰值 ≈ 2×X，新增增量仍受 `floor(X×1.05)` 门槛。
- 回退：`workers < 2`、总行数 `< 65536`、任何 `MaterializeFault` 注入，或非排序输出存在“原始末条无换行且其后还有插入行”的断行边界时走原顺序路径。
- 一致性：`SerializationStats` 按 `bytes/rows/record_capacity` 直接求和、`line_capacity` 求和减 `(块数-1)`；生成遍逐块与计数遍精确相等硬校验，不一致即报错。
- 进度：各块独占 `AtomicU64`，调用线程每 10 ms 汇总并经 80 ms 门限发事件；阶段终态无条件补发 `done == total`。
- 测试：Rust lib 403 通过 / 0 失败 / 3 忽略（含并行物化与打开队列回归）；Node 277 通过 / 0 失败。既有字节精确测试全部保留通过，证明非内存路径零改动。

64 MiB release `memory_peak`（workers=4，preload=false）三场景均 `pass=true`、`commit_order_verified=true`（下表中的 `column-apply` 与“第一次/第二次 apply”是 Task 17 的历史数据；方案 A 后内存模式列操作不再物化，新口径见下一节）：

| 场景 | X | 105% 上限 | 采用峰值增量 | 余量 | count / generation / index |
| --- | ---: | ---: | ---: | ---: | --- |
| column-apply | 75496455 | 79271277 | 76271616 | 2999661 | 100 / 88 / 70 ms |
| column-save | 75496455 | 79271277 | 76251136 | 3020141 | 88 / 88 / 71 ms |
| save | 67108864 | 70464307 | 67846144 | 2618163 | 13 / 14 / 71 ms |

真实界面 A/B（Windows release exe，1.44 GB / 3200 万行 / 6 列 CSV，同一探针脚本跑完整流程“进入内存模式 → 删列 → 执行列操作 → 保存 → 再删列 → 立即保存”）：

| 指标 | 顺序版 exe | 并行版 exe | 对比 |
| --- | ---: | ---: | --- |
| 第一次 apply 墙钟 | 95260 ms | 14568 ms | 6.5× |
| 第二次 apply 墙钟 | ~63500 ms | ~11600 ms | 5.5× |
| 保存墙钟（apply 落定后） | 815 ms | 809 ms | 持平（写盘仍顺序） |
| apply 期间 `file_size` | 3 ms | 2 ms | 均未阻塞主线程 |
| 保存期间 `file_size` | 2 ms | 3 ms | 均未阻塞主线程 |
| 最终输出文件 | 1169973739 字节 | 1169973739 字节 | `cmp` 逐字节一致 |

两版 exe 均由同一工作区源码构建，唯一差异是本次并行物化改动；两次运行使用同一份夹具副本，操作序列相同。`CHECK_FLOW {savedClean:true, waitedForApply:true, pathUnchanged:true}` 两版均通过。新版第二次 apply 的墙钟包含自动点击“确认固定点”对话框前的等待，顺序版同样包含。

## 2026-11-14 内存模式列操作立即生效（方案 A）

- 范围：只在持久内存模式改变列操作语义。插入/删除/重命名/追加列一次用户手势只调用一次 `big_col_commit`，后台 `commit_col_ops(ops, approval)` 在文档锁内按序 `push_col_op` 后提交一条 `Op::Columns`；`pending_cols` 始终为空，工具栏不再出现「执行列操作」「撤销列操作预览」。普通模式保持 pending 预览 + 手动执行列操作完全不变。
- 正文不变：列提交只递增 revision/history_generation，`backing_identity()`、`ram_content_key`、`live_reserved_bytes` 和 `disk_body_dirty` 均不变；因此 2 GB 档列手势也是毫秒级。`apply_col_ops` 统一走 Schema-only 路径，删除原先的内存物化分支；`MaterializationOperation::Columns`/`MaterializationInput::columns` 仅保留给物化机制测试，标记为 dead_code 允许。
- 保存才物化：Ctrl+S 时 `persistent_ram_is_current()` 因内容键不匹配而走 `MaterializationInput::save_current`，恰好一次 `NewRamReady`/`OldRamReleased`，随后顺序写盘；第二次保存内容键已当前，0 次物化。删除 Source 列的保存仍需固定点确认；退出内存模式只拒绝 `disk_body_dirty` 的已物化未写盘正文，纯 Schema 变更允许退出并保留视图与历史。
- 保存时序：前端 `BigModel` 新增模型级列任务链 `_columnTask/_columnPending`，列手势按点击顺序串行；`save()`/`saveAs()` 仅在 `columnOpsBusy()` 时等待队列，空闲时不额外 `await`，避免把 `big_save` 登记推迟一个微任务而让互斥按钮短暂可用。`waitForColumnApply()` 同时等待旧普通模式任务与模型列任务。
- headerless 修复：虚拟插列/删列后切换首行模式，`set_headerless(true)` 改为按 `before.visible()` 构造首行数据（`Source(col)→base_headers[col]`、`Inserted→""`），长度恒等于可见列数；全 Source 顺序路径输出字节不变。
- 诊断：`memory_peak` 的 `column-apply` 期望改为 0 次物化、0 阶段事件、源文件 size/mtime 不变；`commit_order_verified` 支持 `expected_materializations == 0`。64 MiB release（workers=4，preload=false）三场景均 `pass=true`、`commit_order_verified=true`：

| 场景 | X | 105% 上限 | 采用峰值增量 | 余量 | count / generation / disk |
| --- | ---: | ---: | ---: | ---: | --- |
| column-apply | 67108864 | 70464307 | 4096 | 70460211 | 0 / 0 / 0 ms |
| column-save | 75496455 | 79271277 | 76263424 | 3007853 | 89 / 88 / 41 ms |
| save | 67108864 | 70464307 | 67891200 | 2573107 | 14 / 24 / 36 ms |

- 验证：Rust lib 407 通过 / 0 失败 / 3 忽略；memory_peak 21 通过 / 0 失败 / 1 忽略；Node 282 通过 / 0 失败；canonical `node scripts/verify-local.mjs` 重编 debug+release 后双 smoke 各 192 项通过、0 失败。内存模式四个列方法在成功时返回 `EditOutcome`，`columnOp` 因而能执行 `setDirty` 与“列操作已生效，可撤销；保存时整理正文”提示；新增 Node 回归覆盖四个手势的返回值。
- 真实界面探针（Windows release exe，`src-tauri/target/ui-col-insert-probe/probe-scheme-a.mjs`）：在内存模式下删列后立即调 `saveFile()`，保存期间每 100 ms 心跳并采样 `file_size`（不碰文档锁）：

| 夹具 | 操作 | 列手势墙钟 | 保存墙钟 | `file_size` 响应（列/保存） | 列手势前后源文件 | 输出 |
| --- | --- | ---: | ---: | ---: | --- | --- |
| 1.44 GB / 3200 万行 / 6 列 | 删“单价”→立即保存 | 7 ms | 15.4 s | 2 ms / 1 ms | size 与 mtime 不变 | 1,294,453,746 字节，与独立逐行计算一致 |
| 1.17 GB / 3200 万行 / 5 列（上一行输出） | 再删“数量”→立即保存 | 5 ms | 12.6 s | 2 ms / 1 ms | size 与 mtime 不变 | 与既有 4 列参考 `probe-flow-ref.csv` `cmp` 逐字节一致（`cmpStatus=0`） |
| 2.29 GB / 1550 万行 / 8 列 | 删“user”→立即保存 | 6 ms | 11.4 s | 2 ms / 1 ms | size 与 mtime 不变 | 2,146,750,036 字节，与独立逐行计算一致 |

三次保存都在删列后的第一次 Ctrl+S 完成固定点确认并恰好物化一次；界面心跳最大间隔分别为 115 / 115 / 113 ms，没有卡死。2.29 GB 夹具是授权文件 `E:\simpletable_hdd_test\bench_22gb.csv` 的独立副本，`price_j8888.csv` 未被读取或修改。探针日志与 `expected-sizes.mjs` 同目录保留。

## 打开队列的会话归属（2026-11-13 修复）

- `OpenRequestQueue.opened` 记录“路径 → 会话”，重复拖入/打开同一路径时只发 `Activate`，避免创建第二个会话。
- `big_close` 关闭文档会话时必须同步调用 `opens.remove_session(session)`（现由 `close_document` 统一完成）。只关文档不清理队列会让路径仍被判定为“已打开”，再次拖入时只发 `Activate` 指向已关闭会话；前端按会话/路径都找不到标签，界面表现为同一文件无法再打开。
- 同路径重载会替换会话（`register_new_document` 的 replace 分支），旧会话关闭后必须让 `opens.replace_session` 把打开归属转移到新会话；否则重载再关签会留下更早的已关闭会话，同一文件仍然无法打开，外部窗口也会误判“未打开”而创建第二份会话。
- 回归测试：`closing_document_forgets_open_path_so_same_file_reopens` 断言关闭后重新入队必须生成 `Open`；`replacing_document_session_rebinds_open_path_to_new_session` 断言重载后归属指向新会话且再关签后可重新 `Open`。真实界面探针 `probe-reopen-after-close.mjs` 执行“打开 → 点标签 × → 重新提交同一路径”，修复前超时失败、修复后新签打开。

## macOS 打包找不到主程序（2026-11-14 修复）

- 现象：`v2026.11.13.1` 的 macOS CD 在 `npx --yes @tauri-apps/cli@2 build --target aarch64-apple-darwin` 阶段直接失败：`failed to find main binary, make sure you have a package > default-run in the Cargo.toml file`。Windows 发布走 `cargo build --release`，所以只有 macOS 打包任务暴露。
- 根因：`src/bin/memory_peak.rs` 让包拥有两个二进制（`simpletable` 和 `memory_peak`），而 `src-tauri/Cargo.toml` 没有声明主程序。Tauri CLI 2 在 cargo 元数据里无法判定哪个是应用主 binary，就在编译前退出。
- 修复：在 `[package]` 增加 `default-run = "simpletable"`；不改二进制名、不改产物路径。`bundle identifier` 结尾 `.app` 只是警告，非本次失败原因；它同时决定 macOS bundle ID 与各平台应用数据目录，改动会迁移用户数据，未一并修改。
- 验证：修复前用同一条 `npx --yes @tauri-apps/cli@2 build --debug --no-bundle` 复现原始错误；修复后在仓库根和 `src-tauri` 两个工作目录都能找到主程序并成功构建 `simpletable.exe`。`cargo build --release` 与 Rust lib 407 通过 / 3 忽略保持全绿。`aarch64-apple-darwin` 产物仍需 macOS runner 实际打包验证。

## 项目结构

```
frontend/            纯 HTML/CSS/JS：自绘虚拟滚动网格 + 编辑窗格 + 对话框
  js/bigmodel.js     统一数据模型（全表唯一：窗口缓存 / 渐进索引轮询 / 内存排序 / 乐观编辑）
  js/smoke.js        --smoke 对抗自检套件（含 64 MiB 持久内存模式生命周期）
src-tauri/           Tauri 2 外壳 + Rust 数据引擎（mmap / 行索引 / 覆盖层 / 流式保存）
tests_js/            node:test 逻辑层单测
scripts/             截图 / 采样巡检脚本（开发用）
docs/img/            README 截图
```

## 规则 CSV 单空列插入的 RAM 保存快路径（2026-11-12）

- 适用条件：持久 `Backing::Ram(RamBuffer::Region)`、CSV、恒等行序、输出编码不变、空覆盖层、空 tombstone、单段连续 Src、恰好一个 Inserted 空列且其他 Source 列顺序不变。每条记录字段数必须等于原列数，引号闭合，记录物理范围连续且覆盖到文件末尾。
- `raw_empty_field_insert_offset` 只扫描引号外逗号并找到一个插入位置；原字段字节、引号风格、多行字段、BOM 和 CRLF/LF/无末行换行均透传。每条记录只新增一个字节逗号，不逐记录构造字符串列表或重新编码。
- 第一遍旧 RAM 扫描得到精确 `new_bytes` 和最大记录长度；新 RAM 只预留地址，按 64 KiB 私有页提交。第二遍使用有界块，同时 `write_all` 到临时 CSV 并 `RamRegion::append` 到新 RAM，只释放已经完整写出的旧 RAM 前缀。每块最多发一次 `StreamProgress`。
- 块容量为 `min(max(options.chunk_bytes, 4096), 8 MiB, max(new_bytes / 200, 4096))`。单条记录加一个字节超过块容量时回退旧路径，避免为异常大记录额外持有大缓冲。
- 完成后先释放旧 RAM 和旧索引，再直接扫描新 RAM 建 `line_off/rec_lines`。不建立临时文件 mmap，不调用 `prepare_memory_reopen`，因此没有保存后的整份文件读回。
- 历史、撤销预算与紧急恢复预算在消费旧 RAM 前准备。提交或写盘失败先释放失败的新 RAM，再恢复旧 RAM；换名最多重试五次，最终失败保留完整临时文件和原目标文件，并恢复路径、历史、revision、epoch、generation 与 dirty 状态。
- 硬门槛不变：X 取保存前后较大文件大小，self/external 私有峰值增量的较大者不得超过 `floor(X × 1.05)`。应用层顺序 `write_all` 不等于已证明物理连续写；物理盘计数器只能另行描述读写量、请求大小、时延和后台缓存刷新。

## 2026-11-12 首次 mmap 预建块视图验收

- 本节是 Task 13 的新实现证据，上方 03 保存快路径验收快照保持原样。首次转换在 M0 有效时预建全文件独立只读块视图和最终连续匿名 RAM，两者成功后才解除 M0；保存后文件重载仍走 File adapter。
- 取消与进度共用完成边界：每块复制后先检查 cancel，再发布 Progress；取消先生效时返回 Cancelled 且不发送 done==total，最终进度发布后不再因同一取消标志回滚。回归用例 cancel_after_final_copy_does_not_emit_fake_hundred_percent 固定了最后复制期间取消的时序。
- 首轮十九场景快照保留在 src-tauri/target/memory-peak-mmap-1794491845，最小余量 3,056,435 字节；它不是取消竞态修复后的最终 exe 证据。
- 当前最终 exe 的 fresh release 十九场景报告位于 src-tauri/target/memory-peak-mmap-1794495018。runner 退出 0，严格校验退出 0 并输出 MATRIX_VALIDATED 19；全部满足私有增量不超过 floor(X×1.05)，最小余量 3,072,819 字节。工作集峰值见同目录 summary.json，不加入私有内存判断，也不承诺复制期间工作集始终低于 2X。

| 首次转换路径 | 报告 | 状态 | 转换 ms | 样本 | E 盘读字节 | 私有峰值 | 工作集峰值 |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| 旧 read_at 基线 | src-tauri/target/physical-read-baseline-1794490561/physical-read.json | verified_zero | 828 | 60 | 0 | 2480574464 | 2478829568 |
| 新独立块 mmap，首轮 | src-tauri/target/physical-read-new-1794492054/physical-read.json | verified_zero | 1370 | 60 | 0 | 2480455680 | 2478702592 |
| 新独立块 mmap，最终 exe | src-tauri/target/physical-read-new-1794495018/physical-read.json | verified_zero | 1370 | 60 | 0 | 2479980544 | 2478272512 |

三轮均为同一 E 盘合成文件、workers=1、preload=true、covered=true，最终轮自采样 827 次。旧路径本轮也为物理零读取，因此新路径不能被描述为已经实测降低物理读盘；单次耗时也不作为吞吐结论。最终探针后源文件仍为 2,286,250,041 字节，mtime 为 2026-11-11T11:24:40.983Z，与任务开始时一致。

当前最终 exe 的 fresh 私有峰值逐场景数据如下，单位全部为字节：

| 场景 | X 字节 | 允许增量 | 基线私有字节 | 自身峰值 | 外部峰值 | 观测峰值 | 实际增量 | 剩余余量 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| convert 64 MiB, workers=4, preload=false | 67108864 | 70464307 | 103391232 | 170782720 | 170782720 | 170782720 | 67391488 | 3072819 |
| convert 64 MiB, workers=1, preload=false | 67108864 | 70464307 | 103755776 | 170938368 | 162549760 | 170938368 | 67182592 | 3281715 |
| convert 64 MiB, workers=1, preload=true | 67108864 | 70464307 | 107413504 | 174612480 | 166223872 | 174612480 | 67198976 | 3265331 |
| convert 64 MiB, workers=2, preload=false | 67108864 | 70464307 | 103690240 | 170958848 | 170958848 | 170958848 | 67268608 | 3195699 |
| convert 64 MiB, workers=2, preload=true | 67108864 | 70464307 | 104644608 | 171913216 | 171913216 | 171913216 | 67268608 | 3195699 |
| convert 64 MiB, workers=4, preload=true | 67108864 | 70464307 | 103862272 | 171233280 | 171233280 | 171233280 | 67371008 | 3093299 |
| convert 256 MiB, workers=4, preload=false | 268435456 | 281857228 | 408002560 | 676716544 | 676593664 | 676716544 | 268713984 | 13143244 |
| convert 1024 MiB, workers=4, preload=false | 1073741824 | 1127428915 | 1624408064 | 2698444800 | 2698444800 | 2698444800 | 1074036736 | 53392179 |
| cancel 64 MiB, workers=4, preload=false | 67108864 | 70464307 | 103563264 | 162553856 | 162398208 | 162553856 | 58990592 | 11473715 |
| cancel 64 MiB, workers=4, preload=true | 67108864 | 70464307 | 105066496 | 164057088 | 163950592 | 164057088 | 58990592 | 11473715 |
| commit-fail 64 MiB, workers=4, preload=false | 67108864 | 70464307 | 103964672 | 104095744 | 103964672 | 104095744 | 131072 | 70333235 |
| commit-fail 64 MiB, workers=4, preload=true | 67108864 | 70464307 | 104062976 | 104198144 | 104067072 | 104198144 | 135168 | 70329139 |
| read-fail 64 MiB, workers=4, preload=false | 67108864 | 70464307 | 107020288 | 115531776 | 107024384 | 115531776 | 8511488 | 61952819 |
| read-fail 64 MiB, workers=4, preload=true | 67108864 | 70464307 | 104890368 | 113397760 | 104890368 | 113397760 | 8507392 | 61956915 |
| cancel-loop 64 MiB, workers=4, preload=false | 67108864 | 70464307 | 104046592 | 163016704 | 162906112 | 163016704 | 58970112 | 11494195 |
| cancel-loop 64 MiB, workers=4, preload=true | 67108864 | 70464307 | 103866368 | 162861056 | 162750464 | 162861056 | 58994688 | 11469619 |
| save 64 MiB, workers=4, preload=false | 75496455 | 79271277 | 171188224 | 180183040 | 180117504 | 180183040 | 8994816 | 70276461 |
| save 256 MiB, workers=4, preload=false | 301988871 | 317088314 | 678371328 | 714096640 | 715210752 | 715210752 | 36839424 | 280248890 |
| save 1024 MiB, workers=4, preload=false | 1207958535 | 1268356461 | 2698698752 | 2841407488 | 2844758016 | 2844758016 | 146059264 | 1122297197 |

- 第一轮 canonical 的 debug smoke 为 190/191，唯一失败是取消结果出现 done==total，release 当轮未运行；原始报告 src-tauri/target/smoke-debug-1794492657448.txt 保留。确定性回归先复现 RED，再调整复制后取消检查与进度发布顺序。
- 修复后的第二轮 canonical 双构建、前端嵌入时间戳检查和双 smoke 全部通过，最终输出 dual build, embed timestamps, and dual smoke: PASS。debug 报告 src-tauri/target/smoke-debug-1794494727122.txt 与 release 报告 src-tauri/target/smoke-release-1794494866840.txt 均为 191 项通过、0 失败。独立校验 src-tauri/target/smoke-final-validation.json 确认两份报告均无 watchdog，恰好三个 CLEANUP_PATH，且清理目录均不存在。
- Task 13 fresh 全量测试：Rust lib 364 项通过、0 失败、3 项忽略；memory_peak 15 项通过、0 失败、1 项忽略；main 和 doc-tests 均为 0 项且无失败。Node 263 项通过、0 失败。
- 全仓 cargo fmt --check 仍退出 1，涉及 16 个文件，原始日志为 src-tauri/target/task6-cargo-fmt-after-cancel.log；ram_load.rs 定向 rustfmt --check 通过且未出现在全仓差异中。未为重排无关代码扩大改动，不能将全仓格式检查记为通过。
- macOS 真机或远程等价测试未验证；Windows 结果不能替代 macOS 的对齐、析构和私有内存口径验证。
