"""AppImage 的持久位置与显式桌面集成；不依赖 Qt，不自动修改用户桌面。"""

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path

DESKTOP_ID = "io.github.December172.ExamAdmin"
STUDENT_DESKTOP_ID = "io.github.December172.ExamClient"


def appimage_path() -> Path | None:
    if sys.platform != "linux" or not getattr(sys, "frozen", False):
        return None
    image = Path(os.environ.get("APPIMAGE", ""))
    directory = Path(os.environ.get("APPDIR", ""))
    # 外部进程可能继承 APPIMAGE；只认确实位于这次挂载点内的程序。
    if (
        image.is_absolute()
        and directory.is_absolute()
        and directory != Path("/")
        and Path(sys.executable).is_relative_to(directory)
    ):
        return image
    return None


def resource_path(name: str) -> Path:
    base = getattr(sys, "_MEIPASS", None)
    return Path(base) / name if base else Path(__file__).resolve().parents[3] / name


def install_desktop_entry() -> Path:
    image = appimage_path()
    if image is None or not image.is_file():
        raise ValueError("请从保存到固定位置的教师端 AppImage 创建启动器，不要使用源码或解压目录。")
    # 文件名中的换行可注入桌面文件的新字段；明确拒绝而不是静默截断。
    if any(c in str(image) for c in "\n\r\x00"):
        raise ValueError("AppImage 路径含不支持的控制字符，请移动到普通目录后重试。")
    raw = os.environ.get("XDG_DATA_HOME", "")
    data = Path(raw) if raw and Path(raw).is_absolute() else Path.home() / ".local/share"
    icons = data / "icons/hicolor/scalable/apps"
    icons.mkdir(parents=True, exist_ok=True)
    shutil.copy2(resource_path("assets/ees.svg"), icons / f"{DESKTOP_ID}.svg")
    from app.branding import branding

    # Exec 先做双引号内的保留字符转义，再做 desktop 字符串转义。
    # GIO 会在展开 %% 之前查找可执行文件，百分号文件名因此被误判不存在。
    # env 把 AppImage 作为参数接收；不经过 shell，不展开 $/反引号或执行文件名。
    quoted = str(image).replace("%", "%%")
    for char in ("\\", '"', "`", "$"):
        quoted = quoted.replace(char, "\\" + char)
    quoted = quoted.replace("\\", "\\\\")
    text = (
        "[Desktop Entry]\nType=Application\n"
        f"Name={branding().product} · 管理端\n"
        "Name[en]=Encrypted Exam Admin\n"
        f'Exec=/usr/bin/env -- "{quoted}"\nIcon={DESKTOP_ID}\n'
        f"StartupWMClass={DESKTOP_ID}\n"
        "Terminal=false\nCategories=Education;\n"
    )
    dest = data / "applications" / f"{DESKTOP_ID}.desktop"
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(text, encoding="utf-8")
    return dest
