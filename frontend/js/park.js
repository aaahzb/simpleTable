// 截屏诊断（--park <mode>）：把窗口停在指定渲染场景待命（进程不退出），
// 配合仓库根目录 park_capture.ps1 从 Windows 侧截屏读像素。
// 用途：验证 WebView2 真实绘制结果——DOM 探针（offsetTop/rect）只能证明几何正确，
// 绘制级缺陷（如 ~2^22 px 钳制）只有亲眼看到像素才能定论。

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** 真机大文件（用户报告的复现文件，2.2GB / 1553 万行） */
const BIG = "D:/SVN_Cursor/simpleTable-public/price_j8888.csv";

/** 删列实验对象：recovered 文件的副本（真机 2.16GB，不动用户数据） */
const DELCOL = "park_delcol.csv";

/**
 * 渲染现场诊断快照：滚动位置/画布高/渲染窗口/容器 transform。
 *
 * @param {object} grid - Grid 实例。 @param {HTMLElement} sc - 滚动容器。
 * @returns {string} JSON 诊断串。
 */
function diag(grid, sc) {
  const kids = grid.rowsEl.children;
  const rowNo = (el) => (el && el.querySelector(".row-head"))
    ? Number(el.querySelector(".row-head").dataset.row) : -1;
  return JSON.stringify({
    st: Math.round(sc.scrollTop),
    sh: sc.scrollHeight,
    kids: kids.length,
    first: rowNo(kids[0]),
    last: rowNo(kids[kids.length - 1]),
    tf: grid.rowsEl.style.transform.slice(0, 20),
  });
}

/**
 * 写握手标记文件。
 *
 * @param {Function} invoke - Tauri invoke。 @param {string} dir - 标记目录。
 * @param {string} name - 标记名（不含扩展名）。 @param {string} text - 标记内容。
 */
async function mark(invoke, dir, name, text = "1") {
  const bytes = [...new TextEncoder().encode(text)];
  await invoke("smoke_write_bytes", { path: `${dir}/${name}.txt`, bytes });
}

/**
 * 轮询等待外部截屏工具的回执文件。
 *
 * @param {Function} invoke - Tauri invoke。 @param {string} dir - 标记目录。
 * @param {string} name - 回执名（不含扩展名）。
 */
async function waitGo(invoke, dir, name) {
  for (;;) {
    if (await invoke("path_exists", { path: `${dir}/${name}.txt` })) return;
    await sleep(200);
  }
}

/**
 * 入口：按 mode 摆好渲染场景，写 ready 标记后常驻。
 *
 * @param {object} ctx - 依赖注入。 @param {Function} ctx.invoke - Tauri invoke。
 * @param {object} ctx.grid - Grid 实例。 @param {string} ctx.mode - 场景名。
 */
export async function runPark({ invoke, grid, mode }) {
  // 标记直接写临时目录根（smoke_write_bytes 不会建子目录）
  const dir = await invoke("smoke_tmp_dir");
  // 入口即写 hello：区分"runPark 没进来"与"进来后挂起"
  await mark(invoke, dir, "hello", `mode=${mode}`);
  const sc = document.getElementById("grid-scroll");

  if (mode === "userpos" || mode === "userpos-raw" || mode === "userpos1") {
    // 复现用户现场：2.2GB 文件、缩放 2×、滚动到报告的空白起始行 3,900,143
    // （userpos1 = 缩放 1× 对照；百分比定位下两者落在同一压缩滚动位置）
    await invoke("smoke_set_force_big", { v: true });
    await window.__app.openPath(BIG);
    grid.setZoom(mode === "userpos1" ? 1 : 2);
    await sleep(300);
    const oy = grid._originY();
    const h = grid._uniformRowH;
    sc.scrollTop = Math.round(oy + (3900143 * h) / grid._yScale);
    await sleep(600);
    sc.scrollTop = Math.round(oy + (3900143 * h) / grid._yScale);
    await sleep(250);
    await mark(invoke, dir, "diag_0", diag(grid, sc));
    if (mode === "userpos-raw") {
      // 对照实验：剥掉容器 transform，行顶还原为旧版全局真实坐标编码
      // （8107b5d 的画法：top ≈ 滚动条坐标 ± 窗口，最大 16.7M）
      grid.rowsEl.style.transform = "";
      const { sv, anchor } = grid._yAnchor();
      [...grid.rowsEl.children].forEach((k) => {
        const vi = grid.visibleIndexOf(Number(k.querySelector(".row-head").dataset.row));
        k.style.top = `${sv + (vi - anchor) * h}px`;
      });
    }
    await mark(invoke, dir, "ready_0", String(Math.round(sc.scrollTop)));
    return;
  }

  if (mode === "scan") {
    // 钳制定位扫描：20 万行夹具 → 画布 ~4.6M px（< Y_CLAMP，不触发虚拟缩放，
    // 纯静态渲染），细粒度扫 4.19M~4.25M（2^22=4,194,304 在区间内），
    // 每段等外部截屏后再继续
    const p = `${await invoke("smoke_tmp_dir")}/park200k.csv`;
    await invoke("smoke_write_repeat", { path: p, line: "a,b,c,d,e\n", times: 200001, append: false });
    await invoke("smoke_set_force_big", { v: false });
    await window.__app.openPath(p);
    await sleep(600);
    const targets = [4190000, 4192000, 4193000, 4194000, 4194300, 4194400, 4196000, 4250000];
    for (let i = 0; i < targets.length; i += 1) {
      sc.scrollTop = Math.min(targets[i], sc.scrollHeight);
      await sleep(450);
      await mark(invoke, dir, `ready_${i}`, String(Math.round(sc.scrollTop)));
      await waitGo(invoke, dir, `go_${i}`);
    }
    await mark(invoke, dir, "done");
    return;
  }

  if (mode === "track") {
    // 零干预跟踪：装载后每 200ms 采样滚动位置/渲染窗口，诊断"为何显示在文件底部"
    const p = `${await invoke("smoke_tmp_dir")}/park4m.csv`;
    await mark(invoke, dir, "step_tmpdir");
    await invoke("smoke_set_force_big", { v: false });
    await mark(invoke, dir, "step_forcebig");
    await window.__app.openPath(p);
    await mark(invoke, dir, "step_opened");
    for (let i = 0; i < 30; i += 1) {
      await sleep(200);
      await mark(invoke, dir, `t_${String(i).padStart(2, "0")}`, diag(grid, sc));
    }
    // 再主动设一次 1,000,000，看采样序列如何变化
    sc.scrollTop = 1000000;
    for (let i = 30; i < 45; i += 1) {
      await sleep(200);
      await mark(invoke, dir, `t_${String(i).padStart(2, "0")}`, diag(grid, sc));
    }
    await mark(invoke, dir, "done");
    return;
  }

  if (mode === "wall4m") {
    // 4M 画布安全性验证：173,879 行夹具 → 画布 ≈4,000,09x px（拟议新 Y_CLAMP），
    // 扫低位/中位/近顶/最大滚动位置，全部必须绘制才证明 4M 上限安全
    const rows = 173879;
    const p = `${await invoke("smoke_tmp_dir")}/park4m.csv`;
    await invoke("smoke_write_repeat", { path: p, line: "a,b,c,d,e\n", times: rows, append: false });
    await invoke("smoke_set_force_big", { v: false });
    await window.__app.openPath(p);
    await sleep(600);
    const targets = [1000000, 2000000, 3500000, 3999000];
    for (let i = 0; i < targets.length; i += 1) {
      sc.scrollTop = Math.min(targets[i], sc.scrollHeight);
      await sleep(450);
      // 二次钉扎：布局/渲染落定后再设一次，防滚动位置被夹取/锚定改写
      sc.scrollTop = Math.min(targets[i], sc.scrollHeight);
      await sleep(250);
      await mark(invoke, dir, `diag_${i}`, diag(grid, sc));
      await mark(invoke, dir, `ready_${i}`, String(Math.round(sc.scrollTop)));
      await waitGo(invoke, dir, `go_${i}`);
    }
    await mark(invoke, dir, "done");
    return;
  }

  if (mode === "verify") {
    // 修复终验：真机大文件 + 缩放 2×，按行数百分比拖四个深度，逐段截屏
    await invoke("smoke_set_force_big", { v: true });
    await window.__app.openPath(BIG);
    grid.setZoom(2);
    await sleep(300);
    const oy = grid._originY();
    const h = grid._uniformRowH;
    const n = grid.visCount();
    const fracs = [0.25, 0.5, 0.9, 1.0];
    for (let i = 0; i < fracs.length; i += 1) {
      const row = Math.max(0, Math.round(n * fracs[i]) - 1);
      sc.scrollTop = Math.round(oy + (row * h) / grid._yScale);
      await sleep(500);
      sc.scrollTop = Math.round(oy + (row * h) / grid._yScale);
      await sleep(250);
      await mark(invoke, dir, `diag_${i}`, diag(grid, sc));
      await mark(invoke, dir, `ready_${i}`, `f${fracs[i]}_row${row}_st${Math.round(sc.scrollTop)}`);
      await waitGo(invoke, dir, `go_${i}`);
    }
    await mark(invoke, dir, "done");
    return;
  }

  if (mode === "fixh") {
    // 画布高度对照：同一滚动位置 4.25M，先把画布从 4.6M 改大到 16.7M（仅加高空画布，
    // 行布局不动），截屏对比——区分"滚动位置触发"还是"画布高度触发"绘制剔除
    const p = `${await invoke("smoke_tmp_dir")}/park200k.csv`;
    await invoke("smoke_set_force_big", { v: false });
    await window.__app.openPath(p);
    await sleep(600);
    sc.scrollTop = 4250000;
    await sleep(450);
    const cv = document.getElementById("grid-canvas");
    await mark(invoke, dir, "ready_0", "base425");
    await waitGo(invoke, dir, "go_0");
    cv.style.height = "16700000px"; // 仅画布加高，行布局/滚动位置不变
    await sleep(450);
    await mark(invoke, dir, "ready_1", "tallcanvas");
    await waitGo(invoke, dir, "go_1");
    await mark(invoke, dir, "done");
    return;
  }

  if (mode === "big425") {
    // 大画布对照：16.7M 画布滚到 scrollTop=4.25M（行 ~3,951,870），
    // 与 scan/fixh 同滚动位置不同画布高度，三方对照定位剔除触发条件
    await invoke("smoke_set_force_big", { v: true });
    await window.__app.openPath(BIG);
    grid.setZoom(2);
    await sleep(300);
    const oy = grid._originY();
    const h = grid._uniformRowH;
    const row = Math.round(((4250000 - oy) * grid._yScale) / h);
    sc.scrollTop = Math.round(oy + (row * h) / grid._yScale);
    await sleep(600);
    await mark(invoke, dir, "ready_0", `row=${row} st=${Math.round(sc.scrollTop)}`);
    return;
  }

  if (mode === "paneflicker") {
    // 复现 smoke"取页落定不换空编辑窗格"的等待超时：5000 行夹具、force_big，
    // select(0,0) 后跳 3000，采样 cache/inflight/pane/落定数——观察页 14/15 的
    // 调度与落定到底发生了什么（smoke 现场无法看内部状态）
    const p = `${await invoke("smoke_tmp_dir")}/park5k.csv`;
    await invoke("smoke_write_repeat", { path: p, line: "a,b\r\n", times: 1, append: false });
    await invoke("smoke_write_repeat", { path: p, line: "x,y\r\n", times: 5000, append: true });
    await invoke("smoke_set_force_big", { v: true });
    await window.__app.openPath(p);
    const bm = window.__app.model;
    const noop = () => invoke("path_exists", { path: `${dir}/__noop` });
    for (let i = 0; i < 600; i += 1) {
      if (window.__app.isBigMode() && bm.rowCount() > 0) break;
      await noop();
    }
    const log = [];
    const pane = window.__app.pane;
    grid.select([0, 0]);
    log.push(`selected pane=${JSON.stringify(pane.editEl.value)}`);
    const oy3 = grid._originY();
    const h3 = grid._uniformRowH;
    sc.scrollTop = Math.round(oy3 + (3000 * h3) / grid._yScale);
    grid._renderRows();
    const t0 = performance.now();
    let last = t0;
    let n = 0;
    while (performance.now() - t0 < 6000) {
      await noop();
      if (performance.now() - last >= 200) {
        n += 1;
        // 注意：rawData 未命中会调度拉取（与 smoke 探针同行为）
        const rd = bm.rawData(3000, 0);
        log.push(`s${n} t=${Math.round(performance.now())} rd3000=${JSON.stringify(rd)}` +
          ` cache=${bm._cache.size} inflight=${bm._inflight.size}` +
          ` keys=${[...bm._cache.keys()].join("/")} in=${[...bm._inflight].join("/")}` +
          ` pane=${JSON.stringify(pane.editEl.value)}`);
        last = performance.now();
      }
    }
    await mark(invoke, dir, "paneflicker_log", log.join("\n"));
    await mark(invoke, dir, "done");
    return;
  }

  if (mode === "delcol") {
    // 真机删列实验（不动用户数据）：打开 recovered 副本（2.16GB，天然大模式），
    // 删最后一列 → 执行列操作（物理重写，与保存共用流式重写+换名链路），
    // 采样列数/行数/耗时/文件大小与深行渲染，验证大文件删列全链路。
    const p = `${await invoke("smoke_tmp_dir")}/${DELCOL}`;
    const fsize = async () => {
      try { return Number(await invoke("file_size", { path: p })); }
      catch { return -1; }
    };
    await window.__app.openPath(p);
    const bm = window.__app.model;
    const noop = () => invoke("path_exists", { path: `${dir}/__noop` });
    for (let i = 0; i < 1200; i += 1) { // 2.16GB 索引可能要几分钟
      if (window.__app.isBigMode() && bm.rowCount() > 0) break;
      await noop();
    }
    const before = { rows: bm.rowCount(), cols: bm.columnCount(), bytes: await fsize() };
    await mark(invoke, dir, "delcol_before", JSON.stringify(before));
    const t0 = performance.now();
    await bm.removeColumns([before.cols - 1]); // 预览（pending）
    const pendingCols = bm.columnCount();
    await bm.applyColOps(); // 物理重写（流式重写 + replace_with_retry 换名）
    const ms = Math.round(performance.now() - t0);
    const after = { rows: bm.rowCount(), cols: bm.columnCount(), bytes: await fsize() };
    // 渲染验证：跳到 3,900,143 行附近（userpos 深度）再取诊断
    grid.setZoom(1);
    await sleep(400);
    const oy = grid._originY();
    const h = grid._uniformRowH;
    sc.scrollTop = Math.round(oy + (3900143 * h) / grid._yScale);
    await sleep(600);
    sc.scrollTop = Math.round(oy + (3900143 * h) / grid._yScale);
    await sleep(250);
    await mark(invoke, dir, "delcol_after", JSON.stringify({ ...after, pendingCols, ms }));
    await mark(invoke, dir, "delcol_diag", diag(grid, sc));
    await mark(invoke, dir, "done");
    return;
  }

  if (mode === "filterscroll") {
    // 复现"筛选后仅显示底部几行 + 滚动条跳回"：20 万行夹具（force_big），
    // 压深滚动 → 执行筛选（40 命中）→ 采样滚动位置/渲染窗口/手动拖滚动条是否回弹。
    // 筛选切换坐标系（压缩画布→自然画布），旧 scrollTop 在新坐标系无意义。
    const p = `${await invoke("smoke_tmp_dir")}/park_filter.csv`;
    await invoke("smoke_write_repeat", { path: p, line: "h1,h2,h3,h4,h5\n", times: 1, append: false });
    await invoke("smoke_write_repeat", { path: p, line: "大理,b,c,d,e\n", times: 1, append: true });
    await invoke("smoke_write_repeat", { path: p, line: "a,b,c,d,e\n", times: 4999, append: true });
    for (let i = 0; i < 39; i += 1) {
      await invoke("smoke_write_repeat", { path: p, line: "大理,b,c,d,e\n", times: 1, append: true });
      await invoke("smoke_write_repeat", { path: p, line: "a,b,c,d,e\n", times: 4999, append: true });
    }
    await invoke("smoke_set_force_big", { v: true });
    await window.__app.openPath(p);
    const bm = window.__app.model;
    const noop = () => invoke("path_exists", { path: `${dir}/__noop` });
    for (let i = 0; i < 1200; i += 1) {
      if (window.__app.isBigMode() && bm.rowCount() > 0) break;
      await noop();
    }
    // 压深到 ~97.5%（195,000/200,000 行）
    const oy = grid._originY();
    const h = grid._uniformRowH;
    sc.scrollTop = Math.round(oy + (195000 * h) / grid._yScale);
    await sleep(500);
    sc.scrollTop = Math.round(oy + (195000 * h) / grid._yScale);
    await sleep(300);
    await mark(invoke, dir, "diag_0", diag(grid, sc));
    await mark(invoke, dir, "ready_0", String(Math.round(sc.scrollTop)));
    await waitGo(invoke, dir, "go_0");
    // 执行筛选（40 命中），等浏览器夹取滚动位置 + scroll 事件落定
    grid.filters.set(0, "大理");
    const t0 = performance.now();
    const hits = await window.__app.runBigFilter();
    await sleep(600);
    await mark(invoke, dir, "filter_ms", String(Math.round(performance.now() - t0)));
    await mark(invoke, dir, "diag_1", diag(grid, sc));
    await mark(invoke, dir, "ready_1", `hits=${hits} st=${Math.round(sc.scrollTop)}`);
    await waitGo(invoke, dir, "go_1");
    // 手动拖滚动条到顶/到中：观察是否"跳回来"
    sc.scrollTop = 0;
    await sleep(400);
    await mark(invoke, dir, "diag_2", diag(grid, sc));
    await mark(invoke, dir, "ready_2", `set0 st=${Math.round(sc.scrollTop)}`);
    await waitGo(invoke, dir, "go_2");
    sc.scrollTop = 300;
    await sleep(400);
    await mark(invoke, dir, "diag_3", diag(grid, sc));
    await mark(invoke, dir, "ready_3", `set300 st=${Math.round(sc.scrollTop)}`);
    await waitGo(invoke, dir, "go_3");
    // 清除筛选回全量：反向坐标系切换
    grid.clearFilters();
    await sleep(600);
    await mark(invoke, dir, "diag_4", diag(grid, sc));
    await mark(invoke, dir, "done");
    return;
  }

  if (mode === "flicker2") {
    // 带绘制版：可见窗口下跑同样的扫动，计数 renderAll/renderRows/rAF/DOM 变动。
    // 若存在"ResizeObserver→renderAll→尺寸变化→ResizeObserver"逐帧自激，
    // 只有真实出帧时才会转起来（无头/最小化时 rAF 停摆、循环隐身）。
    await invoke("smoke_set_force_big", { v: true });
    await window.__app.openPath(BIG);
    const bm = window.__app.model;
    const noop = () => invoke("path_exists", { path: `${dir}/__noop` });
    for (let i = 0; i < 600; i += 1) {
      if (window.__app.isBigMode() && bm.rowCount() > 0) break;
      await noop();
    }
    grid.setZoom(2);
    await sleep(300);
    let renderAllN = 0;
    let renderRowsN = 0;
    let mutations = 0;
    const origAll = grid.renderAll.bind(grid);
    const origRows = grid._renderRows.bind(grid);
    grid.renderAll = () => { renderAllN += 1; origAll(); };
    grid._renderRows = () => { renderRowsN += 1; origRows(); };
    new MutationObserver(() => { mutations += 1; })
      .observe(grid.rowsEl, { childList: true });
    let rafs = 0;
    const rafLoop = () => { rafs += 1; requestAnimationFrame(rafLoop); };
    requestAnimationFrame(rafLoop);
    let visible = 0;
    const visLoop = () => { if (!document.hidden) visible += 1; requestAnimationFrame(visLoop); };
    requestAnimationFrame(visLoop);
    const log = [];
    const oy2 = grid._originY();
    const h2 = grid._uniformRowH;
    const n2 = grid.visCount();
    const sample = (tag) => {
      const cells = grid.rowsEl.querySelectorAll(".cell");
      let blank = 0;
      for (const el of cells) { if (el.textContent === "") blank += 1; }
      log.push([tag, Math.round(performance.now()), renderAllN, renderRowsN,
        rafs, visible, mutations, bm._cache.size, bm._inflight.size,
        cells.length, blank, Math.round(sc.scrollTop)].join(","));
    };
    // 慢速扫动 ~2s（120 步，每步 IPC 连发拖时间），接近真实拖动的事件密度
    for (let i = 0; i <= 120; i += 1) {
      const row = Math.round(n2 * (0.005 + 0.6 * (i / 120)));
      sc.scrollTop = Math.round(oy2 + (row * h2) / grid._yScale);
      for (let k = 0; k < 40; k += 1) await noop();
      if (i % 10 === 0) sample(`sweep${i}`);
    }
    sample("sweep-end");
    const t0 = performance.now();
    let last = t0;
    while (performance.now() - t0 < 10000) {
      await noop();
      if (performance.now() - last >= 200) { sample("rest"); last = performance.now(); }
    }
    await mark(invoke, dir, "flicker2_log", log.join("\n"));
    await mark(invoke, dir, "done");
    return;
  }

  await mark(invoke, dir, "error", `unknown mode ${mode}`);
}
