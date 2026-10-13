"""表格数据模型。

持有表头与二维字符串数据，负责显示扁平化与物理优先级排序。
所有单元格地位相同：均可格内编辑，也可在右侧编辑窗格编辑。
"""
from __future__ import annotations

import functools
from typing import Any

from PySide6.QtCore import QAbstractTableModel, QModelIndex, Qt


def _to_float(text: str) -> float | None:
    """尝试把字符串解析为浮点数。

    Args:
        text: 单元格文本。

    Returns:
        float | None: 解析成功返回数值，否则 None。
    """
    try:
        return float(text.strip())
    except ValueError:
        return None


class TableModel(QAbstractTableModel):
    """simpleTable 核心数据模型：所有数据以字符串存储。"""

    SORT_TEXT = "text"
    SORT_NUMERIC = "numeric"

    def __init__(self, parent: Any = None) -> None:
        super().__init__(parent)
        self._headers: list[str] = []
        self._rows: list[list[str]] = []

    # ---- 数据装载与读取 ----
    def load(self, headers: list[str], rows: list[list[str]]) -> None:
        """整体替换模型数据。

        Args:
            headers: 表头。
            rows: 数据行（列数须与表头一致）。
        """
        self.beginResetModel()
        self._headers = list(headers)
        self._rows = [list(r) for r in rows]
        self.endResetModel()

    def headers(self) -> list[str]:
        """返回表头副本。

        Returns:
            list[str]: 表头列表。
        """
        return list(self._headers)

    def all_rows(self) -> list[list[str]]:
        """返回全部数据行副本（供保存 CSV 用）。

        Returns:
            list[list[str]]: 数据行列表。
        """
        return [list(r) for r in self._rows]

    # ---- Qt 模型接口 ----
    def rowCount(self, parent: QModelIndex = QModelIndex()) -> int:
        return 0 if parent.isValid() else len(self._rows)

    def columnCount(self, parent: QModelIndex = QModelIndex()) -> int:
        return 0 if parent.isValid() else len(self._headers)

    def raw_data(self, row: int, col: int) -> str:
        """返回单元格完整内容（供筛选代理与编辑窗格使用）。

        Args:
            row: 行号。
            col: 列号。

        Returns:
            str: 单元格完整文本。
        """
        return self._rows[row][col]

    def data(self, index: QModelIndex, role: int = Qt.DisplayRole) -> Any:
        if not index.isValid():
            return None
        text = self._rows[index.row()][index.column()]
        if role == Qt.DisplayRole:
            # 显示层压平换行/Tab：单元格恒为单行，内容再多也不变形
            return text.replace("\r", " ").replace("\n", " ").replace("\t", " ")
        if role == Qt.EditRole:
            return text
        return None

    def flags(self, index: QModelIndex) -> Qt.ItemFlags:
        base = Qt.ItemIsSelectable | Qt.ItemIsEnabled
        if index.isValid():
            # 所有单元格都可格内编辑
            base |= Qt.ItemIsEditable
        return base

    def setData(self, index: QModelIndex, value: Any,
                role: int = Qt.EditRole) -> bool:
        if role == Qt.EditRole and index.isValid():
            self._rows[index.row()][index.column()] = str(value)
            self.dataChanged.emit(index, index, [Qt.DisplayRole, Qt.EditRole])
            return True
        return False

    def headerData(self, section: int, orientation: Qt.Orientation,
                   role: int = Qt.DisplayRole) -> Any:
        if role == Qt.DisplayRole:
            if orientation == Qt.Horizontal:
                return self._headers[section]
            return section + 1
        return None

    # ---- 行列操作 ----
    def append_rows(self, count: int = 1) -> None:
        """末尾追加空行。

        Args:
            count: 追加行数。
        """
        if count <= 0 or not self._headers:
            return
        first = len(self._rows)
        self.beginInsertRows(QModelIndex(), first, first + count - 1)
        for _ in range(count):
            self._rows.append([""] * len(self._headers))
        self.endInsertRows()

    def remove_rows(self, rows: list[int]) -> None:
        """删除指定的若干源模型行。

        Args:
            rows: 源模型行号列表。
        """
        for r in sorted(set(rows), reverse=True):
            if 0 <= r < len(self._rows):
                self.beginRemoveRows(QModelIndex(), r, r)
                del self._rows[r]
                self.endRemoveRows()

    def insert_rows(self, row: int, count: int = 1) -> None:
        """在指定位置插入空行。

        Args:
            row: 插入位置（该行号之前），越界自动夹取到有效范围。
            count: 插入行数。
        """
        if count <= 0 or not self._headers:
            return
        row = max(0, min(row, len(self._rows)))
        self.beginInsertRows(QModelIndex(), row, row + count - 1)
        for _ in range(count):
            self._rows.insert(row, [""] * len(self._headers))
        self.endInsertRows()

    def append_column(self, name: str) -> None:
        """末尾追加一列。

        Args:
            name: 列名。
        """
        col = len(self._headers)
        self.beginInsertColumns(QModelIndex(), col, col)
        self._headers.append(name)
        for row in self._rows:
            row.append("")
        self.endInsertColumns()

    def insert_columns(self, col: int, count: int = 1) -> None:
        """在指定位置插入列，自动命名（新列、新列2…）。

        Args:
            col: 插入位置（该列号之前），越界自动夹取到有效范围。
            count: 插入列数。
        """
        if count <= 0:
            return
        col = max(0, min(col, len(self._headers)))
        self.beginInsertColumns(QModelIndex(), col, col + count - 1)
        for i in range(count):
            self._headers.insert(col + i, self._unique_column_name("新列"))
            for row in self._rows:
                row.insert(col + i, "")
        self.endInsertColumns()

    def _unique_column_name(self, base: str) -> str:
        """生成不与现有表头冲突的列名。

        Args:
            base: 基础名。

        Returns:
            str: base 本身（无冲突时）或 base+序号（从 2 起）。
        """
        if base not in self._headers:
            return base
        n = 2
        while f"{base}{n}" in self._headers:
            n += 1
        return f"{base}{n}"

    def remove_column(self, col: int) -> None:
        """删除一列及其全部数据。

        Args:
            col: 列号。
        """
        if not 0 <= col < len(self._headers):
            return
        self.beginRemoveColumns(QModelIndex(), col, col)
        del self._headers[col]
        for row in self._rows:
            del row[col]
        self.endRemoveColumns()

    def remove_columns(self, cols: list[int]) -> None:
        """批量删除列。

        Args:
            cols: 列号列表（自动去重，从大到小逐个删除）。
        """
        for col in sorted(set(cols), reverse=True):
            self.remove_column(col)

    def rename_column(self, col: int, name: str) -> None:
        """重命名列。

        Args:
            col: 列号。
            name: 新列名。
        """
        self._headers[col] = name
        self.headerDataChanged.emit(Qt.Horizontal, col, col)

    def clear_rows(self, rows: list[int]) -> None:
        """清除指定行的全部内容（行本身保留）。

        Args:
            rows: 源模型行号列表。
        """
        valid = [r for r in set(rows) if 0 <= r < len(self._rows)]
        if not valid or not self._headers:
            return
        for r in valid:
            self._rows[r] = [""] * len(self._headers)
        self.dataChanged.emit(self.index(min(valid), 0),
                              self.index(max(valid), len(self._headers) - 1),
                              [Qt.DisplayRole, Qt.EditRole])

    def clear_columns(self, cols: list[int]) -> None:
        """清除指定列的全部内容（列本身保留）。

        Args:
            cols: 列号列表。
        """
        valid = [c for c in set(cols) if 0 <= c < len(self._headers)]
        if not valid or not self._rows:
            return
        for row in self._rows:
            for c in valid:
                row[c] = ""
        self.dataChanged.emit(self.index(0, min(valid)),
                              self.index(len(self._rows) - 1, max(valid)),
                              [Qt.DisplayRole, Qt.EditRole])

    def clear_cells(self, cells: set[tuple[int, int]]) -> None:
        """清除指定单元格集合的内容（结构保留）。

        Args:
            cells: (行号, 列号) 集合。
        """
        valid = {(r, c) for r, c in cells
                 if 0 <= r < len(self._rows)
                 and 0 <= c < len(self._headers)}
        if not valid:
            return
        for r, c in valid:
            self._rows[r][c] = ""
        rows = [r for r, _ in valid]
        cols = [c for _, c in valid]
        self.dataChanged.emit(self.index(min(rows), min(cols)),
                              self.index(max(rows), max(cols)),
                              [Qt.DisplayRole, Qt.EditRole])

    # ---- 物理排序 ----
    def sort_rows(self, levels: list[tuple[int, str, bool]]) -> None:
        """按优先级层级物理排序（真实重排数据）。

        Args:
            levels: (列号, 方式, 是否升序) 列表，列表顺序即优先级；
                    方式为 SORT_TEXT 或 SORT_NUMERIC。
                    数字方式下无法解析的单元格在该层级视为相等并排在最后。
        """
        if not levels:
            return

        def cmp(r1: list[str], r2: list[str]) -> int:
            for col, mode, asc in levels:
                a, b = r1[col], r2[col]
                if mode == self.SORT_NUMERIC:
                    fa, fb = _to_float(a), _to_float(b)
                    if fa is None and fb is None:
                        continue  # 都非数字：该层视为相等，落入下一层级
                    if fa is None:
                        return 1   # 非数字恒排最后（与升降序无关）
                    if fb is None:
                        return -1
                    if fa != fb:
                        return (-1 if fa < fb else 1) * (1 if asc else -1)
                else:
                    if a != b:
                        return (-1 if a < b else 1) * (1 if asc else -1)
            return 0

        # 用 layoutChanged 而非 beginResetModel：避免代理重建筛选行、丢失筛选状态
        self.layoutAboutToBeChanged.emit()
        self._rows.sort(key=functools.cmp_to_key(cmp))
        self.layoutChanged.emit()
