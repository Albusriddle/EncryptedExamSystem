"""管理端入口。

用法：
    uv run python -m app.main          # 源码运行
    ExamAdmin.exe                      # PyInstaller 打包后
"""

from __future__ import annotations

import sys

from PySide6.QtWidgets import QApplication

from app.ui.main_window import MainWindow


def main() -> int:
    app = QApplication(sys.argv)
    app.setApplicationName("保密考试系统 · 管理端")
    # 中文字体：Windows 下默认字体即可；Linux 若缺失由系统配置决定
    win = MainWindow()
    win.show()
    return app.exec()


if __name__ == "__main__":
    raise SystemExit(main())
