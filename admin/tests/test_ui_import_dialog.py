"""导入确认对话框的行为测试（需要 PySide6）。"""

from __future__ import annotations

import pytest

pytest.importorskip("PySide6")

from PySide6.QtWidgets import QApplication, QMessageBox  # noqa: E402

from app.core.bank_import import convert_pair  # noqa: E402
from app.ui.import_dialog import ImportConfirmDialog  # noqa: E402


@pytest.fixture(scope="module")
def qapp():
    app = QApplication.instance() or QApplication([])
    yield app


def _item():
    q = {
        "format_version": 2,
        "type": "choice",
        "role": "questions",
        "count": 1,
        "questions": [
            {
                "id": "CH-1",
                "difficulty": 1,
                "stem": "示例",
                "content": {"kind": "options", "options": {"A": "甲", "B": "乙"}},
            }
        ],
    }
    a = {
        "format_version": 2,
        "type": "choice",
        "role": "answers",
        "count": 1,
        "answers": [{"id": "CH-1", "verdict": {"kind": "single", "answer": "A"}, "judge": None}],
    }
    return convert_pair(q, a, exam_id="choice", title="choice")


def test_dialog_allows_rename(qapp) -> None:
    items = [_item()]
    dlg = ImportConfirmDialog(items)
    try:
        assert dlg.table.item(0, 1).text() == "choice"
        dlg.table.item(0, 1).setText("数据结构-期中")
        dlg.table.item(0, 2).setText("数据结构期中考试")
        dlg.accept()
        assert items[0].bank.exam_id == "数据结构-期中"
        assert items[0].bank.title == "数据结构期中考试"
    finally:
        dlg.deleteLater()


def test_dialog_rejects_unsafe_id(qapp, monkeypatch) -> None:
    warned: list[str] = []
    monkeypatch.setattr(QMessageBox, "warning", lambda *a, **k: warned.append(a[1]))

    items = [_item()]
    dlg = ImportConfirmDialog(items)
    try:
        dlg.table.item(0, 1).setText("../逃逸")
        dlg.accept()
        assert warned, "非法 exam_id 必须被拦下"
        assert items[0].bank.exam_id == "choice", "被拦下时不能改动题库"
    finally:
        dlg.deleteLater()
