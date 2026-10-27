//! simpleTable Tauri 壳层：文件读写（编码识别）、系统对话框、版本号、
//! 窗口标题、拖放打开、--smoke 自检通道。
//!
//! 前端经 `window.__TAURI__.core.invoke` 调用本模块命令；文件内容一律
//! 以字符串形式跨边界传输，编码处理全部收敛在本层（对齐 csv_io.py）。

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::ErrorKind;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

/// 冒烟自检强制大模式（运行期开关，供 smoke 大文件段在小夹具上路由大模式）。
static FORCE_BIG: AtomicBool = AtomicBool::new(false);

use serde::Serialize;
use tauri::{Emitter, Manager};

mod baseline;
mod drag_registry;
mod drag_preview;
mod clipboard;
mod engine;
pub mod document_registry;
mod history;
pub mod open_request;
pub mod window_registry;
mod order;
mod schema;
mod spool;
use clipboard::{ClipboardMeta, ClipboardStore, CopyOutcome};
use engine::{BigFile, BigMeta, ClearTarget, ColOp, FilterPage, FilterSpec, SortEstimate, SortLevel};
use history::{Approval, EditOutcome};
use document_registry::{
    Access, AdoptedNotice, AdoptedSession, BigEntry, BigSession, DestroyOutcome,
    TransferRoute, TransferUpdate,
};

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
fn is_smoke(app: tauri::AppHandle, window: tauri::Window) -> bool {
    is_diagnostic_main(&app, window.label())
        && std::env::var_os("SIMPLE_TABLE_SMOKE").is_some()
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
fn finish_smoke(
    app: tauri::AppHandle,
    window: tauri::Window,
    report: String,
    code: i32,
) -> Result<(), String> {
    if !is_diagnostic_main(&app, window.label())
        || std::env::var_os("SIMPLE_TABLE_SMOKE").is_none() {
        return Err("只有 Diagnostic main 可以结束冒烟".into());
    }
    let path = smoke_report_path();
    let cleanup_path = app.state::<ClipboardStore>().root().to_path_buf();
    let report = format!("{report}\nCLEANUP_PATH={}", cleanup_path.display());
    if let Err(e) = fs::write(&path, report) {
        // 写失败必须出声（CI 依赖该文件），否则无声挂死难排查
        eprintln!("[smoke] 报告写入失败 {path:?}: {e}");
    }
    eprintln!("[smoke] 报告已写入 {path:?}，退出码 {code}");
    cleanup_process_state(&app);
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
    if let Some(parent) = std::path::Path::new(&path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| io_err(&path, &e))?;
        }
    }
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

/// 冒烟诊断：返回指定文档会话的 owned spool 根目录。
#[tauri::command]
fn smoke_spool_path(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
) -> Result<String, String> {
    with_big(&state, &window, session, lease, Access::Read, |bf| Ok(bf.spool_root().to_string_lossy().into_owned()))
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

/// 进程级运行状态；打开队列先于任何窗口 ready 建立，后续任务在此并入文档和窗口表。
#[derive(Default)]
struct RuntimeState {
    docs: document_registry::DocumentRegistry,
    opens: open_request::OpenRequestQueue,
    windows: window_registry::WindowRegistry,
    drags: drag_registry::DragRegistry,
}

type SharedRuntime = Arc<Mutex<RuntimeState>>;

fn with_runtime<T>(
    app: &tauri::AppHandle,
    f: impl FnOnce(&mut RuntimeState) -> T,
) -> Result<T, String> {
    let state = app.state::<SharedRuntime>();
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    Ok(f(&mut guard))
}

fn recent_ready_window_label(app: &tauri::AppHandle) -> Option<String> {
    let state = app.state::<SharedRuntime>();
    let runtime = state.lock().ok()?;
    runtime.windows.recent_ready()
}

fn recent_window_label(app: &tauri::AppHandle) -> String {
    let state = app.state::<SharedRuntime>();
    if let Ok(runtime) = state.lock() {
        if let Some(label) = runtime.windows.recent_ready()
            .or_else(|| runtime.windows.recent_registered()) {
            return label;
        }
    }
    "main".to_string()
}

fn focus_recent_window(app: &tauri::AppHandle) {
    let Some(label) = recent_ready_window_label(app) else {
        return;
    };
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn diagnostic_mode() -> bool {
    std::env::args_os().any(|arg| arg == "--smoke" || arg == "--park")
}

/// 定向投递打开批次；不可 Unicode 路径和发送失败都必须释放预留并继续排队。
fn dispatch_open_batches(app: &tauri::AppHandle, initial: Vec<open_request::OpenBatch>) {
    let mut scheduled: VecDeque<open_request::OpenBatch> = initial.into();
    while let Some(batch) = scheduled.pop_front() {
        let deliverable = batch.items.iter().filter(|item| match item {
            open_request::OpenItem::Open { path, .. }
            | open_request::OpenItem::Activate { path, .. } => path.to_str().is_some(),
        }).cloned().collect::<Vec<_>>();
        let emitted = if deliverable.is_empty() {
            false
        } else {
            let delivery = open_request::OpenBatch {
                batch_id: batch.batch_id,
                target: batch.target.clone(),
                items: deliverable,
            };
            match app.emit_to(batch.target.as_str(), "open-paths", delivery) {
                Ok(()) => true,
                Err(error) => {
                    eprintln!("打开批次投递失败（{}）：{error}", batch.target);
                    false
                }
            }
        };
        let recent = recent_window_label(app);
        let follow_up = with_runtime(app, |runtime| {
            let mut follow_up = Vec::new();
            for item in &batch.items {
                let open_request::OpenItem::Open { request_id, path } = item else {
                    continue;
                };
                if emitted && path.to_str().is_some() {
                    continue;
                }
                if path.to_str().is_none() {
                    eprintln!("路径不是有效 Unicode，无法安全分发：{path:?}");
                }
                match runtime.opens.fail_open(*request_id, path, &recent) {
                    Ok(batches) => follow_up.extend(batches),
                    Err(error) => eprintln!("释放打开预留失败：{error}"),
                }
            }
            if !emitted {
                match runtime.opens.ack(&batch.target, batch.batch_id) {
                    Ok(next) => follow_up.extend(next),
                    Err(error) => eprintln!("回收打开批次失败：{error}"),
                }
            }
            follow_up
        });
        match follow_up {
            Ok(batches) => scheduled.extend(batches),
            Err(error) => {
                eprintln!("打开队列状态不可用：{error}");
                return;
            }
        }
    }
}

fn enqueue_open_paths(app: &tauri::AppHandle, paths: Vec<std::path::PathBuf>) {
    if paths.is_empty() {
        return;
    }
    let recent = recent_window_label(app);
    match with_runtime(app, |runtime| runtime.opens.enqueue(paths, &recent)) {
        Ok(batches) => dispatch_open_batches(app, batches),
        Err(error) => eprintln!("加入打开队列失败：{error}"),
    }
}

#[tauri::command]
fn open_requests_submit(
    app: tauri::AppHandle,
    paths: Vec<String>,
) -> Result<(), String> {
    enqueue_open_paths(&app, paths.into_iter().map(std::path::PathBuf::from).collect());
    Ok(())
}

#[tauri::command]
fn open_requests_ready(
    app: tauri::AppHandle,
    window: tauri::Window,
) -> Result<Option<open_request::OpenBatch>, String> {
    let batch = with_runtime(&app, |runtime| {
        if !runtime.windows.mark_ready(window.label()) {
            return Err(format!("窗口未注册：{}", window.label()));
        }
        Ok(runtime.opens.mark_ready(window.label()))
    })??;
    dispatch_open_batches(&app, batch.into_iter().collect());
    // 实际批次统一经已注册的 open-paths 监听发送，避免非 Unicode 路径进入 IPC。
    Ok(None)
}

#[tauri::command]
fn open_requests_ack(
    app: tauri::AppHandle,
    window: tauri::Window,
    batch_id: u64,
) -> Result<Option<open_request::OpenBatch>, String> {
    let next = with_runtime(&app, |runtime| runtime.opens.ack(window.label(), batch_id))??;
    dispatch_open_batches(&app, next.into_iter().collect());
    // 下一批经同一 open-paths 监听进入前端现有 Promise 链。
    Ok(None)
}

#[tauri::command]
fn open_requests_commit(
    app: tauri::AppHandle,
    window: tauri::Window,
    request_id: u64,
    path: String,
    session: u64,
    lease: u64,
) -> Result<(), String> {
    let batches = with_runtime(&app, |runtime| {
        let entry = runtime.docs.authorize(session, window.label(), lease, Access::Read)?;
        if entry.path.as_deref() != Some(std::path::Path::new(&path)) {
            return Err("打开提交路径不属于该文档会话".into());
        }
        runtime.opens.complete_open(
            request_id,
            std::path::Path::new(&path),
            session,
            window.label(),
        )
    })??;
    dispatch_open_batches(&app, batches);
    Ok(())
}

#[tauri::command]
fn open_requests_fail(
    app: tauri::AppHandle,
    request_id: u64,
    path: String,
) -> Result<(), String> {
    let recent = recent_window_label(&app);
    let batches = with_runtime(&app, |runtime| {
        runtime.opens.fail_open(request_id, std::path::Path::new(&path), &recent)
    })??;
    dispatch_open_batches(&app, batches);
    Ok(())
}

fn authorize_big(
    runtime: &SharedRuntime,
    owner: &str,
    session: u64,
    lease: u64,
    access: Access,
) -> Result<BigEntry, String> {
    let mut guard = runtime.lock().map_err(|e| e.to_string())?;
    guard.docs.authorize(session, owner, lease, access)
}

/// 两阶段授权：克隆句柄后等待文件锁，锁内再次校验 owner/lease/state。
fn with_big<T>(
    runtime: &SharedRuntime,
    window: &tauri::Window,
    session: u64,
    lease: u64,
    access: Access,
    f: impl FnOnce(&mut BigFile) -> Result<T, String>,
) -> Result<T, String> {
    let entry = authorize_big(runtime, window.label(), session, lease, access)?;
    let mut file = entry.file.lock().map_err(|e| e.to_string())?;
    authorize_big(runtime, window.label(), session, lease, access)?;
    file.refresh_indexed();
    f(&mut file)
}

/// 只有当前 owner/lease 仍能读文档时才发送定向事件。
fn emit_big_event<T: serde::Serialize + Clone>(
    app: &tauri::AppHandle,
    owner: &str,
    session: u64,
    lease: u64,
    event: &str,
    payload: T,
) {
    let runtime = app.state::<SharedRuntime>();
    if authorize_big(&runtime, owner, session, lease, Access::Read).is_ok() {
        let _ = app.emit_to(owner, event, payload);
    }
}

fn preauthorize_replacement(
    runtime: &SharedRuntime,
    owner: &str,
    session: Option<u64>,
    lease: Option<u64>,
) -> Result<(), String> {
    match (session, lease) {
        (Some(session), Some(lease)) => {
            authorize_big(runtime, owner, session, lease, Access::Write)?;
            Ok(())
        }
        (None, None) => Ok(()),
        _ => Err("replaceSession 与 replaceLease 必须同时提供".into()),
    }
}

fn register_new_document(
    runtime: &SharedRuntime,
    owner: &str,
    file: BigFile,
    path: Option<std::path::PathBuf>,
    replace_session: Option<u64>,
    replace_lease: Option<u64>,
) -> Result<BigSession, String> {
    let replacement = match (replace_session, replace_lease) {
        (Some(session), Some(lease)) => Some((session, lease)),
        (None, None) => None,
        _ => return Err("replaceSession 与 replaceLease 必须同时提供".into()),
    };
    let mut guard = runtime.lock().map_err(|e| e.to_string())?;
    let old = match replacement {
        Some((session, lease)) => Some((
            session,
            lease,
            guard.docs.authorize(session, owner, lease, Access::Write)?,
        )),
        None => None,
    };
    let registered = guard.docs.insert_new(file, owner, path);
    if let Some((session, lease, _)) = &old {
        guard.docs.close(*session, owner, *lease)?;
    }
    drop(guard);
    if let Some((_, _, old)) = old {
        old.bump_gate();
        old.load_cancel.store(true, Ordering::Relaxed);
        drop(old.file.lock());
    }
    Ok(registered)
}

/// 查询文件大小（字节），打开路径路由用：≥ 阈值走大文件模式。
#[tauri::command]
fn file_size(path: String) -> Result<u64, String> {
    let meta = fs::metadata(&path).map_err(|e| io_err(&path, &e))?;
    Ok(meta.len())
}

/// 打开大文件会话（mmap + 首屏前缀索引；全量索引转后台，UI 不冻结）。
///
/// 同步命令会占住主线程：慢盘上整文件索引要一两分钟，窗口直接"未响应"。
/// 这里只同步扫 1 MiB/1000 条记录（百毫秒级），其余由引擎后台线程继续；
/// 走 spawn_blocking 连前缀扫描也不占 UI 线程。
#[tauri::command]
async fn big_open(
    window: tauri::Window,
    path: String,
    headerless: Option<bool>,
    replace_session: Option<u64>,
    replace_lease: Option<u64>,
    state: tauri::State<'_, SharedRuntime>,
) -> Result<BigSession, String> {
    let runtime = state.inner().clone();
    let owner = window.label().to_string();
    preauthorize_replacement(&runtime, &owner, replace_session, replace_lease)?;
    let headerless = headerless.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
        let file = BigFile::open_progressive_with_mode(
            std::path::Path::new(&path),
            headerless,
        )?;
        register_new_document(
            &runtime,
            &owner,
            file,
            Some(std::path::PathBuf::from(path)),
            replace_session,
            replace_lease,
        )
    })
    .await
    .map_err(|e| format!("打开任务被取消：{e}"))?
}

/// 新建内存表格会话（统一大引擎：不写盘、无路径，另存为后自动获得路径）。
///
/// text 为前端序列化好的 CSV 全文；headerless 决定首行是否作列名。
/// 新建数据量小、同步解析毫秒级，仍走 spawn_blocking 与 big_open 形态一致。
#[tauri::command]
async fn big_new(
    window: tauri::Window,
    text: String,
    headerless: Option<bool>,
    replace_session: Option<u64>,
    replace_lease: Option<u64>,
    state: tauri::State<'_, SharedRuntime>,
) -> Result<BigSession, String> {
    let runtime = state.inner().clone();
    let owner = window.label().to_string();
    preauthorize_replacement(&runtime, &owner, replace_session, replace_lease)?;
    let headerless = headerless.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
        let file = BigFile::from_bytes(text.into_bytes(), headerless)?;
        register_new_document(&runtime, &owner, file, None, replace_session, replace_lease)
    })
    .await
    .map_err(|e| format!("新建任务被取消：{e}"))?
}

/// 关闭大文件会话。
#[tauri::command]
fn big_close(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
) -> Result<(), String> {
    let entry = {
        let mut guard = state.lock().map_err(|e| e.to_string())?;
        let entry = guard.docs.authorize(session, window.label(), lease, Access::Read)?;
        guard.docs.close(session, window.label(), lease)?;
        entry
    };
    entry.bump_gate();
    entry.load_cancel.store(true, Ordering::Relaxed);
    drop(entry.file.lock());
    Ok(())
}

/// 排序模式内存估算：文件缓冲 + 排列 + 键 + 安全余量，并附系统可用内存。
///
/// # Parameters
/// - `session`: 大文件会话号。
/// - `levels`: 排序级别数（缺省 1）。
#[tauri::command]
fn big_sort_estimate(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    levels: Option<u32>,
) -> Result<SortEstimate, String> {
    with_big(&state, &window, session, lease, Access::Read, |bf| {
        Ok(bf.sort_estimate(levels.unwrap_or(1)))
    })
}

/// 排序模式：把文件分块载入私有内存缓冲（非阻塞，进度经
/// "big-sort-progress" 事件推送，可用 big_sort_cancel 随时取消）。
///
/// 载入期间持文件锁；取消命令只置原子标志、不取锁，因此随时可停。
/// 取消不改文件、不标脏，前端继续用原 mmap 视图。
#[tauri::command]
async fn big_sort_load(
    window: tauri::Window,
    state: tauri::State<'_, SharedRuntime>,
    session: u64,
    lease: u64,
) -> Result<bool, String> {
    let runtime = state.inner().clone();
    let owner = window.label().to_string();
    let entry = authorize_big(&runtime, &owner, session, lease, Access::Write)?;
    entry.load_cancel.store(false, Ordering::Relaxed);
    entry.bump_gate();
    let cancel = entry.load_cancel.clone();
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (tx, rx) = std::sync::mpsc::channel::<(u64, u64)>();
        let emit_owner = owner.clone();
        let emitter = std::thread::spawn(move || {
            while let Ok((done, total)) = rx.recv() {
                emit_big_event(
                    &app,
                    &emit_owner,
                    session,
                    lease,
                    "big-sort-progress",
                    serde_json::json!({
                        "session": session,
                        "lease": lease,
                        "done": done,
                        "total": total,
                    }),
                );
            }
        });
        let result = (|| {
            let mut bf = entry.file.lock().map_err(|e| e.to_string())?;
            authorize_big(&runtime, &owner, session, lease, Access::Write)?;
            bf.refresh_indexed();
            bf.load_into_ram(&cancel, &mut move |done, total| {
                let _ = tx.send((done, total));
            })
        })();
        emitter.join().map_err(|e| format!("进度转发线程异常：{e:?}"))?;
        result
    })
    .await
    .map_err(|e| format!("载入任务被取消：{e}"))?
}

/// 取消正在进行的排序模式载入（幂等；未载入时调用无副作用）。
#[tauri::command]
fn big_sort_cancel(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
) -> Result<(), String> {
    authorize_big(&state, window.label(), session, lease, Access::Cancel)?
        .load_cancel
        .store(true, Ordering::Relaxed);
    Ok(())
}

/// 应用排序（需已载入内存）。排序计算在阻塞线程执行，完成后返回最新 meta。
///
/// # Parameters
/// - `session`: 大文件会话号。
/// - `levels`: 排序级别（列号按当前视图）。
#[tauri::command]
async fn big_sort_apply(
    window: tauri::Window,
    state: tauri::State<'_, SharedRuntime>,
    session: u64,
    lease: u64,
    levels: Vec<SortLevel>,
    approval: Approval,
) -> Result<EditOutcome, String> {
    let runtime = state.inner().clone();
    let owner = window.label().to_string();
    let entry = authorize_big(&runtime, &owner, session, lease, Access::Write)?;
    entry.bump_gate();
    tauri::async_runtime::spawn_blocking(move || {
        let mut bf = entry.file.lock().map_err(|e| e.to_string())?;
        authorize_big(&runtime, &owner, session, lease, Access::Write)?;
        bf.refresh_indexed();
        bf.sort_by_levels(&levels, approval)
    })
    .await
    .map_err(|e| format!("排序任务被取消：{e}"))?
}

/// 退出排序视图；`release_ram` 为 true（缺省）时同时释放私有内存缓冲。
#[tauri::command]
async fn big_sort_exit(
    window: tauri::Window,
    state: tauri::State<'_, SharedRuntime>,
    session: u64,
    lease: u64,
    release_ram: Option<bool>,
    approval: Approval,
) -> Result<EditOutcome, String> {
    let runtime = state.inner().clone();
    let owner = window.label().to_string();
    let entry = authorize_big(&runtime, &owner, session, lease, Access::Write)?;
    let release = release_ram.unwrap_or(true);
    tauri::async_runtime::spawn_blocking(move || {
        let mut bf = entry.file.lock().map_err(|e| e.to_string())?;
        authorize_big(&runtime, &owner, session, lease, Access::Write)?;
        bf.exit_sort_mode(release, approval)
    })
    .await
    .map_err(|e| format!("退出排序任务被取消：{e}"))?
}

/// 元数据（行/列/表头/编码/待执行列操作/撤销栈状态）。
#[tauri::command]
fn big_meta(window: tauri::Window, state: tauri::State<SharedRuntime>, session: u64, lease: u64,) -> Result<BigMeta, String> {
    with_big(&state, &window, session, lease, Access::Read, |bf| Ok(bf.meta()))
}

/// 外部程序是否改动过文件（focus 时前端拉取）。
#[tauri::command]
fn big_stale(window: tauri::Window, state: tauri::State<SharedRuntime>, session: u64, lease: u64,) -> Result<bool, String> {
    with_big(&state, &window, session, lease, Access::Read, |bf| Ok(bf.stale()))
}

/// 窗口取行（含覆盖层/插入块/删除位图的视图行）。
#[tauri::command]
fn big_rows(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    from: u64,
    count: u32,
) -> Result<Vec<Vec<String>>, String> {
    with_big(&state, &window, session, lease, Access::Read, |bf| Ok(bf.rows(from, count)))
}

/// Excel 风格纵向数据边缘查找（Ctrl+↑/↓）。
///
/// # Parameters
/// - `from`: 当前视图行号（筛选视图为命中序号）。
/// - `col`: 视图列号。
/// - `down`: true=向下；false=向上。
/// - `filtered`: 是否按当前筛选命中集导航。
///
/// # Returns
/// 目标视图行号；空表/无命中时为 None。
#[tauri::command]
fn big_find_edge(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    from: u64,
    col: u32,
    down: bool,
    filtered: bool,
) -> Result<Option<u64>, String> {
    with_big(&state, &window, session, lease, Access::Read, |bf| {
        bf.find_vertical_edge(from, col, down, filtered)
    })
}

/// 编辑单元格（源行进覆盖层，插入行直写）。
#[tauri::command]
fn big_set_cell(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    vrow: u64,
    col: u32,
    value: String,
    approval: Approval,
) -> Result<EditOutcome, String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| bf.set_cell(vrow, col, value, approval))
}

/// 将一组单元格修改作为一次用户操作写入。
#[tauri::command]
fn big_set_cells(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    cells: Vec<(u64, u32, String)>,
    approval: Approval,
) -> Result<EditOutcome, String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| bf.set_cells(cells, approval))
}

/// 按结构化单元格、行、列或矩形目标清除内容。
#[tauri::command]
fn big_clear(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    target: ClearTarget,
    approval: Approval,
) -> Result<EditOutcome, String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| bf.clear(target, approval))
}

/// 粘贴不规则行块；既有格与自动扩行合并为一次用户操作。
#[tauri::command]
fn big_paste(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    vrow: u64,
    col: u32,
    rows: Vec<Vec<String>>,
    approval: Approval,
) -> Result<EditOutcome, String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| bf.paste_rows(vrow, col, rows, approval))
}

/// 在 vpos 处插入行（rows 为 null 时插 count 行空行）。
#[tauri::command]
fn big_insert_rows(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    vpos: u64,
    rows: Option<Vec<Vec<String>>>,
    count: u64,
    approval: Approval,
) -> Result<EditOutcome, String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| bf.insert_rows(vpos, rows, count, approval))
}

/// 删除一组 vrow（升序传入），返回删除行数。
#[tauri::command]
fn big_remove_rows(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    vrows: Vec<u64>,
    approval: Approval,
) -> Result<EditOutcome, String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| bf.remove_rows(vrows, approval))
}

/// 设置单个文档的撤销/重做历史字节预算，并返回最新元数据。
#[tauri::command]
fn big_set_undo_budget(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    budget_bytes: u64,
) -> Result<BigMeta, String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| {
        bf.set_history_budget(budget_bytes)?;
        Ok(bf.meta())
    })
}

#[tauri::command]
fn big_undo(window: tauri::Window, state: tauri::State<SharedRuntime>, session: u64, lease: u64,) -> Result<bool, String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| Ok(bf.undo()))
}

#[tauri::command]
fn big_redo(window: tauri::Window, state: tauri::State<SharedRuntime>, session: u64, lease: u64,) -> Result<bool, String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| Ok(bf.redo()))
}

/// 执行一次筛选（rayon 并行扫描），返回命中数；被更新请求打断时返回 null。
///
/// 长扫描走 spawn_blocking，避免占用异步运行时 worker；新请求通过
/// `FilterGen` 自增让旧扫描在下一个检查点退出，不覆盖上一次的结果。
#[tauri::command]
async fn big_filter(
    window: tauri::Window,
    state: tauri::State<'_, SharedRuntime>,
    session: u64,
    lease: u64,
    spec: FilterSpec,
) -> Result<Option<u32>, String> {
    let runtime = state.inner().clone();
    let owner = window.label().to_string();
    let entry = authorize_big(&runtime, &owner, session, lease, Access::Write)?;
    let gen = entry.bump_gate();
    let gate = entry.gate.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut bf = entry.file.lock().map_err(|e| e.to_string())?;
        authorize_big(&runtime, &owner, session, lease, Access::Write)?;
        bf.run_filter_cancelable(&spec, &gate, gen)
    })
    .await
    .map_err(|e| format!("筛选任务被取消：{e}"))?
}

#[tauri::command]
fn big_filter_count(window: tauri::Window, state: tauri::State<SharedRuntime>, session: u64, lease: u64,) -> Result<u32, String> {
    with_big(&state, &window, session, lease, Access::Read, |bf| Ok(if bf.has_filter() { bf.filter_len() } else { 0 }))
}

#[tauri::command]
fn big_filter_page(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    from: u32,
    count: u32,
) -> Result<FilterPage, String> {
    with_big(&state, &window, session, lease, Access::Read, |bf| Ok(bf.filter_page(from, count)))
}

/// 命中序号 → 清筛后全量视图行号（取消筛选锚定查询；无筛选/越界返回 None）。
#[tauri::command]
fn big_filter_hit_view_row(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    idx: u32,
) -> Result<Option<u32>, String> {
    with_big(&state, &window, session, lease, Access::Read, |bf| Ok(bf.filter_hit_view_row(idx)))
}

#[tauri::command]
fn big_clear_filter(window: tauri::Window, state: tauri::State<SharedRuntime>, session: u64, lease: u64,) -> Result<(), String> {
    // 清空是全量视图：还在跑的筛选结果不应再落库
    authorize_big(&state, window.label(), session, lease, Access::Write)?.bump_gate();
    with_big(&state, &window, session, lease, Access::Write, |bf| {
        bf.clear_filter();
        Ok(())
    })
}

/// 切换「首行非列名」并返回统一 EditOutcome。
///
/// 切换记录 HeaderMode 历史且不改文件；实际变化后清空筛选结果。
/// 先 bump 代数让在跑的筛选扫描退出，取消和空改动不写数据。
#[tauri::command]
fn big_set_headerless(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    on: bool,
    approval: Approval,
) -> Result<EditOutcome, String> {
    authorize_big(&state, window.label(), session, lease, Access::Write)?.bump_gate();
    with_big(&state, &window, session, lease, Access::Write, |bf| bf.set_headerless(on, approval))
}

/// 完整矩形复制：小结果返回 Text，大结果返回应用内 token。
#[tauri::command]
fn big_copy(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    clipboard: tauri::State<ClipboardStore>,
    session: u64,
    lease: u64,
    r0: u64,
    r1: u64,
    c0: u32,
    c1: u32,
) -> Result<CopyOutcome, String> {
    with_big(&state, &window, session, lease, Access::Read, |bf| bf.copy_tsv(r0, r1, c0, c1, clipboard.inner()))
}

#[tauri::command]
fn big_clipboard_meta(
    window: tauri::Window,
    runtime: tauri::State<SharedRuntime>,
    clipboard: tauri::State<ClipboardStore>,
    session: u64,
    lease: u64,
) -> Result<Option<ClipboardMeta>, String> {
    with_big(&runtime, &window, session, lease, Access::Read, |_| {
        Ok(clipboard.current_meta())
    })
}

#[tauri::command]
fn big_clipboard_clear(
    window: tauri::Window,
    runtime: tauri::State<SharedRuntime>,
    clipboard: tauri::State<ClipboardStore>,
    session: u64,
    lease: u64,
) -> Result<(), String> {
    with_big(&runtime, &window, session, lease, Access::Read, |_| {
        clipboard.clear_current();
        Ok(())
    })
}

/// 保存（流式重写 + 原子换名），进度经 "big-progress" 事件推送。
///
/// 长操作走 spawn_blocking（独立 OS 线程，不占 tokio worker），进度经
/// 专用转发线程 emit——重写线程全程不触碰 WebView2 COM。v4 采样实证：
/// 重写期间主线程被挂起（Wait/WrSuspended）、进度事件挤压到命令结束才
/// 到达渲染进程，用户看到"进度条卡住 + 窗口未响应"。
#[tauri::command]
async fn big_save(
    window: tauri::Window,
    state: tauri::State<'_, SharedRuntime>,
    session: u64,
    lease: u64,
    approval: crate::history::Approval,
) -> Result<engine::SaveOutcome, String> {
    run_big_save(window, state, session, lease, None, approval).await
}

/// 另存为：把当前视图流式写到 `path`，原文件保持不变；成功后会话切到新路径。
/// 进度与线程模型同 `big_save`。
#[tauri::command]
async fn big_save_as(
    window: tauri::Window,
    state: tauri::State<'_, SharedRuntime>,
    session: u64,
    lease: u64,
    path: String,
    approval: crate::history::Approval,
) -> Result<engine::SaveOutcome, String> {
    run_big_save(window, state, session, lease, Some(path), approval).await
}

/// `big_save`/`big_save_as` 共用实现。`target=None` 写回当前文件。
async fn run_big_save(
    window: tauri::Window,
    state: tauri::State<'_, SharedRuntime>,
    session: u64,
    lease: u64,
    target: Option<String>,
    approval: crate::history::Approval,
) -> Result<engine::SaveOutcome, String> {
    let runtime = state.inner().clone();
    let owner = window.label().to_string();
    let entry = authorize_big(&runtime, &owner, session, lease, Access::Write)?;
    entry.bump_gate();
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (tx, rx) = std::sync::mpsc::channel::<(u64, u64, u8, u64)>();
        let emit_owner = owner.clone();
        let emitter = std::thread::spawn(move || {
            while let Ok((done, total, phase, rows)) = rx.recv() {
                emit_big_event(
                    &app,
                    &emit_owner,
                    session,
                    lease,
                    "big-progress",
                    serde_json::json!({
                        "session": session,
                        "lease": lease,
                        "done": done,
                        "total": total,
                        "phase": phase,
                        "rows": rows,
                    }),
                );
            }
        });
        let result = (|| {
            let mut bf = entry.file.lock().map_err(|e| e.to_string())?;
            authorize_big(&runtime, &owner, session, lease, Access::Write)?;
            bf.refresh_indexed();
            let mut on_progress = move |done: u64, total: u64, phase: u8, rows: u64| {
                let _ = tx.send((done, total, phase, rows));
            };
            match target {
                Some(path) => bf.save_as(
                    std::path::Path::new(&path),
                    approval,
                    &mut on_progress,
                ),
                None => bf.save(approval, &mut on_progress),
            }
        })();
        emitter.join().map_err(|e| format!("进度转发线程异常：{e:?}"))?;
        result
    })
    .await
    .map_err(|e| format!("保存任务被取消：{e}"))?
}

// ---- 列操作：pending 预览 → 执行物理重写 ----

#[tauri::command]
fn big_col_push(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    op: ColOp,
) -> Result<(), String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| bf.push_col_op(op))
}

#[tauri::command]
fn big_col_pop(window: tauri::Window, state: tauri::State<SharedRuntime>, session: u64, lease: u64,) -> Result<(), String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| {
        bf.pop_col_op();
        Ok(())
    })
}

#[tauri::command]
fn big_col_clear(window: tauri::Window, state: tauri::State<SharedRuntime>, session: u64, lease: u64,) -> Result<(), String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| {
        bf.clear_col_ops();
        Ok(())
    })
}

/// 执行待处理列操作；只提交可撤销 Schema，不做物理重写。
#[tauri::command]
fn big_col_apply(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    approval: Approval,
) -> Result<EditOutcome, String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| bf.apply_col_ops(approval))
}

/// 截屏诊断模式：启动参数 `--park <mode>` 时返回 mode 字符串，否则 None。
///
/// 窗口停在指定渲染场景待命（进程不退出），供外部截屏工具读像素、
/// 验证 WebView2 真实绘制结果（DOM 探针看不到绘制级缺陷）。
#[tauri::command]
fn is_park(app: tauri::AppHandle, window: tauri::Window) -> Option<String> {
    if !is_diagnostic_main(&app, window.label()) {
        return None;
    }
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--park" {
            return args.next();
        }
    }
    None
}

fn emit_transfer_update(app: &tauri::AppHandle, event: &str, update: &TransferUpdate, targets: &[String]) {
    for target in targets {
        if app.get_webview_window(target).is_some() {
            let _ = app.emit_to(target, event, update.clone());
        }
    }
}

fn cancel_in_runtime(
    runtime: &mut RuntimeState,
    ticket: u64,
    actor: &str,
) -> Result<(TransferUpdate, TransferRoute, Vec<open_request::OpenBatch>), String> {
    let route = runtime.docs.route(ticket)
        .ok_or_else(|| "迁移 ticket 不存在或已结束".to_string())?;
    let (session, owner, lease) = runtime.docs.cancel(ticket, actor)?;
    runtime.opens.set_session_owner(session, &owner);
    let batches = runtime.opens.set_transferring(session, false);
    Ok((TransferUpdate { ticket, session, owner, lease }, route, batches))
}

fn reap_expired_transfers(app: &tauri::AppHandle) {
    let result = with_runtime(app, |runtime| {
        let now = std::time::Instant::now();
        let routes = runtime.docs.expired_routes(now);
        let expired = runtime.docs.reap_expired(now);
        debug_assert_eq!(routes.len(), expired.len());
        let mut batches = Vec::new();
        let mut updates = Vec::with_capacity(expired.len());
        for (route, (session, owner, lease)) in routes.into_iter().zip(expired) {
            runtime.opens.set_session_owner(session, &owner);
            batches.extend(runtime.opens.set_transferring(session, false));
            updates.push((TransferUpdate {
                ticket: route.ticket,
                session,
                owner,
                lease,
            }, route));
        }
        (updates, batches)
    });
    let Ok((updates, batches)) = result else {
        if let Err(error) = result {
            eprintln!("回收到期迁移失败：{error}");
        }
        return;
    };
    dispatch_open_batches(app, batches);
    for (update, route) in updates {
        emit_transfer_update(app, "transfer-cancelled", &update, &[route.source, route.target]);
    }
}

#[tauri::command]
fn seal_document(
    app: tauri::AppHandle,
    window: tauri::Window,
    session: u64,
    lease: u64,
    target: String,
    side: String,
    index: usize,
    snapshot: serde_json::Value,
) -> Result<u64, String> {
    reap_expired_transfers(&app);
    let source = window.label().to_string();
    let ticket = with_runtime(&app, |runtime| -> Result<u64, String> {
        if !runtime.windows.is_ready(&target) {
            return Err(format!("目标窗口未 ready：{target}"));
        }
        let ticket = runtime.docs.seal(
            session,
            &source,
            lease,
            &target,
            &side,
            index,
            snapshot,
        )?;
        let _ = runtime.opens.set_transferring(session, true);
        Ok(ticket)
    })??;
    if let Err(error) = app.emit_to(
        &target,
        "transfer-request",
        serde_json::json!({ "ticket": ticket, "source": source }),
    ) {
        if let Ok(Ok((update, route, batches))) = with_runtime(&app, |runtime| {
            cancel_in_runtime(runtime, ticket, &source)
        }) {
            dispatch_open_batches(&app, batches);
            emit_transfer_update(
                &app,
                "transfer-cancelled",
                &update,
                &[route.source, route.target],
            );
        }
        return Err(format!("发送迁移请求失败，已回滚：{error}"));
    }
    Ok(ticket)
}

#[tauri::command]
fn accept_transfer(
    app: tauri::AppHandle,
    window: tauri::Window,
    ticket: u64,
) -> Result<AdoptedSession, String> {
    reap_expired_transfers(&app);
    let target = window.label().to_string();
    with_runtime(&app, |runtime| {
        let adopted = runtime.docs.accept(ticket, &target)?;
        runtime.opens.set_session_owner(adopted.session, &target);
        Ok(adopted)
    })?
}

#[tauri::command]
fn transfer_adopted(
    app: tauri::AppHandle,
    window: tauri::Window,
    ticket: u64,
) -> Result<AdoptedNotice, String> {
    reap_expired_transfers(&app);
    let target = window.label().to_string();
    let (notice, route, batches) = with_runtime(&app, |runtime| -> Result<(AdoptedNotice, TransferRoute, Vec<open_request::OpenBatch>), String> {
        let route = runtime.docs.route(ticket)
            .ok_or_else(|| "迁移 ticket 不存在或已结束".to_string())?;
        let notice = runtime.docs.mark_adopted(ticket, &target)?;
        let batches = if notice.source_live {
            Vec::new()
        } else {
            runtime.opens.set_transferring(notice.session, false)
        };
        Ok((notice, route, batches))
    })??;
    if notice.source_live {
        if let Err(error) = app.emit_to(&route.source, "transfer-accepted", notice.clone()) {
            eprintln!("通知源窗口目标已 adopt 失败：{error}");
        }
    } else {
        dispatch_open_batches(&app, batches);
        let update = TransferUpdate {
            ticket,
            session: notice.session,
            owner: target,
            lease: notice.lease,
        };
        reveal_transfer_target(&app, &update.owner);
        emit_transfer_update(&app, "transfer-completed", &update, &[update.owner.clone()]);
    }
    Ok(notice)
}

fn reveal_transfer_target(app: &tauri::AppHandle, owner: &str) {
    if let Some(window) = app.get_webview_window(owner) {
        if let Err(error) = window.show().and_then(|_| window.set_focus()) {
            eprintln!("显示迁移目标窗口失败：{error}");
        }
    }
}

#[tauri::command]
fn complete_transfer(
    app: tauri::AppHandle,
    window: tauri::Window,
    ticket: u64,
) -> Result<TransferUpdate, String> {
    reap_expired_transfers(&app);
    let source = window.label().to_string();
    let (update, batches) = with_runtime(&app, |runtime| -> Result<(TransferUpdate, Vec<open_request::OpenBatch>), String> {
        let (session, owner, lease) = runtime.docs.complete(ticket, &source)?;
        runtime.opens.set_session_owner(session, &owner);
        let batches = runtime.opens.set_transferring(session, false);
        Ok((TransferUpdate { ticket, session, owner, lease }, batches))
    })??;
    dispatch_open_batches(&app, batches);
    reveal_transfer_target(&app, &update.owner);
    if let Err(error) = app.emit_to(&update.owner, "transfer-completed", update.clone()) {
        eprintln!("通知目标迁移完成失败：{error}");
    }
    Ok(update)
}

#[tauri::command]
fn cancel_transfer(
    app: tauri::AppHandle,
    window: tauri::Window,
    ticket: u64,
) -> Result<TransferUpdate, String> {
    reap_expired_transfers(&app);
    let actor = window.label().to_string();
    let (update, route, batches) = with_runtime(&app, |runtime| {
        cancel_in_runtime(runtime, ticket, &actor)
    })??;
    dispatch_open_batches(&app, batches);
    emit_transfer_update(&app, "transfer-cancelled", &update, &[route.source, route.target]);
    Ok(update)
}

fn handle_window_destroyed(app: &tauri::AppHandle, label: &str) {
    let mut live: HashSet<String> = app.webview_windows().keys()
        .filter(|candidate| candidate.as_str() != label)
        .cloned()
        .collect();
    let result = with_runtime(app, |runtime| {
        if runtime.drags.window_destroyed(label) {
            drag_preview::destroy(app);
        }
        if runtime.windows.role(label).is_none() {
            return (Vec::new(), Vec::new(), Vec::new());
        }
        live.retain(|candidate| runtime.windows.role(candidate).is_some());
        let fallback = runtime.windows.recent_ready()
            .filter(|candidate| live.contains(candidate));
        let outcomes = runtime.docs.destroy_window(label, &live, fallback.as_deref());
        let mut batches = Vec::new();
        let mut entries = Vec::new();
        for outcome in &outcomes {
            match outcome {
                DestroyOutcome::Cancelled { session, owner, .. }
                | DestroyOutcome::Completed { session, owner, .. }
                | DestroyOutcome::Recovered { session, owner, .. } => {
                    runtime.opens.set_session_owner(*session, owner);
                    batches.extend(runtime.opens.set_transferring(*session, false));
                }
                DestroyOutcome::Dropped { session, entry: Some(entry), .. } => {
                    runtime.opens.remove_session(*session);
                    entries.push(entry.clone());
                }
                _ => {}
            }
        }
        runtime.windows.unregister(label);
        let recent = runtime.windows.recent_ready()
            .or_else(|| runtime.windows.recent_registered())
            .unwrap_or_else(|| "main".to_string());
        batches.extend(runtime.opens.remove_window(label, &recent));
        (outcomes, batches, entries)
    });
    let Ok((outcomes, batches, entries)) = result else {
        if let Err(error) = result {
            eprintln!("恢复销毁窗口失败：{error}");
        }
        return;
    };
    for entry in entries {
        entry.bump_gate();
        entry.load_cancel.store(true, Ordering::Relaxed);
        drop(entry.file.lock());
    }
    dispatch_open_batches(app, batches);
    for outcome in outcomes {
        match outcome {
            DestroyOutcome::Cancelled { ticket, session, owner, lease } => {
                let update = TransferUpdate { ticket, session, owner, lease };
                emit_transfer_update(app, "transfer-cancelled", &update, &[update.owner.clone()]);
            }
            DestroyOutcome::Completed { ticket, session, owner, lease } => {
                let update = TransferUpdate { ticket, session, owner, lease };
                emit_transfer_update(app, "transfer-completed", &update, &[update.owner.clone()]);
            }
            DestroyOutcome::Recovered { owner, adopted, .. } => {
                reveal_transfer_target(app, &owner);
                if let Err(error) = app.emit_to(&owner, "document-recovered", adopted) {
                    eprintln!("发送销毁恢复文档失败：{error}");
                }
            }
            DestroyOutcome::Detached { .. } | DestroyOutcome::Dropped { .. } => {}
        }
    }
}

fn is_diagnostic_main(app: &tauri::AppHandle, label: &str) -> bool {
    if label != "main" {
        return false;
    }
    let state = app.state::<SharedRuntime>();
    state.lock().ok().is_some_and(|runtime| {
        runtime.windows.role(label) == Some(window_registry::WindowRole::Diagnostic)
    })
}

fn install_window_events(app: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    let app = app.clone();
    let label = window.label().to_string();
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            let _ = app.emit_to(
                label.as_str(),
                "close-request",
                serde_json::json!({ "label": label.as_str() }),
            );
        }
        tauri::WindowEvent::Focused(true) => {
            let state = app.state::<SharedRuntime>();
            if let Ok(mut runtime) = state.lock() {
                runtime.windows.touch(&label);
            };
        }
        tauri::WindowEvent::Destroyed => {
            handle_window_destroyed(&app, &label);
        }
        _ => {}
    });
}

fn discard_unready_window(app: &tauri::AppHandle, label: &str) {
    match with_runtime(app, |runtime| {
        runtime.windows.unregister(label);
        let recent = runtime.windows.recent_ready()
            .or_else(|| runtime.windows.recent_registered())
            .unwrap_or_else(|| "main".to_string());
        runtime.opens.remove_window(label, &recent)
    }) {
        Ok(batches) => dispatch_open_batches(app, batches),
        Err(error) => eprintln!("回收未就绪窗口失败：{error}"),
    }
}

fn authorize_tab_drag(
    app: &tauri::AppHandle,
    source: &str,
    drag_token: u64,
) -> Result<(), String> {
    with_runtime(app, |runtime| {
        let (session, lease) = runtime.drags.authorize(drag_token, source)?;
        runtime.docs.authorize(session, source, lease, Access::Write).map(|_| ())
    })??;
    Ok(())
}

fn source_physical_point(
    window: &tauri::WebviewWindow,
    client_x: f64,
    client_y: f64,
) -> Result<drag_registry::PhysicalPoint, String> {
    let inner = window.inner_position().map_err(|error| format!("读取源窗口位置失败：{error}"))?;
    let scale = window.scale_factor().map_err(|error| format!("读取源窗口缩放失败：{error}"))?;
    drag_registry::client_to_physical(
        drag_registry::PhysicalPoint { x: inner.x, y: inner.y },
        scale,
        drag_registry::ClientPoint { x: client_x, y: client_y },
    )
}

// 预览 WebView 构建需要主事件循环，命令必须离开主线程以避免自等待。
#[tauri::command(async)]
fn begin_tab_drag(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    session: u64,
    lease: u64,
    title: String,
    client_x: f64,
    client_y: f64,
) -> Result<u64, String> {
    let source = window.label().to_string();
    // 先验证坐标，避免登记 active drag 后才失败并留下无法取消的 token。
    let point = source_physical_point(&window, client_x, client_y)?;
    let token = with_runtime(&app, |runtime| {
        runtime.docs.authorize(session, &source, lease, Access::Write)?;
        runtime.drags.begin(session, &source, lease)
    })??;
    if let Err(error) = drag_preview::show_at(&app, &title, point) {
        eprintln!("{error}");
    }
    Ok(token)
}

fn native_drag_route(
    app: &tauri::AppHandle,
    source: &str,
    point: drag_registry::PhysicalPoint,
) -> Result<(
    drag_registry::DropRoute,
    Vec<(tauri::WebviewWindow, drag_registry::WindowRect)>,
), String> {
    let recent = recent_ready_window_label(app);
    let mut candidates = app.webview_windows().into_values().collect::<Vec<_>>();
    candidates.sort_by_key(|window| if Some(window.label()) == recent.as_deref() { 0 } else { 1 });
    let mut frames = Vec::new();
    for window in candidates {
        let label = window.label().to_string();
        let eligible = with_runtime(app, |runtime| {
            runtime.windows.role(&label).is_some() && runtime.windows.is_ready(&label)
        }).unwrap_or(false);
        if !eligible
            || !window.is_visible().unwrap_or(false)
            || window.is_minimized().unwrap_or(true)
        {
            continue;
        }
        let (Ok(origin), Ok(size)) = (window.outer_position(), window.outer_size()) else {
            continue;
        };
        frames.push((window, drag_registry::WindowRect {
            label,
            x: origin.x,
            y: origin.y,
            w: size.width,
            h: size.height,
        }));
    }
    let rects = frames.iter().map(|(_, rect)| rect.clone()).collect::<Vec<_>>();
    Ok((drag_registry::route_drop(&rects, source, point), frames))
}

fn drag_css_point(
    frames: &[(tauri::WebviewWindow, drag_registry::WindowRect)],
    label: &str,
    point: drag_registry::PhysicalPoint,
) -> Result<(f64, f64), String> {
    let (window, _) = frames.iter().find(|(_, rect)| rect.label == label)
        .ok_or_else(|| format!("拖放目标窗口已消失：{label}"))?;
    let inner = window.inner_position().map_err(|error| error.to_string())?;
    let scale = window.scale_factor().map_err(|error| error.to_string())?;
    if !scale.is_finite() || scale <= 0.0 {
        return Err("目标窗口 scale factor 无效".into());
    }
    Ok((
        f64::from(point.x - inner.x) / scale,
        f64::from(point.y - inner.y) / scale,
    ))
}

fn clear_drag_indicator(app: &tauri::AppHandle, label: &str, drag_token: u64) {
    if label.is_empty() {
        return;
    }
    if let Err(error) = app.emit_to(
        label,
        "tab-drag-clear",
        serde_json::json!({ "dragToken": drag_token }),
    ) {
        eprintln!("清理目标拖放指示器失败（{label}）：{error}");
    }
}

fn update_drag_hover(
    app: &tauri::AppHandle,
    source: &str,
    drag_token: u64,
    target: Option<String>,
) -> Result<(), String> {
    let previous = with_runtime(app, |runtime| {
        runtime.drags.set_hover(drag_token, source, target)
    }).and_then(|result| result)?;
    if let Some(label) = previous {
        clear_drag_indicator(app, &label, drag_token);
    }
    Ok(())
}

fn finish_tab_drag(app: &tauri::AppHandle, source: &str, drag_token: u64) {
    match with_runtime(app, |runtime| {
        let previous = runtime.drags.finish(drag_token, source)?;
        // 在运行态锁内回收，避免下一次 begin 与旧清理竞态。
        drag_preview::destroy(app);
        Ok::<Option<String>, String>(previous)
    }).and_then(|result| result) {
        Ok(previous) => {
            if let Some(label) = previous {
                clear_drag_indicator(app, &label, drag_token);
            }
        }
        Err(error) => eprintln!("结束标签拖放失败：{error}"),
    }
}

#[tauri::command]
fn tab_drag_hover(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    drag_token: u64,
    client_x: f64,
    client_y: f64,
) -> Result<drag_registry::DragHover, String> {
    let source = window.label().to_string();
    authorize_tab_drag(&app, &source, drag_token)?;
    let point = source_physical_point(&window, client_x, client_y)?;
    let (route, frames) = native_drag_route(&app, &source, point)?;
    drag_preview::follow(&app, point);
    match route {
        drag_registry::DropRoute::SameWindow { .. } => {
            update_drag_hover(&app, &source, drag_token, None)?;
            match drag_css_point(&frames, &source, point) {
                Ok((x, y)) => Ok(drag_registry::DragHover::SameWindow { x, y }),
                Err(_) => Ok(drag_registry::DragHover::Invalid),
            }
        }
        drag_registry::DropRoute::AskTarget { label, .. } => {
            let Ok((x, y)) = drag_css_point(&frames, &label, point) else {
                update_drag_hover(&app, &source, drag_token, None)?;
                return Ok(drag_registry::DragHover::Invalid);
            };
            update_drag_hover(&app, &source, drag_token, Some(label.clone()))?;
            if let Err(error) = app.emit_to(
                &label,
                "tab-drag-hover",
                serde_json::json!({ "dragToken": drag_token, "x": x, "y": y }),
            ) {
                eprintln!("投递目标拖放 hover 失败（{label}）：{error}");
                update_drag_hover(&app, &source, drag_token, None)?;
                return Ok(drag_registry::DragHover::Invalid);
            }
            Ok(drag_registry::DragHover::Existing)
        }
        drag_registry::DropRoute::NewWindow { .. } => {
            update_drag_hover(&app, &source, drag_token, None)?;
            Ok(drag_registry::DragHover::NewWindow)
        }
    }
}

#[tauri::command]
async fn drop_tab_drag(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    drag_token: u64,
    client_x: f64,
    client_y: f64,
) -> Result<drag_registry::DropOutcome, String> {
    let source = window.label().to_string();
    authorize_tab_drag(&app, &source, drag_token)?;
    let point = source_physical_point(&window, client_x, client_y)?;
    let (route, frames) = native_drag_route(&app, &source, point)?;
    match route {
        drag_registry::DropRoute::SameWindow { .. } => {
            let outcome = match drag_css_point(&frames, &source, point) {
                Ok((x, y)) => drag_registry::DropOutcome::SameWindow { x, y },
                Err(_) => drag_registry::DropOutcome::Invalid,
            };
            finish_tab_drag(&app, &source, drag_token);
            Ok(outcome)
        }
        drag_registry::DropRoute::NewWindow { x, y } => {
            finish_tab_drag(&app, &source, drag_token);
            Ok(drag_registry::DropOutcome::NewWindow { x, y })
        }
        drag_registry::DropRoute::AskTarget { label, .. } => {
            let Ok((x, y)) = drag_css_point(&frames, &label, point) else {
                finish_tab_drag(&app, &source, drag_token);
                return Ok(drag_registry::DropOutcome::Invalid);
            };
            let (drop_id, receiver) = with_runtime(&app, |runtime| {
                runtime.drags.ask_target(drag_token, &source, &label)
            }).and_then(|result| result)?;
            if let Err(error) = update_drag_hover(
                &app, &source, drag_token, Some(label.clone()),
            ) {
                let _ = with_runtime(&app, |runtime| runtime.drags.abort_drop(drop_id));
                finish_tab_drag(&app, &source, drag_token);
                return Err(error);
            }
            if let Err(error) = app.emit_to(
                &label,
                "tab-drag-drop",
                serde_json::json!({
                    "dropId": drop_id, "dragToken": drag_token, "x": x, "y": y,
                }),
            ) {
                eprintln!("投递目标拖放 drop 失败（{label}）：{error}");
                let _ = with_runtime(&app, |runtime| runtime.drags.abort_drop(drop_id));
                finish_tab_drag(&app, &source, drag_token);
                return Ok(drag_registry::DropOutcome::Invalid);
            }
            let outcome = match tokio::time::timeout(
                std::time::Duration::from_secs(2), receiver,
            ).await {
                Ok(Ok(Some(target))) => {
                    if authorize_tab_drag(&app, &source, drag_token).is_ok() {
                        drag_registry::DropOutcome::Existing {
                            target: label,
                            side: target.side,
                            index: target.index,
                        }
                    } else {
                        drag_registry::DropOutcome::Invalid
                    }
                }
                Ok(Ok(None)) | Ok(Err(_)) => drag_registry::DropOutcome::Invalid,
                Err(_) => {
                    let _ = with_runtime(&app, |runtime| runtime.drags.abort_drop(drop_id));
                    drag_registry::DropOutcome::Invalid
                }
            };
            finish_tab_drag(&app, &source, drag_token);
            Ok(outcome)
        }
    }
}

#[tauri::command]
fn tab_drag_drop_result(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    drop_id: u64,
    side: Option<String>,
    index: Option<usize>,
) -> Result<(), String> {
    with_runtime(&app, |runtime| {
        runtime.drags.drop_result(drop_id, window.label(), side.as_deref(), index)
    })??;
    Ok(())
}

#[tauri::command]
fn cancel_tab_drag(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    drag_token: u64,
) -> Result<(), String> {
    let previous = with_runtime(&app, |runtime| {
        let previous = runtime.drags.cancel(drag_token, window.label())?;
        drag_preview::destroy(&app);
        Ok::<Option<String>, String>(previous)
    }).and_then(|result| result)?;
    if let Some(label) = previous {
        clear_drag_indicator(&app, &label, drag_token);
    }
    Ok(())
}

#[tauri::command]
async fn create_document_window(
    app: tauri::AppHandle,
    position: Option<drag_registry::PhysicalPoint>,
) -> Result<String, String> {
    let (label, receiver) = {
        let state = app.state::<SharedRuntime>();
        let mut runtime = state.lock().map_err(|e| e.to_string())?;
        let label = runtime.windows.allocate_document_label();
        runtime.windows.register(&label, window_registry::WindowRole::Document);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        runtime.windows.wait_ready(&label, sender);
        (label, receiver)
    };
    let window = match tauri::WebviewWindowBuilder::new(
        &app,
        &label,
        tauri::WebviewUrl::App("index.html".into()),
    )
    .title("simpleTable")
    .inner_size(1100.0, 720.0)
    .visible(false)
    .background_throttling(tauri::utils::config::BackgroundThrottlingPolicy::Disabled)
    .build() {
        Ok(window) => window,
        Err(error) => {
            discard_unready_window(&app, &label);
            return Err(format!("创建文档窗口失败：{error}"));
        }
    };
    if let Some(point) = position {
        if let Err(error) = window.set_position(tauri::PhysicalPosition::new(point.x, point.y)) {
            discard_unready_window(&app, &label);
            let _ = window.destroy();
            return Err(format!("设置文档窗口落点失败：{error}"));
        }
    }
    install_window_events(&app, &window);
    match tokio::time::timeout(std::time::Duration::from_secs(10), receiver).await {
        Ok(Ok(())) => Ok(label),
        Ok(Err(_)) => {
            discard_unready_window(&app, &label);
            let _ = window.destroy();
            Err("文档窗口 ready 通道已取消".into())
        }
        Err(_) => {
            discard_unready_window(&app, &label);
            let _ = window.destroy();
            Err("等待文档窗口 ready 超时（10 秒）".into())
        }
    }
}

fn close_window_internal(
    app: &tauri::AppHandle,
    window: &tauri::WebviewWindow,
    require_empty: bool,
) -> Result<(), String> {
    let label = window.label().to_string();
    let (entries, batches, cancellations, last) = {
        let state = app.state::<SharedRuntime>();
        let mut runtime = state.lock().map_err(|e| e.to_string())?;
        if runtime.drags.window_destroyed(&label) {
            drag_preview::destroy(app);
        }
        if runtime.docs.has_outgoing(&label) {
            return Err("窗口存在正在迁出的文档，不能关闭".into());
        }
        if require_empty && !runtime.docs.owner_is_empty(&label) {
            return Err("窗口仍拥有文档，不能按空窗口关闭".into());
        }
        runtime.windows.begin_close(&label)?;
        let cancellations = match runtime.docs.cancel_incoming(&label) {
            Ok(updates) => updates,
            Err(error) => {
                runtime.windows.cancel_close(&label);
                return Err(error);
            }
        };
        let mut batches = Vec::new();
        for update in &cancellations {
            runtime.opens.set_session_owner(update.session, &update.owner);
            batches.extend(runtime.opens.set_transferring(update.session, false));
        }
        let entries = match runtime.docs.close_owner(&label) {
            Ok(entries) => entries,
            Err(error) => {
                runtime.windows.cancel_close(&label);
                return Err(error);
            }
        };
        for (session, _) in &entries {
            runtime.opens.remove_session(*session);
        }
        runtime.windows.unregister(&label);
        let recent = runtime.windows.recent_ready()
            .or_else(|| runtime.windows.recent_registered())
            .unwrap_or_else(|| "main".to_string());
        batches.extend(runtime.opens.remove_window(&label, &recent));
        let last = runtime.windows.is_empty();
        (entries, batches, cancellations, last)
    };
    for (_, entry) in entries {
        entry.bump_gate();
        entry.load_cancel.store(true, Ordering::Relaxed);
        drop(entry.file.lock());
    }
    for update in &cancellations {
        emit_transfer_update(
            app,
            "transfer-cancelled",
            update,
            &[update.owner.clone(), label.clone()],
        );
    }
    dispatch_open_batches(app, batches);
    let destroyed = window.destroy().map_err(|e| e.to_string());
    if last {
        app.exit(0);
    }
    destroyed
}

#[tauri::command]
fn close_window(app: tauri::AppHandle, window: tauri::WebviewWindow) -> Result<(), String> {
    close_window_internal(&app, &window, false)
}

#[tauri::command]
fn close_empty_window(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    target_label: Option<String>,
) -> Result<(), String> {
    let target = if let Some(label) = target_label {
        let is_document = with_runtime(&app, |runtime| {
            matches!(runtime.windows.role(&label), Some(window_registry::WindowRole::Document))
        })?;
        if !is_document {
            return Err("只能按标签回收空文档窗口".into());
        }
        app.get_webview_window(&label).ok_or_else(|| format!("目标窗口不存在：{label}"))?
    } else {
        window
    };
    close_window_internal(&app, &target, true)
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
    let builder = tauri::Builder::default()
        .manage(SharedRuntime::default());
    #[cfg(windows)]
    let builder = if diagnostic_mode() {
        builder
    } else {
        builder.plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
            let paths = open_request::normalize_args(
                args.into_iter().map(std::ffi::OsString::from).collect(),
                std::path::Path::new(&cwd),
            );
            focus_recent_window(app);
            enqueue_open_paths(app, paths);
        }))
    };
    builder
        .setup(|app| {
            app.manage(ClipboardStore::create_unique()?);
            {
                let state = app.state::<SharedRuntime>();
                let role = if diagnostic_mode() {
                    window_registry::WindowRole::Diagnostic
                } else {
                    window_registry::WindowRole::Primary
                };
                state.lock().map_err(|e| e.to_string())?.windows.register("main", role);
            }
            if let Some(window) = app.get_webview_window("main") {
                install_window_events(app.handle(), &window);
            }
            let reap_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    interval.tick().await;
                    reap_expired_transfers(&reap_app);
                }
            });
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let paths = open_request::normalize_args(std::env::args_os().collect(), &cwd);
            enqueue_open_paths(app.handle(), paths);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_version,
            open_requests_submit,
            open_requests_ready,
            open_requests_ack,
            open_requests_commit,
            open_requests_fail,
            is_smoke,
            is_park,
            path_exists,
            smoke_force_big,
            smoke_set_force_big,
            finish_smoke,
            smoke_write_bytes,
            smoke_write_repeat,
            smoke_tmp_dir,
            smoke_spool_path,
            read_csv_file,
            write_csv_file,
            pick_open_path,
            pick_save_path,
            set_title,
            create_document_window,
            close_window,
            close_empty_window,
            begin_tab_drag,
            tab_drag_hover,
            drop_tab_drag,
            tab_drag_drop_result,
            cancel_tab_drag,
            seal_document,
            accept_transfer,
            transfer_adopted,
            complete_transfer,
            cancel_transfer,
            file_size,
            big_open,
            big_new,
            big_close,
            big_sort_estimate,
            big_sort_load,
            big_sort_cancel,
            big_sort_apply,
            big_sort_exit,
            big_meta,
            big_stale,
            big_rows,
            big_find_edge,
            big_set_cell,
            big_set_cells,
            big_clear,
            big_paste,
            big_paste_import_begin,
            big_paste_import_chunk,
            big_paste_import_finish,
            big_paste_import_cancel,
            big_paste_clipboard,
            big_insert_rows,
            big_remove_rows,
            big_undo,
            big_redo,
            big_set_undo_budget,
            big_filter,
            big_filter_count,
            big_filter_page,
            big_filter_hit_view_row,
            big_clear_filter,
            big_set_headerless,
            big_copy,
            big_clipboard_meta,
            big_clipboard_clear,
            big_save,
            big_save_as,
            big_col_push,
            big_col_pop,
            big_col_clear,
            big_col_apply,
        ])
        .build(tauri::generate_context!())
        .expect("error while building simpleTable")
        .run(|app, event| {
            match event {
                #[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
                tauri::RunEvent::Opened { urls } => {
                    let mut paths = Vec::new();
                    for url in urls {
                        match open_request::file_url_to_path(url.as_str()) {
                            Ok(path) if path.extension().and_then(|ext| ext.to_str())
                                .is_some_and(|ext| ext.eq_ignore_ascii_case("csv")) => {
                                paths.push(path);
                            }
                            Ok(_) => eprintln!("忽略非 CSV 的 Opened URL：{url}"),
                            Err(error) => eprintln!("忽略无效 Opened URL：{error}"),
                        }
                    }
                    focus_recent_window(app);
                    enqueue_open_paths(app, paths);
                }
                tauri::RunEvent::Exit => cleanup_process_state(app),
                _ => {}
            }
        });
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PasteImportBegin {
    import_id: String,
}

#[derive(Serialize)]
struct PasteImportChunk {
    bytes: u64,
}

/// 开始一个会话级外部文本导入。
#[tauri::command]
fn big_paste_import_begin(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
) -> Result<PasteImportBegin, String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| {
        bf.begin_paste_import().map(|import_id| PasteImportBegin { import_id })
    })
}

/// 上传一个 UTF-8 字节的十六进制分块。
#[tauri::command]
fn big_paste_import_chunk(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    import_id: String,
    hex: String,
    last: bool,
) -> Result<PasteImportChunk, String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| {
        bf.append_paste_import(&import_id, &hex, last).map(|bytes| PasteImportChunk { bytes })
    })
}

/// 流式完成外部文本导入，固定点预检沿用统一编辑协议。
#[tauri::command]
fn big_paste_import_finish(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    import_id: String,
    destination: engine::PasteDestination,
    approval: Approval,
) -> Result<EditOutcome, String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| bf.finish_paste_import(&import_id, destination, approval))
}

/// 使用当前进程的大复制 token 粘贴；失效 token 不回退系统剪贴板。
#[tauri::command]
fn big_paste_clipboard(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    clipboard: tauri::State<ClipboardStore>,
    session: u64,
    lease: u64,
    token: String,
    destination: engine::PasteDestination,
    approval: Approval,
) -> Result<EditOutcome, String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| {
        bf.paste_clipboard(clipboard.inner(), &token, destination, approval)
    })
}

/// 取消并清理一个尚未消费的导入。
#[tauri::command]
fn big_paste_import_cancel(
    window: tauri::Window,
    state: tauri::State<SharedRuntime>,
    session: u64,
    lease: u64,
    import_id: String,
) -> Result<(), String> {
    with_big(&state, &window, session, lease, Access::Write, |bf| bf.cancel_paste_import(&import_id))
}

/// 最后一个窗口退出进程后清理所有文档和进程级临时目录。
fn cleanup_process_state(app: &tauri::AppHandle) {
    let entries = match app.state::<SharedRuntime>().lock() {
        Ok(mut guard) => guard.docs.drain(),
        Err(_) => Vec::new(),
    };
    for entry in entries {
        entry.bump_gate();
        entry.load_cancel.store(true, Ordering::Relaxed);
        drop(entry.file.lock());
    }
    // 可在 quit、RunEvent::Exit 和 smoke 出口重复调用；目录仅属于本进程。
    app.state::<ClipboardStore>().cleanup();
}
