"""管理端的本机设置（INI，不用注册表）。

路径与欢迎弹窗共用 `%LOCALAPPDATA%\\ExamSystem\\settings.ini`——
教育网/受限账户上 QSettings 的注册表后端曾经写不进去。
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

from PySide6.QtCore import QSettings

#: 上次打开的工程（.eesproj / .json 绝对路径）
LAST_PROJECT = "last_project"
#: 老师指定的工程保存目录（空 = 程序目录 projects/）
PROJECT_DIR = "project_dir"
#: 是否加密保存工程（EESPROJ2）
PROJECT_ENCRYPT = "project_encrypt"


def settings_path() -> Path:
    base = os.environ.get("LOCALAPPDATA") or os.environ.get("APPDATA") or str(Path.home())
    return Path(base) / "ExamSystem" / "settings.ini"


def teacher_dir() -> Path | None:
    """教师端程序所在目录（打包后）；源码运行返回 None。

    源码运行时返回 None 是为了不往仓库里写工程文件——测试与开发都走 appdata。
    """
    if getattr(sys, "frozen", False):
        return Path(sys.executable).parent
    return None


def projects_dir(create: bool = True) -> Path:
    """自动保存的工程目录。

    优先**教师端程序目录**下的 `projects/`（老师看得见、好备份）；
    程序目录只读（Program Files / 只读 U 盘）时回退 `%LOCALAPPDATA%`。
    **绝不放在考试包输出目录**——那里是要发给学生的。
    """
    base = teacher_dir()
    if base is not None:
        d = base / "projects"
        if not create:
            return d if d.exists() else base
        try:
            d.mkdir(parents=True, exist_ok=True)
            probe = d / ".write-test"
            probe.write_text("", encoding="utf-8")
            probe.unlink()
            return d
        except OSError:
            pass
    d = settings_path().parent / "projects"
    if create:
        d.mkdir(parents=True, exist_ok=True)
        return d
    return d if d.exists() else settings_path().parent


def settings() -> QSettings:
    settings_path().parent.mkdir(parents=True, exist_ok=True)
    return QSettings(str(settings_path()), QSettings.Format.IniFormat)


def project_dir_display() -> Path:
    """界面显示用：返回实际会用的目录（含 projects 子目录/回退）。"""
    raw = settings().value(PROJECT_DIR, "", type=str)
    return Path(raw) if raw else projects_dir()


def active_projects_dir() -> Path:
    """老师设置过就用设置，否则用默认（程序目录 projects/ 或 appdata 回退）。"""
    raw = settings().value(PROJECT_DIR, "", type=str)
    if raw:
        p = Path(raw)
        try:
            p.mkdir(parents=True, exist_ok=True)
            return p
        except OSError:
            return projects_dir()
    return projects_dir()
