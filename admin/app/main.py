"""管理端入口。

用法：
    uv run python -m app.main          # 源码运行
    ExamAdmin.exe                      # PyInstaller 打包后
"""

from __future__ import annotations

import sys
import traceback
from pathlib import Path

from PySide6.QtWidgets import QApplication, QMessageBox

from app.ui.main_window import MainWindow


def log_dir() -> Path:
    """崩溃日志目录：优先 %LOCALAPPDATA%，不可写则退回程序所在目录。"""
    import os

    base = os.environ.get("LOCALAPPDATA")
    candidates = []
    if base:
        candidates.append(Path(base) / "ExamSystem")
    candidates.append(Path(sys.executable).parent)
    for c in candidates:
        try:
            c.mkdir(parents=True, exist_ok=True)
            return c
        except OSError:
            continue
    return Path.cwd()


def install_excepthook() -> None:
    """把未捕获异常变成**看得见**的提示 + 日志文件。

    背景：Qt 的槽函数里抛异常会被事件循环吞掉，窗口上毫无反应。
    教师看到的是"点了没反应"，完全无从排查。
    装上这个钩子后，任何漏网的异常都会弹窗并把栈写进 crash.log。
    """

    def hook(exc_type, exc, tb) -> None:  # noqa: ANN001
        text = "".join(traceback.format_exception(exc_type, exc, tb))
        path = log_dir() / "admin-crash.log"
        try:
            with path.open("a", encoding="utf-8") as f:
                f.write(text + "\n")
        except OSError:
            path = None
        try:
            box = QMessageBox()
            box.setIcon(QMessageBox.Icon.Critical)
            box.setWindowTitle("程序遇到错误")
            box.setText(f"{exc_type.__name__}: {exc}")
            box.setInformativeText(
                f"详细信息已写入\n{path}\n\n可以把这句话发给技术支持。"
                if path
                else "详细信息无法写入磁盘。"
            )
            box.setDetailedText(text[-2000:])
            box.exec()
        except Exception:  # noqa: BLE001 - 弹窗失败也不能再抛
            pass
        sys.__excepthook__(exc_type, exc, tb)

    sys.excepthook = hook


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
    if "--version" in sys.argv:
        # 供构建自检读取，确认 VERSION 真的被打进包里
        from app.version import app_version

        print(app_version())
        return 0
    app = QApplication(sys.argv)
    install_excepthook()
    app.setApplicationName("保密考试系统 · 管理端")
    # 中文字体：Windows 下默认字体即可；Linux 若缺失由系统配置决定
    win = MainWindow()
    win.show()
    return app.exec()


if __name__ == "__main__":
    raise SystemExit(main())
