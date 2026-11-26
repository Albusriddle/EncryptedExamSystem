"""桌面集成只写用户目录；含空格/特殊字符的 AppImage 仍能由桌面启动器打开。"""

from __future__ import annotations

import shutil
import subprocess
import sys
import time
from pathlib import Path

import pytest

from app.core import desktop


def test_desktop_install_requires_real_appimage(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(sys, "frozen", False, raising=False)
    with pytest.raises(ValueError, match="AppImage"):
        desktop.install_desktop_entry()


@pytest.mark.parametrize("xdg", [True, False])
def test_desktop_entry_handles_spaces_and_reserved_characters(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, xdg: bool
) -> None:
    image = tmp_path / '我的 Exams 100% "quoted" $test `name`.AppImage'
    real_is_file = Path.is_file
    monkeypatch.setattr(Path, "is_file", lambda path: path == image or real_is_file(path))
    monkeypatch.setattr(desktop, "appimage_path", lambda: image)
    icon = tmp_path / "ees.svg"
    icon.write_text('<svg xmlns="http://www.w3.org/2000/svg"/>', encoding="utf-8")
    monkeypatch.setattr(desktop, "resource_path", lambda name: icon)
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: tmp_path / "home"))
    data = tmp_path / "data" if xdg else tmp_path / "home/.local/share"
    monkeypatch.setenv("XDG_DATA_HOME", str(data) if xdg else "relative/data")
    entry = desktop.install_desktop_entry()
    assert entry == data / "applications" / f"{desktop.DESKTOP_ID}.desktop"
    text = entry.read_text(encoding="utf-8")
    assert "Terminal=false" in text
    assert "100%%" in text
    assert '\\\\"quoted\\\\"' in text
    assert (data / "icons/hicolor/scalable/apps" / f"{desktop.DESKTOP_ID}.svg").is_file()
    validator = shutil.which("desktop-file-validate")
    if validator:
        subprocess.run([validator, str(entry)], check=True, encoding="utf-8", capture_output=True)
    if sys.platform == "linux" and shutil.which("gio"):
        # 语法校验通过不代表文件管理器真的能启动，百分号文件名曾因此漏测。
        image.write_text('#!/bin/sh\nprintf ok > "$0.marker"\n', encoding="utf-8")
        image.chmod(0o755)
        subprocess.run(
            ["gio", "launch", str(entry)], check=True, encoding="utf-8", capture_output=True
        )
        marker = Path(str(image) + ".marker")
        for _ in range(20):
            if marker.exists():
                break
            time.sleep(0.1)
        assert marker.read_text(encoding="utf-8") == "ok"


def test_newline_filename_cannot_inject_desktop_fields(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    image = tmp_path / "bad\nExec=unwanted.AppImage"
    monkeypatch.setattr(desktop, "appimage_path", lambda: image)
    # Windows 不允许这样的文件名，但其余验证不需要真的创建它。
    monkeypatch.setattr(Path, "is_file", lambda path: True)
    with pytest.raises(ValueError, match="控制字符"):
        desktop.install_desktop_entry()
