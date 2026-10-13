"""主窗口模块：对话框组件、筛选行、格内编辑委托、主窗口。"""
from __future__ import annotations

import csv
import io
from typing import Any

from PySide6.QtCore import (QAbstractItemModel, QEvent, QItemSelection,
                            QItemSelectionModel, QMimeData, QModelIndex,
                            QPoint, Qt, QTimer, Signal)
from PySide6.QtGui import (QAction, QCloseEvent, QCursor, QDragEnterEvent,
                           QDropEvent, QGuiApplication, QKeyEvent,
                           QKeySequence, QTextOption, QWheelEvent)
from PySide6.QtWidgets import (QAbstractItemDelegate, QAbstractItemView,
                               QComboBox, QDialog,
                               QDialogButtonBox, QDockWidget, QFileDialog,
                               QFormLayout, QHBoxLayout, QInputDialog, QLabel,
                               QLineEdit, QListWidget, QListWidgetItem,
                               QMainWindow, QMenu, QMessageBox, QPlainTextEdit,
                               QPushButton, QStatusBar, QStyledItemDelegate,
                               QTableView, QTableWidget, QTableWidgetItem,
                               QToolBar, QVBoxLayout, QWidget)

import csv_io
from filter_proxy import FilterProxy
from table_model import TableModel
from version import get_version


class SortDialog(QDialog):
    """组内优先级排序对话框：层级列表自上而下为优先级。"""

    def __init__(self, headers: list[str], parent: QWidget | None = None) -> None:
        """初始化排序对话框。

        Args:
            headers: 列名列表。
            parent: 父窗口。
        """
        super().__init__(parent)
        self.setWindowTitle("排序")
        self.resize(520, 320)
        self._headers = headers

        self.level_list = QListWidget()
        self.col_combo = QComboBox()
        self.col_combo.addItems(headers)
        self.mode_combo = QComboBox()
        self.mode_combo.addItems(["字符", "数字"])
        self.order_combo = QComboBox()
        self.order_combo.addItems(["升序", "降序"])

        add_btn = QPushButton("添加层级")
        remove_btn = QPushButton("移除层级")
        up_btn = QPushButton("上移")
        down_btn = QPushButton("下移")
        add_btn.clicked.connect(self._add_level)
        remove_btn.clicked.connect(self._remove_level)
        up_btn.clicked.connect(lambda: self._move(-1))
        down_btn.clicked.connect(lambda: self._move(1))

        buttons = QDialogButtonBox(QDialogButtonBox.Ok | QDialogButtonBox.Cancel)
        buttons.accepted.connect(self.accept)
        buttons.rejected.connect(self.reject)

        right = QVBoxLayout()
        right.addWidget(QLabel("列："))
        right.addWidget(self.col_combo)
        right.addWidget(QLabel("方式："))
        right.addWidget(self.mode_combo)
        right.addWidget(QLabel("方向："))
        right.addWidget(self.order_combo)
        right.addWidget(add_btn)
        right.addWidget(remove_btn)
        right.addWidget(up_btn)
        right.addWidget(down_btn)
        right.addStretch()

        top = QHBoxLayout()
        top.addWidget(self.level_list, 1)
        top.addLayout(right)

        layout = QVBoxLayout(self)
        layout.addWidget(QLabel("排序层级（自上而下为优先级，先排第一级）："))
        layout.addLayout(top)
        layout.addWidget(buttons)

    def _add_level(self) -> None:
        """把当前下拉框选择追加为一个排序层级。"""
        if not self._headers:
            return
        col = self.col_combo.currentIndex()
        mode = (TableModel.SORT_NUMERIC if self.mode_combo.currentIndex() == 1
                else TableModel.SORT_TEXT)
        asc = self.order_combo.currentIndex() == 0
        label = (f"{self._headers[col]}（"
                 f"{'数字' if mode == TableModel.SORT_NUMERIC else '字符'}，"
                 f"{'升序' if asc else '降序'}）")
        item = QListWidgetItem(label)
        item.setData(Qt.UserRole, (col, mode, asc))
        self.level_list.addItem(item)

    def _remove_level(self) -> None:
        """移除选中的层级。"""
        for item in self.level_list.selectedItems():
            self.level_list.takeItem(self.level_list.row(item))

    def _move(self, delta: int) -> None:
        """上下移动选中层级调整优先级。

        Args:
            delta: -1 上移，1 下移。
        """
        row = self.level_list.currentRow()
        target = row + delta
        if row < 0 or not (0 <= target < self.level_list.count()):
            return
        item = self.level_list.takeItem(row)
        self.level_list.insertItem(target, item)
        self.level_list.setCurrentRow(target)

    def levels(self) -> list[tuple[int, str, bool]]:
        """返回排序层级，与 TableModel.sort_rows 参数一致。

        Returns:
            list[tuple[int, str, bool]]: (列号, 方式, 是否升序) 列表。
        """
        return [self.level_list.item(i).data(Qt.UserRole)
                for i in range(self.level_list.count())]


class ColumnDialog(QDialog):
    """添加/重命名列对话框：仅列名。"""

    def __init__(self, title: str, name: str = "",
                 parent: QWidget | None = None) -> None:
        """初始化列对话框。

        Args:
            title: 窗口标题（"添加列" 或 "重命名列"）。
            name: 初始列名。
            parent: 父窗口。
        """
        super().__init__(parent)
        self.setWindowTitle(title)
        self.name_edit = QLineEdit(name)
        buttons = QDialogButtonBox(QDialogButtonBox.Ok | QDialogButtonBox.Cancel)
        buttons.accepted.connect(self.accept)
        buttons.rejected.connect(self.reject)
        layout = QFormLayout(self)
        layout.addRow("列名：", self.name_edit)
        layout.addRow(buttons)

    def result(self) -> str:
        """返回用户输入的列名。

        Returns:
            str: 去首尾空格后的列名。
        """
        return self.name_edit.text().strip()


class NewTableDialog(QDialog):
    """新建表格对话框：一次定义多列列名，确定后生成 20 行空行。"""

    def __init__(self, parent: QWidget | None = None) -> None:
        """初始化新建表格对话框。

        Args:
            parent: 父窗口。
        """
        super().__init__(parent)
        self.setWindowTitle("新建表格")
        self.resize(360, 320)
        self.table = QTableWidget(0, 1)
        self.table.setHorizontalHeaderLabels(["列名"])
        self.table.horizontalHeader().setStretchLastSection(True)

        add_btn = QPushButton("添加列")
        remove_btn = QPushButton("删除选中列")
        add_btn.clicked.connect(lambda: self._append_row(""))
        remove_btn.clicked.connect(self._remove_selected)

        buttons = QDialogButtonBox(QDialogButtonBox.Ok | QDialogButtonBox.Cancel)
        buttons.accepted.connect(self.accept)
        buttons.rejected.connect(self.reject)

        btn_row = QHBoxLayout()
        btn_row.addWidget(add_btn)
        btn_row.addWidget(remove_btn)
        btn_row.addStretch()

        layout = QVBoxLayout(self)
        layout.addWidget(QLabel("定义初始列（之后也可在表格中增删改）："))
        layout.addWidget(self.table)
        layout.addLayout(btn_row)
        layout.addWidget(buttons)

        self._append_row("")  # 默认先给一个空行便于直接填写

    def _append_row(self, name: str) -> None:
        """向列定义表追加一行。

        Args:
            name: 初始列名。
        """
        row = self.table.rowCount()
        self.table.insertRow(row)
        self.table.setItem(row, 0, QTableWidgetItem(name))

    def _remove_selected(self) -> None:
        """删除选中的列定义行。"""
        rows = {i.row() for i in self.table.selectedIndexes()}
        for r in sorted(rows, reverse=True):
            self.table.removeRow(r)

    def columns(self) -> list[str]:
        """返回列名列表，自动跳过空名。

        Returns:
            list[str]: 列名列表。
        """
        result: list[str] = []
        for r in range(self.table.rowCount()):
            item = self.table.item(r, 0)
            name = item.text().strip() if item else ""
            if name:
                result.append(name)
        return result


class FilterRow(QWidget):
    """表头下方的常驻筛选行：每列一个输入框，几何位置与表头同步。"""

    BASE_HEIGHT = 28

    def __init__(self, view: QTableView, proxy: FilterProxy,
                 parent: QWidget) -> None:
        """初始化筛选行。

        Args:
            view: 绑定的表格视图。
            proxy: 筛选代理（输入直接写入代理）。
            parent: 父控件（与视图同一中央容器）。
        """
        super().__init__(parent)
        self._view = view
        self._proxy = proxy
        self._edits: list[QLineEdit] = []
        self._zoom_factor = 1.0
        self._row_h = self.BASE_HEIGHT
        self._base_font_size = self.font().pointSizeF()
        self.setFixedHeight(self.BASE_HEIGHT)

        header = view.horizontalHeader()
        header.sectionResized.connect(self.refresh_geometry)
        header.sectionMoved.connect(self.refresh_geometry)
        view.horizontalScrollBar().valueChanged.connect(self.refresh_geometry)
        view.verticalHeader().geometriesChanged.connect(self.refresh_geometry)
        proxy.modelReset.connect(self.rebuild)
        proxy.columnsInserted.connect(self.rebuild)
        proxy.columnsRemoved.connect(self.rebuild)

    def rebuild(self) -> None:
        """按当前列数重建输入框，并从代理恢复已有筛选文本。"""
        for edit in self._edits:
            edit.deleteLater()
        self._edits = []
        for col in range(self._proxy.columnCount()):
            edit = QLineEdit(self)
            edit.setPlaceholderText("筛选…")
            edit.textChanged.connect(
                lambda text, c=col: self._proxy.set_filter(c, text))
            edit.show()
            self._apply_zoom(edit)
            self._edits.append(edit)
            # 恢复代理中已有的筛选文本（如模型重建后）
            old = self._proxy.get_filter(col)
            if old:
                edit.setText(old)
        self.refresh_geometry()
        # 行号列宽等布局要到下一事件循环才稳定（如行数位数跳变），延迟再校准一次
        QTimer.singleShot(0, self.refresh_geometry)

    def refresh_geometry(self, *args: Any) -> None:
        """按表头各节位置对齐输入框（列宽变化/滚动/行号列宽变化时触发）。

        sectionViewportPosition 是相对表头视口的坐标，必须先映射到
        本控件坐标系（中间隔着行号列与边框），否则输入框整体错位，
        且行号列变宽（行数过百）时错位加剧。

        Args:
            *args: 兼容信号附带的参数（忽略）。
        """
        header = self._view.horizontalHeader()
        # 表头视口原点在本控件坐标系中的 x：行号列宽 + 边框
        base_x = header.viewport().mapTo(self, QPoint(0, 0)).x()
        for col, edit in enumerate(self._edits):
            if col >= header.count():
                break
            x = base_x + header.sectionViewportPosition(col)
            edit.setGeometry(x, 1, header.sectionSize(col), self._row_h - 2)

    def clear(self) -> None:
        """清空全部输入框（textChanged 会连带清空代理筛选条件）。"""
        for edit in self._edits:
            edit.clear()

    def edit_count(self) -> int:
        """返回当前筛选输入框数量（供测试与状态检查）。

        Returns:
            int: 输入框数量。
        """
        return len(self._edits)

    def set_zoom(self, factor: float) -> None:
        """设置缩放因子：行高与输入框字体随表格同步缩放。

        Args:
            factor: 缩放因子（与表格视图一致）。
        """
        self._zoom_factor = factor
        self._row_h = max(16, int(self.BASE_HEIGHT * factor))
        self.setFixedHeight(self._row_h)
        for edit in self._edits:
            self._apply_zoom(edit)
        self.refresh_geometry()

    def _apply_zoom(self, edit: QLineEdit) -> None:
        """把当前缩放因子应用到单个输入框字体。

        Args:
            edit: 筛选输入框。
        """
        font = edit.font()
        font.setPointSizeF(self._base_font_size * self._zoom_factor)
        edit.setFont(font)


class _FontZoomMixin:
    """Ctrl+滚轮 字体缩放公共逻辑。

    子类约定：
    1. __init__ 中调用 _init_zoom()；
    2. 类体中定义 zoomChanged = Signal(float)（信号只能挂在 Qt 子类上）；
    3. wheelEvent 中调用 _wheel_zoom(event)，返回 False 时走默认处理。
    """

    ZOOM_MIN = 0.5
    ZOOM_MAX = 3.0
    ZOOM_STEP = 1.15

    def _init_zoom(self) -> None:
        """初始化缩放状态（记录基准字号）。"""
        self._zoom = 1.0
        self._base_font_size = self.font().pointSizeF()

    def zoom_factor(self) -> float:
        """返回当前缩放因子（1.0 为原始大小）。

        Returns:
            float: 缩放因子。
        """
        return self._zoom

    def set_zoom(self, factor: float) -> None:
        """设置缩放因子（自动夹取到 0.5–3.0），缩放字体并发出通知。

        Args:
            factor: 目标缩放因子。
        """
        factor = max(self.ZOOM_MIN, min(self.ZOOM_MAX, factor))
        if abs(factor - self._zoom) < 1e-6:
            return
        self._zoom = factor
        font = self.font()
        font.setPointSizeF(self._base_font_size * factor)
        self.setFont(font)
        self._after_zoom()
        self.zoomChanged.emit(factor)

    def _after_zoom(self) -> None:
        """缩放后钩子，子类按需覆盖。"""

    def _wheel_zoom(self, event: QWheelEvent) -> bool:
        """处理 Ctrl+滚轮缩放。

        Args:
            event: 滚轮事件。

        Returns:
            bool: True 表示事件已作为缩放消费。
        """
        if not event.modifiers() & Qt.ControlModifier:
            return False
        if event.angleDelta().y() > 0:
            step = self.ZOOM_STEP
        else:
            step = 1 / self.ZOOM_STEP
        self.set_zoom(self._zoom * step)
        event.accept()
        return True


class ZoomableTableView(_FontZoomMixin, QTableView):
    """支持 Ctrl+滚轮 整体缩放的表格视图（行高与表头随字体同步）。"""

    zoomChanged = Signal(float)
    copyRequested = Signal()    # 表格区内 Ctrl+C
    pasteRequested = Signal()   # 表格区内 Ctrl+V
    deleteRequested = Signal()  # 表格区内 Del

    def __init__(self, parent: QWidget | None = None) -> None:
        """初始化视图并记录行高/表头基准值。

        Args:
            parent: 父控件。
        """
        super().__init__(parent)
        self._init_zoom()
        self._applied_zoom = 1.0  # 上次已应用到列宽的缩放因子
        self._base_row_h = self.verticalHeader().defaultSectionSize()

    def _after_zoom(self) -> None:
        header = self.horizontalHeader()
        # 列宽按缩放比同步缩放（网页式整体缩放），否则字体变大后列名会被挤没
        ratio = self._zoom / self._applied_zoom
        self._applied_zoom = self._zoom
        for col in range(header.count()):
            header.resizeSection(
                col, max(24, int(header.sectionSize(col) * ratio)))
        # 行高与表头高度随字体缩放；表头高度按当前字体行高加边距实时重算
        self.verticalHeader().setDefaultSectionSize(
            max(10, int(self._base_row_h * self._zoom)))
        header.setFixedHeight(max(10, header.fontMetrics().height() + 10))

    def wheelEvent(self, event: QWheelEvent) -> None:
        """Ctrl+滚轮缩放，其余保持默认滚动。

        Args:
            event: 滚轮事件。
        """
        if not self._wheel_zoom(event):
            super().wheelEvent(event)

    def move_cursor(self, drow: int, dcol: int) -> None:
        """把当前单元格按行列偏移移动（Excel 式提交后/选择状态导航）。

        Args:
            drow: 行偏移（正为向下）。
            dcol: 列偏移（正为向右）。
        """
        idx = self.currentIndex()
        model = self.model()
        if (not idx.isValid() or model is None
                or model.rowCount() == 0 or model.columnCount() == 0):
            return
        r = min(max(idx.row() + drow, 0), model.rowCount() - 1)
        c = min(max(idx.column() + dcol, 0), model.columnCount() - 1)
        self.setCurrentIndex(model.index(r, c))

    def event(self, event: QEvent) -> bool:
        """在最外层拦截 Tab/Shift+Tab：Excel 式右移/左移。

        Qt 默认的 Tab 行为在 event() 层级且离屏下不可靠，故提前拦截。

        Args:
            event: 事件。

        Returns:
            bool: True 表示事件已处理。
        """
        if (event.type() == QEvent.KeyPress
                and event.key() in (Qt.Key_Tab, Qt.Key_Backtab)):
            self.move_cursor(0, 1 if event.key() == Qt.Key_Tab else -1)
            return True
        return super().event(event)

    def keyPressEvent(self, event: QKeyEvent) -> None:
        """Excel 式单元格键位与剪贴板分发。

        选择状态下 Enter 下移、Shift+Enter 上移；Del / Ctrl+C / Ctrl+V
        通过信号交给主窗口处理。格内编辑器与编辑窗格是独立控件，
        它们的按键不会到达这里。

        Args:
            event: 按键事件。
        """
        if event.key() in (Qt.Key_Return, Qt.Key_Enter):
            up = bool(event.modifiers() & Qt.ShiftModifier)
            self.move_cursor(-1 if up else 1, 0)
            return
        if event.key() == Qt.Key_Delete:
            self.deleteRequested.emit()
            return
        if event.matches(QKeySequence.Copy):
            self.copyRequested.emit()
            return
        if event.matches(QKeySequence.Paste):
            self.pasteRequested.emit()
            return
        super().keyPressEvent(event)


class _ZoomablePlainTextEdit(_FontZoomMixin, QPlainTextEdit):
    """支持 Ctrl+滚轮 缩放的编辑窗格（与表格视图相互独立）。"""

    zoomChanged = Signal(float)

    def __init__(self, parent: QWidget | None = None) -> None:
        """初始化编辑窗格缩放状态。

        Args:
            parent: 父控件。
        """
        super().__init__(parent)
        self._init_zoom()

    def wheelEvent(self, event: QWheelEvent) -> None:
        """Ctrl+滚轮缩放（覆盖 QPlainTextEdit 自带缩放，统一步进与范围）。

        Args:
            event: 滚轮事件。
        """
        if not self._wheel_zoom(event):
            super().wheelEvent(event)


class _CellEditor(QPlainTextEdit):
    """格内编辑器（Excel 键位）。

    Enter 提交并下移；Shift+Enter 提交并上移；Alt+Enter 插入换行；
    Tab 提交并右移；Shift+Tab 提交并左移；Esc 取消。
    """

    def __init__(self, delegate: CellEditorDelegate,
                 parent: QWidget | None = None) -> None:
        """初始化格内编辑器。

        Args:
            delegate: 所属委托（用于提交/关闭信号）。
            parent: 父控件。
        """
        super().__init__(parent)
        self._delegate = delegate

    def keyPressEvent(self, event: QKeyEvent) -> None:
        """Excel 键位：提交后按方向移动，Alt+Enter 换行，Esc 取消。

        Args:
            event: 按键事件。
        """
        key = event.key()
        mods = event.modifiers()
        if key in (Qt.Key_Return, Qt.Key_Enter):
            if mods & Qt.AltModifier:
                self.insertPlainText("\n")  # Excel：Alt+Enter 格内换行
                return
            drow = -1 if mods & Qt.ShiftModifier else 1
            self._commit_and_move(drow, 0)
            return
        if key == Qt.Key_Tab:
            self._commit_and_move(0, 1)
            return
        if key == Qt.Key_Backtab:
            self._commit_and_move(0, -1)
            return
        if key == Qt.Key_Escape:
            self._delegate.closeEditor.emit(
                self, QAbstractItemDelegate.RevertModelCache)
            return
        super().keyPressEvent(event)

    def _commit_and_move(self, drow: int, dcol: int) -> None:
        """提交并关闭编辑器，随后移动当前单元格。

        Args:
            drow: 行偏移（正为向下）。
            dcol: 列偏移（正为向右）。
        """
        self._delegate.commitData.emit(self)
        self._delegate.closeEditor.emit(self)
        view = self._delegate.parent()
        if isinstance(view, ZoomableTableView):
            # 编辑器关闭后再移动，避免关闭过程与选区变化相互干扰
            QTimer.singleShot(0, lambda: view.move_cursor(drow, dcol))


class CellEditorDelegate(QStyledItemDelegate):
    """统一的多行安全格内编辑委托（所有单元格通用）。"""

    def createEditor(self, parent: QWidget, option: Any,
                     index: QModelIndex) -> QWidget:
        """创建格内编辑器（QPlainTextEdit，多行内容安全）。

        Args:
            parent: 父控件。
            option: 样式选项。
            index: 单元格索引。

        Returns:
            QWidget: 编辑器实例。
        """
        return _CellEditor(self, parent)

    def setEditorData(self, editor: _CellEditor, index: QModelIndex) -> None:
        """把模型完整内容装入编辑器。

        Args:
            editor: 编辑器。
            index: 单元格索引。
        """
        editor.setPlainText(index.data(Qt.EditRole) or "")
        editor.selectAll()

    def setModelData(self, editor: _CellEditor, model: QAbstractItemModel,
                     index: QModelIndex) -> None:
        """把编辑器内容写回模型（代理模型的 setData 会映射回源模型）。

        Args:
            editor: 编辑器。
            model: 视图使用的模型。
            index: 单元格索引。
        """
        model.setData(index, editor.toPlainText(), Qt.EditRole)

    def updateEditorGeometry(self, editor: QWidget, option: Any,
                             index: QModelIndex) -> None:
        """设置编辑器几何；多行内容时加高（浮于下方单元格之上，不改行高）。

        Args:
            editor: 编辑器。
            option: 样式选项。
            index: 单元格索引。
        """
        rect = option.rect
        text = index.data(Qt.EditRole) or ""
        lines = text.count("\n") + 1
        if lines > 1:
            line_h = editor.fontMetrics().lineSpacing()
            rect.setHeight(int(line_h * min(lines + 1, 8)) + 8)
        editor.setGeometry(rect)


class MainWindow(QMainWindow):
    """simpleTable 主窗口：工具栏 + 筛选行 + 表格 + 右侧编辑窗格。"""

    NEW_TABLE_ROWS = 20  # 新建表格的初始空行数

    def __init__(self) -> None:
        """初始化主窗口并装配全部组件。"""
        super().__init__()
        self.setWindowTitle("simpleTable")
        self.resize(1100, 700)

        self._path: str | None = None  # 当前文件路径
        self._dirty = False            # 未保存修改标记
        self._current_cell: tuple[int, int] | None = None  # 编辑窗格对应的源模型 (row, col)
        self._dock_origin = False      # 编辑窗格发起的写回中（避免回灌闪烁）

        self.model = TableModel(self)
        self.proxy = FilterProxy(self)
        self.proxy.setSourceModel(self.model)

        self.view = ZoomableTableView()
        self.view.setModel(self.proxy)
        self.view.setSortingEnabled(False)  # 排序走对话框，不用表头点击排序
        self.view.setWordWrap(False)        # 单元格不换行：内容再多也不变形
        self.view.setItemDelegate(CellEditorDelegate(self.view))
        # 编辑触发：双击 / F2 / 打字即输入（Excel 式 AnyKeyPressed）。
        # Enter/Tab/Del/Ctrl+C/Ctrl+V 已在视图层 event/keyPressEvent 拦截，
        # 不会到达 AnyKeyPressed 逻辑，键位行为不受影响。
        self.view.setEditTriggers(QAbstractItemView.DoubleClicked
                                  | QAbstractItemView.EditKeyPressed
                                  | QAbstractItemView.AnyKeyPressed)
        # 单元格快捷键经视图信号分发（按键只在表格聚焦时到达视图，
        # 不影响格内编辑器与右侧编辑窗格）
        self.view.copyRequested.connect(self._copy_selection)
        self.view.pasteRequested.connect(self._paste_from_clipboard)
        self.view.deleteRequested.connect(self._delete_key_clear)

        # 右键菜单（Excel 式行列操作：行头 / 列头 / 单元格三处）
        self.view.setContextMenuPolicy(Qt.CustomContextMenu)
        self.view.customContextMenuRequested.connect(self._on_cell_menu)
        vh = self.view.verticalHeader()
        vh.setContextMenuPolicy(Qt.CustomContextMenu)
        vh.customContextMenuRequested.connect(self._on_row_header_menu)
        hh = self.view.horizontalHeader()
        hh.setContextMenuPolicy(Qt.CustomContextMenu)
        hh.customContextMenuRequested.connect(self._on_col_header_menu)

        # 中央区域：筛选行在上，表格在下
        central = QWidget()
        vbox = QVBoxLayout(central)
        vbox.setContentsMargins(0, 0, 0, 0)
        vbox.setSpacing(0)
        self.filter_row = FilterRow(self.view, self.proxy, central)
        vbox.addWidget(self.filter_row)
        vbox.addWidget(self.view)
        self.setCentralWidget(central)

        # 右侧编辑窗格 Dock：随选中单元格同步，可编辑任意单元格
        self.dock = QDockWidget("编辑窗格", self)
        self.dock.setFeatures(QDockWidget.DockWidgetMovable
                              | QDockWidget.DockWidgetFloatable)
        self.cell_edit = _ZoomablePlainTextEdit()
        self.dock.setWidget(self.cell_edit)
        self.addDockWidget(Qt.RightDockWidgetArea, self.dock)
        # 编辑窗格默认接受文本拖放，关掉以便文件拖放冒泡到主窗口
        self.cell_edit.setAcceptDrops(False)

        self._build_toolbar()
        self.status = QStatusBar()
        self.setStatusBar(self.status)
        # 状态栏右侧常驻版本号（打包注入 release，开发环境 beta+commit）
        self.version_label = QLabel(get_version())
        self.version_label.setStyleSheet("color: gray;")
        self.status.addPermanentWidget(self.version_label)
        self.setAcceptDrops(True)  # 支持把 CSV 文件拖进窗口直接打开

        # ---- 信号连接 ----
        self.view.selectionModel().currentChanged.connect(
            self._on_current_changed)
        self.view.zoomChanged.connect(self._on_view_zoomed)
        self.cell_edit.zoomChanged.connect(
            lambda f: self.status.showMessage(
                f"编辑窗格缩放 {round(f * 100)}%", 1500))
        self.cell_edit.textChanged.connect(self._on_dock_edited)
        self.model.dataChanged.connect(self._on_data_changed)
        self.model.rowsInserted.connect(self._mark_dirty)
        self.model.rowsRemoved.connect(self._mark_dirty)
        self.model.columnsInserted.connect(self._mark_dirty)
        self.model.columnsRemoved.connect(self._mark_dirty)
        self.model.headerDataChanged.connect(self._mark_dirty)
        self.model.modelReset.connect(self._on_model_reset)
        self.proxy.rowsInserted.connect(self._update_status)
        self.proxy.rowsRemoved.connect(self._update_status)
        self.proxy.layoutChanged.connect(self._update_status)
        self.proxy.modelReset.connect(self._update_status)

        self.model.load([], [])  # 起始空表
        self._update_status()

    # ---- 工具栏 ----
    def _build_toolbar(self) -> None:
        """构建顶部工具栏。"""
        tb = QToolBar("主工具栏", self)
        tb.setMovable(False)
        self.addToolBar(tb)
        self.toolbar = tb

        def act(text: str, slot: Any, shortcut: str | None = None,
                checkable: bool = False) -> QAction:
            action = QAction(text, self)
            if checkable:
                action.setCheckable(True)
            action.triggered.connect(slot)
            if shortcut:
                action.setShortcut(shortcut)
            tb.addAction(action)
            return action

        act("新建", self.new_file)
        act("打开", self.open_file)
        self.save_action = act("保存", self.save_file, "Ctrl+S")
        act("另存为", self.save_file_as)
        tb.addSeparator()
        act("排序…", self.open_sort_dialog)
        act("清除筛选", self.clear_filters)
        tb.addSeparator()
        self.show_symbols_action = act(
            "显示符号", self._toggle_show_symbols, checkable=True)
        self.show_symbols_action.setToolTip(
            "在编辑窗格显示空格(·)、Tab(→)、换行符(¶)，类似 Notepad++")

    # ---- 单元格剪贴板与 Del 键（Excel 互通 TSV）----
    def _copy_selection(self) -> None:
        """复制选区为 TSV 文本（含 Tab/换行的单元格按 CSV 规则加引号，与 Excel 互通）。"""
        sel = self.view.selectionModel()
        idxs = sel.selectedIndexes()
        if not idxs:
            return
        rows = sorted({i.row() for i in idxs})
        cols = sorted({i.column() for i in idxs})
        buf = io.StringIO()
        writer = csv.writer(buf, delimiter="\t", lineterminator="\r\n")
        for r in rows:
            line = []
            for c in cols:
                idx = self.proxy.index(r, c)
                # 非矩形选区：未选中的网格位置补空，保持行列对齐
                text = idx.data(Qt.EditRole) if sel.isSelected(idx) else ""
                line.append(text or "")
            writer.writerow(line)
        QGuiApplication.clipboard().setText(buf.getvalue())
        self.status.showMessage(f"已复制 {len(idxs)} 个单元格", 2000)

    def _paste_from_clipboard(self) -> None:
        """从当前单元格起粘贴 TSV 文本（与 Excel 互通），超范围自动扩展行列。"""
        text = QGuiApplication.clipboard().text()
        if not text:
            return
        start = self.view.selectionModel().currentIndex()
        if not start.isValid():
            return
        try:
            rows = list(csv.reader(io.StringIO(text), delimiter="\t"))
        except csv.Error:
            self.status.showMessage("粘贴失败：剪贴板文本无法解析", 3000)
            return
        if not rows:
            return
        src = self.proxy.mapToSource(start)
        r0, c0 = src.row(), src.column()
        # 超出当前范围时自动扩展行列（Excel 行为）
        need_rows = r0 + len(rows) - self.model.rowCount()
        if need_rows > 0:
            self.model.insert_rows(self.model.rowCount(), need_rows)
        need_cols = c0 + max(len(r) for r in rows) - self.model.columnCount()
        if need_cols > 0:
            self.model.insert_columns(self.model.columnCount(), need_cols)
        for i, row in enumerate(rows):
            for j, val in enumerate(row):
                self.model.setData(self.model.index(r0 + i, c0 + j), val)
        # 选中刚粘贴的区域（Excel 行为）
        top = self.proxy.mapFromSource(self.model.index(r0, c0))
        bottom = self.proxy.mapFromSource(self.model.index(
            r0 + len(rows) - 1, c0 + max(len(r) for r in rows) - 1))
        if top.isValid() and bottom.isValid():
            sel = self.view.selectionModel()
            sel.setCurrentIndex(top, QItemSelectionModel.NoUpdate)
            sel.select(QItemSelection(top, bottom),
                       QItemSelectionModel.ClearAndSelect)
        self.status.showMessage(f"已粘贴 {len(rows)} 行", 2000)

    def _delete_key_clear(self) -> None:
        """Del 键：立即清除选中单元格内容（Excel 行为，不弹确认）。"""
        cells: set[tuple[int, int]] = set()
        for i in self.view.selectionModel().selectedIndexes():
            src = self.proxy.mapToSource(i)
            cells.add((src.row(), src.column()))
        if cells:
            self.model.clear_cells(cells)
            self.status.showMessage(f"已清除 {len(cells)} 个单元格的内容", 2000)

    # ---- 行列操作（Excel 式右键菜单）----
    def _selected_source_rows(self) -> list[int]:
        """当前选区覆盖的源模型行号（升序）。

        Returns:
            list[int]: 源模型行号列表。
        """
        return sorted({self.proxy.mapToSource(i).row()
                       for i in self.view.selectionModel().selectedIndexes()})

    def _selected_source_cols(self) -> list[int]:
        """当前选区覆盖的源模型列号（升序）。

        Returns:
            list[int]: 源模型列号列表。
        """
        return sorted({self.proxy.mapToSource(i).column()
                       for i in self.view.selectionModel().selectedIndexes()})

    def _confirm(self, text: str) -> bool:
        """通用二次确认对话框。

        Args:
            text: 提示文本。

        Returns:
            bool: 用户点"是"返回 True。
        """
        return QMessageBox.question(
            self, "确认操作", text,
            QMessageBox.Yes | QMessageBox.No) == QMessageBox.Yes

    def _on_row_header_menu(self, pos: QPoint) -> None:
        """行头右键菜单：插入/添加/删除/清除行。

        Args:
            pos: 菜单触发位置（行头视口坐标）。
        """
        header = self.view.verticalHeader()
        row = header.logicalIndexAt(pos)
        if row < 0:
            return
        if not self.view.selectionModel().isRowSelected(row, QModelIndex()):
            self.view.selectRow(row)  # 右击未选中的行：先把选区移到该行
        rows = self._selected_source_rows()
        if not rows:
            return
        menu = QMenu(self)
        act_insert = menu.addAction(f"在上方插入 {len(rows)} 行")
        act_append = menu.addAction("在末尾添加 N 行…")
        menu.addSeparator()
        act_delete = menu.addAction(f"删除选中的 {len(rows)} 行…")
        act_clear = menu.addAction(f"清除选中 {len(rows)} 行的内容…")
        chosen = menu.exec(QCursor.pos())
        if chosen is act_insert:
            self.model.insert_rows(min(rows), len(rows))
        elif chosen is act_append:
            self._append_rows_dialog()
        elif chosen is act_delete:
            self._delete_rows(rows)
        elif chosen is act_clear:
            self._clear_rows(rows)

    def _on_col_header_menu(self, pos: QPoint) -> None:
        """列头右键菜单：插入/删除/清除/重命名列。

        Args:
            pos: 菜单触发位置（列头视口坐标）。
        """
        header = self.view.horizontalHeader()
        col = header.logicalIndexAt(pos)
        if col < 0:
            return
        if not self.view.selectionModel().isColumnSelected(col,
                                                           QModelIndex()):
            self.view.selectColumn(col)
        cols = self._selected_source_cols()
        if not cols:
            return
        menu = QMenu(self)
        act_insert = menu.addAction(f"在左侧插入 {len(cols)} 列")
        menu.addSeparator()
        act_delete = menu.addAction(f"删除选中的 {len(cols)} 列…")
        act_clear = menu.addAction(f"清除选中 {len(cols)} 列的内容…")
        menu.addSeparator()
        act_rename = menu.addAction("重命名列…")
        chosen = menu.exec(QCursor.pos())
        if chosen is act_insert:
            self._insert_columns(min(cols), len(cols))
        elif chosen is act_delete:
            self._delete_columns(cols)
        elif chosen is act_clear:
            self._clear_columns(cols)
        elif chosen is act_rename:
            self._rename_column(col)

    def _on_cell_menu(self, pos: QPoint) -> None:
        """单元格右键菜单：按选区跨度插入/删除/清除行列。

        Args:
            pos: 菜单触发位置（视口坐标）。
        """
        idx = self.view.indexAt(pos)
        if not idx.isValid():
            return
        if not self.view.selectionModel().isSelected(idx):
            self.view.setCurrentIndex(idx)  # 右击未选中的单元格：先选中它
        rows = self._selected_source_rows()
        cols = self._selected_source_cols()
        cell_count = len(self.view.selectionModel().selectedIndexes())
        menu = QMenu(self)
        act_ins_row = menu.addAction(f"在上方插入 {len(rows)} 行")
        act_ins_col = menu.addAction(f"在左侧插入 {len(cols)} 列")
        menu.addSeparator()
        act_del_row = menu.addAction(f"删除选区跨越的 {len(rows)} 行…")
        act_del_col = menu.addAction(f"删除选区跨越的 {len(cols)} 列…")
        act_clear = menu.addAction(f"清除选区 {cell_count} 个单元格内容…")
        chosen = menu.exec(QCursor.pos())
        if chosen is act_ins_row:
            self.model.insert_rows(min(rows), len(rows))
        elif chosen is act_ins_col:
            self._insert_columns(min(cols), len(cols))
        elif chosen is act_del_row:
            self._delete_rows(rows)
        elif chosen is act_del_col:
            self._delete_columns(cols)
        elif chosen is act_clear:
            self._clear_cells()

    def _append_rows_dialog(self) -> None:
        """弹数字输入框，在末尾一次性添加 N 行。"""
        n, ok = QInputDialog.getInt(
            self, "添加行", "在末尾添加的行数：", 10, 1, 10000)
        if ok:
            self.model.insert_rows(self.model.rowCount(), n)

    def _delete_rows(self, rows: list[int]) -> None:
        """二次确认后删除指定行。

        Args:
            rows: 源模型行号列表。
        """
        if rows and self._confirm(
                f"确定删除选中的 {len(rows)} 行？此操作不可撤销。"):
            self.model.remove_rows(rows)

    def _clear_rows(self, rows: list[int]) -> None:
        """二次确认后清除指定行内容（行结构保留）。

        Args:
            rows: 源模型行号列表。
        """
        if rows and self._confirm(
                f"确定清除选中 {len(rows)} 行的全部内容？（行本身保留）"):
            self.model.clear_rows(rows)

    def _insert_columns(self, col: int, count: int) -> None:
        """在指定位置插入列并清空筛选（列号已错位，避免误筛）。

        Args:
            col: 插入位置列号。
            count: 插入列数。
        """
        self.model.insert_columns(col, count)
        self.clear_filters()

    def _delete_columns(self, cols: list[int]) -> None:
        """二次确认后删除指定列（列结构变化后清空筛选避免误筛）。

        Args:
            cols: 列号列表。
        """
        if cols and self._confirm(
                f"确定删除选中的 {len(cols)} 列及其全部数据？此操作不可撤销。"):
            self.model.remove_columns(cols)
            self.clear_filters()

    def _clear_columns(self, cols: list[int]) -> None:
        """二次确认后清除指定列内容（列结构保留）。

        Args:
            cols: 列号列表。
        """
        if cols and self._confirm(
                f"确定清除选中 {len(cols)} 列的全部内容？（列本身保留）"):
            self.model.clear_columns(cols)

    def _clear_cells(self) -> None:
        """二次确认后清除选区单元格内容（结构保留）。"""
        cells: set[tuple[int, int]] = set()
        for i in self.view.selectionModel().selectedIndexes():
            src = self.proxy.mapToSource(i)
            cells.add((src.row(), src.column()))
        if cells and self._confirm(
                f"确定清除选区 {len(cells)} 个单元格的内容？（结构保留）"):
            self.model.clear_cells(cells)

    def _rename_column(self, col: int) -> None:
        """弹对话框重命名指定列。

        Args:
            col: 列号。
        """
        dlg = ColumnDialog("重命名列", self.model.headers()[col], self)
        if dlg.exec() == QDialog.Accepted:
            name = dlg.result()
            if name:
                self.model.rename_column(col, name)

    # ---- 排序与筛选 ----
    def open_sort_dialog(self) -> None:
        """打开排序对话框并执行物理排序。排序前清空选中避免行号失效。"""
        names = self.model.headers()
        if not names:
            return
        dlg = SortDialog(names, self)
        if dlg.exec() == QDialog.Accepted and dlg.levels():
            self.view.clearSelection()
            self._clear_dock()
            self.model.sort_rows(dlg.levels())
            self._mark_dirty()

    def clear_filters(self) -> None:
        """清空筛选行与代理条件。"""
        self.filter_row.clear()

    # ---- 编辑窗格 Dock ----
    def _clear_dock(self) -> None:
        """清空编辑窗格内容与跟踪位置。"""
        self._current_cell = None
        self.cell_edit.blockSignals(True)
        self.cell_edit.clear()
        self.cell_edit.blockSignals(False)

    def _on_current_changed(self, current: QModelIndex) -> None:
        """选中变化时同步右侧编辑窗格内容。

        Args:
            current: 视图层当前索引。
        """
        if not current.isValid():
            self._clear_dock()
            return
        src = self.proxy.mapToSource(current)
        self._current_cell = (src.row(), src.column())
        # 加载内容时阻断信号，避免触发写回造成无谓的脏标记
        self.cell_edit.blockSignals(True)
        self.cell_edit.setPlainText(self.model.raw_data(src.row(), src.column()))
        self.cell_edit.blockSignals(False)

    def _on_dock_edited(self) -> None:
        """编辑窗格内容实时写回模型。"""
        if self._current_cell is not None:
            row, col = self._current_cell
            self._dock_origin = True
            try:
                self.model.setData(self.model.index(row, col),
                                   self.cell_edit.toPlainText())
            finally:
                self._dock_origin = False

    def _on_data_changed(self, top_left: QModelIndex,
                         bottom_right: QModelIndex,
                         roles: list | None = None) -> None:
        """模型数据变化：标脏，并把格内编辑同步进编辑窗格。

        Args:
            top_left: 变化区域左上角。
            bottom_right: 变化区域右下角。
            roles: 变化的角色（未使用）。
        """
        self._mark_dirty()
        # 编辑窗格自己发起的写回不再回灌（避免光标跳动）
        if self._current_cell is None or self._dock_origin:
            return
        row, col = self._current_cell
        if (top_left.row() <= row <= bottom_right.row()
                and top_left.column() <= col <= bottom_right.column()):
            new_text = self.model.raw_data(row, col)
            if self.cell_edit.toPlainText() != new_text:
                self.cell_edit.blockSignals(True)
                self.cell_edit.setPlainText(new_text)
                self.cell_edit.blockSignals(False)

    def _toggle_show_symbols(self, checked: bool) -> None:
        """切换编辑窗格的不可见字符显示（仿 Notepad++）。

        Args:
            checked: True 显示空格(·)/Tab(→)/换行符(¶)，False 恢复普通显示。
        """
        doc = self.cell_edit.document()
        opt = doc.defaultTextOption()
        symbols = (QTextOption.ShowTabsAndSpaces
                   | QTextOption.ShowLineAndParagraphSeparators
                   | QTextOption.AddSpaceForLineAndParagraphSeparators)
        # 读-改-写，保留文档原有的其他显示标志
        flags = (opt.flags() | symbols) if checked else (opt.flags() & ~symbols)
        opt.setFlags(flags)
        doc.setDefaultTextOption(opt)
        self.cell_edit.viewport().update()  # 立即重绘

    # ---- 缩放 ----
    def _on_view_zoomed(self, factor: float) -> None:
        """表格缩放时同步筛选行并提示当前比例。

        Args:
            factor: 当前缩放因子。
        """
        self.filter_row.set_zoom(factor)
        self.status.showMessage(f"表格缩放 {round(factor * 100)}%", 1500)

    # ---- 文件操作 ----
    def new_file(self) -> None:
        """新建表格：确认丢弃修改后弹出列定义对话框。"""
        if not self._confirm_discard():
            return
        dlg = NewTableDialog(self)
        if dlg.exec() != QDialog.Accepted:
            return
        names = dlg.columns()
        if not names:
            QMessageBox.warning(self, "新建表格", "至少需要一列。")
            return
        self.clear_filters()
        self.model.load(
            names, [[""] * len(names) for _ in range(self.NEW_TABLE_ROWS)])
        self._path = None
        self._dirty = False
        self._update_title()

    def open_file(self) -> None:
        """打开 CSV：文件对话框选择。"""
        if not self._confirm_discard():
            return
        path, _ = QFileDialog.getOpenFileName(
            self, "打开 CSV", "", "CSV 文件 (*.csv)")
        if path:
            self._open_path(path)

    def _open_path(self, path: str) -> None:
        """打开指定路径的 CSV（菜单打开与拖放打开共用）。

        Args:
            path: CSV 文件路径。编码自动识别，列数以表头为准。
        """
        try:
            headers, rows, warnings = csv_io.read_csv(path)
        except (OSError, ValueError) as exc:
            QMessageBox.critical(self, "打开失败", str(exc))
            return
        self.clear_filters()
        self.model.load(headers, rows)
        self._path = path
        self._dirty = False
        self._update_title()
        if warnings:
            QMessageBox.information(
                self, "打开提示", "\n".join(warnings[:20]))

    # ---- 拖放打开 ----
    @staticmethod
    def _csv_path_from_mime(mime: QMimeData) -> str | None:
        """从拖放 MIME 数据中提取第一个本地 .csv 文件路径。

        Args:
            mime: 拖放事件的 MIME 数据。

        Returns:
            str | None: 第一个 .csv 本地文件路径；没有则 None。
        """
        if not mime.hasUrls():
            return None
        for url in mime.urls():
            path = url.toLocalFile()
            if url.isLocalFile() and path.lower().endswith(".csv"):
                return path
        return None

    def dragEnterEvent(self, event: QDragEnterEvent) -> None:
        """拖入内容含 .csv 本地文件时接受拖放。

        Args:
            event: 拖入事件。
        """
        if self._csv_path_from_mime(event.mimeData()) is not None:
            event.acceptProposedAction()
        else:
            event.ignore()

    def dropEvent(self, event: QDropEvent) -> None:
        """放下文件时打开第一个 .csv（沿用未保存修改确认流程）。

        Args:
            event: 放下事件。
        """
        path = self._csv_path_from_mime(event.mimeData())
        if path is not None and self._confirm_discard():
            self._open_path(path)
        event.acceptProposedAction()

    def save_file(self) -> None:
        """保存到当前路径（无路径时转另存为）。"""
        if self._path is None:
            self.save_file_as()
            return
        try:
            csv_io.write_csv(self._path, self.model.headers(),
                             self.model.all_rows())
        except OSError as exc:
            QMessageBox.critical(
                self, "保存失败",
                f"{exc}\n文件可能被其他程序占用，可尝试另存为。")
            return
        self._dirty = False
        self._update_title()

    def save_file_as(self) -> None:
        """另存为 CSV。"""
        path, _ = QFileDialog.getSaveFileName(
            self, "另存为 CSV", "", "CSV 文件 (*.csv)")
        if not path:
            return
        if not path.lower().endswith(".csv"):
            path += ".csv"
        self._path = path
        self.save_file()

    def _confirm_discard(self) -> bool:
        """有未保存修改时询问用户。

        Returns:
            bool: True 表示可以继续（已保存或用户放弃修改）。
        """
        if not self._dirty:
            return True
        ret = QMessageBox.question(
            self, "未保存的修改", "当前表格有未保存的修改，是否保存？",
            QMessageBox.Save | QMessageBox.Discard | QMessageBox.Cancel)
        if ret == QMessageBox.Save:
            self.save_file()
            return not self._dirty  # 保存失败（如取消另存为）则视为不继续
        return ret == QMessageBox.Discard

    def closeEvent(self, event: QCloseEvent) -> None:
        """关闭前确认未保存修改。

        Args:
            event: 关闭事件。
        """
        if self._confirm_discard():
            event.accept()
        else:
            event.ignore()

    # ---- 状态维护 ----
    def _mark_dirty(self, *args: Any) -> None:
        """标记未保存并刷新标题与状态栏。"""
        self._dirty = True
        self._update_title()
        self._update_status()

    def _on_model_reset(self) -> None:
        """模型整体重置后清空编辑窗格并刷新状态栏。"""
        self._clear_dock()
        self._update_status()

    def _update_title(self) -> None:
        """刷新窗口标题（文件名 + 未保存星号）。"""
        name = self._path or "未命名"
        star = "*" if self._dirty else ""
        self.setWindowTitle(f"{star}{name} - simpleTable")

    def _update_status(self, *args: Any) -> None:
        """刷新状态栏：显示 筛选后行数 / 总行数。"""
        self.status.showMessage(
            f"显示 {self.proxy.rowCount()} / 共 {self.model.rowCount()} 行")
