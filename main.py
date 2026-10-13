"""simpleTable 程序入口。"""
import sys

from PySide6.QtWidgets import QApplication

from main_window import MainWindow


def main() -> int:
    """创建应用并显示主窗口。

    Returns:
        int: 程序退出码。
    """
    app = QApplication(sys.argv)
    app.setApplicationName("simpleTable")
    window = MainWindow()
    window.show()
    return app.exec()


if __name__ == "__main__":
    sys.exit(main())
