"""对话框组件单元测试（离屏 Qt，直接操作控件后验证数据出口）。"""
import pytest

from main_window import ColumnDialog, NewTableDialog, SortDialog
from table_model import TableModel


class TestSortDialog:
    def test_levels_priority_order(self, qapp) -> None:
        dlg = SortDialog(["目的地", "评分"])
        # 第一级：评分 数字 降序
        dlg.col_combo.setCurrentIndex(1)
        dlg.mode_combo.setCurrentIndex(1)
        dlg.order_combo.setCurrentIndex(1)
        dlg._add_level()
        # 第二级：目的地 字符 升序
        dlg.col_combo.setCurrentIndex(0)
        dlg.mode_combo.setCurrentIndex(0)
        dlg.order_combo.setCurrentIndex(0)
        dlg._add_level()
        assert dlg.levels() == [
            (1, TableModel.SORT_NUMERIC, False),
            (0, TableModel.SORT_TEXT, True),
        ]

    def test_move_and_remove(self, qapp) -> None:
        dlg = SortDialog(["A", "B"])
        dlg.col_combo.setCurrentIndex(0)
        dlg._add_level()
        dlg.col_combo.setCurrentIndex(1)
        dlg._add_level()
        # 把第 2 级上移为第 1 级
        dlg.level_list.setCurrentRow(1)
        dlg._move(-1)
        assert dlg.levels()[0][0] == 1
        # 移除第 1 级
        dlg.level_list.setCurrentRow(0)
        dlg._remove_level()
        assert dlg.levels() == [(0, TableModel.SORT_TEXT, True)]


class TestColumnDialog:
    def test_result(self, qapp) -> None:
        dlg = ColumnDialog("添加列", "预算")
        assert dlg.result() == "预算"


class TestNewTableDialog:
    def test_columns_skip_empty_names(self, qapp) -> None:
        dlg = NewTableDialog()
        dlg.table.item(0, 0).setText("目的地")
        dlg._append_row("备注")
        dlg._append_row("")  # 空名应被跳过
        assert dlg.columns() == ["目的地", "备注"]
