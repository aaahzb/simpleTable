# simpleTable

[![CI](https://github.com/aaahzb/simpleTable/actions/workflows/ci.yml/badge.svg)](https://github.com/aaahzb/simpleTable/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

**像 Excel 一样查看、编辑、筛选、排序 CSV 的极简桌面工具——格子永远不变形，GB 级大文件秒开。**

把一段带换行的长文本（每日行程、长备注……）放进格子，行高列宽不会被撑变形：格内压成一行显示，点选格子，右侧编辑窗格展示全文、直接编辑。数据就是 CSV 文件本身——纯文本、可 diff、可版本管理、与 Excel/WPS 双向兼容。

![主界面：格内是压平的一行，右侧编辑窗格是多行全文](docs/img/main.jpg)

## 亮点

- **格子不变形**——多行文本在格内显示为空格+省略，编辑窗格看全文（上图）
- **大文件模式**——>50 MiB 自动切换：任意大小秒开，编辑/筛选/撤销照常，保存带进度条
- **多文件页签**——拖入多个 CSV 底部出标签条，逐签关闭、溢出可滚动，切换保留各自数据/脏态/撤销
- **分屏视图**——右键页签「在分屏中打开」，一屏并列两个文档对照（点哪侧哪侧得焦点；大文件受引擎单例限制不可双侧同开）
- **Excel 手感**——打字即编辑、F2、Enter/Tab 移动、Ctrl+C/V、Ctrl+Z/Y、Ctrl+G 跳行、Del 清除
- **键入即筛**——筛选框内嵌表头，多列"并且"叠加"或"多重筛选；多级排序对话框
- **单文件 exe**——冷启动约 80ms，体积 ~9MB，不监听端口，离线可用

## 快速开始

- **Windows 10+**：从 [Releases](../../releases) 下载 `simpletable.exe`，双击即用（系统自带 WebView2；LTSC 需先装 [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)）
- **macOS**：仅 Apple Silicon（arm64），Intel Mac 不提供
- 打开方式：拖 CSV 进窗口 / `simpletable.exe 文件.csv` / 文件对话框

## 功能一览

- **格内编辑**：打字替换、双击或 F2 改原文；`Alt+Enter` 格内换行；`Esc` 取消；编辑器自动加高、行高不变
- **编辑窗格**：点选任意格，右侧窗格同步全文可直接编辑；`Ctrl+滚轮` 独立缩放；"显示符号"像 Notepad++ 一样显示空格/Tab/换行
- **行列操作**：行号/列头选中、右键插入/删除/清除/重命名列；删除有二次确认，全部可 Ctrl+Z 撤销（100 步）
- **复制粘贴**：`Ctrl+C` 复制为 TSV 直粘 Excel；`Ctrl+V` 智能展开区域/纯文本整段入格；复制单格后选多格粘贴 = 全选区填充该值（Excel 一对多）；右键"按行拆分粘贴"
- **多文件页签**：拖入/打开多个 CSV 自动入签；签上 × 或鼠标中键关闭（脏签先问 保存/放弃/取消）；标签溢出出现 ◀▶ 滚动
- **分屏视图**：页签右键「在分屏中打开」并列对照两档（点活动签也行——原签移副侧，主侧自动换显邻签）；点击/右键任一側即把焦点（工具栏、编辑、快捷键）交给该侧，两侧内容稳定不互换；再右键分屏签「关闭分屏」收起（签保留）；大文件受引擎单例限制不可双侧同开（点击后状态栏说明原因）
- **筛选**：每列一个关键字框，子串匹配不区分大小写，多列"并且"；"多重筛选"勾选列按"或"匹配，可再叠加；一键清除
- **排序**：多层级对话框（列 × 字符/数字 × 升降序），物理排序，保存即所得
- **跳转行**：工具栏 / `Ctrl+G` / 行头右键，输入行号直达并选中（筛选视图按可见序号）
- **缩放**：表格区与窗格各自 `Ctrl+滚轮`（0.5x–3.0x）
- **版本号**：状态栏常驻，打 `v*` tag 发版自动显示 release 版本

## 大文件模式

打开时按大小自动路由：**≤ 50 MiB** 走内存模型（上面的全部功能）；**> 50 MiB** 进入大文件模式——

- **秒开**：Rust 引擎（mmap + 行索引），常驻内存只随行数增长（3000 万行 ≈ 240 MB），与文件大小无关
- **翻页**：200 行/页窗口缓存 ±2 屏预取；超高行数自动虚拟缩放，滚动条天然是"百分比定位"
- **筛选**：条件记录后点「执行筛选」（或筛选框回车）整表并行扫描，状态栏报命中数与用时
- **编辑/撤销**：单元格编辑、行插删即时生效（覆盖层 + 逆操作日志，微秒级、可撤销）
- **保存**：流式重写 + 原子换名 + 分阶段进度条；**字节透传**——未编辑的行整段照抄原文件字节（1.16 GB / 1550 万行删行保存实测 1.2 s）；BOM/GBK 随原文件忠实回写
- **列操作**：插入/删除/重命名列先预览，点「执行列操作」物理重写（带进度，完成后不可撤销）
- **边界**：大文件模式不支持排序（「排序…」自动隐藏）；粘贴不超出已有列宽；窗口重聚焦时检测外部改动并提示重载

## CSV 约定

- 第一行为列名；遵循 RFC 4180（引号/逗号/换行自动处理），Excel/WPS 双向兼容
- 读取自动识别 UTF-8 / UTF-8 BOM / GBK；写出统一 UTF-8 with BOM
- 列数与表头不一致的行：短行补空、长行截断并提示

## 开发

需要 Rust 工具链（[rustup](https://rustup.rs/)）。前端资产编译时嵌入 exe，改完前端需重新 build。

```bash
# 前端逻辑单测（node:test）
node --test tests_js/*.test.mjs

# 编译并运行
cd src-tauri
cargo build
./target/debug/simpletable.exe

# 测试：Rust 引擎单测（29 项）+ 真机冒烟自检（真实 WebView2，102 项断言）
cargo test
./target/debug/simpletable.exe --smoke   # 报告写 %TEMP%\simpletable_smoke_report.txt
```

推 `main` / PR 触发 CI（Windows：node + cargo test + 冒烟；macOS arm64：编译 + 引擎单测）；推 `v*` tag 自动发 GitHub Release（Windows exe + macOS dmg）。

## 项目结构

```
frontend/            纯 HTML/CSS/JS，无构建步骤：自绘虚拟滚动网格 + 编辑窗格 + 对话框
  js/bigmodel.js     大文件模式模型（窗口缓存/乐观编辑/列操作 pending）
  js/smoke.js        --smoke 对抗自检套件（102 项）
src-tauri/           Tauri 2 外壳 + Rust 大数据引擎（mmap/行索引/覆盖层/流式保存）
tests_js/            node:test 逻辑层单测
scripts/             截图/采样巡检脚本（开发用）
docs/img/            README 截图
```

## 许可证

[MIT](LICENSE)
