"""TableModel 单元测试（离屏 Qt）。"""
import pytest
from PySide6.QtCore import Qt

from table_model import TableModel


@pytest.fixture()
def model(qapp) -> TableModel:
    """三行三列测试模型：目的地 / 评分 / 备注（含多行内容）。"""
    m = TableModel()
    m.load(["目的地", "评分", "备注"],
           [["大理", "3", "古城\n洱海"],
            ["丽江", "10", "雪山"],
            ["黄山", "abc", ""]])
    return m


class TestDisplay:
    def test_display_flattens_newline(self, model) -> None:
        # 显示层压平换行：单元格恒为单行，不变形
        assert model.data(model.index(0, 2)) == "古城 洱海"

    def test_display_flattens_tab(self, model) -> None:
        model.setData(model.index(0, 0), "a\tb")
        assert model.data(model.index(0, 0)) == "a b"

    def test_edit_role_returns_full(self, model) -> None:
        assert model.data(model.index(0, 2), Qt.EditRole) == "古城\n洱海"

    def test_header_display(self, model) -> None:
        assert model.headerData(2, Qt.Horizontal) == "备注"
        assert model.headerData(0, Qt.Horizontal) == "目的地"


class TestEditable:
    def test_all_cells_editable(self, model) -> None:
        # 所有单元格（含多行内容）都可格内编辑
        for col in range(model.columnCount()):
            assert model.flags(model.index(0, col)) & Qt.ItemIsEditable

    def test_set_data_any_cell(self, model) -> None:
        assert model.setData(model.index(0, 2), "新\n内容") is True
        assert model.raw_data(0, 2) == "新\n内容"


class TestStructure:
    def test_append_and_remove_rows(self, model) -> None:
        model.append_rows(2)
        assert model.rowCount() == 5
        model.remove_rows([0, 1])
        assert model.rowCount() == 3
        assert model.raw_data(0, 0) == "黄山"

    def test_append_rename_remove_column(self, model) -> None:
        model.append_column("评论")
        assert model.columnCount() == 4
        assert model.headers()[3] == "评论"
        model.rename_column(3, "评价")
        assert model.headers()[3] == "评价"
        model.remove_column(3)
        assert model.columnCount() == 3


    def test_insert_rows(self, model) -> None:
        model.insert_rows(1, 2)
        assert model.rowCount() == 5
        assert model.raw_data(1, 0) == ""  # 新插入的空行
        assert model.raw_data(3, 0) == "丽江"  # 原第 2 行下移

    def test_insert_rows_clamped_to_end(self, model) -> None:
        model.insert_rows(999, 1)
        assert model.rowCount() == 4

    def test_insert_columns_autoname(self, model) -> None:
        model.insert_columns(1, 2)
        assert model.columnCount() == 5
        assert model.headers() == ["目的地", "新列", "新列2", "评分", "备注"]
        assert model.raw_data(0, 3) == "3"  # 原评分列右移

    def test_remove_columns(self, model) -> None:
        model.remove_columns([0, 2])
        assert model.headers() == ["评分"]

    def test_clear_rows(self, model) -> None:
        model.clear_rows([0])
        assert model.raw_data(0, 0) == ""
        assert model.raw_data(0, 2) == ""
        assert model.raw_data(1, 0) == "丽江"  # 其他行不受影响
        assert model.rowCount() == 3  # 行保留

    def test_clear_columns(self, model) -> None:
        model.clear_columns([1])
        assert all(model.raw_data(r, 1) == "" for r in range(3))
        assert model.raw_data(0, 0) == "大理"  # 其他列不受影响
        assert model.columnCount() == 3  # 列保留

    def test_clear_cells(self, model) -> None:
        model.clear_cells({(0, 0), (2, 2)})
        assert model.raw_data(0, 0) == ""
        assert model.raw_data(2, 2) == ""
        assert model.raw_data(0, 1) == "3"


class TestSort:
    def test_numeric_descending(self, model) -> None:
        model.sort_rows([(1, TableModel.SORT_NUMERIC, False)])
        assert [model.raw_data(r, 0) for r in range(3)] == ["丽江", "大理", "黄山"]

    def test_non_numeric_last_even_descending(self, model) -> None:
        model.sort_rows([(1, TableModel.SORT_NUMERIC, False)])
        assert model.raw_data(2, 1) == "abc"  # 非数字排最后

    def test_text_ascending(self, model) -> None:
        model.sort_rows([(0, TableModel.SORT_TEXT, True)])
        # Unicode 码点：丽(U+4E3D) < 大(U+5927) < 黄(U+9EC4)
        assert [model.raw_data(r, 0) for r in range(3)] == ["丽江", "大理", "黄山"]

    def test_tie_falls_to_next_level(self, qapp) -> None:
        m = TableModel()
        m.load(["A", "B"], [["x", "2"], ["x", "10"], ["x", "1"]])
        m.sort_rows([(0, TableModel.SORT_TEXT, True),
                     (1, TableModel.SORT_NUMERIC, True)])
        assert [m.raw_data(r, 1) for r in range(3)] == ["1", "2", "10"]

class TestUndo:
    """快照式撤销/重做：所有模型改动自动可撤销。"""

    def test_set_data_undo_redo(self, model) -> None:
        model.setData(model.index(0, 0), "昆明")
        assert model.can_undo()
        assert model.undo()
        assert model.raw_data(0, 0) == "大理"
        assert model.can_redo()
        assert model.redo()
        assert model.raw_data(0, 0) == "昆明"

    def test_new_edit_clears_redo(self, model) -> None:
        model.setData(model.index(0, 0), "昆明")
        model.undo()
        model.setData(model.index(0, 0), "成都")
        assert not model.can_redo()

    def test_undo_stack_capped(self, model) -> None:
        for i in range(TableModel.MAX_UNDO + 5):
            model.setData(model.index(0, 0), str(i))
        assert len(model._undo_stack) == TableModel.MAX_UNDO

    def test_merged_same_cell_is_one_step(self, model) -> None:
        # 编辑窗格同格连续击键：三次写入合并为一步撤销
        model.set_data_merged(model.index(0, 0), "昆")
        model.set_data_merged(model.index(0, 0), "昆明")
        model.set_data_merged(model.index(0, 0), "昆明市")
        model.undo()
        assert model.raw_data(0, 0) == "大理"
        assert not model.can_undo()

    def test_merged_other_cell_is_new_step(self, model) -> None:
        model.set_data_merged(model.index(0, 0), "昆明")
        model.set_data_merged(model.index(1, 0), "成都")
        model.undo()
        assert model.raw_data(1, 0) == "丽江"
        assert model.can_undo()  # 第一格的改动仍可撤销

    def test_batch_is_one_step(self, model) -> None:
        with model.batch():
            model.insert_rows(model.rowCount(), 1)
            model.setData(model.index(3, 0), "香格里拉")
        model.undo()
        assert model.rowCount() == 3
        assert not model.can_undo()

    def test_remove_columns_is_one_step(self, model) -> None:
        model.remove_columns([0, 1, 2])
        assert model.columnCount() == 0
        model.undo()
        assert model.headers() == ["目的地", "评分", "备注"]
        assert model.raw_data(0, 0) == "大理"

    def test_sort_undo_restores_order(self, model) -> None:
        model.sort_rows([(1, TableModel.SORT_NUMERIC, False)])
        assert model.raw_data(0, 0) == "丽江"
        model.undo()
        assert ([model.raw_data(r, 0) for r in range(3)]
                == ["大理", "丽江", "黄山"])

    def test_structure_ops_undo(self, model) -> None:
        model.remove_rows([1])
        model.undo()
        assert model.raw_data(1, 0) == "丽江"
        model.insert_columns(1, 2)
        model.undo()
        assert model.headers() == ["目的地", "评分", "备注"]
        model.rename_column(0, "城市")
        model.undo()
        assert model.headers()[0] == "目的地"

    def test_clear_cells_undo(self, model) -> None:
        model.clear_cells({(0, 0), (1, 1)})
        model.undo()
        assert model.raw_data(0, 0) == "大理"
        assert model.raw_data(1, 1) == "10"

    def test_load_clears_stacks(self, model) -> None:
        model.setData(model.index(0, 0), "昆明")
        model.load(["X"], [["1"]])
        assert not model.can_undo()
        assert not model.can_redo()

    def test_undo_on_empty_stack(self, model) -> None:
        assert not model.undo()
        assert not model.redo()
