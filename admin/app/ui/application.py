"""启动与诊断共用的应用外观；Linux 不继承可能遮住文字的第三方 Qt 主题。"""

from __future__ import annotations

import sys

from PySide6.QtGui import QFont, QFontDatabase, QIcon
from PySide6.QtWidgets import QApplication

from app.branding import branding
from app.core.desktop import DESKTOP_ID, resource_path


def configure_application(app: QApplication) -> None:
    app.setApplicationName(f"{branding().product} · 管理端")
    app.setWindowIcon(QIcon(str(resource_path("assets/ees.svg"))))
    if sys.platform != "linux":
        return
    app.setDesktopFileName(DESKTOP_ID)
    app.setStyle("Fusion")
    app.setPalette(app.style().standardPalette())
    for name in ("NotoSansCJK-Regular.ttc", "NotoSansCJK-Bold.ttc"):
        path = resource_path("fonts") / name
        if path.is_file():
            QFontDatabase.addApplicationFont(str(path))
    if "Noto Sans CJK SC" in QFontDatabase.families():
        app.setFont(QFont("Noto Sans CJK SC", 9))
