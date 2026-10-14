"""主窗口集成测试（离屏 Qt）。"""
import re
import sys

import pytest
from PySide6.QtCore import QMimeData, Qt, QUrl
from PySide6.QtGui import QGuiApplication, QTextOption
from PySide6.QtTest import QTest
from PySide6.QtWidgets import QApplication, QMessageBox

import csv_io
from main_window import (CellEditorDelegate, FilterHeaderView, MainWindow,
                         _CellEditor)


@pytest.fixture()
def window(qapp) -> MainWindow:
    """构造主窗口，测试后关闭（关闭前清脏标记避免弹出模态确认框）。"""
    w = MainWindow()
    yield w
    w._dirty = False  # 否则 closeEvent 会弹模态确认框
    w.close()


class TestBasics:
    def test_constructs_empty(self, window) -> None:
        assert window.model.rowCount() == 0
        assert not window.dock.isHidden()  # 编辑窗格常驻

    def test_filter_row_rebuilds_on_load(self, window) -> None:
        window.model.load(["A", "B"], [["1", "x\ny"]] * 5)
        assert window.proxy.rowCount() == 5
        assert window.filter_row.edit_count() == 2

    def test_ctrl_s_shortcut_registered(self, window) -> None:
        assert window.save_action.shortcut().toString() == "Ctrl+S"

    def test_cell_editor_delegate_installed(self, window) -> None:
        assert isinstance(window.view.itemDelegate(), CellEditorDelegate)


class TestDock:
    def test_select_cell_loads_dock(self, window) -> None:
        # 任意单元格（含普通列）选中后编辑窗格同步显示完整内容
        window.model.load(["A", "B"], [["1", "hello\nworld"]])
        window.view.setCurrentIndex(window.proxy.index(0, 1))
        assert window.cell_edit.toPlainText() == "hello\nworld"

    def test_dock_edit_writes_back(self, window) -> None:
        window.model.load(["A", "B"], [["1", "hello\nworld"]])
        window.view.setCurrentIndex(window.proxy.index(0, 1))
        window.cell_edit.setPlainText("改\n了\t！")
        assert window.model.raw_data(0, 1) == "改\n了\t！"

    def test_dock_follows_selection(self, window) -> None:
        window.model.load(["A", "B"], [["1", "x"], ["2", "y"]])
        window.view.setCurrentIndex(window.proxy.index(0, 1))
        window.view.setCurrentIndex(window.proxy.index(1, 1))
        assert window.cell_edit.toPlainText() == "y"

    def test_in_cell_edit_syncs_to_dock(self, window) -> None:
        window.model.load(["A"], [["旧"]])
        window.view.setCurrentIndex(window.proxy.index(0, 0))
        window.model.setData(window.model.index(0, 0), "新")
        assert window.cell_edit.toPlainText() == "新"

    def test_dock_typing_no_feedback_loop(self, window) -> None:
        # 窗格输入触发写回，但写回不得回灌窗格（否则光标跳动）
        window.model.load(["A"], [[""]])
        window.view.setCurrentIndex(window.proxy.index(0, 0))
        window.cell_edit.setPlainText("abc")
        assert window.cell_edit.toPlainText() == "abc"
        assert window.model.raw_data(0, 0) == "abc"


class TestShowSymbols:
    def test_action_is_checkable(self, window) -> None:
        assert window.show_symbols_action.isCheckable()

    def test_toggle_shows_and_hides_symbols(self, window) -> None:
        action = window.show_symbols_action
        doc = window.cell_edit.document()
        assert not doc.defaultTextOption().flags() & QTextOption.ShowTabsAndSpaces
        action.trigger()  # 勾选
        flags = doc.defaultTextOption().flags()
        assert flags & QTextOption.ShowTabsAndSpaces
        assert flags & QTextOption.ShowLineAndParagraphSeparators
        assert action.isChecked()
        action.trigger()  # 取消勾选
        flags = doc.defaultTextOption().flags()
        assert not flags & QTextOption.ShowTabsAndSpaces
        assert not flags & QTextOption.ShowLineAndParagraphSeparators


class TestZoom:
    def test_view_zoom_scales_font_and_rows(self, window) -> None:
        view = window.view
        base_font = view.font().pointSizeF()
        base_row_h = view.verticalHeader().defaultSectionSize()
        view.set_zoom(1.5)
        assert view.zoom_factor() == 1.5
        assert view.font().pointSizeF() == pytest.approx(base_font * 1.5)
        assert view.verticalHeader().defaultSectionSize() == int(base_row_h * 1.5)

    def test_zoom_clamped(self, window) -> None:
        window.view.set_zoom(100)
        assert window.view.zoom_factor() == 3.0
        window.view.set_zoom(0.01)
        assert window.view.zoom_factor() == 0.5

    def test_edit_pane_zoom_independent(self, window) -> None:
        window.cell_edit.set_zoom(2.0)
        assert window.cell_edit.zoom_factor() == 2.0
        assert window.view.zoom_factor() == 1.0  # 表格不受影响
        window.view.set_zoom(1.5)
        assert window.cell_edit.zoom_factor() == 2.0  # 编辑窗格不受影响

    def test_filter_row_follows_view_zoom(self, window) -> None:
        window.model.load(["A", "B"], [["1", "2"]])
        window.view.set_zoom(1.5)
        header = window.view.horizontalHeader()
        expected = max(16, int(FilterHeaderView.FILTER_BAND * 1.5))
        assert window.filter_row._filter_band_h() == expected
        # 表头总高 = 列名文字带 + 筛选带，固定高度已应用
        assert header.sizeHint().height() == header._text_band_h() + expected
        assert header.maximumHeight() == header._text_band_h() + expected


class TestSaveLoad:
    def test_save_and_reopen(self, window, tmp_path) -> None:
        path = str(tmp_path / "trip.csv")
        window.model.load(["目的地", "备注"],
                          [["大理", "古城\n洱海"], ["丽江", "雪山"]])
        window._path = path
        window.save_file()
        assert window._dirty is False
        # 重新打开
        window.model.load([], [])
        window._path = None
        import csv_io
        headers, rows, warnings = csv_io.read_csv(path)
        assert headers == ["目的地", "备注"]
        assert rows == [["大理", "古城\n洱海"], ["丽江", "雪山"]]
        assert warnings == []

    def test_dirty_flag_on_edit(self, window) -> None:
        window.model.load(["A"], [["1"]])
        window._dirty = False
        window.model.setData(window.model.index(0, 0), "2")
        assert window._dirty is True


def _mime_for(path: str) -> QMimeData:
    """构造含单个本地文件 URL 的 MIME 数据。

    Args:
        path: 本地文件路径。

    Returns:
        QMimeData: 拖放事件用的 MIME 数据。
    """
    mime = QMimeData()
    mime.setUrls([QUrl.fromLocalFile(path)])
    return mime


class TestZoomHeaderFix:
    def test_column_width_scales_with_zoom(self, window) -> None:
        # 回归：缩放时列宽按比例同步缩放，列名不会被挤没
        window.model.load(["目的地", "月份"], [["大理", "三月"]])
        header = window.view.horizontalHeader()
        w0 = header.sectionSize(0)
        window.view.set_zoom(2.0)
        assert header.sectionSize(0) == pytest.approx(w0 * 2, abs=2)
        window.view.set_zoom(1.0)
        assert header.sectionSize(0) == pytest.approx(w0, abs=2)

    def test_header_height_follows_font(self, window) -> None:
        # 回归：表头加高容纳内嵌筛选带后，仍能容纳列名行高
        window.view.set_zoom(2.0)
        header = window.view.horizontalHeader()
        assert (header.sizeHint().height()
                >= header.fontMetrics().height()
                + FilterHeaderView.FILTER_BAND)


class TestDragDrop:
    # 注意：不手工构造 QDropEvent/QDragEnterEvent（Windows 下会访问冲突），
    # 事件处理是薄壳，这里测 MIME 判断逻辑与打开路径。

    def test_mime_with_csv(self) -> None:
        assert MainWindow._csv_path_from_mime(_mime_for("a.csv")) == "a.csv"

    def test_mime_with_uppercase_extension(self) -> None:
        assert MainWindow._csv_path_from_mime(_mime_for("A.CSV")) == "A.CSV"

    def test_mime_with_non_csv(self) -> None:
        assert MainWindow._csv_path_from_mime(_mime_for("a.txt")) is None

    def test_mime_without_urls(self) -> None:
        assert MainWindow._csv_path_from_mime(QMimeData()) is None

    def test_open_path_loads_model(self, window, tmp_path) -> None:
        path = str(tmp_path / "dropped.csv")
        csv_io.write_csv(path, ["目的地", "月份"], [["大理", "三月"]])
        window._open_path(path)
        assert window.model.headers() == ["目的地", "月份"]
        assert window._path == path


class TestVersionLabel:
    def test_version_label_format(self, window) -> None:
        text = window.version_label.text()
        assert re.fullmatch(r"release v\S+|beta\+[0-9a-f]+|beta\+unknown",
                            text)


def _patch_confirm(monkeypatch, answer) -> None:
    """把 main_window 命名空间里的 QMessageBox 换成直接返回 answer 的假对象。

    Args:
        monkeypatch: pytest 的 monkeypatch fixture。
        answer: QMessageBox 按钮枚举（如 QMessageBox.Yes / QMessageBox.No）。
    """
    fake = type("FakeBox", (),
                {"question": staticmethod(lambda *a, **k: answer),
                 "Yes": QMessageBox.Yes, "No": QMessageBox.No})
    monkeypatch.setattr("main_window.QMessageBox", fake)


class TestRowColumnOps:
    """Excel 式行列操作（右键菜单背后的动作层）。"""

    def test_toolbar_decluttered(self, window) -> None:
        # 行列操作移出工具栏
        texts = [a.text() for a in window.toolbar.actions()]
        for gone in ("添加行", "删除行", "添加列", "删除列", "重命名列"):
            assert gone not in texts
        assert "排序…" in texts

    def test_selected_rows_mapping(self, window) -> None:
        window.model.load(["A", "B"], [["1", "x"], ["2", "y"], ["3", "z"]])
        window.view.selectRow(1)
        assert window._selected_source_rows() == [1]

    def test_selected_rows_mapping_with_filter(self, window) -> None:
        window.model.load(["A"], [["苹果"], ["香蕉"], ["苹果派"]])
        window.proxy.set_filter(0, "苹果")
        window.view.selectRow(1)  # 视图第 2 行 = 源模型第 3 行
        assert window._selected_source_rows() == [2]

    def test_row_header_click_selects_row(self, window) -> None:
        window.model.load(["A", "B"], [["1", "x"], ["3", "z"]])
        window.view.verticalHeader().sectionPressed.emit(1)
        assert window._selected_source_rows() == [1]

    def test_col_header_click_selects_col(self, window) -> None:
        window.model.load(["A", "B"], [["1", "x"]])
        window.view.horizontalHeader().sectionPressed.emit(1)
        assert window._selected_source_cols() == [1]

    def test_delete_rows_confirmed(self, window, monkeypatch) -> None:
        _patch_confirm(monkeypatch, QMessageBox.Yes)
        window.model.load(["A"], [["1"], ["2"], ["3"]])
        window._delete_rows([0, 2])
        assert window.model.rowCount() == 1
        assert window.model.raw_data(0, 0) == "2"

    def test_delete_rows_cancelled(self, window, monkeypatch) -> None:
        _patch_confirm(monkeypatch, QMessageBox.No)
        window.model.load(["A"], [["1"]])
        window._delete_rows([0])
        assert window.model.rowCount() == 1  # 取消则不删

    def test_clear_rows_confirmed(self, window, monkeypatch) -> None:
        _patch_confirm(monkeypatch, QMessageBox.Yes)
        window.model.load(["A", "B"], [["1", "x"]])
        window._clear_rows([0])
        assert window.model.raw_data(0, 0) == ""
        assert window.model.rowCount() == 1  # 行结构保留

    def test_delete_columns_confirmed(self, window, monkeypatch) -> None:
        _patch_confirm(monkeypatch, QMessageBox.Yes)
        window.model.load(["A", "B"], [["1", "x"]])
        window._delete_columns([1])
        assert window.model.headers() == ["A"]

    def test_clear_columns_confirmed(self, window, monkeypatch) -> None:
        _patch_confirm(monkeypatch, QMessageBox.Yes)
        window.model.load(["A", "B"], [["1", "x"]])
        window._clear_columns([1])
        assert window.model.headers() == ["A", "B"]  # 列结构保留
        assert window.model.raw_data(0, 1) == ""

    def test_clear_cells_confirmed(self, window, monkeypatch) -> None:
        _patch_confirm(monkeypatch, QMessageBox.Yes)
        window.model.load(["A", "B"], [["1", "x"], ["2", "y"]])
        window.view.setCurrentIndex(window.proxy.index(1, 1))
        window._clear_cells()
        assert window.model.raw_data(1, 1) == ""
        assert window.model.raw_data(0, 1) == "x"

    def test_insert_columns_clears_filters(self, window) -> None:
        # 列结构变化后旧筛选列号错位，必须清空避免误筛
        window.model.load(["A", "B"], [["x", "1"]])
        window.proxy.set_filter(1, "1")
        window._insert_columns(0, 1)
        assert window.proxy.active_count() == 0
        assert window.model.headers() == ["新列", "A", "B"]


class TestFilterRowAlignment:
    """筛选输入框内嵌在水平表头里：与列共用同一坐标系，天然对齐。"""

    def test_filter_boxes_live_inside_header(self, window) -> None:
        # 筛选行不是独立控件，就是表头本身（同一个元素）
        header = window.view.horizontalHeader()
        assert window.filter_row is header
        window.model.load(["A", "B"], [["1", "2"]])
        for edit in window.filter_row._edits:
            assert edit.parentWidget() is header.viewport()

    def test_edits_align_with_columns(self, window) -> None:
        window.model.load(["A", "B", "C"], [["1", "2", "3"]] * 5)
        window.show()
        QApplication.processEvents()
        header = window.view.horizontalHeader()
        for col, edit in enumerate(window.filter_row._edits):
            assert edit.x() == header.sectionViewportPosition(col)
            assert edit.width() == header.sectionSize(col)

    def test_edits_follow_horizontal_scroll(self, window) -> None:
        window.model.load(["c%d" % i for i in range(10)],
                          [[str(i)] * 10 for i in range(50)])
        window.resize(500, 400)
        window.show()
        QApplication.processEvents()
        window.view.horizontalScrollBar().setValue(300)
        QApplication.processEvents()
        header = window.view.horizontalHeader()
        for col, edit in enumerate(window.filter_row._edits):
            assert edit.x() == header.sectionViewportPosition(col)

    def test_edits_align_when_row_header_widens(self, window) -> None:
        # 200 行：行号变三位数、行号列变宽——同一控件内天然无错位
        window.model.load(["A", "B"], [["1", "2"]] * 200)
        window.show()
        QApplication.processEvents()
        header = window.view.horizontalHeader()
        for col, edit in enumerate(window.filter_row._edits):
            assert edit.x() == header.sectionViewportPosition(col)

    def test_edits_follow_column_resize(self, window) -> None:
        window.model.load(["A", "B", "C"], [["1", "2", "3"]])
        window.show()
        QApplication.processEvents()
        header = window.view.horizontalHeader()
        header.resizeSection(0, 260)
        assert window.filter_row._edits[0].width() == header.sectionSize(0)
        assert (window.filter_row._edits[1].x()
                == header.sectionViewportPosition(1))

    def test_edits_follow_window_resize(self, window) -> None:
        window.model.load(["A", "B", "C"], [["1", "2", "3"]] * 30)
        window.show()
        QApplication.processEvents()
        header = window.view.horizontalHeader()
        window.resize(1400, 800)
        QApplication.processEvents()
        for col, edit in enumerate(window.filter_row._edits):
            assert edit.x() == header.sectionViewportPosition(col)
        window.resize(500, 400)
        QApplication.processEvents()
        for col, edit in enumerate(window.filter_row._edits):
            assert edit.x() == header.sectionViewportPosition(col)


class TestExcelKeys:
    """单元格 Excel 式键位与剪贴板。"""

    def _open_editor(self, window: MainWindow, row: int,
                     col: int) -> _CellEditor:
        """打开指定代理单元格的格内编辑器并返回它。

        Args:
            window: 主窗口。
            row: 代理行号。
            col: 代理列号。

        Returns:
            _CellEditor: 打开的编辑器实例。
        """
        idx = window.proxy.index(row, col)
        window.view.setCurrentIndex(idx)
        window.view.edit(idx)
        return window.view.viewport().findChild(_CellEditor)

    def test_typing_starts_edit_replacing_content(self, window) -> None:
        """打字即编辑（Excel 式）：编辑器打开且旧内容被新输入替换。"""
        window.model.load(["A"], [["hello"]])
        window.view.setCurrentIndex(window.proxy.index(0, 0))
        QTest.keyClick(window.view, Qt.Key_A)
        ed = window.view.viewport().findChild(_CellEditor)
        assert ed is not None
        assert ed.toPlainText() == "a"

    def test_typing_then_enter_commits_and_moves_down(self, window) -> None:
        """打字后 Enter：替换内容提交到模型，光标下移一行。"""
        window.model.load(["A"], [["hello"], ["x"]])
        window.view.setCurrentIndex(window.proxy.index(0, 0))
        QTest.keyClick(window.view, Qt.Key_A)
        ed = window.view.viewport().findChild(_CellEditor)
        assert ed is not None
        QTest.keyClick(ed, Qt.Key_Return)
        QApplication.processEvents()  # 让 QTimer.singleShot(0) 的移动生效
        assert window.model.raw_data(0, 0) == "a"
        assert window.view.currentIndex().row() == 1

    @pytest.mark.skipif(sys.platform == "darwin",
                        reason="Qt macOS 源码无 F2 编辑触发器"
                               "（#ifdef Q_OS_MACOS 改为 Enter，且本应用"
                               " Enter 用于下移），F2 仅 Windows/Linux")
    def test_f2_edit_keeps_existing_content(self, window) -> None:
        """F2 维持旧行为：打开编辑器并保留原文（全选）。"""
        window.model.load(["A"], [["hello"]])
        idx = window.proxy.index(0, 0)
        window.view.setCurrentIndex(idx)
        QTest.keyClick(window.view, Qt.Key_F2)
        ed = window.view.viewport().findChild(_CellEditor)
        assert ed is not None
        assert ed.toPlainText() == "hello"

    def test_enter_moves_selection_down(self, window) -> None:
        window.model.load(["A"], [["1"], ["2"], ["3"]])
        window.view.setCurrentIndex(window.proxy.index(0, 0))
        QTest.keyClick(window.view, Qt.Key_Return)
        assert window.view.currentIndex().row() == 1

    def test_shift_enter_moves_up(self, window) -> None:
        window.model.load(["A"], [["1"], ["2"]])
        window.view.setCurrentIndex(window.proxy.index(1, 0))
        QTest.keyClick(window.view, Qt.Key_Return, Qt.ShiftModifier)
        assert window.view.currentIndex().row() == 0

    def test_tab_moves_selection_right(self, window) -> None:
        window.model.load(["A", "B"], [["1", "x"]])
        window.view.setCurrentIndex(window.proxy.index(0, 0))
        QTest.keyClick(window.view, Qt.Key_Tab)
        assert window.view.currentIndex().column() == 1

    def test_editor_enter_commits_and_moves_down(self, window) -> None:
        window.model.load(["A"], [["1"], ["2"]])
        ed = self._open_editor(window, 0, 0)
        assert ed is not None
        ed.setPlainText("新")
        QTest.keyClick(ed, Qt.Key_Return)
        QApplication.processEvents()  # 让 QTimer.singleShot(0) 的移动生效
        assert window.model.raw_data(0, 0) == "新"
        assert window.view.currentIndex().row() == 1

    def test_editor_tab_commits_and_moves_right(self, window) -> None:
        window.model.load(["A", "B"], [["1", "x"], ["2", "y"]])
        ed = self._open_editor(window, 0, 0)
        ed.setPlainText("改")
        QTest.keyClick(ed, Qt.Key_Tab)
        QApplication.processEvents()
        assert window.model.raw_data(0, 0) == "改"
        assert window.view.currentIndex().column() == 1

    def test_editor_alt_enter_inserts_newline(self, window) -> None:
        window.model.load(["A"], [["旧"]])
        ed = self._open_editor(window, 0, 0)
        QTest.keyClick(ed, Qt.Key_Return, Qt.AltModifier)
        assert ed.toPlainText().endswith("\n")  # 编辑器未关闭且已插入换行
        assert window.model.raw_data(0, 0) == "旧"  # 尚未提交

    def test_editor_escape_reverts(self, window) -> None:
        window.model.load(["A"], [["旧"]])
        ed = self._open_editor(window, 0, 0)
        ed.setPlainText("改")
        QTest.keyClick(ed, Qt.Key_Escape)
        QApplication.processEvents()
        assert window.model.raw_data(0, 0) == "旧"

    def test_delete_key_clears_cells(self, window) -> None:
        window.model.load(["A", "B"], [["1", "x"], ["2", "y"]])
        window.view.setCurrentIndex(window.proxy.index(0, 1))
        QTest.keyClick(window.view, Qt.Key_Delete)
        assert window.model.raw_data(0, 1) == ""
        assert window.model.raw_data(0, 0) == "1"  # 其他单元格不受影响

    def test_copy_selection_tsv_quotes_special(self, window) -> None:
        window.model.load(["A", "B"], [["a\nb", "x"], ["c", "d"]])
        window.view.selectAll()
        window._copy_selection()
        assert (QGuiApplication.clipboard().text()
                == '"a\nb"\tx\r\nc\td\r\n')

    def test_paste_tsv_grows_table(self, window) -> None:
        window.model.load(["A"], [["1"]])
        window.view.setCurrentIndex(window.proxy.index(0, 0))
        QGuiApplication.clipboard().setText("a\tb\r\nc\td")
        window._paste_from_clipboard()
        assert window.model.headers() == ["A", "新列"]
        assert window.model.rowCount() == 2
        assert window.model.raw_data(1, 1) == "d"

    def test_paste_within_bounds(self, window) -> None:
        window.model.load(["A", "B"], [["1", "2"], ["3", "4"]])
        window.view.setCurrentIndex(window.proxy.index(1, 0))
        QGuiApplication.clipboard().setText("hello")
        window._paste_from_clipboard()
        assert window.model.raw_data(1, 0) == "hello"
        assert window.model.rowCount() == 2  # 不扩展

    def test_copy_paste_roundtrip(self, window) -> None:
        # 复制一块区域再原样粘贴，内容一致
        window.model.load(["A", "B"], [["1", "x"], ["2", "y"]])
        window.view.selectAll()
        window._copy_selection()
        window.view.setCurrentIndex(window.proxy.index(0, 0))
        window._paste_from_clipboard()
        assert window.model.raw_data(0, 0) == "1"
        assert window.model.raw_data(1, 1) == "y"

class TestUndo:
    """撤销/重做与主窗口整合：工具栏按钮、粘贴合并、窗格击键合并。"""

    def test_toolbar_undo_redo_enabled_state(self, window) -> None:
        window.model.load(["A"], [["1"]])
        assert not window.undo_action.isEnabled()  # load 清空撤销栈
        assert not window.redo_action.isEnabled()
        window.model.setData(window.model.index(0, 0), "2")
        assert window.undo_action.isEnabled()
        window.undo_action.trigger()
        assert not window.undo_action.isEnabled()
        assert window.redo_action.isEnabled()

    def test_paste_is_single_undo_step(self, window) -> None:
        window.model.load(["A", "B"], [["1", "2"], ["3", "4"]])
        window.view.setCurrentIndex(window.proxy.index(0, 0))
        QGuiApplication.clipboard().setText("x\ty\nz\tw")
        window._paste_from_clipboard()
        assert window.model.raw_data(1, 1) == "w"
        window.undo_action.trigger()
        assert window.model.raw_data(0, 0) == "1"
        assert window.model.raw_data(1, 1) == "4"
        assert not window.model.can_undo()  # 整块粘贴只是一步

    def test_dock_typing_merges_to_one_step(self, window) -> None:
        window.model.load(["A"], [["hello"]])
        window.view.setCurrentIndex(window.proxy.index(0, 0))
        window.cell_edit.setPlainText("h")
        window.cell_edit.setPlainText("he")
        window.undo_action.trigger()
        assert window.model.raw_data(0, 0) == "hello"
        assert not window.model.can_undo()

    def test_undo_marks_dirty(self, window) -> None:
        window.model.load(["A"], [["1"]])
        window.model.setData(window.model.index(0, 0), "2")
        window._dirty = False  # 模拟已保存状态
        window.undo_action.trigger()
        assert window._dirty is True
        assert window.model.raw_data(0, 0) == "1"
