// 截屏诊断（--park <mode>）：把窗口停在指定渲染场景待命（进程不退出），
// 配合仓库根目录 park_capture.ps1 从 Windows 侧截屏读像素。
// 用途：验证 WebView2 真实绘制结果——DOM 探针（offsetTop/rect）只能证明几何正确，
// 绘制级缺陷（如 ~2^22 px 钳制）只有亲眼看到像素才能定论。

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** 真机大文件（用户报告的复现文件，2.2GB / 1553 万行） */
const BIG = "D:/SVN_Cursor/simpleTable-public/price_j8888.csv";

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

  await mark(invoke, dir, "error", `unknown mode ${mode}`);
}
