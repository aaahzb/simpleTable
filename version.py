"""版本信息：打包时优先读注入的 _build_info，开发环境回退 git 查询。

CI/CD 约定：打包脚本先运行 make_version.py 生成 _build_info.py，
HEAD 精确匹配 v 开头的 tag → "release <tag>"，否则 "beta+<commit>"。
"""
from __future__ import annotations

import subprocess

try:
    from _build_info import BUILD_VERSION
except ImportError:
    BUILD_VERSION = ""


def compute_version() -> str:
    """通过 git 计算版本字符串。

    Returns:
        str: HEAD 精确匹配 v 开头 tag → "release <tag>"；
             否则 "beta+<short commit>"；git 不可用 → "beta+unknown"。
    """
    try:
        tag = subprocess.run(
            ["git", "describe", "--tags", "--exact-match", "HEAD"],
            capture_output=True, text=True, timeout=5).stdout.strip()
        if tag.startswith("v"):
            return f"release {tag}"
        commit = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True, timeout=5).stdout.strip()
        if commit:
            return f"beta+{commit}"
    except (OSError, subprocess.SubprocessError):
        pass
    return "beta+unknown"


def get_version() -> str:
    """返回当前应显示的版本字符串。

    Returns:
        str: 打包注入值优先，否则实时 git 查询。
    """
    return BUILD_VERSION or compute_version()
