"""管理端的本机设置（INI，不用注册表）。

路径与欢迎弹窗共用 `%LOCALAPPDATA%\\ExamSystem\\settings.ini`——
教育网/受限账户上 QSettings 的注册表后端曾经写不进去。
"""

from __future__ import annotations

import os
from pathlib import Path

from PySide6.QtCore import QSettings

#: 上次打开的工程（.eesproj / .json 绝对路径）
LAST_PROJECT = "last_project"
#: 启动时是否自动打开上次工程
AUTOLOAD = "autoload"
#: 是否已经提示过"工程文件含答案，别随包分发"
WARNED_EESPROJ = "warned_eesproj"


def settings_path() -> Path:
    base = os.environ.get("LOCALAPPDATA") or os.environ.get("APPDATA") or str(Path.home())
    return Path(base) / "ExamSystem" / "settings.ini"


def settings() -> QSettings:
    settings_path().parent.mkdir(parents=True, exist_ok=True)
    return QSettings(str(settings_path()), QSettings.Format.IniFormat)
