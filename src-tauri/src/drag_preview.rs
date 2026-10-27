//! 跨窗口拖动时的轻量原生标签预览。
//!
//! 预览不是文档窗口，不注册 WindowRegistry，也不执行应用启动路径。它置顶、
//! 不抢焦点并忽略鼠标事件，只跟随原生光标；结束、取消或源销毁时由调用方回收。

use tauri::{AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};

use crate::drag_registry::PhysicalPoint;

const PREVIEW_LABEL: &str = "tab-drag-preview";
const PREVIEW_OFFSET_X: i32 = 12;
const PREVIEW_OFFSET_Y: i32 = 12;

/// 生成只通过查询参数传递文本的本地预览页 URL。
pub fn preview_url(title: &str) -> String {
    let title = title.chars().take(160).collect::<String>();
    format!("drag-preview.html?title={}", percent_encode(&title))
}

/// UTF-8 字节百分号编码；未保留字符按 URL 规范原样保留。
fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for &byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

/// 预览位于光标右下方，避免遮住光标和落点，并支持负显示器坐标。
pub fn preview_position(point: PhysicalPoint) -> PhysicalPoint {
    PhysicalPoint {
        x: point.x.saturating_add(PREVIEW_OFFSET_X),
        y: point.y.saturating_add(PREVIEW_OFFSET_Y),
    }
}

/// 在 Pointer Capture 换算出的物理点创建预览；视觉失败不阻断文档拖放。
pub fn show_at(app: &AppHandle, title: &str, point: PhysicalPoint) -> Result<(), String> {
    show(app, title, point)
}

fn show(app: &AppHandle, title: &str, point: PhysicalPoint) -> Result<(), String> {
    // begin 已保证没有活动拖放；这里只回收上次异常留下的同类预览。
    if let Some(existing) = app.get_webview_window(PREVIEW_LABEL) {
        let _ = existing.destroy();
    }
    let position = preview_position(point);
    let window = WebviewWindowBuilder::new(
        app,
        PREVIEW_LABEL,
        WebviewUrl::App(preview_url(title).into()),
    )
    .title("标签拖动")
    .inner_size(200.0, 34.0)
    .position(f64::from(position.x), f64::from(position.y))
    .decorations(false)
    .resizable(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .focused(false)
    .focusable(false)
    .visible(false)
    .shadow(true)
    .background_throttling(tauri::utils::config::BackgroundThrottlingPolicy::Disabled)
    .build()
    .map_err(|error| format!("创建标签预览失败：{error}"))?;
    if let Err(error) = window.set_ignore_cursor_events(true) {
        let _ = window.destroy();
        return Err(format!("设置标签预览鼠标穿透失败：{error}"));
    }
    if let Err(error) = window.show() {
        let _ = window.destroy();
        return Err(format!("显示标签预览失败：{error}"));
    }
    Ok(())
}

/// 每次原生 hover 都让预览跟随真实光标，包括其他窗口和桌面区域。
pub fn follow(app: &AppHandle, point: PhysicalPoint) {
    let Some(window) = app.get_webview_window(PREVIEW_LABEL) else {
        return;
    };
    let position = preview_position(point);
    if let Err(error) = window.set_position(PhysicalPosition::new(position.x, position.y)) {
        eprintln!("移动标签预览失败：{error}");
    }
}

/// 回收预览；重复调用安全，不影响任何文档窗口。
pub fn destroy(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(PREVIEW_LABEL) {
        if let Err(error) = window.destroy() {
            eprintln!("关闭标签预览失败：{error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{preview_position, preview_url};
    use crate::drag_registry::PhysicalPoint;

    #[test]
    fn preview_url_percent_encodes_utf8_spaces_and_markup() {
        assert_eq!(
            preview_url("表 格#<1>.csv"),
            "drag-preview.html?title=%E8%A1%A8%20%E6%A0%BC%23%3C1%3E.csv",
        );
        assert_eq!(
            preview_url("a-b_c.d~e.csv"),
            "drag-preview.html?title=a-b_c.d~e.csv",
        );
    }

    #[test]
    fn preview_position_follows_negative_coordinates_without_overflow() {
        assert_eq!(
            preview_position(PhysicalPoint { x: -400, y: 220 }),
            PhysicalPoint { x: -388, y: 232 },
        );
        assert_eq!(
            preview_position(PhysicalPoint { x: i32::MAX, y: i32::MAX }),
            PhysicalPoint { x: i32::MAX, y: i32::MAX },
        );
    }
}
