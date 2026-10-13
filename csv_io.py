"""CSV 读写模块。

遵循 RFC 4180：含换行、逗号、双引号的字段由 csv 模块自动加双引号，
字段内双引号转义为 ""。写出统一 utf-8-sig，读取自动识别编码。
"""
from __future__ import annotations

import csv
import io


def _read_text(path: str) -> str:
    """按 utf-8-sig → utf-8 → gbk 顺序尝试解码文件。

    Args:
        path: 文件路径。

    Returns:
        str: 解码后的文本。

    Raises:
        ValueError: 三种编码均无法解码时抛出。
    """
    with open(path, "rb") as f:
        data = f.read()
    for enc in ("utf-8-sig", "utf-8", "gbk"):
        try:
            return data.decode(enc)
        except UnicodeDecodeError:
            continue
    raise ValueError("无法识别文件编码（已尝试 utf-8-sig / utf-8 / gbk）")


def read_csv(path: str) -> tuple[list[str], list[list[str]], list[str]]:
    """读取 CSV 文件。

    Args:
        path: CSV 文件路径。

    Returns:
        tuple[list[str], list[list[str]], list[str]]:
            (表头, 数据行, 警告列表)。短行补空字符串，
            长行截断并产生警告；空文件返回 ([], [], [])。

    Raises:
        OSError: 文件无法打开。
        ValueError: 编码无法识别。
    """
    text = _read_text(path)
    warnings: list[str] = []
    reader = csv.reader(io.StringIO(text))
    # 跳过完全空的物理行（如文件末尾多余空行），但保留 ",," 这类空单元格行
    records = [row for row in reader if row]
    if not records:
        return [], [], []
    headers = records[0]
    ncols = len(headers)
    rows: list[list[str]] = []
    for i, row in enumerate(records[1:], start=1):
        if len(row) < ncols:
            warnings.append(
                f"数据行 {i} 只有 {len(row)} 列（表头 {ncols} 列），已补空")
            row = row + [""] * (ncols - len(row))
        elif len(row) > ncols:
            warnings.append(
                f"数据行 {i} 有 {len(row)} 列（表头 {ncols} 列），已截断")
            row = row[:ncols]
        rows.append(row)
    return headers, rows, warnings


def write_csv(path: str, headers: list[str], rows: list[list[str]]) -> None:
    """以 utf-8-sig 写出 CSV。

    Args:
        path: 目标路径。
        headers: 表头。
        rows: 数据行，列数须与表头一致。

    Raises:
        OSError: 写入失败（如文件被占用）。
    """
    # newline="" 是 csv 模块要求；默认 dialect 即 RFC 4180 最小引用
    with open(path, "w", encoding="utf-8-sig", newline="") as f:
        writer = csv.writer(f)
        writer.writerow(headers)
        writer.writerows(rows)
