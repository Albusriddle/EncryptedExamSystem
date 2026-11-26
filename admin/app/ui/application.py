"""启动与诊断共用的应用外观；Linux 不继承可能遮住文字的第三方 Qt 主题。"""

from __future__ import annotations

import sys

from PySide6.QtGui import QFont, QFontDatabase, QIcon
from PySide6.QtWidgets import QApplication

from app.branding import branding
from app.core.desktop import DESKTOP_ID, resource_path


def configure_application(app: QApplication) -> dict[str, list[str]]:
    app.setApplicationName(f"{branding().product} · 管理端")
    app.setWindowIcon(QIcon(str(resource_path("assets/ees.svg"))))
    if sys.platform != "linux":
        return {}
    app.setDesktopFileName(DESKTOP_ID)
    app.setStyle("Fusion")
    app.setPalette(app.style().standardPalette())
    bundled_fonts: dict[str, list[str]] = {}
    for name in ("NotoSansCJK-Regular.ttc", "NotoSansCJK-Bold.ttc"):
        path = resource_path("fonts") / name
        if path.is_file():
            font_id = QFontDatabase.addApplicationFont(str(path))
            if font_id >= 0:
                # 全局 families() 也包含系统字体，不能证明同名字体确实从随包文件加载。
                bundled_fonts[name] = QFontDatabase.applicationFontFamilies(font_id)
    if "Noto Sans CJK SC" in QFontDatabase.families():
        app.setFont(QFont("Noto Sans CJK SC", 9))
    return bundled_fonts
