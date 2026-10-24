"""答题卡标签页的显示逻辑（离屏）。"""

from __future__ import annotations

from pathlib import Path

import pytest

pytest.importorskip("PySide6")

from PySide6.QtWidgets import QApplication  # noqa: E402

from app.ui.tab_answers import AnswersTab  # noqa: E402


@pytest.fixture(scope="module")
def qapp():
    app = QApplication.instance() or QApplication([])
    yield app


def _sheet(**over):
    base = {
        "student_id": 2021001,
        "session_id": 1,
        "qids": ["q1", "q2", "q3"],
        "answers": ["B", "AC", None],
        "submitted_unix": 1_700_000_000,
    }
    base.update(over)
    return base


def test_verified_row_is_filled(qapp) -> None:
    tab = AnswersTab()
    try:
        tab._append([Path("answers-2021001-1.eea")])
        tab._on_one("answers-2021001-1.eea", {"ok": True, "sheet": _sheet()})
        assert tab.table.item(0, 1).text() == "2021001"
        assert tab.table.item(0, 3).text() == "3"
        assert tab.table.item(0, 4).text() == "2"
        assert tab.table.item(0, 5).text() == "1"
        assert "通过" in tab.table.item(0, 7).text()
        assert tab.table.item(0, 6).text() != "未交卷"
    finally:
        tab.deleteLater()


def test_failed_row_shows_reason(qapp) -> None:
    tab = AnswersTab()
    try:
        tab._append([Path("bad.eea")])
        tab._on_one("bad.eea", {"ok": False, "message": "解密失败（密钥不匹配或文件被改动）"})
        assert "失败" in tab.table.item(0, 7).text()
        assert "解密失败" in tab.table.item(0, 8).text()
    finally:
        tab.deleteLater()


def test_summary_counts(qapp) -> None:
    tab = AnswersTab()
    try:
        tab._append([Path("a.eea"), Path("b.eea")])
        tab._on_one("a.eea", {"ok": True, "sheet": _sheet()})
        tab._on_one("b.eea", {"ok": False, "message": "x"})
        tab._on_all()
        assert "1 通过" in tab.status.text() and "1 失败" in tab.status.text()
    finally:
        tab.deleteLater()
