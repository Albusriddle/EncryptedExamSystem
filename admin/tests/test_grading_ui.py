"""判分界面：生成成绩、双击改分、导出四个 CSV。"""

from __future__ import annotations

import csv
from pathlib import Path

import pytest

pytest.importorskip("PySide6")

from PySide6.QtWidgets import QFileDialog, QMessageBox  # noqa: E402

from app.core.models import ExamBank, Question  # noqa: E402
from app.core.state import ProjectState  # noqa: E402
from app.ui.tab_answers import AnswersTab  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
WORK = ROOT / "target" / "test-grading-ui"


@pytest.fixture(scope="module")
def qapp():
    import sys

    from PySide6.QtWidgets import QApplication

    app = QApplication.instance() or QApplication(sys.argv)
    yield app


@pytest.fixture()
def tab(qapp, monkeypatch, tmp_path):
    WORK.mkdir(parents=True, exist_ok=True)
    bank = ExamBank(
        exam_id="m",
        title="数学",
        pick=3,
        questions=[
            Question(
                id="q1",
                type="single",
                stem="1+1?",
                answer={"kind": "single", "answer": "A"},
                score=2,
            ),
            Question(
                id="q2",
                type="multiple",
                stem="偶数?",
                answer={"kind": "multi", "answer": ["A", "C"]},
                score=3,
            ),
            Question(
                id="q3",
                type="fill",
                stem="填空",
                answer={"kind": "blanks", "answer": [{"no": "①", "answer": "x"}]},
                score=4,
            ),
        ],
    )
    state = ProjectState(salt="00" * 16, students=[1])
    state.banks["m"] = bank

    t = AnswersTab(state)
    fake = WORK / "answers-1-1-00000000.eea"
    fake.write_bytes(b"x")
    t.files = [fake]
    t.results = {
        str(fake): {
            "ok": True,
            "sheet": {
                "student_id": 1,
                "session_id": 1,
                "qids": ["q1", "q2", "q3"],
                "answers": ["A", "AC", '["y"]'],
            },
        }
    }
    monkeypatch.setattr(QMessageBox, "information", staticmethod(lambda *a, **k: None))
    monkeypatch.setattr(QMessageBox, "warning", staticmethod(lambda *a, **k: None))
    yield t
    t.deleteLater()


def test_grade_fills_table_and_summary(tab) -> None:
    tab._grade()
    assert tab.grade_table.rowCount() == 3
    assert len(tab.grade_rows) == 3
    assert tab.grade_rows[0].status == "correct"
    assert tab.grade_rows[2].status == "unjudged", "默认不判填空"
    assert "已判 2 题" in tab.grade_summary.text()


def test_manual_score_edit_updates_total(tab) -> None:
    tab._grade()
    before = tab.grade_summary.text()
    tab.grade_table.item(1, 7).setText("1")  # 第二题（多选）本来满分 3
    assert tab.grade_rows[1].manual_score == 1
    assert "3 → 1" in tab.grade_table.item(1, 9).text()
    assert tab.grade_summary.text() != before


def test_invalid_score_is_clamped(tab) -> None:
    tab._grade()
    tab.grade_table.item(0, 7).setText("999")
    assert tab.grade_rows[0].final_score == 2, "不能超过满分"
    tab.grade_table.item(0, 7).setText("-5")
    assert tab.grade_rows[0].final_score == 0


def test_export_writes_four_csvs(tab, monkeypatch) -> None:
    tab._grade()
    out = WORK / "out"
    out.mkdir(exist_ok=True)
    monkeypatch.setattr(QFileDialog, "getExistingDirectory", staticmethod(lambda *a, **k: str(out)))
    tab._export_grading()

    names = {p.name for p in out.glob("*.csv")}
    assert names == {"已判分-明细.csv", "已判分-汇总.csv", "未判分.csv", "全量.csv"}
    text = (out / "已判分-明细.csv").read_text(encoding="utf-8-sig")
    assert "判分口径" in text.splitlines()[0]
    rows = list(csv.reader(text.splitlines()[1:]))
    assert rows[0][0] == "学号"
