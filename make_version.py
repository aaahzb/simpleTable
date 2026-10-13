"""打包辅助：计算版本字符串并写入 _build_info.py（不入库，打包后删除）。"""
from __future__ import annotations

import sys

from version import compute_version

BUILD_INFO_PATH = "_build_info.py"


def _utf8_stdout() -> None:
    """把标准输出切为 UTF-8。

    Windows CI runner 的控制台默认 cp1252，无法编码中文，
    打印中文日志会抛 UnicodeEncodeError 导致步骤失败。

    Returns:
        None
    """
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except AttributeError:
        pass  # 流对象不支持重配置时保持原样（不影响功能）


def main() -> None:
    """生成 _build_info.py 并打印注入的版本字符串。"""
    _utf8_stdout()
    ver = compute_version()
    with open(BUILD_INFO_PATH, "w", encoding="utf-8") as f:
        f.write('"""构建时生成的版本信息（由打包脚本生成，勿入库）。"""\n\n')
        f.write(f'BUILD_VERSION = "{ver}"\n')
    print(f"版本注入: {ver}")


if __name__ == "__main__":
    main()
