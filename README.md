# simpleTable

**痛点：要把一段带换行的文字（每日行程、长备注……）放进一个格子，又不想把行高列宽撑变形，还想随时看清全文。** simpleTable 的答案：格子永远不变形——换行在格内显示为空格、超长自动省略；点选单元格，右侧编辑窗格同步显示完整内容，可直接阅读和编辑；格内编辑时编辑器自动加高、行高不变。

在此之上，它是个人自用的极简桌面表格工具：像 Excel 一样查看、编辑、筛选、排序 CSV 表格，没有多余功能。数据就是 CSV 文件本身——纯文本、可 diff、可版本管理、与 Excel/WPS 双向兼容。

适合任何"一张表 + 若干文本列"的整理工作。比如一张旅行计划表：目的地、月份、预算、评分、攻略链接、每日行程（多行长文本）……列自定义，内容随意。

## 下载（免安装）

在 Releases 页面下载最新版（当前发布的是 Qt 版；Tauri 版在 `tauri-port` 分支验证中，验证通过后接替发布）：

- **Windows (x64)**：`simpleTable-windows-x64.exe`，单文件，双击即用。
- **macOS (Apple Silicon)**：`simpleTable-macos-arm64.dmg`，双击打开后把 `simpleTable.app` 拖进“应用程序”。首次打开如提示"无法验证开发者"：右键 → 打开，或在终端执行 `xattr -cr /Applications/simpleTable.app`。

## 技术架构（tauri-port 分支）

Tauri（Rust 外壳 + 系统 WebView 前端）：启动快、体积小、UI 用 HTML/CSS 绘制。

- `frontend/` 纯 HTML/CSS/JS，无构建步骤、无框架：自绘虚拟滚动网格、两带式表头、模态对话框、右键菜单。
- `src-tauri/` Rust 侧只做 OS 能力：文件对话框、CSV 读写（UTF-8 BOM / GBK 识别）、窗口标题、版本注入、`--smoke` 自检通道。
- 行为逐项对齐 Qt 版（main_window.py）；`frontend/js/smoke.js` 为对抗自检套件（49 项），`--smoke` 运行后写报告退出。
- 运行要求：Windows 10+（自带 WebView2 运行时；LTSC/老系统需装 [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)）。

## 功能

- **格内编辑（Excel 键位）**：打字即进入编辑（新内容替换旧内容），双击或 `F2`（macOS 仅双击，Qt 在该平台无 F2 编辑键）在原文基础上编辑；`Enter` 提交并下移、`Shift+Enter` 上移、`Tab` 右移、`Shift+Tab` 左移、`Alt+Enter` 格内换行、`Esc` 取消；多行内容时编辑器自动加高（不改行高）。
- **编辑窗格**：单击选中任意单元格，右侧编辑窗格同步显示完整内容，可直接编辑（支持回车、空格、Tab），实时写回；格内编辑也会同步到窗格。工具栏"显示符号"开关可像 Notepad++ 一样显示空格（·）、Tab（→）、换行符（¶）。
- **单元格显示**：内容里的换行/Tab 在格内显示为空格，超出列宽自动省略——内容再多单元格也不变形。
- **缩放**：表格区与编辑窗格各自支持 `Ctrl+滚轮` 整体缩放（0.5x–3.0x，互不影响），表格的行高、列宽、表头与内嵌筛选框随字体同步缩放；缩放时状态栏短暂提示当前比例。
- **拖放打开 / 命令行打开**：把 CSV 文件直接拖进窗口即可打开（拖入多个时取第一个）；也可用 `simpletable.exe 文件.csv` 启动即打开（可做文件关联），两者共用同一编码识别与脏检查流程。
- **行列操作（Excel 式）**：左键点行号/列名选中整行/整列（支持 Ctrl/Shift 多选）；在行号、列名或单元格选区上**右键**弹出菜单——插入（数量=选中数量）、"在末尾添加 N 行"、删除、清除内容、重命名列。删除与清除均有二次确认（误操作可用 Ctrl+Z 撤销）；清除只清内容、保留行列结构；新列自动命名"新列N"；列插入/删除后自动清空筛选（防止列号错位误筛）。

- **撤销/重做**：`Ctrl+Z` 撤销、`Ctrl+Y` / `Ctrl+Shift+Z` 重做，覆盖格内编辑、粘贴、行列增删清、重命名、排序等全部改动（最多 100 步，粘贴整块算一步）；编辑窗格聚焦时 `Ctrl+Z` 是窗格自己的文本撤销。
- **单元格操作（Excel 式）**：`Ctrl+C` 复制选区为 TSV（含换行/Tab 的单元格自动加引号，可直接粘进 Excel）；`Ctrl+V` 粘贴：本软件/Excel 复制的区域或含 Tab 的文本按区域展开，超出范围自动扩展行列（新列自动命名）；不含 Tab 的纯文本（如带换行的长文章）整段进入当前单元格（如需摊开成多行：右键 → 按行拆分粘贴）；`Del` 立即清除选中单元格内容（不弹确认）；选择状态下 `Enter` 下移、`Tab` 右移。快捷键仅在表格区生效，格内编辑器与编辑窗格不受影响。
- **筛选**：筛选框内嵌在表头里（列名正下方），与列一体联动、不会错位，每列一个关键字框，子串匹配、不区分大小写、多列"并且"关系；一律按完整内容匹配。
- **排序**：工具栏"排序…"打开多层级对话框，每层选列、方式（字符/数字）、升降序，层级顺序即优先级；数字方式下非数字单元格排在该级最后。排序为物理排序（保存的 CSV 即排序后顺序）。
- **文件**：新建（自定义初始列，生成 20 空行）/ 打开（文件对话框或拖入）/ 保存（支持快捷键 `Ctrl+S`）/ 另存为 CSV。
- **版本号**：主窗口状态栏右侧常驻显示。打包时自动注入：HEAD 打了 `v` 开头的 tag（如 `v1.0.0`）→ 显示 `release v1.0.0`，否则显示 `beta+<commit哈希>`。

## CSV 约定

- 第一行为列名。
- 遵循 RFC 4180：含换行、逗号、双引号的字段自动加双引号，Excel/WPS 双向兼容。
- 写出统一 UTF-8 with BOM（Excel 打开中文不乱码）；读取自动识别 UTF-8 / UTF-8 BOM / GBK。
- 列数与表头不一致的行：短行补空、长行截断并在打开时提示。

## 运行（开发环境）

Tauri 版（tauri-port 分支，推荐）：

```bash
# 前端逻辑单测（node:test，无需 Rust）
node --test tests_js/

# 编译并运行（需要 Rust 工具链；前端资产在编译时嵌入，改完前端要重新 build）
cd src-tauri
cargo build
./target/debug/simpletable.exe

# 对抗自检：49 项断言，写 %TEMP%\simpletable_smoke_report.txt 后退出
./target/debug/simpletable.exe --smoke
```

Qt 版（main 分支）：

```bash
# Windows
venv\Scripts\python.exe main.py
# macOS
python3 main.py
```

依赖安装：`pip install -r requirements.txt`（国内可走阿里源 `-i https://mirrors.aliyun.com/pypi/simple/`）

## 测试

```bash
# Tauri 版：JS 逻辑层单测 + 真机冒烟自检
node --test tests_js/
./src-tauri/target/debug/simpletable.exe --smoke

# Qt 版
python -m pytest tests/ -v
```

无显示环境下离屏运行（Qt 版）：`QT_QPA_PLATFORM=offscreen python -m pytest tests/ -q`

## 打包与发布

- **CI/CD（推荐）**：推送 `v` 开头的 tag，GitHub Actions 自动跑测试、编译 Windows (x64) 与 macOS (Apple Silicon) 两个版本并发布到 Releases（附下载说明与自动生成的变更列表）：

  ```bash
  git tag v1.0.0
  git push origin v1.0.0
  ```

  也可以在 Actions 页面手动触发（只构建不发布，用于验证流水线）。

- **本地打包（Tauri 版，Windows x64）**：

  ```bash
  cd src-tauri
  cargo build --release
  # 产物：src-tauri\target\release\simpletable.exe（单文件）
  ```

- **本地打包 Windows（Qt 版）**：双击运行 `build_win.bat`，产物在 `dist\simpleTable.exe`。
- **本地打包 macOS (Qt 版, arm64)**：需在 Mac 上执行 `bash build_mac.sh`（自动建 venv、装依赖、打包；PyInstaller 不支持交叉编译），产物为 `dist/simpleTable.app`。

## 项目结构

Tauri 版（tauri-port 分支）：

```
frontend/index.html    主页面：布局 + 全部样式（em 缩放体系）
frontend/js/app.js     总装：工具栏/状态栏/文件操作/菜单/全局键位
frontend/js/grid.js    自绘虚拟滚动网格（选区/编辑/拖拽/缩放/筛选）
frontend/js/model.js   数据模型 + 快照撤销栈（对齐 table_model.py）
frontend/js/csv.js     CSV 解析/生成 RFC4180（对齐 csv_io.py）
frontend/js/pane.js    编辑窗格（符号镜像层、独立缩放）
frontend/js/dialogs.js 模态对话框（新建/排序/列/确认/数字输入）
frontend/js/menu.js    右键上下文菜单
frontend/js/smoke.js   --smoke 对抗自检套件（49 项）
tests_js/              node:test 逻辑层单测（csv/model）
src-tauri/src/lib.rs   Rust 命令层（文件对话框/读写/标题/版本/冒烟通道）
src-tauri/build.rs     版本注入 + 前端资产变更触发重编译
```

Qt 版（main 分支）：

```
main.py          入口
main_window.py   主窗口、对话框、筛选表头、格内编辑委托、缩放/拖放
table_model.py   数据模型 + 物理排序
filter_proxy.py  筛选代理
csv_io.py        CSV 读写
version.py       版本号（打包注入优先，开发环境回退 git）
make_version.py  打包辅助：生成 _build_info.py（不入库）
tests/           pytest 单元/集成测试
.github/workflows/release.yml   GitHub Actions 发布流水线
```

## 许可证

[MIT](LICENSE)
