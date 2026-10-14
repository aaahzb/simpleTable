"""表格数据模型。

持有表头与二维字符串数据，负责显示扁平化与物理优先级排序。
内置快照式撤销/重做：所有改动方法在变更前自动存档。
所有单元格地位相同：均可格内编辑，也可在右侧编辑窗格编辑。
"""
from __future__ import annotations

import functools
from collections.abc import Iterator
from contextlib import contextmanager
from typing import Any

from PySide6.QtCore import QAbstractTableModel, QModelIndex, Qt, Signal


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

    MAX_UNDO = 100  # 撤销栈上限（超出丢弃最旧快照）

    undoStackChanged = Signal()  # 撤销/重做栈状态变化（供按钮置灰）

    def __init__(self, parent: Any = None) -> None:
        super().__init__(parent)
        self._headers: list[str] = []
        self._rows: list[list[str]] = []
        # 撤销/重做：改动前的 (表头, 行数据) 快照栈；batch 块合并为一步；
        # _merge_cell 记录编辑窗格击键合并的目标格
        self._undo_stack: list[tuple[list[str], list[list[str]]]] = []
        self._redo_stack: list[tuple[list[str], list[list[str]]]] = []
        self._batch_depth = 0
        self._merge_cell: tuple[int, int] | None = None

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
        # 打开/新建文件 = 新的编辑会话：清空撤销历史（与 Excel 一致）
        self._undo_stack.clear()
        self._redo_stack.clear()
        self._merge_cell = None
        self.undoStackChanged.emit()

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

    # ---- 撤销/重做（快照式：改动前自动存档）----
    def _snapshot(self) -> tuple[list[str], list[list[str]]]:
        """生成当前数据快照（逐行浅拷贝，字符串不可变故安全）。

        Returns:
            tuple[list[str], list[list[str]]]: (表头副本, 行数据副本)。
        """
        return list(self._headers), [list(r) for r in self._rows]

    def _checkpoint(self, merge_key: tuple[int, int] | None = None) -> None:
        """数据改动前压入撤销快照（所有改动方法在变更前调用）。

        新改动使重做失效（清空重做栈）；batch 块内不重复压栈；
        merge_key 相同（编辑窗格同格连续击键）时合并为一步。

        Args:
            merge_key: 可合并改动的目标格 (行, 列)，None 表示不可合并。
        """
        had_redo = bool(self._redo_stack)
        self._redo_stack.clear()
        pushed = False
        if self._batch_depth > 0:
            pass  # batch() 进入时已压过快照
        elif merge_key is not None and merge_key == self._merge_cell:
            pass  # 同格连续击键：快照已存在，合并为一步
        else:
            self._undo_stack.append(self._snapshot())
            if len(self._undo_stack) > self.MAX_UNDO:
                self._undo_stack.pop(0)
            self._merge_cell = merge_key
            pushed = True
        if pushed or had_redo:
            self.undoStackChanged.emit()

    @contextmanager
    def batch(self) -> Iterator[None]:
        """批量改动上下文：整个 with 块合并为一步撤销（如粘贴一片区域）。"""
        if self._batch_depth == 0:
            self._checkpoint()
        self._batch_depth += 1
        try:
            yield
        finally:
            self._batch_depth -= 1

    def set_data_merged(self, index: QModelIndex, value: Any) -> bool:
        """写入单元格并把同格连续写入合并为一步撤销（编辑窗格击键用）。

        Args:
            index: 目标单元格。
            value: 新文本。

        Returns:
            bool: 写入成功为 True。
        """
        if index.isValid():
            self._checkpoint(merge_key=(index.row(), index.column()))
            self._rows[index.row()][index.column()] = str(value)
            self.dataChanged.emit(index, index, [Qt.DisplayRole, Qt.EditRole])
            return True
        return False

    def can_undo(self) -> bool:
        """是否有可撤销的改动。

        Returns:
            bool: 撤销栈非空。
        """
        return bool(self._undo_stack)

    def can_redo(self) -> bool:
        """是否有可重做的改动。

        Returns:
            bool: 重做栈非空。
        """
        return bool(self._redo_stack)

    def undo(self) -> bool:
        """撤销一步改动。

        Returns:
            bool: 有改动被撤销为 True。
        """
        if not self._undo_stack:
            return False
        self._redo_stack.append(self._snapshot())
        self._restore(self._undo_stack.pop())
        return True

    def redo(self) -> bool:
        """重做一步改动。

        Returns:
            bool: 有改动被重做为 True。
        """
        if not self._redo_stack:
            return False
        self._undo_stack.append(self._snapshot())
        self._restore(self._redo_stack.pop())
        return True

    def _restore(self, snapshot: tuple[list[str], list[list[str]]]) -> None:
        """用快照整体替换当前数据（整体重置视图，选择随之清空）。

        Args:
            snapshot: (表头, 行数据) 快照（所有权转移给模型）。
        """
        self.beginResetModel()
        self._headers, self._rows = snapshot
        self._merge_cell = None
        self.endResetModel()
        self.undoStackChanged.emit()

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
            self._checkpoint()
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
        self._checkpoint()
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
        self._checkpoint()
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
        self._checkpoint()
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
        self._checkpoint()
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
        self._checkpoint()
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
        self._checkpoint()
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
        cols = sorted(set(cols))
        if not cols:
            return
        with self.batch():  # 批量删列合并为一步撤销
            for col in reversed(cols):
                self.remove_column(col)

    def rename_column(self, col: int, name: str) -> None:
        """重命名列。

        Args:
            col: 列号。
            name: 新列名。
        """
        self._checkpoint()
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
        self._checkpoint()
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
        self._checkpoint()
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
        self._checkpoint()
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

        self._checkpoint()
        # 用 layoutChanged 而非 beginResetModel：避免代理重建筛选行、丢失筛选状态
        self.layoutAboutToBeChanged.emit()
        self._rows.sort(key=functools.cmp_to_key(cmp))
        self.layoutChanged.emit()
