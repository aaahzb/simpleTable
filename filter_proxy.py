"""多列关键字筛选代理：子串匹配、不区分大小写、列间 AND。"""
from __future__ import annotations

from typing import Any

from PySide6.QtCore import QModelIndex, QSortFilterProxyModel

from table_model import TableModel


class FilterProxy(QSortFilterProxyModel):
    """每列一个筛选关键字的行过滤代理。"""

    def __init__(self, parent: Any = None) -> None:
        super().__init__(parent)
        self._filters: dict[int, str] = {}  # 列号 → 关键字

    def _refilter(self) -> None:
        """重新应用筛选条件。

        Qt 6.10 起 invalidateFilter() 被弃用，改用
        beginFilterChange()/endFilterChange()；为兼容旧版 Qt 做存在性判断。
        """
        if hasattr(self, "beginFilterChange"):
            self.beginFilterChange()
            self.endFilterChange()
        else:
            self.invalidateFilter()

    def set_filter(self, col: int, text: str) -> None:
        """设置某列筛选关键字。

        Args:
            col: 列号。
            text: 关键字；空字符串表示取消该列筛选。
        """
        if text:
            self._filters[col] = text
        else:
            self._filters.pop(col, None)
        self._refilter()

    def clear_filters(self) -> None:
        """清除全部筛选条件。"""
        self._filters.clear()
        self._refilter()

    def active_count(self) -> int:
        """返回当前生效的筛选条件数。

        Returns:
            int: 条件数。
        """
        return len(self._filters)

    def get_filter(self, col: int) -> str:
        """返回某列当前关键字（供筛选行重建时恢复文本）。

        Args:
            col: 列号。

        Returns:
            str: 关键字，无则空串。
        """
        return self._filters.get(col, "")

    def filterAcceptsRow(self, source_row: int,
                         source_parent: QModelIndex) -> bool:
        model = self.sourceModel()
        assert isinstance(model, TableModel), "FilterProxy 需搭配 TableModel"
        for col, keyword in self._filters.items():
            if col >= model.columnCount():
                continue
            # 大窗格列按完整内容匹配，而非表格里显示的摘要
            if keyword.lower() not in model.raw_data(source_row, col).lower():
                return False
        return True
