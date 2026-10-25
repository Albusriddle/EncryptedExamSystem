"""子进程调用的公共参数（Windows 下不弹控制台窗口）。"""

from __future__ import annotations

import os
import subprocess


def no_window_kwargs() -> dict:
    """GUI（--windowed）启动控制台程序时，不让 Windows 新建黑窗口。"""
    if os.name == "nt":
        return {"creationflags": subprocess.CREATE_NO_WINDOW}
    return {}
