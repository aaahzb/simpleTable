//! simpleTable Tauri 壳层：文件读写（编码识别）、系统对话框、版本号、
//! 窗口标题、拖放打开、--smoke 自检通道。
//!
//! 前端经 `window.__TAURI__.core.invoke` 调用本模块命令；文件内容一律
//! 以字符串形式跨边界传输，编码处理全部收敛在本层（对齐 csv_io.py）。

use std::fs;
use std::io::ErrorKind;

use serde::Serialize;
use tauri::{Emitter, Manager};

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
    fs::write(&path, bytes).map_err(|e| io_err(&path, &e))
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
            finish_smoke,
            smoke_write_bytes,
            smoke_tmp_dir,
            read_csv_file,
            write_csv_file,
            pick_open_path,
            pick_save_path,
            set_title,
            quit_app,
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
