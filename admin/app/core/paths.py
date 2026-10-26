"""路径工具（core 层，不依赖 UI）。"""

from __future__ import annotations

import os
import sys
from pathlib import Path


def settings_path() -> Path:
    """QSettings ini 的固定位置（源码运行与打包后一致）。"""
    base = os.environ.get("LOCALAPPDATA") or os.environ.get("APPDATA") or str(Path.home())
    if getattr(sys, "frozen", False):
        base = os.environ.get("LOCALAPPDATA") or os.environ.get("APPDATA") or str(Path.home())
    return Path(base) / "ExamSystem" / "settings.ini"
