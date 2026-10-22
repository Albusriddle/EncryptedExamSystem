"""管理端入口。

用法：
    uv run python -m app.main          # 源码运行
    ExamAdmin.exe                      # PyInstaller 打包后
"""

from __future__ import annotations

import sys

from PySide6.QtWidgets import QApplication

from app.ui.main_window import MainWindow


def selftest() -> int:
    """只验证运行依赖可导入，不创建窗口。

    打包（PyInstaller）后由 build_exe.py 调用，用来确认产物真的能跑——
    缺少依赖（如 pydantic）时 PyInstaller 不会报错，但打出来的 exe
    一启动就 ModuleNotFoundError。这一步把它变成构建期失败。
    """
    try:
        import pydantic  # noqa: F401
        from PySide6.QtWidgets import QApplication  # noqa: F401

        from app.core.models import Manifest  # noqa: F401
        from app.core.state import ProjectState  # noqa: F401
        from app.ui.main_window import MainWindow  # noqa: F401
    except Exception:  # noqa: BLE001 - 打印完整栈以便打包时定位
        import traceback

        traceback.print_exc()
        return 1
    return 0


def main() -> int:
    if "--selftest" in sys.argv:
        return selftest()
    app = QApplication(sys.argv)
    app.setApplicationName("保密考试系统 · 管理端")
    # 中文字体：Windows 下默认字体即可；Linux 若缺失由系统配置决定
    win = MainWindow()
    win.show()
    return app.exec()


if __name__ == "__main__":
    raise SystemExit(main())
