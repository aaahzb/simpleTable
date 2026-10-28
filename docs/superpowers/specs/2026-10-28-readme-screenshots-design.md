# simpleTable README 与实机截图重设计

## 目标

把仓库首页从完整说明书改为清晰的产品入口，并用同一当前版本制作三张真实、统一、可复现的应用截图。首次访问者应能快速理解产品价值、看到当前界面并找到下载；需要细节时仍能进入完整文档。

## 已批准方案

采用“首页加分层文档”。README 目标为约 90–120 行，完整用户内容迁移到 `docs/guide.md`，开发内容迁移到 `docs/development.md`。新增 `main.png`、`split-view.png`、`text-lines.png` 三张实机图；新图验收后删除旧 `docs/img/main.jpg`。

本设计已获用户明确批准，包括旧图删除。不得扩大到产品视觉重设计、营销横幅、GIF 或正常交互逻辑改造。

## 首页信息架构

README 按以下顺序组织：

1. 项目名、一句话定位、现有 CI/下载/许可证/平台徽章。
2. `docs/img/main.png` 主图及准确替代文本。
3. Windows 与 macOS 下载入口，紧邻简述 WebView2 和 macOS 未公证要求。
4. 四个核心亮点：多行格不变形、大文件渐进打开、筛选编辑与撤销、多文档与 TXT/LOG。
5. “上手 30 秒”最小流程。
6. 分屏和纯文本两张补充图，各配一句说明。
7. 用户指南、开发文档、Issue 和许可证链接。

首页不保留精确测试数量、长性能表、引擎实现长文、完整快捷键表、逐条 FAQ 或完整项目结构。现有内容优先迁移而非丢失；竞品对比不放在首页。

## 分层文档

### `docs/guide.md`

接收查看与编辑、TXT/LOG 语义、筛选排序、多文档、大文件机制、性能数据、引擎边界、快捷键、CSV/编码及 FAQ。保留必要的 macOS 与文件关联说明。文档面向使用者，不按 README 首页结构重复营销内容。

### `docs/development.md`

接收环境要求、构建、Node/Rust 测试、真实 WebView 冒烟、CI/发布链、项目结构与人工验收边界。测试命令保持可直接复制，数量只作为可变化信息，不在首页重复维护。

## 截图内容与视觉约束

三张图均来自本次重新构建的同一个 release exe，使用虚构中文数据，不读取真实业务文件。统一为 1440×900 PNG，保留完整应用界面并裁掉桌面，不放大拉伸，不添加虚构箭头、功能标签或调试覆盖层。画面不得包含鼠标指针、隐私数据、旧版页签或空白 WebView 帧。

### `main.png`

打开 `release-plan.csv`，选中具有代表性的多行发布备注格。网格内保持单行，右侧编辑窗格显示完整多行内容；底部多个页签和状态栏可见。首图必须直接证明“多行格不变形”和当前版本界面。

### `split-view.png`

左右分屏展示两份相关联的 CSV，当前 A/B 页签分组和双网格同时可见。数据应在缩小到 GitHub 图片宽度后仍能辨认，不用大面积空列或无意义随机字符。

### `text-lines.png`

打开虚构 `server.log`，展示 TextLines 的固定“列1”、一物理行一数据行、普通标点及中间空行。画面应明确不是逗号分隔的 CSV 解析。

## 可复现截图管线

新增专用 `--park` 截图场景和安全捕获脚本，场景只生成临时目录中的虚构夹具并布置界面。不得运行现有任何会引用 `price_j8888.csv` 的模式。

捕获脚本必须：

- 只启动并结束自己创建的诊断进程，不使用 `taskkill` 或按进程名批量停止。
- 按 PID 和最大可见窗口定位目标，不选取用户已有的 simpleTable 窗口。
- 使用独立临时握手路径，避免与既有 `park_capture.ps1` 的全局 TEMP 标记冲突。
- 对 WebView2 使用真实屏幕捕获或已验证的完整渲染捕获，并检查空白帧和尺寸。
- 仅在新图全部生成并人工查看通过后替换 README 引用和删除旧图。

正常应用行为不变；诊断代码仍需遵守前端嵌入构建规则。

## 验证与验收

- 逐张读取三张 PNG，检查版本、构图、文字、数据、页签、编辑窗格和状态栏。
- 检查 README 相对图片和文档链接，确认无旧 `main.jpg` 引用。
- 对照迁移清单确认原手册和开发内容未无意丢失。
- README 首屏可独立回答“是什么、长什么样、如何下载”。
- 运行文档链接检查、完整 Node/Rust 回归；因诊断前端会变化，重编 debug/release 并运行双真实 WebView 冒烟。
- 只本地提交，不推送；合并和发布仍由用户另行决定。

## 参考

- [GitHub Docs：About READMEs](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/about-readmes)
- [Make a README](https://www.makeareadme.com/)
- [Sniffnet README](https://github.com/GyulyVGC/sniffnet)
- [bat README](https://github.com/sharkdp/bat)
