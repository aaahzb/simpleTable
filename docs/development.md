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

推 `main` / PR 触发 CI（Windows：node + cargo test + 真实私有内存峰值硬门禁 + WebView2 冒烟；macOS arm64：编译 + cargo test + 经 `sudo -n` 的真实私有内存峰值硬门禁）；推 `v*` tag 先在 Windows 执行真实峰值发布硬门禁，再自动发 GitHub Release（Windows exe + macOS dmg），峰值 JSON 均上传为构件。

## 内存模式实现与真实峰值门禁

- Windows 使用 `VirtualAlloc` 先 `MEM_RESERVE` 预留连续地址，再按 8 MiB 块 `MEM_COMMIT`；释放必须整段 `VirtualFree(base, 0, MEM_RELEASE)`。macOS 使用 `MAP_PRIVATE | MAP_ANON` 的匿名 `mmap/munmap`，不伪造 Windows commit。两阶段读取最多 4 个并行任务。
- 转换预留成功后、任何文件读取开始前，先解除完整源 mmap；不能把只读文件映射当作可零复制转换的匿名 RAM。保存和另存为同样分离旧 RAM、旧索引、新索引构建缓冲与新 RAM 的生命周期，避免双 RAM 或双索引同时驻留；失败恢复必须保留可读索引、修改和历史。
- Windows 硬口径是 `PROCESS_MEMORY_COUNTERS_EX.PrivateUsage`，macOS 是 `task_vm_info.internal + compressed`。工作集、`resident_size`、系统可用内存和 `phys_footprint` 只作诊断，不能替代 `private_bytes` 进入硬断言。
- 初次转换的 X 是文件字节数；保存路径的 X 取保存前后文件大小较大者。允许私有内存增量为 `floor(X × 1.05)`，实际增量为 `max(self_peak, external_peak) - baseline`，超过上限即 `pass=false` 和进程失败。
- 独立子进程在 READY 前打开真实文档、按需 preload 并预热分配；父进程先验证 PID 采样，再以 5 ms 外部监测执行 GO → RESULT → DONE。自身采样和外部采样任一缺失都失败，不能降级为只报工作集。

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

## 2026-11-11 Windows 本地验收快照

- 全量 Node：257 项通过、0 失败。Rust lib：348 项通过、0 失败、3 项忽略；显式 memory_peak：15 项通过、0 失败、1 项忽略。Rust 仍报告既有 4 个 dead_code 警告。
- canonical 双构建双 smoke 退出码 0，debug/release 各 191 项通过、0 失败；独立文本校验确认合计唯一、无 watchdog，且每份报告的三个 CLEANUP_PATH 均已清理。
- 最新前端时间为 2026-11-11 18:38:13.143 +0800；debug simpletable.exe 为 18:40:43.159，release simpletable.exe 为 18:41:04.825，release memory_peak.exe 为 18:41:05.618，均不早于前端。
- 十九场景 release 真实峰值矩阵全部通过，报告不早于当前 memory_peak.exe，self/external 均有样本，公式、5 ms 监测配置和 pass=true 均经独立脚本复核。

下表单位全部为字节。采用峰值是自身与外部峰值的最大值，增量是采用峰值减基线，余量是 105% 上限减增量。

| 场景 | X | 105% 上限 | 基线 | 自身峰值 | 外部峰值 | 采用峰值 | 增量 | 余量 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| convert-64-w4-preload-false | 67108864 | 70464307 | 103706624 | 171073536 | 171073536 | 171073536 | 67366912 | 3097395 |
| convert-64-w1-preload-false | 67108864 | 70464307 | 103624704 | 170811392 | 170811392 | 170811392 | 67186688 | 3277619 |
| convert-64-w1-preload-true | 67108864 | 70464307 | 103772160 | 170930176 | 170930176 | 170930176 | 67158016 | 3306291 |
| convert-64-w2-preload-false | 67108864 | 70464307 | 103751680 | 170987520 | 170942464 | 170987520 | 67235840 | 3228467 |
| convert-64-w2-preload-true | 67108864 | 70464307 | 103919616 | 171155456 | 154345472 | 171155456 | 67235840 | 3228467 |
| convert-64-w4-preload-true | 67108864 | 70464307 | 106078208 | 173383680 | 173383680 | 173383680 | 67305472 | 3158835 |
| convert-256-w4-preload-false | 268435456 | 281857228 | 407846912 | 676532224 | 676397056 | 676532224 | 268685312 | 13171916 |
| convert-1024-w4-preload-false | 1073741824 | 1127428915 | 1625108480 | 2699051008 | 2699010048 | 2699051008 | 1073942528 | 53486387 |
| cancel-64-w4-preload-false | 67108864 | 70464307 | 103571456 | 154075136 | 153989120 | 154075136 | 50503680 | 19960627 |
| cancel-64-w4-preload-true | 67108864 | 70464307 | 103919616 | 154411008 | 154324992 | 154411008 | 50491392 | 19972915 |
| commit-fail-64-w4-preload-false | 67108864 | 70464307 | 103948288 | 103948288 | 103948288 | 103948288 | 0 | 70464307 |
| commit-fail-64-w4-preload-true | 67108864 | 70464307 | 105279488 | 105283584 | 105283584 | 105283584 | 4096 | 70460211 |
| read-fail-64-w4-preload-false | 67108864 | 70464307 | 107196416 | 115576832 | 107196416 | 115576832 | 8380416 | 62083891 |
| read-fail-64-w4-preload-true | 67108864 | 70464307 | 103641088 | 112128000 | 103645184 | 112128000 | 8486912 | 61977395 |
| cancel-loop-64-w4-preload-false | 67108864 | 70464307 | 104882176 | 155398144 | 155348992 | 155398144 | 50515968 | 19948339 |
| cancel-loop-64-w4-preload-true | 67108864 | 70464307 | 103931904 | 154451968 | 154361856 | 154451968 | 50520064 | 19944243 |
| save-64-w4-preload-false | 67108864 | 70464307 | 171413504 | 172654592 | 240873472 | 240873472 | 69459968 | 1004339 |
| save-256-w4-preload-false | 268435456 | 281857228 | 678543360 | 680656896 | 942317568 | 942317568 | 263774208 | 18083020 |
| save-1024-w4-preload-false | 1073741824 | 1127428915 | 2698584064 | 2701807616 | 3787657216 | 3787657216 | 1089073152 | 38355763 |

最小余量出现在 save-64-w4-preload-false，为 1004339 字节。此次只验证本地 Windows 自动门禁；未推送、未运行远程 CI/release，macOS 匿名 mmap、真实私有字节采样和 WKWebView 内存模式交互仍未做真机验收。

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
