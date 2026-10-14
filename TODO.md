# TODO（2026-10-14 Tauri/HTML 重写版）

依据：Qt 版（main_window.py + README）+ wx 移植教训。标准：逐项对齐 Qt 行为，
交付前 --smoke 对抗自检全过 + 截图目检 + 本地 exe 手测。

## A. 基础设施
- [x] A1. Rust 工具链 + cargo rsproxy 镜像 + @tauri-apps/cli（npm 阿里源）
- [x] A2. Tauri 工程骨架（src-tauri/ + frontend/），空窗跑通
- [x] A3. Rust 命令层：打开/保存对话框、读文件（UTF-8/BOM/GBK 识别）、写文件（UTF-8 BOM）、
      版本号（tag→release vX / beta+短哈希）、窗口标题、--smoke 模式通道
- [x] A4. JS 逻辑层测试（node:test）：csv.js、model.js 全覆盖

## B. 核心逻辑（js/model.js + js/csv.js，对齐 table_model.py / csv_io.py）
- [x] B1. CSV RFC4180 解析/生成（引号、内嵌换行、Tab/换行显示转换）；读 GBK 回退；列数不齐补空/截断+警告
- [x] B2. 模型：行列增删清、重命名、setData、物理排序（多层级，数字方式非数字排最后）
- [x] B3. 快照撤销栈（100 步上限、batch 合并、同格击键合并、load 清空、undo/redo 信号）

## C. 网格 UI（自绘虚拟滚动，js/grid.js）
- [x] C1. 虚拟滚动（8000 行流畅，只渲染可视行）、行号列、列头
- [x] C2. 单元格渲染：换行/Tab→空格、省略号截断、选中高亮、光标格、整行/整列选择高亮
- [x] C3. 选区模型：单击移动、Shift 块选、Ctrl 多块、鼠标拖选、行号/列头点击选整行/列（Ctrl/Shift 多选）
- [x] C4. 列宽拖动、列头拖动排序列序（对齐 wx EVT_GRID_COL_MOVE 能力/Qt 版列序调整）
- [x] C5. 格内编辑：打字即编辑（替换）、双击/F2 原位编辑、Enter/Shift+Enter/Tab/Shift+Tab 提交移动、
      Alt+Enter 换行、Esc 取消、编辑器自动加高（不改行高）

## D. 筛选（表头第二行，对齐 FilterHeaderView）
- [x] D1. 两带式表头：列名带 + 筛选带，输入框与列严格同宽对齐（CSS Grid 天然对齐）
- [x] D2. 子串匹配、忽略大小写、多列并且、按完整内容匹配；输入即时过滤
- [x] D3. 清除筛选按钮；列插入/删除后清空筛选
- [x] D4. 状态栏"显示 X / 共 Y 行"

## E. 编辑窗格 + 工具栏 + 状态栏
- [x] E1. 右侧编辑窗格（标题"编辑窗格"）：单击同步显示完整内容、可编辑实时写回、双向同步
- [x] E2. 显示符号开关：空格→·、Tab→→、换行→¶（作用于窗格，网格始终原文）
- [x] E3. 编辑窗格独立 Ctrl+滚轮缩放（与表格互不影响）
- [x] E4. 工具栏：新建/打开/保存/另存为/撤销/重做/排序…/清除筛选/显示符号（纯文字样式对齐 Qt）
- [x] E5. 状态栏：左"显示 X / 共 Y 行"（+缩放比例短暂提示），右版本号
- [x] E6. 标题栏：`*名 - simpleTable` 脏标记

## F. 文件与对话框
- [x] F1. 新建对话框（列名列表增删 + 初始行数，生成空表）
- [x] F2. 打开/保存/另存为（对话框 + Ctrl+S；保存失败错误框）
- [x] F3. 拖放打开 CSV（多文件取第一个）
- [x] F4. 关闭前未保存确认（是/否/取消）
- [x] F5. 排序对话框（多层级：每层 列+方式+升降序；层级增删/上下移）
- [x] F6. 行/列/单元格右键菜单（插入=选中数/末尾添加N行/删除/清除内容/重命名列；二次确认）

## G. 剪贴板与快捷键
- [x] G1. Ctrl+C 复制选区 TSV（含引号规则）；Ctrl+X 剪切
- [x] G2. Ctrl+V 三级判据：本软件标记/含Tab→区域展开（自动扩行列、整块一步撤销）；
      纯文本→整段进当前格；右键"按行拆分粘贴"强制区域
- [x] G3. Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z 全局（窗格聚焦时是窗格自身文本撤销）
- [x] G4. Del 立即清除选区内容
- [x] G5. 表格区 Ctrl+滚轮整体缩放（0.5–3.0，行高列宽表头筛选框同步）；Shift+滚轮横向滚动

## H. 交付
- [x] H1. --smoke 对抗自检（52 项全绿：Rust 命令层/渲染/选区/编辑键位/筛选/窗格/剪贴板/
      撤销/排序/结构/右键菜单/溢出/缩放/对话框）
- [x] H2. cargo tauri build → 单文件 exe；检查体积/启动速度；启动截图目检对比 Qt 版截图
- [x] H3. README/TODO 更新；分支提交（不合并，等用户手测）

## 开发日志（2026-10-14，第四轮：显示符号吞换行）

- 用户手测反馈：开启"显示符号"后，¶ 把换行替换掉了——4 个换行的文本
  塌成一行只剩 4 个 ¶。
- 根因：`toSymbols` 用 `¶` 替换 `\n`；而镜像层是 pre，断行靠真实换行符。
- 修复：`\n` → `¶\n`（符号在行尾，换行保留，对齐 Qt/Notepad++ 视觉）。
  空格→·、Tab→→ 本就是同宽替换不受影响；编辑层始终保存原文不被污染。
- 冒烟新增 1 项（先红后红确认：实际输出 "a¶b¶¶c" 复现塌行），现 52 项全绿；
  release exe 重编（23:03）。

## 开发日志（2026-10-14，第三轮：Esc 取消选定 + 筛选框防劫持）

- 用户手测反馈：选中单元格后 Esc 不能取消选定；且筛选框打字被"打字即编辑"
  劫持（keydown 冒泡到网格处理器，preventDefault 抢走字符并聚焦格内编辑器）。
- grid.js `_onKeyDown` 增加输入控件守卫：事件源是 INPUT/TEXTAREA/SELECT 时
  直接放行（app.js 全局撤销本就有同样守卫，两处对齐）。
- 新增 `deselect()`：Esc 清空选区填充但保留当前格（对齐 Qt 表格视图
  Escape 行为）；编辑中的 Esc 仍走编辑器取消路径（stopPropagation 不变）。
- 冒烟新增 2 项（先红后绿）：Esc 清选区留当前格；筛选框聚焦打字不劫持。
- 验证：冒烟 51/51 全绿；release exe 重编（22:34）。

## 开发日志（2026-10-14，第二轮：截图目检 + 启动参数）

- 吸顶表头布局 bug（截图目检发现，冒烟测不出）：`.row` 绝对定位原解析到
  `#grid-canvas`，行画进吸顶列头底下（第 1、2 行被遮）。修复分两层：
  - index.html：`#rows { position: relative }`，行相对列头之下的容器定位。
  - grid.js：两套坐标系统一——行区坐标（相对 #rows）与画布坐标（相对
    #grid-canvas，含滚动）之间加 `_originY()`（表头高）换算，覆盖
    `_layout` 画布高、`_renderRows` 可视窗口、`_hitTest`、行头拖拽选区、
    `ensureVisible`、`openEditor`（编辑器在画布层）。`.row.style.top` 不加偏移
    （其包含块已是 #rows，避免双计）。
- 幽灵横向滚动条：列宽拖拽手柄 `right:-3px` 悬垂超出画布右缘 2px，撑起
  scrollWidth。修复 `#grid-canvas { overflow-x: clip }`（不建滚动容器，
  吸顶表头不受影响；宽表时画布随内容加宽，不会误裁）。冒烟加第 49 项
  几何自检（scrollWidth/scrollHeight 与 clientWidth/Height 齐平 ±1px）。
- 命令行/文件关联：`simpletable.exe 路径.csv` 启动即打开（Rust 解析启动
  参数，`get_startup_path` 命令透传前端；与拖放打开共用 openPath）。
- WebView2 会话记忆滚动位置：装载/恢复后 `grid.resetScroll()` 回顶（对齐 Qt）。
- index.html 诊断通道（DIAG 写窗口标题）门控到 --smoke 模式，正常运行不再
  踩原生标题；无 Tauri 的纯浏览器调试保留 DOM 标题诊断。
- build.rs：`rerun-if-changed` 改逐文件递归声明（目录不跟踪内容，
  前端改动曾导致 cargo 不重新嵌入资产）。
- 验证：冒烟 49/49 全绿；tests_js 22/22；release 截图目检通过
  （3 行数据齐整渲染在表头之下，无幽灵滚动条，标题正常）。

## 开发日志（2026-10-14，第一轮）

- 冒烟自检修复记录（真实 bug 3 个，其余为测试夹具/断言问题）：
  - grid.js 编辑器 Enter/Tab/Esc 未 stopPropagation → 提交移动被网格导航重复执行
    （Enter 下移两格、Shift+Tab 越界漂移）。真 bug，影响实机手感。
  - app.js onData 无条件标脏 → load/undo 恢复路径脏标记错乱；
    改为 onReset 置干净 + resetPending 跳过一次 onData 标脏（undo/redo wrapper 显式标脏）。
  - menu.js showMenu 遇裸 null 分隔线项抛 TypeError（item.label 空指针）。
  - smoke.js：GBK 夹具字节错误（C4C4=哪，文=CEC4）；Ctrl+Shift+Z 断言方向写反
    （Qt 双绑定为重做）；loadBasic 补 clearFilters（对齐 app 打开流程）；
    行高拖拽前 ensureVisible(0,0)；删除残留的 void undoSteps。
- 48/48 全绿（debug 构建）。

## 教训清单（wx 版踩过的坑，本版预防）
- 筛选行错位 → CSS Grid 天然对齐，无跨控件坐标换算
- 缩放溢出/过小 → 单一 CSS 变量全组件 em 跟随
- Esc 假取消真保存 → 自绘编辑器，提交/取消路径完全自己控制
- 编辑器只注册首行 → 编辑器是独立浮层，与行无关
- 省略号/截断 → text-overflow: ellipsis
