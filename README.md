# simpleTable

个人自用的极简桌面表格工具：像 Excel 一样查看、编辑、筛选、排序 CSV 表格，没有多余功能。数据就是 CSV 文件本身——纯文本、可 diff、可版本管理、与 Excel/WPS 双向兼容。

适合任何"一张表 + 若干文本列"的整理工作。比如一张旅行计划表：目的地、月份、预算、评分、攻略链接、每日行程（多行长文本）……列自定义，内容随意。

## 下载（免安装）

在 Releases 页面下载最新版：

- **Windows (x64)**：`simpleTable-windows-x64.exe`，单文件，双击即用。
- **macOS (Apple Silicon)**：`simpleTable-macos-arm64.zip`，解压得到 `simpleTable.app`。首次打开如提示"无法验证开发者"：右键 → 打开，或在终端执行 `xattr -cr /Applications/simpleTable.app`。

## 功能

- **格内编辑（Excel 键位）**：双击或 `F2` 进入编辑；`Enter` 提交并下移、`Shift+Enter` 上移、`Tab` 右移、`Shift+Tab` 左移、`Alt+Enter` 格内换行、`Esc` 取消；多行内容时编辑器自动加高（不改行高）。
- **编辑窗格**：单击选中任意单元格，右侧编辑窗格同步显示完整内容，可直接编辑（支持回车、空格、Tab），实时写回；格内编辑也会同步到窗格。工具栏"显示符号"开关可像 Notepad++ 一样显示空格（·）、Tab（→）、换行符（¶）。
- **单元格显示**：内容里的换行/Tab 在格内显示为空格，超出列宽自动省略——内容再多单元格也不变形。
- **缩放**：表格区与编辑窗格各自支持 `Ctrl+滚轮` 整体缩放（0.5x–3.0x，互不影响），表格的行高、列宽、表头、筛选行随字体同步缩放；缩放时状态栏短暂提示当前比例。
- **拖放打开**：把 CSV 文件直接拖进窗口即可打开（拖入多个时取第一个）。
- **行列操作（Excel 式）**：左键点行号/列名选中整行/整列（支持 Ctrl/Shift 多选）；在行号、列名或单元格选区上**右键**弹出菜单——插入（数量=选中数量）、"在末尾添加 N 行"、删除、清除内容、重命名列。删除与清除均有二次确认；清除只清内容、保留行列结构；新列自动命名"新列N"；列插入/删除后自动清空筛选（防止列号错位误筛）。
- **单元格操作（Excel 式）**：`Ctrl+C` 复制选区为 TSV（含换行/Tab 的单元格自动加引号，可直接粘进 Excel）；`Ctrl+V` 从当前单元格粘贴 TSV，超出范围自动扩展行列（新列自动命名）；`Del` 立即清除选中单元格内容（不弹确认）；选择状态下 `Enter` 下移、`Tab` 右移。快捷键仅在表格区生效，格内编辑器与编辑窗格不受影响。
- **筛选**：表头下方每列一个关键字框，子串匹配、不区分大小写、多列"并且"关系；一律按完整内容匹配。
- **排序**：工具栏"排序…"打开多层级对话框，每层选列、方式（字符/数字）、升降序，层级顺序即优先级；数字方式下非数字单元格排在该级最后。排序为物理排序（保存的 CSV 即排序后顺序）。
- **文件**：新建（自定义初始列，生成 20 空行）/ 打开（文件对话框或拖入）/ 保存（支持快捷键 `Ctrl+S`）/ 另存为 CSV。
- **版本号**：主窗口状态栏右侧常驻显示。打包时自动注入：HEAD 打了 `v` 开头的 tag（如 `v1.0.0`）→ 显示 `release v1.0.0`，否则显示 `beta+<commit哈希>`。

## CSV 约定

- 第一行为列名。
- 遵循 RFC 4180：含换行、逗号、双引号的字段自动加双引号，Excel/WPS 双向兼容。
- 写出统一 UTF-8 with BOM（Excel 打开中文不乱码）；读取自动识别 UTF-8 / UTF-8 BOM / GBK。
- 列数与表头不一致的行：短行补空、长行截断并在打开时提示。

## 运行（开发环境）

```bash
# Windows
venv\Scripts\python.exe main.py
# macOS
python3 main.py
```

依赖安装：`pip install -r requirements.txt`（国内可走阿里源 `-i https://mirrors.aliyun.com/pypi/simple/`）

## 测试

```bash
python -m pytest tests/ -v
```

无显示环境下离屏运行：`QT_QPA_PLATFORM=offscreen python -m pytest tests/ -q`

## 打包与发布

- **CI/CD（推荐）**：推送 `v` 开头的 tag，GitHub Actions 自动跑测试、编译 Windows (x64) 与 macOS (Apple Silicon) 两个版本并发布到 Releases（附下载说明与自动生成的变更列表）：

  ```bash
  git tag v1.0.0
  git push origin v1.0.0
  ```

  也可以在 Actions 页面手动触发（只构建不发布，用于验证流水线）。

- **本地打包 Windows**：双击运行 `build_win.bat`，产物在 `dist\simpleTable.exe`。
- **本地打包 macOS (arm64)**：需在 Mac 上执行 `bash build_mac.sh`（自动建 venv、装依赖、打包；PyInstaller 不支持交叉编译），产物为 `dist/simpleTable.app`。

## 项目结构

```
main.py          入口
main_window.py   主窗口、对话框、筛选行、格内编辑委托、缩放/拖放
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
