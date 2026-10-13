"""pytest 公共配置：离屏运行 Qt。"""
import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import pytest
from PySide6.QtWidgets import QApplication


@pytest.fixture(scope="session")
def qapp() -> QApplication:
    """提供全局唯一的 QApplication（离屏）。

    Returns:
        QApplication: Qt 应用实例。
    """
    app = QApplication.instance()
    if app is None:
        app = QApplication([])
    return app
