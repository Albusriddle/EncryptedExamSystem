"""PIN 表暂存：放应用数据目录（不是系统临时目录）、尽力 0600、下次打包清理。

为什么单独测：暂存的是**明文**学号 + PIN 表。以前写在 `tempfile.gettempdir()`，
Windows 上风险低，但管理端也能在 Linux/macOS 上跑，那里的 `/tmp` 全局可读。
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from app.ui import tab_build


@pytest.fixture
def settings_dir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """把 `settings_path()` 指到 tmp_path 下的假应用数据目录。"""
    fake_settings = tmp_path / "ExamSystem" / "settings.ini"
    monkeypatch.setattr(tab_build, "settings_path", lambda: fake_settings)
    return fake_settings.parent


def test_stash_dir_is_app_data(settings_dir: Path) -> None:
    assert tab_build.pin_stash_dir() == settings_dir
    assert settings_dir.is_dir(), "目录不存在时应自动创建"


def test_stash_copies_pin_file_into_app_dir(settings_dir: Path, tmp_path: Path) -> None:
    src = tmp_path / "pins.csv"
    src.write_text("2021001,123456\n", encoding="utf-8")

    dest = tab_build.stash_pin_file(src, "20261124-130000")

    assert dest.parent == settings_dir
    assert dest.name == "ees-pin-20261124-130000.csv"
    assert dest.read_text(encoding="utf-8") == "2021001,123456\n"
    if os.name != "nt":
        # Windows 上 chmod 只切只读位，拿不到真正的 0600
        assert dest.stat().st_mode & 0o777 == 0o600


def test_cleanup_removes_only_stashed_pins(settings_dir: Path) -> None:
    settings_dir.mkdir(parents=True, exist_ok=True)
    keep = settings_dir / "settings.ini"
    keep.write_text("[general]\n", encoding="utf-8")
    stale = settings_dir / "ees-pin-20260101-000000.csv"
    stale.write_text("old", encoding="utf-8")

    assert tab_build.cleanup_stashed_pins() == 1
    assert not stale.exists(), "上次暂存的明文 PIN 表必须删掉"
    assert keep.is_file(), "不能误删同目录下的其它文件"
