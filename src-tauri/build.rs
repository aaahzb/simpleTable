//! 构建脚本：版本号计算（对齐 make_version.py 逻辑）+ tauri-build。
//!
//! 优先使用显式注入的 `BUILD_VERSION`（发布 CI 直接传 tag，避免构建缓存
//! 或 tag 改名导致版本号陈旧）；未注入时 HEAD 精确匹配 v 开头 tag →
//! `release <tag>`，否则 `beta+<short commit>`，git 不可用 → `beta+unknown`。
//! 结果经 `cargo:rustc-env` 注入为 `BUILD_VERSION` 环境常量。

use std::process::Command;

/// 计算版本字符串（对照 version.py 的 compute_version）。
///
/// # Returns
/// 版本字符串，如 `release v1.0.0` / `beta+a1b2c3d` / `beta+unknown`。
fn compute_version() -> String {
    // CI 发布时显式传入 tag（如 `release v2026.10.19.4`）。版本号只取决于
    // 本次构建的参数，不受 git 状态、tag 改名或 cargo 缓存影响。
    if let Ok(v) = std::env::var("BUILD_VERSION") {
        let v = v.trim();
        if !v.is_empty() {
            return v.to_string();
        }
    }

    let run_git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .args(args)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };

    if let Some(tag) = run_git(&["describe", "--tags", "--exact-match", "HEAD"]) {
        if tag.starts_with('v') {
            return format!("release {tag}");
        }
    }
    match run_git(&["rev-parse", "--short", "HEAD"]) {
        Some(commit) => format!("beta+{commit}"),
        None => "beta+unknown".to_string(),
    }
}

fn main() {
    let version = compute_version();
    println!("cargo:rustc-env=BUILD_VERSION={version}");
    println!("cargo:rerun-if-env-changed=BUILD_VERSION");
    // 版本号跟随提交：HEAD（分支指针/游离哈希）与 refs/heads 下引用文件内容变化即重算
    println!("cargo:rerun-if-changed=../.git/HEAD");
    track_dir("../.git/refs/heads");
    // tag 变化也要重算版本（本地打 tag 后再次 build；CI 的缓存恢复同样依赖此声明）
    println!("cargo:rerun-if-changed=../.git/refs/tags");
    println!("cargo:rerun-if-changed=../.git/packed-refs");
    // 前端资产嵌入式打包：目录级 rerun-if-changed 不递归文件内容，
    // 必须逐文件声明（frontend/* 与 frontend/js/* 两层覆盖当前全部资产）。
    println!("cargo:rerun-if-changed=../frontend");
    track_dir("../frontend");
    tauri_build::build();
}

/// 递归声明目录内每个文件的 rerun-if-changed（仅限本仓库 frontend 的两层结构）。
///
/// # Parameters
/// - `dir`: 要跟踪的目录路径。
fn track_dir(dir: &str) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                track_dir(&path.to_string_lossy());
            } else {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
}
