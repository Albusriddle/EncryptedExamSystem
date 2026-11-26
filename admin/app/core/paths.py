"""路径工具（core 层，不依赖 UI）。"""

from __future__ import annotations

import os
import sys
from pathlib import Path


def _user_dir(xdg_name: str, fallback: str) -> Path:
    # 保留显式 APPDATA 覆盖，兼容既有便携部署及测试中的隔离目录。
    base = os.environ.get("LOCALAPPDATA") or os.environ.get("APPDATA")
    if base:
        return Path(base) / "ExamSystem"
    if sys.platform == "linux":
        raw = os.environ.get(xdg_name, "")
        # XDG 规范要求绝对路径；相对路径不能随启动工作目录漂移。
        root = Path(raw) if raw and Path(raw).is_absolute() else Path.home() / fallback
        return root / "ExamSystem"
    return Path.home() / "ExamSystem"


def settings_path() -> Path:
    """QSettings ini 的固定位置（源码运行与打包后一致）。"""
    return _user_dir("XDG_CONFIG_HOME", ".config") / "settings.ini"


def data_dir() -> Path:
    """自带题库的持久副本不能随 AppImage 卸载或临时解压目录清理而消失。"""
    return _user_dir("XDG_DATA_HOME", ".local/share")


def state_dir() -> Path:
    """崩溃日志等运行状态不能写进 /usr/bin 或只读安装目录。"""
    return _user_dir("XDG_STATE_HOME", ".local/state")


def executable_name(stem: str) -> str:
    return f"{stem}.exe" if sys.platform == "win32" else stem


def package_file_filter() -> str:
    # Linux 学生端是无扩展名的 ELF，不能被默认 *.exe 过滤器隐藏。
    return "可执行文件 (*.exe);;所有文件 (*)" if sys.platform == "win32" else "学生端包 (*)"
