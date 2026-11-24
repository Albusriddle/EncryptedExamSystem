"""PIN 表暂存：放应用数据目录（不是系统临时目录）、尽力 0600、按保留期清理。

为什么单独测：暂存的是**明文**学号 + PIN 表。以前写在 `tempfile.gettempdir()`，
Windows 上风险低，但管理端也能在 Linux/macOS 上跑，那里的 `/tmp` 全局可读。

为什么清理必须按保留期：老师「取消保存」时暂存件是**唯一副本**（源文件在临时
题库目录里，打包结束就被删）。以前「下次打包一开始就全删」，而清理发生在配置/
密钥校验之前——打包随后被拦下时，PIN 表也已经被删了；PIN 是构建时随机生成的，
丢了只能重打包并重新分发。
"""

from __future__ import annotations

import os
import time
from pathlib import Path

import pytest

from app.ui import tab_build


@pytest.fixture
def settings_dir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """把 `settings_path()` 指到 tmp_path 下的假应用数据目录。"""
    fake_settings = tmp_path / "ExamSystem" / "settings.ini"
    monkeypatch.setattr(tab_build, "settings_path", lambda: fake_settings)
    return fake_settings.parent


def _stashed(directory: Path, name: str, age_days: float) -> Path:
    """造一个 PIN 暂存件，并把 mtime 往回拨 `age_days` 天。"""
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / name
    path.write_text("2021001,123456\n", encoding="utf-8")
    stamp = time.time() - age_days * 24 * 3600
    os.utime(path, (stamp, stamp))
    return path


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


def test_cleanup_keeps_recent_and_removes_expired(settings_dir: Path) -> None:
    """刚暂存的必须留着（唯一副本），过期件才删，别动其它文件。"""
    fresh = _stashed(settings_dir, "ees-pin-20261124-130000.csv", 0)
    old = _stashed(
        settings_dir,
        "ees-pin-20260101-000000.csv",
        tab_build.STASH_RETENTION_DAYS + 1,
    )
    keep = settings_dir / "settings.ini"
    keep.write_text("[general]\n", encoding="utf-8")

    assert tab_build.cleanup_stashed_pins() == 1
    assert fresh.is_file(), "刚暂存的 PIN 表不能删（它是唯一副本）"
    assert not old.exists(), "超过保留期的明文 PIN 表必须删掉"
    assert keep.is_file(), "不能误删同目录下的其它文件"


def test_cleanup_boundary_is_retention_days(settings_dir: Path) -> None:
    edge_keep = _stashed(settings_dir, "ees-pin-keep.csv", tab_build.STASH_RETENTION_DAYS - 0.1)
    edge_drop = _stashed(settings_dir, "ees-pin-drop.csv", tab_build.STASH_RETENTION_DAYS + 0.1)

    assert tab_build.cleanup_stashed_pins() == 1
    assert edge_keep.is_file()
    assert not edge_drop.exists()


def test_cleanup_survives_unwritable_dir(
    settings_dir: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """清理失败不能让「打包」按钮抛异常（只读漫游配置 / 受限账户）。"""

    def boom() -> Path:
        raise OSError(13, "只读目录")

    monkeypatch.setattr(tab_build, "pin_stash_dir", boom)
    assert tab_build.cleanup_stashed_pins() == 0
