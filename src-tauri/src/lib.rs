//! simpleTable Tauri 壳层：文件读写（编码识别）、系统对话框、版本号、
//! 窗口标题、拖放打开、--smoke 自检通道。
//!
//! 前端经 `window.__TAURI__.core.invoke` 调用本模块命令；文件内容一律
//! 以字符串形式跨边界传输，编码处理全部收敛在本层（对齐 csv_io.py）。

use std::fs;
use std::io::ErrorKind;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// 冒烟自检强制大模式（运行期开关，供 smoke 大文件段在小夹具上路由大模式）。
static FORCE_BIG: AtomicBool = AtomicBool::new(false);

use serde::Serialize;
use tauri::{Emitter, Manager};

mod engine;
use engine::{BigFile, BigMeta, ColOp, FilterSpec};

/// 读文件结果：解码后的纯文本 + 实际使用的编码名（供状态栏展示）。
#[derive(Serialize)]
struct ReadResult {
    text: String,
    encoding: String,
}

/// 计算版本字符串（构建期已由 build.rs 注入 BUILD_VERSION）。
///
/// # Returns
/// `release vX.Y.Z`（HEAD 精确匹配 v 开头 tag）或 `beta+<短哈希>`。
#[tauri::command]
fn get_version() -> String {
    env!("BUILD_VERSION").to_string()
}

/// 当前是否处于 --smoke 冒烟自检模式。
///
/// # Returns
/// 启动参数含 `--smoke` 时为 true。
#[tauri::command]
fn is_smoke() -> bool {
    std::env::var_os("SIMPLE_TABLE_SMOKE").is_some()
}

/// 冒烟自检是否强制走大文件模式（SIMPLE_TABLE_FORCE_BIG 置位或运行期开启）。
///
/// # Returns
/// 环境变量 `SIMPLE_TABLE_FORCE_BIG` 存在或 smoke 运行期开启时为 true。
#[tauri::command]
fn smoke_force_big() -> bool {
    std::env::var_os("SIMPLE_TABLE_FORCE_BIG").is_some() || FORCE_BIG.load(Ordering::Relaxed)
}

/// 冒烟自检辅助：运行期开关强制大模式（SIMPLE_TABLE_FORCE_BIG 的环境量等价物）。
///
/// # Parameters
/// - `v`: 是否强制。
#[tauri::command]
fn smoke_set_force_big(v: bool) {
    FORCE_BIG.store(v, Ordering::Relaxed);
}

/// 冒烟自检结束：写报告文件并退出进程。
///
/// # Parameters
/// - `report`: 自检报告全文（UTF-8）。
/// - `code`: 进程退出码（0 = 全部通过）。
///
/// 冒烟报告路径：环境变量 `SMOKE_REPORT_PATH` 优先（CI 显式指定），
/// 否则取系统临时目录（本机默认）。
fn smoke_report_path() -> std::path::PathBuf {
    std::env::var("SMOKE_REPORT_PATH").map(std::path::PathBuf::from).unwrap_or_else(|_| {
        std::env::temp_dir().join("simpletable_smoke_report.txt")
    })
}

/// # Returns
/// 不返回（进程退出）。
#[tauri::command]
fn finish_smoke(report: String, code: i32) {
    let path = smoke_report_path();
    if let Err(e) = fs::write(&path, report) {
        // 写失败必须出声（CI 依赖该文件），否则无声挂死难排查
        eprintln!("[smoke] 报告写入失败 {path:?}: {e}");
    }
    eprintln!("[smoke] 报告已写入 {path:?}，退出码 {code}");
    std::process::exit(code);
}

/// 冒烟自检辅助：按字节数组写文件（构造 GBK 等编码夹具用，仅 --smoke 模式调用）。
///
/// # Parameters
/// - `path`: 目标路径。
/// - `bytes`: 原始字节。
///
/// # Returns
/// 空元组表示成功。
///
/// # Errors
/// 写入失败返回错误串。
#[tauri::command]
fn smoke_write_bytes(path: String, bytes: Vec<u8>) -> Result<(), String> {
    // 前端按名字构造子目录（如 tabm/a.csv），父目录不存在时先建
    if let Some(parent) = std::path::Path::new(&path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| io_err(&path, &e))?;
        }
    }
    fs::write(&path, bytes).map_err(|e| io_err(&path, &e))
}

/// 冒烟自检辅助：把一行文本重复写入文件若干次（大文件夹具用；
/// 避免巨量字节经 IPC 传输）。UTF-8 编码，调用方自行带 \r\n。
///
/// # Parameters
/// - `path`: 目标路径。
/// - `line`: 单行文本（可含行尾）。
/// - `times`: 重复次数。
/// - `append`: true 追加写；false 覆盖写。
///
/// # Returns
/// 空元组表示成功。
///
/// # Errors
/// 写入失败返回错误串。
#[tauri::command]
fn smoke_write_repeat(path: String, line: String, times: u32, append: bool) -> Result<(), String> {
    use std::io::Write;
    let f = if append {
        fs::OpenOptions::new().append(true).open(&path)
    } else {
        fs::File::create(&path)
    };
    let f = f.map_err(|e| io_err(&path, &e))?;
    // 行数可达百万级，逐行直写是百万次小写入系统调用，加缓冲一次性落盘
    let mut w = std::io::BufWriter::new(f);
    for _ in 0..times {
        w.write_all(line.as_bytes()).map_err(|e| io_err(&path, &e))?;
    }
    w.flush().map_err(|e| io_err(&path, &e))?;
    Ok(())
}

/// 冒烟自检辅助：返回临时目录路径（前端构造夹具文件用）。
///
/// # Returns
/// 系统临时目录字符串。
#[tauri::command]
fn smoke_tmp_dir() -> String {
    std::env::temp_dir().to_string_lossy().into_owned()
}

/// 读取 CSV 文件并按 utf-8-sig → utf-8 → gbk 顺序识别编码（对齐 csv_io.py）。
///
/// # Parameters
/// - `path`: 文件绝对路径。
///
/// # Returns
/// 解码文本与编码名；IO 失败时返回错误字符串。
///
/// # Errors
/// 文件不存在/不可读时返回 OS 错误描述。
#[tauri::command]
fn read_csv_file(path: String) -> Result<ReadResult, String> {
    let bytes = fs::read(&path).map_err(|e| io_err(&path, &e))?;
    // utf-8-sig：带 BOM 的 UTF-8（Windows Excel 默认产物）
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let (text, _, _) = encoding_rs::UTF_8.decode(&bytes[3..]);
        return Ok(ReadResult { text: text.into_owned(), encoding: "utf-8-sig".into() });
    }
    // 纯 UTF-8（无 BOM）
    if let Ok(text) = std::str::from_utf8(&bytes) {
        return Ok(ReadResult { text: text.to_string(), encoding: "utf-8".into() });
    }
    // GBK 回退（老式中文 CSV）
    let (text, _, _) = encoding_rs::GBK.decode(&bytes);
    Ok(ReadResult { text: text.into_owned(), encoding: "gbk".into() })
}

/// 以 UTF-8 BOM 写出 CSV（对齐 csv_io.py 的 utf-8-sig 约定）。
///
/// # Parameters
/// - `path`: 目标文件绝对路径。
/// - `text`: CSV 全文（前端已序列化，\r\n 行尾）。
///
/// # Returns
/// 空元组表示成功。
///
/// # Errors
/// 写入失败返回 OS 错误描述。
#[tauri::command]
fn write_csv_file(path: String, text: String) -> Result<(), String> {
    let mut bytes = Vec::with_capacity(text.len() + 3);
    bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    bytes.extend_from_slice(text.as_bytes());
    fs::write(&path, bytes).map_err(|e| io_err(&path, &e))
}

/// 打开文件对话框（原生，rfd）。
///
/// # Returns
/// 选中的路径；取消返回 null。
#[tauri::command]
fn pick_open_path() -> Option<String> {
    rfd::FileDialog::new()
        .add_filter("CSV 表格", &["csv"])
        .add_filter("所有文件", &["*"])
        .pick_file()
        .map(|p| p.to_string_lossy().into_owned())
}

/// 保存文件对话框（原生，rfd）。
///
/// # Parameters
/// - `default_name`: 默认文件名（如 "未命名.csv"）。
///
/// # Returns
/// 选中的路径；取消返回 null。
#[tauri::command]
fn pick_save_path(default_name: String) -> Option<String> {
    rfd::FileDialog::new()
        .add_filter("CSV 表格", &["csv"])
        .set_file_name(&default_name)
        .save_file()
        .map(|p| p.to_string_lossy().into_owned())
}

/// 设置窗口标题（脏标记 `*名 - simpleTable` 由前端计算）。
///
/// # Parameters
/// - `title`: 完整标题文本。
#[tauri::command]
fn set_title(window: tauri::Window, title: String) {
    let _ = window.set_title(&title);
}

/// 把 IO 错误格式化为前端可展示的中文描述。
///
/// # Parameters
/// - `path`: 出错文件路径。
/// - `e`: 原始错误。
///
/// # Returns
/// 中文错误串。
fn io_err(path: &str, e: &std::io::Error) -> String {
    let reason = match e.kind() {
        ErrorKind::NotFound => "文件不存在".to_string(),
        ErrorKind::PermissionDenied => "权限不足".to_string(),
        _ => e.to_string(),
    };
    format!("{path}: {reason}")
}

/// 启动参数状态：命令行携带的待打开 CSV（文件关联/双击打开用）。
struct StartupArgs {
    open_path: Option<String>,
}

/// 大文件会话状态：一次只开一个（打开新文件先关旧）。
/// Arc 托管以便长命令把会话搬进 spawn_blocking（离开 tauri::State 借用）。
type BigShared = std::sync::Arc<Mutex<Option<BigFile>>>;

/// 取大文件会话锁，未打开时返回统一错误串。
fn with_big<T>(
    state: &BigShared,
    f: impl FnOnce(&mut BigFile) -> Result<T, String>,
) -> Result<T, String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    let bf = guard.as_mut().ok_or("大文件会话未打开")?;
    f(bf)
}

/// 查询文件大小（字节），打开路径路由用：≥ 阈值走大文件模式。
#[tauri::command]
fn file_size(path: String) -> Result<u64, String> {
    let meta = fs::metadata(&path).map_err(|e| io_err(&path, &e))?;
    Ok(meta.len())
}

/// 打开大文件会话（mmap + 行索引，秒开）。
#[tauri::command]
fn big_open(path: String, state: tauri::State<BigShared>) -> Result<BigMeta, String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    *guard = Some(BigFile::open(std::path::Path::new(&path))?);
    Ok(guard.as_ref().unwrap().meta())
}

/// 关闭大文件会话。
#[tauri::command]
fn big_close(state: tauri::State<BigShared>) {
    if let Ok(mut guard) = state.lock() {
        *guard = None;
    }
}

/// 元数据（行/列/表头/编码/待执行列操作/撤销栈状态）。
#[tauri::command]
fn big_meta(state: tauri::State<BigShared>) -> Result<BigMeta, String> {
    with_big(&state, |bf| Ok(bf.meta()))
}

/// 外部程序是否改动过文件（focus 时前端拉取）。
#[tauri::command]
fn big_stale(state: tauri::State<BigShared>) -> Result<bool, String> {
    with_big(&state, |bf| Ok(bf.stale()))
}

/// 窗口取行（含覆盖层/插入块/删除位图的视图行）。
#[tauri::command]
fn big_rows(state: tauri::State<BigShared>, from: u64, count: u32) -> Result<Vec<Vec<String>>, String> {
    with_big(&state, |bf| Ok(bf.rows(from, count)))
}

/// 编辑单元格（源行进覆盖层，插入行直写）。
#[tauri::command]
fn big_set_cell(
    state: tauri::State<BigShared>,
    vrow: u64,
    col: u32,
    value: String,
) -> Result<(), String> {
    with_big(&state, |bf| bf.set_cell(vrow, col, value))
}

/// 在 vpos 处插入行（rows 为 null 时插 count 行空行）。
#[tauri::command]
fn big_insert_rows(
    state: tauri::State<BigShared>,
    vpos: u64,
    rows: Option<Vec<Vec<String>>>,
    count: u32,
) -> Result<(), String> {
    with_big(&state, |bf| bf.insert_rows(vpos, rows, count))
}

/// 删除一组 vrow（升序传入），返回删除行数。
#[tauri::command]
fn big_remove_rows(state: tauri::State<BigShared>, vrows: Vec<u64>) -> Result<u32, String> {
    with_big(&state, |bf| bf.remove_rows(vrows))
}

#[tauri::command]
fn big_undo(state: tauri::State<BigShared>) -> Result<bool, String> {
    with_big(&state, |bf| Ok(bf.undo()))
}

#[tauri::command]
fn big_redo(state: tauri::State<BigShared>) -> Result<bool, String> {
    with_big(&state, |bf| Ok(bf.redo()))
}

/// 执行一次筛选（rayon 并行扫描），返回命中数。
#[tauri::command]
fn big_filter(state: tauri::State<BigShared>, spec: FilterSpec) -> Result<u32, String> {
    with_big(&state, |bf| bf.run_filter(&spec))
}

#[tauri::command]
fn big_filter_count(state: tauri::State<BigShared>) -> Result<u32, String> {
    with_big(&state, |bf| Ok(if bf.has_filter() { bf.filter_len() } else { 0 }))
}

#[tauri::command]
fn big_filter_page(
    state: tauri::State<BigShared>,
    from: u32,
    count: u32,
) -> Result<Vec<Vec<String>>, String> {
    with_big(&state, |bf| Ok(bf.filter_page(from, count)))
}

#[tauri::command]
fn big_clear_filter(state: tauri::State<BigShared>) -> Result<(), String> {
    with_big(&state, |bf| {
        bf.clear_filter();
        Ok(())
    })
}

/// 大选区复制（TSV，\r\n 行尾）。
#[tauri::command]
fn big_copy(
    state: tauri::State<BigShared>,
    r0: u64,
    r1: u64,
    c0: u32,
    c1: u32,
) -> Result<String, String> {
    with_big(&state, |bf| bf.copy_tsv(r0, r1, c0, c1))
}

/// 保存（流式重写 + 原子换名），进度经 "big-progress" 事件推送。
///
/// 长操作走 spawn_blocking（独立 OS 线程，不占 tokio worker），进度经
/// 专用转发线程 emit——重写线程全程不触碰 WebView2 COM。v4 采样实证：
/// 重写期间主线程被挂起（Wait/WrSuspended）、进度事件挤压到命令结束才
/// 到达渲染进程，用户看到"进度条卡住 + 窗口未响应"。
#[tauri::command]
async fn big_save(window: tauri::Window, state: tauri::State<'_, BigShared>) -> Result<(), String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // (加权done, total, 阶段, 行数)：阶段 0 流式重写 / 1 落盘换名 / 2 重建索引。
        let (tx, rx) = std::sync::mpsc::channel::<(u64, u64, u8, u64)>();
        let emitter = std::thread::spawn(move || {
            while let Ok((done, total, phase, rows)) = rx.recv() {
                let _ = window.emit("big-progress", (done, total, phase, rows));
            }
        });
        let result = with_big(&shared, |bf| {
            bf.save(move |done, total, phase, rows| {
                let _ = tx.send((done, total, phase, rows));
            })
        });
        // save 返回后进度闭包随 tx 一并释放，emit 线程 recv 出错即退出。
        emitter.join().map_err(|e| format!("进度转发线程异常：{e:?}"))?;
        result
    })
    .await
    .map_err(|e| format!("保存任务被取消：{e}"))?
}

// ---- 列操作：pending 预览 → 执行物理重写 ----

#[tauri::command]
fn big_col_push(state: tauri::State<BigShared>, op: ColOp) -> Result<(), String> {
    with_big(&state, |bf| {
        bf.push_col_op(op);
        Ok(())
    })
}

#[tauri::command]
fn big_col_pop(state: tauri::State<BigShared>) -> Result<(), String> {
    with_big(&state, |bf| {
        bf.pop_col_op();
        Ok(())
    })
}

#[tauri::command]
fn big_col_clear(state: tauri::State<BigShared>) -> Result<(), String> {
    with_big(&state, |bf| {
        bf.clear_col_ops();
        Ok(())
    })
}

/// 执行待处理的列操作（物理重写整个文件，带进度事件）。
/// 同 big_save：spawn_blocking + 专用 emit 转发线程（见 big_save 注释）。
#[tauri::command]
async fn big_col_apply(window: tauri::Window, state: tauri::State<'_, BigShared>) -> Result<(), String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (tx, rx) = std::sync::mpsc::channel::<(u64, u64, u8, u64)>();
        let emitter = std::thread::spawn(move || {
            while let Ok((done, total, phase, rows)) = rx.recv() {
                let _ = window.emit("big-progress", (done, total, phase, rows));
            }
        });
        let result = with_big(&shared, |bf| {
            bf.apply_col_ops(move |done, total, phase, rows| {
                let _ = tx.send((done, total, phase, rows));
            })
        });
        emitter.join().map_err(|e| format!("进度转发线程异常：{e:?}"))?;
        result
    })
    .await
    .map_err(|e| format!("列操作任务被取消：{e}"))?
}

/// 取命令行第一个 .csv 路径（启动时由前端拉取并打开）。
///
/// # Parameters
/// - `state`: 托管的启动参数。
///
/// # Returns
/// 路径字符串；无则 null。
#[tauri::command]
fn get_startup_path(state: tauri::State<StartupArgs>) -> Option<String> {
    state.open_path.clone()
}

/// 截屏诊断模式：启动参数 `--park <mode>` 时返回 mode 字符串，否则 None。
///
/// 窗口停在指定渲染场景待命（进程不退出），供外部截屏工具读像素、
/// 验证 WebView2 真实绘制结果（DOM 探针看不到绘制级缺陷）。
#[tauri::command]
fn is_park() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--park" {
            return args.next();
        }
    }
    None
}

/// 截屏诊断辅助：探测路径是否存在（--park 模式与外部截屏工具握手用）。
///
/// # Parameters
/// - `path`: 待查路径。
///
/// # Returns
/// 存在 true，否则 false。
#[tauri::command]
fn path_exists(path: String) -> bool {
    std::path::Path::new(&path).exists()
}

/// 应用入口：注册命令、接管线外拖放事件、运行事件循环。
///
/// # Returns
/// 不返回（事件循环退出时进程结束）。
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // 命令行 .csv 参数（资源管理器"打开方式"/双击关联场景）
            let open_path = std::env::args()
                .skip(1)
                .find(|a| a.to_lowercase().ends_with(".csv"));
            app.manage(StartupArgs { open_path });
            app.manage(BigShared::new(Mutex::new(None)));
            // 关闭请求转发给前端做"未保存确认"，确认后由 quit_app 命令真正退出
            if let Some(window) = app.get_webview_window("main") {
                let handle = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = handle.emit("close-request", ());
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_version,
            get_startup_path,
            is_smoke,
            is_park,
            path_exists,
            smoke_force_big,
            smoke_set_force_big,
            finish_smoke,
            smoke_write_bytes,
            smoke_write_repeat,
            smoke_tmp_dir,
            read_csv_file,
            write_csv_file,
            pick_open_path,
            pick_save_path,
            set_title,
            quit_app,
            file_size,
            big_open,
            big_close,
            big_meta,
            big_stale,
            big_rows,
            big_set_cell,
            big_insert_rows,
            big_remove_rows,
            big_undo,
            big_redo,
            big_filter,
            big_filter_count,
            big_filter_page,
            big_clear_filter,
            big_copy,
            big_save,
            big_col_push,
            big_col_pop,
            big_col_clear,
            big_col_apply,
        ])
        .build(tauri::generate_context!())
        .expect("error while building simpleTable")
        .run(|_app, _event| {});
}

/// 前端确认关闭（或保存完毕）后调用：立即退出进程。
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}
