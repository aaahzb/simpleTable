const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** 解析固定 README 场景，拒绝旧 park 模式和路径字符。 */
export function parseReadmeMode(mode) {
  const match = /^readme-(main|split|text)-([A-Za-z0-9]{12,64})$/.exec(mode);
  if (!match) throw new Error(`未知 README 场景：${mode}`);
  return { scene: match[1], token: match[2] };
}

/** 返回本轮独立目录中的四个干净文件名；调用方先创建该目录。 */
export function createReadmeFixtures(dir, token) {
  const main = '迭代,模块,状态,发布备注\r\n'
    + '1.8,表格编辑,进行中,"多行备注保持单行显示\r\n右侧窗格复核完整内容"\r\n'
    + '1.8,大文件索引,已完成,"机械盘复测完成\r\n首屏与渐进索引数据已归档"\r\n'
    + '1.8,撤销重做,已完成,"区域粘贴按一次操作撤销\r\n内存预算设置已复核"\r\n'
    + '1.9,纯文本逐行,进行中,"TXT 与 LOG 不按 CSV 解析\r\n保留空行和字面标点"\r\n'
    + '1.9,分屏对照,待验收,"核对 A/B 页签分组\r\n检查两侧独立滚动"\r\n'
    + '1.9,发布检查,待验收,"重编 release\r\n运行完整真实窗口冒烟"\r\n'
    + '1.9,编码保存,已完成,"UTF-8 与 GBK 均已覆盖\r\n保留原始 BOM 与换行"\r\n'
    + '2.0,筛选排序,计划中,"组合多个列表头条件\r\n核对排序后的编辑映射"\r\n'
    + '2.0,页签迁移,计划中,"检查跨窗口独立会话\r\n复核远程桌面拖放"\r\n'
    + '2.0,快捷键,计划中,"验证跳转与区域填充\r\n保持一次操作一步撤销"\r\n'
    + '2.0,文档截图,进行中,"使用同一版本实机窗口\r\n仅采用虚构中文数据"\r\n'
    + '2.0,版本发布,计划中,"完成双平台人工验收\r\n整理发行说明与下载"\r\n'
    + '2.0,编码检测,已完成,"GBK 中文关键字可筛选\r\n字节边界复核完成"\r\n'
    + '2.0,大文本复制,已完成,"系统剪贴板与应用内令牌\r\n按体量自动选择"\r\n'
    + '2.0,外部粘贴,进行中,"区域与整列粘贴原子化\r\n固定点确认保留现场"\r\n'
    + '2.0,行列清除,已完成,"稀疏目标一次撤销\r\n清除后不产生多余历史"\r\n'
    + '2.0,表头切换,已完成,"首行数据语义保持稳定\r\n筛选视图同步复位"\r\n'
    + '2.0,内存排序,已完成,"估算进度与取消可用\r\n排序后编辑映射正确"\r\n'
    + '2.0,保存语义,已完成,"未编辑行保留原始字节\r\n新增行使用 CRLF"\r\n'
    + '2.0,文件关联,已完成,"CSV TXT LOG 可选编辑器\r\n不修改系统默认应用"\r\n'
    + '2.1,单实例入口,已完成,"多路径归入既有窗口\r\n重复文件激活原页签"\r\n'
    + '2.1,窗口迁移,进行中,"保持会话与撤销历史\r\n取消时恢复源页签"\r\n'
    + '2.1,拖放预览,待验收,"检查远程桌面坐标\r\n验证多显示器边界"\r\n'
    + '2.1,关闭确认,计划中,"仅关闭当前文档窗口\r\n保留其他独立会话"\r\n'
    + '2.1,缩放布局,进行中,"网格与窗格独立缩放\r\n页签分界保持对齐"\r\n'
    + '2.1,快捷键,计划中,"核对跳转与区域选择\r\n避免编辑器按键冲突"\r\n'
    + '2.1,故障恢复,计划中,"模拟保存冲突与外部变更\r\n保留用户未保存内容"\r\n'
    + '2.1,发布说明,进行中,"精简首页并分层手册\r\n补齐实机截图"\r\n'
    + '2.1,安装包,待验收,"核对 Windows 与 macOS 下载\r\n记录平台人工检查"\r\n'
    + '2.1,最终验收,计划中,"完成完整自动化回归\r\n复核双真实窗口冒烟"\r\n';
  const checklist = '检查项,负责人,状态,备注\r\n'
    + 'Windows 启动,测试,通过,全新用户目录启动\r\n'
    + 'macOS 首次打开,发布,待实机,保留未公证提示\r\n'
    + 'CSV 中文编码,引擎,通过,UTF-8 与 GBK 均覆盖\r\n'
    + 'TXT 空行保存,引擎,通过,未编辑行保留原始字节\r\n'
    + '分屏拖放,测试,待手测,远程桌面场景单独验收\r\n'
    + '多行编辑窗格,前端,通过,格内单行与全文同步\r\n'
    + '撤销重做,引擎,通过,区域操作一步恢复\r\n'
    + '大文件索引,引擎,通过,首屏后渐进完成\r\n'
    + '发行说明,发布,进行中,核对下载与文档链接\r\n'
    + '真实窗口冒烟,测试,通过,双构建完整回归\r\n'
    + '打开对话框,前端,通过,支持 CSV TXT LOG\r\n'
    + '拖放打开,前端,通过,多路径归入页签\r\n'
    + '启动多路径,后端,通过,按当前目录解析\r\n'
    + 'Windows 第二实例,后端,通过,重复文件激活原签\r\n'
    + 'macOS Opened URL,后端,待实机,仅接受本地文件\r\n'
    + 'CSV 多行格,引擎,通过,格内单行窗格全文\r\n'
    + 'CSV 引号逗号,引擎,通过,按 CSV 规则解析\r\n'
    + 'GBK 保存,引擎,通过,保留原编码\r\n'
    + 'BOM 保留,引擎,通过,不重复写入\r\n'
    + 'LOG 字面标点,引擎,通过,逗号引号不拆分\r\n'
    + 'TXT 区域粘贴,前端,通过,多行按物理行写入\r\n'
    + '撤销内存预算,引擎,通过,进程预算 64 MiB\r\n'
    + '大区域固定点,引擎,通过,确认后保留现场\r\n'
    + '筛选三态,前端,通过,包含与两种等于\r\n'
    + '数值排序,引擎,通过,非数字置后\r\n'
    + '跨窗口迁移,后端,通过,保持会话与历史\r\n'
    + '单标签拖出,前端,待手测,唯一页签仍可拖回\r\n'
    + '高 DPI 布局,前端,通过,页签与网格分界对齐\r\n'
    + 'README 链接,文档,通过,相对路径均存在\r\n'
    + '截图临时清理,测试,通过,只清理自有文件\r\n';
  const log = '2026-10-28 09:30:01 INFO  应用启动完成\r\n'
    + '2026-10-28 09:30:02 INFO  载入 release-plan.csv\r\n'
    + '\r\n'
    + '2026-10-28 09:30:03 DEBUG 路径 C:\\demo\\release，逗号, 与 "引号" 均为普通字符\r\n'
    + '2026-10-28 09:30:04 INFO  索引完成，共 30 条记录\r\n'
    + '2026-10-28 09:30:05 WARN  这是一条用于展示的虚构警告\r\n'
    + '2026-10-28 09:30:06 INFO  多行备注已在右侧窗格复核\r\n'
    + '2026-10-28 09:30:07 INFO  分屏 A/B 页签分组就绪\r\n'
    + '2026-10-28 09:30:08 DEBUG Tab\t与反斜杠\\均按字面显示\r\n'
    + '2026-10-28 09:30:09 INFO  原始编码与换行保存检查完成\r\n'
    + '2026-10-28 09:30:10 INFO  截图夹具准备完成\r\n'
    + '2026-10-28 09:30:11 INFO  打开 release-checklist.csv\r\n'
    + '2026-10-28 09:30:12 INFO  分屏左右会话独立\r\n'
    + '\r\n'
    + '2026-10-28 09:30:13 DEBUG 筛选 "状态" 包含 已完成, 保留逗号\r\n'
    + '2026-10-28 09:30:14 INFO  当前格全文同步到编辑窗格\r\n'
    + '2026-10-28 09:30:15 WARN  虚构警告，不表示真实故障\r\n'
    + '2026-10-28 09:30:16 INFO  撤销与重做历史保持独立\r\n'
    + '2026-10-28 09:30:17 DEBUG 路径 C:\\demo\\logs\\server.log\r\n'
    + '2026-10-28 09:30:18 INFO  中间空行保留\r\n'
    + '\r\n'
    + '2026-10-28 09:30:19 INFO  UTF-8 编码检查通过\r\n'
    + '2026-10-28 09:30:20 INFO  GBK 编码检查通过\r\n'
    + '2026-10-28 09:30:21 INFO  未编辑行保留原始字节\r\n'
    + '2026-10-28 09:30:22 DEBUG 逗号, 双引号 "引用" 反斜杠 \\ 均为字面字符\r\n'
    + '2026-10-28 09:30:23 INFO  单元格禁止 CR 与 LF\r\n'
    + '2026-10-28 09:30:24 INFO  多行剪贴板按物理行写入\r\n'
    + '2026-10-28 09:30:25 INFO  首行作为普通数据\r\n'
    + '2026-10-28 09:30:26 INFO  虚拟列名固定为列1\r\n'
    + '2026-10-28 09:30:27 INFO  列结构操作已锁定\r\n'
    + '2026-10-28 09:30:28 INFO  另存扩展名限制为 TXT 与 LOG\r\n'
    + '2026-10-28 09:30:29 INFO  纯文本场景复核完成\r\n'
    + '2026-10-28 09:30:30 INFO  截图数据全部为虚构内容';
  const notes = '发布前检查\r\n\r\n确认所有截图均来自同一版本。\r\n不要使用真实业务数据。';
  const fixtureDir = `${dir}/simpletable-readme-${token}`;
  return [
    { path: `${fixtureDir}/release-plan.csv`, text: main },
    { path: `${fixtureDir}/release-checklist.csv`, text: checklist },
    { path: `${fixtureDir}/server.log`, text: log },
    { path: `${fixtureDir}/notes.txt`, text: notes },
  ];
}

async function waitFor(predicate, label) {
  for (let i = 0; i < 200; i += 1) {
    if (predicate()) return;
    await sleep(100);
  }
  throw new Error(`README 场景等待超时：${label}`);
}

async function writeBytes(invoke, path, text) {
  await invoke('smoke_write_bytes', { path, bytes: [...new TextEncoder().encode(text)] });
}

async function openReady(paths) {
  if (!(await window.__app.openPathsAsTabs(paths))) throw new Error('README 夹具打开失败');
  await waitFor(() => window.__app.model.rowCount() > 0, '首屏行数');
  await window.__app.model.warmup();
}

/** 布置一个固定场景并写本轮唯一 ready/error 标记。 */
export async function runReadmeScene({ invoke, grid, mode }) {
  const { scene, token } = parseReadmeMode(mode);
  const dir = await invoke('smoke_tmp_dir');
  const base = `${dir}/simpletable-readme-${token}`;
  const files = createReadmeFixtures(dir, token);
  try {
    for (const file of files) await writeBytes(invoke, file.path, file.text);
    const [main, checklist, log, notes] = files.map((file) => file.path);
    if (scene === 'main') {
      await openReady([main, log, notes]);
      await waitFor(() => window.__app.tabs().length === 3, '三个页签');
      grid.select([1, 3]);
    } else if (scene === 'split') {
      await openReady([main, checklist]);
      const right = window.__app.tabs().find((tab) => tab.path === checklist);
      if (!right) throw new Error('分屏右签不存在');
      await window.__app.openSplit(right.id);
      await waitFor(() => window.__app.split().on
        && document.querySelectorAll('#grid-area2 .cell').length > 0, '分屏绘制');
      grid.select([0, 3]);
    } else {
      await openReady([log]);
      await waitFor(() => window.__app.model.isTextLines() === true
        && window.__app.model.columnCount() === 1, 'TextLines 一列');
      grid.select([3, 0]);
    }
    await sleep(700);
    await writeBytes(invoke, `${base}-ready.txt`, scene);
  } catch (error) {
    await writeBytes(invoke, `${base}-error.txt`, String(error?.stack ?? error));
    throw error;
  }
}
