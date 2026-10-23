"""第三轮全仓审阅发现问题的回归测试。"""

from __future__ import annotations

import json
import os
import sys
import time
from pathlib import Path

import pytest

from app.core.timeparse import TimeParseError, parse_rfc3339

# ---------------------------------------------------------------- 日历合法性


@pytest.mark.parametrize(
    "bad",
    [
        "2026-02-31T10:00:00+08:00",
        "2026-04-31T10:00:00+08:00",
        "2026-02-29T10:00:00+08:00",  # 2026 不是闰年
        "2026-06-31T10:00:00+08:00",
    ],
)
def test_nonexistent_calendar_dates_are_rejected(bad: str) -> None:
    """只查 1..=31 会把 2 月 31 日静默算成 3 月 3 日。"""
    with pytest.raises(TimeParseError):
        parse_rfc3339(bad)


@pytest.mark.parametrize(
    "good",
    [
        "2024-02-29T10:00:00+08:00",  # 闰年
        "2026-01-31T10:00:00+08:00",
        "2026-12-31T10:00:00+08:00",
        "2026-04-30T10:00:00+08:00",
    ],
)
def test_valid_calendar_dates_still_parse(good: str) -> None:
    assert parse_rfc3339(good) > 0


# ---------------------------------------------------------------- 工程文件缺 salt


def test_load_without_salt_reports_clearly(tmp_path: Path) -> None:
    from app.core.state import ProjectState

    p = tmp_path / "project.json"
    p.write_text(json.dumps({"version": 1, "students": [], "sessions": []}), encoding="utf-8")

    with pytest.raises(ValueError, match="salt"):
        ProjectState.load(p)


# ---------------------------------------------------------------- 时间轴高度


@pytest.fixture(scope="module")
def qapp():
    pytest.importorskip("PySide6")
    from PySide6.QtWidgets import QApplication

    app = QApplication.instance() or QApplication(sys.argv)
    yield app


def test_timeline_grows_with_session_count(qapp) -> None:
    """行高有 14pt 下限：10 个场次需要 ~170pt，固定 110pt 会裁掉一半。"""
    from app.core.models import Session
    from app.ui.tab_sessions import TimelineWidget

    def sess(i: int) -> Session:
        return Session(
            session_id=i,
            start="2026-10-23T09:00:00+08:00",
            end="2026-10-23T10:00:00+08:00",
            students=[1],
            exams=[],
        )

    w = TimelineWidget()
    w.set_sessions([], set())
    base = w.minimumHeight()
    w.set_sessions([sess(i) for i in range(1, 11)], set())
    assert w.minimumHeight() > base, "10 个场次时高度必须增加"
    need = 26 + 10 * 17
    assert w.minimumHeight() >= need


# ---------------------------------------------------------------- 产物选择


def test_find_artifact_prefers_newest(tmp_path: Path) -> None:
    """本机同时有 target/release 与 triple 目录时，应取最新的那份。

    固定取前者可能拿到很久以前的旧 client，把旧版本打进发布包。
    """
    from buildtools.build_exe import find_artifact

    old = tmp_path / "target" / "release" / "demo-bin"
    new = tmp_path / "target" / "x86_64-pc-windows-msvc" / "release" / "demo-bin"
    new.parent.mkdir(parents=True)
    old.parent.mkdir(parents=True)
    old.write_bytes(b"old")
    new.write_bytes(b"new")
    os.utime(old, (time.time() - 3600, time.time() - 3600))

    assert find_artifact("demo-bin", root=tmp_path) == new
