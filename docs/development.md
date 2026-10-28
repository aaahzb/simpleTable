# simpleTable 开发文档

[返回项目首页](../README.md)

## 开发

Rust + Tauri 2 外壳，前端是原生 HTML/CSS/JS，无构建步骤。需要 Rust 工具链（[rustup](https://rustup.rs/)）和 Node.js（仅跑前端单测）。

```bash
# 前端逻辑单测（216 项通过）
node --test tests_js/*.test.mjs

# Rust 引擎单测（289 项通过，另 3 项默认忽略基准）
cd src-tauri && cargo test

# 编译并运行（前端资产编译时嵌入 exe，改完前端需重新 build）
cargo build
./target/debug/simpletable.exe

# 真机冒烟自检：真实 WebView2，debug/release 各 174 项断言
./target/debug/simpletable.exe --smoke   # 报告写 %TEMP%\simpletable_smoke_report.txt

# 双构建、前端嵌入时间检查和 debug/release 双真机冒烟
cd .. && node scripts/verify-local.mjs
```

`--smoke` 和 `--park` 是隔离诊断模式，不注册单实例，也不会把测试参数转交给正常窗口；迁移产生的子窗口不会重复运行启动路径或诊断套件。

平台验收边界：Windows 默认打开方式、真实鼠标/RDP 拖放和最后一个窗口由用户关闭仍需在 release exe 上验收。macOS 需在新的 `.app` 上验证 Finder 冷启动/已运行双击、同路径激活、跨窗口真实拖放与拖出，并确认 TXT/LOG 默认打开其他程序不受影响；Windows 环境的配置检查和编译通过不等于 macOS 实机验收。

推 `main` / PR 触发 CI（Windows：node + cargo test + 冒烟；macOS arm64：编译 + 引擎单测）；推 `v*` tag 自动发 GitHub Release（Windows exe + macOS dmg）。

## 项目结构

```
frontend/            纯 HTML/CSS/JS：自绘虚拟滚动网格 + 编辑窗格 + 对话框
  js/bigmodel.js     统一数据模型（全表唯一：窗口缓存 / 渐进索引轮询 / 内存排序 / 乐观编辑）
  js/smoke.js        --smoke 对抗自检套件（174 项）
src-tauri/           Tauri 2 外壳 + Rust 数据引擎（mmap / 行索引 / 覆盖层 / 流式保存）
tests_js/            node:test 逻辑层单测
scripts/             截图 / 采样巡检脚本（开发用）
docs/img/            README 截图
```
