//! simpleTable 程序入口：检测 --smoke 冒烟自检参数后进入 Tauri 事件循环。
//!
//! 发布版（release）不分配控制台窗口（windows_subsystem），避免双击 exe
//! 时闪出黑色命令行窗口。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|arg| arg == "--smoke") {
        // 冒烟自检模式：前端跑内置断言套件后调 finish_smoke 写报告并退出
        std::env::set_var("SIMPLE_TABLE_SMOKE", "1");
    }
    simpletable_lib::run();
}
