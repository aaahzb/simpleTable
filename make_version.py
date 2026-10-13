"""打包辅助：计算版本字符串并写入 _build_info.py（不入库，打包后删除）。"""
from __future__ import annotations

from version import compute_version

BUILD_INFO_PATH = "_build_info.py"


def main() -> None:
    """生成 _build_info.py 并打印注入的版本字符串。"""
    ver = compute_version()
    with open(BUILD_INFO_PATH, "w", encoding="utf-8") as f:
        f.write('"""构建时生成的版本信息（由打包脚本生成，勿入库）。"""\n\n')
        f.write(f'BUILD_VERSION = "{ver}"\n')
    print(f"版本注入: {ver}")


if __name__ == "__main__":
    main()
