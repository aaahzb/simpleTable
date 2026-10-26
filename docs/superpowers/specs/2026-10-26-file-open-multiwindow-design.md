# 文件关联、单实例与 Chrome 式多窗口标签设计

- 日期：2026-10-26
- 分支：`feat/file-open-multiwindow`
- 状态：设计已获用户确认，待书面规格审阅
- 范围：Windows 单实例打开 CSV、macOS 文件关联打开 CSV、标签移到新窗口与窗口间拖放

## 1. 背景与已确认根因

simpleTable 当前是 Tauri 2 单进程、单窗口、多标签应用。每个标签拥有独立 BigFile 会话，文档修改、撤销/重做、排序、筛选、列操作和临时文件均随会话存活。

三个需求暴露出同一入口与所有权问题：

1. Windows 没有注册 `tauri-plugin-single-instance`。第二次双击 CSV 会启动第二个进程，无法在已有窗口新增标签。
2. macOS 没有配置 `bundle.fileAssociations`，也没有处理 `RunEvent::Opened { urls }`，Finder 传入的文件 URL 没有进入 `openPathsAsTabs`。
3. 直接创建第二个 WebviewWindow 会造成严重会话冲突：每个 JS realm 都从 1 分配 session id，而 Rust `BigShared` 是进程级 `HashMap<u64, BigEntry>`。同号 `big_open`/`big_new` 会替换其他窗口的会话，`big_close` 也可能误删已迁移会话。
4. 当前 `close-request` 和 `quit_app` 假设只有一个窗口；进度事件使用广播且部分事件没有 session 过滤；capability 仅覆盖 `main`。
5. 新窗口若重复执行启动逻辑，会重复打开启动 CSV，并在 smoke/park 模式下重复运行诊断或退出进程。

因此第三项不能只做菜单或窗口创建，必须同时建立进程级文档所有权和跨窗口迁移协议。

## 2. 产品目标

### 2.1 Windows 单实例打开

- 首次双击 CSV 启动应用并打开文件。
- 应用已运行时再次双击 CSV，第二个进程只负责把参数和工作目录交给已有进程，随后退出。
- 文件在最近使用的窗口新增标签；同一路径已打开时聚焦原窗口和原标签。
- 不带 CSV 的第二次启动只聚焦最近使用窗口，不打开空标签。
- 多窗口与单实例不冲突：单实例限制进程数，不限制同一进程内的窗口数。

### 2.2 macOS 文件关联打开

- `.app` 声明 CSV 文件关联，角色为 Editor，内容类型使用系统 CSV UTI。
- Finder 双击 CSV 时，无论应用尚未启动还是已经运行，都通过 `RunEvent::Opened` 接收文件 URL。
- `file://` URL 转为本地路径后，与 Windows 启动参数共用同一打开队列和标签去重逻辑。
- 非文件 URL 不扩展成 deep-link 功能，本轮忽略。

### 2.3 Chrome 式多窗口标签

- 标签右键在现有分屏命令之外增加“在新窗口打开”。
- 移动的是当前文档会话，不是从磁盘再开一个独立副本。
- 未保存修改、脏点、undo/redo、排序、筛选、列操作状态、会话临时文件和每文档撤销预算随会话保留。
- 同窗口拖动仍负责排序和分屏换组；拖到另一窗口的标签组按插入指示位置落签；拖出形成新窗口。
- 无效投放或取消不迁移。移走源窗口最后一个标签时，只有目标接管成功后才关闭源窗口，不留下空白源窗口。
- 关闭一个窗口只处理该窗口的标签和未保存确认；最后一个窗口关闭才退出应用。

## 3. 非目标与验收边界

- 不支持多个独立应用进程，不恢复重启前的标签或撤销历史。
- 不允许多个窗口各自打开同一路径形成两个独立编辑会话。
- 不改变 CSV 编解码、保存、统一撤销预算、应用内大复制或分块粘贴协议。
- 不为“新建空窗口”增加独立菜单；新窗口由标签迁移产生。
- macOS Finder 双击的最终实机验收由用户在新 `.app` 构建上完成；Windows 环境不宣称完成 macOS 实测。
- 原有四个未跟踪杂项和真实业务数据不进入变更集合；不读取、不试验 `price_j8888.csv`。

## 4. 总体架构

新增三个明确边界：

1. **OpenRequestQueue**：统一接收启动参数、第二实例参数和 macOS Opened URL，负责路径解析、待打开排队、目标窗口路由和重复打开预留。
2. **WindowRegistry + DocumentOwner**：记录窗口 label、最近活动窗口、session 所属窗口和 lease generation。所有文档命令校验 owner + session + lease。
3. **TransferTicket**：用 seal → accept → complete/cancel 迁移同一 session。目标窗口只 adopt 既有 session，不重新 `big_open`/`big_new`。

窗口 label 使用 `main` 和唯一的 `doc-*`。主窗口与诊断窗口角色明确；迁移目标窗口在就绪前不接收启动文件或运行 smoke/park。

## 5. 文件打开队列

### 5.1 输入归一化

- 初次启动使用 `args_os` 和当前工作目录；第二实例使用插件回调的 `args` 与 `cwd`。
- 仅接收 `.csv` 扩展，忽略选项和其他文件；相对路径按对应启动工作目录解析。
- macOS 只接收能转换为本地路径的 `file://` URL。
- 路径在进入前端前保持 `PathBuf`，避免先把非 UTF-8 参数强制转为字符串。
- 无法读取的 CSV 继续走现有 `loadDoc` 错误提示，不把失败路径登记为已打开。

### 5.2 就绪前排队，就绪后定向交付

后端先托管 OpenRequestQueue，再注册单实例插件，避免插件回调早于状态初始化。

- 前端先注册 `open-paths` 监听，再调用 `open_requests_ready`。
- 就绪调用原子取走该窗口的待打开批次；之后的新路径用 `emit_to` 定向发送。
- 每窗口用单一 Promise 链串行消费批次，禁止两个异步 `openPathsAsTabs` 并发修改 tabs。
- 重复路径在入队时查“已打开 + 待打开预留”，实际打开前再查一次，覆盖快速连续双击和窗口切换竞态。
- 新 CSV 路由到最近活动且可接收的窗口；已打开路径路由到其 owner 窗口并发送定向 activate-session。

### 5.3 诊断模式隔离

`--smoke` 和 `--park` 不注册单实例插件，避免测试参数被已有正常窗口截获。子窗口依据后端分配的角色跳过启动路径、smoke 和 park；早期错误处理不得因子窗口异常直接结束主 smoke 进程。

## 6. 文档会话所有权

### 6.1 全局唯一 session 与 owner

- session id 改由 Rust 进程级分配器生成，禁止各 WebView 从 1 自行编号。
- open/create 在 Rust 分配 session 后返回 id 与 BigMeta；BigModel 在取得 id 前不发送依赖 session 的命令。
- BigEntry 增加 owner label 和 lease generation；owner 迁移时 lease 递增。
- open/create 只绑定调用窗口；所有已建立会话的 big_* 命令统一经过 owner/session/lease 校验。迟到的旧 owner 请求和旧 lease 事件一律无效，不能删除或修改新 owner 的会话。
- `big_close` 只允许当前 owner 关闭当前 lease；迁移走 detach/complete，绝不复用 close。

### 6.2 定向事件

- `big-progress`、`big-sort-progress`、索引和其他文档事件带 session + lease，并只发给 owner label。
- BigModel 监听同时校验 session 与 lease；adopt 后使用新 lease，源窗口迟到事件自然失效。
- `close-request` 只发给请求关闭的窗口，不再广播给所有窗口。
- capability 覆盖 `main` 与 `doc-*`，保留最小 core 权限；窗口由 Rust WebviewWindowBuilder 创建，不向前端开放任意窗口创建命令。

### 6.3 全局设置一致性

撤销预算变更继续作用于所有已打开和在途文档，包括其他窗口的会话；adopt 不重新套用默认预算。后端完成下发后定向通知各 owner 刷新脏点和 undo/redo UI，源窗口不得再对已迁移模型下发预算。

## 7. 跨窗口迁移协议

### 7.1 可迁移条件与冻结

每个标签具有 idle/transferring/closing 的单一状态机。迁移开始前：

1. 等待已发出的单元格编辑、取页和索引轮询回调落定。
2. 保存、排序载入、外部粘贴导入或固定点两段确认进行中时，不开始迁移，提示操作完成后重试。
3. transferring 状态禁止源标签继续编辑、保存、关闭或再次拖出；后台索引不取消，目标接管后恢复轮询。
4. Rust seal 校验 owner，推进 gate 并记录当前 lease，获取并释放文件锁作为操作屏障，并生成一次性 ticket。

### 7.2 accept 与 complete

1. 源端请求 seal，携带目标窗口/插入位置和视图快照。
2. 新窗口场景先创建隐藏目标窗口；已有窗口场景直接使用目标窗口。
3. 目标前端监听器就绪后调用 `accept(ticket)`，Rust 原子切换 owner + lease 并返回 BigMeta、路径、筛选状态和视图快照。
4. BigModel 以 adopt 模式连接同一 session，重建页缓存和事件过滤；不得调用 open/create 或重设撤销预算。
5. 目标恢复 dirty、undo/redo、排序和筛选状态，选区与滚动沿用现有换签语义；源端确认 accept 后调用 complete，随后才移除本地标签并注销监听，不调用 big_close。
6. 目标失败、超时或在 complete 前销毁时执行 cancel，源端恢复同一标签；已 sealed 会话不得遗留为无主状态。

源窗口因迁移变空时，在 complete 后关闭。若迁移失败，源窗口和标签原样保留。

## 8. 拖放与窗口行为

- 同窗口投放继续调用现有 `moveTab(side, index)`，不经过跨窗口会话迁移。
- 跨窗口 dragstart 建立带来源信息的 drag token；dragover 只计算插入位置，drop 才申请迁移。
- 目标标签组 A/B 都可接收，落点规则复用同窗口插入索引计算。
- 拖出形成新窗口时，目标窗口在有效投放位置显示；源窗口最后一个标签也按同一迁移协议处理。
- Escape、无效投放、目标拒绝或创建失败均不删除源标签。
- 原生 HTML5 跨窗口拖放需在 Windows 实机验证；macOS 实机同时覆盖此路径，不能用合成 DOM 事件替代最终验收。
- 同一文件已打开时只激活既有标签；激活分屏中的标签时同时聚焦正确侧。

## 9. 关闭、退出与异常恢复

- 每个窗口创建时都注册 CloseRequested 拦截和定向 close-request。
- 窗口关闭只逐项确认该窗口的脏标签；用户取消则窗口保持打开。
- 确认后由后端关闭该窗口并清理其拥有的会话；其他窗口及其临时文件不受影响。
- 最后一个窗口关闭后沿用进程级 cleanup_process_state。
- 窗口销毁、ticket 超时和目标创建失败都必须有明确回收或回滚路径。
- smoke 退出仍清理全部会话和应用内剪贴板；诊断模式不注册单实例，防止测试互相截获。

## 10. 测试策略

新行为先写失败测试并亲见预期 RED，再做最小实现；既有行为直接通过的测试如实记录，不制造 RED。

### 10.1 Rust

- args/cwd 解析、相对路径、多个 CSV、非 CSV 和 file URL 转换。
- 就绪前入队、就绪后交付、批次只消费一次、目标窗口路由和重复预留。
- 两个窗口创建 session 不碰撞；错误 owner/lease 不能 open 替换、读写或 close。
- seal/accept/complete/cancel 全状态机；迟到 close、目标失败、超时和窗口销毁不丢会话。
- adopt 保留 overlay、undo/redo、排序、筛选、spool 和预算；迁移期间后台索引可继续。

### 10.2 Node 与真机 smoke

- fake 使用共享 session 表模拟两个窗口，不再依赖单 JS realm 的假唯一性。
- adopt 不调用 big_open/big_new/big_set_undo_budget；旧 lease 事件不刷新目标或源窗口。
- 多批次打开严格串行；重复路径激活原签；关闭子窗口不触发 quit_app。
- smoke 覆盖右键新窗口、跨窗口投放、拖出、最后标签迁移、未保存修改与 undo/redo 保留、失败回滚和窗口级关闭确认。
- 自动测试之外，Windows 实机执行两次进程启动和真实跨窗口拖放；macOS 由用户执行冷启动/已运行两种 Finder 双击及拖放验收。

### 10.3 全量回归与交付

- `cd src-tauri && cargo test -p simpletable --lib`
- `node --test tests_js/*.test.mjs`
- `node scripts/verify-local.mjs`
- debug/release exe 必须重新链接且嵌入最新前端，双 smoke 均零失败。
- 更新 README 的单实例、macOS 文件关联、多窗口迁移、关闭规则和实测方法；测试数按最终实际结果填写。
- 所有相关 Rust 构建无新增警告；stable 缺少 rustfmt 时不擅自安装，也不宣称运行过。

## 11. 规格自审清单

- [x] 三项用户需求均有明确入口、数据流和验收方式。
- [x] 多窗口 session 碰撞、迟到 close、事件广播和子窗口启动副作用均有根因对应设计。
- [x] 未保存修改、历史、临时文件和预算在迁移中保持同一物理会话。
- [x] 失败、取消、目标崩溃、最后标签和最后窗口行为均有定义。
- [x] 无占位实现、无跨进程副本替代迁移、无静默缩减 macOS 验收声明。
